// SPDX-License-Identifier: Apache-2.0
//! Subprocess helper: stdin feed, captured output (bounded), timeout with
//! process-group kill on Unix so `sh -c` children die too.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Captured output is kept to the last `MAX_CAPTURE` bytes per stream.
pub const MAX_CAPTURE: usize = 4 << 20;

#[derive(Debug, Clone)]
pub struct CmdOut {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    /// Killed because the cancel flag was set (the table was stopped).
    pub cancelled: bool,
    pub duration_ms: u64,
}

impl CmdOut {
    pub fn ok(&self) -> bool {
        self.code == Some(0) && !self.timed_out && !self.cancelled
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
    run_cancellable(argv, cwd, stdin, env, timeout, None)
}

/// Like [`run`], and also kills the whole process group as soon as `cancel` is set.
pub fn run_cancellable(
    argv: &[String],
    cwd: &Path,
    stdin: Option<&str>,
    env: &BTreeMap<String, String>,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
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
    let mut cancelled = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
                    cancelled = true;
                    kill_tree(&mut child);
                    break child.wait().ok();
                }
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
        code: if timed_out || cancelled {
            None
        } else {
            status.and_then(|s| s.code())
        },
        stdout,
        stderr,
        timed_out,
        cancelled,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

/// What [`LiveProc::recv`] got.
#[derive(Debug, PartialEq, Eq)]
pub enum Recv {
    Line(String),
    /// stdout closed (the process ended or closed it).
    Eof,
    TimedOut,
    Cancelled,
}

/// A long-lived child (an agent CLI in a streaming mode): lines in on stdin, lines out on
/// stdout, stderr kept (bounded) for error messages. It leads its own process group, which
/// is killed when the value is dropped.
pub struct LiveProc {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    lines: std::sync::mpsc::Receiver<String>,
    stderr: std::sync::Arc<std::sync::Mutex<String>>,
}

impl LiveProc {
    pub fn spawn(
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<LiveProc, String> {
        use std::io::BufRead;
        let (prog, args) = argv.split_first().ok_or("empty command")?;
        let mut cmd = Command::new(prog);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
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
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot start {prog:?} in {}: {e}", cwd.display()))?;
        let stdin = child.stdin.take();
        let out = child.stdout.take().expect("stdout piped");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let stderr = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let (mut err, sink) = (child.stderr.take().expect("stderr piped"), stderr.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut s = sink.lock().unwrap_or_else(|p| p.into_inner());
                s.push_str(&String::from_utf8_lossy(&buf[..n]));
                if s.len() > 64 << 10 {
                    *s = tail(&s, 32 << 10);
                }
            }
        });
        Ok(LiveProc {
            child,
            stdin,
            lines: rx,
            stderr,
        })
    }

    /// Write one line (a newline is added).
    pub fn send(&mut self, line: &str) -> Result<(), String> {
        let pipe = self.stdin.as_mut().ok_or("stdin closed")?;
        pipe.write_all(line.as_bytes())
            .and_then(|_| pipe.write_all(b"\n"))
            .and_then(|_| pipe.flush())
            .map_err(|e| format!("cannot write to the player process: {e}"))
    }

    /// Next stdout line, waiting until `deadline` or until `cancel` is set.
    pub fn recv(&mut self, deadline: Instant, cancel: Option<&AtomicBool>) -> Recv {
        loop {
            if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
                return Recv::Cancelled;
            }
            let now = Instant::now();
            if now >= deadline {
                return Recv::TimedOut;
            }
            let wait = (deadline - now).min(Duration::from_millis(100));
            match self.lines.recv_timeout(wait) {
                Ok(l) => return Recv::Line(l),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Recv::Eof,
            }
        }
    }

    /// The last `max` bytes of what the process wrote to stderr.
    pub fn stderr_tail(&self, max: usize) -> String {
        let s = self.stderr.lock().unwrap_or_else(|p| p.into_inner());
        tail(s.trim(), max)
    }

    /// Exit code if the process has ended.
    pub fn exited(&mut self) -> Option<Option<i32>> {
        self.child.try_wait().ok().flatten().map(|s| s.code())
    }
}

impl Drop for LiveProc {
    fn drop(&mut self) {
        self.stdin.take();
        if self.child.try_wait().ok().flatten().is_none() {
            kill_tree(&mut self.child);
        }
        let _ = self.child.wait();
    }
}

/// The first `head` and last `tail` bytes of `s` (char boundaries), with the cut marked:
/// a failing test's first error and its summary both survive.
pub fn head_tail(s: &str, head_max: usize, tail_max: usize) -> String {
    if s.len() <= head_max + tail_max + 40 {
        return s.to_string();
    }
    let mut end = head_max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut start = s.len() - tail_max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "{}\n[... {} bytes cut ...]\n{}",
        &s[..end],
        start - end,
        &s[start..]
    )
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
    fn cancel_kills_the_group_at_once() {
        let flag = std::sync::Arc::new(AtomicBool::new(false));
        let f2 = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            f2.store(true, Ordering::SeqCst);
        });
        let t = Instant::now();
        let o = run_cancellable(
            &sh("sleep 30 & sleep 30"),
            Path::new("."),
            None,
            &BTreeMap::new(),
            Duration::from_secs(60),
            Some(&flag),
        )
        .unwrap();
        assert!(o.cancelled && !o.timed_out && !o.ok());
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    }

    #[test]
    fn tail_and_head_respect_char_boundaries() {
        let s = "αβγδε";
        assert!(tail(s, 3).ends_with('ε'));
        assert!(head(s, 3).starts_with('α'));
        let long = format!("FIRST{}LAST", "x".repeat(5000));
        let ht = head_tail(&long, 100, 100);
        assert!(ht.starts_with("FIRST") && ht.ends_with("LAST") && ht.len() < 300);
        assert_eq!(head_tail("short", 10, 10), "short");
    }

    #[test]
    fn live_process_talks_in_lines_and_dies_with_its_value() {
        let mut p = LiveProc::spawn(
            &sh("while read l; do echo \"got $l\"; done"),
            Path::new("."),
            &BTreeMap::new(),
        )
        .unwrap();
        let far = Instant::now() + Duration::from_secs(10);
        for w in ["a", "b"] {
            p.send(w).unwrap();
            assert_eq!(p.recv(far, None), Recv::Line(format!("got {w}")));
        }
        let soon = Instant::now() + Duration::from_millis(200);
        assert_eq!(p.recv(soon, None), Recv::TimedOut);
        let flag = AtomicBool::new(true);
        assert_eq!(p.recv(far, Some(&flag)), Recv::Cancelled);
        let mut q =
            LiveProc::spawn(&sh("echo one; exit 0"), Path::new("."), &BTreeMap::new()).unwrap();
        assert_eq!(q.recv(far, None), Recv::Line("one".into()));
        assert_eq!(q.recv(far, None), Recv::Eof);
    }
}
