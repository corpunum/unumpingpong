// SPDX-License-Identifier: Apache-2.0
//! End-to-end tests of the `upp` binary with scripted fake agents.
#![cfg(unix)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn upp() -> Command {
    Command::new(env!("CARGO_BIN_EXE_upp"))
}

fn fake(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fake-agents")
        .join(name);
    std::fs::canonicalize(p)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn write_match(dir: &Path, body: &str) -> PathBuf {
    let f = dir.join("match.toml");
    std::fs::write(&f, body).unwrap();
    f
}

fn json_out(c: &mut Command) -> Value {
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

#[test]
fn run_prints_progress_and_exit_codes() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("upp.db");
    let f = write_match(
        d.path(),
        &format!(
            r#"
[defaults]
player = {{ kind = "shell", command = ["sh", "{}"] }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt", name = "is-42" }}]
[[table]]
name = "wins"
ball = "x"
cwd = "a"
[[table]]
name = "loses"
ball = "x"
cwd = "b"
lease = {{ max_rallies = 1 }}
"#,
            fake("fake-coder.sh")
        ),
    );
    std::fs::create_dir_all(d.path().join("a")).unwrap();
    std::fs::create_dir_all(d.path().join("b")).unwrap();
    let o = upp()
        .arg("--db")
        .arg(&db)
        .arg("run")
        .arg(&f)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("referee FAIL: FAIL is-42"), "{out}");
    assert!(out.contains("POINT"), "{out}");
    assert!(out.contains("LOST"), "{out}");
    assert!(out.contains("1/2 tables scored"), "{out}");

    let list = json_out(upp().arg("--db").arg(&db).args(["list", "--json"]));
    assert_eq!(list["tables"].as_array().unwrap().len(), 2);
    let st = json_out(upp().arg("--db").arg(&db).args(["status", "wins"]));
    assert_eq!(st["status"], "point");
    assert_eq!(st["rally_log"].as_array().unwrap().len(), 2);
    let exp = upp().arg("--db").arg(&db).arg("export").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&exp.stdout).lines().count(), 3);
}

#[test]
fn validate_rejects_a_self_judging_table() {
    let d = tempfile::tempdir().unwrap();
    let f = write_match(
        d.path(),
        r#"
[[table]]
name = "x"
ball = "b"
player = { kind = "shell", command = "cat" }
referee = [{ kind = "judge", player = { kind = "shell", command = "cat" } }]
"#,
    );
    let o = upp().arg("validate").arg(&f).output().unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("judge"));
}

#[test]
fn external_server_answers_over_the_cli() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("upp.db");
    let f = write_match(
        d.path(),
        &format!(
            r#"
[[table]]
name = "ext"
ball = "write 42"
player = {{ kind = "shell", command = ["sh", "{}"], env = {{ FAKE_LOG = "log", FAKE_PASS_ON = "2" }} }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt" }}]
server = {{ mode = "external", timeout_secs = 30 }}
"#,
            fake("fake-coder.sh")
        ),
    );
    let mut run = upp()
        .arg("--db")
        .arg(&db)
        .arg("run")
        .arg(&f)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let wait_pending = |rally: u64| {
        let t0 = Instant::now();
        loop {
            let p = json_out(upp().arg("--db").arg(&db).arg("pending"));
            if let Some(b) = p["pending"].as_array().and_then(|a| a.first()) {
                if b["rally"] == rally {
                    return b.clone();
                }
            }
            assert!(t0.elapsed() < Duration::from_secs(20), "no pending ball");
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    // Rally 1 fails the referee; the outside server returns it with a note.
    let b = wait_pending(1);
    assert_eq!(b["payload"]["referee"]["passed"], false);
    // The server gets a card and the referee output, not the transcript.
    assert!(b["payload"]["card"]["summary"].is_string(), "{b}");
    assert!(b["payload"].get("answer").is_none(), "{b}");
    let a = json_out(
        upp()
            .arg("--db")
            .arg(&db)
            .args(["answer", "ext", "return", "use", "exactly", "42"]),
    );
    assert_eq!(a["rally"], 1);
    // Rally 2 passes; accept scores the point.
    let b = wait_pending(2);
    assert_eq!(b["payload"]["referee"]["passed"], true);
    json_out(upp().arg("--db").arg(&db).args(["answer", "ext", "accept"]));
    assert!(run.wait().unwrap().success());
    let ball2 = std::fs::read_to_string(d.path().join("log/ball-2.txt")).unwrap();
    assert!(ball2.contains("Server note: use exactly 42"), "{ball2}");
}

struct Mcp {
    child: std::process::Child,
    out: BufReader<std::process::ChildStdout>,
    id: u64,
}

impl Mcp {
    fn call(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params});
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], self.id);
        v["result"].clone()
    }
    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.call("tools/call", json!({"name": name, "arguments": args}));
        assert_eq!(r["isError"], false, "{r}");
        r["structuredContent"].clone()
    }
}

#[test]
fn mcp_server_creates_and_serves_tables() {
    let d = tempfile::tempdir().unwrap();
    let mut child = upp()
        .arg("--db")
        .arg(d.path().join("upp.db"))
        .arg("mcp")
        .current_dir(d.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let out = BufReader::new(child.stdout.take().unwrap());
    let mut m = Mcp { child, out, id: 0 };
    let init = m.call("initialize", json!({"protocolVersion": "2025-06-18"}));
    assert_eq!(init["serverInfo"]["name"], "unumpingpong");
    let tools = m.call("tools/list", json!({}));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for n in [
        "table_create",
        "table_status",
        "table_list",
        "ball_answer",
        "table_stop",
        "table_wait",
        "cards_wait",
        "card_answer",
        "transcript",
        "integrate",
    ] {
        assert!(names.contains(&n), "{names:?}");
    }
    // An outside orchestrator (this test) acts as the server.
    let created = m.tool(
        "table_create",
        json!({"match": {
            "name": "mcp-table",
            "goal": "answer.txt holds 42",
            "cwd": d.path(),
            "player": {"kind": "shell", "command": ["sh", fake("fake-coder.sh")]},
            "referee": [{"kind": "command", "run": "grep -qx 42 answer.txt"}],
            "server": {"mode": "external"}
        }}),
    );
    let id = created["tables"][0]["id"].as_str().unwrap().to_string();
    let w = m.tool("table_wait", json!({"tables": [id], "timeout_secs": 20}));
    assert_eq!(w["reason"], "awaiting_server", "{w}");
    assert_eq!(w["pending"][0]["referee"]["passed"], false);
    m.tool(
        "ball_answer",
        json!({"table": id, "verdict": "return", "note": "42, not 41"}),
    );
    let w = m.tool("table_wait", json!({"tables": [id], "timeout_secs": 20}));
    assert_eq!(w["pending"][0]["referee"]["passed"], true, "{w}");
    m.tool("ball_answer", json!({"table": id, "verdict": "accept"}));
    let w = m.tool("table_wait", json!({"tables": [id], "timeout_secs": 20}));
    assert_eq!(w["reason"], "ended");
    let st = m.tool("table_status", json!({"table": id}));
    assert_eq!(st["status"], "point");
    assert_eq!(st["trail"], ".*");
    let l = m.tool("table_list", json!({}));
    assert_eq!(l["tables"].as_array().unwrap().len(), 1);
    // Errors are tool errors, not protocol errors.
    let r = m.call(
        "tools/call",
        json!({"name": "ball_answer", "arguments": {"table": id, "verdict": "maybe"}}),
    );
    assert_eq!(r["isError"], true);
    drop(m.child.stdin.take());
    let _ = m.child.wait();
}

fn spawn_mcp(dir: &Path) -> Mcp {
    let mut child = upp()
        .arg("--db")
        .arg(dir.join("upp.db"))
        .arg("mcp")
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let out = BufReader::new(child.stdout.take().unwrap());
    let mut m = Mcp { child, out, id: 0 };
    m.call("initialize", json!({"protocolVersion": "2025-06-18"}));
    m
}

#[test]
fn claude_code_style_orchestration_over_mcp_cards() {
    // The recipe from docs/integrations/claude-code.md: open external tables, then loop
    // cards_wait -> card_answer, fetching a transcript only when a card is not enough.
    let d = tempfile::tempdir().unwrap();
    let mut m = spawn_mcp(d.path());
    let mk = |name: &str| {
        json!({
            "name": name,
            "ball": "write 42",
            "cwd": d.path().join(name),
            "player": {"kind": "shell", "command": ["sh", fake("fake-coder.sh")], "env": {"FAKE_CARD": "1"}},
            "referee": [{"kind": "command", "run": "grep -qx 42 answer.txt"}],
            "server": {"mode": "external"}
        })
    };
    let r1 = m.tool("table_create", json!({"match": mk("one")}));
    let r2 = m.tool("table_create", json!({"match": mk("two")}));
    let ids = [
        r1["tables"][0]["id"].as_str().unwrap().to_string(),
        r2["tables"][0]["id"].as_str().unwrap().to_string(),
    ];
    let mut answered = 0;
    let mut points = 0;
    let mut since: Option<i64> = None;
    let t0 = Instant::now();
    while points < 2 {
        assert!(t0.elapsed() < Duration::from_secs(30), "stalled");
        let mut args = json!({"tables": ids, "timeout_secs": 20});
        if let Some(s) = since {
            args["since"] = json!(s);
        }
        let w = m.tool("cards_wait", args);
        since = w["cursor"].as_i64();
        for c in w["cards"].as_array().unwrap() {
            assert!(c.get("answer").is_none());
            assert!(!c.to_string().contains("SECRET-BODY-LINE"));
            if c["kind"] == "final" {
                if c["status"] == "point" {
                    points += 1;
                }
                continue;
            }
            if c["needs_answer"] != true {
                continue;
            }
            let table = c["table_id"].as_str().unwrap();
            if c["referee"]["passed"] == true {
                m.tool("card_answer", json!({"table": table, "verdict": "accept"}));
            } else {
                // Not enough in the card? Fetch the transcript of just this rally.
                let tr = m.tool("transcript", json!({"table": table, "rally": c["rally"]}));
                assert!(tr["rallies"][0]["answer"]
                    .as_str()
                    .unwrap()
                    .contains("SECRET-BODY-LINE"));
                m.tool(
                    "card_answer",
                    json!({"table": table, "verdict": "return", "note": "42, not 41"}),
                );
            }
            answered += 1;
        }
    }
    assert_eq!(answered, 4);
    let st = m.tool("table_status", json!({"table": ids[0]}));
    assert_eq!(st["status"], "point");
    assert!(st["orchestrator_tokens"].as_u64().unwrap() > 0);
    assert!(st["player_tokens"].is_u64());
    drop(m.child.stdin.take());
    let _ = m.child.wait();
}

#[test]
fn a_player_opens_sub_tables_and_its_rally_waits_for_them() {
    // The parent player opens a sub-table with `upp create --parent $UPP_TABLE` and answers
    // at once; its referee checks the child's work, so the point proves the rally waited.
    // One slot only: the waiting parent must free it for the child.
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("upp.db");
    let child_dir = d.path().join("child");
    std::fs::create_dir_all(&child_dir).unwrap();
    let child = d.path().join("child.toml");
    std::fs::write(
        &child,
        format!(
            r#"
[[table]]
name = "kid"
ball = "write 42"
cwd = "{}"
player = {{ kind = "shell", command = ["sh", "{}"] }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt" }}]
"#,
            child_dir.display(),
            fake("fake-coder.sh")
        ),
    )
    .unwrap();
    let parent_cmd = format!(
        "cat >/dev/null; \"{bin}\" create {child} --parent \"$UPP_TABLE\" > opened.json && echo opened a sub-table",
        bin = env!("CARGO_BIN_EXE_upp"),
        child = child.display()
    );
    let f = write_match(
        d.path(),
        &format!(
            r#"
[[table]]
name = "parent"
ball = "delegate"
player = {{ kind = "shell", command = {cmd} }}
referee = [{{ kind = "command", run = "grep -qx 42 child/answer.txt" }}]
"#,
            cmd = serde_json::to_string(&parent_cmd).unwrap()
        ),
    );
    let o = upp()
        .arg("--db")
        .arg(&db)
        .args(["run", "--parallel", "1"])
        .arg(&f)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        o.status.success(),
        "{out}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let st = json_out(upp().arg("--db").arg(&db).args(["status", "parent"]));
    assert_eq!(st["trail"], "*", "{st}");
    assert_eq!(st["children"][0]["name"], "kid");
    assert_eq!(st["children"][0]["status"], "point");
    let card = &st["rally_log"][0]["card"];
    assert_eq!(card["children"][0]["status"], "point", "{card}");
    let kid = json_out(upp().arg("--db").arg(&db).args(["status", "kid"]));
    assert_eq!(
        (kid["depth"].as_u64(), kid["trail"].as_str()),
        (Some(1), Some(".*"))
    );
    assert!(out.contains("opened sub-tables"), "{out}");
}

#[test]
fn a_crashed_run_resumes_after_its_last_rally() {
    // `upp run` is killed (SIGKILL) while rally 2 hangs; `upp resume` continues from the
    // recorded rally 1 and scores on rally 2.
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("upp.db");
    let f = write_match(
        d.path(),
        &format!(
            r#"
[[table]]
name = "crashy"
ball = "write 42"
player = {{ kind = "shell", command = ["sh", "{}"], env = {{ FAKE_HANG_ON = "2", FAKE_LOG = "log" }} }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt" }}]
"#,
            fake("fake-coder.sh")
        ),
    );
    for args in [
        &["init", "-q"][..],
        &["add", "-A"],
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
    ] {
        let o = std::process::Command::new("git")
            .args(args)
            .current_dir(d.path())
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    let mut run = upp()
        .arg("--db")
        .arg(&db)
        .arg("run")
        .arg(&f)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while !d.path().join(".fake-hung").exists() {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "rally 2 never started"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // The interrupted player had already changed a file before the crash.
    std::fs::write(d.path().join("partial.txt"), "half done\n").unwrap();
    run.kill().unwrap();
    run.wait().unwrap();
    let st = json_out(upp().arg("--db").arg(&db).args(["status", "crashy"]));
    assert_eq!(st["trail"], ".", "{st}");
    let o = upp().arg("--db").arg(&db).arg("resume").output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let st = json_out(upp().arg("--db").arg(&db).args(["status", "crashy"]));
    assert_eq!(st["status"], "point", "{st}");
    assert_eq!(st["trail"], ".*");
    assert_eq!(st["resumes"], 1);
    let ns: Vec<u64> = st["rally_log"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["n"].as_u64().unwrap())
        .collect();
    assert_eq!(ns, [1, 2]);
    // The resumed rally 2 carried the referee output of rally 1 and the brief (shell
    // players keep no session).
    let b2 = std::fs::read_to_string(d.path().join("log/ball-2.txt")).unwrap();
    assert!(
        b2.contains("Failed checks") && b2.contains("Original ball"),
        "{b2}"
    );
    // Not a blind replay: rally 2 was in flight, so the player is told what changed since
    // it started and to continue from there.
    assert!(
        b2.starts_with("Recovery:") && b2.contains("partial.txt"),
        "{b2}"
    );
}

#[test]
fn cards_cli_and_parallel_auto_fallback() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("upp.db");
    let f = write_match(
        d.path(),
        &format!(
            r#"
[defaults]
max_parallel = "auto"
[[table]]
name = "c"
ball = "x"
player = {{ kind = "shell", command = ["sh", "{}"] }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt" }}]
"#,
            fake("fake-coder.sh")
        ),
    );
    let o = upp()
        .arg("--db")
        .arg(&db)
        .arg("run")
        .arg(&f)
        .output()
        .unwrap();
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--parallel auto -> 4"));
    let c = json_out(upp().arg("--db").arg(&db).args(["cards", "--table", "c"]));
    let kinds: Vec<&str> = c["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["rally", "rally", "final"]);
    let t = json_out(
        upp()
            .arg("--db")
            .arg(&db)
            .args(["transcript", "c", "--rally", "2"]),
    );
    assert_eq!(t["rallies"].as_array().unwrap().len(), 1);
}

/// A git repo with one committed file `a.txt` = "base".
fn git_repo(dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let g = |args: &[&str]| {
        let o = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    };
    g(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "base\n").unwrap();
    g(&["add", "a.txt"]);
    g(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "base",
    ]);
    repo
}

/// Parent table in its own worktree whose player writes `parent_write` and opens one writer
/// sub-table writing `kid_write`; returns (parent status, parent worktree path).
fn merge_up_case(
    d: &Path,
    parent_write: &str,
    kid_write: &str,
    parent_referee: &str,
) -> (Value, PathBuf) {
    let repo = git_repo(d);
    let db = d.join("upp.db");
    let child = d.join("child.toml");
    let kid_cmd = format!("cat >/dev/null; {kid_write}; echo done");
    std::fs::write(
        &child,
        format!(
            r#"
[[table]]
name = "kid"
ball = "write"
player = {{ kind = "shell", command = {} }}
referee = [{{ kind = "command", run = "true" }}]
"#,
            serde_json::to_string(&vec!["sh", "-c", kid_cmd.as_str()]).unwrap()
        ),
    )
    .unwrap();
    let parent_cmd = format!(
        "cat >/dev/null; {parent_write}; \"{bin}\" create {child} --parent \"$UPP_TABLE\" > /dev/null && echo delegated",
        bin = env!("CARGO_BIN_EXE_upp"),
        child = child.display()
    );
    let f = write_match(
        d,
        &format!(
            r#"
[[table]]
name = "parent"
ball = "delegate"
worktree = {{ repo = "{repo}" }}
lease = {{ max_rallies = 1 }}
player = {{ kind = "shell", command = {cmd} }}
referee = [{{ kind = "command", run = {referee} }}]
"#,
            repo = repo.display(),
            cmd = serde_json::to_string(&vec!["sh", "-c", parent_cmd.as_str()]).unwrap(),
            referee = serde_json::to_string(parent_referee).unwrap()
        ),
    );
    let o = upp()
        .arg("--db")
        .arg(&db)
        .args(["run", "--parallel", "1"])
        .arg(&f)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&o.stderr).contains("panicked"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let st = json_out(
        upp()
            .arg("--db")
            .arg(&db)
            .args(["status", "parent", "--full"]),
    );
    let wt = PathBuf::from(st["worktree"]["path"].as_str().unwrap_or_default());
    (st, wt)
}

#[test]
fn a_scored_writer_sub_table_merges_into_its_parent_worktree() {
    let d = tempfile::tempdir().unwrap();
    let (st, wt) = merge_up_case(
        d.path(),
        "echo parent > parent.txt",
        "echo kid > kid.txt",
        "test -f kid.txt && test -f parent.txt",
    );
    // The parent's referee saw the child's file in its own worktree: the merge came first.
    assert_eq!(st["trail"], "*", "{st}");
    assert_eq!(
        std::fs::read_to_string(wt.join("kid.txt")).unwrap(),
        "kid\n"
    );
    let log = Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["log", "--format=%s"])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log.stdout);
    assert!(log.contains("upp: merge kid"), "{log}");
}

#[test]
fn a_merge_conflict_returns_the_ball_and_keeps_the_parent_files() {
    let d = tempfile::tempdir().unwrap();
    let (st, wt) = merge_up_case(d.path(), "echo parent > a.txt", "echo kid > a.txt", "true");
    // The referee itself passes, but the merge check fails: no point.
    assert_eq!(st["trail"], ".", "{st}");
    assert!(
        st.to_string()
            .contains("merge conflict with sub-table 'kid'"),
        "{st}"
    );
    assert_eq!(
        std::fs::read_to_string(wt.join("a.txt")).unwrap(),
        "parent\n"
    );
    let s = Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        s.stdout.is_empty(),
        "the aborted merge leaves a clean tree: {}",
        String::from_utf8_lossy(&s.stdout)
    );
}
