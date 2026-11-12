// SPDX-License-Identifier: Apache-2.0
//! Referee a commit: run a deterministic check command against a commit in a throwaway
//! detached worktree and return a versioned JSON verdict. No model is involved.
//!
//! Used by `upp referee` and by ship pipelines (unumship's referee hook) so that "done" is
//! backed by evidence: a queue entry carries the verdict of its own commit, not a claim.
//! The check runs on the committed tree only: uncommitted files in the author's worktree
//! cannot make it pass.

use crate::config::Cmd;
use crate::proc;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Version of the verdict JSON (bump on a breaking change of its fields).
pub const VERDICT_VERSION: u32 = 1;
/// Output kept in the verdict: the head (first error) and the tail (summary) of the log.
const OUTPUT_HEAD: usize = 1000;
const OUTPUT_TAIL: usize = 1000;

/// A detached worktree that is removed when dropped, even on an early return.
struct TempWorktree {
    repo: PathBuf,
    path: PathBuf,
}

impl Drop for TempWorktree {
    fn drop(&mut self) {
        let p = self.path.to_string_lossy().to_string();
        if crate::git::git(
            &self.repo,
            &["worktree", "remove", "--force", &p],
            &BTreeMap::new(),
        )
        .is_err()
        {
            let _ = std::fs::remove_dir_all(&self.path);
            let _ = crate::git::git(&self.repo, &["worktree", "prune"], &BTreeMap::new());
        }
    }
}

fn now_iso() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    crate::compat::iso(ms)
}

/// Run `cmd` (a shell line) in a fresh detached worktree of `sha` in `repo`, with `timeout`.
/// `Ok(verdict)` whenever the check ran (point or not); `Err` only when it could not run
/// (not a repository, unknown commit, worktree failure).
pub fn check_commit(repo: &Path, sha: &str, cmd: &str, timeout: Duration) -> Result<Value, String> {
    if cmd.trim().is_empty() {
        return Err("empty check command".into());
    }
    let none = BTreeMap::new();
    let full = crate::git::git(
        repo,
        &["rev-parse", "--verify", &format!("{sha}^{{commit}}")],
        &none,
    )
    .map_err(|e| format!("unknown commit {sha}: {e}"))?;
    let dir = std::env::temp_dir().join("unumpingpong").join(format!(
        "referee-{}-{}",
        &full[..12.min(full.len())],
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(dir.parent().unwrap_or(Path::new(".")))
        .map_err(|e| format!("temp dir: {e}"))?;
    let path_s = dir.to_string_lossy().to_string();
    crate::git::git(
        repo,
        &["worktree", "add", "--detach", &path_s, &full],
        &none,
    )?;
    let wt = TempWorktree {
        repo: repo.to_path_buf(),
        path: dir,
    };
    let argv = Cmd::Line(cmd.to_string()).argv();
    let out = proc::run(&argv, &wt.path, None, &none, timeout)?;
    let point = out.ok();
    Ok(json!({
        "version": VERDICT_VERSION,
        "mode": "check",
        "point": point,
        "sha": full,
        "cmd": cmd,
        "exit": out.code,
        "timedOut": out.timed_out,
        "durationMs": out.duration_ms,
        "output": proc::head_tail(out.combined().trim(), OUTPUT_HEAD, OUTPUT_TAIL),
        "ranAt": now_iso(),
        "engine": format!("upp {}", crate::VERSION),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .args(args)
                .current_dir(d.path())
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
                .status()
                .unwrap()
                .success())
        };
        git(&["init", "-q"]);
        std::fs::write(d.path().join("v.txt"), "1\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "one"]);
        std::fs::write(d.path().join("v.txt"), "2\n").unwrap();
        git(&["commit", "-qam", "two"]);
        d
    }

    fn worktrees(d: &Path) -> usize {
        crate::git::git(d, &["worktree", "list"], &BTreeMap::new())
            .unwrap()
            .lines()
            .count()
    }

    #[test]
    fn point_for_a_passing_commit_and_the_worktree_is_removed() {
        let d = repo();
        let v = check_commit(
            d.path(),
            "HEAD",
            "grep -qx 2 v.txt",
            Duration::from_secs(20),
        )
        .unwrap();
        assert_eq!(v["point"], true, "{v}");
        assert_eq!(v["exit"], 0);
        assert_eq!(v["version"], 1);
        assert_eq!(v["sha"].as_str().unwrap().len(), 40);
        assert_eq!(worktrees(d.path()), 1);
    }

    #[test]
    fn the_check_sees_the_commit_not_the_working_tree() {
        let d = repo();
        // Uncommitted change in the author's tree must not count.
        std::fs::write(d.path().join("v.txt"), "3\n").unwrap();
        let v = check_commit(
            d.path(),
            "HEAD",
            "grep -qx 3 v.txt",
            Duration::from_secs(20),
        )
        .unwrap();
        assert_eq!(v["point"], false, "{v}");
        let older = check_commit(
            d.path(),
            "HEAD~1",
            "grep -qx 1 v.txt",
            Duration::from_secs(20),
        )
        .unwrap();
        assert_eq!(older["point"], true, "{older}");
    }

    #[test]
    fn failing_and_timed_out_checks_carry_their_evidence() {
        let d = repo();
        let v = check_commit(
            d.path(),
            "HEAD",
            "echo first-error; exit 3",
            Duration::from_secs(20),
        )
        .unwrap();
        assert_eq!(v["point"], false);
        assert_eq!(v["exit"], 3);
        assert!(v["output"].as_str().unwrap().contains("first-error"));
        let t = check_commit(d.path(), "HEAD", "sleep 30", Duration::from_millis(300)).unwrap();
        assert_eq!(t["point"], false);
        assert_eq!(t["timedOut"], true);
        assert_eq!(worktrees(d.path()), 1);
    }

    #[test]
    fn errors_when_it_cannot_run() {
        let d = repo();
        assert!(check_commit(d.path(), "deadbeef", "true", Duration::from_secs(5)).is_err());
        assert!(check_commit(d.path(), "HEAD", "  ", Duration::from_secs(5)).is_err());
        let plain = tempfile::tempdir().unwrap();
        assert!(check_commit(plain.path(), "HEAD", "true", Duration::from_secs(5)).is_err());
    }
}
