// SPDX-License-Identifier: Apache-2.0
//! Git support: per-table worktrees, rally provenance, and the integration step.
//!
//! * **Provenance.** Before the referee runs, the table's working state is written as a
//!   git tree through a *temporary index* (the user's index is not touched), so every
//!   referee result is tied to `{commit, tree}`: the exact content that was checked.
//! * **Worktrees.** `worktree = { repo = "..." }` gives a table its own `git worktree` on
//!   its own branch; players and the referee work there, isolated from each other.
//! * **Integration.** `upp integrate` commits, per scored table, the *tree the referee
//!   passed* (not whatever the worktree holds later) and merges those branches one by
//!   one into a fresh integration branch, stopping at the first conflict.

use crate::proc;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// Run git in `dir`; Ok(stdout trimmed) on exit 0.
pub fn git(dir: &Path, args: &[&str], env: &BTreeMap<String, String>) -> Result<String, String> {
    let mut argv = vec!["git".to_string()];
    argv.extend(args.iter().map(|s| s.to_string()));
    let out = proc::run(&argv, dir, None, env, GIT_TIMEOUT)?;
    if out.ok() {
        Ok(out.stdout.trim().to_string())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            proc::tail(out.combined().trim(), 600)
        ))
    }
}

fn g(dir: &Path, args: &[&str]) -> Result<String, String> {
    git(dir, args, &BTreeMap::new())
}

/// The repository top level containing `dir`, if any.
pub fn toplevel(dir: &Path) -> Option<PathBuf> {
    g(dir, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(PathBuf::from)
}

/// Write the working state of the repository containing `dir` as a tree, without
/// touching the real index. Untracked files are included; ignored files are not.
pub fn working_tree(dir: &Path) -> Result<(Option<String>, String), String> {
    let top = toplevel(dir).ok_or("not a git repository")?;
    let commit = g(&top, &["rev-parse", "--verify", "-q", "HEAD"]).ok();
    let tmp = std::env::temp_dir().join(format!("upp-index-{}", uuid::Uuid::new_v4().simple()));
    // Start from a copy of the real index so unchanged files need no re-hashing.
    let real = g(
        &top,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )
    .map(PathBuf::from)
    .ok();
    let mut env = BTreeMap::new();
    env.insert("GIT_INDEX_FILE".into(), tmp.to_string_lossy().into_owned());
    let seeded = real
        .as_ref()
        .filter(|p| p.exists())
        .map(|p| std::fs::copy(p, &tmp).is_ok())
        .unwrap_or(false);
    let res = (|| {
        if !seeded {
            match &commit {
                Some(_) => git(&top, &["read-tree", "HEAD"], &env)?,
                None => git(&top, &["read-tree", "--empty"], &env)?,
            };
        }
        git(&top, &["add", "-A", "."], &env)?;
        git(&top, &["write-tree"], &env)
    })();
    let _ = std::fs::remove_file(&tmp);
    Ok((commit, res?))
}

/// The tracked working state as a tree, cheaply: `git stash create` writes a commit of the
/// tracked changes without touching the index, the worktree or the stash list (it costs
/// only the changed files). Untracked files are not included.
pub fn tracked_tree(dir: &Path) -> Result<(Option<String>, String), String> {
    let top = toplevel(dir).ok_or("not a git repository")?;
    let commit = g(&top, &["rev-parse", "--verify", "-q", "HEAD"])
        .map_err(|_| "the repository has no commit yet".to_string())?;
    // `stash create` refreshes the index it uses: give it a copy, never the real one.
    let tmp = std::env::temp_dir().join(format!("upp-index-{}", uuid::Uuid::new_v4().simple()));
    let mut env = upp_ident();
    env.insert("GIT_INDEX_FILE".into(), tmp.to_string_lossy().into_owned());
    let real = g(
        &top,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )
    .map(PathBuf::from)
    .ok()
    .filter(|p| p.exists());
    let stash = (|| {
        match real {
            Some(r) => {
                std::fs::copy(&r, &tmp).map_err(|e| format!("copy index: {e}"))?;
            }
            None => {
                git(&top, &["read-tree", "HEAD"], &env)?;
            }
        }
        git(&top, &["stash", "create"], &env)
    })();
    let _ = std::fs::remove_file(&tmp);
    let stash = stash?;
    let snap = if stash.is_empty() {
        commit.clone()
    } else {
        stash
    };
    let tree = g(&top, &["rev-parse", &format!("{snap}^{{tree}}")])?;
    Ok((Some(commit), tree))
}

/// Provenance of the state the referee is about to check: commit, tree, whether it
/// differs from the commit, and the files changed since `base` (or since the commit).
///
/// `full` (a table's own worktree) snapshots every file but ignored ones, untracked
/// included. Otherwise only tracked files are snapshotted (`scope: "tracked"`), because a
/// table's cwd may sit inside a large unrelated repository.
pub fn provenance(dir: &Path, base: Option<&str>, full: bool) -> Value {
    let snap = if full {
        working_tree(dir)
    } else {
        tracked_tree(dir)
    };
    let scope = if full { "all" } else { "tracked" };
    match snap {
        Err(e) => json!({"git": false, "note": e}),
        Ok((commit, tree)) => {
            let top = toplevel(dir).unwrap_or_else(|| dir.to_path_buf());
            let head_tree = commit
                .as_ref()
                .and_then(|c| g(&top, &["rev-parse", &format!("{c}^{{tree}}")]).ok());
            let from = base.map(String::from).or(commit.clone());
            let changed: Vec<String> = match &from {
                Some(f) => g(&top, &["diff", "--name-only", f, &tree])
                    .map(|s| s.lines().map(String::from).collect())
                    .unwrap_or_default(),
                None => g(&top, &["ls-tree", "-r", "--name-only", &tree])
                    .map(|s| s.lines().map(String::from).collect())
                    .unwrap_or_default(),
            };
            json!({
                "git": true,
                "repo": top.to_string_lossy(),
                "scope": scope,
                "commit": commit,
                "tree": tree,
                "dirty": head_tree.as_deref() != Some(tree.as_str()),
                "base": base,
                "changed": changed,
            })
        }
    }
}

/// Create (or reuse, on resume) a worktree for a table. Returns the record stored on
/// the table: repo, path, branch, base commit.
pub fn worktree_add(repo: &Path, path: &Path, branch: &str, base: &str) -> Result<Value, String> {
    let top =
        toplevel(repo).ok_or_else(|| format!("{} is not a git repository", repo.display()))?;
    let base_commit = g(
        &top,
        &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
    )?;
    if path.join(".git").exists() {
        // Resumed table: keep its worktree and branch as they are.
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let p = path.to_string_lossy().into_owned();
        g(
            &top,
            &["worktree", "add", "-q", "-b", branch, &p, &base_commit],
        )?;
    }
    Ok(json!({
        "repo": top.to_string_lossy(),
        "path": path.to_string_lossy(),
        "branch": branch,
        "base": base_commit,
    }))
}

fn upp_ident() -> BTreeMap<String, String> {
    [
        ("GIT_AUTHOR_NAME", "unumpingpong"),
        ("GIT_AUTHOR_EMAIL", "upp@localhost"),
        ("GIT_COMMITTER_NAME", "unumpingpong"),
        ("GIT_COMMITTER_EMAIL", "upp@localhost"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// Commit exactly `tree` (the content the referee passed) on top of `branch`, unless the
/// branch already has it. Returns the branch head. The worktree's files are not touched.
pub fn commit_verified(repo: &Path, branch: &str, tree: &str, msg: &str) -> Result<String, String> {
    let top =
        toplevel(repo).ok_or_else(|| format!("{} is not a git repository", repo.display()))?;
    let head = g(&top, &["rev-parse", &format!("refs/heads/{branch}")])?;
    let head_tree = g(&top, &["rev-parse", &format!("{head}^{{tree}}")])?;
    if head_tree == tree {
        return Ok(head);
    }
    let c = git(
        &top,
        &["commit-tree", tree, "-p", &head, "-m", msg],
        &upp_ident(),
    )?;
    g(
        &top,
        &["update-ref", &format!("refs/heads/{branch}"), &c, &head],
    )?;
    Ok(c)
}

/// Branch names: `upp/<name>-<id>` with characters git accepts.
pub fn branch_name(table_name: &str, id: &str) -> String {
    let clean: String = table_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("upp/{}-{id}", clean.trim_matches(['-', '.']))
}

/// One scored table to integrate: its worktree record and the tree its point was verified on.
pub struct Scored {
    pub table_id: String,
    pub name: String,
    pub worktree: Value,
    pub tree: String,
}

/// Integrate scored tables: commit each verified tree onto its branch, then merge the
/// branches one by one into `upp/integrate-<label>` (created from `into`) in a fresh
/// worktree. Stops at the first conflict. `check` runs in the integration worktree.
pub fn integrate(
    repo: &Path,
    into: &str,
    label: &str,
    tables: &[Scored],
    work: &Path,
    check: Option<&str>,
) -> Result<Value, String> {
    let top =
        toplevel(repo).ok_or_else(|| format!("{} is not a git repository", repo.display()))?;
    let env = upp_ident();
    let mut steps = Vec::new();
    // 1. Make sure each branch carries its verified tree (a no-op when the point already did).
    for t in tables {
        let branch = t.worktree["branch"].as_str().unwrap_or_default();
        let msg = format!("upp: {} ({}) verified point", t.name, t.table_id);
        let commit = commit_verified(&top, branch, &t.tree, &msg)?;
        steps.push(json!({"table": t.name, "table_id": t.table_id, "branch": branch, "commit": commit, "tree": t.tree}));
    }
    // 2. Merge them into a fresh integration branch, in order.
    let ibranch = format!("upp/integrate-{label}");
    let ipath = work.join(format!("integrate-{label}"));
    if ipath.exists() {
        let _ = g(
            &top,
            &["worktree", "remove", "--force", &ipath.to_string_lossy()],
        );
    }
    let _ = g(&top, &["branch", "-D", &ibranch]);
    let base = g(
        &top,
        &["rev-parse", "--verify", &format!("{into}^{{commit}}")],
    )?;
    std::fs::create_dir_all(work).map_err(|e| format!("{}: {e}", work.display()))?;
    g(
        &top,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &ibranch,
            &ipath.to_string_lossy(),
            &base,
        ],
    )?;
    let mut merged = Vec::new();
    let mut conflict: Option<Value> = None;
    for s in &steps {
        let branch = s["branch"].as_str().unwrap_or_default();
        let msg = format!("upp: integrate {}", s["table"].as_str().unwrap_or(""));
        match git(
            &ipath,
            &["merge", "--no-ff", "-q", "-m", &msg, branch],
            &env,
        ) {
            Ok(_) => merged.push(s["table"].clone()),
            Err(e) => {
                let files =
                    g(&ipath, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
                let _ = g(&ipath, &["merge", "--abort"]);
                conflict = Some(
                    json!({"table": s["table"], "branch": branch, "files": files.lines().collect::<Vec<_>>(), "error": e}),
                );
                break;
            }
        }
    }
    let head = g(&ipath, &["rev-parse", "HEAD"])?;
    let tree = g(&ipath, &["rev-parse", "HEAD^{tree}"])?;
    let check_res = match (check, &conflict) {
        (Some(cmd), None) => {
            let argv = crate::config::Cmd::Line(cmd.to_string()).argv();
            let o = proc::run(
                &argv,
                &ipath,
                None,
                &BTreeMap::new(),
                Duration::from_secs(1800),
            )?;
            Some(
                json!({"run": cmd, "pass": o.ok(), "code": o.code, "output": proc::tail(o.combined().trim(), 2000)}),
            )
        }
        _ => None,
    };
    Ok(json!({
        "ok": conflict.is_none() && check_res.as_ref().is_none_or(|c| c["pass"] == true),
        "branch": ibranch,
        "path": ipath.to_string_lossy(),
        "into": into,
        "base": base,
        "commit": head,
        "tree": tree,
        "tables": steps,
        "merged": merged,
        "conflict": conflict,
        "check": check_res,
    }))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        g(p, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(p.join("a.txt"), "one\n").unwrap();
        g(p, &["add", "-A"]).unwrap();
        git(
            p,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "init",
            ],
            &BTreeMap::new(),
        )
        .unwrap();
        d
    }

    #[test]
    fn provenance_names_the_checked_tree_without_touching_the_index() {
        let d = repo();
        let p = d.path();
        let clean = provenance(p, None, true);
        assert_eq!(clean["dirty"], false);
        std::fs::write(p.join("a.txt"), "two\n").unwrap();
        std::fs::write(p.join("new.txt"), "n\n").unwrap();
        // Tracked scope: sees the change to a.txt, not the untracked file; touches nothing.
        let t = provenance(p, None, false);
        assert_eq!(
            (t["scope"].as_str(), t["dirty"].as_bool()),
            (Some("tracked"), Some(true))
        );
        assert_eq!(t["changed"], json!(["a.txt"]));
        assert_eq!(
            g(p, &["stash", "list"]).unwrap(),
            "",
            "stash create leaves no entry"
        );
        let idx = p.join(".git/index");
        let before = std::fs::metadata(&idx).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        provenance(p, None, false);
        assert_eq!(
            std::fs::metadata(&idx).unwrap().modified().unwrap(),
            before,
            "real index untouched"
        );
        let v = provenance(p, None, true);
        assert_eq!(v["dirty"], true);
        assert_ne!(v["tree"], clean["tree"]);
        assert_eq!(v["changed"], json!(["a.txt", "new.txt"]));
        // The user's index is untouched: new.txt is still untracked.
        assert!(g(p, &["status", "--porcelain"])
            .unwrap()
            .contains("?? new.txt"));
        // Same content, same tree.
        assert_eq!(provenance(p, None, true)["tree"], v["tree"]);
        let nd = tempfile::tempdir().unwrap();
        assert_eq!(provenance(nd.path(), None, true)["git"], false);
        assert_eq!(provenance(nd.path(), None, false)["git"], false);
    }

    #[test]
    fn worktrees_and_integration_use_the_verified_tree() {
        let d = repo();
        let p = d.path();
        let work = d.path().join(".upp/worktrees");
        let mut scored = Vec::new();
        for (i, (file, body)) in [("b.txt", "bee\n"), ("c.txt", "sea\n")].iter().enumerate() {
            let id = format!("t{i}");
            let wt = worktree_add(
                p,
                &work.join(&id),
                &branch_name(&format!("job {i}"), &id),
                "HEAD",
            )
            .unwrap();
            let path = PathBuf::from(wt["path"].as_str().unwrap());
            std::fs::write(path.join(file), body).unwrap();
            let prov = provenance(&path, wt["base"].as_str(), true);
            assert_eq!(prov["changed"], json!([file]));
            // A change after the point must not be integrated.
            std::fs::write(path.join("late.txt"), "not verified\n").unwrap();
            scored.push(Scored {
                table_id: id,
                name: format!("job {i}"),
                worktree: wt,
                tree: prov["tree"].as_str().unwrap().to_string(),
            });
        }
        assert_eq!(branch_name("job 0", "t0"), "upp/job-0-t0");
        let r = integrate(
            p,
            "main",
            "r1",
            &scored,
            &work,
            Some("test -f b.txt && test -f c.txt"),
        )
        .unwrap();
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["merged"].as_array().unwrap().len(), 2);
        let files = g(
            p,
            &[
                "ls-tree",
                "-r",
                "--name-only",
                r["commit"].as_str().unwrap(),
            ],
        )
        .unwrap();
        assert!(files.contains("b.txt") && files.contains("c.txt"));
        assert!(!files.contains("late.txt"), "{files}");
        // The user's checkout is untouched.
        assert_eq!(
            g(p, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(),
            "main"
        );
        assert!(!p.join("b.txt").exists());
    }

    #[test]
    fn integration_stops_at_a_conflict() {
        let d = repo();
        let p = d.path();
        let work = d.path().join(".upp/worktrees");
        let mut scored = Vec::new();
        for i in 0..2 {
            let id = format!("c{i}");
            let wt = worktree_add(p, &work.join(&id), &branch_name("x", &id), "HEAD").unwrap();
            let path = PathBuf::from(wt["path"].as_str().unwrap());
            std::fs::write(path.join("a.txt"), format!("version {i}\n")).unwrap();
            let tree = provenance(&path, None, true)["tree"]
                .as_str()
                .unwrap()
                .to_string();
            scored.push(Scored {
                table_id: id,
                name: format!("x{i}"),
                worktree: wt,
                tree,
            });
        }
        let r = integrate(p, "main", "r2", &scored, &work, Some("true")).unwrap();
        assert_eq!(r["ok"], false);
        assert_eq!(r["merged"], json!(["x0"]));
        assert_eq!(r["conflict"]["table"], "x1");
        assert_eq!(r["conflict"]["files"], json!(["a.txt"]));
        assert!(r["check"].is_null(), "no check after a conflict");
    }
}
