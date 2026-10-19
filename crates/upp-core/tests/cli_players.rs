// SPDX-License-Identifier: Apache-2.0
//! Claude Code and Codex players with fake CLIs (tests/fake-agents/*.py): one persistent
//! process per table fed rally by rally over stdin, delta-only returns, and token
//! accounting (fresh input, cache reads, output, cost) per rally.
#![cfg(unix)]

use serde_json::Value;
use std::path::{Path, PathBuf};
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

fn run_match(dir: &Path, toml: &str) -> (Store, Vec<TableRow>) {
    let f = dir.join("match.toml");
    std::fs::write(&f, toml).unwrap();
    let m = MatchFile::load(&f).unwrap();
    let db = dir.join("upp.db");
    let store = Store::open(&db).unwrap();
    let created = api::create(&store, m, None).unwrap();
    let run = created["run_id"].as_str().unwrap().to_string();
    let mut s = Scheduler::new(&db, 1, "test");
    s.run = Some(run.clone());
    s.until_idle = true;
    s.run().unwrap();
    let mut rows = store.tables(Some(&run), false).unwrap();
    rows.reverse();
    (store, rows)
}

fn lines(p: &Path) -> Vec<String> {
    std::fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

/// The referee fails with a long output until answer.txt holds 42.
const REFEREE: &str = r#"referee = [{ kind = "command", name = "answer is 42", run = "grep -qx 42 answer.txt || { seq 1 4000; echo 'AssertionError: expected 42'; exit 1; }" }]"#;

fn mode_events(store: &Store, id: &str) -> Vec<String> {
    store
        .events_since(0, 1000)
        .unwrap()
        .into_iter()
        .filter(|e| e.table_id == id && e.kind == "player_mode")
        .map(|e| e.data["mode"].as_str().unwrap_or("").to_string())
        .collect()
}

#[test]
fn claude_plays_every_rally_in_one_process_with_delta_returns_and_split_tokens() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[[table]]
name = "c"
ball = "Write 42 to answer.txt. LONG-BRIEF-MARKER"
player = {{ kind = "claude", bin = "{bin}", model = "haiku", tools = ["Read", "Edit"], env = {{ FAKE_LOG = "log", FAKE_ANSWERS = "41|41|42" }} }}
{REFEREE}
"#,
        bin = fake("fake-claude-stream.py")
    );
    let (store, rows) = run_match(d.path(), &toml);
    let t = &rows[0];
    assert_eq!(t.status, "point", "{t:?}");
    assert_eq!(t.trail, "..*");
    // One process for three rallies.
    let spawns = lines(&d.path().join("log/spawns.log"));
    assert_eq!(spawns.len(), 1, "{spawns:?}");
    let a = &spawns[0];
    for f in [
        "--input-format stream-json",
        "--output-format stream-json",
        "--model haiku",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--exclude-dynamic-system-prompt-sections",
        "--tools Read,Edit",
    ] {
        assert!(a.contains(f), "{f} missing in {a}");
    }
    assert!(!a.contains("--resume"), "{a}");
    // Rally 1 carries the brief; returns carry only the trimmed referee output.
    let turn =
        |n: u32| std::fs::read_to_string(d.path().join(format!("log/turn-{n}.txt"))).unwrap();
    assert!(turn(1).contains("LONG-BRIEF-MARKER"));
    for n in [2, 3] {
        let b = turn(n);
        assert!(
            !b.contains("LONG-BRIEF-MARKER") && !b.contains("Original ball"),
            "{b}"
        );
        assert!(
            b.contains("AssertionError: expected 42") && b.contains("bytes cut"),
            "{b}"
        );
        assert!(b.len() < 2600, "return ball is {} bytes", b.len());
    }
    // Accounting: fresh input (input + cache writes), cache reads and output apart; the
    // cumulative process cost becomes a per-rally cost.
    let r = store.rallies(&t.id).unwrap();
    assert_eq!(r.len(), 3);
    for x in &r {
        assert_eq!((x.tokens_in, x.tokens_cached, x.tokens_out), (110, 1000, 5));
        assert!((x.cost_usd - 0.01).abs() < 1e-9, "{}", x.cost_usd);
        assert_eq!(x.session.as_deref(), Some("sess-1"));
    }
    // Table tokens are billable tokens: 3 x (110 + 5 + 1000 x 0.1).
    assert_eq!(t.tokens, 3 * 215);
    assert!((t.cost_usd - 0.03).abs() < 1e-9);
    assert_eq!(
        mode_events(&store, &t.id),
        vec!["persistent (claude stream-json)"]
    );
    // The export carries the split.
    let ex = api::export_jsonl(&store, None).unwrap();
    let first: Value = serde_json::from_str(ex.lines().next().unwrap()).unwrap();
    assert_eq!(first["tokens_cached"], 1000);
    assert_eq!(first["billable_tokens"], 215);
}

#[test]
fn claude_restarts_on_the_same_session_when_the_rally_effort_changes() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[[table]]
name = "c"
ball = "x"
effort = ["low", "high"]
player = {{ kind = "claude", bin = "{bin}", lean = false, env = {{ FAKE_LOG = "log", FAKE_ANSWERS = "41|41|42" }} }}
{REFEREE}
"#,
        bin = fake("fake-claude-stream.py")
    );
    let (store, rows) = run_match(d.path(), &toml);
    assert_eq!(rows[0].status, "point");
    let spawns = lines(&d.path().join("log/spawns.log"));
    // low (rally 1), then high with --resume (rallies 2 and 3 share that process).
    assert_eq!(spawns.len(), 2, "{spawns:?}");
    assert!(spawns[0].contains("--effort low") && !spawns[0].contains("--resume"));
    assert!(spawns[1].contains("--effort high") && spawns[1].contains("--resume sess-1"));
    assert!(!spawns[0].contains("--strict-mcp-config"), "lean = false");
    // Costs are per process: each rally still costs 0.01.
    let r = store.rallies(&rows[0].id).unwrap();
    assert!(r.iter().all(|x| (x.cost_usd - 0.01).abs() < 1e-9));
}

#[test]
fn codex_plays_every_rally_as_a_turn_on_one_app_server_thread() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[[table]]
name = "x"
ball = "Write 42. LONG-BRIEF-MARKER"
effort = ["low", "medium"]
player = {{ kind = "codex", bin = "{bin}", model = "gpt-5.6-luna", tools = [], system_prompt = "You are a terse coder.", args = ["--dangerously-bypass-approvals-and-sandbox", "-c", "x.y=1"], env = {{ FAKE_LOG = "log", FAKE_ANSWERS = "41|42", CODEX_HOME = "codexhome" }} }}
{REFEREE}
"#,
        bin = fake("fake-codex-appserver.py")
    );
    // A user config with an MCP server: lean mode turns it off for the player.
    std::fs::create_dir_all(d.path().join("codexhome")).unwrap();
    std::fs::write(
        d.path().join("codexhome/config.toml"),
        "[mcp_servers.search]\ncommand = \"x\"\n[mcp_servers.\"odd.name\"]\ncommand = \"y\"\n",
    )
    .unwrap();
    let (store, rows) = run_match(d.path(), &toml);
    let t = &rows[0];
    assert_eq!(t.status, "point", "{t:?}");
    let spawns = lines(&d.path().join("log/spawns.log"));
    assert_eq!(spawns.len(), 1, "{spawns:?}");
    let a = &spawns[0];
    assert!(a.starts_with("app-server "), "{a}");
    for f in [
        "-c features.apps=false",
        "-c features.memories=false",
        "-c web_search=\"disabled\"",
        "-c mcp_servers.search.enabled=false",
        "-c mcp_servers.\"odd.name\".enabled=false",
        "-c features.shell_tool=false",
        "-c x.y=1",
    ] {
        assert!(a.contains(f), "{f} missing in {a}");
    }
    assert!(
        !a.contains("--dangerously"),
        "exec-only flag passed to the server: {a}"
    );
    let th: Value = serde_json::from_str(&lines(&d.path().join("log/thread.log"))[0]).unwrap();
    assert_eq!(th["method"], "thread/start");
    assert_eq!(th["params"]["model"], "gpt-5.6-luna");
    assert_eq!(th["params"]["approvalPolicy"], "never");
    assert_eq!(th["params"]["sandbox"], "danger-full-access");
    assert_eq!(th["params"]["baseInstructions"], "You are a terse coder.");
    let turn = |n: u32| -> Value {
        serde_json::from_str(
            &std::fs::read_to_string(d.path().join(format!("log/turn-{n}.json"))).unwrap(),
        )
        .unwrap()
    };
    assert_eq!(turn(1)["effort"], "low");
    assert_eq!(turn(2)["effort"], "medium");
    assert_eq!(turn(2)["thread"], "th-1");
    let b2 = turn(2)["text"].as_str().unwrap().to_string();
    assert!(!b2.contains("LONG-BRIEF-MARKER") && b2.len() < 2600, "{b2}");
    // The approval request was answered (declined), not left hanging.
    let ap: Value = serde_json::from_str(
        &std::fs::read_to_string(d.path().join("log/approval-1.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(ap["result"]["decision"], "decline");
    let r = store.rallies(&t.id).unwrap();
    for x in &r {
        assert_eq!((x.tokens_in, x.tokens_cached, x.tokens_out), (200, 800, 20));
        assert_eq!(x.session.as_deref(), Some("th-1"));
    }
    assert_eq!(t.tokens, 2 * (200 + 20 + 80));
    assert_eq!(
        mode_events(&store, &t.id),
        vec!["persistent (codex app-server)"]
    );
}

#[test]
fn a_failed_app_server_turn_is_a_fault_and_the_next_turn_runs_on_the_same_thread() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[[table]]
name = "x"
ball = "x"
player = {{ kind = "codex", bin = "{bin}", env = {{ FAKE_LOG = "log", FAKE_FAIL_TURN = "1", FAKE_ANSWERS = "42" }} }}
{REFEREE}
"#,
        bin = fake("fake-codex-appserver.py")
    );
    let (store, rows) = run_match(d.path(), &toml);
    assert_eq!(rows[0].status, "point");
    assert_eq!(rows[0].trail, "!*");
    let r = store.rallies(&rows[0].id).unwrap();
    assert_eq!(r[0].verdict, "fault");
    assert!(r[0].note.contains("stream disconnected"), "{}", r[0].note);
    assert_eq!(lines(&d.path().join("log/spawns.log")).len(), 1);
}

#[test]
fn without_an_app_server_codex_falls_back_to_one_exec_per_rally() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[[table]]
name = "x"
ball = "x"
player = {{ kind = "codex", bin = "{bin}", env = {{ FAKE_LOG = "log", FAKE_NO_SERVER = "1", FAKE_ANSWERS = "41|42" }} }}
{REFEREE}
"#,
        bin = fake("fake-codex-appserver.py")
    );
    let (store, rows) = run_match(d.path(), &toml);
    assert_eq!(rows[0].status, "point", "{:?}", rows[0]);
    let spawns = lines(&d.path().join("log/spawns.log"));
    // app-server tried once, then exec, then exec resume.
    assert!(spawns[0].starts_with("app-server"), "{spawns:?}");
    assert!(spawns[1].starts_with("exec --json"), "{spawns:?}");
    assert!(spawns[2].starts_with("exec resume"), "{spawns:?}");
    assert!(spawns[2].contains("th-exec"), "{spawns:?}");
    let modes = mode_events(&store, &rows[0].id);
    assert_eq!(modes.len(), 1, "{modes:?}");
    assert!(
        modes[0].starts_with("per-rally (persistent mode unavailable"),
        "{modes:?}"
    );
    // exec usage is split too: input_tokens includes the cached part.
    let r = store.rallies(&rows[0].id).unwrap();
    assert_eq!(
        (r[0].tokens_in, r[0].tokens_cached, r[0].tokens_out),
        (200, 800, 20)
    );
}

#[test]
fn exec_only_codex_arguments_keep_the_per_rally_path() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[[table]]
name = "x"
ball = "x"
player = {{ kind = "codex", bin = "{bin}", args = ["--oss"], env = {{ FAKE_LOG = "log", FAKE_ANSWERS = "42" }} }}
{REFEREE}
"#,
        bin = fake("fake-codex-appserver.py")
    );
    let (store, rows) = run_match(d.path(), &toml);
    assert_eq!(rows[0].status, "point");
    let spawns = lines(&d.path().join("log/spawns.log"));
    assert_eq!(spawns.len(), 1);
    assert!(spawns[0].starts_with("exec --json") && spawns[0].contains("--oss"));
    assert_eq!(mode_events(&store, &rows[0].id), vec!["per-rally"]);
}

// ---------------------------------------------------------------- UnumSearch

#[test]
fn search_macros_and_context_search_put_capped_hits_into_the_brief() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("search.log");
    std::env::set_var("UPP_UNUMSEARCH", fake("fake-unumsearch.py"));
    std::env::set_var("FAKE_SEARCH_LOG", &log);
    let toml = format!(
        r#"
[[table]]
name = "s"
ball = "Fix it. Where parse lives: {{{{search:parse_line}}}} and {{{{search:nothing}}}}"
context_search = ["Config", "  "]
player = {{ kind = "shell", command = ["sh", "{coder}"], env = {{ FAKE_LOG = "log", FAKE_PASS_ON = "1" }} }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt" }}]
"#,
        coder = fake("fake-coder.sh")
    );
    let (_s, rows) = run_match(d.path(), &toml);
    assert_eq!(rows[0].status, "point");
    let b1 = std::fs::read_to_string(d.path().join("log/ball-1.txt")).unwrap();
    // Paths relative to the table's cwd, 8 hits per query, lines capped.
    assert!(
        b1.contains("# unumsearch: parse_line\nsrc/f0.rs:1: fn parse_line_0()"),
        "{b1}"
    );
    assert!(
        b1.contains("src/f7.rs:8:") && !b1.contains("src/f8.rs"),
        "{b1}"
    );
    assert!(b1.contains("(first 8 hits)"), "{b1}");
    assert!(!b1.contains(&"x".repeat(250)), "a hit line was not capped");
    assert!(b1.contains("# unumsearch: nothing\n(no matches)"), "{b1}");
    assert!(
        b1.contains("Code search results (unumsearch") && b1.contains("# Config\nsrc/f0.rs:1:"),
        "{b1}"
    );
    // One literal search per query (the blank one is skipped), in the table's directory.
    let calls = lines(&log);
    assert_eq!(calls.len(), 3, "{calls:?}");
    let first: Vec<String> = serde_json::from_str(&calls[0]).unwrap();
    assert_eq!(first[..5], ["search", "-F", "-m", "8", "parse_line"]);
    // A missing binary is a note, never a failed table.
    std::env::set_var("UPP_UNUMSEARCH", d.path().join("no-such-unumsearch"));
    let out = upp_core::search::render("{{search:x}}", d.path());
    assert!(out.contains("(unumsearch unavailable"), "{out}");
    std::env::remove_var("UPP_UNUMSEARCH");
    std::env::remove_var("FAKE_SEARCH_LOG");
}

#[test]
fn search_attaches_only_the_unumsearch_mcp_server_to_cli_players() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("codexhome")).unwrap();
    std::fs::write(
        d.path().join("codexhome/config.toml"),
        "[mcp_servers.unumsearch]\ncommand = \"old\"\n[mcp_servers.other]\ncommand = \"y\"\n",
    )
    .unwrap();
    let toml = format!(
        r#"
[[table]]
name = "c"
ball = "x"
cwd = "c"
player = {{ kind = "claude", bin = "{claude}", tools = [], search = "unumsearch", env = {{ FAKE_LOG = "log" }} }}
{REFEREE}

[[table]]
name = "x"
ball = "x"
cwd = "x"
player = {{ kind = "codex", bin = "{codex}", search = "/opt/bin/unumsearch", env = {{ FAKE_LOG = "log", CODEX_HOME = "../codexhome" }} }}
{REFEREE}
"#,
        claude = fake("fake-claude-stream.py"),
        codex = fake("fake-codex-appserver.py")
    );
    let (_s, rows) = run_match(d.path(), &toml);
    assert!(rows.iter().all(|r| r.status == "point"), "{rows:?}");
    let c = &lines(&d.path().join("c/log/spawns.log"))[0];
    assert!(
        c.contains(r#"--mcp-config {"mcpServers":{"unumsearch":{"args":["mcp"],"command":"unumsearch","type":"stdio"}}}"#),
        "{c}"
    );
    assert_eq!(c.matches("--strict-mcp-config").count(), 1, "{c}");
    assert!(c.contains("--allowedTools mcp__unumsearch__search,mcp__unumsearch__find_files,mcp__unumsearch__index_status"), "{c}");
    let x = &lines(&d.path().join("x/log/spawns.log"))[0];
    assert!(x.contains("-c mcp_servers.other.enabled=false"), "{x}");
    assert!(!x.contains("mcp_servers.unumsearch.enabled=false"), "{x}");
    assert!(
        x.contains(r#"-c mcp_servers.unumsearch.command="/opt/bin/unumsearch" -c mcp_servers.unumsearch.args=["mcp"] -c mcp_servers.unumsearch.enabled=true"#),
        "{x}"
    );
}
