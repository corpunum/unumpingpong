// SPDX-License-Identifier: Apache-2.0
//! `uppd`: the UnumPingPong daemon. Runs queued tables (created over HTTP, by
//! `upp create`, or by `upp mcp --queue-only`) and serves a local HTTP/JSON API
//! with a Server-Sent Events stream of every table event.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response, Server};
use upp_core::store::{default_db_path, Store};
use upp_core::{api, Scheduler};

const HELP: &str = "uppd: UnumPingPong daemon (runs queued tables; HTTP/JSON API + SSE)

USAGE:
  uppd [--listen 127.0.0.1:7878] [--db PATH] [--parallel N] [--global-parallel N]
       [--auth] [--token-file PATH]

  --global-parallel N  at most N tables running at once across every process sharing
                       the store (also UPP_GLOBAL_PARALLEL); one GPU or endpoint, many runs
  --auth               require a bearer token on loopback too (always required otherwise)
  --token-file PATH    the token (default: $XDG_CONFIG_HOME/upp/token, created 0600 if
                       missing); UPPD_TOKEN overrides. Send `Authorization: Bearer <token>`
                       (or `?token=` for an EventSource). /health stays open.

API:
  GET  /health                        version, store, parallel cap
  GET  /api/tables[?all=1&run=ID]     table summaries (active by default)
  POST /api/tables[?parent=ID]        create: match JSON ({defaults, table:[...]}) or a single table;
                                      ?parent opens sub-tables of a running table
  GET  /api/tables/{id}[?full=1]      one table: rallies as result cards, token split, pending ball
  GET  /api/tables/{id}/cards         the table's cards
  GET  /api/tables/{id}/transcript[?rally=N]  full balls and answers (on demand)
  POST /api/tables/{id}/stop          stop: the running player call is killed
  POST /api/tables/{id}/answer        {verdict: accept|return|stop, note, next?, open?} as the server
  GET  /api/pending                   balls waiting for an external server
  GET  /api/cards?tables=a,b&run=R&since=N&timeout=60
                                      block until a card arrives (the orchestrator inbox)
  POST /api/integrate                 {run, into?, check?}: merge verified worktree points
  GET  /api/wait?tables=a,b&timeout=60  block until a ball waits or the tables end
  GET  /api/events[?since=ID]         Server-Sent Events (one `data:` JSON per event)
  GET  /api/export[?run=ID]           every rally as JSON lines

Binds to loopback by default (no token needed there unless --auth); any other address
always requires the token. Docs: https://github.com/corpunum/unumpingpong";

struct Opts {
    listen: String,
    db: PathBuf,
    parallel: usize,
    global_parallel: Option<usize>,
    auth: bool,
    token_file: Option<PathBuf>,
}

fn parse() -> Result<Opts, String> {
    use lexopt::prelude::*;
    let mut o = Opts {
        listen: "127.0.0.1:7878".into(),
        db: default_db_path(),
        parallel: 4,
        global_parallel: None,
        auth: false,
        token_file: None,
    };
    let mut p = lexopt::Parser::from_env();
    while let Some(arg) = p.next().map_err(|e| e.to_string())? {
        match arg {
            Long("listen") => {
                o.listen = p
                    .value()
                    .map_err(|e| e.to_string())?
                    .string()
                    .map_err(|_| "bad --listen")?
            }
            Long("db") => o.db = p.value().map_err(|e| e.to_string())?.into(),
            Long("parallel") | Short('j') => {
                o.parallel = p
                    .value()
                    .map_err(|e| e.to_string())?
                    .parse()
                    .map_err(|e| format!("--parallel: {e}"))?
            }
            Long("global-parallel") => {
                o.global_parallel = Some(
                    p.value()
                        .map_err(|e| e.to_string())?
                        .parse()
                        .map_err(|e| format!("--global-parallel: {e}"))?,
                )
            }
            Long("auth") => o.auth = true,
            Long("token-file") => o.token_file = Some(p.value().map_err(|e| e.to_string())?.into()),
            Short('V') | Long("version") => {
                println!("uppd {}", upp_core::VERSION);
                std::process::exit(0);
            }
            Short('h') | Long("help") => {
                println!("{HELP}");
                std::process::exit(0);
            }
            _ => return Err(arg.unexpected().to_string()),
        }
    }
    Ok(o)
}

fn main() {
    let o = match parse() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("uppd: {e}");
            std::process::exit(2)
        }
    };
    if let Err(e) = serve(o) {
        eprintln!("uppd: {e}");
        std::process::exit(1)
    }
}

fn serve(o: Opts) -> Result<(), String> {
    let db = std::path::absolute(&o.db).unwrap_or(o.db.clone());
    Store::open(&db)?;
    let token = token_for(&o)?.map(Arc::new);
    let mut sched = Scheduler::new(&db, o.parallel, "uppd");
    if o.global_parallel.is_some() {
        sched.global_parallel = o.global_parallel.filter(|n| *n > 0);
    }
    std::thread::spawn(move || {
        if let Err(e) = sched.run() {
            eprintln!("uppd: scheduler stopped: {e}");
            std::process::exit(1);
        }
    });
    let server =
        Server::http(&o.listen).map_err(|e| format!("cannot listen on {}: {e}", o.listen))?;
    eprintln!(
        "uppd {} listening on http://{} (store {}, parallel {})",
        upp_core::VERSION,
        o.listen,
        db.display(),
        o.parallel
    );
    for req in server.incoming_requests() {
        let db = db.clone();
        let parallel = o.parallel;
        let token = token.clone();
        std::thread::spawn(move || {
            if let Some(t) = token {
                if !authorized(&req, &t) {
                    return reply(
                        req,
                        401,
                        &json!({"ok": false, "error": "unauthorized: send Authorization: Bearer <token>"}),
                    );
                }
            }
            handle(req, &db, parallel)
        });
    }
    Ok(())
}

fn loopback(listen: &str) -> bool {
    let host = listen.rsplit_once(':').map(|(h, _)| h).unwrap_or(listen);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn default_token_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("upp").join("token")
}

/// The bearer token, when one is required: on a non-loopback address always, on loopback
/// with --auth / --token-file / UPPD_TOKEN. A missing token file is created (0600) with a
/// random token; its path is printed, never the token.
fn token_for(o: &Opts) -> Result<Option<String>, String> {
    if let Ok(t) = std::env::var("UPPD_TOKEN") {
        if !t.trim().is_empty() {
            return Ok(Some(t.trim().to_string()));
        }
    }
    if !(o.auth || o.token_file.is_some() || !loopback(&o.listen)) {
        return Ok(None);
    }
    let path = o.token_file.clone().unwrap_or_else(default_token_path);
    if let Ok(t) = std::fs::read_to_string(&path) {
        if !t.trim().is_empty() {
            eprintln!("uppd: token required (from {})", path.display());
            return Ok(Some(t.trim().to_string()));
        }
    }
    let t = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    write_private(&path, &t).map_err(|e| format!("{}: {e}", path.display()))?;
    eprintln!("uppd: token required; created {}", path.display());
    Ok(Some(t))
}

#[cfg(unix)]
fn write_private(path: &Path, s: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(s.as_bytes())
}

#[cfg(not(unix))]
fn write_private(path: &Path, s: &str) -> std::io::Result<()> {
    std::fs::write(path, s)
}

fn authorized(req: &Request, token: &str) -> bool {
    let url = req.url();
    if url == "/health" || url.starts_with("/health?") {
        return true;
    }
    let bearer = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .and_then(|h| {
            h.value
                .as_str()
                .strip_prefix("Bearer ")
                .map(str::trim)
                .map(String::from)
        });
    let given = bearer.or_else(|| {
        query(url)
            .into_iter()
            .find(|(k, _)| k == "token")
            .map(|(_, v)| v)
    });
    given.is_some_and(|g| same(g.as_bytes(), token.as_bytes()))
}

/// Constant-time comparison (no early exit on the first differing byte).
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn json_header() -> Header {
    Header::from_bytes("Content-Type", "application/json").expect("static header")
}

fn reply(req: Request, code: u16, v: &Value) {
    let _ = req.respond(
        Response::from_string(v.to_string())
            .with_status_code(code)
            .with_header(json_header()),
    );
}

fn query(url: &str) -> Vec<(String, String)> {
    url.split_once('?')
        .map(|(_, q)| {
            q.split('&')
                .map(|kv| {
                    let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                    (k.to_string(), decode(v))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn handle(mut req: Request, db: &Path, parallel: usize) {
    let url = req.url().to_string();
    let path = url
        .split('?')
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string();
    let q = query(&url);
    let qget = |k: &str| q.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
    let method = req.method().clone();
    let mut body = String::new();
    if method == Method::Post {
        let _ = req.as_reader().take(8 << 20).read_to_string(&mut body);
    }
    if path == "/api/events" && method == Method::Get {
        let since = qget("since").and_then(|s| s.parse().ok());
        return sse(req, db, since);
    }
    let store = match Store::open(db) {
        Ok(s) => s,
        Err(e) => return reply(req, 500, &json!({"ok": false, "error": e})),
    };
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let res: Result<Value, (u16, String)> = (|| {
        let bad = |e: String| (400u16, e);
        let nf = |e: String| (404u16, e);
        match (&method, parts.as_slice()) {
            (Method::Get, ["health"]) | (Method::Get, []) => Ok(json!({
                "ok": true, "name": "uppd", "version": upp_core::VERSION,
                "store": db.display().to_string(), "parallel": parallel,
            })),
            (Method::Get, ["api", "tables"]) => {
                let all = qget("all").is_some_and(|v| v == "1" || v == "true");
                api::list(&store, !all, qget("run").as_deref()).map_err(bad)
            }
            (Method::Post, ["api", "tables"]) => {
                let v: Value =
                    serde_json::from_str(&body).map_err(|e| bad(format!("invalid JSON: {e}")))?;
                match qget("parent") {
                    Some(p) => api::create_child(&store, &p, v).map_err(bad),
                    None => api::create_json(&store, v).map_err(bad),
                }
            }
            (Method::Get, ["api", "tables", id, "cards"]) => {
                let t = store.resolve(id).map_err(nf)?;
                Ok(json!({"cards": store.cards_of(&t.id).map_err(bad)?}))
            }
            (Method::Get, ["api", "tables", id, "transcript"]) => {
                let rally = qget("rally").and_then(|s| s.parse().ok());
                api::transcript(&store, id, rally).map_err(nf)
            }
            (Method::Get, ["api", "cards"]) => {
                let q = api::CardQuery {
                    tables: qget("tables")
                        .map(|s| {
                            s.split(',')
                                .filter(|x| !x.is_empty())
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default(),
                    run: qget("run"),
                    since: qget("since").and_then(|s| s.parse().ok()),
                    timeout: Duration::from_secs(
                        qget("timeout")
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(60u64)
                            .min(600),
                    ),
                };
                api::cards_wait(db, &q).map_err(bad)
            }
            (Method::Post, ["api", "integrate"]) => {
                let v: Value =
                    serde_json::from_str(&body).map_err(|e| bad(format!("invalid JSON: {e}")))?;
                api::integrate(
                    &store,
                    v["run"]
                        .as_str()
                        .ok_or_else(|| bad("run is required".into()))?,
                    v["into"].as_str(),
                    v["check"].as_str(),
                )
                .map_err(bad)
            }
            (Method::Get, ["api", "tables", id]) => {
                let full = qget("full").is_some_and(|v| v == "1" || v == "true");
                api::status(&store, id, full).map_err(nf)
            }
            (Method::Post, ["api", "tables", id, "stop"]) => api::stop(&store, id).map_err(nf),
            (Method::Post, ["api", "tables", id, "answer"]) => {
                let v: Value =
                    serde_json::from_str(&body).map_err(|e| bad(format!("invalid JSON: {e}")))?;
                api::card_answer(
                    &store,
                    id,
                    v["verdict"].as_str().unwrap_or(""),
                    v["note"].as_str().unwrap_or(""),
                    v["next"].as_str(),
                    v.get("open").filter(|o| o.is_object()),
                )
                .map_err(bad)
            }
            (Method::Get, ["api", "pending"]) => {
                Ok(json!({"pending": store.pending(None).map_err(bad)?}))
            }
            (Method::Get, ["api", "wait"]) => {
                let ids: Vec<String> = qget("tables")
                    .map(|s| {
                        s.split(',')
                            .filter(|x| !x.is_empty())
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default();
                let t = qget("timeout")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(60u64)
                    .min(600);
                api::wait(db, &ids, Duration::from_secs(t)).map_err(bad)
            }
            (Method::Get, ["api", "export"]) => Err((
                0,
                api::export_jsonl(&store, qget("run").as_deref()).map_err(bad)?,
            )),
            _ => Err((404, format!("no route {method} {path}"))),
        }
    })();
    match res {
        Ok(v) => reply(req, 200, &v),
        // Code 0 carries a raw JSON-lines body (export).
        Err((0, text)) => {
            let _ = req.respond(Response::from_string(text).with_header(
                Header::from_bytes("Content-Type", "application/x-ndjson").expect("static header"),
            ));
        }
        Err((code, e)) => reply(req, code, &json!({"ok": false, "error": e})),
    }
}

/// Server-Sent Events: raw writer, one flush per batch, until the client disconnects.
fn sse(req: Request, db: &Path, since: Option<i64>) {
    let store = match Store::open(db) {
        Ok(s) => s,
        Err(e) => return reply(req, 500, &json!({"ok": false, "error": e})),
    };
    let mut last = since.unwrap_or_else(|| store.last_event_id().unwrap_or(0));
    let mut w = req.into_writer();
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\
                Connection: close\r\nX-Accel-Buffering: no\r\n\r\n: unumpingpong events\n\n";
    if w.write_all(head.as_bytes())
        .and_then(|_| w.flush())
        .is_err()
    {
        return;
    }
    let mut last_send = Instant::now();
    loop {
        let evs = match store.events_since(last, 200) {
            Ok(v) => v,
            Err(_) => return,
        };
        let mut buf = String::new();
        for e in &evs {
            last = e.id;
            let data = serde_json::to_string(e).unwrap_or_default();
            buf.push_str(&format!(
                "id: {}\nevent: {}\ndata: {}\n\n",
                e.id, e.kind, data
            ));
        }
        if buf.is_empty() && last_send.elapsed() > Duration::from_secs(15) {
            buf.push_str(": keep-alive\n\n");
        }
        if !buf.is_empty() {
            if w.write_all(buf.as_bytes()).and_then(|_| w.flush()).is_err() {
                return; // client gone
            }
            last_send = Instant::now();
        }
        if evs.is_empty() {
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_decoding() {
        let q = query("/api/wait?tables=a%2Cb,c&timeout=5&x");
        assert_eq!(q[0], ("tables".into(), "a,b,c".into()));
        assert_eq!(q[1], ("timeout".into(), "5".into()));
        assert_eq!(decode("a+b%20c%"), "a b c%");
    }
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    #[test]
    fn only_loopback_addresses_may_run_without_a_token() {
        for a in ["127.0.0.1:7878", "localhost:1", "[::1]:7878", "127.0.0.2:9"] {
            assert!(loopback(a), "{a}");
        }
        for a in [
            "0.0.0.0:7878",
            "192.168.1.5:7878",
            "[::]:7878",
            "example.com:80",
        ] {
            assert!(!loopback(a), "{a}");
        }
        assert!(same(b"abc", b"abc") && !same(b"abc", b"abd") && !same(b"abc", b"ab"));
    }
}
