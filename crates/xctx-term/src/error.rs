//! Errors returned by terminal sessions.

use std::fmt;
use std::io;

/// Why a terminal operation failed.
#[derive(Debug)]
pub enum TermError {
    /// Opening a pseudo-terminal pair failed.
    OpenPty(io::Error),
    /// Configuring the pseudo-terminal failed.
    ConfigurePty(io::Error),
    /// The program could not be started (not found, not executable, bad working directory).
    Spawn {
        /// The program that was asked for.
        program: String,
        /// The underlying error.
        source: io::Error,
    },
    /// A background thread could not be started.
    Thread(io::Error),
    /// Setting up the PTY reader's poller failed.
    Poll(io::Error),
    /// Telling the kernel the new window size failed.
    Resize(io::Error),
    /// The session's writer has stopped because the program exited or the session shut down.
    Closed,
}

impl fmt::Display for TermError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TermError::OpenPty(err) => write!(f, "could not open a pseudo-terminal: {err}"),
            TermError::ConfigurePty(err) => {
                write!(f, "could not configure the pseudo-terminal: {err}")
            }
            TermError::Spawn { program, source } => {
                write!(f, "could not start `{program}`: {source}")
            }
            TermError::Thread(err) => write!(f, "could not start a terminal thread: {err}"),
            TermError::Poll(err) => write!(f, "could not watch the pseudo-terminal: {err}"),
            TermError::Resize(err) => write!(f, "could not resize the terminal: {err}"),
            TermError::Closed => write!(f, "the terminal session has closed"),
        }
    }
}

impl std::error::Error for TermError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TermError::OpenPty(err)
            | TermError::ConfigurePty(err)
            | TermError::Thread(err)
            | TermError::Poll(err)
            | TermError::Resize(err) => Some(err),
            TermError::Spawn { source, .. } => Some(source),
            TermError::Closed => None,
        }
    }
}
