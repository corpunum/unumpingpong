// SPDX-License-Identifier: Apache-2.0
//! `openunum` player: an OpenUnum agent over its HTTP API. One OpenUnum chat
//! session per table (`sessionId`), so the agent keeps its context across rallies.
//!
//! Every rally is one OpenUnum *turn* with a `turnId` chosen here, and only that
//! turn's reply is accepted:
//!
//! 1. If the session still runs a turn (an earlier rally that timed out), wait for it
//!    to end first and discard its result: it is not this rally's answer.
//! 2. `POST /api/chat {sessionId, message, turnId}`. 200 carries the finished turn;
//!    202 means it still runs. A 202 whose `turnId` is not ours means the message was
//!    queued into a turn that was already running: that is refused (a fault), because
//!    its reply would belong to the previous turn.
//! 3. Poll `GET /api/chat/pending?sessionId=...` until a completed payload with our
//!    `turnId` arrives. Completed payloads of other turns are ignored.
//!
//! A terminal payload with `ok: false`, `cancelled`, `timedOut`/`timeout`, or an empty
//! reply is a fault, never an answer.

use super::{PlayCtx, Player, Reply};
use crate::config::PlayerSpec;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub struct OpenUnumPlayer {
    base_url: String,
    password: Option<String>,
    timeout_secs: Option<u64>,
    prefix: String,
    /// Delay between `/api/chat/pending` polls.
    pub poll: Duration,
}

impl OpenUnumPlayer {
    pub fn new(spec: PlayerSpec) -> Self {
        let PlayerSpec::Openunum {
            base_url,
            password_env,
            timeout_secs,
            session_prefix,
        } = spec
        else {
            unreachable!("openunum player built from another spec")
        };
        OpenUnumPlayer {
            base_url: base_url.trim_end_matches('/').to_string(),
            password: password_env.and_then(|v| std::env::var(v).ok()),
            timeout_secs,
            prefix: session_prefix.unwrap_or_else(|| "upp-".into()),
            poll: Duration::from_millis(1500),
        }
    }
}

/// Accept a terminal turn payload only when it is a successful reply.
pub fn terminal_reply(v: &Value) -> Result<String, String> {
    let turn = v["turnId"].as_str().unwrap_or("?");
    if v["cancelled"].as_bool() == Some(true) {
        return Err(format!("OpenUnum turn {turn} was cancelled"));
    }
    if v["timedOut"].as_bool() == Some(true) || v["timeout"].as_bool() == Some(true) {
        return Err(format!("OpenUnum turn {turn} timed out on the server"));
    }
    if v["ok"].as_bool() == Some(false) {
        let err = v["error"].as_str().unwrap_or("ok: false");
        return Err(format!("OpenUnum turn {turn} failed: {err}"));
    }
    match v["reply"].as_str().filter(|s| !s.trim().is_empty()) {
        Some(r) => Ok(r.to_string()),
        None => Err(format!("OpenUnum turn {turn} ended without a reply")),
    }
}

impl Player for OpenUnumPlayer {
    fn play(&mut self, ball: &str, ctx: &PlayCtx) -> Result<Reply, String> {
        let session = format!("{}{}", self.prefix, ctx.session_key);
        let budget = ctx.budget(self.timeout_secs, 1800);
        let until = Instant::now() + budget;
        let agent = ureq::AgentBuilder::new()
            .timeout(budget.min(Duration::from_secs(120)))
            .build();
        let with_pw = |r: ureq::Request| match &self.password {
            Some(p) => r.set("X-OpenUnum-Password", p),
            None => r,
        };
        let poll_url = format!(
            "{}/api/chat/pending?sessionId={}",
            self.base_url,
            url_encode(&session)
        );
        let get_pending = || -> Option<Value> {
            with_pw(agent.get(&poll_url))
                .call()
                .ok()
                .and_then(|r| r.into_json().ok())
        };

        // 1. A turn still running in this session is not ours: let it end, drop its result.
        while let Some(v) = get_pending() {
            if v["pending"].as_bool() != Some(true) {
                break;
            }
            if ctx.cancelled() {
                return Err("cancelled (table stopped)".into());
            }
            if Instant::now() >= until {
                return Err(format!(
                    "OpenUnum session {session} is still busy with turn {}",
                    v["turnId"].as_str().unwrap_or("?")
                ));
            }
            std::thread::sleep(self.poll);
        }

        // 2. Start our own turn.
        let turn_id = uuid::Uuid::new_v4().to_string();
        let resp = with_pw(agent.post(&format!("{}/api/chat", self.base_url)))
            .send_json(json!({"sessionId": session, "message": ball, "turnId": turn_id}));
        let (code, v): (u16, Value) = match resp {
            Ok(r) => (r.status(), r.into_json().unwrap_or(Value::Null)),
            Err(ureq::Error::Status(c, r)) => {
                return Err(format!(
                    "OpenUnum HTTP {c}: {}",
                    crate::proc::head(&r.into_string().unwrap_or_default(), 600)
                ))
            }
            Err(e) => return Err(format!("OpenUnum request failed: {e}")),
        };
        let mk = |text: String| Reply {
            text,
            session: Some(session.clone()),
            ..Default::default()
        };
        let theirs = v["turnId"].as_str().unwrap_or("");
        if theirs != turn_id {
            return Err(format!(
                "OpenUnum answered for turn '{theirs}', not this rally's turn {turn_id} \
                 (HTTP {code}{}): the session was busy, so the reply would belong to another turn",
                if v["queued"].as_bool() == Some(true) {
                    ", message queued into the running turn"
                } else {
                    ""
                }
            ));
        }
        if code == 200 {
            return terminal_reply(&v).map(mk);
        }

        // 3. 202: poll until our turn is terminal.
        let mut idle_polls = 0;
        while Instant::now() < until {
            std::thread::sleep(self.poll);
            if ctx.cancelled() {
                // Ask OpenUnum to stop the turn too (best effort).
                let _ = with_pw(agent.post(&format!("{}/api/chat/cancel", self.base_url)))
                    .send_json(json!({"sessionId": session}));
                return Err(format!("turn {turn_id} cancelled (table stopped)"));
            }
            let Some(v) = get_pending() else { continue };
            if v["pending"].as_bool() == Some(true) {
                idle_polls = 0;
                continue;
            }
            if v["completed"].as_bool() == Some(true) || v.get("reply").is_some() {
                if v["turnId"].as_str() == Some(turn_id.as_str()) {
                    return terminal_reply(&v).map(mk);
                }
                continue; // another turn's result: never ours
            }
            idle_polls += 1;
            if idle_polls > 5 {
                return Err(format!(
                    "OpenUnum turn {turn_id} ended without a result for this rally"
                ));
            }
        }
        Err(format!(
            "OpenUnum turn {turn_id} still running after {:.0}s",
            budget.as_secs_f64()
        ))
    }

    fn keeps_session(&self) -> bool {
        true
    }
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    /// `post(our_turn_id) -> (code, body)`; `pending(poll_index, our_turn_id) -> body`.
    type PostFn = Box<dyn Fn(&str) -> (u16, Value) + Send>;
    type PendFn = Box<dyn Fn(usize, &str) -> Value + Send>;

    struct Fake {
        url: String,
        posts: Arc<Mutex<u32>>,
    }

    /// A scripted OpenUnum on a loopback port.
    fn fake(post: PostFn, pending: PendFn) -> Fake {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let posts = Arc::new(Mutex::new(0u32));
        let p2 = posts.clone();
        std::thread::spawn(move || {
            let mut polls = 0usize;
            let mut ours = String::new();
            for s in l.incoming() {
                let Ok(mut s) = s else { break };
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut first = String::new();
                if r.read_line(&mut first).is_err() {
                    continue;
                }
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0u8; len];
                r.read_exact(&mut body).unwrap();
                let (code, v) = if first.starts_with("POST") {
                    *p2.lock().unwrap() += 1;
                    let req: Value = serde_json::from_slice(&body).unwrap();
                    ours = req["turnId"].as_str().unwrap_or("").to_string();
                    post(&ours)
                } else {
                    polls += 1;
                    (200, pending(polls - 1, &ours))
                };
                let b = v.to_string();
                let _ = write!(
                    s,
                    "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
                    b.len()
                );
            }
        });
        Fake { url, posts }
    }

    fn player(url: &str) -> OpenUnumPlayer {
        let mut p = OpenUnumPlayer::new(PlayerSpec::Openunum {
            base_url: url.into(),
            password_env: None,
            timeout_secs: Some(5),
            session_prefix: None,
        });
        p.poll = Duration::from_millis(20);
        p
    }

    fn ctx() -> PlayCtx {
        PlayCtx::new(
            "t",
            1,
            PathBuf::from("."),
            Instant::now() + Duration::from_secs(5),
            "t",
        )
    }

    fn idle() -> Value {
        json!({"ok": true, "pending": false})
    }

    #[test]
    fn a_message_queued_into_a_running_turn_is_refused() {
        // POST answers 202 for the turn that was already running ("old"): the message
        // was queued into it. Its reply must never become this rally's answer.
        let f = fake(
            Box::new(|_| {
                (
                    202,
                    json!({"ok": true, "pending": true, "queued": true, "turnId": "old"}),
                )
            }),
            Box::new(|i, _| {
                if i == 0 {
                    idle()
                } else {
                    json!({"ok": true, "completed": true, "turnId": "old", "reply": "OLD ANSWER"})
                }
            }),
        );
        let e = player(&f.url).play("ball", &ctx()).unwrap_err();
        assert!(e.contains("not this rally's turn"), "{e}");
        assert!(e.contains("queued"), "{e}");
    }

    #[test]
    fn waits_out_a_busy_session_and_takes_only_its_own_turn() {
        // Polls: 0-1 an earlier turn still runs (pre-wait), 2 idle -> POST (202, ours);
        // then: ours pending, an older turn's late result, then ours.
        let f = fake(
            Box::new(|t| (202, json!({"ok": true, "pending": true, "turnId": t}))),
            Box::new(|i, ours| match i {
                0 | 1 => json!({"ok": true, "pending": true, "turnId": "earlier"}),
                2 => idle(),
                3 => json!({"ok": true, "pending": true, "turnId": ours}),
                4 => {
                    json!({"ok": true, "pending": false, "completed": true, "turnId": "earlier", "reply": "OLD"})
                }
                _ => {
                    json!({"ok": true, "pending": false, "completed": true, "turnId": ours, "reply": "NEW"})
                }
            }),
        );
        let r = player(&f.url).play("b", &ctx()).unwrap();
        assert_eq!(r.text, "NEW");
        assert_eq!(
            *f.posts.lock().unwrap(),
            1,
            "posted only after the busy turn ended"
        );
    }

    #[test]
    fn failed_cancelled_and_timed_out_turns_are_faults() {
        for v in [
            json!({"ok": false, "error": "chat_turn_failed", "reply": "Something failed", "turnId": "a"}),
            json!({"ok": false, "cancelled": true, "reply": "Request was cancelled.", "turnId": "a"}),
            json!({"ok": false, "timedOut": true, "timeout": true, "reply": "Request is taking too long.", "turnId": "a"}),
            json!({"ok": true, "reply": "  ", "turnId": "a"}),
        ] {
            assert!(terminal_reply(&v).is_err(), "{v}");
        }
        // A 200 that carries a cancelled terminal is refused.
        let f = fake(
            Box::new(|t| {
                (
                    200,
                    json!({"ok": false, "cancelled": true, "reply": "Request was cancelled.", "turnId": t}),
                )
            }),
            Box::new(|_, _| idle()),
        );
        let e = player(&f.url).play("b", &ctx()).unwrap_err();
        assert!(e.contains("cancelled"), "{e}");
        // A 202 whose polled result timed out on the server is refused.
        let f = fake(
            Box::new(|t| (202, json!({"ok": true, "pending": true, "turnId": t}))),
            Box::new(|i, ours| {
                if i == 0 {
                    idle()
                } else {
                    json!({"ok": false, "pending": false, "completed": true, "timedOut": true,
                           "turnId": ours, "reply": "Request is taking too long."})
                }
            }),
        );
        let e = player(&f.url).play("b", &ctx()).unwrap_err();
        assert!(e.contains("timed out"), "{e}");
    }

    #[test]
    fn a_finished_200_for_our_turn_is_the_answer() {
        let f = fake(
            Box::new(|t| (200, json!({"ok": true, "reply": "ANSWER", "turnId": t}))),
            Box::new(|_, _| idle()),
        );
        assert_eq!(player(&f.url).play("b", &ctx()).unwrap().text, "ANSWER");
    }
}
