// SPDX-License-Identifier: Apache-2.0
//! Change notification for waiters (orchestrators, `cards_wait`, the scheduler, a
//! table waiting on its server or its sub-tables).
//!
//! In one process every store write that matters bumps a generation counter and
//! wakes waiters at once (a condition variable: no polling). Writes from *other*
//! processes sharing the database are noticed through SQLite's `PRAGMA data_version`,
//! checked when a waiter's wake-up slice ends (`CROSS_PROCESS_CHECK`): one cheap
//! pragma, not a query loop.

use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How often a waiter looks for writes made by other processes.
pub const CROSS_PROCESS_CHECK: Duration = Duration::from_millis(400);

fn bus() -> &'static (Mutex<u64>, Condvar) {
    static BUS: OnceLock<(Mutex<u64>, Condvar)> = OnceLock::new();
    BUS.get_or_init(|| (Mutex::new(0), Condvar::new()))
}

/// Signal a change (called by the store after writes).
pub fn bump() {
    let (m, cv) = bus();
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    *g = g.wrapping_add(1);
    cv.notify_all();
}

/// Current generation.
pub fn generation() -> u64 {
    *bus().0.lock().unwrap_or_else(|e| e.into_inner())
}

/// Block until the generation differs from `seen` or `timeout` passes. Returns the generation.
pub fn wait_change(seen: u64, timeout: Duration) -> u64 {
    let (m, cv) = bus();
    let until = Instant::now() + timeout;
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    while *g == seen {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        g = cv
            .wait_timeout(g, left)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
    *g
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bump_wakes_a_waiter_at_once() {
        let g = generation();
        let h = std::thread::spawn(move || {
            let t = Instant::now();
            let n = wait_change(g, Duration::from_secs(10));
            (n, t.elapsed())
        });
        std::thread::sleep(Duration::from_millis(50));
        bump();
        let (n, waited) = h.join().unwrap();
        assert_ne!(n, g);
        assert!(waited < Duration::from_secs(2), "{waited:?}");
        // No change: returns after the timeout.
        let g = generation();
        let t = Instant::now();
        // Other tests may bump concurrently; only check that it returns.
        wait_change(g, Duration::from_millis(30));
        assert!(t.elapsed() < Duration::from_secs(5));
    }
}
