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
