//! The error type of the API client core.

use std::path::PathBuf;

/// A request that was refused before anything was sent, or a failure to store a space's history.
///
/// Failures that happen once a request is on its way (a timeout, a refused connection, a redirect to a host
/// that is not allowed) are not errors: they are recorded in the [`crate::Exchange`] as a
/// [`crate::Outcome::Failed`], so they show up in the history like any other answer.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// The URL could not be parsed.
    #[error("{url:?} is not a valid URL: {reason}")]
    InvalidUrl {
        /// The URL as given.
        url: String,
        /// Why it was rejected.
        reason: String,
    },

    /// The URL is not `http` or `https`.
    #[error("only http and https URLs can be sent, not {0}:")]
    UnsupportedScheme(String),

    /// The URL's host is not one this space may send to.
    #[error("{host} is not an allowed host; this space may send to {allowed}")]
    HostNotAllowed {
        /// The host that was refused.
        host: String,
        /// The allowed hosts, comma separated (or `no host` when there are none).
        allowed: String,
    },

    /// The method or a header is malformed.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// No exchange in the history has this id.
    #[error("no exchange {0} in the history")]
    NotFound(u64),

    /// A replay asked for no sends or for more than one replay may make.
    #[error("a replay sends between 1 and {max} times, not {asked}")]
    BadCount {
        /// How many were asked for.
        asked: u32,
        /// The most allowed.
        max: u32,
    },

    /// The slug cannot name a folder under the spaces directory.
    #[error("{0:?} is not a valid space slug")]
    InvalidSlug(String),

    /// Reading or writing `http.json` failed.
    #[error(transparent)]
    Store(#[from] xctx_core::Error),

    /// `http.json` in one space's folder says it belongs to another space.
    #[error("{path} holds the HTTP history of space {found}, not {expected}")]
    WrongSpace {
        /// The file.
        path: PathBuf,
        /// The space it names.
        found: String,
        /// The space whose folder it is in.
        expected: String,
    },

    /// `http.json` was written by a newer xctx.
    #[error("{path} is version {found}; this xctx reads up to version {supported}")]
    UnsupportedVersion {
        /// The file.
        path: PathBuf,
        /// Its version.
        found: u32,
        /// The newest this build reads.
        supported: u32,
    },
}

/// The result type of the API client core's fallible operations.
pub type Result<T, E = HttpError> = std::result::Result<T, E>;
