// SPDX-License-Identifier: Apache-2.0
//! Scheduler: claims ready tables from the store and runs them on threads,
//! never more than `max_parallel` at once. Serial chains wait in the queue until
//! their upstream table scores (`after`); a failed upstream skips the chain.

use crate::runner;
use crate::store::Store;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

pub struct Scheduler {
    pub db: PathBuf,
    pub max_parallel: usize,
    /// Owner tag written on claimed tables: `pid:<pid>:<role>`.
    pub owner: String,
    /// Only run tables of this run (foreground `upp run`); `None` serves every run (daemon).
    pub run: Option<String>,
    /// Return once the run (or the whole queue) has no open tables.
    pub until_idle: bool,
    /// Set to stop claiming; `run` returns once in-flight tables end.
    pub stop: Arc<AtomicBool>,
}

pub fn owner_tag(role: &str) -> String {
    format!("pid:{}:{role}", std::process::id())
}

/// Mark tables whose scheduler process is gone as `stopped` (Linux: checks /proc).
pub fn reap_dead_owners(store: &Store) -> Result<usize, String> {
    let mut n = 0;
    for owner in store.running_owners()? {
        let pid = owner
            .strip_prefix("pid:")
            .and_then(|r| r.split(':').next())
            .and_then(|p| p.parse::<u32>().ok());
        if let Some(pid) = pid {
            if pid != std::process::id() && !pid_alive(pid) {
                n += store.orphan(&owner)?;
            }
        }
    }
    Ok(n)
}

fn pid_alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        Path::new(&format!("/proc/{pid}")).exists()
    } else {
        true // unknown: never reap on other systems
    }
}

impl Scheduler {
    pub fn new(db: &Path, max_parallel: usize, role: &str) -> Scheduler {
        Scheduler {
            db: db.to_path_buf(),
            max_parallel: max_parallel.max(1),
            owner: owner_tag(role),
            run: None,
            until_idle: false,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn run(&self) -> Result<(), String> {
        let store = Store::open(&self.db)?;
        let _ = reap_dead_owners(&store);
        let mut handles: Vec<JoinHandle<()>> = Vec::new();
        loop {
            store.settle_queue()?;
            handles.retain(|h| !h.is_finished());
            let stopping = self.stop.load(Ordering::SeqCst);
            if !stopping {
                let free = self.max_parallel.saturating_sub(handles.len());
                for t in store.claim(&self.owner, free, self.run.as_deref())? {
                    let db = self.db.clone();
                    let id = t.id.clone();
                    handles.push(std::thread::spawn(move || {
                        let r = std::panic::catch_unwind(|| runner::run_table(&db, &id));
                        if r.is_err() {
                            if let Ok(s) = Store::open(&db) {
                                let _ = s.finish(&id, "error", None, "runner panicked");
                            }
                        }
                    }));
                }
            }
            if handles.is_empty() {
                if stopping {
                    return Ok(());
                }
                if self.until_idle && store.open_count(self.run.as_deref())? == 0 {
                    return Ok(());
                }
            }
            // Poll fast while tables run, slower when idle (a daemon waiting for work).
            let idle = handles.is_empty();
            std::thread::sleep(Duration::from_millis(if idle { 300 } else { 100 }));
        }
    }
}
