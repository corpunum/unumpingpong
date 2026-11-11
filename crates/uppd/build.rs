// SPDX-License-Identifier: Apache-2.0
//! Embeds the commit this binary is built from as `UNUM_GIT_SHA` (`GET /health`).
//! `UNUM_GIT_SHA` in the environment overrides it; outside a git checkout it is `unknown`.
use std::process::Command;
fn git(args: &[&str]) -> Option<String> {
    let o = Command::new("git").args(args).output().ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}
fn main() {
    println!("cargo:rerun-if-env-changed=UNUM_GIT_SHA");
    for p in ["HEAD", "index"] {
        if let Some(path) = git(&["rev-parse", "--git-path", p]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let sha = std::env::var("UNUM_GIT_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| git(&["rev-parse", "--short=12", "HEAD"]))
        .unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some();
    println!(
        "cargo:rustc-env=UNUM_GIT_SHA={sha}{}",
        if dirty { "-dirty" } else { "" }
    );
}
