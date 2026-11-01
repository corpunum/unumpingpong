// SPDX-License-Identifier: Apache-2.0
//! OpenUnum-compatible ping-pong API. OpenUnum's `pingpong_create` / `pingpong_answer` /
//! `pingpong_tables` tools (scripts/openunum-mcp.mjs) call
//!
//! ```text
//! GET  /api/pingpong/tables[?status=S&limit=N]   {ok, tables:[table...]}
//! POST /api/pingpong/tables                      201 {ok, table}   (400 invalid_payload)
//! GET  /api/pingpong/tables/{id}                 {ok, table + ball + rallies}   (404 table_not_found)
//! POST /api/pingpong/tables/{id}/answer          {ok, table}   (409 not_waiting_for_server)
//! POST /api/pingpong/tables/{id}/stop            {ok, table}   (409 table_not_active)
//! ```
//!
//! This module maps those request and response shapes onto the engine's own tables, so the
//! MCP tools work against `uppd` unchanged. Every function returns `(http status, body)`.
//!
//! Mapping notes: the OpenUnum `players` ({model, thinking}) overlay the daemon's default
//! player; `server.mode = "model"` runs as `auto` (a model server needs an explicit player
//! in this engine); `next` becomes a table that starts `after` this one scores, with
//! `{{evidence}}` and `{{reply}}` both bound to the previous answer.

use crate::api;
use crate::config::MatchFile;
use crate::store::{Store, TableRow};
use serde_json::{json, Map, Value};

/// Status names of the OpenUnum tool schema.
pub const STATUSES: [&str; 8] = [
    "queued",
    "serving",
    "refereeing",
    "waiting_server",
    "done",
    "lost",
    "stopped",
    "failed",
];
const MAX_CHAIN_DEPTH: usize = 20;
const PLAYER_THINKING: [&str; 6] = ["max", "xhigh", "high", "medium", "low", "off"];

/// Engine status/phase -> OpenUnum status.
pub fn status_of(t: &TableRow) -> &'static str {
    match t.status.as_str() {
        "queued" => "queued",
        "running" => {
            if t.phase == "referee" {
                "refereeing"
            } else {
                "serving"
            }
        }
        "awaiting_server" => "waiting_server",
        "point" => "done",
        "lost" | "expired" => "lost",
        "stopped" | "skipped" => "stopped",
        _ => "failed",
    }
}

fn is_active(s: &str) -> bool {
    matches!(s, "queued" | "serving" | "refereeing" | "waiting_server")
}

fn iso(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000
    )
}

fn err(code: u16, error: &str, details: Value) -> (u16, Value) {
    let mut v = json!({"ok": false, "error": error});
    if !details.is_null() {
        v["details"] = details;
    }
    (code, v)
}

// ---- validation (mirrors src/core/pingpong/spec.mjs) --------------------------------

fn is_str(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.trim().is_empty())
}

fn in_range(v: &Value, lo: f64, hi: f64) -> bool {
    v.as_f64().is_some_and(|n| n >= lo && n <= hi)
}

fn bad(errors: &mut Vec<Value>, field: impl Into<String>, issue: impl Into<String>) {
    errors.push(json!({"field": field.into(), "issue": issue.into()}));
}

fn validate_referee(r: &Value, errors: &mut Vec<Value>, field: &str) {
    let Some(o) = r.as_object() else {
        return bad(errors, field, "expected object {cmd|checks}");
    };
    if !o.contains_key("cmd") && !o.contains_key("checks") {
        bad(errors, field, "needs cmd or checks");
    }
    if o.get("cmd").is_some_and(|c| !is_str(c)) {
        bad(errors, format!("{field}.cmd"), "expected non-empty string");
    }
    if o.get("cwd").is_some_and(|c| !is_str(c)) {
        bad(errors, format!("{field}.cwd"), "expected string");
    }
    if o.get("timeoutMs")
        .is_some_and(|c| !in_range(c, 100.0, 3_600_000.0))
    {
        bad(
            errors,
            format!("{field}.timeoutMs"),
            "number in [100,3600000]",
        );
    }
    if let Some(checks) = o.get("checks") {
        match checks.as_array() {
            Some(a) if !a.is_empty() => {
                for (i, c) in a.iter().enumerate() {
                    let f = format!("{field}.checks[{i}]");
                    match c.get("type").and_then(Value::as_str) {
                        Some("file_exists") if !is_str(&c["path"]) => {
                            bad(errors, format!("{f}.path"), "required string")
                        }
                        Some("http_ok")
                            if !c["url"].as_str().is_some_and(|u| {
                                u.starts_with("http://") || u.starts_with("https://")
                            }) =>
                        {
                            bad(errors, format!("{f}.url"), "expected http(s) URL")
                        }
                        Some("command_exit0") if !is_str(&c["cmd"]) => {
                            bad(errors, format!("{f}.cmd"), "required string")
                        }
                        Some("file_exists" | "http_ok" | "command_exit0") => {}
                        _ => bad(errors, f, "type one of file_exists|http_ok|command_exit0"),
                    }
                }
            }
            _ => bad(
                errors,
                format!("{field}.checks"),
                "expected non-empty array",
            ),
        }
    }
}

/// Validate a table spec (`template`: a chain template, where `referee` and `goal` may be omitted).
pub fn validate(body: &Value, template: bool, depth: usize) -> Vec<Value> {
    let mut errors = Vec::new();
    let Some(o) = body.as_object() else {
        bad(&mut errors, "body", "expected JSON object");
        return errors;
    };
    if !is_str(o.get("goal").unwrap_or(&Value::Null)) && !(template && !o.contains_key("goal")) {
        bad(&mut errors, "goal", "required non-empty string");
    }
    if o.get("brief").is_some_and(|b| !is_str(b)) {
        bad(&mut errors, "brief", "expected non-empty string");
    }
    if o.get("expectedEvidence").is_some_and(|b| !b.is_string()) {
        bad(&mut errors, "expectedEvidence", "expected string");
    }
    if let Some(s) = o.get("server") {
        match s.as_object() {
            None => bad(&mut errors, "server", "expected object {mode}"),
            Some(so) => {
                if so
                    .get("mode")
                    .is_some_and(|m| !matches!(m.as_str(), Some("auto" | "model" | "external")))
                {
                    bad(&mut errors, "server.mode", "one of auto|model|external");
                }
            }
        }
    }
    if let Some(p) = o.get("players") {
        match p.as_array() {
            Some(a) if !a.is_empty() => {
                for (i, pl) in a.iter().enumerate() {
                    match pl.as_object() {
                        None => bad(
                            &mut errors,
                            format!("players[{i}]"),
                            "expected object {model?, thinking?}",
                        ),
                        Some(po) => {
                            if po.get("model").is_some_and(|m| !m.is_string()) {
                                bad(
                                    &mut errors,
                                    format!("players[{i}].model"),
                                    "expected string",
                                );
                            }
                            if po.get("thinking").is_some_and(|t| {
                                !t.as_str().is_some_and(|t| PLAYER_THINKING.contains(&t))
                            }) {
                                bad(
                                    &mut errors,
                                    format!("players[{i}].thinking"),
                                    "max|xhigh|high|medium|low|off",
                                );
                            }
                        }
                    }
                }
            }
            _ => bad(&mut errors, "players", "expected non-empty array"),
        }
    }
    if !template || o.contains_key("referee") {
        match o.get("referee") {
            None => bad(&mut errors, "referee", "required {cmd|checks}"),
            Some(r) => validate_referee(r, &mut errors, "referee"),
        }
    }
    if o.get("maxRallies")
        .is_some_and(|v| !in_range(v, 1.0, 100.0))
    {
        bad(&mut errors, "maxRallies", "number in [1,100]");
    }
    if o.get("leaseMs")
        .is_some_and(|v| !in_range(v, 1000.0, 7.0 * 24.0 * 3_600_000.0))
    {
        bad(&mut errors, "leaseMs", "number in [1000,604800000]");
    }
    if o.get("cwd").is_some_and(|v| !is_str(v)) {
        bad(&mut errors, "cwd", "expected string");
    }
    if o.get("parentSessionId").is_some_and(|v| !v.is_string()) {
        bad(&mut errors, "parentSessionId", "expected string");
    }
    if let Some(n) = o.get("next").filter(|n| !n.is_null()) {
        if depth + 1 > MAX_CHAIN_DEPTH {
            bad(
                &mut errors,
                "next",
                format!("chain deeper than {MAX_CHAIN_DEPTH}"),
            );
        } else {
            for e in validate(n, true, depth + 1) {
                bad(
                    &mut errors,
                    format!("next.{}", e["field"].as_str().unwrap_or("")),
                    e["issue"].as_str().unwrap_or("").to_string(),
                );
            }
        }
    }
    errors
}

// ---- create ------------------------------------------------------------------------

fn interpolate(text: &str, goal: &str, as_ball: bool) -> String {
    let re = regex::Regex::new(r"\{\{\s*(goal|evidence|reply)\s*\}\}").expect("static regex");
    re.replace_all(text, |c: &regex::Captures| match &c[1] {
        "goal" => goal.to_string(),
        _ if as_ball => "{{prev.answer}}".to_string(),
        _ => "(the previous table's result)".to_string(),
    })
    .into_owned()
}

fn absolute(p: &str) -> String {
    std::path::absolute(p)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_string())
}

fn referee_checks(r: &Value) -> Vec<Value> {
    let secs = |ms: &Value| ms.as_f64().map(|m| (m / 1000.0).ceil().max(1.0) as u64);
    let rcwd = r["cwd"].as_str().map(absolute);
    let join = |base: &Option<String>, p: &str| match base {
        Some(b) => std::path::Path::new(b)
            .join(p)
            .to_string_lossy()
            .into_owned(),
        None => p.to_string(),
    };
    let command = |cmd: &str, timeout: Option<u64>, cwd: Option<String>| {
        let mut c = json!({"kind": "command", "run": cmd});
        if let Some(t) = timeout {
            c["timeout_secs"] = json!(t);
        }
        if let Some(d) = cwd {
            c["cwd"] = json!(d);
        }
        c
    };
    let mut out = Vec::new();
    if let Some(cmd) = r["cmd"].as_str() {
        out.push(command(cmd, secs(&r["timeoutMs"]), rcwd.clone()));
    }
    for c in r["checks"].as_array().into_iter().flatten() {
        match c["type"].as_str() {
            Some("file_exists") => {
                out.push(json!({"kind": "file_exists", "path": join(&rcwd, c["path"].as_str().unwrap_or(""))}))
            }
            Some("http_ok") => {
                let mut h = json!({"kind": "http_ok", "url": c["url"]});
                if let Some(s) = c["status"].as_u64() {
                    h["status"] = json!(s);
                }
                if let Some(t) = secs(&c["timeoutMs"]) {
                    h["timeout_secs"] = json!(t);
                }
                out.push(h);
            }
            Some("command_exit0") => out.push(command(
                c["cmd"].as_str().unwrap_or(""),
                secs(&c["timeoutMs"]).or_else(|| secs(&r["timeoutMs"])),
                c["cwd"].as_str().map(absolute).or_else(|| rcwd.clone()),
            )),
            _ => {}
        }
    }
    out
}

fn player_for(default_player: &Value, spec: &Value) -> Value {
    let mut p = default_player.clone();
    let first = &spec["players"][0];
    if let (Some(po), Some(model)) = (p.as_object_mut(), first["model"].as_str()) {
        if po.get("kind").and_then(Value::as_str) != Some("shell") {
            po.insert("model".into(), json!(model));
        }
    }
    if let (Some(po), Some(th)) = (p.as_object_mut(), first["thinking"].as_str()) {
        if po.get("kind").and_then(Value::as_str) == Some("openunum") {
            po.insert("thinking".into(), json!(th));
        }
    }
    p
}

/// `POST /api/pingpong/tables`. `default_player` is the daemon's player (a `PlayerSpec` as JSON).
pub fn create(store: &Store, default_player: &Value, body: &Value) -> (u16, Value) {
    let errors = validate(body, false, 0);
    if !errors.is_empty() {
        return err(400, "invalid_payload", json!(errors));
    }
    // Flatten the `next` chain into effective specs.
    let mut specs: Vec<Value> = vec![body.clone()];
    while let Some(tpl) = specs
        .last()
        .and_then(|p| p.get("next"))
        .filter(|n| n.is_object())
    {
        let parent = specs.last().expect("non-empty").clone();
        let pgoal = parent["goal"].as_str().unwrap_or("").to_string();
        let mut eff = Map::new();
        let tgoal = tpl["goal"].as_str().unwrap_or(&pgoal);
        eff.insert("goal".into(), json!(interpolate(tgoal, &pgoal, false)));
        let brief_src = tpl["brief"].as_str().unwrap_or(tgoal);
        eff.insert("brief".into(), json!(interpolate(brief_src, &pgoal, true)));
        if let Some(e) = tpl["expectedEvidence"].as_str() {
            eff.insert(
                "expectedEvidence".into(),
                json!(interpolate(e, &pgoal, true)),
            );
        }
        for k in [
            "server",
            "players",
            "referee",
            "maxRallies",
            "leaseMs",
            "cwd",
        ] {
            if let Some(v) = tpl
                .get(k)
                .or_else(|| parent.get(k))
                .filter(|v| !v.is_null())
            {
                eff.insert(k.into(), v.clone());
            }
        }
        if let Some(n) = tpl.get("next").filter(|n| n.is_object()) {
            eff.insert("next".into(), n.clone());
        }
        let eff = Value::Object(eff);
        specs.push(eff);
        if specs.len() > MAX_CHAIN_DEPTH + 1 {
            return err(400, "chain_too_deep", Value::Null);
        }
    }
    let base = format!("pp-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let mut tables = Vec::new();
    for (i, s) in specs.iter().enumerate() {
        let name = if i == 0 {
            base.clone()
        } else {
            format!("{base}-{i}")
        };
        let mode = match s["server"]["mode"].as_str() {
            Some("external") => "external",
            _ => "auto",
        };
        let mut ball = s["brief"]
            .as_str()
            .or_else(|| s["goal"].as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if let Some(e) = s["expectedEvidence"]
            .as_str()
            .filter(|e| !e.trim().is_empty())
        {
            ball.push_str(&format!("\n\nExpected evidence: {e}"));
        }
        let cwd = s["cwd"]
            .as_str()
            .or_else(|| s["referee"]["cwd"].as_str())
            .map(absolute);
        let mut lease = Map::new();
        if let Some(ms) = s["leaseMs"].as_f64() {
            lease.insert("wall_secs".into(), json!((ms / 1000.0).ceil() as u64));
        }
        if let Some(n) = s["maxRallies"].as_u64() {
            lease.insert("max_rallies".into(), json!(n));
        }
        let mut t = json!({
            "name": name,
            "goal": s["goal"].as_str().unwrap_or("").trim(),
            "ball": ball,
            "player": player_for(default_player, s),
            "server": {"mode": mode, "review_points": false},
            "referee": referee_checks(&s["referee"]),
            "lease": lease,
        });
        if let Some(c) = cwd {
            t["cwd"] = json!(c);
        }
        if i > 0 {
            t["after"] = json!(if i == 1 {
                base.clone()
            } else {
                format!("{base}-{}", i - 1)
            });
        }
        tables.push(t);
    }
    let m = match MatchFile::from_json(json!({"tables": tables})) {
        Ok(m) => m,
        Err(e) => {
            return err(
                400,
                "invalid_payload",
                json!([{"field": "body", "issue": e}]),
            )
        }
    };
    let created = match api::create(store, m, Some("openunum-compat")) {
        Ok(c) => c,
        Err(e) => {
            return err(
                400,
                "invalid_payload",
                json!([{"field": "body", "issue": e}]),
            )
        }
    };
    let ids: Vec<String> = created["tables"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["id"].as_str().map(String::from))
        .collect();
    for (i, id) in ids.iter().enumerate() {
        let s = &specs[i];
        let meta = json!({
            "goal": s["goal"], "brief": s["brief"], "expectedEvidence": s["expectedEvidence"],
            "server": if s["server"].is_object() { s["server"].clone() } else { json!({"mode": "auto"}) },
            "serverMode": s["server"]["mode"].as_str().map(|m| if m == "external" { "external" } else { "auto" }).unwrap_or("auto"),
            "players": s.get("players").cloned().unwrap_or_else(|| json!([{}])),
            "referee": s["referee"], "next": s.get("next").cloned().unwrap_or(Value::Null),
            "cwd": s["cwd"], "parentSessionId": if i == 0 { body["parentSessionId"].clone() } else { Value::Null },
            "chainFrom": if i == 0 { Value::Null } else { json!(ids[i - 1]) },
        });
        if let Err(e) = store.meta_set(id, &meta) {
            return err(500, "store_error", json!([{"field": "meta", "issue": e}]));
        }
    }
    match ids.first().and_then(|id| store.table(id).ok().flatten()) {
        Some(t) => (201, json!({"ok": true, "table": view(store, &t, false)})),
        None => err(500, "store_error", Value::Null),
    }
}

// ---- read --------------------------------------------------------------------------

fn referee_view(checks: &Value, passed: bool) -> Value {
    let list: Vec<Value> = checks
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            let ok = c["pass"].as_bool().unwrap_or(false);
            let name = c["name"].as_str().unwrap_or("");
            json!({
                "type": c["kind"], "label": name, "ok": ok,
                "output": if c["output"].is_null() { c["output_tail"].clone() } else { c["output"].clone() },
                "summary": c["summary"], "durationMs": c["duration_ms"],
            })
        })
        .collect();
    let output = list
        .iter()
        .map(|c| {
            format!(
                "[{}] {}{}",
                if c["ok"] == true { "PASS" } else { "FAIL" },
                c["label"].as_str().unwrap_or(""),
                match c["output"].as_str().filter(|o| !o.is_empty()) {
                    Some(o) => format!("\n{o}"),
                    None => String::new(),
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    json!({"passed": passed, "checks": list, "output": output})
}

fn chain_next_id(store: &Store, t: &TableRow) -> Value {
    store
        .tables(Some(&t.run_id), false)
        .ok()
        .and_then(|v| v.into_iter().find(|x| x.after_id.as_deref() == Some(&t.id)))
        .map(|x| json!(x.id))
        .unwrap_or(Value::Null)
}

/// One table in OpenUnum's shape (`detail`: with the current ball and every rally).
pub fn view(store: &Store, t: &TableRow, detail: bool) -> Value {
    let meta = store
        .meta_get(&t.id)
        .ok()
        .flatten()
        .unwrap_or_else(|| json!({}));
    let status = status_of(t);
    let wall_ms = (t.wall_secs * 1000) as i64;
    let rallies = store.rallies(&t.id).unwrap_or_default();
    let ended = t.ended_ms;
    let evidence = if status == "done" {
        let last = rallies.last();
        json!({
            "referee": last.map(|r| referee_view(&r.checks, r.passed)),
            "reply": t.result, "rallies": t.rallies,
        })
    } else {
        Value::Null
    };
    let end_reason = if t.is_terminal() {
        json!(if status == "done" {
            "point".to_string()
        } else {
            t.reason.clone().unwrap_or_default()
        })
    } else {
        Value::Null
    };
    let mut v = json!({
        "id": t.id,
        "goal": t.goal,
        "status": status,
        "endReason": end_reason,
        "serverMode": meta["serverMode"].as_str().unwrap_or("auto"),
        "server": meta.get("server").cloned().unwrap_or_else(|| json!({})),
        "players": meta.get("players").cloned().unwrap_or_else(|| json!([])),
        "referee": meta.get("referee").cloned().unwrap_or_else(|| json!({})),
        "next": meta.get("next").cloned().unwrap_or(Value::Null),
        "maxRallies": t.max_rallies,
        "leaseMs": wall_ms,
        "leaseExpiresAt": t.started_ms.filter(|_| is_active(status)).map(|s| iso(s + wall_ms)),
        "cwd": meta.get("cwd").cloned().unwrap_or(Value::Null),
        "parentSessionId": meta.get("parentSessionId").cloned().unwrap_or(Value::Null),
        "ownsParent": false,
        "chainFrom": meta.get("chainFrom").cloned().unwrap_or(Value::Null),
        "chainNextId": chain_next_id(store, t),
        "rallyCount": t.rallies,
        "evidence": evidence,
        "createdAt": iso(t.created_ms),
        "updatedAt": iso(ended.or(t.started_ms).unwrap_or(t.created_ms)),
        "startedAt": t.started_ms.map(iso),
        "finishedAt": ended.map(iso),
        "score": {"rallies": t.rallies, "maxRallies": t.max_rallies, "point": status == "done"},
    });
    if detail {
        let pending = store
            .pending(Some(&t.id))
            .ok()
            .and_then(|p| p.into_iter().next());
        let ball_status = match status {
            "waiting_server" => "waiting_server",
            "done" => "done",
            "stopped" => "stopped",
            "lost" | "failed" => "lost",
            _ => "open",
        };
        v["ball"] = json!({
            "id": format!("{}:1", t.id), "tableId": t.id, "seq": 1,
            "brief": meta["brief"].as_str().filter(|b| !b.is_empty()).unwrap_or(&t.goal),
            "expectedEvidence": meta.get("expectedEvidence").cloned().unwrap_or(Value::Null),
            "status": ball_status, "playerSessionId": t.session,
            "pendingMessage": pending.as_ref().map(|p| p.payload.clone()),
            "createdAt": iso(t.created_ms), "updatedAt": v["updatedAt"],
        });
        v["waitingForServer"] = json!(status == "waiting_server" && pending.is_some());
        // While a ball waits for its external server, its rally is not stored yet: show it
        // from the ball payload, as OpenUnum records a rally before it parks the ball.
        let parked = pending.as_ref().filter(|p| rallies.iter().all(|r| r.n != p.rally)).map(|p| {
            json!({
                "id": Value::Null, "tableId": t.id, "ballId": format!("{}:1", t.id), "n": p.rally,
                "playerSessionId": t.session, "playerStatus": "completed",
                "playerReply": p.payload["card"]["summary"],
                "referee": referee_view(&p.payload["referee"]["checks"], p.payload["referee"]["passed"] == true),
                "passed": p.payload["referee"]["passed"] == true, "serverMode": v["serverMode"],
                "serverNote": Value::Null, "returnMessage": Value::Null, "createdAt": iso(p.created_ms),
            })
        });
        v["rallies"] = json!(rallies
            .iter()
            .map(|r| json!({
                "id": r.id, "tableId": t.id, "ballId": format!("{}:1", t.id), "n": r.n,
                "playerSessionId": r.session, "playerStatus": if r.verdict == "fault" { "failed" } else { "completed" },
                "playerReply": r.answer, "referee": referee_view(&r.checks, r.passed),
                "passed": r.passed, "serverMode": v["serverMode"],
                "serverNote": r.note, "returnMessage": r.ball, "createdAt": iso(r.ended_ms),
            }))
            .chain(parked)
            .collect::<Vec<_>>());
        v["mergeLog"] = json!(store.merge_log(&t.id).unwrap_or_default());
    }
    v
}

/// `GET /api/pingpong/tables`
pub fn list(store: &Store, status: Option<&str>, limit: usize) -> (u16, Value) {
    let rows = match store.tables(None, false) {
        Ok(r) => r,
        Err(e) => return err(500, "store_error", json!([{"field": "store", "issue": e}])),
    };
    let limit = limit.clamp(1, 500);
    let tables: Vec<Value> = rows
        .iter()
        .filter(|t| status.is_none_or(|s| status_of(t) == s))
        .take(limit)
        .map(|t| view(store, t, false))
        .collect();
    (200, json!({"ok": true, "tables": tables}))
}

fn find(store: &Store, id: &str) -> Option<TableRow> {
    store.table(id).ok().flatten()
}

/// `GET /api/pingpong/tables/{id}`
pub fn get(store: &Store, id: &str) -> (u16, Value) {
    match find(store, id) {
        Some(t) => (200, json!({"ok": true, "table": view(store, &t, true)})),
        None => err(404, "table_not_found", Value::Null),
    }
}

/// `POST /api/pingpong/tables/{id}/answer`: `{note?, verdict?: return|accept|stop}`.
pub fn answer(store: &Store, id: &str, body: &Value) -> (u16, Value) {
    let mut errors = Vec::new();
    if !body.is_object() {
        bad(&mut errors, "body", "expected JSON object");
    }
    if body.get("note").is_some_and(|n| !n.is_string()) {
        bad(&mut errors, "note", "expected string");
    }
    if body
        .get("verdict")
        .is_some_and(|v| !matches!(v.as_str(), Some("return" | "accept" | "stop")))
    {
        bad(&mut errors, "verdict", "one of return|accept|stop");
    }
    if !errors.is_empty() {
        return err(400, "invalid_payload", json!(errors));
    }
    let Some(t) = find(store, id) else {
        return err(404, "table_not_found", Value::Null);
    };
    let status = status_of(&t);
    if status != "waiting_server" {
        let mut e = err(409, "not_waiting_for_server", Value::Null);
        e.1["status"] = json!(status);
        return e;
    }
    let verdict = body["verdict"].as_str().unwrap_or("return");
    let note = body["note"].as_str().unwrap_or("");
    if let Err(e) = api::card_answer(store, &t.id, verdict, note, None, None) {
        return err(
            409,
            "not_waiting_for_server",
            json!([{"field": "table", "issue": e}]),
        );
    }
    let t = find(store, &t.id).unwrap_or(t);
    let mut table = view(store, &t, false);
    // The engine picks the answer up asynchronously; report where the ball goes next.
    if table["status"] == "waiting_server" {
        table["status"] = json!(match verdict {
            "stop" => "stopped",
            "accept" => "serving",
            _ => "queued",
        });
    }
    (200, json!({"ok": true, "table": table}))
}

/// `POST /api/pingpong/tables/{id}/stop`: `{reason?}`.
pub fn stop(store: &Store, id: &str, body: &Value) -> (u16, Value) {
    if body.get("reason").is_some_and(|r| !r.is_string()) {
        return err(
            400,
            "invalid_payload",
            json!([{"field": "reason", "issue": "expected string"}]),
        );
    }
    let Some(t) = find(store, id) else {
        return err(404, "table_not_found", Value::Null);
    };
    let status = status_of(&t);
    if !is_active(status) {
        let mut e = err(409, "table_not_active", Value::Null);
        e.1["status"] = json!(status);
        return e;
    }
    if let Err(e) = api::stop(store, &t.id) {
        return err(
            409,
            "table_not_active",
            json!([{"field": "table", "issue": e}]),
        );
    }
    let t = find(store, &t.id).unwrap_or(t);
    let mut table = view(store, &t, false);
    if is_active(table["status"].as_str().unwrap_or("")) {
        table["status"] = json!("stopped");
        table["endReason"] = json!(body["reason"].as_str().unwrap_or("stopped_by_request"));
    }
    (200, json!({"ok": true, "table": table}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_formats_epoch_ms() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
        assert_eq!(iso(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn validation_matches_the_openunum_contract() {
        let ok = json!({"goal": "g", "referee": {"cmd": "true"}});
        assert!(validate(&ok, false, 0).is_empty());
        let e = validate(&json!({"referee": {}}), false, 0);
        let fields: Vec<_> = e.iter().map(|x| x["field"].as_str().unwrap()).collect();
        assert_eq!(fields, ["goal", "referee"]);
        let e = validate(
            &json!({"goal": "g", "referee": {"checks": [{"type": "nope"}]}, "maxRallies": 0,
                    "server": {"mode": "x"}, "next": {"goal": "n", "referee": {"cmd": ""}}}),
            false,
            0,
        );
        let fields: Vec<_> = e.iter().map(|x| x["field"].as_str().unwrap()).collect();
        assert!(fields.contains(&"referee.checks[0]"), "{fields:?}");
        assert!(fields.contains(&"maxRallies") && fields.contains(&"server.mode"));
        assert!(fields.contains(&"next.referee.cmd"), "{fields:?}");
        // A chain template may leave referee and goal out.
        assert!(validate(&json!({"referee": {"cmd": "true"}, "next": {}}), false, 0).len() == 1);
    }

    #[test]
    fn interpolation_binds_goal_and_previous_answer() {
        assert_eq!(
            interpolate("do {{goal}} using {{ reply }} / {{evidence}}", "G", true),
            "do G using {{prev.answer}} / {{prev.answer}}"
        );
    }
}
