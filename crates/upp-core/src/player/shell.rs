// SPDX-License-Identifier: Apache-2.0
//! `shell` player: any command. The ball goes to stdin, the answer comes from stdout.
//! Environment: UPP_TABLE, UPP_RALLY, UPP_SESSION (stable per table), UPP_CWD.

use super::{PlayCtx, Player, Reply};
use crate::config::Cmd;
use crate::proc;
use std::collections::BTreeMap;

pub struct ShellPlayer {
    command: Cmd,
    timeout_secs: Option<u64>,
    env: BTreeMap<String, String>,
}

impl ShellPlayer {
    pub fn new(command: Cmd, timeout_secs: Option<u64>, env: BTreeMap<String, String>) -> Self {
        ShellPlayer {
            command,
            timeout_secs,
            env,
        }
    }
}

impl Player for ShellPlayer {
    fn play(&mut self, ball: &str, ctx: &PlayCtx) -> Result<Reply, String> {
        let mut env = ctx.env_with(&self.env);
        env.insert("UPP_SESSION".into(), ctx.session_key.clone());
        env.insert("UPP_CWD".into(), ctx.cwd.to_string_lossy().into_owned());
        if let Some(e) = &ctx.effort {
            env.entry("UPP_EFFORT".into()).or_insert_with(|| e.clone());
        }
        let out = proc::run_cancellable(
            &self.command.argv(),
            &ctx.cwd,
            Some(ball),
            &env,
            ctx.budget(self.timeout_secs, 900),
            Some(&ctx.cancel),
        )?;
        if out.cancelled {
            return Err("player call cancelled (table stopped)".into());
        }
        if out.timed_out {
            return Err(format!(
                "player timed out after {:.1}s",
                out.duration_ms as f64 / 1000.0
            ));
        }
        if !out.ok() {
            let code = out.code.map(|c| c.to_string()).unwrap_or("a signal".into());
            return Err(format!(
                "player exited with {code}: {}",
                proc::tail(out.stderr.trim(), 2000)
            ));
        }
        Ok(Reply {
            text: out.stdout,
            session: Some(ctx.session_key.clone()),
            ..Default::default()
        })
    }

    fn keeps_session(&self) -> bool {
        false
    }
}
