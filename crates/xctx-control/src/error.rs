//! The error type of the control socket's server, client and paths.

use std::io;
use std::path::PathBuf;

use crate::protocol::RpcError;

/// A control socket failure the app or CLI can show.
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    /// Creating, binding, reading or writing the socket or its directory failed.
    #[error("{path}: {source}")]
    Io {
        /// The socket or directory involved.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },

    /// The socket path does not fit in a Unix socket address.
    #[error("socket path {path} is {len} bytes; Unix sockets allow at most {max}")]
    SocketPathTooLong {
        /// The path.
        path: PathBuf,
        /// Its length in bytes.
        len: usize,
        /// The longest path the platform accepts.
        max: usize,
    },

    /// Another xctx window is already serving this socket.
    #[error("another xctx is already listening on {0}")]
    AlreadyRunning(PathBuf),

    /// Nothing is listening on the socket: xctx is not running, or uses another socket.
    #[error("xctx is not running (nothing is listening on {0})")]
    NotRunning(PathBuf),

    /// There is no socket path: neither `XCTX_CONTROL_SOCKET`, `XCTX_DATA_DIR` nor `HOME` is set.
    #[error("no control socket path: {0}")]
    NoSocketPath(#[from] xctx_core::Error),

    /// The other side sent something that is not this protocol.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// The window answered the request with an error.
    #[error(transparent)]
    Rpc(#[from] RpcError),
}

/// The result type of the control socket's fallible operations.
pub type Result<T, E = ControlError> = std::result::Result<T, E>;

/// Wrap an I/O error with the path it concerns.
pub(crate) fn io_error(path: impl Into<PathBuf>) -> impl FnOnce(io::Error) -> ControlError {
    let path = path.into();
    move |source| ControlError::Io { path, source }
}
