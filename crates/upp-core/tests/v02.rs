// SPDX-License-Identifier: Apache-2.0
//! v0.2 engine tests with scripted fake players: result cards, transcripts, the
//! persistent orchestrator, nested tables, cancellation, effort hints, provenance,
//! worktrees and integration. No model needed.
#![cfg(unix)]

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use upp_core::orchestrator::Orchestrator;
use upp_core::store::{Store, TableRow};
use upp_core::{api, MatchFile, Scheduler};

fn fake(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fake-agents")
        .join(name);
    std::fs::canonicalize(p)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// Create the match's run; run it to the end (with its orchestrator, if any).
fn run_match(dir: &Path, toml: &str, parallel: usize) -> (Store, Vec<TableRow>) {
    let f = dir.join("match.toml");
    std::fs::write(&f, toml).unwrap();
    let m = MatchFile::load(&f).unwrap();
    let orch = m.orchestrator.clone();
    let db = dir.join("upp.db");
    let store = Store::open(&db).unwrap();
    let created = api::create(&store, m, None).unwrap();
    let run = created["run_id"].as_str().unwrap().to_string();
    let oh = orch.map(|spec| {
        let mut o = Orchestrator::new(&db, spec);
        o.run = Some(run.clone());
        o.until_idle = true;
        std::thread::spawn(move || o.run().unwrap())
    });
    let mut s = Scheduler::new(&db, parallel, "test");
    s.run = Some(run.clone());
    s.until_idle = true;
    s.run().unwrap();
    if let Some(h) = oh {
        h.join().unwrap();
    }
    let mut rows = store.tables(Some(&run), false).unwrap();
    rows.reverse();
    (store, rows)
}

fn by_name<'a>(rows: &'a [TableRow], name: &str) -> &'a TableRow {
    rows.iter().find(|t| t.name == name).unwrap()
}

// ---------------------------------------------------------------- cards

#[test]
fn cards_come_from_the_block_or_the_extractor_and_status_shows_cards_not_answers() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[defaults]
referee = [{{ kind = "command", name = "is-42", run = "grep -qx 42 answer.txt" }}]
[[table]]
name = "carded"
ball = "write 42"
cwd = "a"
player = {{ kind = "shell", command = ["sh", "{coder}"], env = {{ FAKE_CARD = "1", FAKE_LOG = "log" }} }}
[[table]]
name = "plain"
ball = "write 42"
cwd = "b"
player = {{ kind = "shell", command = ["sh", "{coder}"] }}
"#,
        coder = fake("fake-coder.sh")
    );
    let (store, rows) = run_match(d.path(), &toml, 2);
    assert!(rows.iter().all(|t| t.status == "point"), "{rows:?}");
    // The first ball asks for a card, after the referee description.
    let b1 = std::fs::read_to_string(d.path().join("a/log/ball-1.txt")).unwrap();
    let (checks, card) = (b1.find("grep -qx 42").unwrap(), b1.find("<card>").unwrap());
    assert!(checks < card, "{b1}");

    let st = api::status(&store, "carded", false).unwrap();
    let r1 = &st["rally_log"][0];
    assert!(
        r1.get("answer").is_none(),
        "status gives cards, not answers: {r1}"
    );
    let c1 = &r1["card"];
    assert_eq!(c1["source"], "block");
    assert_eq!(c1["done"], true, "the player claimed done");
    assert_eq!(c1["referee"]["passed"], false, "the referee says otherwise");
    assert!(c1["referee"]["failed"][0]
        .as_str()
        .unwrap()
        .contains("is-42"));
    assert_eq!(c1["artifacts"], json!(["answer.txt"]));
    assert!(c1["tokens"].is_u64() && c1["elapsed_s"].is_f64());
    assert!(!c1.to_string().contains("SECRET-BODY-LINE"));
    assert_eq!(st["final_card"]["status"], "point");
    // The stored result has no card block; the transcript has the whole answer.
    assert!(!st["result"].as_str().unwrap().contains("<card>"));
    let before = store.resolve("carded").unwrap().server_tokens;
    let tr = api::transcript(&store, "carded", Some(1)).unwrap();
    assert!(tr["rallies"][0]["answer"]
        .as_str()
        .unwrap()
        .contains("SECRET-BODY-LINE 1"));
    assert!(api::transcript(&store, "carded", Some(9)).is_err());
    // Reading a transcript is charged to the orchestrator side.
    assert!(store.resolve("carded").unwrap().server_tokens > before);

    let plain = api::status(&store, "plain", false).unwrap();
    let pc = &plain["rally_log"][0]["card"];
    assert_eq!(pc["source"], "extracted");
    assert!(pc["summary"].as_str().unwrap().starts_with("Rally 1: done"));
    assert!(pc["artifacts"]
        .as_array()
        .unwrap()
        .contains(&json!("answer.txt")));
    let split = &plain["token_split"];
    assert!(split["transcripts_est"].as_u64().unwrap() > 0);
    // The card stream sees rally and final cards in order.
    let all = api::cards_wait(
        &d.path().join("upp.db"),
        &api::CardQuery {
            since: Some(0),
            ..Default::default()
        },
    )
    .unwrap();
    let kinds: Vec<&str> = all["cards"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["table"] == "plain")
        .map(|c| c["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["rally", "rally", "final"]);
}

// ---------------------------------------------------------------- persistent orchestrator

#[test]
fn the_orchestrator_corrects_accepts_and_chains_the_next_ball() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("orch.log");
    let toml = format!(
        r#"
[orchestrator]
player = {{ kind = "shell", command = ["sh", "{orch}"], env = {{ ORCH_LOG = "{log}", ORCH_NOTE = "use exactly 42", ORCH_CHAIN_FROM = "plan", ORCH_NEXT = "BUILD-BALL now build it" }} }}

[[table]]
name = "fix"
ball = "write 42"
cwd = "fix"
player = {{ kind = "shell", command = ["sh", "{coder}"], env = {{ FAKE_LOG = "log", FAKE_CARD = "1" }} }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt" }}]

[[table]]
name = "plan"
ball = "PLAN make a plan"
cwd = "plan"
player = {{ kind = "shell", command = ["sh", "{echo}"] }}
referee = [{{ kind = "command", run = "true" }}]
"#,
        orch = fake("fake-orchestrator.sh"),
        coder = fake("fake-coder.sh"),
        echo = fake("echo-agent.sh"),
        log = log.display()
    );
    let (store, rows) = run_match(d.path(), &toml, 3);
    assert!(rows.iter().all(|t| t.status == "point"), "{rows:?}");
    // Tables default to the external server when a match has an orchestrator.
    let fix = by_name(&rows, "fix");
    assert_eq!(fix.trail, ".*");
    let b2 = std::fs::read_to_string(d.path().join("fix/log/ball-2.txt")).unwrap();
    assert!(b2.contains("Server note: use exactly 42"), "{b2}");
    // The chained table started after `plan` scored and got the next ball.
    let next = by_name(&rows, "plan-next1");
    let plan = by_name(&rows, "plan");
    assert_eq!(next.after_id.as_deref(), Some(plan.id.as_str()));
    assert!(
        next.result.as_deref().unwrap().contains("BUILD-BALL"),
        "{next:?}"
    );
    assert!(next.started_ms.unwrap() >= plan.ended_ms.unwrap());
    // The orchestrator read cards, never transcripts.
    let seen = std::fs::read_to_string(&log).unwrap();
    assert!(seen.contains("CARD ") && seen.contains("referee=FAIL"));
    assert!(!seen.contains("SECRET-BODY-LINE"), "{seen}");
    assert!(seen.starts_with("You are the orchestrator"));
    // Its estimated reading cost is on the tables (external payloads).
    assert!(store.resolve("fix").unwrap().server_tokens > 0);
}

#[test]
fn the_orchestrator_never_waits_on_one_slow_table() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[orchestrator]
player = {{ kind = "shell", command = ["sh", "{orch}"] }}
[defaults]
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt" }}]
[[table]]
name = "slow"
ball = "x"
cwd = "slow"
player = {{ kind = "shell", command = ["sh", "{coder}"], env = {{ FAKE_SLEEP = "2.5", FAKE_PASS_ON = "1" }} }}
[[table]]
name = "fast"
ball = "x"
cwd = "fast"
player = {{ kind = "shell", command = ["sh", "{coder}"], env = {{ FAKE_PASS_ON = "3" }} }}
"#,
        orch = fake("fake-orchestrator.sh"),
        coder = fake("fake-coder.sh"),
    );
    let (_s, rows) = run_match(d.path(), &toml, 2);
    let (slow, fast) = (by_name(&rows, "slow"), by_name(&rows, "fast"));
    assert_eq!(
        (slow.status.as_str(), fast.status.as_str()),
        ("point", "point")
    );
    assert_eq!(fast.trail, "..*");
    // Three answered rallies of `fast` happened while `slow` was still on its first.
    assert!(
        fast.ended_ms.unwrap() < slow.ended_ms.unwrap(),
        "{fast:?} {slow:?}"
    );
}

#[test]
fn a_failing_or_silent_orchestrator_falls_back_to_the_referee() {
    for mode in ["garbage", "crash"] {
        let d = tempfile::tempdir().unwrap();
        let toml = format!(
            r#"
[orchestrator]
player = {{ kind = "shell", command = ["sh", "{orch}"], env = {{ ORCH_MODE = "{mode}" }} }}
[[table]]
name = "t"
ball = "x"
player = {{ kind = "shell", command = ["sh", "{coder}"] }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt" }}]
"#,
            orch = fake("fake-orchestrator.sh"),
            coder = fake("fake-coder.sh"),
        );
        let (store, rows) = run_match(d.path(), &toml, 1);
        assert_eq!(rows[0].status, "point", "{mode}: {:?}", rows[0]);
        assert_eq!(rows[0].trail, ".*");
        let evs: Vec<String> = store
            .events_since(0, 1000)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        if mode == "crash" {
            assert!(evs.contains(&"orchestrator_error".to_string()));
        }
    }
}

#[test]
fn review_required_never_scores_without_a_decision_and_ends_the_run() {
    for mode in ["garbage", "crash"] {
        let d = tempfile::tempdir().unwrap();
        let toml = format!(
            r#"
[orchestrator]
review = "required"
player = {{ kind = "shell", command = ["sh", "{orch}"], env = {{ ORCH_MODE = "{mode}" }} }}
[[table]]
name = "t"
ball = "x"
player = {{ kind = "shell", command = ["sh", "{coder}"] }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt" }}]
"#,
            orch = fake("fake-orchestrator.sh"),
            coder = fake("fake-coder.sh"),
        );
        // The referee passes, but nobody reviewed the card: no point, and the run
        // ends (table stopped as needs_review) instead of waiting forever.
        let (store, rows) = run_match(d.path(), &toml, 1);
        assert_eq!(rows[0].status, "stopped", "{mode}: {:?}", rows[0]);
        let evs: Vec<String> = store
            .events_since(0, 1000)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert!(evs.contains(&"needs_review".to_string()), "{mode}: {evs:?}");
        assert_eq!(evs.iter().filter(|k| *k == "orchestrator_retry").count(), 2);
    }
}

#[test]
fn cards_wait_returns_waiting_cards_at_once_and_new_cards_by_cursor() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("upp.db");
    let toml = format!(
        r#"
[[table]]
name = "ext"
ball = "x"
player = {{ kind = "shell", command = ["sh", "{coder}"] }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt" }}]
server = {{ mode = "external" }}
"#,
        coder = fake("fake-coder.sh"),
    );
    let f = d.path().join("m.toml");
    std::fs::write(&f, toml).unwrap();
    let store = Store::open(&db).unwrap();
    let c = api::create(&store, MatchFile::load(&f).unwrap(), None).unwrap();
    let run = c["run_id"].as_str().unwrap().to_string();
    let mut s = Scheduler::new(&db, 1, "test");
    s.run = Some(run.clone());
    s.until_idle = true;
    let h = std::thread::spawn(move || s.run());
    let q = |since: Option<i64>| api::CardQuery {
        tables: vec!["ext".into()],
        since,
        timeout: Duration::from_secs(20),
        ..Default::default()
    };
    let t0 = Instant::now();
    let w = api::cards_wait(&db, &q(None)).unwrap();
    assert_eq!(w["reason"], "needs_answer", "{w}");
    let card = &w["cards"][0];
    assert_eq!(
        (card["rally"].as_u64(), card["needs_answer"].as_bool()),
        (Some(1), Some(true))
    );
    assert_eq!(card["referee"]["passed"], false);
    assert!(t0.elapsed() < Duration::from_secs(10));
    let cursor = w["cursor"].as_i64().unwrap();
    // Answer and chain a next ball (it starts after the point).
    let a = api::card_answer(&store, "ext", "return", "42 please", None, None).unwrap();
    assert_eq!(a["rally"], 1);
    let w = api::cards_wait(&db, &q(Some(cursor))).unwrap();
    assert_eq!(w["reason"], "needs_answer", "{w}");
    assert_eq!(w["cards"][0]["rally"], 2);
    assert_eq!(w["cards"][0]["referee"]["passed"], true);
    let a = api::card_answer(
        &store,
        "ext",
        "accept",
        "",
        Some("NEXT-BALL"),
        Some(&json!({"name": "side", "ball": "side job", "unverified": true})),
    )
    .unwrap();
    assert_eq!(a["created"].as_array().unwrap().len(), 2);
    // Answering a table that waits for nothing is an error.
    assert!(api::card_answer(&store, "ext", "accept", "", None, None).is_err());
    let rows = store.tables(Some(&run), false).unwrap();
    assert_eq!(rows.len(), 3, "{rows:?}");
    let next = rows.iter().find(|t| t.name == "ext-next1").unwrap();
    assert_eq!(
        next.after_id.as_deref(),
        Some(store.resolve("ext").unwrap().id.as_str())
    );
    // ext-next1 and side inherited the external server: answer nothing, stop them.
    for t in &rows {
        if t.name != "ext" {
            api::stop(&store, &t.id).unwrap();
        }
    }
    h.join().unwrap().unwrap();
    assert_eq!(store.resolve("ext").unwrap().status, "point");
}

// ---------------------------------------------------------------- nested tables

#[test]
fn sub_tables_have_a_depth_cap_and_inherit_player_and_cwd() {
    let d = tempfile::tempdir().unwrap();
    let store = Store::open(&d.path().join("upp.db")).unwrap();
    let c = api::create_json(
        &store,
        json!({"name": "parent", "ball": "b", "cwd": d.path(), "max_depth": 1,
               "player": {"kind": "shell", "command": "cat"},
               "referee": [{"kind": "command", "run": "true"}]}),
    )
    .unwrap();
    let pid = c["tables"][0]["id"].as_str().unwrap().to_string();
    let kid = api::create_child(
        &store,
        &pid,
        json!({"name": "kid", "ball": "k", "referee": [{"kind": "command", "run": "true"}]}),
    )
    .unwrap();
    assert_eq!(kid["depth"], 1);
    let kid_id = kid["tables"][0]["id"].as_str().unwrap();
    let ks = store.spec(kid_id).unwrap();
    assert!(ks.player.is_some(), "inherits the parent's player");
    assert_eq!(ks.cwd.as_deref(), Some(d.path().to_str().unwrap()));
    // max_depth 1: the kid (depth 1) may not open sub-tables.
    let e = api::create_child(
        &store,
        kid_id,
        json!({"name": "grandkid", "ball": "g", "referee": [{"kind": "command", "run": "true"}]}),
    )
    .unwrap_err();
    assert!(e.contains("depth cap"), "{e}");
    // A child without checks is still refused (no unverified points by inheritance).
    assert!(api::create_child(&store, &pid, json!({"name": "nochecks", "ball": "x"})).is_err());
}

#[test]
fn writer_children_of_a_worktree_table_get_their_own_worktree() {
    let d = tempfile::tempdir().unwrap();
    let store = Store::open(&d.path().join("upp.db")).unwrap();
    let c = api::create_json(
        &store,
        json!({"name": "parent", "ball": "b", "cwd": d.path(),
               "player": {"kind": "shell", "command": "cat"},
               "referee": [{"kind": "command", "run": "true"}]}),
    )
    .unwrap();
    let pid = c["tables"][0]["id"].as_str().unwrap().to_string();
    let repo = d.path().join("repo");
    store
        .set_worktree(
            &pid,
            &json!({"repo": repo, "branch": "upp/parent", "path": d.path().join("wt-parent")}),
        )
        .unwrap();
    let kids = api::create_child(
        &store,
        &pid,
        json!({"tables": [
            {"name": "writer-a", "ball": "a", "referee": [{"kind": "command", "run": "true"}]},
            {"name": "writer-b", "ball": "b", "referee": [{"kind": "command", "run": "true"}]},
            {"name": "reader", "ball": "r", "read_only": true, "referee": [{"kind": "command", "run": "true"}]}
        ]}),
    )
    .unwrap();
    let spec = |i: usize| {
        store
            .spec(kids["tables"][i]["id"].as_str().unwrap())
            .unwrap()
    };
    // Writers: each its own worktree, branched from the parent's branch, never the parent's cwd.
    for i in [0, 1] {
        let w = spec(i).worktree.expect("writer child gets a worktree");
        assert_eq!(w.repo.as_deref(), repo.to_str());
        assert_eq!(w.base.as_deref(), Some("upp/parent"));
        assert!(spec(i).cwd.is_none());
    }
    // Readers share the parent's working directory.
    assert!(spec(2).worktree.is_none());
    assert!(spec(2).cwd.is_some());
}

#[test]
fn the_wall_lease_counts_time_used_before_a_resume() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("upp.db");
    let store = Store::open(&db).unwrap();
    let c = api::create_json(
        &store,
        json!({"name": "t", "ball": "b", "cwd": d.path(), "lease": {"wall_secs": 60},
               "player": {"kind": "shell", "command": ["sh", fake("fake-coder.sh")]},
               "referee": [{"kind": "command", "run": "grep -qx 42 answer.txt"}]}),
    )
    .unwrap();
    let id = c["tables"][0]["id"].as_str().unwrap().to_string();
    let run = c["run_id"].as_str().unwrap().to_string();
    // An earlier run (before a crash) already used the whole minute.
    store.set_wall_used_ms(&id, 60_000).unwrap();
    let mut s = Scheduler::new(&db, 1, "test");
    s.run = Some(run);
    s.until_idle = true;
    s.run().unwrap();
    let t = store.table(&id).unwrap().unwrap();
    assert_eq!(t.status, "expired", "{t:?}");
    assert!(store.rallies(&id).unwrap().is_empty());
}

// ---------------------------------------------------------------- cancellation, effort

#[test]
fn stop_kills_the_running_player_at_once() {
    let d = tempfile::tempdir().unwrap();
    let pidfile = d.path().join("pid");
    let toml = format!(
        r#"
[[table]]
name = "hang"
ball = "x"
player = {{ kind = "shell", command = "cat >/dev/null; echo $$ > {pid}; sleep 60" }}
referee = [{{ kind = "command", run = "true" }}]
"#,
        pid = pidfile.display()
    );
    let f = d.path().join("m.toml");
    std::fs::write(&f, toml).unwrap();
    let db = d.path().join("upp.db");
    let store = Store::open(&db).unwrap();
    let c = api::create(&store, MatchFile::load(&f).unwrap(), None).unwrap();
    let mut s = Scheduler::new(&db, 1, "test");
    s.run = Some(c["run_id"].as_str().unwrap().into());
    s.until_idle = true;
    let h = std::thread::spawn(move || s.run());
    let t0 = Instant::now();
    while !pidfile.exists() {
        assert!(t0.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .to_string();
    let t1 = Instant::now();
    api::stop(&store, "hang").unwrap();
    h.join().unwrap().unwrap();
    assert!(t1.elapsed() < Duration::from_secs(5), "{:?}", t1.elapsed());
    let t = store.resolve("hang").unwrap();
    assert_eq!(t.status, "stopped", "{t:?}");
    // The player's process is gone.
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
}

#[test]
fn the_effort_ladder_reaches_the_player_per_rally() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[[table]]
name = "e"
ball = "x"
effort = ["low", "high"]
player = {{ kind = "shell", command = ["sh", "{coder}"], env = {{ FAKE_LOG = "log", FAKE_PASS_ON = "3" }} }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt" }}]
"#,
        coder = fake("fake-coder.sh")
    );
    let (_s, rows) = run_match(d.path(), &toml, 1);
    assert_eq!(rows[0].status, "point");
    let e = |n: u32| {
        std::fs::read_to_string(d.path().join(format!("log/effort-{n}.txt")))
            .unwrap()
            .trim()
            .to_string()
    };
    assert_eq!(
        (e(1), e(2), e(3)),
        ("low".into(), "high".into(), "high".into())
    );
}

// ---------------------------------------------------------------- git: provenance, worktrees

fn git(dir: &Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn worktree_tables_are_isolated_tied_to_trees_and_integrated() {
    let d = tempfile::tempdir().unwrap();
    let repo = d.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README"), "base\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "base",
        ],
    );
    let toml = format!(
        r#"
[defaults]
worktree = {{ repo = "repo" }}
referee = [{{ kind = "command", run = "test \"$(cat $FILE)\" = 42", env = {{ FILE = "x" }} }}]
[[table]]
name = "alpha"
ball = "write a.txt"
player = {{ kind = "shell", command = ["sh", "{coder}"], env = {{ FAKE_FILE = "a.txt", FAKE_PASS_ON = "1" }} }}
referee = [{{ kind = "command", run = "grep -qx 42 a.txt" }}]
[[table]]
name = "beta"
ball = "write b.txt"
player = {{ kind = "shell", command = ["sh", "{coder}"], env = {{ FAKE_FILE = "b.txt" }} }}
referee = [{{ kind = "command", run = "grep -qx 42 b.txt" }}]
"#,
        coder = fake("fake-coder.sh")
    );
    let (store, rows) = run_match(d.path(), &toml, 2);
    assert!(rows.iter().all(|t| t.status == "point"), "{rows:?}");
    let (a, b) = (by_name(&rows, "alpha"), by_name(&rows, "beta"));
    let (wa, wb) = (a.worktree.clone().unwrap(), b.worktree.clone().unwrap());
    assert_ne!(wa["path"], wb["path"]);
    // Isolated: each worktree holds only its own file; the user's checkout holds neither.
    let pa = PathBuf::from(wa["path"].as_str().unwrap());
    assert!(pa.join("a.txt").exists() && !pa.join("b.txt").exists());
    assert!(!repo.join("a.txt").exists() && !repo.join("b.txt").exists());
    // Provenance: each rally names the tree it checked; the failing and passing trees differ.
    let rb = store.rallies(&b.id).unwrap();
    assert_eq!(rb.len(), 2);
    assert_ne!(rb[0].provenance["tree"], rb[1].provenance["tree"]);
    assert_eq!(rb[1].provenance["changed"], json!(["b.txt"]));
    let card = &api::status(&store, "beta", false).unwrap()["rally_log"][1]["card"];
    assert_eq!(card["provenance"]["tree"], rb[1].provenance["tree"]);
    assert!(card["artifacts"]
        .as_array()
        .unwrap()
        .contains(&json!("b.txt")));
    // The point committed exactly the verified tree on the table's branch.
    let branch = wb["branch"].as_str().unwrap();
    let head_tree = git(&repo, &["rev-parse", &format!("{branch}^{{tree}}")]);
    assert_eq!(head_tree, rb[1].provenance["tree"].as_str().unwrap());
    // One integration step merges both verified points; the user's branch is untouched.
    let v = api::integrate(
        &store,
        &a.run_id,
        Some("main"),
        Some("test -f a.txt && test -f b.txt"),
    )
    .unwrap();
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["merged"], json!(["alpha", "beta"]));
    assert_eq!(git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "main");
    let files = git(
        &repo,
        &[
            "ls-tree",
            "-r",
            "--name-only",
            v["commit"].as_str().unwrap(),
        ],
    );
    assert!(files.contains("a.txt") && files.contains("b.txt") && files.contains("README"));
    let _: Value = v;
}

#[test]
fn the_orchestrator_runs_in_the_match_files_directory() {
    // A relative orchestrator command resolves against the match file, like table cwds.
    let d = tempfile::tempdir().unwrap();
    std::fs::copy(fake("fake-orchestrator.sh"), d.path().join("orch.sh")).unwrap();
    let toml = format!(
        r#"
[orchestrator]
player = {{ kind = "shell", command = ["sh", "orch.sh"], env = {{ ORCH_NOTE = "RELATIVE-OK" }} }}
[[table]]
name = "t"
ball = "x"
player = {{ kind = "shell", command = ["sh", "{coder}"], env = {{ FAKE_LOG = "log" }} }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt" }}]
"#,
        coder = fake("fake-coder.sh"),
    );
    let (_s, rows) = run_match(d.path(), &toml, 1);
    assert_eq!(rows[0].status, "point");
    let b2 = std::fs::read_to_string(d.path().join("log/ball-2.txt")).unwrap();
    assert!(b2.contains("Server note: RELATIVE-OK"), "{b2}");
}
