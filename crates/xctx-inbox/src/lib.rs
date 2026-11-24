//! The Inbox: everything that waits for the user.
//!
//! - A **Plan Gate** holds a proposed space; nothing launches until the user approves it (after editing it, if
//!   they like).
//! - A **space widening** asks to add repos to an active space, or to switch it to `MISE_ENV=production`.
//! - A **missing variable** says a repo's mise config `require`s a variable that is not set, with mise's help.
//! - A **notice** is something an agent or the app wants the user to see.
//!
//! [`Inbox`] keeps the items in `inbox.json` in the xctx data directory and changes them only through the
//! transitions in [`ItemState`]. Approving returns the approved item; carrying it out (launching, widening) is
//! the app's job. [`InboxItem::preview`] gives the text to show with secrets masked.

#![warn(missing_docs)]

mod error;
mod item;
mod store;

pub use error::{InboxError, Result};
pub use item::{
    InboxItem, ItemBody, ItemId, ItemKind, ItemState, Level, MissingEnvVar, Notice, PlanGate,
    Preview, Source, SpaceWiden, welcome_notice,
};
pub use store::{Filter, INBOX_FILE, Inbox, MAX_SETTLED_ITEMS};
