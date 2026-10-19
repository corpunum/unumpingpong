// SPDX-License-Identifier: Apache-2.0
//! Players: every coding/agent tool behind one trait. Each player is built once
//! per table and keeps its session across rallies where the tool supports it.

mod cli;
mod openai;
mod openunum;
mod shell;

use crate::config::PlayerSpec;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use cli::{parse_claude, parse_codex, parse_gemini, parse_opencode, Flavor};
pub use openai::strip_think;

/// Per-call context.
#[derive(Clone, Debug)]
pub struct PlayCtx {
    pub table_id: String,
    pub rally: u32,
    pub cwd: PathBuf,
    /// The table's lease deadline: no call may run past it.
    pub deadline: Instant,
    /// Stable per table (and role): a session id for tools that take one from the caller.
    pub session_key: String,
    /// Environment for process players: `UPP_TABLE`, `UPP_RALLY`, `UPP_DB`, `UPP_RUN`,
    /// `UPP_DEPTH` (so a player can open sub-tables with its table as the parent).
    pub env: BTreeMap<String, String>,
    /// Reasoning effort hint for this rally (the table's `effort`).
    pub effort: Option<String>,
    /// Set when the table is stopped: players abort the running call (process group killed).
    pub cancel: Arc<AtomicBool>,
}

impl PlayCtx {
    pub fn new(
        table_id: &str,
        rally: u32,
        cwd: PathBuf,
        deadline: Instant,
        session_key: &str,
    ) -> PlayCtx {
        PlayCtx {
            table_id: table_id.to_string(),
            rally,
            cwd,
            deadline,
            session_key: session_key.to_string(),
            env: BTreeMap::new(),
            effort: None,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// `env` merged under the player's own environment (the player's entries win).
    pub fn env_with(&self, own: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut e = self.env.clone();
        e.insert("UPP_TABLE".into(), self.table_id.clone());
        e.insert("UPP_RALLY".into(), self.rally.to_string());
        e.extend(own.iter().map(|(k, v)| (k.clone(), v.clone())));
        e
    }

    /// Time left for one call: the lease remainder, capped by the player's own timeout.
    pub fn budget(&self, cap_secs: Option<u64>, default_secs: u64) -> Duration {
        self.deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(cap_secs.unwrap_or(default_secs)))
            .max(Duration::from_millis(200))
    }
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct Reply {
    pub text: String,
    /// Fresh (uncached) input tokens, cache writes included.
    pub tokens_in: u64,
    /// Input tokens read from the provider's prompt cache (billed at a discount).
    pub tokens_cached: u64,
    pub tokens_out: u64,
    /// Cost the tool reported for this call (0 when it reports none).
    pub cost_usd: f64,
    /// The tool's own session/thread id, when it reports one.
    pub session: Option<String>,
}

/// Cache reads are billed at about a tenth of fresh input by Anthropic and OpenAI.
pub const CACHED_WEIGHT: f64 = 0.1;

/// Billable tokens: fresh input + output + cache reads at their discount. The token count
/// on tables, cards and leases.
pub fn billable(tokens_in: u64, tokens_cached: u64, tokens_out: u64) -> u64 {
    tokens_in + tokens_out + (tokens_cached as f64 * CACHED_WEIGHT).round() as u64
}

impl Reply {
    pub fn billable(&self) -> u64 {
        billable(self.tokens_in, self.tokens_cached, self.tokens_out)
    }
}

pub trait Player: Send {
    /// Send one ball (or a return message) and get the answer.
    fn play(&mut self, ball: &str, ctx: &PlayCtx) -> Result<Reply, String>;
    /// True when the player remembers earlier rallies, so a return message need not repeat the brief.
    fn keeps_session(&self) -> bool;
    /// How the player talks to its tool (e.g. "persistent (codex app-server)"), for the
    /// table's events; empty when there is only one way.
    fn mode(&self) -> String {
        String::new()
    }
    /// Crash recovery: rebuild the session from the recorded rallies (ball, answer) and the
    /// tool's session id. Returns true when the player now remembers them.
    fn restore(&mut self, _history: &[(String, String)], _session: Option<&str>) -> bool {
        false
    }
}

pub fn build(spec: &PlayerSpec, _ctx: &PlayCtx) -> Result<Box<dyn Player>, String> {
    Ok(match spec {
        PlayerSpec::Shell {
            command,
            timeout_secs,
            env,
        } => Box::new(shell::ShellPlayer::new(
            command.clone(),
            *timeout_secs,
            env.clone(),
        )),
        PlayerSpec::Openai { .. } => Box::new(openai::OpenAiPlayer::new(spec.clone())?),
        PlayerSpec::Openunum { .. } => Box::new(openunum::OpenUnumPlayer::new(spec.clone())),
        PlayerSpec::Claude(a) => Box::new(cli::AgentPlayer::new(Flavor::Claude, a.clone())),
        PlayerSpec::Codex(a) => Box::new(cli::AgentPlayer::new(Flavor::Codex, a.clone())),
        PlayerSpec::Gemini(a) => Box::new(cli::AgentPlayer::new(Flavor::Gemini, a.clone())),
        PlayerSpec::Opencode(a) => Box::new(cli::AgentPlayer::new(Flavor::Opencode, a.clone())),
    })
}
