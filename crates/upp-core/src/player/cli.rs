// SPDX-License-Identifier: Apache-2.0
//! Agent CLI players: Claude Code, Codex, Gemini CLI, opencode.
//!
//! Claude Code and Codex run as one long-lived process per table by default (`persistent`):
//! `claude -p --input-format stream-json --output-format stream-json` takes each rally as a
//! new user message on stdin, and `codex app-server` (JSON-RPC over stdio) takes it as a new
//! `turn/start` on the same thread. Otherwise (and for Gemini CLI and opencode) each rally is
//! one headless invocation in the table's `cwd`, and the tool's session id from the first
//! rally is passed back on the next one so the agent keeps its context.
//!
//! `lean` (default on for Claude Code and Codex) drops context a coding player does not
//! need: MCP servers, user settings, skills and plugins, memory, extra feature tools.
//!
//! Flags were taken from each tool's `--help` (Claude Code 2.1.303, codex-cli 0.160.0,
//! gemini 0.45.1, opencode 1.18.33), the app-server protocol from
//! `codex app-server generate-json-schema`; output shapes from the tools' JSON formatters.
//! See docs/integrations/ for what is verified live.

use super::{PlayCtx, Player, Reply};
use crate::config::AgentCli;
use crate::proc::{self, LiveProc, Recv};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

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
    /// Tools with a multi-turn stdio mode and lean flags.
    fn streams(self) -> bool {
        matches!(self, Flavor::Claude | Flavor::Codex)
    }
}

/// Codex feature flags that add tools or context a coding player does not need. Unknown
/// names are ignored by codex, so the list may outlive a feature.
const CODEX_LEAN_FEATURES: &[&str] = &[
    "apps",
    "browser_use",
    "browser_use_external",
    "computer_use",
    "image_generation",
    "multi_agent",
    "goals",
    "plugins",
    "remote_plugin",
    "skill_search",
    "skill_mcp_dependency_install",
    "tool_suggest",
    "view_image",
    "sleep_tool",
    "realtime_conversation",
    "in_app_browser",
    "workspace_dependencies",
    "hooks",
    "memories",
];

/// UnumSearch's MCP tools (all read-only), as Claude Code names them.
const UNUMSEARCH_TOOLS: &[&str] = &[
    "mcp__unumsearch__search",
    "mcp__unumsearch__find_files",
    "mcp__unumsearch__index_status",
];

/// A running persistent process and what it was started with.
struct Live {
    proc: LiveProc,
    /// Claude: the `--effort` it was started with (a different rally effort restarts it
    /// with `--resume`). Codex takes effort per turn.
    effort: Option<String>,
    /// Claude reports a cumulative `total_cost_usd` per process.
    cost_seen: f64,
    /// Codex: the thread id and the next JSON-RPC request id.
    thread: Option<String>,
    next_id: u64,
}

pub struct AgentPlayer {
    flavor: Flavor,
    spec: AgentCli,
    session: Option<String>,
    live: Option<Live>,
    /// Set when the persistent mode is unavailable (the process could not start or speak
    /// the protocol): later rallies use the per-rally path.
    fell_back: Option<String>,
    /// Codex exec: the file holding a replacement system prompt (`model_instructions_file`).
    instructions_file: Option<PathBuf>,
}

impl Drop for AgentPlayer {
    fn drop(&mut self) {
        self.live.take();
        if let Some(f) = self.instructions_file.take() {
            let _ = std::fs::remove_file(f);
        }
    }
}

fn s(x: &str) -> String {
    x.to_string()
}

/// A TOML string literal for `-c key=value` (JSON string escapes are valid TOML).
fn toml_str(v: &str) -> String {
    Value::String(v.to_string()).to_string()
}

/// A TOML key segment: bare when it can be, quoted otherwise.
fn toml_key(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        k.to_string()
    } else {
        toml_str(k)
    }
}

impl AgentPlayer {
    pub fn new(flavor: Flavor, spec: AgentCli) -> Self {
        AgentPlayer {
            flavor,
            spec,
            session: None,
            live: None,
            fell_back: None,
            instructions_file: None,
        }
    }

    fn resume(&self) -> bool {
        self.spec.resume.unwrap_or(self.flavor.resume_default())
    }

    fn lean(&self) -> bool {
        self.flavor.streams() && self.spec.lean.unwrap_or(true)
    }

    fn bin(&self) -> String {
        self.spec
            .bin
            .clone()
            .unwrap_or_else(|| self.flavor.bin().to_string())
    }

    /// Persistent mode applies: wanted, a tool that has it, sessions kept, and (Codex)
    /// arguments the app server understands.
    fn persistent(&self) -> bool {
        self.flavor.streams()
            && self.spec.persistent.unwrap_or(true)
            && self.resume()
            && self.fell_back.is_none()
            && (self.flavor != Flavor::Codex || codex_server_args(&self.spec.args).is_some())
    }

    fn has_arg(&self, flag: &str) -> bool {
        self.spec.args.iter().any(|a| a == flag)
    }

    /// Claude Code flags shared by both modes: lean context, tools, system prompt.
    fn claude_context_args(&self) -> Vec<String> {
        let mut a = Vec::new();
        if self.lean() {
            for f in ["--strict-mcp-config", "--disable-slash-commands"] {
                if !self.has_arg(f) {
                    a.push(s(f));
                }
            }
            if !self.has_arg("--setting-sources") {
                a.extend([s("--setting-sources"), s("")]);
            }
            // Per-machine sections (cwd, git status) move to the first message, so every
            // table shares one cached system prompt.
            if self.spec.system_prompt.is_none()
                && !self.has_arg("--exclude-dynamic-system-prompt-sections")
            {
                a.push(s("--exclude-dynamic-system-prompt-sections"));
            }
        }
        if let Some(bin) = self.search_bin() {
            // Only the UnumSearch MCP server, pre-approved (its tools are read-only).
            let cfg = json!({"mcpServers": {"unumsearch": {"type": "stdio", "command": bin, "args": ["mcp"]}}});
            a.extend([s("--mcp-config"), cfg.to_string()]);
            if !a.iter().any(|x| x == "--strict-mcp-config") && !self.has_arg("--strict-mcp-config")
            {
                a.push(s("--strict-mcp-config"));
            }
            a.extend([s("--allowedTools"), UNUMSEARCH_TOOLS.join(",")]);
        }
        if let Some(t) = &self.spec.tools {
            a.extend([s("--tools"), t.join(",")]);
        }
        if let Some(p) = &self.spec.system_prompt {
            a.extend([s("--system-prompt"), p.clone()]);
        }
        a
    }

    /// The `unumsearch` binary when `search` is on.
    fn search_bin(&self) -> Option<String> {
        match self.spec.search.as_deref().map(str::trim) {
            None | Some("") | Some("off") | Some("none") => None,
            Some("unumsearch") => Some(s("unumsearch")),
            Some(path) => Some(path.to_string()),
        }
    }

    /// Codex `-c` overrides shared by both modes (lean context, tools). The replacement
    /// system prompt is mode-specific (a file for exec, a thread parameter for the server).
    fn codex_context_args(&self, env: &BTreeMap<String, String>, cwd: &Path) -> Vec<String> {
        let mut a = Vec::new();
        let search_bin = self.search_bin();
        let search_on = search_bin.is_some();
        let mut c = |kv: String| a.extend([s("-c"), kv]);
        if self.lean() {
            for f in CODEX_LEAN_FEATURES {
                c(format!("features.{f}=false"));
            }
            c(s("web_search=\"disabled\""));
            for name in codex_mcp_servers(env, cwd) {
                if search_on && name == "unumsearch" {
                    continue;
                }
                c(format!("mcp_servers.{}.enabled=false", toml_key(&name)));
            }
        }
        if let Some(bin) = search_bin {
            c(format!("mcp_servers.unumsearch.command={}", toml_str(&bin)));
            c(s("mcp_servers.unumsearch.args=[\"mcp\"]"));
            c(s("mcp_servers.unumsearch.enabled=true"));
        }
        if self.spec.tools.as_ref().is_some_and(|t| t.is_empty()) {
            c(s("features.shell_tool=false"));
            c(s("features.unified_exec=false"));
        }
        a
    }

    /// Build argv and stdin for one per-rally call. `out_file` is used by codex `-o`.
    fn command(&self, ball: &str, out_file: &str, cwd: &Path) -> (Vec<String>, Option<String>) {
        let mut a: Vec<String> = vec![self.bin()];
        let session = if self.resume() {
            self.session.clone()
        } else {
            None
        };
        match self.flavor {
            Flavor::Claude => {
                // claude -p --output-format json [--model M] [--resume ID] LEAN ARGS  (prompt on stdin)
                a.extend([s("-p"), s("--output-format"), s("json")]);
                if let Some(m) = &self.spec.model {
                    a.extend([s("--model"), m.clone()]);
                }
                if let Some(id) = &session {
                    a.extend([s("--resume"), id.clone()]);
                }
                a.extend(self.claude_context_args());
                a.extend(self.spec.args.iter().cloned());
                (a, Some(ball.to_string()))
            }
            Flavor::Codex => {
                // codex exec [resume ID] --json --skip-git-repo-check -o FILE [-m M] LEAN ARGS -  (stdin)
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
                a.extend(self.codex_context_args(&self.spec.env, cwd));
                if let Some(f) = &self.instructions_file {
                    a.extend([
                        s("-c"),
                        format!("model_instructions_file={}", toml_str(&f.to_string_lossy())),
                    ]);
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
                // --dir: opencode must work in the table's directory, whatever $PWD says.
                a.extend([
                    s("run"),
                    s("--format"),
                    s("json"),
                    s("--dir"),
                    cwd.to_string_lossy().into_owned(),
                ]);
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

    /// One rally as a separate invocation (resuming the session by id).
    fn play_once(&mut self, ball: &str, ctx: &PlayCtx) -> Result<Reply, String> {
        if self.flavor == Flavor::Codex
            && self.spec.system_prompt.is_some()
            && self.instructions_file.is_none()
        {
            let f = std::env::temp_dir().join(format!(
                "unumpingpong-{}-{}.instructions.md",
                ctx.table_id,
                uuid::Uuid::new_v4()
            ));
            std::fs::write(&f, self.spec.system_prompt.as_deref().unwrap_or(""))
                .map_err(|e| format!("cannot write {}: {e}", f.display()))?;
            self.instructions_file = Some(f);
        }
        let out_file = std::env::temp_dir().join(format!(
            "unumpingpong-{}-{}-{}.last.txt",
            ctx.table_id,
            ctx.rally,
            std::process::id()
        ));
        let out_path = out_file.to_string_lossy().into_owned();
        // Gemini: pick our own session UUID on the first rally so it can be resumed.
        let mut gemini_new: Option<String> = None;
        let (mut argv, stdin) = self.command(ball, &out_path, &ctx.cwd);
        if self.flavor == Flavor::Gemini && self.resume() && self.session.is_none() {
            let id = uuid::Uuid::new_v4().to_string();
            let pos = argv.len() - 2; // before -p BALL
            argv.splice(pos..pos, ["--session-id".to_string(), id.clone()]);
            gemini_new = Some(id);
        }
        if self.flavor == Flavor::Claude {
            if let Some(e) = &ctx.effort {
                // claude --effort low|medium|high|xhigh|max (the ball is on stdin, so the end is free)
                argv.extend(["--effort".to_string(), e.clone()]);
            }
        }
        if self.flavor == Flavor::Codex {
            if let Some(e) = &ctx.effort {
                // codex reads config overrides: -c model_reasoning_effort="low"
                let pos = argv.len() - 1; // before the trailing "-"
                argv.splice(
                    pos..pos,
                    ["-c".to_string(), format!("model_reasoning_effort=\"{e}\"")],
                );
            }
        }
        let out = proc::run_cancellable(
            &argv,
            &ctx.cwd,
            stdin.as_deref(),
            &ctx.env_with(&self.spec.env),
            ctx.budget(self.spec.timeout_secs, 900),
            Some(&ctx.cancel),
        )?;
        let last = std::fs::read_to_string(&out_file).ok();
        let _ = std::fs::remove_file(&out_file);
        if out.cancelled {
            return Err(format!("{} cancelled (table stopped)", self.flavor.bin()));
        }
        if out.timed_out {
            return Err(format!(
                "{} timed out after {:.0}s",
                self.flavor.bin(),
                out.duration_ms as f64 / 1000.0
            ));
        }
        // Every tool must exit 0: a nonzero exit is a failed turn even when it printed
        // something that parses (a partial answer, an error object, a stale result).
        if !out.ok() {
            return Err(format!(
                "{} exited with {}: {}\n{}",
                self.flavor.bin(),
                out.code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "a signal".into()),
                proc::tail(out.stderr.trim(), 1500),
                proc::tail(out.stdout.trim(), 500)
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
        Ok(reply)
    }

    // ------------------------------------------------------------ Claude Code, stream-json

    fn claude_spawn(&self, ctx: &PlayCtx) -> Result<Live, String> {
        // claude -p --input-format stream-json --output-format stream-json --verbose
        //        [--model M] [--effort E] [--resume ID] LEAN ARGS
        let mut a = vec![
            self.bin(),
            s("-p"),
            s("--input-format"),
            s("stream-json"),
            s("--output-format"),
            s("stream-json"),
            s("--verbose"),
        ];
        if let Some(m) = &self.spec.model {
            a.extend([s("--model"), m.clone()]);
        }
        if let Some(e) = &ctx.effort {
            a.extend([s("--effort"), e.clone()]);
        }
        if let Some(id) = &self.session {
            a.extend([s("--resume"), id.clone()]);
        }
        a.extend(self.claude_context_args());
        a.extend(self.spec.args.iter().cloned());
        let proc = LiveProc::spawn(&a, &ctx.cwd, &ctx.env_with(&self.spec.env))?;
        Ok(Live {
            proc,
            effort: ctx.effort.clone(),
            cost_seen: 0.0,
            thread: None,
            next_id: 1,
        })
    }

    fn claude_turn(&mut self, ball: &str, ctx: &PlayCtx, deadline: Instant) -> Turn {
        if self.live.as_ref().is_some_and(|l| l.effort != ctx.effort) {
            // `--effort` is per process: restart on the same session with the new effort.
            self.live = None;
        }
        let fresh = self.live.is_none();
        if fresh {
            match self.claude_spawn(ctx) {
                Ok(l) => self.live = Some(l),
                Err(e) => return Turn::Unavailable(e),
            }
        }
        let live = self.live.as_mut().expect("spawned");
        let msg = json!({"type": "user", "message": {"role": "user", "content": ball}});
        if let Err(e) = live.proc.send(&msg.to_string()) {
            return Turn::Failed(e);
        }
        let mut seen_any = false;
        loop {
            match live.proc.recv(deadline, Some(&ctx.cancel)) {
                Recv::Line(l) => {
                    seen_any = true;
                    let Ok(v) = serde_json::from_str::<Value>(l.trim()) else {
                        continue;
                    };
                    if let Some(id) = v["session_id"].as_str() {
                        self.session = Some(id.to_string());
                    }
                    if v["type"] != "result" {
                        continue;
                    }
                    let mut r = match claude_result(&v) {
                        Ok(r) => r,
                        Err(e) => return Turn::Failed(e),
                    };
                    // total_cost_usd is cumulative for the process.
                    let total = r.cost_usd;
                    r.cost_usd = if total >= live.cost_seen {
                        total - live.cost_seen
                    } else {
                        total
                    };
                    live.cost_seen = total;
                    return Turn::Done(r);
                }
                Recv::Eof => {
                    let code = live.proc.exited();
                    let err = live.proc.stderr_tail(1500);
                    self.live = None;
                    let msg = format!(
                        "claude (stream-json) ended before answering (exit {:?}): {err}",
                        code.flatten()
                    );
                    return if fresh && !seen_any {
                        Turn::Unavailable(msg)
                    } else {
                        Turn::Failed(msg)
                    };
                }
                Recv::TimedOut => {
                    self.live = None;
                    return Turn::Failed("claude timed out".into());
                }
                Recv::Cancelled => {
                    self.live = None;
                    return Turn::Failed("claude cancelled (table stopped)".into());
                }
            }
        }
    }

    // ------------------------------------------------------------ Codex, app-server

    fn codex_spawn(&mut self, ctx: &PlayCtx, deadline: Instant) -> Result<Live, String> {
        let env = ctx.env_with(&self.spec.env);
        let srv = codex_server_args(&self.spec.args).ok_or("arguments not supported")?;
        // codex app-server [LEAN -c ...] [ARGS: -c / --enable / --disable]   (stdio JSON-RPC)
        let mut a = vec![self.bin(), s("app-server")];
        a.extend(self.codex_context_args(&env, &ctx.cwd));
        a.extend(srv.pass);
        let proc = LiveProc::spawn(&a, &ctx.cwd, &env)?;
        let mut live = Live {
            proc,
            effort: None,
            cost_seen: 0.0,
            thread: None,
            next_id: 1,
        };
        rpc(
            &mut live,
            "initialize",
            json!({"clientInfo": {"name": "unumpingpong", "version": env!("CARGO_PKG_VERSION")}}),
            deadline,
            ctx,
        )?;
        live.proc
            .send(&json!({"jsonrpc": "2.0", "method": "initialized"}).to_string())?;
        let mut p = json!({
            "cwd": ctx.cwd.to_string_lossy(),
            "approvalPolicy": srv.approval.unwrap_or_else(|| s("never")),
        });
        if let Some(sb) = srv.sandbox {
            p["sandbox"] = json!(sb);
        }
        if let Some(m) = &self.spec.model {
            p["model"] = json!(m);
        }
        if let Some(sp) = &self.spec.system_prompt {
            p["baseInstructions"] = json!(sp);
        }
        let (method, p) = match &self.session {
            Some(id) => {
                p["threadId"] = json!(id);
                ("thread/resume", p)
            }
            None => ("thread/start", p),
        };
        let r = rpc(&mut live, method, p, deadline, ctx)?;
        let tid = r["thread"]["id"]
            .as_str()
            .ok_or_else(|| {
                format!(
                    "codex {method}: no thread id in {}",
                    proc::head(&r.to_string(), 300)
                )
            })?
            .to_string();
        self.session = Some(tid.clone());
        live.thread = Some(tid);
        Ok(live)
    }

    fn codex_turn(&mut self, ball: &str, ctx: &PlayCtx, deadline: Instant) -> Turn {
        let fresh = self.live.is_none();
        if fresh {
            match self.codex_spawn(ctx, deadline) {
                Ok(l) => self.live = Some(l),
                Err(e) => {
                    // A resumed thread that cannot be found is a fault, not a missing mode.
                    return if self.session.is_some() && e.contains("thread/resume") {
                        Turn::Failed(e)
                    } else {
                        Turn::Unavailable(e)
                    };
                }
            }
        }
        let live = self.live.as_mut().expect("spawned");
        let tid = live.thread.clone().unwrap_or_default();
        let mut p = json!({"threadId": tid, "input": [{"type": "text", "text": ball}]});
        if let Some(e) = &ctx.effort {
            p["effort"] = json!(e);
        }
        let id = live.next_id;
        live.next_id += 1;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": "turn/start", "params": p});
        if let Err(e) = live.proc.send(&req.to_string()) {
            self.live = None;
            return Turn::Failed(e);
        }
        let mut r = Reply {
            session: Some(tid.clone()),
            ..Default::default()
        };
        let mut text: Option<String> = None;
        let mut error: Option<String> = None;
        loop {
            let line = match live.proc.recv(deadline, Some(&ctx.cancel)) {
                Recv::Line(l) => l,
                Recv::Eof => {
                    let err = live.proc.stderr_tail(1500);
                    self.live = None;
                    return Turn::Failed(format!("codex app-server ended mid-turn: {err}"));
                }
                Recv::TimedOut => {
                    self.live = None;
                    return Turn::Failed("codex timed out".into());
                }
                Recv::Cancelled => {
                    self.live = None;
                    return Turn::Failed("codex cancelled (table stopped)".into());
                }
            };
            trace_line(&line);
            let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if v.get("method").is_some() && v.get("id").is_some() {
                // A server request (approval, user input): nobody is there to answer it.
                let _ = live.proc.send(&server_request_reply(&v).to_string());
                continue;
            }
            if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                if let Some(e) = v.get("error") {
                    return Turn::Failed(format!("codex turn/start: {}", rpc_error(e)));
                }
                continue;
            }
            let params = &v["params"];
            match v["method"].as_str().unwrap_or("") {
                "item/completed" if params["item"]["type"] == "agentMessage" => {
                    if let Some(t) = params["item"]["text"].as_str() {
                        text = Some(t.to_string());
                    }
                }
                "thread/tokenUsage/updated" => {
                    // `last` is one model call; a turn may make several.
                    let l = &params["tokenUsage"]["last"];
                    let input = u(&l["inputTokens"]);
                    let cached = u(&l["cachedInputTokens"]).min(input);
                    r.tokens_in += input - cached;
                    r.tokens_cached += cached;
                    r.tokens_out += u(&l["outputTokens"]);
                }
                "error" if params["willRetry"] != true => {
                    error = params["error"]["message"].as_str().map(String::from);
                }
                "turn/completed" if params["threadId"] == tid.as_str() => {
                    let turn = &params["turn"];
                    let status = turn["status"].as_str().unwrap_or("");
                    if status != "completed" {
                        let why = turn["error"]["message"]
                            .as_str()
                            .map(String::from)
                            .or(error)
                            .unwrap_or_else(|| format!("turn {status}"));
                        return Turn::Failed(format!("codex turn failed: {why}"));
                    }
                    if let Some(e) = error {
                        return Turn::Failed(format!("codex turn failed: {e}"));
                    }
                    return match text.filter(|t| !t.trim().is_empty()) {
                        Some(t) => {
                            r.text = t;
                            Turn::Done(r)
                        }
                        None => Turn::Failed("no agent message in codex turn".into()),
                    };
                }
                _ => {}
            }
        }
    }
}

/// Outcome of one turn on a persistent process.
enum Turn {
    Done(Reply),
    Failed(String),
    /// The mode is not available here (could not start, did not speak the protocol).
    Unavailable(String),
}

/// What the codex app server accepts from a player's `args`: `-c`/`--config`, `--enable`,
/// `--disable` pass through; `--dangerously-bypass-approvals-and-sandbox`, `-s`/`--sandbox`
/// and `--full-auto` become thread settings; `--skip-git-repo-check` is moot. Anything else
/// is exec-only: `None` (the player keeps the per-rally path).
struct CodexServerArgs {
    pass: Vec<String>,
    approval: Option<String>,
    sandbox: Option<String>,
}

fn codex_server_args(args: &[String]) -> Option<CodexServerArgs> {
    let mut out = CodexServerArgs {
        pass: Vec::new(),
        approval: None,
        sandbox: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-c" | "--config" | "--enable" | "--disable" => {
                out.pass.push(a.clone());
                out.pass.push(it.next()?.clone());
            }
            "--strict-config" => out.pass.push(a.clone()),
            "--dangerously-bypass-approvals-and-sandbox" => {
                out.approval = Some(s("never"));
                out.sandbox = Some(s("danger-full-access"));
            }
            "-s" | "--sandbox" => out.sandbox = Some(it.next()?.clone()),
            "--full-auto" => out.sandbox = Some(s("workspace-write")),
            "--skip-git-repo-check" => {}
            x if x.starts_with("--config=")
                || x.starts_with("--enable=")
                || x.starts_with("--disable=") =>
            {
                out.pass.push(a.clone())
            }
            x if x.starts_with("--sandbox=") => {
                out.sandbox = Some(x["--sandbox=".len()..].to_string())
            }
            _ => return None,
        }
    }
    Some(out)
}

/// MCP servers in the user's codex config (`$CODEX_HOME/config.toml`, else
/// `~/.codex/config.toml`): lean mode turns each off for the player. A relative
/// `CODEX_HOME` is the player's, so it is taken from the table's cwd.
fn codex_mcp_servers(env: &BTreeMap<String, String>, cwd: &Path) -> Vec<String> {
    let home = env
        .get("CODEX_HOME")
        .cloned()
        .or_else(|| std::env::var("CODEX_HOME").ok())
        .map(|h| cwd.join(h))
        .or_else(|| {
            std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .ok()
                .map(|h| PathBuf::from(h).join(".codex"))
        });
    let Some(text) = home.and_then(|h| std::fs::read_to_string(h.join("config.toml")).ok()) else {
        return Vec::new();
    };
    match toml::from_str::<toml::Table>(&text) {
        Ok(t) => t
            .get("mcp_servers")
            .and_then(|m| m.as_table())
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn rpc_error(e: &Value) -> String {
    e["message"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| e.to_string())
}

/// One JSON-RPC request on a live codex app server; notifications in between are skipped.
fn rpc(
    live: &mut Live,
    method: &str,
    params: Value,
    deadline: Instant,
    ctx: &PlayCtx,
) -> Result<Value, String> {
    let id = live.next_id;
    live.next_id += 1;
    live.proc.send(
        &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
    )?;
    loop {
        match live.proc.recv(deadline, Some(&ctx.cancel)) {
            Recv::Line(l) => {
                let Ok(v) = serde_json::from_str::<Value>(l.trim()) else {
                    continue;
                };
                if v.get("method").is_some() && v.get("id").is_some() {
                    let _ = live.proc.send(&server_request_reply(&v).to_string());
                    continue;
                }
                if v.get("id").and_then(|x| x.as_u64()) != Some(id) {
                    continue;
                }
                if let Some(e) = v.get("error") {
                    return Err(format!("codex {method}: {}", rpc_error(e)));
                }
                return Ok(v["result"].clone());
            }
            Recv::Eof => {
                return Err(format!(
                    "codex app-server ended during {method} (exit {:?}): {}",
                    live.proc.exited().flatten(),
                    live.proc.stderr_tail(1500)
                ))
            }
            Recv::TimedOut => return Err(format!("codex {method} timed out")),
            Recv::Cancelled => return Err("codex cancelled (table stopped)".into()),
        }
    }
}

/// Approvals are declined (the player runs unattended with the policy it was given);
/// other server requests get "method not found".
fn server_request_reply(req: &Value) -> Value {
    let id = req["id"].clone();
    match req["method"].as_str().unwrap_or("") {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            json!({"jsonrpc": "2.0", "id": id, "result": {"decision": "decline"}})
        }
        "applyPatchApproval" | "execCommandApproval" => {
            json!({"jsonrpc": "2.0", "id": id, "result": {"decision": "denied"}})
        }
        m => json!({"jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": format!("unumpingpong does not answer {m}")}}),
    }
}

impl Player for AgentPlayer {
    fn play(&mut self, ball: &str, ctx: &PlayCtx) -> Result<Reply, String> {
        if self.persistent() {
            let deadline = Instant::now() + ctx.budget(self.spec.timeout_secs, 900);
            let turn = match self.flavor {
                Flavor::Claude => self.claude_turn(ball, ctx, deadline),
                _ => self.codex_turn(ball, ctx, deadline),
            };
            match turn {
                Turn::Done(mut r) => {
                    if r.session.is_none() {
                        r.session = self.session.clone();
                    }
                    if r.session.is_some() {
                        self.session = r.session.clone();
                    }
                    return Ok(r);
                }
                Turn::Failed(e) => return Err(e),
                Turn::Unavailable(e) => {
                    // Fall back to one invocation per rally for the rest of the table.
                    self.live = None;
                    self.fell_back = Some(e);
                }
            }
        }
        let reply = self.play_once(ball, ctx)?;
        if reply.session.is_some() {
            self.session = reply.session.clone();
        }
        Ok(reply)
    }

    fn keeps_session(&self) -> bool {
        self.resume()
    }

    fn restore(&mut self, _history: &[(String, String)], session: Option<&str>) -> bool {
        // Gemini sessions are keyed by the UUID we chose, which is the stored session id too.
        // A persistent player resumes the same session when it starts its process.
        match session.filter(|_| self.resume()) {
            Some(s) => {
                self.session = Some(s.to_string());
                self.live = None;
                true
            }
            None => false,
        }
    }

    fn mode(&self) -> String {
        if self.persistent() {
            match self.flavor {
                Flavor::Claude => s("persistent (claude stream-json)"),
                _ => s("persistent (codex app-server)"),
            }
        } else if let Some(why) = &self.fell_back {
            format!(
                "per-rally (persistent mode unavailable: {})",
                proc::head(why, 200)
            )
        } else {
            s("per-rally")
        }
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
    claude_result(&r)
}

/// A Claude Code `result` object (json or stream-json). Fresh input = `input_tokens` +
/// cache writes; cache reads are counted apart.
fn claude_result(r: &Value) -> Result<Reply, String> {
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
        tokens_in: u(&us["input_tokens"]) + u(&us["cache_creation_input_tokens"]),
        tokens_cached: u(&us["cache_read_input_tokens"]),
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
                // input_tokens includes the cached part.
                let us = &v["usage"];
                let cached = u(&us["cached_input_tokens"]).min(u(&us["input_tokens"]));
                r.tokens_in += u(&us["input_tokens"]) - cached;
                r.tokens_cached += cached;
                r.tokens_out += u(&us["output_tokens"]);
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
    // A failed turn is a failure even if an agent message (or -o file) was produced.
    if let Some(e) = error {
        return Err(format!("codex turn failed: {e}"));
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
        None => Err("no agent message in codex output".into()),
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
            // prompt includes the cached part.
            let cached = u(&m["tokens"]["cached"]).min(u(&m["tokens"]["prompt"]));
            r.tokens_in += u(&m["tokens"]["prompt"]) - cached;
            r.tokens_cached += cached;
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
                r.tokens_in += u(&part["tokens"]["input"]) + u(&part["tokens"]["cache"]["write"]);
                r.tokens_cached += u(&part["tokens"]["cache"]["read"]);
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
    if let Some(e) = error {
        return Err(format!("opencode error: {e}"));
    }
    if texts.is_empty() {
        return Err("no text in opencode output".into());
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

/// `UPP_PLAYER_TRACE=<file>`: append every raw message a persistent agent CLI sends (for
/// auditing token accounting against the tool's own counters).
fn trace_line(line: &str) {
    if let Some(path) = std::env::var_os("UPP_PLAYER_TRACE") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{}", line.trim_end());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_json() {
        let out = r#"{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"abc","total_cost_usd":0.01,"usage":{"input_tokens":10,"cache_creation_input_tokens":40,"cache_read_input_tokens":500,"output_tokens":3}}"#;
        let r = parse_claude(out).unwrap();
        assert_eq!(r.text, "done");
        assert_eq!(r.session.as_deref(), Some("abc"));
        // Fresh input = input + cache writes; cache reads apart; billable = 50 + 3 + 50.
        assert_eq!((r.tokens_in, r.tokens_cached, r.tokens_out), (50, 500, 3));
        assert_eq!(r.billable(), 103);
        assert!(parse_claude(r#"{"type":"result","is_error":true,"result":"boom"}"#).is_err());
    }

    #[test]
    fn codex_jsonl() {
        let out = "{\"type\":\"thread.started\",\"thread_id\":\"th-1\"}\n\
                   {\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"hi\"}}\n\
                   {\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":7,\"cached_input_tokens\":4,\"output_tokens\":2}}\n";
        let r = parse_codex(out, None).unwrap();
        assert_eq!(r.text, "hi");
        assert_eq!((r.tokens_in, r.tokens_cached, r.tokens_out), (3, 4, 2));
        assert_eq!(r.session.as_deref(), Some("th-1"));
        assert_eq!(parse_codex(out, Some("from -o")).unwrap().text, "from -o");
    }

    #[test]
    fn failed_turns_never_count_as_answers() {
        // codex: text was seen (and -o written), but the turn failed.
        let out = "{\"type\":\"thread.started\",\"thread_id\":\"th-1\"}\n\
                   {\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"All done!\"}}\n\
                   {\"type\":\"turn.failed\",\"error\":{\"message\":\"stream disconnected\"}}\n";
        let e = parse_codex(out, Some("All done!")).unwrap_err();
        assert!(e.contains("stream disconnected"), "{e}");
        let out =
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"ok\"}}\n\
                   {\"type\":\"error\",\"message\":\"quota exceeded\"}\n";
        assert!(parse_codex(out, None).unwrap_err().contains("quota"));
        // opencode: an error event after some text.
        let o = "{\"type\":\"text\",\"part\":{\"text\":\"partial\"}}\n\
                 {\"type\":\"error\",\"error\":{\"message\":\"provider down\"}}\n";
        assert!(parse_opencode(o).unwrap_err().contains("provider down"));
    }

    #[cfg(unix)]
    #[test]
    fn a_nonzero_exit_is_a_fault_even_with_parseable_output() {
        // A fake `claude` that prints a valid success result and then exits 1.
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("fake-claude.sh");
        std::fs::write(
            &bin,
            "#!/bin/sh\ncat >/dev/null\necho '{\"type\":\"result\",\"is_error\":false,\"result\":\"done\",\"session_id\":\"s\"}'\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let mut p = AgentPlayer::new(
            Flavor::Claude,
            AgentCli {
                bin: Some(bin.to_string_lossy().into_owned()),
                persistent: Some(false),
                ..Default::default()
            },
        );
        let ctx = PlayCtx::new(
            "t",
            1,
            d.path().to_path_buf(),
            std::time::Instant::now() + std::time::Duration::from_secs(20),
            "t",
        );
        let e = p.play("ball", &ctx).unwrap_err();
        assert!(e.contains("exited with 1"), "{e}");
    }

    #[cfg(unix)]
    #[test]
    fn claude_resumes_the_session_and_gets_the_rally_effort() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("argv.log");
        let bin = d.path().join("fake-claude.sh");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\ncat >/dev/null\necho \"$@\" >> {}\necho '{{\"type\":\"result\",\"is_error\":false,\"result\":\"ok\",\"session_id\":\"sess-1\"}}'\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let mut p = AgentPlayer::new(
            Flavor::Claude,
            AgentCli {
                bin: Some(bin.to_string_lossy().into_owned()),
                model: Some("haiku".into()),
                persistent: Some(false),
                ..Default::default()
            },
        );
        for (n, effort) in [(1, "low"), (2, "high")] {
            let mut ctx = PlayCtx::new(
                "t",
                n,
                d.path().to_path_buf(),
                std::time::Instant::now() + std::time::Duration::from_secs(20),
                "t",
            );
            ctx.effort = Some(effort.into());
            p.play("ball", &ctx).unwrap();
        }
        let calls: Vec<String> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].contains("--effort low") && !calls[0].contains("--resume"));
        assert!(calls[1].contains("--resume sess-1") && calls[1].contains("--effort high"));
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
        let (a, stdin) = p.command("ball", "/tmp/x", Path::new("."));
        assert_eq!(a[..4], ["claude", "-p", "--output-format", "json"]);
        assert!(stdin.is_some());
        p.session = Some("S".into());
        let (a, _) = p.command("ball", "/tmp/x", Path::new("."));
        assert!(a.windows(2).any(|w| w == ["--resume", "S"]));

        let mut c = AgentPlayer::new(Flavor::Codex, AgentCli::default());
        c.session = Some("T".into());
        let (a, _) = c.command("b", "/tmp/o", Path::new("."));
        assert_eq!(a[1..3], ["exec", "resume"]);
        assert_eq!(a[a.len() - 2..], ["T", "-"]);

        let o = AgentPlayer::new(Flavor::Opencode, AgentCli::default());
        let (a, _) = o.command("-x", "/tmp/o", Path::new("."));
        assert_eq!(a.last().unwrap(), "Task: -x");

        let g = AgentPlayer::new(Flavor::Gemini, AgentCli::default());
        let (a, stdin) = g.command("b", "/tmp/o", Path::new("."));
        assert_eq!(a[a.len() - 2..], ["-p", "b"]);
        assert!(stdin.is_none());
        assert!(g.keeps_session());
    }
}
