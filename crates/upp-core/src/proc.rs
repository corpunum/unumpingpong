// SPDX-License-Identifier: Apache-2.0
//! Subprocess helper: stdin feed, captured output (bounded), timeout with
//! process-group kill on Unix so `sh -c` children die too.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Captured output is kept to the last `MAX_CAPTURE` bytes per stream.
pub const MAX_CAPTURE: usize = 4 << 20;

#[derive(Debug, Clone)]
pub struct CmdOut {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub duration_ms: u64,
}

impl CmdOut {
    pub fn ok(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }
    pub fn combined(&self) -> String {
        if self.stderr.trim().is_empty() {
            self.stdout.clone()
        } else if self.stdout.trim().is_empty() {
            self.stderr.clone()
        } else {
            format!("{}\n{}", self.stdout, self.stderr)
        }
    }
}

fn drain<R: Read + Send + 'static>(mut r: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut buf = [0u8; 16384];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    out.extend_from_slice(&buf[..n]);
                    if out.len() > MAX_CAPTURE * 2 {
                        let cut = out.len() - MAX_CAPTURE;
                        out.drain(..cut);
                    }
                }
            }
        }
        if out.len() > MAX_CAPTURE {
            let cut = out.len() - MAX_CAPTURE;
            out.drain(..cut);
        }
        out
    })
}

#[cfg(unix)]
fn kill_tree(child: &mut std::process::Child) {
    // The child leads its own process group (see `process_group(0)` below).
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{}", child.id())])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
}

#[cfg(not(unix))]
fn kill_tree(child: &mut std::process::Child) {
    let _ = child.kill();
}

/// Run `argv` in `cwd` with `stdin` fed to it, killing it after `timeout`.
pub fn run(
    argv: &[String],
    cwd: &Path,
    stdin: Option<&str>,
    env: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<CmdOut, String> {
    let (prog, args) = argv.split_first().ok_or("empty command")?;
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {prog:?} in {}: {e}", cwd.display()))?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let input = input.to_string();
        std::thread::spawn(move || {
            let _ = pipe.write_all(input.as_bytes());
        });
    }
    let out_h = drain(child.stdout.take().expect("stdout piped"));
    let err_h = drain(child.stderr.take().expect("stderr piped"));
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    timed_out = true;
                    kill_tree(&mut child);
                    break child.wait().ok();
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("wait failed: {e}")),
        }
    };
    let stdout = String::from_utf8_lossy(&out_h.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_h.join().unwrap_or_default()).into_owned();
    Ok(CmdOut {
        code: if timed_out {
            None
        } else {
            status.and_then(|s| s.code())
        },
        stdout,
        stderr,
        timed_out,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

/// Keep the last `max` bytes of `s` (on a char boundary), marking the cut.
pub fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("[... {} bytes cut ...]\n{}", start, &s[start..])
}

/// Keep the first `max` bytes of `s` (on a char boundary).
pub fn head(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} [... cut]", &s[..end])
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(s: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), s.into()]
    }

    #[test]
    fn captures_and_feeds_stdin() {
        let o = run(
            &sh("tr a-z A-Z; echo err >&2; exit 3"),
            Path::new("."),
            Some("hello"),
            &BTreeMap::new(),
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(o.stdout.trim(), "HELLO");
        assert_eq!(o.stderr.trim(), "err");
        assert_eq!(o.code, Some(3));
        assert!(!o.ok());
    }

    #[test]
    fn timeout_kills_the_group() {
        let t = Instant::now();
        let o = run(
            &sh("sleep 30 & sleep 30; echo never"),
            Path::new("."),
            None,
            &BTreeMap::new(),
            Duration::from_millis(300),
        )
        .unwrap();
        assert!(o.timed_out);
        assert!(!o.stdout.contains("never"));
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "took {:?}",
            t.elapsed()
        );
    }

    #[test]
    fn tail_and_head_respect_char_boundaries() {
        let s = "αβγδε";
        assert!(tail(s, 3).ends_with('ε'));
        assert!(head(s, 3).starts_with('α'));
    }
}
