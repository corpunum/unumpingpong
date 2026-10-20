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
