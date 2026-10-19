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
pub mod card;
pub mod config;
pub mod git;
pub mod notify;
pub mod orchestrator;
pub mod player;
pub mod probe;
pub mod proc;
pub mod referee;
pub mod runner;
pub mod scheduler;
pub mod search;
pub mod store;

pub use config::{
    Check, CheckSpec, Effort, Lease, MatchFile, OrchestratorSpec, Parallel, PlayerSpec, ServerMode,
    ServerSpec, TableSpec,
};
pub use scheduler::Scheduler;
pub use store::{CardRow, RallyRow, Store, TableRow};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
