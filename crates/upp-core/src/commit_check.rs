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

/// A fresh detached worktree of `full` (a resolved commit) under the temp dir.
fn temp_worktree(repo: &Path, full: &str, kind: &str) -> Result<TempWorktree, String> {
    let dir = std::env::temp_dir().join("unumpingpong").join(format!(
        "{kind}-{}-{}",
        &full[..12.min(full.len())],
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(dir.parent().unwrap_or(Path::new(".")))
        .map_err(|e| format!("temp dir: {e}"))?;
    let path_s = dir.to_string_lossy().to_string();
    crate::git::git(
        repo,
        &["worktree", "add", "--detach", &path_s, full],
        &BTreeMap::new(),
    )?;
    Ok(TempWorktree {
        repo: repo.to_path_buf(),
        path: dir,
    })
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
    let wt = temp_worktree(repo, &full, "referee")?;
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

/// Options of the auto-fix step (`upp referee --fix N`).
pub struct FixOpts {
    /// At most this many rallies (the orchestrator rule: 2), then a human or orchestrator.
    pub rallies: u32,
    /// The player, as a `PlayerSpec` JSON object (e.g. `{"kind":"openunum"}`).
    pub player: Value,
    /// Wall budget of the whole fix table.
    pub wall: Duration,
    /// Where a scoring fix's patch is written.
    pub patch_dir: PathBuf,
}

/// Neutralise ball template tokens inside embedded text (only these two are expanded).
fn inert(text: &str) -> String {
    text.replace("{{file:", "{ {file:")
        .replace("{{search:", "{ {search:")
}

/// Referee `sha`; when it fails and `fix` is set, give a player at most `fix.rallies` rallies
/// in a separate temp worktree to make the same check pass. The commit's own verdict is never
/// changed (`point` stays false): a scoring fix becomes a patch file for a human or the
/// orchestrator to review and apply, and nothing is committed or shipped automatically.
pub fn check_and_fix(
    repo: &Path,
    sha: &str,
    cmd: &str,
    timeout: Duration,
    fix: Option<&FixOpts>,
) -> Result<Value, String> {
    let mut v = check_commit(repo, sha, cmd, timeout)?;
    let Some(fix) = fix else { return Ok(v) };
    if v["point"] == true || fix.rallies == 0 {
        return Ok(v);
    }
    v["fix"] = match run_fix(repo, &v, cmd, timeout, fix) {
        Ok(f) => f,
        Err(e) => json!({"point": false, "error": e, "escalate": true}),
    };
    Ok(v)
}

fn run_fix(
    repo: &Path,
    v: &Value,
    cmd: &str,
    timeout: Duration,
    fix: &FixOpts,
) -> Result<Value, String> {
    let full = v["sha"].as_str().unwrap_or_default().to_string();
    let wt = temp_worktree(repo, &full, "referee-fix")?;
    let ball = format!(
        "This commit fails its referee check. Make the check pass by fixing the code.\n\n\
         Working directory: {cwd}\nCheck command (run it yourself; it must exit 0): `{cmd}`\n\n\
         Current failure (head and tail):\n```\n{out}\n```\n\n\
         Rules: work only in the working directory. Do not commit, push or create branches. \
         Do not weaken or delete tests or the check to make it pass. If the check command itself \
         is broken (command or file not found), answer at once and say so. \
         End with what you changed and the check output.",
        cwd = wt.path.display(),
        out = inert(v["output"].as_str().unwrap_or_default()),
    );
    let short = &full[..12.min(full.len())];
    let spec = json!({
        "name": format!("referee-fix-{short}"),
        "goal": format!("Make the referee check pass for commit {short}"),
        "cwd": wt.path.to_string_lossy(),
        "ball": ball,
        "lease": {"wall_secs": fix.wall.as_secs().max(60), "max_rallies": fix.rallies},
        "player": fix.player,
        "referee": [{"name": "referee check", "kind": "command", "run": cmd,
                     "timeout_secs": timeout.as_secs().max(1)}],
    });
    let m = crate::config::MatchFile::from_json(spec)?;
    let db_dir = tempfile_dir(&full)?;
    let db = db_dir.join("fix.db");
    let store = crate::store::Store::open(&db)?;
    let created = crate::api::create(&store, m, Some("upp referee --fix"))?;
    let run_id = created["run_id"].as_str().unwrap_or_default().to_string();
    let mut sched = crate::scheduler::Scheduler::new(&db, 1, "referee-fix");
    sched.run = Some(run_id.clone());
    sched.until_idle = true;
    let ran = sched.run();
    let row = store
        .tables(Some(&run_id), false)?
        .into_iter()
        .next()
        .ok_or("fix table vanished")?;
    let _ = std::fs::remove_dir_all(&db_dir);
    ran?;
    let point = row.status == "point";
    let mut out = json!({
        "point": point,
        "rallies": row.rallies,
        "maxRallies": fix.rallies,
        "status": row.status,
        "reason": row.reason,
        "player": fix.player.get("kind").cloned().unwrap_or(Value::Null),
        "escalate": !point,
        "patch": Value::Null,
    });
    if point {
        let none = BTreeMap::new();
        crate::git::git(&wt.path, &["add", "-A"], &none)?;
        let stat = crate::git::git(&wt.path, &["diff", "--cached", "--stat", "HEAD"], &none)?;
        let patch = crate::git::git(&wt.path, &["diff", "--cached", "--binary", "HEAD"], &none)?;
        std::fs::create_dir_all(&fix.patch_dir).map_err(|e| format!("patch dir: {e}"))?;
        let file = fix.patch_dir.join(format!(
            "{short}-{}.patch",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        std::fs::write(&file, format!("{patch}\n")).map_err(|e| format!("write patch: {e}"))?;
        out["patch"] = json!(file.to_string_lossy());
        out["diffstat"] = json!(stat);
    }
    Ok(out)
}

fn tempfile_dir(full: &str) -> Result<PathBuf, String> {
    let d = std::env::temp_dir().join("unumpingpong").join(format!(
        "referee-fixdb-{}-{}",
        &full[..12.min(full.len())],
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&d).map_err(|e| format!("temp dir: {e}"))?;
    Ok(d)
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

    fn fix_opts(command: &str, rallies: u32, patch_dir: &Path) -> FixOpts {
        FixOpts {
            rallies,
            player: json!({"kind": "shell", "command": command}),
            wall: Duration::from_secs(60),
            patch_dir: patch_dir.to_path_buf(),
        }
    }

    #[test]
    fn a_scoring_fix_becomes_a_patch_and_the_commit_verdict_stays_failed() {
        let d = repo();
        let patches = tempfile::tempdir().unwrap();
        // The check wants 9; a scripted player writes it.
        let opts = fix_opts("printf '9\\n' > v.txt; echo fixed v.txt", 2, patches.path());
        let v = check_and_fix(
            d.path(),
            "HEAD",
            "grep -qx 9 v.txt",
            Duration::from_secs(20),
            Some(&opts),
        )
        .unwrap();
        assert_eq!(v["point"], false, "the commit itself still fails: {v}");
        assert_eq!(v["fix"]["point"], true, "{v}");
        assert_eq!(v["fix"]["rallies"], 1);
        assert_eq!(v["fix"]["escalate"], false);
        let patch = std::fs::read_to_string(v["fix"]["patch"].as_str().unwrap()).unwrap();
        assert!(patch.contains("+9"), "{patch}");
        // Nothing was committed or changed in the real repository.
        assert_eq!(
            std::fs::read_to_string(d.path().join("v.txt")).unwrap(),
            "2\n"
        );
        assert_eq!(worktrees(d.path()), 1);
    }

    #[test]
    fn a_failed_fix_stops_at_the_rally_cap_and_escalates() {
        let d = repo();
        let patches = tempfile::tempdir().unwrap();
        let opts = fix_opts("echo I tried", 2, patches.path());
        let v = check_and_fix(
            d.path(),
            "HEAD",
            "grep -qx 9 v.txt",
            Duration::from_secs(20),
            Some(&opts),
        )
        .unwrap();
        assert_eq!(v["point"], false);
        assert_eq!(v["fix"]["point"], false, "{v}");
        assert_eq!(v["fix"]["rallies"], 2, "{v}");
        assert_eq!(v["fix"]["escalate"], true);
        assert!(v["fix"]["patch"].is_null());
        assert_eq!(std::fs::read_dir(patches.path()).unwrap().count(), 0);
        assert_eq!(worktrees(d.path()), 1);
    }

    #[test]
    fn a_passing_commit_runs_no_fix() {
        let d = repo();
        let patches = tempfile::tempdir().unwrap();
        let opts = fix_opts("echo should not run > ran.txt", 2, patches.path());
        let v = check_and_fix(
            d.path(),
            "HEAD",
            "grep -qx 2 v.txt",
            Duration::from_secs(20),
            Some(&opts),
        )
        .unwrap();
        assert_eq!(v["point"], true);
        assert!(v.get("fix").is_none(), "{v}");
    }

    #[test]
    fn template_tokens_in_failure_output_are_inert() {
        assert_eq!(
            inert("a {{file:/etc/passwd}} b {{search:x}}"),
            "a { {file:/etc/passwd}} b { {search:x}}"
        );
    }
}
