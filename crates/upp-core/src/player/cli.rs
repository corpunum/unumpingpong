// SPDX-License-Identifier: Apache-2.0
//! Agent CLI players: Claude Code, Codex, Gemini CLI, opencode. Each rally is one
//! headless invocation in the table's `cwd`; the tool's session id from the first
//! rally is passed back on the next one so the agent keeps its context.
//!
//! Flags were taken from each tool's `--help` (Claude Code 2.1.303, codex-cli 0.160.0,
//! gemini 0.45.1, opencode 1.18.33); output shapes from the tools' JSON formatters.
//! See docs/integrations/ for what is verified live.

use super::{PlayCtx, Player, Reply};
use crate::config::AgentCli;
use crate::proc;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Claude,
    Codex,
    Gemini,
    Opencode,
}

impl Flavor {
    fn bin(self) -> &'static str {
        match self {
            Flavor::Claude => "claude",
            Flavor::Codex => "codex",
            Flavor::Gemini => "gemini",
            Flavor::Opencode => "opencode",
        }
    }
    /// Every supported tool can resume a session by id (Gemini CLI: per project directory,
    /// which is the table's cwd).
    fn resume_default(self) -> bool {
        true
    }
}

pub struct AgentPlayer {
    flavor: Flavor,
    spec: AgentCli,
    session: Option<String>,
}

impl AgentPlayer {
    pub fn new(flavor: Flavor, spec: AgentCli) -> Self {
        AgentPlayer {
            flavor,
            spec,
            session: None,
        }
    }

    fn resume(&self) -> bool {
        self.spec.resume.unwrap_or(self.flavor.resume_default())
    }

    /// Build argv and stdin for one call. `out_file` is used by codex `-o`.
    fn command(&self, ball: &str, out_file: &str) -> (Vec<String>, Option<String>) {
        let bin = self
            .spec
            .bin
            .clone()
            .unwrap_or_else(|| self.flavor.bin().to_string());
        let mut a: Vec<String> = vec![bin];
        let s = |x: &str| x.to_string();
        let session = if self.resume() {
            self.session.clone()
        } else {
            None
        };
        match self.flavor {
            Flavor::Claude => {
                // claude -p --output-format json [--model M] [--resume ID] ARGS  (prompt on stdin)
                a.extend([s("-p"), s("--output-format"), s("json")]);
                if let Some(m) = &self.spec.model {
                    a.extend([s("--model"), m.clone()]);
                }
                if let Some(id) = &session {
                    a.extend([s("--resume"), id.clone()]);
                }
                a.extend(self.spec.args.iter().cloned());
                (a, Some(ball.to_string()))
            }
            Flavor::Codex => {
                // codex exec [resume ID] --json --skip-git-repo-check -o FILE [-m M] ARGS -  (stdin)
                a.push(s("exec"));
                if session.is_some() {
                    a.push(s("resume"));
                }
                a.extend([
                    s("--json"),
                    s("--skip-git-repo-check"),
                    s("-o"),
                    s(out_file),
                ]);
                if let Some(m) = &self.spec.model {
                    a.extend([s("-m"), m.clone()]);
                }
                a.extend(self.spec.args.iter().cloned());
                if let Some(id) = &session {
                    a.push(id.clone());
                }
                a.push(s("-"));
                (a, Some(ball.to_string()))
            }
            Flavor::Gemini => {
                // gemini -o json [-m M] [--session-id U | --resume U] ARGS -p BALL
                a.extend([s("-o"), s("json")]);
                if let Some(m) = &self.spec.model {
                    a.extend([s("-m"), m.clone()]);
                }
                if self.resume() {
                    if let Some(id) = &self.session {
                        a.extend([s("--resume"), id.clone()]);
                    }
                }
                a.extend(self.spec.args.iter().cloned());
                a.extend([s("-p"), ball.to_string()]);
                (a, None)
            }
            Flavor::Opencode => {
                // opencode run --format json [-m provider/model] [-s ID] ARGS MESSAGE
                a.extend([s("run"), s("--format"), s("json")]);
                if let Some(m) = &self.spec.model {
                    a.extend([s("-m"), m.clone()]);
                }
                if let Some(id) = &session {
                    a.extend([s("-s"), id.clone()]);
                }
                a.extend(self.spec.args.iter().cloned());
                // A message starting with '-' would be read as a flag.
                let msg = if ball.starts_with('-') {
                    format!("Task: {ball}")
                } else {
                    ball.to_string()
                };
                a.push(msg);
                (a, None)
            }
        }
    }
}

impl Player for AgentPlayer {
    fn play(&mut self, ball: &str, ctx: &PlayCtx) -> Result<Reply, String> {
        let out_file = std::env::temp_dir().join(format!(
            "unumpingpong-{}-{}-{}.last.txt",
            ctx.table_id,
            ctx.rally,
            std::process::id()
        ));
        let out_path = out_file.to_string_lossy().into_owned();
        // Gemini: pick our own session UUID on the first rally so it can be resumed.
        let mut gemini_new: Option<String> = None;
        let (mut argv, stdin) = self.command(ball, &out_path);
        if self.flavor == Flavor::Gemini && self.resume() && self.session.is_none() {
            let id = uuid::Uuid::new_v4().to_string();
            let pos = argv.len() - 2; // before -p BALL
            argv.splice(pos..pos, ["--session-id".to_string(), id.clone()]);
            gemini_new = Some(id);
        }
        let out = proc::run(
            &argv,
            &ctx.cwd,
            stdin.as_deref(),
            &self.spec.env,
            ctx.budget(self.spec.timeout_secs, 900),
        )?;
        let last = std::fs::read_to_string(&out_file).ok();
        let _ = std::fs::remove_file(&out_file);
        if out.timed_out {
            return Err(format!(
                "{} timed out after {:.0}s",
                self.flavor.bin(),
                out.duration_ms as f64 / 1000.0
            ));
        }
        let parsed = match self.flavor {
            Flavor::Claude => parse_claude(&out.stdout),
            Flavor::Codex => parse_codex(&out.stdout, last.as_deref()),
            Flavor::Gemini => parse_gemini(&out.stdout),
            Flavor::Opencode => parse_opencode(&out.stdout),
        };
        let mut reply = match parsed {
            Ok(r) => r,
            Err(e) => {
                return Err(format!(
                    "{} (exit {:?}): {e}\n{}",
                    self.flavor.bin(),
                    out.code,
                    proc::tail(out.stderr.trim(), 1500)
                ))
            }
        };
        if reply.session.is_none() {
            reply.session = gemini_new;
        }
        if reply.session.is_some() {
            self.session = reply.session.clone();
        }
        Ok(reply)
    }

    fn keeps_session(&self) -> bool {
        self.resume()
    }
}

fn u(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// `claude -p --output-format json`: one result object (or an array ending in one).
pub fn parse_claude(stdout: &str) -> Result<Reply, String> {
    let v: Value = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("not JSON output ({e}): {}", proc::head(stdout.trim(), 300)))?;
    let r = match &v {
        Value::Array(a) => a
            .iter()
            .rev()
            .find(|x| x["type"] == "result")
            .cloned()
            .unwrap_or(Value::Null),
        _ => v,
    };
    if r["is_error"].as_bool() == Some(true) {
        return Err(format!(
            "claude reported an error: {}",
            r["result"].as_str().unwrap_or(&r["subtype"].to_string())
        ));
    }
    let text = r["result"]
        .as_str()
        .ok_or("no `result` field in claude output")?
        .to_string();
    let us = &r["usage"];
    Ok(Reply {
        text,
        tokens_in: u(&us["input_tokens"])
            + u(&us["cache_creation_input_tokens"])
            + u(&us["cache_read_input_tokens"]),
        tokens_out: u(&us["output_tokens"]),
        cost_usd: r["total_cost_usd"].as_f64().unwrap_or(0.0),
        session: r["session_id"].as_str().map(String::from),
    })
}

/// `codex exec --json`: JSONL events. Thread id from `thread.started`, usage from
/// `turn.completed`, the answer from `-o` (fallback: the last `agent_message` item).
pub fn parse_codex(stdout: &str, last_message: Option<&str>) -> Result<Reply, String> {
    let mut r = Reply::default();
    let mut last_text: Option<String> = None;
    let mut error: Option<String> = None;
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let ty = v["type"].as_str().unwrap_or("");
        if let Some(id) = v["thread_id"].as_str().or(v["session_id"].as_str()) {
            r.session.get_or_insert_with(|| id.to_string());
        }
        match ty {
            "turn.completed" => {
                r.tokens_in += u(&v["usage"]["input_tokens"]);
                r.tokens_out += u(&v["usage"]["output_tokens"]);
            }
            "item.completed" if v["item"]["type"] == "agent_message" => {
                last_text = v["item"]["text"].as_str().map(String::from);
            }
            "turn.failed" | "error" => {
                error = Some(
                    v["error"]["message"]
                        .as_str()
                        .or(v["message"].as_str())
                        .unwrap_or("codex error")
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    let text = last_message
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .or(last_text);
    match text {
        Some(t) => {
            r.text = t;
            Ok(r)
        }
        None => Err(error.unwrap_or_else(|| "no agent message in codex output".into())),
    }
}

/// `gemini -o json`: `{"response": ..., "stats": {...}}`; falls back to plain text.
pub fn parse_gemini(stdout: &str) -> Result<Reply, String> {
    let t = stdout.trim();
    let Some(v) = crate::referee::extract_json(t).filter(|v| v.is_object()) else {
        if t.is_empty() {
            return Err("empty output".into());
        }
        return Ok(Reply {
            text: t.to_string(),
            ..Default::default()
        });
    };
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        return Err(format!(
            "gemini error: {}",
            err["message"].as_str().unwrap_or(&err.to_string())
        ));
    }
    let text = v["response"]
        .as_str()
        .ok_or("no `response` field in gemini output")?
        .to_string();
    let mut r = Reply {
        text,
        session: v["session_id"]
            .as_str()
            .or(v["sessionId"].as_str())
            .map(String::from),
        ..Default::default()
    };
    if let Some(models) = v["stats"]["models"].as_object() {
        for m in models.values() {
            r.tokens_in += u(&m["tokens"]["prompt"]);
            r.tokens_out += u(&m["tokens"]["candidates"]);
        }
    }
    Ok(r)
}

/// `opencode run --format json`: JSON events. Text parts are concatenated; the session
/// id is the first `sessionID` seen; tokens/cost from `step_finish` parts.
pub fn parse_opencode(stdout: &str) -> Result<Reply, String> {
    let mut r = Reply::default();
    let mut texts: Vec<String> = Vec::new();
    let mut error: Option<String> = None;
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if r.session.is_none() {
            r.session = find_key(&v, "sessionID").map(String::from);
        }
        let part = &v["part"];
        match v["type"].as_str().unwrap_or("") {
            "text" => {
                if let Some(t) = part["text"].as_str() {
                    texts.push(t.to_string());
                }
            }
            "step_finish" => {
                r.tokens_in += u(&part["tokens"]["input"]);
                r.tokens_out += u(&part["tokens"]["output"]);
                r.cost_usd += part["cost"].as_f64().unwrap_or(0.0);
            }
            "error" => {
                error = Some(
                    find_key(&v, "message")
                        .unwrap_or("opencode error")
                        .to_string(),
                )
            }
            _ => {}
        }
    }
    if texts.is_empty() {
        return Err(error.unwrap_or_else(|| "no text in opencode output".into()));
    }
    r.text = texts.join("\n");
    Ok(r)
}

fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    match v {
        Value::Object(m) => {
            if let Some(s) = m.get(key).and_then(|x| x.as_str()) {
                return Some(s);
            }
            m.values().find_map(|x| find_key(x, key))
        }
        Value::Array(a) => a.iter().find_map(|x| find_key(x, key)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_json() {
        let out = r#"{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"abc","total_cost_usd":0.01,"usage":{"input_tokens":10,"cache_read_input_tokens":5,"output_tokens":3}}"#;
        let r = parse_claude(out).unwrap();
        assert_eq!(r.text, "done");
        assert_eq!(r.session.as_deref(), Some("abc"));
        assert_eq!((r.tokens_in, r.tokens_out), (15, 3));
        assert!(parse_claude(r#"{"type":"result","is_error":true,"result":"boom"}"#).is_err());
    }

    #[test]
    fn codex_jsonl() {
        let out = "{\"type\":\"thread.started\",\"thread_id\":\"th-1\"}\n\
                   {\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"hi\"}}\n\
                   {\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":7,\"output_tokens\":2}}\n";
        let r = parse_codex(out, None).unwrap();
        assert_eq!(r.text, "hi");
        assert_eq!(r.session.as_deref(), Some("th-1"));
        assert_eq!(parse_codex(out, Some("from -o")).unwrap().text, "from -o");
    }

    #[test]
    fn gemini_and_opencode() {
        let g = parse_gemini(
            r#"{"response":"ok","stats":{"models":{"m":{"tokens":{"prompt":4,"candidates":1}}}}}"#,
        )
        .unwrap();
        assert_eq!((g.text.as_str(), g.tokens_in, g.tokens_out), ("ok", 4, 1));
        assert_eq!(parse_gemini("plain text").unwrap().text, "plain text");
        let o = "{\"type\":\"step_start\",\"sessionID\":\"ses_1\"}\n\
                 {\"type\":\"text\",\"sessionID\":\"ses_1\",\"part\":{\"text\":\"answer\"}}\n\
                 {\"type\":\"step_finish\",\"part\":{\"tokens\":{\"input\":9,\"output\":4},\"cost\":0}}\n";
        let r = parse_opencode(o).unwrap();
        assert_eq!(r.text, "answer");
        assert_eq!(r.session.as_deref(), Some("ses_1"));
        assert_eq!(r.tokens_in, 9);
    }

    #[test]
    fn argv_shapes() {
        let mut p = AgentPlayer::new(
            Flavor::Claude,
            AgentCli {
                model: Some("sonnet".into()),
                args: vec!["--permission-mode".into(), "acceptEdits".into()],
                ..Default::default()
            },
        );
        let (a, stdin) = p.command("ball", "/tmp/x");
        assert_eq!(a[..4], ["claude", "-p", "--output-format", "json"]);
        assert!(stdin.is_some());
        p.session = Some("S".into());
        let (a, _) = p.command("ball", "/tmp/x");
        assert!(a.windows(2).any(|w| w == ["--resume", "S"]));

        let mut c = AgentPlayer::new(Flavor::Codex, AgentCli::default());
        c.session = Some("T".into());
        let (a, _) = c.command("b", "/tmp/o");
        assert_eq!(a[1..3], ["exec", "resume"]);
        assert_eq!(a[a.len() - 2..], ["T", "-"]);

        let o = AgentPlayer::new(Flavor::Opencode, AgentCli::default());
        let (a, _) = o.command("-x", "/tmp/o");
        assert_eq!(a.last().unwrap(), "Task: -x");

        let g = AgentPlayer::new(Flavor::Gemini, AgentCli::default());
        let (a, stdin) = g.command("b", "/tmp/o");
        assert_eq!(a[a.len() - 2..], ["-p", "b"]);
        assert!(stdin.is_none());
        assert!(g.keeps_session());
    }
}
