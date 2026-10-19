// SPDX-License-Identifier: Apache-2.0
//! `openunum` player: an OpenUnum agent over its HTTP API. One OpenUnum chat
//! session per table (`sessionId`), so the agent keeps its context across rallies.
//! `POST /api/chat` answers 200 with the reply, or 202 while the turn runs; then
//! `GET /api/chat/pending?sessionId=...` is polled until the turn completes.

use super::{PlayCtx, Player, Reply};
use crate::config::PlayerSpec;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub struct OpenUnumPlayer {
    base_url: String,
    password: Option<String>,
    timeout_secs: Option<u64>,
    prefix: String,
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
        }
    }

    fn reply_of(v: &Value) -> Option<String> {
        v.get("reply")
            .and_then(|r| r.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(String::from)
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
        let resp = with_pw(agent.post(&format!("{}/api/chat", self.base_url)))
            .send_json(json!({"sessionId": session, "message": ball}));
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
        if code == 200 {
            if let Some(t) = Self::reply_of(&v) {
                return Ok(mk(t));
            }
        }
        // 202 (or an empty 200): the turn runs in the background; poll for it.
        let poll_url = format!(
            "{}/api/chat/pending?sessionId={}",
            self.base_url,
            url_encode(&session)
        );
        let mut idle_polls = 0;
        while Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1500));
            let v: Value = match with_pw(agent.get(&poll_url)).call() {
                Ok(r) => r.into_json().unwrap_or(Value::Null),
                Err(_) => continue,
            };
            if v["pending"].as_bool() == Some(true) {
                idle_polls = 0;
                continue;
            }
            if let Some(t) = Self::reply_of(&v) {
                return Ok(mk(t));
            }
            idle_polls += 1;
            if idle_polls > 5 {
                return Err("OpenUnum turn ended without a reply".into());
            }
        }
        Err(format!(
            "OpenUnum turn still running after {:.0}s",
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
