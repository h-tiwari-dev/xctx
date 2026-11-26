//! The native omp pane for xctx, drawn with GPUI over `xctx-omp`'s [`xctx_omp::SessionModel`].
//!
//! [`OmpView`] takes read-only snapshots of the session and emits [`OmpIntent`]s; the host sends
//! [`OmpIntent::command`] through its `OmpClient` and nothing here talks to omp. The pane has:
//!
//! - a HUD row: working indicator (spinner, elapsed time, Stop), model and effort menus, context use, cost,
//!   tokens per second, todo progress and the session name ([`hud`]);
//! - the transcript, a virtualised list of user and assistant messages, thinking, markdown ([`markdown`], via
//!   `pulldown-cmark`), and tool cards per tool kind, collapsed until opened ([`tool_card`], [`transcript`]);
//! - a banner for the problem the user should see now: omp exited, an undecodable frame, a failed turn, a retry
//!   ([`banner`]);
//! - the composer: multi-line, omp's `!`/`!!`/`$`/`$$`/`?` modes with their borders, slash and `@file` menus,
//!   image paste and drop, queue chips (edit, remove, promote), send, steer and follow-up ([`composer`],
//!   [`draft`]).
//!
//! Colours come from `xctx-ui`'s [`xctx_ui::Theme`] only; code and diffs are plain monospace.

#![warn(missing_docs)]

pub mod banner;
pub mod composer;
pub mod draft;
pub mod hud;
pub mod intent;
pub mod markdown;
pub mod tool_card;
pub mod transcript;
mod view;

pub use banner::{Banner, BannerKind};
pub use composer::{Composer, FileCompleter, PaneEscape};
pub use draft::DraftMode;
pub use hud::ModelChoice;
pub use intent::OmpIntent;
pub use view::OmpView;
