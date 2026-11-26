//! omp's RPC protocol for xctx, with no window in sight.
//!
//! omp (oh-my-pi) runs as `omp --mode rpc-ui`: commands go in as JSON lines on its input, frames come back on its
//! output (`omp://rpc.md` in omp itself is the reference). This crate has four parts, each testable alone:
//!
//! - [`codec`]: lines to logical frames, including protocol v2's `rpc_chunk` reassembly.
//! - [`frame`] and [`command`]: what omp says ([`Frame`]) and what xctx says ([`Command`]). Decoding never
//!   fails on something new: an unknown frame is kept as it is.
//! - [`client`]: the child process, its two threads and the handshake.
//! - [`model`]: [`SessionModel`], a pure reducer from frames to the state a pane draws, and the [`Effect`]s the
//!   host carries out. Replaying a recorded conversation (`tests/fixtures`) reproduces a session exactly.
//! - [`dialog`]: the pending-request registry and the one answer path for forms, selects, confirms, inputs and
//!   tool approvals, with the danger classifier the Inbox uses.
//! - [`form`]: omp's `ask` tool sends an HTML form; this parses the subset omp produces and checks answers the
//!   way omp does, so a reply omp would reject is never sent.
//!
//! Nothing here approves anything: a tool approval is a [`Gate`] the user answers.

#![warn(missing_docs)]

pub mod client;
pub mod codec;
pub mod command;
pub mod dialog;
mod error;
pub mod form;
pub mod frame;
pub mod model;

pub use client::{Incoming, LaunchSpec, OmpClient, Started};
pub use command::{Command, Image, Queue, Streaming, SubagentLevel};
pub use dialog::{
    Answer, Change, Danger, DangerClass, DialogError, Dialogs, Kind, Origin, Pending, Permission,
};
pub use error::{OmpError, Result};
pub use frame::{Approval, Frame, PromptResult, Ready, Response, UiMethod, UiRequest};
pub use model::{
    AssistantEntry, Block, Effect, Entry, Gate, GateKind, Level, Phase, SessionInfo, SessionModel,
    ToolRun, ToolState,
};
