// SPDX-License-Identifier: Apache-2.0
//! `openai` player: any OpenAI-compatible `/chat/completions` endpoint (llama.cpp,
//! Halogen, vLLM, Ollama, LM Studio, cloud). The conversation is kept in memory for
//! the table's lifetime, so every rally extends the same prefix (prompt-cache friendly).
//! With `halogen_keep = true` it also sends `X-Halogen-Keep` / `X-Halogen-From` so a
//! Halogen server forks the stored session state instead of re-reading the prefix.

use super::{PlayCtx, Player, Reply};
use crate::config::PlayerSpec;
use serde_json::{json, Value};

pub struct OpenAiPlayer {
    base_url: String,
    model: String,
    api_key: Option<String>,
    temperature: Option<f64>,
    max_tokens: Option<u32>,
    reasoning_effort: Option<String>,
    timeout_secs: Option<u64>,
    halogen_keep: bool,
    extra: Option<Value>,
    messages: Vec<Value>,
    kept: bool,
}

impl OpenAiPlayer {
    pub fn new(spec: PlayerSpec) -> Result<Self, String> {
        let PlayerSpec::Openai {
            base_url,
            model,
            api_key_env,
            system,
            temperature,
            max_tokens,
            reasoning_effort,
            timeout_secs,
            halogen_keep,
            extra,
        } = spec
        else {
            return Err("not an openai player".into());
        };
        let api_key = match api_key_env {
            Some(var) => Some(
                std::env::var(&var)
                    .map_err(|_| format!("api_key_env: environment variable {var} is not set"))?,
            ),
            None => None,
        };
        let mut messages = Vec::new();
        if let Some(s) = system {
            messages.push(json!({"role": "system", "content": s}));
        }
        Ok(OpenAiPlayer {
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            api_key,
            temperature,
            max_tokens,
            reasoning_effort,
            timeout_secs,
            halogen_keep,
            extra,
            messages,
            kept: false,
        })
    }
}

/// Halogen handle names: 1 to 64 of letters, digits and `_.:-`.
fn handle_name(key: &str) -> String {
    let s: String = format!("upp-{key}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.:-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    s.chars().take(64).collect()
}

/// Remove `<think>...</think>` blocks some reasoning models put in `content`.
pub fn strip_think(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("<think>") {
        out.push_str(&rest[..i]);
        match rest[i..].find("</think>") {
            Some(j) => rest = &rest[i + j + "</think>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

impl Player for OpenAiPlayer {
    fn play(&mut self, ball: &str, ctx: &PlayCtx) -> Result<Reply, String> {
        self.messages.push(json!({"role": "user", "content": ball}));
        let mut body = json!({"model": self.model, "messages": self.messages, "stream": false});
        if let Some(t) = self.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(m) = self.max_tokens {
            body["max_tokens"] = json!(m);
        }
        if let Some(e) = ctx.effort.as_ref().or(self.reasoning_effort.as_ref()) {
            body["reasoning_effort"] = json!(e);
        }
        if let Some(Value::Object(x)) = &self.extra {
            for (k, v) in x {
                body[k] = v.clone();
            }
        }
        let agent = ureq::AgentBuilder::new()
            .timeout(ctx.budget(self.timeout_secs, 900))
            .build();
        let mut req = agent
            .post(&format!("{}/chat/completions", self.base_url))
            .set("Content-Type", "application/json");
        if let Some(k) = &self.api_key {
            req = req.set("Authorization", &format!("Bearer {k}"));
        }
        if self.halogen_keep {
            let h = handle_name(&ctx.session_key);
            if self.kept {
                req = req.set("X-Halogen-From", &h);
            }
            req = req.set("X-Halogen-Keep", &h);
        }
        // The blocking call runs on its own thread so a stop can abandon it at once.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let r = req
                .send_json(body)
                .map_err(|e| match e {
                    ureq::Error::Status(code, r) => {
                        let b = r.into_string().unwrap_or_default();
                        format!("HTTP {code}: {}", crate::proc::head(&b, 800))
                    }
                    e => format!("request failed: {e}"),
                })
                .and_then(|r| {
                    r.into_json::<Value>()
                        .map_err(|e| format!("bad JSON from server: {e}"))
                });
            let _ = tx.send(r);
        });
        let res = loop {
            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(r) => break r,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if ctx.cancelled() => {
                    break Err("request cancelled (table stopped)".into())
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => break Err("request thread ended".into()),
            }
        };
        let v: Value = match res {
            Ok(v) => v,
            Err(e) => {
                self.messages.pop();
                return Err(e);
            }
        };
        let msg = &v["choices"][0]["message"];
        let raw = msg["content"].as_str().unwrap_or_default().to_string();
        if raw.trim().is_empty() && msg["tool_calls"].is_null() {
            self.messages.pop();
            return Err(format!(
                "empty answer (finish_reason {})",
                v["choices"][0]["finish_reason"]
            ));
        }
        self.messages
            .push(json!({"role": "assistant", "content": raw.clone()}));
        self.kept = true;
        let u = &v["usage"];
        Ok(Reply {
            text: strip_think(&raw),
            tokens_in: u["prompt_tokens"].as_u64().unwrap_or(0),
            tokens_out: u["completion_tokens"].as_u64().unwrap_or(0),
            cost_usd: 0.0,
            session: Some(ctx.session_key.clone()),
        })
    }

    fn keeps_session(&self) -> bool {
        true
    }

    fn restore(&mut self, history: &[(String, String)], _session: Option<&str>) -> bool {
        for (ball, answer) in history {
            self.messages.push(json!({"role": "user", "content": ball}));
            self.messages
                .push(json!({"role": "assistant", "content": answer}));
        }
        !history.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-shot OpenAI-compatible server that records request bodies.
    fn fake_server(n: usize) -> (String, std::sync::mpsc::Receiver<Value>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", l.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for s in l.incoming().take(n) {
                let mut s = s.unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut len = 0usize;
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
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
                let mut b = vec![0u8; len];
                r.read_exact(&mut b).unwrap();
                tx.send(serde_json::from_slice::<Value>(&b).unwrap())
                    .unwrap();
                let body = r#"{"choices":[{"message":{"content":"<think>x</think>ok"}}],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#;
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        (url, rx)
    }

    #[test]
    fn effort_reaches_the_request_and_history_restores() {
        let (url, rx) = fake_server(2);
        let mut p = OpenAiPlayer::new(PlayerSpec::Openai {
            base_url: url,
            model: "m".into(),
            api_key_env: None,
            system: None,
            temperature: None,
            max_tokens: None,
            reasoning_effort: Some("medium".into()),
            timeout_secs: Some(10),
            halogen_keep: false,
            extra: None,
        })
        .unwrap();
        let far = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut ctx = PlayCtx::new("t", 1, ".".into(), far, "t");
        // The spec default applies when the table gives no effort...
        let r = p.play("one", &ctx).unwrap();
        assert_eq!((r.text.as_str(), r.tokens_in, r.tokens_out), ("ok", 3, 2));
        assert_eq!(rx.recv().unwrap()["reasoning_effort"], "medium");
        // ...and the table's per-rally effort wins.
        ctx.effort = Some("low".into());
        p.play("two", &ctx).unwrap();
        let body = rx.recv().unwrap();
        assert_eq!(body["reasoning_effort"], "low");
        assert_eq!(
            body["messages"].as_array().unwrap().len(),
            3,
            "kept conversation"
        );
        // Crash recovery rebuilds the conversation.
        let mut q = OpenAiPlayer::new(PlayerSpec::Openai {
            base_url: "http://127.0.0.1:9/v1".into(),
            model: "m".into(),
            api_key_env: None,
            system: Some("sys".into()),
            temperature: None,
            max_tokens: None,
            reasoning_effort: None,
            timeout_secs: None,
            halogen_keep: false,
            extra: None,
        })
        .unwrap();
        assert!(q.restore(&[("b1".into(), "a1".into())], None));
        assert_eq!(q.messages.len(), 3);
    }

    #[test]
    fn think_is_stripped_and_handles_are_valid() {
        assert_eq!(strip_think("<think>hmm</think>\nAnswer"), "Answer");
        assert_eq!(strip_think("a<think>x</think>b<think>y"), "ab");
        let h = handle_name("t123/judge 1");
        assert_eq!(h, "upp-t123_judge_1");
        assert!(handle_name(&"x".repeat(100)).len() <= 64);
    }
}
