//! The native omp pane for xctx, drawn with GPUI over `xctx-omp`'s [`xctx_omp::SessionModel`].
//!
//! [`OmpView`] takes read-only snapshots of the session and emits [`OmpIntent`]s; the host sends
//! [`OmpIntent::command`] through its `OmpClient` and nothing here talks to omp. The pane has:
//!
//! - a session bar with quick actions: in-session Changes/Diff drawer ([`changes`]), Subagent tray & peek, Side
//!   questions (`btw`), Todo checklist drawer, Session info/stats/actions ([`panels`]), Expand/collapse all tools,
//!   and one-click Copy everywhere;
//! - a HUD row: working indicator (spinner, elapsed time, Stop), model and effort menus, context use, cost,
//!   tokens per second, clickable todo progress and the session name ([`hud`]);
//! - the transcript, a virtualised list of user and assistant messages, thinking, markdown ([`markdown`], via
//!   `pulldown-cmark`, with copyable code block headers), and tool cards per tool kind with one-click copy
//!   ([`tool_card`], [`transcript`]);
//! - a banner for the problem the user should see now: omp exited, an undecodable frame, a failed turn, a retry
//!   ([`banner`]);
//! - the composer: multi-line, omp's `!`/`!!`/`$`/`$$`/`?` modes with their borders, slash and `@file` menus,
//!   image paste and drop, queue chips (edit, remove, promote), send, steer and follow-up ([`composer`],
//!   [`draft`]).
//!
//! Colours come from `xctx-ui`'s [`xctx_ui::Theme`] only; code and diffs are plain monospace.

#![warn(missing_docs)]

pub mod banner;
pub mod changes;
pub mod composer;
pub mod draft;
mod gate;
pub mod hud;
pub mod intent;
pub mod markdown;
pub mod panels;
pub mod selectable;
pub mod tool_card;
pub mod transcript;
mod view;

pub use banner::{Banner, BannerKind};
pub use changes::{ChangeKind, ChangedFile, ChangesRefresher, SessionChanges};
pub use composer::{Composer, FileCompleter, PaneEscape};
pub use draft::DraftMode;
pub use hud::ModelChoice;
pub use intent::{GateAnswer, OmpIntent};
pub use panels::{SessionEntry, SessionsRefresher, SplitAgentHandler};
pub use selectable::{LinkHandler, SegmentId, TextSelection};
pub use tool_card::OpenFileHandler;
pub use view::OmpView;
