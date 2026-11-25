//! The error type of the browser plugin's page actions.

use std::time::Duration;

/// Everything that can go wrong opening, driving or reading a page of the live browser.
///
/// Every variant's message is written to be shown to the user (and to an agent) as it is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BrowserError {
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
    #[error("{host} is outside the space's app; the page tools only open {allowed}")]
    HostNotAllowed {
        /// The host that was refused.
        host: String,
        /// The allowed hosts, comma separated (or `no host` when there are none).
        allowed: String,
    },

    /// The page is on a site outside the agents' policy (the user browsed there, or an action navigated
    /// there): the tools neither read, capture nor act on it. The message does not say where the page is.
    #[error(
        "the page is on another site, outside the space's app; page.open a page of the app first"
    )]
    OtherSite,
    /// The page could not be loaded.
    #[error("{url} could not be loaded: {reason}")]
    Navigation {
        /// The URL.
        url: String,
        /// WebKit's error text, for example `Could not connect to the server.`
        reason: String,
    },

    /// Something did not finish in time.
    #[error("{what} did not finish within {timeout:?}")]
    Timeout {
        /// What was waited for, for example `the page load`.
        what: String,
        /// How long was waited.
        timeout: Duration,
    },

    /// A script run in the page threw, or WebKit could not run it.
    #[error("the script failed: {0}")]
    Script(String),

    /// The element an action targets cannot be used: nothing matches its selector, it has no box to click,
    /// or it cannot take text.
    #[error("{selector}: {reason}")]
    Element {
        /// The CSS selector as given.
        selector: String,
        /// What is wrong, for example `no element matches`.
        reason: String,
    },

    /// WebKit could not take the screenshot.
    #[error("the screenshot failed: {0}")]
    Snapshot(String),

    /// The live browser could not be made, or it was closed (the pane was unmounted or its space closed).
    #[error("the browser is not available: {0}")]
    Unavailable(String),
}

impl BrowserError {
    /// An [`BrowserError::Element`] error for `selector`.
    pub fn element(selector: &str, reason: &str) -> Self {
        Self::Element {
            selector: selector.to_owned(),
            reason: reason.to_owned(),
        }
    }
}

/// The result type of the browser plugin's fallible operations.
pub type Result<T, E = BrowserError> = std::result::Result<T, E>;
