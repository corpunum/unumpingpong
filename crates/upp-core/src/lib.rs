// SPDX-License-Identifier: Apache-2.0
//! UnumPingPong engine.
//!
//! A **table** is a loop with a goal: a *player* (any coding/agent tool) gets a
//! *ball* (a brief), answers, an external *referee* checks the answer with
//! deterministic evidence (commands, files, HTTP, JSON), and the *server*
//! returns the ball with the referee output and a note, or scores the *point*.
//! Many tables run in parallel under a cap; tables chain in series (`after`).
//! Every rally is appended to a SQLite store: a replayable trace.

pub mod api;
pub mod config;
pub mod player;
pub mod proc;
pub mod referee;
pub mod runner;
pub mod scheduler;
pub mod store;

pub use config::{
    Check, CheckSpec, Lease, MatchFile, PlayerSpec, ServerMode, ServerSpec, TableSpec,
};
pub use scheduler::Scheduler;
pub use store::{RallyRow, Store, TableRow};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
