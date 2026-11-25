//! The error type of the built-in tools.

/// Why a built-in tool call failed. Every message is masked with the call's secrets before it is returned by
/// [`crate::run_builtin`], so it can be shown to an agent or the user as is.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConnectorError {
    /// No built-in has this name.
    #[error("there is no built-in tool {0:?} in xctx-connectors")]
    UnknownBuiltin(String),

    /// An argument is missing, unknown or of the wrong type or shape.
    #[error("bad arguments: {0}")]
    BadArgs(String),

    /// The space is not set up for this tool: no log source, no database, a missing secret or base URL.
    #[error("not configured: {0}")]
    NotConfigured(String),

    /// The call was refused before anything ran: a statement that is not a single read, a path outside the
    /// worktree, a write that was not approved, a host that is not allowed.
    #[error("refused: {0}")]
    Refused(String),

    /// The command-line tool (`sqlite3`, `psql`) could not be run, failed, or timed out.
    #[error("{0}")]
    Tool(String),

    /// The request could not be sent or got no answer.
    #[error("request failed: {0}")]
    Http(String),

    /// The service answered with an error status.
    #[error("{service} answered {status}: {message}")]
    Service {
        /// `Jira` or `Figma`.
        service: String,
        /// The HTTP status.
        status: u16,
        /// What it said, shortened.
        message: String,
    },

    /// Reading or writing a local file failed.
    #[error("{0}")]
    Io(String),
}

impl ConnectorError {
    /// This error with every message passed through `f` (used to mask secrets).
    pub fn map_text(self, f: impl Fn(&str) -> String) -> ConnectorError {
        use ConnectorError::*;
        match self {
            UnknownBuiltin(s) => UnknownBuiltin(f(&s)),
            BadArgs(s) => BadArgs(f(&s)),
            NotConfigured(s) => NotConfigured(f(&s)),
            Refused(s) => Refused(f(&s)),
            Tool(s) => Tool(f(&s)),
            Http(s) => Http(f(&s)),
            Service {
                service,
                status,
                message,
            } => Service {
                service,
                status,
                message: f(&message),
            },
            Io(s) => Io(f(&s)),
        }
    }
}

/// The result type of the built-in tools.
pub type Result<T, E = ConnectorError> = std::result::Result<T, E>;
