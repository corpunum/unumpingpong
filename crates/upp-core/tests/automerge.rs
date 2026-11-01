// SPDX-License-Identifier: Apache-2.0
//! Auto-merge through the engine: a worktree table scores (or not) and its point is
//! merged (or not) into a temporary git repository with a bare `origin`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use upp_core::store::{Store, TableRow};
use upp_core::{api, MatchFile, Scheduler};

fn fake(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fake-agents")
        .join(name);
    std::fs::canonicalize(p)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

struct Fx {
    dir: tempfile::TempDir,
    repo: PathBuf,
    origin: PathBuf,
}

fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let origin = dir.path().join("origin.git");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "-q", "--bare", "-b", "main"]);
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README"), "base\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(
        &repo,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    git(&repo, &["push", "-q", "origin", "main"]);
    Fx { dir, repo, origin }
}

/// One worktree table whose player writes `answer.txt` = `good` and whose referee wants 42.
fn run(fx: &Fx, base: &str, good: &str, test: &str) -> (Store, TableRow) {
    let toml = format!(
        r#"
[[table]]
name = "worker"
ball = "write the answer"
player = {{ kind = "shell", command = ["sh", "{coder}"], env = {{ FAKE_PASS_ON = "1", FAKE_GOOD = "{good}" }} }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt" }}]
lease = {{ max_rallies = 2 }}
worktree = {{ repo = "{repo}", base = "{base}" }}
auto_merge = {{ repo = "{repo}", target = "main", test = "{test}" }}
"#,
        coder = fake("fake-coder.sh"),
        repo = fx.repo.display(),
    );
    let f = fx.dir.path().join("match.toml");
    std::fs::write(&f, toml).unwrap();
    let db = fx.dir.path().join("upp.db");
    let store = Store::open(&db).unwrap();
    let created = api::create(&store, MatchFile::load(&f).unwrap(), None).unwrap();
    let mut s = Scheduler::new(&db, 1, "test");
    s.run = Some(created["run_id"].as_str().unwrap().to_string());
    s.until_idle = true;
    s.run().unwrap();
    let t = store.resolve("worker").unwrap();
    (store, t)
}

fn decisions(store: &Store, t: &TableRow) -> Vec<String> {
    store
        .merge_log(&t.id)
        .unwrap()
        .iter()
        .map(|d| d["decision"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn referee_pass_fast_forwards_tests_then_pushes() {
    let fx = fixture();
    let (store, t) = run(&fx, "HEAD", "42", "test -f answer.txt");
    assert_eq!(t.status, "point", "{t:?}");
    assert_eq!(decisions(&store, &t), ["merged_ff"]);
    let log = store.merge_log(&t.id).unwrap();
    assert_eq!(log[0]["details"]["pushed"], true);
    assert!(log[0]["reason"].as_str().unwrap().contains("tests passed"));
    assert!(fx.repo.join("answer.txt").exists());
    assert_eq!(
        git(&fx.repo, &["rev-parse", "main"]),
        git(&fx.origin, &["rev-parse", "main"])
    );
}

#[test]
fn diverged_main_gets_a_rebase_merge() {
    let fx = fixture();
    let base = git(&fx.repo, &["rev-parse", "HEAD"]);
    std::fs::write(fx.repo.join("other.txt"), "o\n").unwrap();
    git(&fx.repo, &["add", "-A"]);
    git(&fx.repo, &["commit", "-q", "-m", "main moved"]);
    git(&fx.repo, &["push", "-q", "origin", "main"]);
    let (store, t) = run(&fx, &base, "42", "test -f answer.txt && test -f other.txt");
    assert_eq!(decisions(&store, &t), ["merged_rebase"]);
    assert_eq!(
        git(&fx.repo, &["rev-list", "--merges", "--count", "main"]),
        "0"
    );
    assert_eq!(
        git(&fx.repo, &["rev-parse", "main"]),
        git(&fx.origin, &["rev-parse", "main"])
    );
}

#[test]
fn referee_failure_never_merges() {
    let fx = fixture();
    let head = git(&fx.repo, &["rev-parse", "main"]);
    let (store, t) = run(&fx, "HEAD", "41", "true");
    assert_eq!(t.status, "lost", "{t:?}");
    assert_eq!(decisions(&store, &t), ["skipped_referee"]);
    assert_eq!(git(&fx.repo, &["rev-parse", "main"]), head);
    assert_eq!(git(&fx.origin, &["rev-parse", "main"]), head);
    assert!(!fx.repo.join("answer.txt").exists());
}

#[test]
fn dirty_target_repo_blocks_the_merge() {
    let fx = fixture();
    let head = git(&fx.repo, &["rev-parse", "main"]);
    std::fs::write(fx.repo.join("README"), "edited locally\n").unwrap();
    let (store, t) = run(&fx, "HEAD", "42", "true");
    assert_eq!(t.status, "point");
    assert_eq!(decisions(&store, &t), ["skipped_dirty"]);
    assert_eq!(git(&fx.repo, &["rev-parse", "main"]), head);
    assert_eq!(git(&fx.origin, &["rev-parse", "main"]), head);
    assert_eq!(
        std::fs::read_to_string(fx.repo.join("README")).unwrap(),
        "edited locally\n"
    );
}

#[test]
fn failing_tests_roll_back_and_do_not_push() {
    let fx = fixture();
    let head = git(&fx.repo, &["rev-parse", "main"]);
    let (store, t) = run(&fx, "HEAD", "42", "exit 1");
    assert_eq!(t.status, "point");
    assert_eq!(decisions(&store, &t), ["skipped_tests_failed"]);
    assert_eq!(git(&fx.repo, &["rev-parse", "main"]), head);
    assert_eq!(git(&fx.origin, &["rev-parse", "main"]), head);
    // Idempotent: running the hook again does not decide twice.
    upp_core::automerge::after_table(&store, &t.id).unwrap();
    assert_eq!(decisions(&store, &t).len(), 1);
}
