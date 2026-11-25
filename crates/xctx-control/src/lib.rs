//! The xctx control socket: how agents and scripts inspect and drive a running xctx window.
//!
//! The wire format is JSON-RPC 2.0, one JSON object per line, over a Unix domain socket. By default the socket is
//! `$XCTX_CONTROL_SOCKET`, else `control.sock` in the xctx data directory (see [`socket_path`]). The app exports
//! `XCTX_CONTROL_SOCKET` to every pane it spawns, so `xctx-ctl` inside a pane finds the window with no setup.
//!
//! - [`protocol`] holds the typed requests ([`Call`]), their parameters and results, and the error codes.
//! - [`Server`] binds the socket and hands each authorised request to the app as an [`Incoming`].
//! - [`Client`] connects to a running window and makes calls; `xctx-ctl` is a thin CLI over it.
//!
//! Nothing on the socket can approve an Inbox item, launch a space or widen one: `space.draft` and
//! `space.widen_propose` only propose, and `plugin.mount` and `plugin.call` post a plugin gate to the Inbox for
//! anything that writes or is not trusted yet. Approval stays with the user in the window. Resuming a paused agent and
//! setting a loop's autonomy need [`Capability::Steer`], which [`ServerConfig::default`] does not grant.

#![warn(missing_docs)]

pub mod cli;
mod client;
mod error;
mod path;
mod peer;
pub mod protocol;
mod server;

pub use client::Client;
pub use error::{ControlError, Result};
pub use path::{
    CONTROL_SOCKET_ENV, SOCKET_FILE_NAME, SUN_PATH_MAX, check_socket_path, resolve_socket_path,
    socket_path,
};
pub use protocol::{Call, Capability, ErrorCode, Method, PROTOCOL_VERSION, RpcError, StatusLevel};
pub use server::{Handler, Incoming, Responder, Server, ServerConfig};
