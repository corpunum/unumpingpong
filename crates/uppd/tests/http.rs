// SPDX-License-Identifier: Apache-2.0
//! uppd end to end: create a table over HTTP, wait for its point, read the SSE stream.
#![cfg(unix)]

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn fake(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fake-agents")
        .join(name);
    std::fs::canonicalize(p)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn http_api_and_sse() {
    let d = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let child = Command::new(env!("CARGO_BIN_EXE_uppd"))
        .args(["--listen", &addr, "--parallel", "2", "--db"])
        .arg(d.path().join("upp.db"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _guard = KillOnDrop(child);
    let base = format!("http://{addr}");
    let t0 = Instant::now();
    while ureq::get(&format!("{base}/health")).call().is_err() {
        assert!(t0.elapsed() < Duration::from_secs(10), "uppd did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    let h: Value = ureq::get(&format!("{base}/health"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(h["parallel"], 2);

    let created: Value = ureq::post(&format!("{base}/api/tables"))
        .send_json(json!({
            "name": "http-table",
            "ball": "write 42",
            "cwd": d.path(),
            "player": {"kind": "shell", "command": ["sh", fake("fake-coder.sh")]},
            "referee": [{"kind": "command", "run": "grep -qx 42 answer.txt"}]
        }))
        .unwrap()
        .into_json()
        .unwrap();
    let id = created["tables"][0]["id"].as_str().unwrap().to_string();
    let w: Value = ureq::get(&format!("{base}/api/wait?tables={id}&timeout=20"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(w["reason"], "ended", "{w}");
    let st: Value = ureq::get(&format!("{base}/api/tables/{id}"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(st["status"], "point");
    assert_eq!(st["rally_log"].as_array().unwrap().len(), 2);

    // Bad input is a 400 with a message.
    let e = ureq::post(&format!("{base}/api/tables")).send_string("{\"name\":\"x\"}");
    assert!(matches!(e, Err(ureq::Error::Status(400, _))));
    assert!(matches!(
        ureq::get(&format!("{base}/nope")).call(),
        Err(ureq::Error::Status(404, _))
    ));

    // SSE from the start: the stream replays every event, ending with the point.
    let mut s = std::net::TcpStream::connect(&addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "GET /api/events?since=0 HTTP/1.1\r\nHost: {addr}\r\n\r\n"
    )
    .unwrap();
    let mut got = String::new();
    let mut buf = [0u8; 4096];
    while !got.contains("event: point") {
        let n = s.read(&mut buf).unwrap();
        assert!(n > 0, "stream closed: {got}");
        got.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    assert!(got.contains("text/event-stream"));
    assert!(got.contains("event: referee"));

    // Cards by default; transcripts on demand.
    assert!(st["rally_log"][0]["card"]["summary"].is_string(), "{st}");
    assert!(st["rally_log"][0].get("answer").is_none());
    let tr: Value = ureq::get(&format!("{base}/api/tables/{id}/transcript?rally=1"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert!(tr["rallies"][0]["answer"].is_string());
    let cards: Value = ureq::get(&format!("{base}/api/cards?tables={id}&since=0&timeout=5"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(cards["cards"].as_array().unwrap().len(), 3, "{cards}");
    // A sub-table over HTTP: ?parent= on an ended table is refused.
    let e = ureq::post(&format!("{base}/api/tables?parent={id}")).send_json(json!({
        "name": "kid", "ball": "x", "referee": [{"kind": "command", "run": "true"}]
    }));
    assert!(matches!(e, Err(ureq::Error::Status(400, _))));

    let exp = ureq::get(&format!("{base}/api/export"))
        .call()
        .unwrap()
        .into_string()
        .unwrap();
    assert_eq!(exp.lines().count(), 2);
}

struct KillOnDrop(std::process::Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_token_guards_the_api_but_not_health() {
    let d = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let token_file = d.path().join("cfg/token");
    let child = Command::new(env!("CARGO_BIN_EXE_uppd"))
        .args(["--listen", &addr, "--auth", "--token-file"])
        .arg(&token_file)
        .arg("--db")
        .arg(d.path().join("upp.db"))
        .env_remove("UPPD_TOKEN")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _guard = KillOnDrop(child);
    let base = format!("http://{addr}");
    let t0 = Instant::now();
    while ureq::get(&format!("{base}/health")).call().is_err() {
        assert!(t0.elapsed() < Duration::from_secs(10), "uppd never came up");
        std::thread::sleep(Duration::from_millis(50));
    }
    // The token was created (private to the owner) and is never needed for /health.
    let token = std::fs::read_to_string(&token_file).unwrap();
    assert!(token.len() >= 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&token_file).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "token file must not be readable by others");
    }
    match ureq::get(&format!("{base}/api/tables")).call() {
        Err(ureq::Error::Status(code, _)) => assert_eq!(code, 401),
        other => panic!("expected 401, got {other:?}"),
    }
    match ureq::get(&format!("{base}/api/tables"))
        .set("Authorization", "Bearer wrong")
        .call()
    {
        Err(ureq::Error::Status(code, _)) => assert_eq!(code, 401),
        other => panic!("expected 401, got {other:?}"),
    }
    let ok: Value = ureq::get(&format!("{base}/api/tables"))
        .set("Authorization", &format!("Bearer {}", token.trim()))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert!(ok.is_array() || ok.is_object(), "{ok}");
    // EventSource clients cannot set headers: ?token= works too.
    assert!(
        ureq::get(&format!("{base}/api/pending?token={}", token.trim()))
            .call()
            .is_ok()
    );
}

fn status_of(e: Result<ureq::Response, ureq::Error>) -> (u16, Value) {
    match e {
        Ok(r) => (r.status(), r.into_json().unwrap()),
        Err(ureq::Error::Status(c, r)) => (c, r.into_json().unwrap()),
        Err(e) => panic!("{e}"),
    }
}

fn wait_status(base: &str, id: &str, want: &str) -> Value {
    let t0 = Instant::now();
    loop {
        let (_, v) = status_of(ureq::get(&format!("{base}/api/pingpong/tables/{id}")).call());
        if v["table"]["status"] == want {
            return v["table"].clone();
        }
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "table never reached {want}: {v}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The routes and shapes that OpenUnum's pingpong_create / pingpong_answer /
/// pingpong_tables tools use (scripts/openunum-mcp.mjs, src/server/routes/pingpong.mjs).
#[test]
fn openunum_pingpong_routes_and_shapes() {
    let d = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let player = json!({"kind": "shell", "command": ["sh", fake("fake-coder.sh")]}).to_string();
    let child = Command::new(env!("CARGO_BIN_EXE_uppd"))
        .args(["--listen", &addr, "--db"])
        .arg(d.path().join("upp.db"))
        .args(["--compat-player", &player])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _guard = KillOnDrop(child);
    let base = format!("http://{addr}");
    let t0 = Instant::now();
    while ureq::get(&format!("{base}/health")).call().is_err() {
        assert!(t0.elapsed() < Duration::from_secs(10), "uppd did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    let cwd = d.path().to_string_lossy().to_string();
    let url = format!("{base}/api/pingpong/tables");

    // Validation errors: 400 {ok:false, error:"invalid_payload", details:[{field,issue}]}.
    let (code, v) = status_of(ureq::post(&url).send_json(json!({"referee": {"cmd": "true"}})));
    assert_eq!(code, 400);
    assert_eq!(v["error"], "invalid_payload");
    assert_eq!(v["details"][0]["field"], "goal");

    // Create (external server, chained follow-up): 201 {ok, table:{id,status,serverMode,maxRallies,leaseMs}}.
    let (code, v) = status_of(ureq::post(&url).send_json(json!({
        "goal": "write 42", "expectedEvidence": "answer.txt holds 42",
        "server": {"mode": "external"},
        "players": [{"model": "whatever", "thinking": "low"}],
        "referee": {"cmd": "grep -qx 42 answer.txt", "cwd": cwd},
        "maxRallies": 5, "leaseMs": 600000,
        "next": {"goal": "follow up on {{goal}} with {{reply}}", "server": {"mode": "auto"}}
    })));
    assert_eq!(code, 201, "{v}");
    assert_eq!(v["ok"], true);
    let t = &v["table"];
    let id = t["id"].as_str().unwrap().to_string();
    assert_eq!(t["serverMode"], "external");
    assert_eq!(t["maxRallies"], 5);
    assert_eq!(t["leaseMs"], 600000);
    assert!(["queued", "serving", "refereeing"].contains(&t["status"].as_str().unwrap()));
    assert!(t["chainNextId"].is_string(), "{t}");

    // The first rally fails the referee (the fake coder writes 41): the ball waits for us.
    let table = wait_status(&base, &id, "waiting_server");
    assert_eq!(table["rallyCount"], 1);
    assert_eq!(table["goal"], "write 42");
    assert!(table["endReason"].is_null());
    let r0 = &table["rallies"][0];
    assert_eq!(r0["n"], 1);
    assert_eq!(r0["passed"], false);
    assert_eq!(r0["referee"]["passed"], false);
    assert!(r0["referee"]["output"].as_str().unwrap().contains("[FAIL]"));
    assert_eq!(table["ball"]["status"], "waiting_server");
    assert_eq!(table["waitingForServer"], true);

    // List: {ok, tables:[...]} with the fields pingpong_tables picks, filtered by status.
    let (code, v) = status_of(ureq::get(&format!("{url}?status=waiting_server")).call());
    assert_eq!(code, 200);
    let rows = v["tables"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    for k in [
        "id",
        "goal",
        "status",
        "serverMode",
        "rallyCount",
        "maxRallies",
        "leaseExpiresAt",
        "chainNextId",
    ] {
        assert!(rows[0].get(k).is_some(), "missing {k}: {}", rows[0]);
    }
    let (_, v) = status_of(ureq::get(&format!("{url}?status=done")).call());
    assert_eq!(v["tables"].as_array().unwrap().len(), 0);

    // Errors: unknown table 404; bad verdict 400; answering a table that is not waiting 409.
    let (code, v) = status_of(ureq::get(&format!("{url}/nope")).call());
    assert_eq!((code, v["error"].as_str()), (404, Some("table_not_found")));
    let (code, v) =
        status_of(ureq::post(&format!("{url}/{id}/answer")).send_json(json!({"verdict": "maybe"})));
    assert_eq!((code, v["error"].as_str()), (400, Some("invalid_payload")));

    // Answer "return": {ok, table:{id,status,rallyCount}}; the player continues and scores.
    let (code, v) = status_of(
        ureq::post(&format!("{url}/{id}/answer")).send_json(json!({"note": "write 42 this time"})),
    );
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["table"]["id"], id.as_str());
    assert!(v["table"]["status"].is_string() && v["table"]["rallyCount"].is_number());
    let done = wait_status(&base, &id, "done");
    assert_eq!(done["endReason"], "point");
    assert_eq!(done["rallyCount"], 2);
    assert_eq!(done["score"]["point"], true);
    assert_eq!(done["evidence"]["referee"]["passed"], true);
    assert_eq!(done["rallies"][0]["serverNote"], "write 42 this time");
    let (code, v) = status_of(ureq::post(&format!("{url}/{id}/answer")).send_json(json!({})));
    assert_eq!(
        (code, v["error"].as_str()),
        (409, Some("not_waiting_for_server"))
    );
    assert_eq!(v["status"], "done");

    // The chained table starts after the point (it runs with the auto server and loops until lost
    // or scores: the fake coder's second rally passes the inherited referee).
    let next_id = done["chainNextId"].as_str().unwrap().to_string();
    let next = wait_status(&base, &next_id, "done");
    assert_eq!(next["chainFrom"], id.as_str());
    assert!(next["goal"]
        .as_str()
        .unwrap()
        .starts_with("follow up on write 42"));

    // Stop: an active table can be stopped (200), an ended one cannot (409 table_not_active).
    let (code, v) = status_of(ureq::post(&url).send_json(json!({
        "goal": "never", "server": {"mode": "external"},
        "referee": {"checks": [{"type": "file_exists", "path": "never-there"}]},
        "cwd": cwd
    })));
    assert_eq!(code, 201);
    let sid = v["table"]["id"].as_str().unwrap().to_string();
    let (code, v) =
        status_of(ureq::post(&format!("{url}/{sid}/stop")).send_json(json!({"reason": "test"})));
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["table"]["status"], "stopped");
    wait_status(&base, &sid, "stopped");
    let (code, v) = status_of(ureq::post(&format!("{url}/{sid}/stop")).send_json(json!({})));
    assert_eq!((code, v["error"].as_str()), (409, Some("table_not_active")));
}

struct Compat {
    _d: tempfile::TempDir,
    _guard: KillOnDrop,
    base: String,
}

impl Compat {
    fn url(&self) -> String {
        format!("{}/api/pingpong/tables", self.base)
    }
    fn dir(&self, name: &str) -> String {
        let p = self._d.path().join(name);
        std::fs::create_dir_all(&p).unwrap();
        p.to_string_lossy().into_owned()
    }
}

fn spawn_compat() -> Compat {
    let d = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let player = json!({"kind": "shell", "command": ["sh", fake("fake-coder.sh")]}).to_string();
    let child = Command::new(env!("CARGO_BIN_EXE_uppd"))
        .args(["--listen", &addr, "--db"])
        .arg(d.path().join("upp.db"))
        .args(["--compat-player", &player])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let guard = KillOnDrop(child);
    let t0 = Instant::now();
    while ureq::get(&format!("http://{addr}/health")).call().is_err() {
        assert!(t0.elapsed() < Duration::from_secs(10), "uppd did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    Compat {
        _d: d,
        _guard: guard,
        base: format!("http://{addr}"),
    }
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// A throwaway repo on `main` with a `feat` branch one commit ahead (fast-forwardable).
fn merge_repo(c: &Compat) -> std::path::PathBuf {
    let repo = PathBuf::from(c.dir("repo"));
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README"), "base\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(repo.join("feature.txt"), "feature\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "feature"]);
    git(&repo, &["checkout", "-q", "main"]);
    repo
}

fn wait_merge_log(base: &str, id: &str) -> Value {
    let t0 = Instant::now();
    loop {
        let (_, v) = status_of(ureq::get(&format!("{base}/api/pingpong/tables/{id}")).call());
        if v["table"]["mergeLog"]
            .as_array()
            .is_some_and(|l| !l.is_empty())
        {
            return v["table"]["mergeLog"][0].clone();
        }
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "no merge decision: {v}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn compat_accept_scores_a_failed_referee_but_never_merges() {
    let c = spawn_compat();
    let url = c.url();
    let repo = merge_repo(&c);
    let (code, v) = status_of(ureq::post(&url).send_json(json!({
        "goal": "write 42", "server": {"mode": "external"},
        "referee": {"cmd": "grep -qx 42 answer.txt", "cwd": c.dir("w1")},
        "autoMerge": {"repo": repo, "source": "feat", "test": "true", "push": false}
    })));
    assert_eq!(code, 201, "{v}");
    let id = v["table"]["id"].as_str().unwrap().to_string();
    let t = wait_status(&c.base, &id, "waiting_server");
    assert_eq!(t["rallies"][0]["passed"], false);
    // OpenUnum semantics: accept scores the ball although the referee failed.
    let (code, v) = status_of(
        ureq::post(&format!("{url}/{id}/answer")).send_json(json!({"verdict": "accept"})),
    );
    assert_eq!(code, 200, "{v}");
    let done = wait_status(&c.base, &id, "done");
    assert_eq!(done["endReason"], "point");
    assert_eq!(done["score"]["point"], true);
    assert_eq!(done["evidence"]["acceptedBy"], "server");
    assert_eq!(done["evidence"]["referee"]["passed"], false);
    // ...but a server-accepted point with a failed referee never auto-merges.
    let log = wait_merge_log(&c.base, &id);
    assert_eq!(log["decision"], "skipped_referee", "{log}");
    assert!(log["reason"].as_str().unwrap().contains("referee"), "{log}");
    assert!(!repo.join("feature.txt").exists());
    assert_eq!(git(&repo, &["rev-list", "--count", "main"]), "1");
}

#[test]
fn compat_stop_persists_the_callers_reason() {
    let c = spawn_compat();
    let url = c.url();
    let (_, v) = status_of(ureq::post(&url).send_json(json!({
        "goal": "never", "server": {"mode": "external"},
        "referee": {"cmd": "grep -qx 42 answer.txt", "cwd": c.dir("w2")}
    })));
    let id = v["table"]["id"].as_str().unwrap().to_string();
    wait_status(&c.base, &id, "waiting_server");
    let (code, v) = status_of(
        ureq::post(&format!("{url}/{id}/stop")).send_json(json!({"reason": "owner changed mind"})),
    );
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["table"]["endReason"], "owner changed mind");
    // The reason survives the engine ending the table, in get and in list.
    let t = wait_status(&c.base, &id, "stopped");
    assert_eq!(t["endReason"], "owner changed mind");
    let (_, l) = status_of(ureq::get(&format!("{url}?status=stopped")).call());
    assert_eq!(l["tables"][0]["endReason"], "owner changed mind");
    // No reason: the default stays.
    let (_, v) = status_of(ureq::post(&url).send_json(json!({
        "goal": "never", "server": {"mode": "external"},
        "referee": {"cmd": "grep -qx 42 answer.txt", "cwd": c.dir("w3")}
    })));
    let id2 = v["table"]["id"].as_str().unwrap().to_string();
    wait_status(&c.base, &id2, "waiting_server");
    status_of(ureq::post(&format!("{url}/{id2}/stop")).send_json(json!({})));
    assert_eq!(
        wait_status(&c.base, &id2, "stopped")["endReason"],
        "stopped by the server"
    );
}

#[test]
fn compat_create_passes_auto_merge_through() {
    let c = spawn_compat();
    let url = c.url();
    // spec.mjs-style payload errors.
    let (code, v) = status_of(ureq::post(&url).send_json(json!({
        "goal": "g", "referee": {"cmd": "true"},
        "autoMerge": {"repo": "/x", "test": 5, "push": "yes", "bogus": 1}
    })));
    assert_eq!(code, 400);
    assert_eq!(v["error"], "invalid_payload");
    let fields: Vec<&str> = v["details"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["field"].as_str().unwrap())
        .collect();
    for f in [
        "autoMerge.source",
        "autoMerge.test",
        "autoMerge.push",
        "autoMerge.bogus",
    ] {
        assert!(fields.contains(&f), "{f} missing in {fields:?}");
    }
    let (code, v) = status_of(
        ureq::post(&url)
            .send_json(json!({"goal": "g", "referee": {"cmd": "true"}, "autoMerge": "main"})),
    );
    assert_eq!(
        (code, v["details"][0]["field"].as_str()),
        (400, Some("autoMerge"))
    );

    // create -> referee passes -> merged into the throwaway repo.
    let repo = merge_repo(&c);
    let (code, v) = status_of(ureq::post(&url).send_json(json!({
        "goal": "just pass", "referee": {"cmd": "true", "cwd": c.dir("w4")},
        "autoMerge": {"repo": repo, "source": "feat", "target": "main", "test": "test -f feature.txt", "push": false}
    })));
    assert_eq!(code, 201, "{v}");
    let id = v["table"]["id"].as_str().unwrap().to_string();
    wait_status(&c.base, &id, "done");
    let log = wait_merge_log(&c.base, &id);
    assert_eq!(log["decision"], "merged_ff", "{log}");
    assert!(repo.join("feature.txt").exists());
    assert_eq!(git(&repo, &["rev-list", "--count", "main"]), "2");
}
