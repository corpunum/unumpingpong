// SPDX-License-Identifier: Apache-2.0
//! Front-end-neutral operations shared by the CLI, the daemon (HTTP) and the MCP server.
//! Every function takes the store and returns JSON.

use crate::config::MatchFile;
use crate::proc;
use crate::store::{now_ms, Store, TableRow};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

/// Compact view of a table for listings.
pub fn summary(t: &TableRow) -> Value {
    let now = now_ms();
    json!({
        "id": t.id,
        "run_id": t.run_id,
        "name": t.name,
        "status": t.status,
        "phase": t.phase,
        "player": t.player,
        "rallies": t.rallies,
        "max_rallies": t.max_rallies,
        "trail": t.trail,
        "elapsed_s": t.elapsed_ms(now) as f64 / 1000.0,
        "wall_s": t.wall_secs,
        "tokens": t.tokens,
        "cost_usd": t.cost_usd,
        "after": t.after_id,
        "reason": t.reason,
    })
}

/// Create tables from a match (TOML/JSON text or a JSON value). They start queued;
/// a running scheduler (uppd, `upp mcp`, `upp run`) claims them.
pub fn create(store: &Store, m: MatchFile, source: Option<&str>) -> Result<Value, String> {
    let tables = m.resolve()?;
    let (run_id, ids) = store.create_run(m.name.as_deref(), source, &tables)?;
    Ok(json!({
        "run_id": run_id,
        "tables": tables.iter().zip(ids.iter()).map(|(t, id)| json!({"id": id, "name": t.name, "after": t.after})).collect::<Vec<_>>(),
        "max_parallel": m.defaults.max_parallel,
    }))
}

pub fn create_json(store: &Store, v: Value) -> Result<Value, String> {
    let m = match &v {
        Value::String(s) => MatchFile::parse(s)?,
        _ => MatchFile::from_json(v)?,
    };
    create(store, m, Some("api"))
}

pub fn list(store: &Store, active_only: bool, run: Option<&str>) -> Result<Value, String> {
    let t = store.tables(run, active_only)?;
    Ok(
        json!({"tables": t.iter().map(summary).collect::<Vec<_>>(), "pending": store.pending(None)?.len()}),
    )
}

/// Full status of one table: summary, current ball, rallies (answers truncated unless `full`),
/// and the ball waiting for an external server, if any.
pub fn status(store: &Store, id: &str, full: bool) -> Result<Value, String> {
    let t = store.resolve(id)?;
    let cut = |s: &str, n: usize| {
        if full {
            s.to_string()
        } else {
            proc::head(s, n)
        }
    };
    let rallies: Vec<Value> = store
        .rallies(&t.id)?
        .iter()
        .map(|r| {
            json!({
                "n": r.n, "verdict": r.verdict, "passed": r.passed,
                "ball": cut(&r.ball, 600), "answer": cut(&r.answer, 1200), "note": cut(&r.note, 600),
                "checks": r.checks, "tokens": r.tokens_in + r.tokens_out, "cost_usd": r.cost_usd,
                "duration_s": (r.ended_ms - r.started_ms) as f64 / 1000.0,
            })
        })
        .collect();
    let mut v = summary(&t);
    v["goal"] = json!(t.goal);
    v["current_ball"] = json!(t.current_ball.as_deref().map(|b| cut(b, 2000)));
    v["result"] = json!(t.result.as_deref().map(|b| cut(b, 4000)));
    v["session"] = json!(t.session);
    v["rally_log"] = json!(rallies);
    v["pending"] = json!(store.pending(Some(&t.id))?.first().map(|p| &p.payload));
    Ok(v)
}

pub fn answer(store: &Store, id: &str, verdict: &str, note: &str) -> Result<Value, String> {
    if !matches!(verdict, "accept" | "return" | "stop") {
        return Err(format!(
            "verdict must be accept, return or stop (got '{verdict}')"
        ));
    }
    let t = store.resolve(id)?;
    let rally = store.answer(&t.id, &json!({"verdict": verdict, "note": note}))?;
    Ok(json!({"ok": true, "table_id": t.id, "rally": rally, "verdict": verdict}))
}

pub fn stop(store: &Store, id: &str) -> Result<Value, String> {
    let t = store.resolve(id)?;
    store.request_stop(&t.id)?;
    Ok(
        json!({"ok": true, "table_id": t.id, "note": "stops before the next rally (queued tables stop at once)"}),
    )
}

/// Block until one of the tables (or any table, when `ids` is empty) waits for its
/// server or every one of them has ended, or `timeout` passes.
pub fn wait(db: &Path, ids: &[String], timeout: Duration) -> Result<Value, String> {
    let store = Store::open(db)?;
    let until = Instant::now() + timeout;
    loop {
        let rows: Vec<TableRow> = if ids.is_empty() {
            store.tables(None, true)?
        } else {
            ids.iter()
                .map(|i| store.resolve(i))
                .collect::<Result<Vec<_>, _>>()?
        };
        let pending = store.pending(None)?;
        let pend: Vec<&crate::store::PendingBall> = pending
            .iter()
            .filter(|p| rows.iter().any(|r| r.id == p.table_id))
            .collect();
        let all_done = rows.iter().all(|r| r.is_terminal());
        if !pend.is_empty() || all_done || Instant::now() >= until {
            return Ok(json!({
                "reason": if !pend.is_empty() { "awaiting_server" } else if all_done { "ended" } else { "timeout" },
                "pending": pend.iter().map(|p| &p.payload).collect::<Vec<_>>(),
                "tables": rows.iter().map(summary).collect::<Vec<_>>(),
            }));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// One JSON object per rally (oldest first), with table context: trace / training data.
pub fn export_jsonl(store: &Store, run: Option<&str>) -> Result<String, String> {
    let mut out = String::new();
    let mut cache: std::collections::HashMap<String, TableRow> = Default::default();
    for r in store.all_rallies(run)? {
        if !cache.contains_key(&r.table_id) {
            if let Some(t) = store.table(&r.table_id)? {
                cache.insert(r.table_id.clone(), t);
            }
        }
        let t = cache.get(&r.table_id);
        let line = json!({
            "run_id": t.map(|t| &t.run_id),
            "table_id": r.table_id,
            "table": t.map(|t| &t.name),
            "goal": t.map(|t| &t.goal),
            "player": t.map(|t| &t.player),
            "table_status": t.map(|t| &t.status),
            "rally": r.n,
            "ball": r.ball,
            "answer": r.answer,
            "referee": r.checks,
            "passed": r.passed,
            "verdict": r.verdict,
            "server_note": r.note,
            "tokens_in": r.tokens_in,
            "tokens_out": r.tokens_out,
            "cost_usd": r.cost_usd,
            "session": r.session,
            "started_ms": r.started_ms,
            "ended_ms": r.ended_ms,
        });
        out.push_str(&line.to_string());
        out.push('\n');
    }
    Ok(out)
}
