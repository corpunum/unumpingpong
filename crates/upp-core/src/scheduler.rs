// SPDX-License-Identifier: Apache-2.0
//! Scheduler: claims ready tables from the store and runs them on threads,
//! never more than `max_parallel` at once. Serial chains wait in the queue until
//! their upstream table scores (`after`); a failed upstream skips the chain.
//!
//! A table waiting on its own sub-tables (phase `children`) does not hold a slot, so
//! nesting cannot deadlock the cap. The loop is event-driven: it sleeps until the store
//! changes (a table ends, a table is queued) instead of polling.
//!
//! Crash recovery: tables whose scheduler process died are put back in the queue and
//! resume after their last recorded rally (`Store::requeue`).

use crate::runner;
use crate::store::Store;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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
    /// Machine-wide cap across every scheduler sharing the store (`UPP_GLOBAL_PARALLEL`,
    /// `uppd --global-parallel`): one GPU or model endpoint shared by several runs.
    pub global_parallel: Option<usize>,
}

pub fn owner_tag(role: &str) -> String {
    format!("pid:{}:{role}", std::process::id())
}

/// Requeue the tables of scheduler processes that are gone (Linux: checks /proc), so a
/// scheduler resumes them after their last recorded rally.
pub fn reap_dead_owners(store: &Store) -> Result<usize, String> {
    let mut n = 0;
    for owner in store.running_owners()? {
        let pid = owner
            .strip_prefix("pid:")
            .and_then(|r| r.split(':').next())
            .and_then(|p| p.parse::<u32>().ok());
        if let Some(pid) = pid {
            if pid != std::process::id() && !pid_alive(pid) {
                n += store.requeue(&format!("pid:{pid}:"))?;
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
            global_parallel: std::env::var("UPP_GLOBAL_PARALLEL")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|n: &usize| *n > 0),
        }
    }

    pub fn run(&self) -> Result<(), String> {
        let store = Store::open(&self.db)?;
        let _ = reap_dead_owners(&store);
        let mut reaped = Instant::now();
        let mut handles: Vec<JoinHandle<()>> = Vec::new();
        let mut w = store.watch();
        loop {
            store.settle_queue()?;
            handles.retain(|h| !h.is_finished());
            if reaped.elapsed() > Duration::from_secs(15) {
                let _ = reap_dead_owners(&store);
                reaped = Instant::now();
            }
            let stopping = self.stop.load(Ordering::SeqCst);
            if !stopping {
                let waiting = store.count_in_phase(&self.owner, "children")?;
                let active = handles.len().saturating_sub(waiting);
                let free = self.max_parallel.saturating_sub(active);
                for t in store.claim_capped(
                    &self.owner,
                    free,
                    self.run.as_deref(),
                    self.global_parallel,
                )? {
                    let db = self.db.clone();
                    let id = t.id.clone();
                    handles.push(std::thread::spawn(move || {
                        let r = std::panic::catch_unwind(|| runner::run_table(&db, &id));
                        if r.is_err() {
                            if let Ok(s) = Store::open(&db) {
                                let _ = s.finish(&id, "error", None, "runner panicked");
                            }
                        }
                        // Wake the scheduler: this slot is free now.
                        crate::notify::bump();
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
            // Sleep until something changes (a table ends or is queued, a stop arrives).
            store.wait(&mut w, Duration::from_secs(2));
        }
    }
}
