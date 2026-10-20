// SPDX-License-Identifier: Apache-2.0
//! One table, start to end: the rally loop.
//!
//! Rally: the player answers -> the referee checks -> the server reviews ->
//! the ball goes back (referee output + server note) or the point is scored.

use crate::card;
use crate::config::{Check, ServerMode, TableSpec};
use crate::git;
use crate::player::{self, PlayCtx, Player, Reply};
use crate::proc;
use crate::referee::{self, RefCtx, Verdict};
use crate::search;
use crate::store::{now_ms, Progress, RallyRow, Store};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How the server decided a rally.
#[derive(Debug, Clone, PartialEq)]
enum Decision {
    Point,
    Return(String),
    Stop,
    Expire(String),
    /// Fail closed: the table ends as `error` (e.g. the reviewer stayed unavailable).
    Fail(String),
}

/// Attempts at a model server's review of a ball the referee passed, before failing closed.
pub const REVIEW_ATTEMPTS: u32 = 3;

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
    let answer = card::strip(answer);
    let content = if a.from == "answer" {
        answer.clone()
    } else {
        first_code_block(&answer).unwrap_or_else(|| answer.clone())
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
    let card_note = if spec.card != Some(false) {
        format!("\n\n---\n{}", card::CARD_INSTRUCTION)
    } else {
        String::new()
    };
    if spec.referee.is_empty() {
        return s + &card_note;
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
    s + &card_note
}

/// Referee output a returned ball carries per failing check: its head (the first error)
/// and its tail (the summary). A player that keeps its session gets only this delta.
pub const RETURN_HEAD: usize = 700;
pub const RETURN_TAIL: usize = 1300;

fn return_message(
    n: u32,
    verdict: &Verdict,
    note: &str,
    standing: Option<&str>,
    brief: Option<&str>,
) -> String {
    let mut s = format!("Rally {n}: the referee did not accept this yet.\n");
    let failed: Vec<&referee::CheckResult> = verdict
        .checks
        .iter()
        .filter(|c| !c.pass && !c.skipped)
        .collect();
    if !failed.is_empty() {
        s.push_str("Failed checks:\n");
        for c in failed {
            s.push_str(&format!("- [FAIL] {}: {}\n", c.name, c.summary));
            let out = c.output.trim();
            if !out.is_empty() {
                s.push_str("```\n");
                s.push_str(&proc::head_tail(out, RETURN_HEAD, RETURN_TAIL));
                s.push_str("\n```\n");
            }
        }
    } else if verdict.passed {
        s.push_str("The checks passed, but the server returned the ball.\n");
    }
    if !note.trim().is_empty() {
        s.push_str(&format!("Server note: {}\n", note.trim()));
    }
    if let Some(st) = standing {
        s.push_str(&format!("{}\n", st.trim()));
    }
    if let Some(b) = brief {
        s.push_str(&format!("\nOriginal ball (repeated):\n{b}\n\n"));
    }
    s.push_str("Fix it and answer again; the same checks will run.");
    s
}

fn fault_message(err: &str, brief: Option<&str>) -> String {
    let mut s = format!(
        "Your previous turn failed before the referee could run:\n{}\n\nPlease try again.",
        proc::head_tail(err.trim(), RETURN_HEAD, RETURN_TAIL)
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
    transcript: bool,
    model: Option<Box<dyn Player>>,
}

/// Watches the store for a stop request and sets the cancel flag (killing the running
/// player call). Ends when `done` is set.
fn cancel_watcher(db: PathBuf, id: String, cancel: Arc<AtomicBool>, done: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let Ok(store) = Store::open(&db) else { return };
        let mut w = store.watch();
        while !done.load(Ordering::SeqCst) {
            if store.stop_requested(&id).unwrap_or(false) {
                cancel.store(true, Ordering::SeqCst);
                return;
            }
            store.wait(&mut w, Duration::from_secs(1));
        }
    });
}

struct DoneOnDrop(Arc<AtomicBool>);
impl Drop for DoneOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Set up the table's git worktree (created on first start, reused on resume).
fn setup_worktree(
    store: &Store,
    id: &str,
    spec: &TableSpec,
    w: &crate::config::WorktreeSpec,
) -> Result<PathBuf, String> {
    let row = store.table(id)?.ok_or("table vanished")?;
    if let Some(rec) = &row.worktree {
        if let Some(p) = rec["path"].as_str() {
            if Path::new(p).join(".git").exists() {
                return Ok(PathBuf::from(p));
            }
        }
    }
    let repo = PathBuf::from(w.repo.clone().unwrap_or_else(|| ".".into()));
    let path = match &w.path {
        Some(p) => PathBuf::from(p),
        None => {
            let dir = store
                .path()
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            std::path::absolute(dir.join("worktrees").join(id))
                .map_err(|e| format!("worktree path: {e}"))?
        }
    };
    let branch = w
        .branch
        .clone()
        .unwrap_or_else(|| git::branch_name(&spec.name, id));
    let rec = git::worktree_add(&repo, &path, &branch, w.base.as_deref().unwrap_or("HEAD"))?;
    store.set_worktree(id, &rec)?;
    store.event(id, "worktree", rec.clone())?;
    Ok(path)
}

/// Block until every sub-table in `ids` has ended (the table's slot is free meanwhile),
/// then return their final cards. A stop or the wall lease stops the wait.
fn wait_children(
    store: &Store,
    id: &str,
    ids: &[String],
    deadline: Instant,
) -> Result<Vec<Value>, String> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    store.set_phase(id, "children", None)?;
    store.event(id, "waiting_children", json!({"tables": ids}))?;
    let mut w = store.watch();
    loop {
        let open: Vec<String> = ids
            .iter()
            .filter(|c| {
                store
                    .table(c)
                    .ok()
                    .flatten()
                    .is_some_and(|t| !t.is_terminal())
            })
            .cloned()
            .collect();
        if open.is_empty() {
            break;
        }
        if store.stop_requested(id)? || Instant::now() >= deadline {
            for c in &open {
                let _ = store.request_stop(c);
            }
            break;
        }
        store.wait(
            &mut w,
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(5)),
        );
    }
    let mut out = Vec::new();
    for c in ids {
        let t = store.table(c)?.ok_or("sub-table vanished")?;
        let card = store.latest_card(c, "final")?.map(|r| r.card);
        out.push(json!({
            "table_id": t.id, "table": t.name, "status": t.status, "reason": t.reason,
            "summary": card.as_ref().map(|c| c["summary"].clone()),
            "artifacts": card.as_ref().map(|c| c["artifacts"].clone()),
        }));
    }
    Ok(out)
}

/// The engine's card for one rally: the player's card plus what the engine knows.
#[allow(clippy::too_many_arguments)]
fn build_card(
    spec: &TableSpec,
    id: &str,
    n: u32,
    reply: &Reply,
    elapsed: Duration,
    verdict: &Verdict,
    prov: &Value,
    children: &[Value],
    needs_answer: bool,
) -> Value {
    let mut c = if spec.card == Some(false) {
        card::extract(&reply.text)
    } else {
        card::from_answer(&reply.text)
    };
    if let Some(a) = &spec.apply {
        if !c.artifacts.contains(&a.write_file) {
            c.artifacts.push(a.write_file.clone());
        }
    }
    if let Some(ch) = prov["changed"].as_array() {
        for f in ch.iter().filter_map(|x| x.as_str()) {
            if c.artifacts.len() < 24 && !c.artifacts.iter().any(|x| x == f) {
                c.artifacts.push(f.to_string());
            }
        }
    }
    let failed: Vec<String> = verdict
        .checks
        .iter()
        .filter(|x| !x.pass && !x.skipped)
        .map(|x| format!("{}: {}", x.name, x.summary))
        .collect();
    let tail = verdict
        .checks
        .iter()
        .find(|x| !x.pass && !x.skipped && !x.output.trim().is_empty())
        .map(|x| proc::tail(x.output.trim(), REFEREE_TAIL));
    json!({
        "table_id": id,
        "table": spec.name,
        "rally": n,
        "kind": "rally",
        "done": c.done,
        "summary": c.summary,
        "evidence": c.evidence,
        "open_issues": c.open_issues,
        "artifacts": c.artifacts,
        "source": c.source,
        "tokens": reply.billable(),
        "elapsed_s": (elapsed.as_millis() as f64 / 1000.0),
        "referee": {"passed": verdict.passed, "failed": failed, "tail": tail},
        "provenance": if prov["git"] == true {
            json!({"commit": prov["commit"], "tree": prov["tree"], "dirty": prov["dirty"], "scope": prov["scope"]})
        } else { Value::Null },
        "children": children,
        "needs_answer": needs_answer,
    })
}

/// Files that differ between the snapshot `tree` and the working tree now (untracked
/// files included). Empty outside git or when nothing changed.
fn changed_since(cwd: &Path, tree: Option<&str>) -> Vec<String> {
    let Some(tree) = tree else { return vec![] };
    let now = git::provenance(cwd, None, true);
    let (Some(repo), Some(cur)) = (now["repo"].as_str(), now["tree"].as_str()) else {
        return vec![];
    };
    if cur == tree {
        return vec![];
    }
    git::git(
        Path::new(repo),
        &["diff", "--name-only", tree, cur],
        &Default::default(),
    )
    .map(|s| s.lines().map(String::from).collect())
    .unwrap_or_default()
}

/// Referee output kept on a card (the orchestrator's view; the full output is in the transcript).
pub const REFEREE_TAIL: usize = 800;

fn run_inner(store: &Store, id: &str) -> Result<String, String> {
    let spec = store.spec(id)?;
    let row = store.table(id)?.ok_or("table vanished")?;
    let lease = spec.lease.clone().unwrap_or_default();
    let start = Instant::now();
    // The wall lease covers the whole table, across crashes and resumes: time used by
    // earlier runs is spent.
    let used_before = store.wall_used_ms(id)?;
    let deadline = start
        + Duration::from_secs(lease.wall()).saturating_sub(Duration::from_millis(used_before));
    let save_wall = |store: &Store| {
        store.set_wall_used_ms(id, used_before + start.elapsed().as_millis() as u64)
    };
    let db = std::path::absolute(store.path()).unwrap_or_else(|_| store.path().to_path_buf());
    let prior = store.rallies(id)?;
    let resuming = !prior.is_empty() || row.resumes > 0;

    // Worktree isolation: the table's cwd is its own worktree.
    let mut cwd = PathBuf::from(spec.cwd.clone().unwrap_or_else(|| ".".into()));
    if let Some(w) = &spec.worktree {
        store.set_phase(id, "setup", None)?;
        cwd = setup_worktree(store, id, &spec, w)?;
    }
    let wt_base = store
        .table(id)?
        .and_then(|t| t.worktree)
        .and_then(|w| w["base"].as_str().map(String::from));
    std::fs::create_dir_all(&cwd).map_err(|e| format!("cwd {}: {e}", cwd.display()))?;
    if let (Some(setup), false) = (&spec.setup, resuming) {
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
    // {{search:...}} before {{file:...}}: a file's own text is never expanded.
    let brief = render_files(&search::render(&render(&ball_t, &vars), &cwd), &cwd)
        + &search::context(&spec.context_search, &cwd)
        + &describe_checks(&spec);
    let goal = render(&goal, &vars);

    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let _done_guard = DoneOnDrop(done.clone());
    cancel_watcher(db.clone(), id.to_string(), cancel.clone(), done);
    let mut env = BTreeMap::new();
    env.insert("UPP_DB".into(), db.to_string_lossy().into_owned());
    env.insert("UPP_RUN".into(), row.run_id.clone());
    env.insert("UPP_DEPTH".into(), row.depth.to_string());
    let base = PlayCtx {
        env,
        cancel: cancel.clone(),
        ..PlayCtx::new(id, 0, cwd.clone(), deadline, id)
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
        transcript: sspec.transcript.unwrap_or(false),
        model: match (&sspec.mode, &sspec.player) {
            (ServerMode::Model, Some(p)) => Some(player::build(p, &base)?),
            _ => None,
        },
    };

    let mut prog = Progress::default();
    let mut msg = brief.clone();
    let mut first = 1;
    // Crash recovery: continue after the last recorded rally.
    if let Some(last) = prior.last() {
        prog.rallies = last.n;
        prog.tokens = prior
            .iter()
            .map(|r| player::billable(r.tokens_in, r.tokens_cached, r.tokens_out))
            .sum();
        prog.cost_usd = prior.iter().map(|r| r.cost_usd).sum();
        prog.trail = row.trail.clone();
        prog.session = row.session.clone();
        prog.last_answer = row.last_answer.clone();
        let history: Vec<(String, String)> = prior
            .iter()
            .filter(|r| r.verdict != "fault")
            .map(|r| (r.ball.clone(), r.answer.clone()))
            .collect();
        let restored = player.restore(&history, row.session.as_deref());
        if last.verdict == "point" {
            store.finish(
                id,
                "point",
                Some(&last.answer),
                &format!("point on rally {}", last.n),
            )?;
            return Ok("point".into());
        }
        let repeat = if restored { None } else { Some(brief.as_str()) };
        msg = if last.verdict == "fault" {
            fault_message(&last.note, repeat)
        } else {
            let checks: Vec<referee::CheckResult> =
                serde_json::from_value(last.checks.clone()).unwrap_or_default();
            let v = Verdict {
                passed: last.passed,
                checks,
            };
            return_message(last.n, &v, &last.note, server.standing.as_deref(), repeat)
        };
        first = last.n + 1;
        store.event(
            id,
            "resumed",
            json!({"after_rally": last.n, "session_restored": restored}),
        )?;
    } else if resuming {
        store.event(id, "resumed", json!({"after_rally": 0}))?;
    }
    // A rally the player started but never recorded (the scheduler died mid-rally): the
    // player may already have changed files. Never replay it blind: say so, with what
    // changed since it started, and continue from the current state.
    if let Some(f) = store.inflight(id)? {
        if f["n"].as_u64() == Some(first as u64) {
            let changed = changed_since(&cwd, f["tree"].as_str());
            if !changed.is_empty() {
                msg = format!(
                    "Recovery: your previous attempt at this rally was interrupted after you \
                     started (the scheduler stopped). It may already have changed files. \
                     Changed since it started: {}. Continue from the current state; do not \
                     redo work that is already done.\n\n{msg}",
                    changed.join(", ")
                );
            }
            store.event(
                id,
                "recovered_inflight",
                json!({"n": first, "changed_since_start": changed}),
            )?;
        }
        store.set_inflight(id, None)?;
    }
    let mut seen_children: Vec<String> = store.children(id)?.into_iter().map(|t| t.id).collect();
    let mut last_mode = String::new();
    for n in first..=lease.rallies() {
        if store.stop_requested(id)? {
            store.finish(id, "stopped", None, "stop requested")?;
            return Ok("stopped".into());
        }
        if Instant::now() >= deadline {
            store.finish(
                id,
                "expired",
                None,
                &format!("wall lease of {}s used up (runaway guard)", lease.wall()),
            )?;
            return Ok("expired".into());
        }
        if let Some(mt) = lease.max_tokens {
            if prog.tokens >= mt {
                store.finish(
                    id,
                    "expired",
                    None,
                    &format!("token lease of {mt} used up (runaway guard)"),
                )?;
                return Ok("expired".into());
            }
        }
        if let Some(mc) = lease.max_cost_usd {
            if prog.cost_usd >= mc {
                store.finish(
                    id,
                    "expired",
                    None,
                    &format!("cost lease of ${mc} used up (runaway guard)"),
                )?;
                return Ok("expired".into());
            }
        }
        store.set_phase(id, "player", Some(&msg))?;
        let effort = spec.effort.as_ref().and_then(|e| e.for_rally(n));
        store.event(
            id,
            "rally",
            json!({"n": n, "ball": proc::head(&msg, 400), "effort": effort}),
        )?;
        let started = now_ms();
        let t_play = Instant::now();
        let mut ctx = PlayCtx {
            rally: n,
            effort,
            ..base.clone()
        };
        ctx.env.insert("UPP_RALLY".into(), n.to_string());
        let repeat_brief = if player.keeps_session() {
            None
        } else {
            Some(brief.as_str())
        };
        let snap = git::provenance(&cwd, None, true);
        store.set_inflight(id, Some(&json!({"n": n, "tree": snap["tree"]})))?;
        let played = player.play(&msg, &ctx);
        store.set_inflight(id, None)?;
        save_wall(store)?;
        let mode = player.mode();
        if !mode.is_empty() && mode != last_mode {
            store.event(id, "player_mode", json!({"n": n, "mode": mode}))?;
            last_mode = mode;
        }
        let reply = match played {
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
                    tokens_cached: 0,
                    cost_usd: 0.0,
                    session: None,
                    started_ms: started,
                    ended_ms: now_ms(),
                    provenance: Value::Null,
                })?;
                store.progress(id, &prog)?;
                store.event(id, "fault", json!({"n": n, "error": proc::head(&err, 400)}))?;
                if cancel.load(Ordering::SeqCst) {
                    store.finish(
                        id,
                        "stopped",
                        None,
                        "stop requested (player call cancelled)",
                    )?;
                    return Ok("stopped".into());
                }
                msg = fault_message(&err, repeat_brief);
                continue;
            }
        };
        let elapsed = t_play.elapsed();
        prog.rallies = n;
        prog.tokens += reply.billable();
        prog.cost_usd += reply.cost_usd;
        if reply.session.is_some() {
            prog.session = reply.session.clone();
        }
        prog.last_answer = Some(reply.text.clone());
        store.progress(id, &prog)?;
        store.event(
            id,
            "answer",
            json!({"n": n, "answer": proc::head(&reply.text, 400), "tokens": reply.billable(),
                   "tokens_in": reply.tokens_in, "tokens_cached": reply.tokens_cached, "tokens_out": reply.tokens_out}),
        )?;

        // Sub-tables opened during this rally: the referee runs once they have ended.
        let new_children: Vec<String> = store
            .children(id)?
            .into_iter()
            .map(|t| t.id)
            .filter(|c| !seen_children.contains(c))
            .collect();
        let children = wait_children(store, id, &new_children, deadline)?;
        seen_children.extend(new_children);

        store.set_phase(id, "referee", None)?;
        let applied = spec.apply.as_ref().map(|a| apply(a, &reply.text, &cwd));
        // Full snapshot (untracked files too) only in the table's own worktree.
        let prov = git::provenance(&cwd, wt_base.as_deref(), spec.worktree.is_some());
        let unverified = spec.referee.is_empty() && spec.unverified;
        let mut verdict = if unverified {
            referee::unverified()
        } else {
            referee::referee(
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
            )
        };
        if let Some(a) = applied {
            verdict.passed = verdict.passed && a.pass;
            verdict.checks.insert(0, a);
        }
        store.event(
            id,
            "referee",
            json!({"n": n, "passed": verdict.passed, "tree": prov["tree"],
                   "checks": verdict.checks.iter().map(|c| json!({"name": c.name, "pass": c.pass, "summary": c.summary})).collect::<Vec<_>>()}),
        )?;
        let needs_answer =
            server.mode == ServerMode::External && (!verdict.passed || server.review_points);
        let card = build_card(
            &spec,
            id,
            n,
            &reply,
            elapsed,
            &verdict,
            &prov,
            &children,
            needs_answer,
        );
        // A card for the external server is published with its pending ball (in decide).
        let card_id = if needs_answer {
            0
        } else {
            store.append_card(id, &row.run_id, n, "rally", &card)?
        };

        let decision = decide(
            store,
            id,
            n,
            &mut server,
            &goal,
            &brief,
            &reply.text,
            &verdict,
            &card,
            card_id,
            &row.run_id,
            deadline,
            &ctx,
        )?;
        let (verdict_s, note) = match &decision {
            Decision::Point => ("point", String::new()),
            Decision::Return(note) => ("return", note.clone()),
            Decision::Stop => ("return", "stopped by the server".into()),
            Decision::Expire(r) => ("return", r.clone()),
            Decision::Fail(r) => ("return", r.clone()),
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
            tokens_cached: reply.tokens_cached,
            cost_usd: reply.cost_usd,
            session: reply.session.clone(),
            started_ms: started,
            ended_ms: now_ms(),
            provenance: prov.clone(),
        })?;
        store.progress(id, &prog)?;
        match decision {
            Decision::Point => {
                let reason = if unverified {
                    format!("UNVERIFIED point on rally {n}: no referee checks (unverified = true)")
                } else {
                    format!("point on rally {n}")
                };
                // A worktree table's branch gets a commit of exactly the verified tree.
                if let (Some(w), Some(tree)) = (
                    store.table(id)?.and_then(|t| t.worktree),
                    prov["tree"].as_str(),
                ) {
                    let msg = format!("upp: {} ({id}) verified point, rally {n}", spec.name);
                    match git::commit_verified(&cwd, w["branch"].as_str().unwrap_or(""), tree, &msg)
                    {
                        Ok(c) => {
                            store.event(id, "committed", json!({"commit": c, "tree": tree}))?
                        }
                        Err(e) => store.event(id, "commit_error", json!({"error": e}))?,
                    }
                }
                store.finish(id, "point", Some(&reply.text), &reason)?;
                return Ok("point".into());
            }
            Decision::Fail(r) => {
                store.finish(id, "error", None, &r)?;
                return Ok("error".into());
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
                if !children.is_empty() {
                    msg.push_str("\n\nSub-tables you opened:\n");
                    for c in &children {
                        msg.push_str(&format!(
                            "- {} ({}): {}\n",
                            c["table"].as_str().unwrap_or(""),
                            c["status"].as_str().unwrap_or(""),
                            c["summary"].as_str().unwrap_or("")
                        ));
                    }
                }
            }
        }
    }
    store.finish(
        id,
        "lost",
        None,
        &format!(
            "{} rallies without a point (runaway guard)",
            lease.rallies()
        ),
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
    card: &Value,
    card_id: i64,
    run_id: &str,
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
            // The server reads the card and the referee output; the answer only on request.
            let answer_part = if server.transcript {
                format!("\n\nWorker's full answer:\n{}", proc::head(answer, 8000))
            } else {
                String::new()
            };
            let prompt = format!(
                "You are the server (orchestrator) at a ping-pong table. A worker answered a ball and an \
                 external referee checked it.\n\nGoal:\n{goal}\n\nBall:\n{}\n\nWorker's result card (rally {n}):\n{}{answer_part}\n\n\
                 Referee:\n{}\n{ask}",
                proc::head(brief, 4000),
                card_text(card),
                verdict.report(!verdict.passed)
            );
            let sctx = PlayCtx {
                session_key: format!("{id}-server"),
                ..ctx.clone()
            };
            let model = server
                .model
                .as_mut()
                .ok_or("model server without a player")?;
            if !verdict.passed {
                // Only a correction is at stake: a reviewer failure falls back to the
                // referee output alone (never a point: the referee failed).
                return Ok(match model.play(&prompt, &sctx) {
                    Ok(r) => {
                        store.add_server_tokens(id, r.billable())?;
                        Decision::Return(r.text.trim().to_string())
                    }
                    Err(e) => {
                        store.event(id, "server_error", json!({"error": proc::head(&e, 300)}))?;
                        Decision::Return(String::new())
                    }
                });
            }
            // The referee passed and the server must confirm the point. A reviewer that
            // cannot answer must not grant it: retry, then fail closed.
            let mut last_err = String::new();
            for attempt in 1..=REVIEW_ATTEMPTS {
                match model.play(&prompt, &sctx) {
                    Ok(r) => {
                        store.add_server_tokens(id, r.billable())?;
                        let t = r.text.trim();
                        return Ok(if referee::first_word(t) == "ACCEPT" {
                            Decision::Point
                        } else {
                            Decision::Return(t.to_string())
                        });
                    }
                    Err(e) => {
                        store.event(
                            id,
                            "server_error",
                            json!({"attempt": attempt, "error": proc::head(&e, 300)}),
                        )?;
                        last_err = e;
                        if attempt < REVIEW_ATTEMPTS && Instant::now() < deadline {
                            std::thread::sleep(Duration::from_millis(500 * attempt as u64));
                        }
                    }
                }
            }
            Ok(Decision::Fail(format!(
                "the model server could not review the ball after {REVIEW_ATTEMPTS} attempts \
                 (fail closed, no point): {}",
                proc::head(&last_err, 300)
            )))
        }
        ServerMode::External => {
            let _ = card_id;
            let mut payload = json!({
                "table_id": id,
                "table": card["table"],
                "rally": n,
                "goal": proc::head(goal, 1000),
                "card": card,
                "referee": {
                    "passed": verdict.passed,
                    "checks": verdict.checks.iter().map(|c| json!({
                        "name": c.name, "pass": c.pass, "skipped": c.skipped, "summary": c.summary,
                        "output_tail": if c.pass { String::new() } else { proc::tail(c.output.trim(), REFEREE_TAIL) },
                    })).collect::<Vec<_>>(),
                },
                "transcript": "full ball and answer: transcript(table, rally)",
                "how_to_answer": "card_answer / ball_answer(table, verdict = accept | return | stop, note[, next][, open]). \
                                  accept scores the point only if the referee passed; return sends the note back.",
            });
            if server.transcript {
                payload["ball"] = json!(proc::head(brief, 8000));
                payload["answer"] = json!(proc::head(answer, 16000));
            }
            store.add_server_tokens(id, card::est_tokens(&payload.to_string()))?;
            let card_id = store.publish_for_server(id, run_id, n, card, &mut payload)?;
            store.event(
                id,
                "awaiting_server",
                json!({"n": n, "passed": verdict.passed, "card_id": card_id}),
            )?;
            let until = Instant::now() + server.timeout;
            let mut w = store.watch();
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
                store.wait(&mut w, until.min(deadline).saturating_duration_since(now));
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

/// A card as compact text (for model servers and orchestrators).
pub fn card_text(c: &Value) -> String {
    let list = |k: &str| {
        c[k].as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "none".into())
    };
    let mut s =
        format!(
        "claimed done: {}\nsummary: {}\nevidence: {}\nopen issues: {}\nartifacts: {}\nreferee: {}",
        if c["done"] == true { "yes" } else { "no" },
        c["summary"].as_str().unwrap_or(""),
        list("evidence"),
        list("open_issues"),
        list("artifacts"),
        if c["referee"]["passed"] == true {
            "PASS".to_string()
        } else {
            format!(
                "FAIL ({})",
                c["referee"]["failed"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("; "))
                    .unwrap_or_default()
            )
        }
    );
    if let Some(t) = c["referee"]["tail"].as_str() {
        s.push_str(&format!("\nreferee output (tail):\n{t}"));
    }
    if let Some(ch) = c["children"].as_array().filter(|a| !a.is_empty()) {
        s.push_str("\nsub-tables:");
        for x in ch {
            s.push_str(&format!(
                " {} {};",
                x["table"].as_str().unwrap_or(""),
                x["status"].as_str().unwrap_or("")
            ));
        }
    }
    s
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
