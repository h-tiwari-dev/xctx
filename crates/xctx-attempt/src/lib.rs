//! A space's competing attempts: copies of one writer's work, each in its own worktree with a forked `pi` session,
//! of which the user adopts at most one.
//!
//! - [`AttemptSet`] holds a space's attempts, grouped into rounds (one per fork), and enforces the rules: at most
//!   [`xctx_core::MAX_ATTEMPTS`] live at once, at most one adopted per round, states only move forward.
//! - [`Outcome`] is what an attempt's agent reports about its work; [`Comparison`] is what the review sheet shows,
//!   with secrets masked.
//! - [`store`] keeps the set in `<data dir>/spaces/<slug>/attempts.json`.
//!
//! Making and removing worktrees is `xctx_core::attempt`'s job; this crate only keeps the records.

#![warn(missing_docs)]

mod compare;
mod error;
mod set;
pub mod store;

pub use compare::{Column, Comparison, comparison};
pub use error::{AttemptError, Result};
pub use set::{Attempt, AttemptSet, AttemptState, ForkPlan, Outcome, Repro, TestSummary};
