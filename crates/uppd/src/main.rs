// SPDX-License-Identifier: Apache-2.0
//! `uppd`: the UnumPingPong daemon. Runs queued tables (created over HTTP, by
//! `upp create`, or by `upp mcp --queue-only`) and serves a local HTTP/JSON API
//! with a Server-Sent Events stream of every table event.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response, Server};
use upp_core::store::{default_db_path, Store};
use upp_core::{api, Scheduler};

const HELP: &str = "uppd: UnumPingPong daemon (runs queued tables; HTTP/JSON API + SSE)

USAGE:
  uppd [--listen 127.0.0.1:7878] [--db PATH] [--parallel N]

API:
  GET  /health                        version, store, parallel cap
  GET  /api/tables[?all=1&run=ID]     table summaries (active by default)
  POST /api/tables                    create: match JSON ({defaults, table:[...]}) or a single table
  GET  /api/tables/{id}[?full=1]      one table with rallies and the pending ball
  POST /api/tables/{id}/stop          stop before the next rally
  POST /api/tables/{id}/answer        {verdict: accept|return|stop, note} as the external server
  GET  /api/pending                   balls waiting for an external server
  GET  /api/wait?tables=a,b&timeout=60  block until a ball waits or the tables end
  GET  /api/events[?since=ID]         Server-Sent Events (one `data:` JSON per event)
  GET  /api/export[?run=ID]           every rally as JSON lines

Binds to loopback by default; there is no authentication. Docs: https://github.com/corpunum/unumpingpong";

struct Opts {
    listen: String,
    db: PathBuf,
    parallel: usize,
}

fn parse() -> Result<Opts, String> {
    use lexopt::prelude::*;
    let mut o = Opts {
        listen: "127.0.0.1:7878".into(),
        db: default_db_path(),
        parallel: 4,
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
    let sched = Scheduler::new(&db, o.parallel, "uppd");
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
        std::thread::spawn(move || handle(req, &db, parallel));
    }
    Ok(())
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
                api::create_json(&store, v).map_err(bad)
            }
            (Method::Get, ["api", "tables", id]) => {
                let full = qget("full").is_some_and(|v| v == "1" || v == "true");
                api::status(&store, id, full).map_err(nf)
            }
            (Method::Post, ["api", "tables", id, "stop"]) => api::stop(&store, id).map_err(nf),
            (Method::Post, ["api", "tables", id, "answer"]) => {
                let v: Value =
                    serde_json::from_str(&body).map_err(|e| bad(format!("invalid JSON: {e}")))?;
                api::answer(
                    &store,
                    id,
                    v["verdict"].as_str().unwrap_or(""),
                    v["note"].as_str().unwrap_or(""),
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
