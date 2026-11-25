//! The API client core behind the http pane and the `http.send`, `http.replay` and `http.diff` tools: sending
//! requests to a space's own origins only (scheme, host and port), a bounded history per space on disk
//! (`<data dir>/spaces/<slug>/http.json`), replaying a saved request, diffing two exchanges, the
//! `response.status` and `response.slow` events, and masking secrets before an exchange is shown.
//!
//! Sending is blocking (through `ureq`, no async runtime); the app calls [`send`] and [`replay`] from a
//! background thread.

#![warn(missing_docs)]

pub mod client;
pub mod error;
pub mod events;
pub mod exchange;
pub mod history;
pub mod replay;

pub use client::{
    DEFAULT_MAX_BODY_BYTES, DEFAULT_MAX_REDIRECTS, DEFAULT_TIMEOUT, LOCAL_HOSTS, SendOptions,
    host_of, send,
};
pub use error::{HttpError, Result};
pub use events::{EventRules, HttpEvent, RESPONSE_SLOW, RESPONSE_STATUS, events};
pub use exchange::{
    Exchange, Failure, FailureKind, Outcome, Request, Response, SENSITIVE_HEADER_SUFFIXES,
    SENSITIVE_HEADERS, SENSITIVE_QUERY_KEYS, is_masked_value, is_sensitive_header,
    is_sensitive_query_key, mask_url,
};
pub use history::{
    HTTP_FILE, HTTP_VERSION, HttpHistory, MAX_HISTORY, QUARANTINE_SUFFIX, http_path, load,
    quarantine, save,
};
pub use replay::{ExchangeDiff, HeaderChange, MAX_REPLAYS, VOLATILE_HEADERS, diff, replay};
