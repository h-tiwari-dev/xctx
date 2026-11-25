//! The events an exchange raises for the plugin system: `response.status` (by default any 5xx) and
//! `response.slow` (by default two seconds or more).

use serde::{Deserialize, Serialize};

use crate::exchange::Exchange;

/// The plugin event name of [`HttpEvent::ResponseStatus`].
pub const RESPONSE_STATUS: &str = "response.status";

/// The plugin event name of [`HttpEvent::ResponseSlow`].
pub const RESPONSE_SLOW: &str = "response.slow";

/// When an exchange raises events. A `None` threshold turns that event off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRules {
    /// Raise `response.status` for a status at or above this.
    pub status_at_least: Option<u16>,
    /// Raise `response.slow` when the send took at least this many milliseconds.
    pub slow_ms: Option<u64>,
}

impl Default for EventRules {
    /// Any 5xx, and anything taking two seconds or more.
    fn default() -> Self {
        EventRules {
            status_at_least: Some(500),
            slow_ms: Some(2000),
        }
    }
}

/// An event raised by an exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum HttpEvent {
    /// `response.status`: the response's status reached the rule's threshold.
    #[serde(rename = "response.status")]
    ResponseStatus {
        /// The status.
        status: u16,
    },
    /// `response.slow`: the send took at least the rule's time, response or not.
    #[serde(rename = "response.slow")]
    ResponseSlow {
        /// How long it took.
        elapsed_ms: u64,
    },
}

impl HttpEvent {
    /// The plugin event name: [`RESPONSE_STATUS`] or [`RESPONSE_SLOW`].
    pub fn name(&self) -> &'static str {
        match self {
            HttpEvent::ResponseStatus { .. } => RESPONSE_STATUS,
            HttpEvent::ResponseSlow { .. } => RESPONSE_SLOW,
        }
    }
}

/// The events `exchange` raises under `rules`, status first. An exchange with no response raises no
/// `response.status`, but can still be slow (a timeout is).
pub fn events(exchange: &Exchange, rules: &EventRules) -> Vec<HttpEvent> {
    let mut out = Vec::new();
    if let (Some(threshold), Some(status)) = (rules.status_at_least, exchange.status())
        && status >= threshold
    {
        out.push(HttpEvent::ResponseStatus { status });
    }
    if let Some(slow) = rules.slow_ms {
        let elapsed_ms = exchange.elapsed_ms();
        if elapsed_ms >= slow {
            out.push(HttpEvent::ResponseSlow { elapsed_ms });
        }
    }
    out
}
