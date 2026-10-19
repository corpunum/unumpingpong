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
        let resp = match req.send_json(body) {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                self.messages.pop();
                let b = r.into_string().unwrap_or_default();
                return Err(format!("HTTP {code}: {}", crate::proc::head(&b, 800)));
            }
            Err(e) => {
                self.messages.pop();
                return Err(format!("request failed: {e}"));
            }
        };
        let v: Value = resp
            .into_json()
            .map_err(|e| format!("bad JSON from server: {e}"))?;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn think_is_stripped_and_handles_are_valid() {
        assert_eq!(strip_think("<think>hmm</think>\nAnswer"), "Answer");
        assert_eq!(strip_think("a<think>x</think>b<think>y"), "ab");
        let h = handle_name("t123/judge 1");
        assert_eq!(h, "upp-t123_judge_1");
        assert!(handle_name(&"x".repeat(100)).len() <= 64);
    }
}
