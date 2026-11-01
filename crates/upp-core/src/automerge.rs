// SPDX-License-Identifier: Apache-2.0
//! Auto-merge: when a table with `auto_merge` scores and its referee passed, land the
//! verified point on the target branch, under fixed safety rules:
//!
//! 1. the referee must have passed on the scoring rally (an unverified point, or one the
//!    server accepted despite a failing referee, is never merged);
//! 2. the target repository must be clean (no tracked or untracked changes, no merge or
//!    rebase in progress) and have the target branch checked out;
//! 3. the merge is a fast-forward, or a conflict-free rebase of the source onto the target
//!    (done in a scratch worktree, then fast-forwarded in): no merge commits, no conflict
//!    resolution;
//! 4. the repository's test command runs after the merge; on failure the target branch is
//!    reset to where it was and nothing is pushed;
//! 5. the push is a plain `git push` (never forced); if the remote refuses it, the local
//!    merge is rolled back too.
//!
//! Every decision, merged or not, is a row in the `merge_log` table with its reason.

use crate::config::{AutoMerge, Cmd};
use crate::git::{self, upp_ident};
use crate::proc;
use crate::store::Store;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A resolved merge request.
#[derive(Debug, Clone)]
pub struct Plan {
    pub repo: PathBuf,
    pub target: String,
    pub source: String,
    pub test: Cmd,
    pub push: bool,
    pub remote: String,
    pub test_timeout: Duration,
}

impl Plan {
    pub fn from_spec(am: &AutoMerge, repo: PathBuf, source: String) -> Plan {
        Plan {
            repo,
            target: am.target.clone().unwrap_or_else(|| "main".into()),
            source,
            test: am.test.clone(),
            push: am.push.unwrap_or(true),
            remote: am.remote.clone().unwrap_or_else(|| "origin".into()),
            test_timeout: Duration::from_secs(am.test_timeout_secs.unwrap_or(1800)),
        }
    }
}

/// What happened. `decision` is one of: `merged_ff`, `merged_rebase`, `already_merged`,
/// `skipped_referee`, `skipped_config`, `skipped_dirty`, `skipped_wrong_branch`,
/// `skipped_conflict`, `skipped_tests_failed`, `skipped_push_failed`, `skipped_error`.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub decision: &'static str,
    pub reason: String,
    pub details: Value,
}

impl Outcome {
    pub fn merged(&self) -> bool {
        self.decision.starts_with("merged")
    }
}

fn out(decision: &'static str, reason: impl Into<String>, details: Value) -> Outcome {
    Outcome {
        decision,
        reason: reason.into(),
        details,
    }
}

fn g(dir: &Path, args: &[&str]) -> Result<String, String> {
    git::git(dir, args, &upp_ident())
}

fn is_ancestor(dir: &Path, a: &str, b: &str) -> bool {
    g(dir, &["merge-base", "--is-ancestor", a, b]).is_ok()
}

/// Merge `plan.source` into `plan.target` of `plan.repo` under the safety rules.
pub fn merge(plan: &Plan) -> Outcome {
    let top = match git::toplevel(&plan.repo) {
        Some(t) => t,
        None => {
            return out(
                "skipped_error",
                format!("{} is not a git repository", plan.repo.display()),
                json!({}),
            )
        }
    };
    let base =
        json!({"repo": top.display().to_string(), "target": plan.target, "source": plan.source});
    let with = |extra: Value| -> Value {
        let mut b = base.clone();
        if let (Some(bo), Some(eo)) = (b.as_object_mut(), extra.as_object()) {
            for (k, v) in eo {
                bo.insert(k.clone(), v.clone());
            }
        }
        b
    };
    // Rule 2: clean repository, nothing in progress, target checked out.
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
    ] {
        if let Ok(p) = g(&top, &["rev-parse", "--git-path", marker]) {
            let p = PathBuf::from(p);
            let p = if p.is_absolute() { p } else { top.join(p) };
            if p.exists() {
                return out(
                    "skipped_dirty",
                    format!("target repository has an operation in progress ({marker})"),
                    base,
                );
            }
        }
    }
    match g(&top, &["status", "--porcelain", "--untracked-files=normal"]) {
        Ok(s) if s.is_empty() => {}
        Ok(s) => {
            return out(
                "skipped_dirty",
                "target repository has uncommitted changes; refusing to merge into it",
                with(json!({"status": proc::head(&s, 600)})),
            )
        }
        Err(e) => return out("skipped_error", e, base),
    }
    let branch = g(&top, &["symbolic-ref", "-q", "--short", "HEAD"]).unwrap_or_default();
    if branch != plan.target {
        return out(
            "skipped_wrong_branch",
            format!(
                "target branch '{}' is not checked out in the repository (HEAD is '{}')",
                plan.target,
                if branch.is_empty() {
                    "detached"
                } else {
                    &branch
                }
            ),
            base,
        );
    }
    let before = match g(&top, &["rev-parse", "HEAD"]) {
        Ok(h) => h,
        Err(e) => return out("skipped_error", e, base),
    };
    let src = match g(
        &top,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", plan.source),
        ],
    ) {
        Ok(h) => h,
        Err(e) => return out("skipped_error", format!("source not found: {e}"), base),
    };
    if is_ancestor(&top, &src, &before) {
        return out(
            "already_merged",
            "the source is already contained in the target branch",
            with(json!({"head": before, "source_commit": src})),
        );
    }
    // Rule 3: fast-forward, else a conflict-free rebase.
    let kind = if is_ancestor(&top, &before, &src) {
        if let Err(e) = g(&top, &["merge", "--ff-only", "-q", &src]) {
            return out("skipped_error", e, base);
        }
        "ff"
    } else {
        let scratch =
            std::env::temp_dir().join(format!("upp-merge-{}", uuid::Uuid::new_v4().simple()));
        let scratch_s = scratch.to_string_lossy().to_string();
        if let Err(e) = g(
            &top,
            &["worktree", "add", "-q", "--detach", &scratch_s, &src],
        ) {
            return out("skipped_error", e, base);
        }
        let rebased = g(&scratch, &["rebase", "-q", &before]);
        let result = match rebased {
            Ok(_) => g(&scratch, &["rev-parse", "HEAD"]),
            Err(e) => {
                let files =
                    g(&scratch, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
                let _ = g(&scratch, &["rebase", "--abort"]);
                let _ = g(&top, &["worktree", "remove", "--force", &scratch_s]);
                return out(
                    "skipped_conflict",
                    "the source does not rebase onto the target without conflicts",
                    with(json!({
                        "conflicts": files.lines().collect::<Vec<_>>(),
                        "error": proc::head(&e, 400),
                    })),
                );
            }
        };
        let _ = g(&top, &["worktree", "remove", "--force", &scratch_s]);
        let rebased_head = match result {
            Ok(h) => h,
            Err(e) => return out("skipped_error", e, base),
        };
        if let Err(e) = g(&top, &["merge", "--ff-only", "-q", &rebased_head]) {
            return out("skipped_error", e, base);
        }
        "rebase"
    };
    let after = g(&top, &["rev-parse", "HEAD"]).unwrap_or_default();
    let rollback = |why: &str| {
        if let Err(e) = g(&top, &["reset", "--hard", "-q", &before]) {
            return format!("{why}; ROLLBACK FAILED: {e}");
        }
        format!("{why}; target branch reset to {before}")
    };
    // Rule 4: the repository's tests.
    let argv = plan.test.argv();
    let t = proc::run(&argv, &top, None, &BTreeMap::new(), plan.test_timeout);
    let tail = match &t {
        Ok(o) => proc::tail(o.combined().trim(), 800),
        Err(e) => e.clone(),
    };
    let passed = t.as_ref().is_ok_and(|o| o.ok());
    if !passed {
        let why = match &t {
            Ok(o) if o.timed_out => "tests timed out".to_string(),
            Ok(o) => format!("tests failed (exit {:?})", o.code),
            Err(e) => format!("tests could not run: {e}"),
        };
        let reason = rollback(&why);
        return out(
            "skipped_tests_failed",
            reason,
            with(json!({"kind": kind, "merged_head": after, "test_output": tail})),
        );
    }
    // Rule 5: plain push, only after the tests passed.
    let mut pushed = false;
    let mut push_note = "push disabled".to_string();
    if plan.push {
        if g(&top, &["remote", "get-url", &plan.remote]).is_err() {
            push_note = format!("no remote '{}': merged locally only", plan.remote);
        } else {
            let mut env = upp_ident();
            env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
            let refspec = format!("HEAD:refs/heads/{}", plan.target);
            match git::git(&top, &["push", "-q", &plan.remote, &refspec], &env) {
                Ok(_) => {
                    pushed = true;
                    push_note = format!("pushed to {}", plan.remote);
                }
                Err(e) => {
                    let reason = rollback(&format!("push refused: {}", proc::head(&e, 300)));
                    return out(
                        "skipped_push_failed",
                        reason,
                        with(json!({"kind": kind, "merged_head": after, "test_output": tail})),
                    );
                }
            }
        }
    }
    out(
        if kind == "ff" {
            "merged_ff"
        } else {
            "merged_rebase"
        },
        format!("merged by {kind}, tests passed, {push_note}"),
        with(json!({"kind": kind, "before": before, "after": after,
                    "pushed": pushed, "test_output": proc::tail(&tail, 300)})),
    )
}

/// Hook run after a table ended: decides (and logs) what happens to its point.
/// Idempotent: a table with a logged decision is not looked at again.
pub fn after_table(store: &Store, id: &str) -> Result<(), String> {
    let spec = store.spec(id)?;
    let Some(am) = spec.auto_merge.clone() else {
        return Ok(());
    };
    if !store.merge_log(id)?.is_empty() {
        return Ok(());
    }
    let t = store.table(id)?.ok_or("table vanished")?;
    if !t.is_terminal() {
        return Ok(());
    }
    let log = |o: &Outcome| store.log_merge(id, o.decision, &o.reason, &o.details);
    if t.status != "point" {
        return log(&out(
            "skipped_referee",
            format!(
                "table ended as '{}' ({}): no point, nothing to merge",
                t.status,
                t.reason.clone().unwrap_or_default()
            ),
            json!({}),
        ));
    }
    let last = store.rallies(id)?.pop();
    if !last
        .as_ref()
        .is_some_and(|r| r.passed && r.verdict == "point")
    {
        return log(&out(
            "skipped_referee",
            "the point was not backed by a passing referee (accepted by the server or unverified)",
            json!({}),
        ));
    }
    let repo = am
        .repo
        .clone()
        .or_else(|| {
            t.worktree
                .as_ref()
                .and_then(|w| w["repo"].as_str().map(String::from))
        })
        .map(PathBuf::from);
    let source = am.source.clone().or_else(|| {
        t.worktree
            .as_ref()
            .and_then(|w| w["branch"].as_str().map(String::from))
    });
    let (Some(repo), Some(source)) = (repo, source) else {
        return log(&out(
            "skipped_config",
            "auto_merge needs a repo and a source: set them, or give the table a worktree",
            json!({}),
        ));
    };
    let repo = if repo.is_relative() {
        spec.cwd
            .as_deref()
            .map(|c| Path::new(c).join(&repo))
            .unwrap_or(repo)
    } else {
        repo
    };
    log(&merge(&Plan::from_spec(&am, repo, source)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    pub fn sh(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .args(args)
            .current_dir(dir)
            .envs(upp_ident())
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// A bare `origin`, a clone on `main` with one commit, pushed.
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let origin = d.path().join("origin.git");
        let repo = d.path().join("repo");
        std::fs::create_dir_all(&origin).unwrap();
        sh(&origin, &["init", "-q", "--bare", "-b", "main"]);
        std::fs::create_dir_all(&repo).unwrap();
        sh(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        std::fs::write(repo.join("b.txt"), "b\n").unwrap();
        sh(&repo, &["add", "."]);
        sh(&repo, &["commit", "-q", "-m", "base"]);
        sh(
            &repo,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        sh(&repo, &["push", "-q", "origin", "main"]);
        (d, repo, origin)
    }

    fn commit_on(repo: &Path, branch: &str, from: &str, file: &str, content: &str) {
        sh(repo, &["checkout", "-q", "-B", branch, from]);
        std::fs::write(repo.join(file), content).unwrap();
        sh(repo, &["add", "."]);
        sh(
            repo,
            &["commit", "-q", "-m", &format!("{file} on {branch}")],
        );
        sh(repo, &["checkout", "-q", "main"]);
    }

    fn plan(repo: &Path, test: &str) -> Plan {
        Plan {
            repo: repo.to_path_buf(),
            target: "main".into(),
            source: "work".into(),
            test: Cmd::Line(test.into()),
            push: true,
            remote: "origin".into(),
            test_timeout: Duration::from_secs(60),
        }
    }

    #[test]
    fn fast_forward_merges_and_pushes() {
        let (_d, repo, origin) = fixture();
        commit_on(&repo, "work", "main", "c.txt", "c\n");
        let o = merge(&plan(&repo, "test -f c.txt"));
        assert_eq!(o.decision, "merged_ff", "{o:?}");
        assert!(o.merged() && o.details["pushed"] == true);
        assert_eq!(
            sh(&repo, &["rev-parse", "main"]),
            sh(&origin, &["rev-parse", "main"])
        );
        assert_eq!(
            sh(&repo, &["rev-parse", "main"]),
            sh(&repo, &["rev-parse", "work"])
        );
    }

    #[test]
    fn diverged_branch_is_rebased_then_merged() {
        let (_d, repo, origin) = fixture();
        commit_on(&repo, "work", "main", "c.txt", "c\n");
        std::fs::write(repo.join("b.txt"), "b2\n").unwrap();
        sh(&repo, &["commit", "-q", "-am", "main moves on"]);
        sh(&repo, &["push", "-q", "origin", "main"]);
        let work_before = sh(&repo, &["rev-parse", "work"]);
        let o = merge(&plan(&repo, "test -f c.txt && grep -q b2 b.txt"));
        assert_eq!(o.decision, "merged_rebase", "{o:?}");
        assert_eq!(
            sh(&repo, &["rev-parse", "main"]),
            sh(&origin, &["rev-parse", "main"])
        );
        // Linear history: the worker commit sits on top of main's own commit.
        assert_eq!(sh(&repo, &["rev-list", "--merges", "--count", "main"]), "0");
        assert_eq!(
            sh(&repo, &["log", "-1", "--format=%s", "main"]),
            "c.txt on work"
        );
        // The worker's branch itself is untouched.
        assert_eq!(sh(&repo, &["rev-parse", "work"]), work_before);
        // No scratch worktree is left behind.
        assert_eq!(sh(&repo, &["worktree", "list"]).lines().count(), 1);
    }

    #[test]
    fn conflict_means_no_merge_and_no_push() {
        let (_d, repo, origin) = fixture();
        commit_on(&repo, "work", "main", "a.txt", "worker\n");
        std::fs::write(repo.join("a.txt"), "main\n").unwrap();
        sh(&repo, &["commit", "-q", "-am", "main edits a"]);
        sh(&repo, &["push", "-q", "origin", "main"]);
        let head = sh(&repo, &["rev-parse", "main"]);
        let o = merge(&plan(&repo, "true"));
        assert_eq!(o.decision, "skipped_conflict", "{o:?}");
        assert_eq!(o.details["conflicts"][0], "a.txt");
        assert_eq!(sh(&repo, &["rev-parse", "main"]), head);
        assert_eq!(sh(&origin, &["rev-parse", "main"]), head);
        assert!(sh(&repo, &["status", "--porcelain"]).is_empty());
        assert_eq!(sh(&repo, &["worktree", "list"]).lines().count(), 1);
    }

    #[test]
    fn dirty_target_is_never_touched() {
        let (_d, repo, origin) = fixture();
        commit_on(&repo, "work", "main", "c.txt", "c\n");
        let head = sh(&repo, &["rev-parse", "main"]);
        for dirty in ["tracked", "untracked"] {
            if dirty == "tracked" {
                std::fs::write(repo.join("a.txt"), "edited\n").unwrap();
            } else {
                sh(&repo, &["checkout", "-q", "--", "a.txt"]);
                std::fs::write(repo.join("new.txt"), "x\n").unwrap();
            }
            let o = merge(&plan(&repo, "true"));
            assert_eq!(o.decision, "skipped_dirty", "{dirty}: {o:?}");
            assert_eq!(sh(&repo, &["rev-parse", "main"]), head);
            assert_eq!(sh(&origin, &["rev-parse", "main"]), head);
        }
        assert_eq!(
            std::fs::read_to_string(repo.join("new.txt")).unwrap(),
            "x\n"
        );
    }

    #[test]
    fn failing_tests_roll_back_and_never_push() {
        let (_d, repo, origin) = fixture();
        commit_on(&repo, "work", "main", "c.txt", "c\n");
        let head = sh(&repo, &["rev-parse", "main"]);
        let o = merge(&plan(&repo, "echo boom >&2; exit 3"));
        assert_eq!(o.decision, "skipped_tests_failed", "{o:?}");
        assert!(o.reason.contains("reset to"), "{}", o.reason);
        assert!(o.details["test_output"].as_str().unwrap().contains("boom"));
        assert_eq!(sh(&repo, &["rev-parse", "main"]), head);
        assert_eq!(sh(&origin, &["rev-parse", "main"]), head);
        assert!(!repo.join("c.txt").exists());
    }

    #[test]
    fn refused_push_rolls_back_and_is_never_forced() {
        let (_d, repo, origin) = fixture();
        commit_on(&repo, "work", "main", "c.txt", "c\n");
        // origin moves ahead behind our back, so a plain push is rejected.
        let other = repo.parent().unwrap().join("other");
        sh(
            repo.parent().unwrap(),
            &["clone", "-q", &origin.to_string_lossy(), "other"],
        );
        std::fs::write(other.join("z.txt"), "z\n").unwrap();
        sh(&other, &["add", "."]);
        sh(&other, &["commit", "-q", "-m", "remote work"]);
        sh(&other, &["push", "-q", "origin", "main"]);
        let remote_head = sh(&origin, &["rev-parse", "main"]);
        let head = sh(&repo, &["rev-parse", "main"]);
        let o = merge(&plan(&repo, "true"));
        assert_eq!(o.decision, "skipped_push_failed", "{o:?}");
        assert_eq!(sh(&origin, &["rev-parse", "main"]), remote_head);
        assert_eq!(sh(&repo, &["rev-parse", "main"]), head);
    }

    #[test]
    fn wrong_branch_and_already_merged() {
        let (_d, repo, _o) = fixture();
        commit_on(&repo, "work", "main", "c.txt", "c\n");
        sh(&repo, &["checkout", "-q", "-b", "other"]);
        assert_eq!(merge(&plan(&repo, "true")).decision, "skipped_wrong_branch");
        sh(&repo, &["checkout", "-q", "main"]);
        assert_eq!(merge(&plan(&repo, "true")).decision, "merged_ff");
        assert_eq!(merge(&plan(&repo, "true")).decision, "already_merged");
    }

    #[test]
    fn no_remote_merges_locally_only() {
        let (_d, repo, _o) = fixture();
        sh(&repo, &["remote", "remove", "origin"]);
        commit_on(&repo, "work", "main", "c.txt", "c\n");
        let o = merge(&plan(&repo, "true"));
        assert_eq!(o.decision, "merged_ff", "{o:?}");
        assert_eq!(o.details["pushed"], false);
    }
}
