// SPDX-License-Identifier: Apache-2.0
//! One table, start to end: the rally loop.
//!
//! Rally: the player answers -> the referee checks -> the server reviews ->
//! the ball goes back (referee output + server note) or the point is scored.

use crate::config::{Check, ServerMode, TableSpec};
use crate::player::{self, PlayCtx, Player};
use crate::proc;
use crate::referee::{self, RefCtx, Verdict};
use crate::store::{now_ms, Progress, RallyRow, Store};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How the server decided a rally.
#[derive(Debug, Clone, PartialEq)]
enum Decision {
    Point,
    Return(String),
    Stop,
    Expire(String),
}

/// Replace `{{key}}` placeholders.
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut s = template.to_string();
    for (k, v) in vars {
        s = s.replace(&format!("{{{{{k}}}}}"), v);
    }
    s
}

/// Replace `{{file:REL}}` with the content of `cwd/REL` (or a note when it is missing).
pub fn render_files(s: &str, cwd: &Path) -> String {
    let re = regex::Regex::new(r"\{\{file:([^}]+)\}\}").expect("static regex");
    re.replace_all(s, |c: &regex::Captures| {
        let rel = c[1].trim();
        std::fs::read_to_string(cwd.join(rel))
            .unwrap_or_else(|e| format!("(cannot read {rel}: {e})"))
    })
    .into_owned()
}

/// First fenced code block of a text (```lang ... ```), if any.
pub fn first_code_block(s: &str) -> Option<String> {
    let start = s.find("```")?;
    let after = &s[start + 3..];
    let body_start = after.find('\n')? + 1;
    let body = &after[body_start..];
    let end = body.find("```")?;
    Some(body[..end].to_string())
}

fn apply(a: &crate::config::Apply, answer: &str, cwd: &Path) -> referee::CheckResult {
    let content = if a.from == "answer" {
        answer.to_string()
    } else {
        first_code_block(answer).unwrap_or_else(|| answer.to_string())
    };
    let path = cwd.join(&a.write_file);
    let res = path
        .parent()
        .map(std::fs::create_dir_all)
        .unwrap_or(Ok(()))
        .and_then(|_| std::fs::write(&path, &content));
    let (pass, summary) = match res {
        Ok(()) => (
            true,
            format!("wrote {} bytes to {}", content.len(), a.write_file),
        ),
        Err(e) => (false, format!("cannot write {}: {e}", a.write_file)),
    };
    referee::CheckResult {
        name: format!("apply {}", a.write_file),
        kind: "apply".into(),
        pass,
        skipped: false,
        summary,
        output: String::new(),
        duration_ms: 0,
    }
}

/// What the referee will check, appended to the first ball so the player knows the evidence expected.
fn describe_checks(spec: &TableSpec) -> String {
    let mut s = String::new();
    if let Some(a) = &spec.apply {
        s.push_str(&format!(
            "\n\n---\nAnswer with the complete new content of `{}` in one fenced code block; \
             it is written to that file before the checks run.",
            a.write_file
        ));
    }
    if spec.referee.is_empty() {
        return s;
    }
    s.push_str("\n\n---\nAn external referee will check your work:\n");
    for c in &spec.referee {
        let line = match &c.check {
            Check::Command {
                run, expect_regex, ..
            } => match expect_regex {
                Some(r) => format!("`{}` exits 0 and its output matches /{r}/", run.display()),
                None => format!("`{}` exits 0", run.display()),
            },
            Check::FileExists { path, contains } => match contains {
                Some(r) => format!("file `{path}` exists and matches /{r}/"),
                None => format!("file `{path}` exists"),
            },
            Check::HttpOk { url, .. } => format!("GET {url} answers 2xx"),
            Check::JsonPathEquals {
                path,
                equals,
                file,
                url,
            } => {
                let src = file
                    .clone()
                    .or(url.clone())
                    .unwrap_or_else(|| "your answer (as JSON)".into());
                format!("in {src}, `{path}` equals {equals}")
            }
            Check::Judge { .. } => "an independent reviewer reads your answer".into(),
        };
        s.push_str(&format!("- {line}\n"));
    }
    s.push_str("Answer when you believe these checks pass. Say briefly what you changed.");
    s
}

fn return_message(
    n: u32,
    verdict: &Verdict,
    note: &str,
    standing: Option<&str>,
    brief: Option<&str>,
) -> String {
    let mut s = format!("Rally {n}: the referee did not accept this yet.\n\n");
    let report = verdict.report(true);
    if !report.trim().is_empty() {
        s.push_str("Failed checks:\n");
        s.push_str(&report);
        s.push('\n');
    } else if verdict.passed {
        s.push_str("The checks passed, but the server returned the ball.\n\n");
    }
    if !note.trim().is_empty() {
        s.push_str(&format!("Server note: {}\n\n", note.trim()));
    }
    if let Some(st) = standing {
        s.push_str(&format!("{}\n\n", st.trim()));
    }
    if let Some(b) = brief {
        s.push_str(&format!("Original ball (repeated):\n{b}\n\n"));
    }
    s.push_str("Fix it and answer again; the same checks will run.");
    s
}

fn fault_message(err: &str, brief: Option<&str>) -> String {
    let mut s = format!(
        "Your previous turn failed before the referee could run:\n{}\n\nPlease try again.",
        proc::tail(err, 2000)
    );
    if let Some(b) = brief {
        s.push_str(&format!("\n\nOriginal ball (repeated):\n{b}"));
    }
    s
}

/// Run one claimed table to its end. Returns the final status.
pub fn run_table(db: &Path, table_id: &str) -> String {
    let store = match Store::open(db) {
        Ok(s) => s,
        Err(_) => return "error".into(),
    };
    match run_inner(&store, table_id) {
        Ok(status) => status,
        Err(e) => {
            let _ = store.finish(table_id, "error", None, &e);
            "error".into()
        }
    }
}

struct Server {
    mode: ServerMode,
    review_points: bool,
    timeout: Duration,
    standing: Option<String>,
    model: Option<Box<dyn Player>>,
}

fn run_inner(store: &Store, id: &str) -> Result<String, String> {
    let spec = store.spec(id)?;
    let row = store.table(id)?.ok_or("table vanished")?;
    let lease = spec.lease.clone().unwrap_or_default();
    let start = Instant::now();
    let deadline = start + Duration::from_secs(lease.wall());
    let cwd = PathBuf::from(spec.cwd.clone().unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&cwd).map_err(|e| format!("cwd {}: {e}", cwd.display()))?;
    if let Some(setup) = &spec.setup {
        store.set_phase(id, "setup", None)?;
        let out = proc::run(
            &setup.argv(),
            &cwd,
            None,
            &Default::default(),
            deadline.saturating_duration_since(Instant::now()),
        )?;
        store.event(
            id,
            "setup",
            json!({"ok": out.ok(), "output": proc::tail(&out.combined(), 400)}),
        )?;
        if !out.ok() {
            return Err(format!(
                "setup `{}` failed ({}): {}",
                setup.display(),
                out.code.map(|c| c.to_string()).unwrap_or("timeout".into()),
                proc::tail(out.combined().trim(), 800)
            ));
        }
    }

    // Serial chaining: the upstream point becomes part of this ball.
    let (prev_name, prev_answer) = match &row.after_id {
        Some(a) => {
            let up = store.table(a)?.ok_or("upstream table vanished")?;
            (up.name, up.result.unwrap_or_default())
        }
        None => (String::new(), String::new()),
    };
    let goal = if spec.goal.trim().is_empty() {
        spec.ball.clone()
    } else {
        spec.goal.clone()
    };
    let ball_t = if spec.ball.trim().is_empty() {
        spec.goal.clone()
    } else {
        spec.ball.clone()
    };
    let vars = [
        ("goal", goal.as_str()),
        ("prev.answer", prev_answer.as_str()),
        ("prev.name", prev_name.as_str()),
        ("table", spec.name.as_str()),
    ];
    let brief = render_files(&render(&ball_t, &vars), &cwd) + &describe_checks(&spec);
    let goal = render(&goal, &vars);

    let base = PlayCtx {
        table_id: id.to_string(),
        rally: 0,
        cwd: cwd.clone(),
        deadline,
        session_key: id.to_string(),
    };
    let pspec = spec.player.clone().ok_or("no player")?;
    let mut player = player::build(&pspec, &base)?;
    let sspec = spec.server.clone().unwrap_or_default();
    let mut server = Server {
        mode: sspec.mode,
        review_points: sspec
            .review_points
            .unwrap_or(sspec.mode == ServerMode::External),
        timeout: Duration::from_secs(sspec.timeout_secs.unwrap_or(3600)),
        standing: sspec.note.clone(),
        model: match (&sspec.mode, &sspec.player) {
            (ServerMode::Model, Some(p)) => Some(player::build(p, &base)?),
            _ => None,
        },
    };

    let mut prog = Progress::default();
    let mut msg = brief.clone();
    for n in 1..=lease.rallies() {
        if store.stop_requested(id)? {
            store.finish(id, "stopped", None, "stop requested")?;
            return Ok("stopped".into());
        }
        if Instant::now() >= deadline {
            store.finish(
                id,
                "expired",
                None,
                &format!("wall lease of {}s used up", lease.wall()),
            )?;
            return Ok("expired".into());
        }
        if let Some(mt) = lease.max_tokens {
            if prog.tokens >= mt {
                store.finish(id, "expired", None, &format!("token lease of {mt} used up"))?;
                return Ok("expired".into());
            }
        }
        if let Some(mc) = lease.max_cost_usd {
            if prog.cost_usd >= mc {
                store.finish(id, "expired", None, &format!("cost lease of ${mc} used up"))?;
                return Ok("expired".into());
            }
        }
        store.set_phase(id, "player", Some(&msg))?;
        store.event(id, "rally", json!({"n": n, "ball": proc::head(&msg, 400)}))?;
        let started = now_ms();
        let ctx = PlayCtx {
            rally: n,
            ..base.clone()
        };
        let repeat_brief = if player.keeps_session() {
            None
        } else {
            Some(brief.as_str())
        };
        let reply = match player.play(&msg, &ctx) {
            Ok(r) => r,
            Err(err) => {
                prog.rallies = n;
                prog.trail.push('!');
                store.append_rally(&RallyRow {
                    id: 0,
                    table_id: id.into(),
                    n,
                    ball: msg.clone(),
                    answer: String::new(),
                    checks: json!([]),
                    passed: false,
                    verdict: "fault".into(),
                    note: err.clone(),
                    tokens_in: 0,
                    tokens_out: 0,
                    cost_usd: 0.0,
                    session: None,
                    started_ms: started,
                    ended_ms: now_ms(),
                })?;
                store.progress(id, &prog)?;
                store.event(id, "fault", json!({"n": n, "error": proc::head(&err, 400)}))?;
                msg = fault_message(&err, repeat_brief);
                continue;
            }
        };
        prog.rallies = n;
        prog.tokens += reply.tokens_in + reply.tokens_out;
        prog.cost_usd += reply.cost_usd;
        if reply.session.is_some() {
            prog.session = reply.session.clone();
        }
        prog.last_answer = Some(reply.text.clone());
        store.progress(id, &prog)?;
        store.event(
            id,
            "answer",
            json!({"n": n, "answer": proc::head(&reply.text, 400), "tokens": reply.tokens_in + reply.tokens_out}),
        )?;

        store.set_phase(id, "referee", None)?;
        let applied = spec.apply.as_ref().map(|a| apply(a, &reply.text, &cwd));
        let mut verdict = referee::referee(
            &spec.referee,
            &RefCtx {
                table_id: id,
                rally: n,
                cwd: &cwd,
                goal: &goal,
                ball: &brief,
                answer: &reply.text,
                deadline,
            },
        );
        if let Some(a) = applied {
            verdict.passed = verdict.passed && a.pass;
            verdict.checks.insert(0, a);
        }
        store.event(
            id,
            "referee",
            json!({"n": n, "passed": verdict.passed,
                   "checks": verdict.checks.iter().map(|c| json!({"name": c.name, "pass": c.pass, "summary": c.summary})).collect::<Vec<_>>()}),
        )?;

        let decision = decide(
            store,
            id,
            n,
            &mut server,
            &goal,
            &brief,
            &reply.text,
            &verdict,
            deadline,
            &ctx,
        )?;
        let (verdict_s, note) = match &decision {
            Decision::Point => ("point", String::new()),
            Decision::Return(note) => ("return", note.clone()),
            Decision::Stop => ("return", "stopped by the server".into()),
            Decision::Expire(r) => ("return", r.clone()),
        };
        prog.trail
            .push(if verdict_s == "point" { '*' } else { '.' });
        store.append_rally(&RallyRow {
            id: 0,
            table_id: id.into(),
            n,
            ball: msg.clone(),
            answer: reply.text.clone(),
            checks: serde_json::to_value(&verdict.checks).unwrap_or(Value::Null),
            passed: verdict.passed,
            verdict: verdict_s.into(),
            note: note.clone(),
            tokens_in: reply.tokens_in,
            tokens_out: reply.tokens_out,
            cost_usd: reply.cost_usd,
            session: reply.session.clone(),
            started_ms: started,
            ended_ms: now_ms(),
        })?;
        store.progress(id, &prog)?;
        match decision {
            Decision::Point => {
                store.finish(
                    id,
                    "point",
                    Some(&reply.text),
                    &format!("point on rally {n}"),
                )?;
                return Ok("point".into());
            }
            Decision::Stop => {
                store.finish(id, "stopped", None, "stopped by the server")?;
                return Ok("stopped".into());
            }
            Decision::Expire(r) => {
                store.finish(id, "expired", None, &r)?;
                return Ok("expired".into());
            }
            Decision::Return(note) => {
                store.event(
                    id,
                    "return",
                    json!({"n": n, "note": proc::head(&note, 400)}),
                )?;
                msg = return_message(n, &verdict, &note, server.standing.as_deref(), repeat_brief);
            }
        }
    }
    store.finish(
        id,
        "lost",
        None,
        &format!("{} rallies without a point", lease.rallies()),
    )?;
    Ok("lost".into())
}

#[allow(clippy::too_many_arguments)]
fn decide(
    store: &Store,
    id: &str,
    n: u32,
    server: &mut Server,
    goal: &str,
    brief: &str,
    answer: &str,
    verdict: &Verdict,
    deadline: Instant,
    ctx: &PlayCtx,
) -> Result<Decision, String> {
    if verdict.passed && !server.review_points {
        return Ok(Decision::Point);
    }
    match server.mode {
        ServerMode::Auto => Ok(if verdict.passed {
            Decision::Point
        } else {
            Decision::Return(String::new())
        }),
        ServerMode::Model => {
            store.set_phase(id, "server", None)?;
            let ask = if verdict.passed {
                "The referee passed it. Reply ACCEPT (first word) if the answer fully meets the goal; \
                 otherwise write the correction for the worker."
            } else {
                "Write the correction for the worker: at most 8 lines, precise and actionable, no praise. \
                 Do not solve the task yourself."
            };
            let prompt = format!(
                "You are the server (orchestrator) at a ping-pong table. A worker answered a ball and an \
                 external referee checked it.\n\nGoal:\n{goal}\n\nBall:\n{}\n\nWorker's answer (rally {n}):\n{}\n\n\
                 Referee:\n{}\n{ask}",
                proc::head(brief, 4000),
                proc::head(answer, 8000),
                verdict.report(false)
            );
            let sctx = PlayCtx {
                session_key: format!("{id}-server"),
                ..ctx.clone()
            };
            let model = server
                .model
                .as_mut()
                .ok_or("model server without a player")?;
            match model.play(&prompt, &sctx) {
                Ok(r) => {
                    let t = r.text.trim();
                    if verdict.passed && t.to_ascii_uppercase().starts_with("ACCEPT") {
                        Ok(Decision::Point)
                    } else {
                        Ok(Decision::Return(t.to_string()))
                    }
                }
                // The server model failing must not cost the point: fall back to the referee.
                Err(e) => {
                    store.event(id, "server_error", json!({"error": proc::head(&e, 300)}))?;
                    Ok(if verdict.passed {
                        Decision::Point
                    } else {
                        Decision::Return(String::new())
                    })
                }
            }
        }
        ServerMode::External => {
            let payload = json!({
                "table_id": id,
                "rally": n,
                "goal": goal,
                "ball": proc::head(brief, 8000),
                "answer": proc::head(answer, 16000),
                "referee": {"passed": verdict.passed, "checks": verdict.checks},
                "how_to_answer": "ball_answer(table_id, verdict = accept | return | stop, note). \
                                  accept scores the point only if the referee passed; return sends the note back.",
            });
            store.push_pending(id, n, &payload)?;
            store.set_status(id, "awaiting_server")?;
            store.set_phase(id, "server", None)?;
            store.event(
                id,
                "awaiting_server",
                json!({"n": n, "passed": verdict.passed}),
            )?;
            let until = Instant::now() + server.timeout;
            let answer = loop {
                if let Some(a) = store.take_answer(id, n)? {
                    break Some(a);
                }
                if store.stop_requested(id)? {
                    store.set_status(id, "running")?;
                    return Ok(Decision::Stop);
                }
                let now = Instant::now();
                if now >= until || now >= deadline {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(200));
            };
            store.set_status(id, "running")?;
            let Some(a) = answer else {
                return Ok(Decision::Expire(
                    "the external server did not answer in time".into(),
                ));
            };
            let note = a["note"].as_str().unwrap_or("").to_string();
            store.event(id, "server_answer", json!({"n": n, "answer": a}))?;
            match a["verdict"].as_str().unwrap_or("return") {
                "accept" if verdict.passed => Ok(Decision::Point),
                "accept" => Ok(Decision::Return(format!(
                    "{note}\n(The server accepted, but the referee still fails: the referee decides.)"
                ))),
                "stop" => Ok(Decision::Stop),
                _ => Ok(Decision::Return(note)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_blocks_and_file_templates() {
        assert_eq!(
            first_code_block("Here:\n```rust\nfn a() {}\n```\nbye").as_deref(),
            Some("fn a() {}\n")
        );
        assert_eq!(first_code_block("no code"), None);
        let d = std::env::temp_dir();
        let name = format!("upp-render-{}.txt", std::process::id());
        std::fs::write(d.join(&name), "CONTENT").unwrap();
        let out = render_files(
            &format!("a {{{{file:{name}}}}} b {{{{file:missing.txt}}}}"),
            &d,
        );
        assert!(
            out.starts_with("a CONTENT b (cannot read missing.txt"),
            "{out}"
        );
        let _ = std::fs::remove_file(d.join(name));
    }

    #[test]
    fn render_replaces_placeholders() {
        assert_eq!(
            render(
                "a {{prev.answer}} b {{goal}}",
                &[("prev.answer", "X"), ("goal", "G")]
            ),
            "a X b G"
        );
    }
}
