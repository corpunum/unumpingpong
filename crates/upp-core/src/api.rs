// SPDX-License-Identifier: Apache-2.0
//! Front-end-neutral operations shared by the CLI, the daemon (HTTP), the MCP server
//! and the persistent orchestrator. Every function takes the store and returns JSON.
//!
//! Consumers get **cards** by default (`status`, `cards_wait`, pending balls); the full
//! ball and answer of a rally are fetched on demand with `transcript`.

use crate::card;
use crate::config::{MatchFile, TableSpec, DEFAULT_MAX_DEPTH};
use crate::git;
use crate::proc;
use crate::store::{now_ms, CardRow, Store, TableRow};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
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
        "player_tokens": t.tokens,
        "orchestrator_tokens": t.server_tokens,
        "cost_usd": t.cost_usd,
        "after": t.after_id,
        "parent": t.parent_id,
        "depth": t.depth,
        "resumes": t.resumes,
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

fn parse_match(v: Value) -> Result<MatchFile, String> {
    match &v {
        Value::String(s) => MatchFile::parse(s),
        _ => MatchFile::from_json(v),
    }
}

pub fn create_json(store: &Store, v: Value) -> Result<Value, String> {
    create(store, parse_match(v)?, Some("api"))
}

/// A writer child of a table that runs in a git worktree gets its own worktree,
/// branched from the parent's branch, so parallel children never edit the same
/// files. Read-only children (and tables that set their own worktree) keep theirs.
fn isolate_writer(t: &mut TableSpec, parent_tree: Option<&Value>) {
    let Some(pt) = parent_tree else { return };
    if t.read_only || t.worktree.is_some() {
        return;
    }
    let Some(repo) = pt["repo"].as_str() else {
        return;
    };
    t.worktree = Some(crate::config::WorktreeSpec {
        repo: Some(repo.to_string()),
        base: pt["branch"].as_str().map(String::from),
        branch: None,
        path: None,
    });
    t.cwd = None;
}

/// Fill what a new table leaves out from the table it comes from (player, cwd, lease,
/// effort, card). Referee checks are never inherited: each table states its evidence.
fn inherit(t: &mut TableSpec, from: &TableSpec, cwd: Option<String>) {
    if t.player.is_none() {
        t.player = from.player.clone();
    }
    if t.cwd.is_none() {
        t.cwd = cwd.or_else(|| from.cwd.clone());
    }
    if t.lease.is_none() {
        t.lease = from.lease.clone();
    }
    if t.effort.is_none() {
        t.effort = from.effort.clone();
    }
    if t.card.is_none() {
        t.card = from.card;
    }
}

/// Open sub-tables of `parent` (nested tables): they join the parent's run at depth
/// parent + 1, inherit its player and working directory unless they set their own, and
/// the parent's rally waits for them before its referee runs. Depth is capped by the
/// parent's `max_depth` (default 3).
pub fn create_child(store: &Store, parent: &str, v: Value) -> Result<Value, String> {
    let p = store.resolve(parent)?;
    if p.is_terminal() {
        return Err(format!(
            "parent table '{}' has ended ({})",
            p.name, p.status
        ));
    }
    let pspec = store.spec(&p.id)?;
    let cap = pspec.max_depth.unwrap_or(DEFAULT_MAX_DEPTH);
    if p.depth + 1 > cap {
        return Err(format!(
            "depth cap: '{}' is at depth {} and may not open sub-tables (max_depth {cap})",
            p.name, p.depth
        ));
    }
    let mut m = parse_match(v)?;
    let cwd = p
        .worktree
        .as_ref()
        .and_then(|w| w["path"].as_str().map(String::from))
        .or_else(|| pspec.cwd.clone());
    for t in &mut m.tables {
        inherit(t, &pspec, cwd.clone());
        isolate_writer(t, p.worktree.as_ref());
        if t.max_depth.is_none() {
            t.max_depth = Some(cap);
        }
    }
    let tables = m.resolve()?;
    if p.worktree.is_some() && tables.iter().any(|t| !t.read_only && t.worktree.is_none()) {
        return Err(
            "child writer requires an isolated worktree because the parent has a worktree".into(),
        );
    }
    let ids = store.add_tables(&p.run_id, &tables, None, Some(&p))?;
    store.event(&p.id, "children", json!({"tables": ids}))?;
    Ok(json!({
        "run_id": p.run_id,
        "parent": p.id,
        "depth": p.depth + 1,
        "tables": tables.iter().zip(ids.iter()).map(|(t, id)| json!({"id": id, "name": t.name})).collect::<Vec<_>>(),
    }))
}

pub fn list(store: &Store, active_only: bool, run: Option<&str>) -> Result<Value, String> {
    let t = store.tables(run, active_only)?;
    Ok(
        json!({"tables": t.iter().map(summary).collect::<Vec<_>>(), "pending": store.pending(None)?.len()}),
    )
}

/// Status of one table: summary, its rallies as cards (the full ball and answer only with
/// `full`), token split, sub-tables, worktree and the ball waiting for its server.
pub fn status(store: &Store, id: &str, full: bool) -> Result<Value, String> {
    let t = store.resolve(id)?;
    let cards = store.cards_of(&t.id)?;
    let rallies = store.rallies(&t.id)?;
    let rally_log: Vec<Value> = rallies
        .iter()
        .map(|r| {
            let card = cards
                .iter()
                .rev()
                .find(|c| c.kind == "rally" && c.rally == r.n)
                .map(|c| c.card.clone());
            let mut v = json!({
                "n": r.n, "verdict": r.verdict, "passed": r.passed,
                "card": card,
                "note": proc::head(&r.note, 600),
                "tokens": crate::player::billable(r.tokens_in, r.tokens_cached, r.tokens_out), "cost_usd": r.cost_usd,
                "duration_s": (r.ended_ms - r.started_ms) as f64 / 1000.0,
                "tree": r.provenance["tree"],
            });
            if full {
                v["ball"] = json!(r.ball);
                v["answer"] = json!(r.answer);
                v["note"] = json!(r.note);
                v["checks"] = r.checks.clone();
                v["provenance"] = r.provenance.clone();
            }
            v
        })
        .collect();
    let card_est: u64 = cards
        .iter()
        .map(|c| card::est_tokens(&c.card.to_string()))
        .sum();
    let transcript_est: u64 = rallies
        .iter()
        .map(|r| card::est_tokens(&r.ball) + card::est_tokens(&r.answer))
        .sum();
    let mut v = summary(&t);
    v["goal"] = json!(t.goal);
    v["result"] = json!(t.result.as_deref().map(|b| if full {
        b.to_string()
    } else {
        proc::head(b, 1500)
    }));
    v["session"] = json!(t.session);
    v["rally_log"] = json!(rally_log);
    v["final_card"] = json!(store.latest_card(&t.id, "final")?.map(|c| c.card));
    v["token_split"] = json!({
        "player": t.tokens,
        "orchestrator": t.server_tokens,
        "cards_est": card_est,
        "transcripts_est": transcript_est,
    });
    v["children"] = json!(store
        .children(&t.id)?
        .iter()
        .map(|c| json!({"id": c.id, "name": c.name, "status": c.status}))
        .collect::<Vec<_>>());
    v["worktree"] = json!(t.worktree);
    v["pending"] = json!(store.pending(Some(&t.id))?.first().map(|p| &p.payload));
    if full {
        v["current_ball"] = json!(t.current_ball);
    }
    Ok(v)
}

/// The full transcript of a table (or one rally): balls, answers, referee output, notes,
/// provenance. Reading it is charged to the table's orchestrator-side token estimate.
pub fn transcript(store: &Store, id: &str, rally: Option<u32>) -> Result<Value, String> {
    let t = store.resolve(id)?;
    let mut rows: Vec<Value> = store
        .rallies(&t.id)?
        .into_iter()
        .filter(|r| rally.is_none_or(|n| r.n == n))
        .map(|r| {
            json!({
                "n": r.n, "ball": r.ball, "answer": r.answer, "checks": r.checks, "passed": r.passed,
                "verdict": r.verdict, "note": r.note, "tokens_in": r.tokens_in, "tokens_cached": r.tokens_cached, "tokens_out": r.tokens_out,
                "session": r.session, "provenance": r.provenance,
                "started_ms": r.started_ms, "ended_ms": r.ended_ms,
            })
        })
        .collect();
    // The rally waiting for its server is recorded only after the answer: serve it from
    // the table's current ball and answer, with the referee result from the pending ball.
    if let Some(p) = store.pending(Some(&t.id))?.into_iter().next() {
        if rally.is_none_or(|n| n == p.rally) {
            rows.push(json!({
                "n": p.rally, "in_flight": true,
                "ball": t.current_ball, "answer": t.last_answer,
                "checks": p.payload["referee"]["checks"], "passed": p.payload["referee"]["passed"],
                "session": t.session,
            }));
        }
    }
    if let Some(n) = rally {
        if rows.is_empty() {
            return Err(format!("table '{}' has no rally {n}", t.name));
        }
    }
    let v = json!({"table_id": t.id, "table": t.name, "goal": t.goal, "rallies": rows});
    store.add_server_tokens(&t.id, card::est_tokens(&v.to_string()))?;
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
    store.add_server_tokens(&t.id, card::est_tokens(note))?;
    Ok(json!({"ok": true, "table_id": t.id, "rally": rally, "verdict": verdict}))
}

/// Answer a card as the orchestrator: `accept` / `return` / `stop` with a note, and
/// optionally `next` (a ball chained after this table's point: a new table that starts
/// when this one scores, with its result as `{{prev.answer}}`) and `open` (a new table,
/// as a table object, in the same run). New tables inherit the player and cwd.
pub fn card_answer(
    store: &Store,
    id: &str,
    verdict: &str,
    note: &str,
    next: Option<&str>,
    open: Option<&Value>,
) -> Result<Value, String> {
    if !matches!(verdict, "accept" | "return" | "stop") {
        return Err(format!(
            "verdict must be accept, return or stop (got '{verdict}')"
        ));
    }
    let t = store.resolve(id)?;
    if store.pending(Some(&t.id))?.is_empty() {
        return Err(format!(
            "table '{}' has no ball waiting for its server",
            t.name
        ));
    }
    let spec = store.spec(&t.id)?;
    let cwd = t
        .worktree
        .as_ref()
        .and_then(|w| w["path"].as_str().map(String::from));
    let mut created = Vec::new();
    // New tables are queued before the answer, so a run never looks idle in between.
    if let Some(ball) = next.filter(|b| !b.trim().is_empty()) {
        let k = store
            .tables(Some(&t.run_id), false)?
            .iter()
            .filter(|x| x.after_id.as_deref() == Some(t.id.as_str()))
            .count();
        let mut n = spec.clone();
        n.name = format!("{}-next{}", spec.name, k + 1);
        n.goal = ball.to_string();
        n.ball = ball.to_string();
        n.after = None;
        n.setup = None;
        if let (Some(w), Some(_)) = (&mut n.worktree, &t.worktree) {
            // Build on the verified work: the source branch carries its point's commit.
            w.base = t
                .worktree
                .as_ref()
                .and_then(|x| x["branch"].as_str().map(String::from));
            w.branch = None;
            w.path = None;
        }
        let ids = store.add_tables(&t.run_id, &[n.clone()], Some(&t.id), None)?;
        created.push(json!({"id": ids[0], "name": n.name, "after": t.id}));
    }
    if let Some(o) = open {
        let mut m = parse_match(o.clone())?;
        for x in &mut m.tables {
            inherit(x, &spec, cwd.clone().or(spec.cwd.clone()));
            if x.server.is_none() {
                x.server = spec.server.clone();
            }
        }
        for x in &mut m.tables {
            isolate_writer(x, t.worktree.as_ref());
        }
        let tables = m.resolve()?;
        let ids = store.add_tables(&t.run_id, &tables, None, None)?;
        for (x, i) in tables.iter().zip(ids) {
            created.push(json!({"id": i, "name": x.name}));
        }
    }
    let mut r = answer(store, &t.id, verdict, note)?;
    r["created"] = json!(created);
    Ok(r)
}

pub fn stop(store: &Store, id: &str) -> Result<Value, String> {
    let t = store.resolve(id)?;
    store.request_stop(&t.id)?;
    Ok(
        json!({"ok": true, "table_id": t.id, "note": "the running player call is cancelled (process group killed); queued tables stop at once; sub-tables stop too"}),
    )
}

/// Block until one of the tables (or any table, when `ids` is empty) waits for its
/// server or every one of them has ended, or `timeout` passes. Event-driven.
pub fn wait(db: &Path, ids: &[String], timeout: Duration) -> Result<Value, String> {
    let store = Store::open(db)?;
    let until = Instant::now() + timeout;
    let mut w = store.watch();
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
        store.wait(&mut w, until.saturating_duration_since(Instant::now()));
    }
}

/// What `cards_wait` looks at.
#[derive(Debug, Clone, Default)]
pub struct CardQuery {
    /// Table ids or names; empty: every table (of `run`, if set).
    pub tables: Vec<String>,
    pub run: Option<String>,
    /// Card cursor: return cards with a larger id. `None`: return the cards that need an
    /// answer now, or else wait for the next card.
    pub since: Option<i64>,
    pub timeout: Duration,
}

fn card_view(store: &Store, c: &CardRow) -> Result<Value, String> {
    let t = store.table(&c.table_id)?;
    let waiting = c.kind == "rally"
        && store
            .pending(Some(&c.table_id))?
            .iter()
            .any(|p| p.rally == c.rally);
    let mut v = c.card.clone();
    if !v.is_object() {
        v = json!({});
    }
    v["card_id"] = json!(c.id);
    v["table_id"] = json!(c.table_id);
    v["run_id"] = json!(c.run_id);
    v["rally"] = json!(c.rally);
    v["kind"] = json!(c.kind);
    v["needs_answer"] = json!(waiting);
    v["table_status"] = json!(t.as_ref().map(|t| &t.status));
    Ok(v)
}

/// Cards stream: block until a card arrives for the tables in scope and return it with
/// a cursor. With no `since`, cards that need an answer now come back at once. Every
/// card is compact; `transcript` has the full rally.
pub fn cards_wait(db: &Path, q: &CardQuery) -> Result<Value, String> {
    let store = Store::open(db)?;
    let ids: Vec<String> = q
        .tables
        .iter()
        .map(|i| store.resolve(i).map(|t| t.id))
        .collect::<Result<_, _>>()?;
    let in_scope = |table_id: &str| ids.is_empty() || ids.iter().any(|i| i == table_id);
    let until = Instant::now() + q.timeout;
    let mut since = q.since;
    let mut w = store.watch();
    loop {
        match since {
            None => {
                let mut cards = Vec::new();
                for p in store.pending(None)? {
                    if !in_scope(&p.table_id) {
                        continue;
                    }
                    let t = store.table(&p.table_id)?;
                    if q.run.is_some() && t.as_ref().map(|t| &t.run_id) != q.run.as_ref() {
                        continue;
                    }
                    if let Some(c) = p.payload["card_id"].as_i64() {
                        if let Some(row) =
                            store.cards_of(&p.table_id)?.into_iter().find(|r| r.id == c)
                        {
                            cards.push(card_view(&store, &row)?);
                        }
                    }
                }
                if !cards.is_empty() {
                    return Ok(
                        json!({"reason": "needs_answer", "cursor": store.last_card_id()?, "cards": cards}),
                    );
                }
                since = Some(store.last_card_id()?);
            }
            Some(s) => {
                let rows: Vec<CardRow> = store
                    .cards_since(q.run.as_deref(), s, 200)?
                    .into_iter()
                    .filter(|c| in_scope(&c.table_id))
                    .collect();
                if !rows.is_empty() {
                    let cursor = rows.last().map(|c| c.id).unwrap_or(s);
                    let cards = rows
                        .iter()
                        .map(|c| card_view(&store, c))
                        .collect::<Result<Vec<_>, _>>()?;
                    let reason = if cards.iter().any(|c| c["needs_answer"] == true) {
                        "needs_answer"
                    } else {
                        "cards"
                    };
                    return Ok(json!({"reason": reason, "cursor": cursor, "cards": cards}));
                }
            }
        }
        let scope: Vec<TableRow> = if ids.is_empty() {
            store.tables(q.run.as_deref(), false)?
        } else {
            ids.iter()
                .filter_map(|i| store.table(i).ok().flatten())
                .collect()
        };
        if !scope.is_empty() && scope.iter().all(|t| t.is_terminal()) {
            return Ok(
                json!({"reason": "ended", "cursor": since.unwrap_or(0), "cards": [],
                             "tables": scope.iter().map(summary).collect::<Vec<_>>()}),
            );
        }
        if Instant::now() >= until {
            return Ok(
                json!({"reason": "timeout", "cursor": since.unwrap_or(0), "cards": [],
                             "open": scope.iter().filter(|t| !t.is_terminal()).count()}),
            );
        }
        store.wait(&mut w, until.saturating_duration_since(Instant::now()));
    }
}

/// Integrate the scored worktree tables of a run: one branch with every verified point
/// merged in table order (stops at the first conflict); `check` runs on the result.
pub fn integrate(
    store: &Store,
    run: &str,
    into: Option<&str>,
    check: Option<&str>,
) -> Result<Value, String> {
    let mut tables = store.tables(Some(run), false)?;
    tables.reverse(); // creation order
    let mut scored = Vec::new();
    let mut skipped = Vec::new();
    let mut repo: Option<PathBuf> = None;
    for t in &tables {
        let Some(w) = &t.worktree else { continue };
        if t.status != "point" {
            skipped.push(json!({"table": t.name, "status": t.status}));
            continue;
        }
        let rallies = store.rallies(&t.id)?;
        let Some(tree) = rallies
            .iter()
            .rev()
            .find(|r| r.verdict == "point")
            .and_then(|r| r.provenance["tree"].as_str().map(String::from))
        else {
            skipped.push(json!({"table": t.name, "status": "point without provenance"}));
            continue;
        };
        repo.get_or_insert_with(|| PathBuf::from(w["repo"].as_str().unwrap_or(".")));
        scored.push(git::Scored {
            table_id: t.id.clone(),
            name: t.name.clone(),
            worktree: w.clone(),
            tree,
        });
    }
    let Some(repo) = repo else {
        return Err(format!(
            "run {run} has no scored worktree table to integrate"
        ));
    };
    let work = store
        .path()
        .parent()
        .map(|p| p.join("worktrees"))
        .unwrap_or_else(|| PathBuf::from("worktrees"));
    let work = std::path::absolute(&work).unwrap_or(work);
    let mut v = git::integrate(&repo, into.unwrap_or("HEAD"), run, &scored, &work, check)?;
    v["skipped"] = json!(skipped);
    Ok(v)
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
            "parent_id": t.and_then(|t| t.parent_id.clone()),
            "rally": r.n,
            "ball": r.ball,
            "answer": r.answer,
            "referee": r.checks,
            "passed": r.passed,
            "verdict": r.verdict,
            "server_note": r.note,
            "tokens_in": r.tokens_in,
            "tokens_cached": r.tokens_cached,
            "tokens_out": r.tokens_out,
            "billable_tokens": crate::player::billable(r.tokens_in, r.tokens_cached, r.tokens_out),
            "cost_usd": r.cost_usd,
            "session": r.session,
            "provenance": r.provenance,
            "started_ms": r.started_ms,
            "ended_ms": r.ended_ms,
        });
        out.push_str(&line.to_string());
        out.push('\n');
    }
    Ok(out)
}
