// SPDX-License-Identifier: Apache-2.0
//! Table configuration: match files in TOML or JSON.
//!
//! A *match file* holds `[defaults]` and one or more `[[table]]` entries. Every
//! field has an env-free default, so a table needs only a `name`, a `ball`, a
//! `player` and (usually) a `referee`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// A command: either one shell line (`sh -c` on Unix, `cmd /C` on Windows) or an argv array.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum Cmd {
    Line(String),
    Argv(Vec<String>),
}

impl Cmd {
    pub fn argv(&self) -> Vec<String> {
        match self {
            Cmd::Argv(v) => v.clone(),
            Cmd::Line(s) => {
                if cfg!(windows) {
                    vec!["cmd".into(), "/C".into(), s.clone()]
                } else {
                    vec!["sh".into(), "-c".into(), s.clone()]
                }
            }
        }
    }
    pub fn display(&self) -> String {
        match self {
            Cmd::Line(s) => s.clone(),
            Cmd::Argv(v) => v.join(" "),
        }
    }
}

/// Bounds for one table. The first one reached ends the table as `expired`
/// (or `lost` for `max_rallies`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    /// Wall-clock budget for the whole table, seconds. Default 1800.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_secs: Option<u64>,
    /// Maximum rallies (player turns). Default 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rallies: Option<u32>,
    /// Token budget (input + output), when the player reports usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Cost budget in USD, when the player reports cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
}

impl Lease {
    pub fn merged(&self, base: &Lease) -> Lease {
        Lease {
            wall_secs: self.wall_secs.or(base.wall_secs),
            max_rallies: self.max_rallies.or(base.max_rallies),
            max_tokens: self.max_tokens.or(base.max_tokens),
            max_cost_usd: self.max_cost_usd.or(base.max_cost_usd),
        }
    }
    pub fn wall(&self) -> u64 {
        self.wall_secs.unwrap_or(1800)
    }
    pub fn rallies(&self) -> u32 {
        self.max_rallies.unwrap_or(5).max(1)
    }
}

/// Options shared by the agent CLI players (claude, codex, gemini, opencode).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct AgentCli {
    /// Executable; defaults to the tool name on PATH.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Extra arguments appended to every call (permission flags, sandbox mode, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Per-rally timeout, seconds (also bounded by the table's wall lease). Default 900.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Keep the tool's session across rallies when it supports resuming. Default true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// A player (worker) or a model server / judge. Selected by `kind`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlayerSpec {
    /// Any command: the ball on stdin, the answer on stdout.
    Shell {
        command: Cmd,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    },
    /// Any OpenAI-compatible `/chat/completions` endpoint.
    Openai {
        #[serde(default = "default_openai_base")]
        base_url: String,
        model: String,
        /// Name of the environment variable holding the API key (never the key itself).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key_env: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        temperature: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_tokens: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
        /// Send `X-Halogen-Keep`/`X-Halogen-From` so a Halogen server forks the cached session.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        halogen_keep: bool,
        /// Extra JSON merged into every request body (e.g. `{"top_p": 0.9}`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extra: Option<serde_json::Value>,
    },
    /// OpenUnum HTTP API (`POST /api/chat` with a sessionId, poll `/api/chat/pending`).
    Openunum {
        #[serde(default = "default_openunum_base")]
        base_url: String,
        /// Environment variable holding the WebUI password, if one is set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password_env: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_prefix: Option<String>,
    },
    Claude(AgentCli),
    Codex(AgentCli),
    Gemini(AgentCli),
    Opencode(AgentCli),
}

fn default_openai_base() -> String {
    "http://127.0.0.1:8080/v1".into()
}
fn default_openunum_base() -> String {
    "http://127.0.0.1:18880".into()
}

impl PlayerSpec {
    pub fn kind(&self) -> &'static str {
        match self {
            PlayerSpec::Shell { .. } => "shell",
            PlayerSpec::Openai { .. } => "openai",
            PlayerSpec::Openunum { .. } => "openunum",
            PlayerSpec::Claude(_) => "claude",
            PlayerSpec::Codex(_) => "codex",
            PlayerSpec::Gemini(_) => "gemini",
            PlayerSpec::Opencode(_) => "opencode",
        }
    }
    /// Short label for listings: `kind` or `kind:model`.
    pub fn label(&self) -> String {
        let model = match self {
            PlayerSpec::Openai { model, .. } => Some(model.clone()),
            PlayerSpec::Claude(a)
            | PlayerSpec::Codex(a)
            | PlayerSpec::Gemini(a)
            | PlayerSpec::Opencode(a) => a.model.clone(),
            _ => None,
        };
        match model {
            Some(m) => format!("{}:{}", self.kind(), m.rsplit('/').next().unwrap_or(&m)),
            None => self.kind().to_string(),
        }
    }
}

/// One referee check. `name` is optional (a default is derived from the kind).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CheckSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(flatten)]
    pub check: Check,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Check {
    /// Run a command; passes on exit 0 (and `expect_regex` matching stdout+stderr, if set).
    Command {
        run: Cmd,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_regex: Option<String>,
        /// Working directory, relative to the table's `cwd`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    },
    /// A file exists (relative to the table's `cwd`), optionally containing a regex.
    FileExists {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contains: Option<String>,
    },
    /// An HTTP GET answers 2xx (or exactly `status`), optionally matching a regex.
    HttpOk {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_regex: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
    },
    /// A JSON value at `path` (dotted, `a.b.0.c`) equals `equals`. Source: `file`, `url`,
    /// or (neither) the player's answer, parsed as JSON (the first `{...}`/`[...]` block).
    JsonPathEquals {
        path: String,
        equals: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    /// An LLM judge: last resort, runs only after every deterministic check passed,
    /// and must not be the table's own player. Its reply must start with PASS or FAIL.
    Judge {
        player: PlayerSpec,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rubric: Option<String>,
    },
}

impl Check {
    pub fn kind(&self) -> &'static str {
        match self {
            Check::Command { .. } => "command",
            Check::FileExists { .. } => "file_exists",
            Check::HttpOk { .. } => "http_ok",
            Check::JsonPathEquals { .. } => "json_path_equals",
            Check::Judge { .. } => "judge",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ServerMode {
    /// Deterministic: the return message is a template over the referee output.
    #[default]
    Auto,
    /// A model (another player) writes the correction note.
    Model,
    /// An outside orchestrator answers each ball (MCP `ball_answer`, HTTP, or `upp answer`).
    External,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ServerSpec {
    #[serde(default)]
    pub mode: ServerMode,
    /// The server model for `mode = "model"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player: Option<PlayerSpec>,
    /// Ask the server to confirm balls the referee passed (default: true for external, else false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_points: Option<bool>,
    /// How long an external server may take to answer one ball, seconds. Default 3600.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Extra standing instructions appended to every return message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Write the player's answer into a file before the referee runs. This lets a
/// chat-only model (the `openai` player) work on code: it answers with a code block.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Apply {
    /// Target file, relative to the table's `cwd`.
    pub write_file: String,
    /// `code_block` (default): the first fenced block of the answer, else the whole answer.
    /// `answer`: the whole answer.
    #[serde(default = "default_apply_from")]
    pub from: String,
}

fn default_apply_from() -> String {
    "code_block".into()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct TableSpec {
    pub name: String,
    #[serde(default)]
    pub goal: String,
    /// The first ball (the brief). Templates: `{{goal}}`, `{{prev.answer}}`, `{{prev.name}}`,
    /// `{{file:path}}` (a file's content, read from `cwd` after `setup`).
    #[serde(default)]
    pub ball: String,
    /// Serial chaining: start only after the named table scored its point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Working directory (created if missing). Relative paths resolve against the match file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Command run once in `cwd` before the first rally (copy a fixture, create a worktree...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<Cmd>,
    /// Write the answer (or its first code block) to a file before the referee runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply: Option<Apply>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player: Option<PlayerSpec>,
    #[serde(default, alias = "referees", skip_serializing_if = "Vec::is_empty")]
    pub referee: Vec<CheckSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<Lease>,
    /// Opt-in for a table without referee checks: its answers score with no evidence,
    /// and the point is recorded as unverified. Without it, a table needs at least one check.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unverified: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player: Option<PlayerSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<Lease>,
    #[serde(default, alias = "referees", skip_serializing_if = "Vec::is_empty")]
    pub referee: Vec<CheckSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply: Option<Apply>,
    /// Default for `unverified` (see TableSpec).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unverified: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct MatchFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default, rename = "table", alias = "tables")]
    pub tables: Vec<TableSpec>,
}

impl MatchFile {
    /// Parse TOML or JSON (JSON when the text starts with `{`). A bare table object
    /// (JSON with a `name` and no `table` list) is accepted as a one-table match.
    pub fn parse(text: &str) -> Result<MatchFile, String> {
        let t = text.trim_start();
        if t.starts_with('{') {
            let v: serde_json::Value =
                serde_json::from_str(t).map_err(|e| format!("invalid JSON: {e}"))?;
            Self::from_json(v)
        } else {
            toml::from_str(text).map_err(|e| format!("invalid TOML: {e}"))
        }
    }

    pub fn from_json(v: serde_json::Value) -> Result<MatchFile, String> {
        let is_single =
            v.get("name").is_some() && v.get("table").is_none() && v.get("tables").is_none();
        if is_single {
            let t: TableSpec =
                serde_json::from_value(v).map_err(|e| format!("invalid table: {e}"))?;
            return Ok(MatchFile {
                tables: vec![t],
                ..Default::default()
            });
        }
        serde_json::from_value(v).map_err(|e| format!("invalid match: {e}"))
    }

    pub fn load(path: &Path) -> Result<MatchFile, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut m = Self::parse(&text)?;
        // Relative cwd values are relative to the match file's directory.
        let base = path
            .parent()
            .map(|p| {
                if p.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    p
                }
            })
            .unwrap_or(Path::new("."));
        let base = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
        let fix = |c: &mut Option<String>| {
            let rel = c.clone().unwrap_or_else(|| ".".into());
            if !Path::new(&rel).is_absolute() {
                *c = Some(base.join(rel).to_string_lossy().into_owned());
            }
        };
        fix(&mut m.defaults.cwd);
        for t in &mut m.tables {
            if let Some(c) = &t.cwd {
                if !Path::new(c).is_absolute() {
                    t.cwd = Some(base.join(c).to_string_lossy().into_owned());
                }
            }
        }
        Ok(m)
    }

    /// Fill each table from `[defaults]` and validate. Returns fully resolved tables.
    pub fn resolve(&self) -> Result<Vec<TableSpec>, String> {
        if self.tables.is_empty() {
            return Err("no tables: add at least one [[table]]".into());
        }
        let d = &self.defaults;
        let mut out = Vec::new();
        let mut names = std::collections::HashSet::new();
        for t in &self.tables {
            let mut t = t.clone();
            if t.name.trim().is_empty() {
                return Err("every table needs a name".into());
            }
            if !names.insert(t.name.clone()) {
                return Err(format!("duplicate table name '{}'", t.name));
            }
            if t.cwd.is_none() {
                t.cwd = d.cwd.clone();
            }
            if t.player.is_none() {
                t.player = d.player.clone();
            }
            if t.server.is_none() {
                t.server = d.server.clone();
            }
            if t.referee.is_empty() {
                t.referee = d.referee.clone();
            }
            if t.apply.is_none() {
                t.apply = d.apply.clone();
            }
            if !t.unverified {
                t.unverified = d.unverified.unwrap_or(false);
            }
            let base = d.lease.clone().unwrap_or_default();
            t.lease = Some(t.lease.clone().unwrap_or_default().merged(&base));
            validate_table(&t)?;
            out.push(t);
        }
        for t in &out {
            if let Some(a) = &t.after {
                if !names.contains(a) {
                    return Err(format!("table '{}': after = '{a}' names no table", t.name));
                }
                if a == &t.name {
                    return Err(format!("table '{}' cannot follow itself", t.name));
                }
            }
        }
        // Reject cycles in `after`.
        for t in &out {
            let mut seen = std::collections::HashSet::new();
            let mut cur = t.after.clone();
            while let Some(c) = cur {
                if !seen.insert(c.clone()) {
                    return Err(format!("cycle in `after` chain at '{}'", t.name));
                }
                cur = out
                    .iter()
                    .find(|x| x.name == c)
                    .and_then(|x| x.after.clone());
            }
        }
        Ok(out)
    }
}

pub fn validate_table(t: &TableSpec) -> Result<(), String> {
    let player = t.player.as_ref().ok_or_else(|| {
        format!(
            "table '{}': no player (set [table.player] or [defaults.player])",
            t.name
        )
    })?;
    if t.ball.trim().is_empty() && t.goal.trim().is_empty() {
        return Err(format!(
            "table '{}': needs a ball (brief) or a goal",
            t.name
        ));
    }
    if t.referee.is_empty() && !t.unverified {
        return Err(format!(
            "table '{}': no referee checks, so nothing would verify an answer. Add a check \
             (command, file_exists, http_ok, json_path_equals, judge) or set `unverified = true` \
             to accept answers without evidence",
            t.name
        ));
    }
    for c in &t.referee {
        match &c.check {
            Check::Judge { player: j, .. } => {
                if j == player {
                    return Err(format!(
                        "table '{}': the judge must not be the table's own player",
                        t.name
                    ));
                }
            }
            Check::Command {
                expect_regex: Some(r),
                ..
            }
            | Check::HttpOk {
                expect_regex: Some(r),
                ..
            }
            | Check::FileExists {
                contains: Some(r), ..
            } => {
                regex::Regex::new(r)
                    .map_err(|e| format!("table '{}': bad regex {r:?}: {e}", t.name))?;
            }
            _ => {}
        }
    }
    if let Some(s) = &t.server {
        if s.mode == ServerMode::Model && s.player.is_none() {
            return Err(format!(
                "table '{}': server mode \"model\" needs server.player",
                t.name
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOML: &str = r#"
[defaults]
max_parallel = 2
lease = { wall_secs = 60, max_rallies = 3 }
player = { kind = "shell", command = "cat" }

[[table]]
name = "a"
ball = "do a"
referee = [{ kind = "command", run = "true" }]

[[table]]
name = "b"
after = "a"
ball = "use {{prev.answer}}"
lease = { max_rallies = 7 }
unverified = true
[table.player]
kind = "claude"
model = "sonnet"
args = ["--permission-mode", "acceptEdits"]
"#;

    #[test]
    fn toml_defaults_and_chain() {
        let m = MatchFile::parse(TOML).unwrap();
        let t = m.resolve().unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].lease.as_ref().unwrap().rallies(), 3);
        assert_eq!(t[1].lease.as_ref().unwrap().rallies(), 7);
        assert_eq!(t[1].lease.as_ref().unwrap().wall(), 60);
        assert_eq!(t[1].after.as_deref(), Some("a"));
        assert_eq!(t[1].player.as_ref().unwrap().label(), "claude:sonnet");
        // b inherits the default referee? No: defaults.referee is empty, b has none.
        assert!(t[1].referee.is_empty());
    }

    #[test]
    fn json_single_table() {
        let m = MatchFile::parse(
            r#"{"name":"x","ball":"hi","player":{"kind":"openai","model":"m"},
                "referee":[{"kind":"json_path_equals","path":"ok","equals":true}]}"#,
        )
        .unwrap();
        let t = m.resolve().unwrap();
        assert_eq!(t[0].name, "x");
        assert!(matches!(
            t[0].player,
            Some(PlayerSpec::Openai { ref base_url, .. }) if base_url == "http://127.0.0.1:8080/v1"
        ));
    }

    #[test]
    fn rejects_bad_configs() {
        let judge_self = r#"{"name":"x","ball":"b","player":{"kind":"shell","command":"cat"},
            "referee":[{"kind":"judge","player":{"kind":"shell","command":"cat"}}]}"#;
        assert!(MatchFile::parse(judge_self)
            .unwrap()
            .resolve()
            .unwrap_err()
            .contains("judge"));
        let cycle = r#"
[defaults]
unverified = true
player = { kind = "shell", command = "cat" }
[[table]]
name = "a"
after = "b"
ball = "x"
[[table]]
name = "b"
after = "a"
ball = "y"
"#;
        assert!(MatchFile::parse(cycle)
            .unwrap()
            .resolve()
            .unwrap_err()
            .contains("cycle"));
        let typo = "[[table]]\nname = \"x\"\nbal = \"y\"\n";
        assert!(MatchFile::parse(typo)
            .unwrap_err()
            .contains("unknown field"));
        let no_player = r#"{"name":"x","ball":"b"}"#;
        assert!(MatchFile::parse(no_player).unwrap().resolve().is_err());
        // No referee: rejected unless the table opts in to unverified points.
        let unchecked = r#"{"name":"x","ball":"b","player":{"kind":"shell","command":"cat"}}"#;
        assert!(MatchFile::parse(unchecked)
            .unwrap()
            .resolve()
            .unwrap_err()
            .contains("unverified = true"));
        let opted = r#"{"name":"x","ball":"b","unverified":true,"player":{"kind":"shell","command":"cat"}}"#;
        assert!(MatchFile::parse(opted).unwrap().resolve().unwrap()[0].unverified);
    }
}
