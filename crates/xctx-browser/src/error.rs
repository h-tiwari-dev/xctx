//! The error type of the DevTools client.

use std::path::PathBuf;
use std::time::Duration;

/// Everything that can go wrong finding, starting or talking to Chrome.
///
/// Every variant's message is written to be shown to the user as it is.
#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    /// No Chrome, Chromium, Brave or Edge was found.
    #[error(
        "no Chrome or Chromium was found (looked at {searched}); install Google Chrome from \
         https://www.google.com/chrome/ or set XCTX_CHROME to the browser's executable"
    )]
    ChromeNotFound {
        /// The paths that were checked, comma separated.
        searched: String,
    },

    /// `$XCTX_CHROME` names a file that does not exist or is not executable.
    #[error("XCTX_CHROME is set to {0}, which is not an executable file")]
    BadChromeOverride(PathBuf),

    /// Chrome could not be started.
    #[error("could not start {chrome}: {source}")]
    Spawn {
        /// The executable.
        chrome: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// Chrome exited before it opened its DevTools port.
    #[error("Chrome exited before it opened its DevTools port ({status})")]
    ExitedEarly {
        /// The exit status, as text.
        status: String,
    },

    /// Chrome did not write `DevToolsActivePort` in time.
    #[error("Chrome did not open its DevTools port within {0:?}")]
    LaunchTimeout(Duration),

    /// `DevToolsActivePort` could not be read.
    #[error("{path} is not a valid DevToolsActivePort file: {reason}")]
    BadActivePort {
        /// The file.
        path: PathBuf,
        /// Why it was rejected.
        reason: String,
    },

    /// A file or socket operation failed.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// One of the DevTools HTTP endpoints answered with something unexpected.
    #[error("the DevTools endpoint {path} failed: {reason}")]
    Http {
        /// The endpoint path, for example `/json/version`.
        path: String,
        /// What went wrong.
        reason: String,
    },

    /// The DevTools websocket failed.
    #[error("the DevTools connection failed: {0}")]
    WebSocket(String),

    /// The DevTools connection was closed by Chrome.
    #[error("the DevTools connection was closed (Chrome or the page went away)")]
    Closed,

    /// A DevTools call got no answer in time.
    #[error("{method} got no answer within {timeout:?}")]
    Timeout {
        /// The CDP method, or what was being waited for.
        method: String,
        /// How long was waited.
        timeout: Duration,
    },

    /// Chrome answered a DevTools call with an error.
    #[error("{method} failed: {message} (code {code})")]
    Protocol {
        /// The CDP method.
        method: String,
        /// The CDP error code.
        code: i64,
        /// The CDP error message.
        message: String,
    },

    /// A message from Chrome could not be understood.
    #[error("unexpected DevTools message: {0}")]
    BadMessage(String),

    /// The URL could not be parsed.
    #[error("{url:?} is not a valid URL: {reason}")]
    InvalidUrl {
        /// The URL as given.
        url: String,
        /// Why it was rejected.
        reason: String,
    },

    /// The URL is not `http` or `https`.
    #[error("only http and https pages can be opened, not {0}:")]
    UnsupportedScheme(String),

    /// The URL's host is not one the policy allows.
    #[error("{host} is not an allowed host; the browser may open {allowed}")]
    HostNotAllowed {
        /// The host that was refused.
        host: String,
        /// The allowed hosts, comma separated (or `no host` when there are none).
        allowed: String,
    },

    /// Chrome could not load the page.
    #[error("{url} could not be loaded: {reason}")]
    Navigation {
        /// The URL.
        url: String,
        /// Chrome's error text, for example `net::ERR_CONNECTION_REFUSED`.
        reason: String,
    },

    /// A script passed to [`crate::Page::evaluate`] threw.
    #[error("the script threw: {0}")]
    Script(String),
}

impl BrowserError {
    /// Whether the DevTools connection itself broke ([`BrowserError::Closed`], [`BrowserError::WebSocket`],
    /// [`BrowserError::Io`] or [`BrowserError::BadMessage`]), so the browser should be reset. Everything else
    /// (a page that failed to load or took too long, a refused URL, a script that threw, an error answer)
    /// leaves the connection usable: keep Chrome and report the error.
    pub fn is_transport(&self) -> bool {
        matches!(
            self,
            Self::Closed | Self::WebSocket(_) | Self::Io { .. } | Self::BadMessage(_)
        )
    }

    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

/// The result type of the DevTools client's fallible operations.
pub type Result<T, E = BrowserError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_broken_connection_is_a_transport_error() {
        let transport = [
            BrowserError::Closed,
            BrowserError::WebSocket("reset".into()),
            BrowserError::io("read", std::io::Error::other("x")),
            BrowserError::BadMessage("garbage".into()),
        ];
        for e in &transport {
            assert!(e.is_transport(), "{e}");
        }
        let page = [
            BrowserError::Navigation {
                url: "http://localhost:3000/".into(),
                reason: "net::ERR_CONNECTION_REFUSED".into(),
            },
            BrowserError::HostNotAllowed {
                host: "example.com:443".into(),
                allowed: "no host".into(),
            },
            BrowserError::Script("boom".into()),
            BrowserError::Timeout {
                method: "the page load".into(),
                timeout: Duration::from_secs(1),
            },
            BrowserError::Protocol {
                method: "Page.navigate".into(),
                code: -32000,
                message: "x".into(),
            },
        ];
        for e in &page {
            assert!(!e.is_transport(), "{e}");
        }
    }
}
