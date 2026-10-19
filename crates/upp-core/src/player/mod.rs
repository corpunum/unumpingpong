// SPDX-License-Identifier: Apache-2.0
//! Players: every coding/agent tool behind one trait. Each player is built once
//! per table and keeps its session across rallies where the tool supports it.

mod cli;
mod openai;
mod openunum;
mod shell;

use crate::config::PlayerSpec;
use serde::Serialize;
use std::path::PathBuf;
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
}

impl PlayCtx {
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
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_usd: f64,
    /// The tool's own session/thread id, when it reports one.
    pub session: Option<String>,
}

pub trait Player: Send {
    /// Send one ball (or a return message) and get the answer.
    fn play(&mut self, ball: &str, ctx: &PlayCtx) -> Result<Reply, String>;
    /// True when the player remembers earlier rallies, so a return message need not repeat the brief.
    fn keeps_session(&self) -> bool;
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
