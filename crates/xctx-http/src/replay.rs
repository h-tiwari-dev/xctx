//! Re-sending a saved request (`http.replay`, the design's `POST /checkout x11`) and comparing two exchanges
//! (`http.diff`).

use serde::{Deserialize, Serialize};
use similar::TextDiff;

use crate::client::{SendOptions, send};
use crate::error::{HttpError, Result};
use crate::exchange::Exchange;
use crate::history::HttpHistory;

/// The most sends one replay makes.
pub const MAX_REPLAYS: u32 = 100;

/// Headers left out of [`diff`], because they differ on every response.
pub const VOLATILE_HEADERS: &[&str] = &["date"];

/// Send the request of exchange `id` in `history` again, `times` times one after another, with `options`.
///
/// Returns the new exchanges in order, not yet recorded: pass them to [`HttpHistory::record_all`]. Fails when
/// no exchange has that id, when `times` is 0 or above [`MAX_REPLAYS`], or when `options` refuse the request
/// (its host is no longer allowed, say); nothing is sent in those cases.
pub fn replay(
    history: &HttpHistory,
    id: u64,
    times: u32,
    options: &SendOptions,
) -> Result<Vec<Exchange>> {
    if times == 0 || times > MAX_REPLAYS {
        return Err(HttpError::BadCount {
            asked: times,
            max: MAX_REPLAYS,
        });
    }
    let request = &history.get(id).ok_or(HttpError::NotFound(id))?.request;
    (0..times).map(|_| send(request, options)).collect()
}

/// A header that differs between two responses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum HeaderChange {
    /// Only the second response has it.
    Added {
        /// Its name, lower-cased.
        name: String,
        /// Its value in the second response.
        value: String,
    },
    /// Only the first response has it.
    Removed {
        /// Its name, lower-cased.
        name: String,
        /// Its value in the first response.
        value: String,
    },
    /// Both have it with different values.
    Changed {
        /// Its name, lower-cased.
        name: String,
        /// Its value in the first response.
        from: String,
        /// Its value in the second response.
        to: String,
    },
}

/// How the second of two exchanges differs from the first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeDiff {
    /// The first exchange's id.
    pub a: u64,
    /// The second exchange's id.
    pub b: u64,
    /// The first exchange's status, `None` when it got no response.
    pub a_status: Option<u16>,
    /// The second exchange's status, `None` when it got no response.
    pub b_status: Option<u16>,
    /// True when the statuses differ (a response against no response counts).
    pub status_changed: bool,
    /// The response headers that differ, by name, leaving out [`VOLATILE_HEADERS`]. Repeated headers are
    /// compared as their values joined with `, `.
    pub headers: Vec<HeaderChange>,
    /// A unified diff of the response bodies (a failure's message stands in for the body), one line per
    /// entry, with `---`/`+++` headers naming the exchanges. Empty when the bodies are the same.
    pub body: Vec<String>,
    /// The first exchange's time in milliseconds.
    pub a_elapsed_ms: u64,
    /// The second exchange's time in milliseconds.
    pub b_elapsed_ms: u64,
}

impl ExchangeDiff {
    /// True when the status, headers and body are all the same.
    pub fn is_same(&self) -> bool {
        !self.status_changed && self.headers.is_empty() && self.body.is_empty()
    }
}

fn header_map(exchange: &Exchange) -> std::collections::BTreeMap<String, String> {
    let mut map: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (name, value) in exchange.response().map(|r| &r.headers[..]).unwrap_or(&[]) {
        let name = name.to_ascii_lowercase();
        if VOLATILE_HEADERS.contains(&name.as_str()) {
            continue;
        }
        map.entry(name)
            .and_modify(|v| {
                v.push_str(", ");
                v.push_str(value);
            })
            .or_insert_with(|| value.clone());
    }
    map
}

fn body_text(exchange: &Exchange) -> String {
    match (exchange.response(), exchange.failure()) {
        (Some(r), _) => r.body.clone(),
        (None, Some(f)) => format!("(no response: {})", f.message),
        (None, None) => String::new(),
    }
}

/// Compare exchange `b` against exchange `a`: status, response headers and body.
pub fn diff(a: &Exchange, b: &Exchange) -> ExchangeDiff {
    let (ha, hb) = (header_map(a), header_map(b));
    let mut headers = Vec::new();
    for (name, from) in &ha {
        match hb.get(name) {
            None => headers.push(HeaderChange::Removed {
                name: name.clone(),
                value: from.clone(),
            }),
            Some(to) if to != from => headers.push(HeaderChange::Changed {
                name: name.clone(),
                from: from.clone(),
                to: to.clone(),
            }),
            Some(_) => {}
        }
    }
    for (name, value) in &hb {
        if !ha.contains_key(name) {
            headers.push(HeaderChange::Added {
                name: name.clone(),
                value: value.clone(),
            });
        }
    }

    let (body_a, body_b) = (body_text(a), body_text(b));
    let body = if body_a == body_b {
        Vec::new()
    } else {
        let text_diff = TextDiff::from_lines(&body_a, &body_b);
        let unified = text_diff
            .unified_diff()
            .context_radius(3)
            .header(&format!("exchange {}", a.id), &format!("exchange {}", b.id))
            .to_string();
        unified
            .lines()
            .filter(|l| !l.starts_with("\\ No newline"))
            .map(str::to_owned)
            .collect()
    };

    ExchangeDiff {
        a: a.id,
        b: b.id,
        a_status: a.status(),
        b_status: b.status(),
        status_changed: a.status() != b.status(),
        headers,
        body,
        a_elapsed_ms: a.elapsed_ms(),
        b_elapsed_ms: b.elapsed_ms(),
    }
}
