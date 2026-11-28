//! Terminal sessions for xctx: a PTY, terminal emulation, grid snapshots for drawing, input
//! encoding and selection. No GUI dependencies; `xctx-term-view` draws a session.
//!
//! Start a program with [`TerminalSession::spawn`], redraw from [`TerminalSession::snapshot`]
//! when the callback reports [`TerminalEvent::Wakeup`], and send input encoded with the
//! functions in [`input`] using the session's current [`TerminalModes`].
//!
//! Emulation is provided by `alacritty_terminal` (Apache-2.0).

mod config;
mod emulator;
mod error;
pub mod input;
mod pty;
mod session;
mod snapshot;

pub use config::TerminalConfig;
pub use emulator::{
    Emulator, LinkSpan, SelectionMode, Side, TerminalEvent, TerminalLink, TerminalLinkKind,
};
pub use error::TermError;
pub use session::TerminalSession;
pub use snapshot::{
    AnsiColor, Cell, CellFlags, Color, CursorShape, CursorSnapshot, GridSnapshot,
    KittyKeyboardFlags, MouseEncoding, MouseReporting, Rgb, SelectionSpan, TerminalModes,
    ViewportPoint, default_indexed_color,
};
