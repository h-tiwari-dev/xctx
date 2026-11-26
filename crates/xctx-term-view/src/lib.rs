//! A GPUI view of an `xctx-term` terminal session: draws the grid, feeds it keys, mouse,
//! paste, focus and resize, and reports title, bell, exit and focus to its parent.
//!
//! Create one with [`TerminalView::spawn`] and subscribe to [`TerminalViewEvent`]s. Look and
//! feel come from [`TerminalFont`] and [`TerminalTheme`].

mod grid;
mod keys;
mod theme;
mod view;

pub use keys::key_event_for_keystroke;
pub use theme::{CellColors, TerminalFont, TerminalTheme};
pub use view::{TerminalView, TerminalViewEvent, shell_quote_paths};
