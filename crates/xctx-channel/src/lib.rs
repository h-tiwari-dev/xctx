//! The shared space of an xctx space: how writers in different repos coordinate without touching each other's
//! repo.
//!
//! - The **Contract** ([`Contract`]) is the versioned API shape, event payload or schema the writers agree on.
//!   Every revision bumps the version and clears agreement to just its author.
//! - The **Channel** ([`Channel`]) carries [`ChannelMessage`]s between agents, addressed or broadcast, with a
//!   bounded history ([`MAX_MESSAGES`]).
//! - **Findings** ([`Finding`]) are what helpers report; **Notes** ([`Note`]) are anything the space should
//!   remember.
//! - [`SharedSpaceStore`] keeps all of these in `<data dir>/spaces/<slug>/shared.json`, writing atomically on
//!   every change and refusing damaged or newer files instead of overwriting them.
//! - [`ScratchDir`] is the space's `/tmp/xctx-scratch/<slug>/` directory for intermediate artifacts.
//!
//! Everything an agent or the UI reads goes through an [`xctx_core::Redactor`], so secrets stay masked.

#![warn(missing_docs)]

mod channel;
mod contract;
mod error;
mod notes;
mod scratch;
mod store;

pub use channel::{Channel, ChannelMessage, MAX_MESSAGES, MessageKind};
pub use contract::{Contract, ContractDraft};
pub use error::{ChannelError, Result};
pub use notes::{Finding, FindingSeverity, MAX_FINDINGS, MAX_NOTES, Note, SharedNotes};
pub use scratch::{DEFAULT_SCRATCH_BASE, ScratchDir};
pub use store::{SHARED_FILE, SPACES_DIR, SharedSpaceStore};
