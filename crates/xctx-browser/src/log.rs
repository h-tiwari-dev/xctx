//! The page's console messages and failed requests, as the capture script reports them.
//!
//! The live browser adds [`capture_script`] to every page at document start. It wraps `console.*`, listens for
//! uncaught errors (and resources that failed to load) and unhandled promise rejections, and reports `fetch`
//! and `XMLHttpRequest` calls that failed or answered with a status of 400 or more. Each report is one JSON
//! string posted to the [`MESSAGE_HANDLER`] script message handler; [`parse_message`] reads it and
//! [`PageLog`] keeps the newest of each kind.
//!
//! A page can post to the handler itself, so what arrives is treated as the page's text: it is capped here and
//! masked by the app before it is shown or returned.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::script::js_string;

/// The name of the script message handler the capture script posts to.
pub const MESSAGE_HANDLER: &str = "xctxPage";

/// The most console entries a page keeps; older ones are dropped first.
pub const MAX_CONSOLE_ENTRIES: usize = 500;

/// The most network errors a page keeps; older ones are dropped first.
pub const MAX_NETWORK_ERRORS: usize = 200;

/// The most characters one console entry's text keeps.
pub const MAX_ENTRY_CHARS: usize = 4000;

/// The most characters of a URL or a reason kept.
const MAX_FIELD_CHARS: usize = 2000;

/// A message bigger than this is dropped unread (the capture script never sends one).
const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// A list that keeps only its newest `capacity` items and counts the ones it dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedLog<T> {
    items: VecDeque<T>,
    capacity: usize,
    dropped: u64,
}

impl<T> BoundedLog<T> {
    /// An empty log keeping at most `capacity` items (at least one).
    pub fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::new(),
            capacity: capacity.max(1),
            dropped: 0,
        }
    }

    /// Adds `item`, dropping the oldest one if the log is full.
    pub fn push(&mut self, item: T) {
        if self.items.len() == self.capacity {
            self.items.pop_front();
            self.dropped += 1;
        }
        self.items.push_back(item);
    }

    /// The items, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.items.iter()
    }

    /// How many items are kept.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether nothing is kept.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// How many items were dropped to stay within capacity since the last [`BoundedLog::clear`].
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Empties the log and resets the dropped count.
    pub fn clear(&mut self) {
        self.items.clear();
        self.dropped = 0;
    }
}

/// One line of the page's console: a `console.*` call or an uncaught exception.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleEntry {
    /// The console method (`log`, `info`, `warning`, `error`, `debug`, `trace`, `assert`), or `exception` for
    /// an uncaught error or an unhandled promise rejection.
    pub level: String,
    /// The message: the call's arguments joined by spaces, or the error's message with its stack. Cut to
    /// [`MAX_ENTRY_CHARS`].
    pub text: String,
    /// Where it came from, as `url:line:column`, when the page's stack said.
    pub location: Option<String>,
}

impl ConsoleEntry {
    /// Whether this is an uncaught exception rather than a `console.*` call.
    pub fn is_exception(&self) -> bool {
        self.level == "exception"
    }

    /// Whether this is an error: `console.error`, a failed `console.assert` or an exception.
    pub fn is_error(&self) -> bool {
        matches!(self.level.as_str(), "error" | "assert" | "exception")
    }
}

/// A request of the page that failed: it got an HTTP status of 400 or more, or it never got an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkError {
    /// The request URL (empty if the page did not say).
    pub url: String,
    /// The HTTP status, or `None` when the request failed without an answer.
    pub status: Option<u16>,
    /// The status text (`Not Found`) or why it failed (`Load failed`).
    pub reason: String,
    /// What made the request: `document` (the page itself), `fetch`, `xhr`, or the tag of an element whose
    /// resource failed to load (`img`, `script`, `link`), when known.
    pub resource_type: Option<String>,
}

/// One report of the capture script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageMessage {
    /// A console message or an exception.
    Console(ConsoleEntry),
    /// A failed request.
    Network(NetworkError),
}

/// The console and failed requests of the page shown, both bounded ([`MAX_CONSOLE_ENTRIES`],
/// [`MAX_NETWORK_ERRORS`]). The live browser clears it as each new document commits, so it describes the
/// current page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageLog {
    /// The console, oldest first.
    pub console: BoundedLog<ConsoleEntry>,
    /// The failed requests, oldest first.
    pub network: BoundedLog<NetworkError>,
}

impl Default for PageLog {
    fn default() -> Self {
        Self {
            console: BoundedLog::new(MAX_CONSOLE_ENTRIES),
            network: BoundedLog::new(MAX_NETWORK_ERRORS),
        }
    }
}

impl PageLog {
    /// Keeps `message` in its list.
    pub fn record(&mut self, message: PageMessage) {
        match message {
            PageMessage::Console(entry) => self.console.push(entry),
            PageMessage::Network(error) => self.network.push(error),
        }
    }

    /// Forgets everything (a new document).
    pub fn clear(&mut self) {
        self.console.clear();
        self.network.clear();
    }
}

#[derive(Deserialize)]
struct RawMessage {
    kind: String,
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    status: Option<f64>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    resource_type: Option<String>,
}

/// The report in `body`, a JSON string the capture script posted, or `None` when it is not one (too big, not
/// JSON, an unknown kind). Texts are cut to [`MAX_ENTRY_CHARS`], URLs and reasons to 2000 characters;
/// `console.warn` is reported as level `warning`, and an unknown level as `log`.
pub fn parse_message(body: &str) -> Option<PageMessage> {
    if body.len() > MAX_MESSAGE_BYTES {
        return None;
    }
    let raw: RawMessage = serde_json::from_str(body).ok()?;
    let field = |value: Option<String>| value.map(|v| cut(v, MAX_FIELD_CHARS));
    match raw.kind.as_str() {
        "console" | "exception" => {
            let level = if raw.kind == "exception" {
                "exception"
            } else {
                match raw.level.as_deref().unwrap_or("log") {
                    "warn" | "warning" => "warning",
                    level @ ("log" | "info" | "error" | "debug" | "trace" | "assert") => level,
                    _ => "log",
                }
            };
            Some(PageMessage::Console(ConsoleEntry {
                level: level.to_owned(),
                text: cut(raw.text.unwrap_or_default(), MAX_ENTRY_CHARS),
                location: field(raw.location).filter(|l| !l.is_empty()),
            }))
        }
        "network" => Some(PageMessage::Network(NetworkError {
            url: field(raw.url).unwrap_or_default(),
            status: raw
                .status
                .filter(|s| s.is_finite() && (100.0..1000.0).contains(s))
                .map(|s| s as u16),
            reason: field(raw.reason).unwrap_or_default(),
            resource_type: field(raw.resource_type).filter(|t| !t.is_empty()),
        })),
        _ => None,
    }
}

/// `text` cut to `max` characters, with `…` after a cut.
fn cut(mut text: String, max: usize) -> String {
    if let Some((at, _)) = text.char_indices().nth(max) {
        text.truncate(at);
        text.push('…');
    }
    text
}

const CAPTURE: &str = r#"(() => {
  const handlers = window.webkit && window.webkit.messageHandlers;
  const handler = handlers && handlers[__HANDLER__];
  if (!handler || window.__xctxCaptured) return;
  Object.defineProperty(window, '__xctxCaptured', { value: true });
  const stringify = JSON.stringify;
  const post = (message) => {
    try {
      if (typeof message.text === 'string') message.text = message.text.slice(0, __MAX__);
      handler.postMessage(stringify(message));
    } catch (_) {}
  };
  const show = (value) => {
    if (typeof value === 'string') return value;
    if (value instanceof Error) return value.stack ? `${value}\n${value.stack}` : String(value);
    if (value === undefined || typeof value === 'function' || typeof value === 'symbol' || typeof value === 'bigint') return String(value);
    try { const text = stringify(value); return text === undefined ? String(value) : text; } catch (_) { return String(value); }
  };
  const caller = () => {
    const line = String(new Error().stack || '').split('\n')[2] || '';
    const at = line.slice(line.indexOf('@') + 1);
    return /:\d+:\d+$/.test(at) ? at : null;
  };
  for (const level of ['log', 'info', 'warn', 'error', 'debug', 'trace']) {
    const original = console[level];
    if (typeof original !== 'function') continue;
    console[level] = function (...args) {
      post({ kind: 'console', level, text: args.map(show).join(' '), location: caller() });
      return original.apply(this, args);
    };
  }
  const assert = console.assert;
  if (typeof assert === 'function') {
    console.assert = function (condition, ...args) {
      if (!condition) post({ kind: 'console', level: 'assert', text: ['Assertion failed:', ...args.map(show)].join(' '), location: caller() });
      return assert.call(this, condition, ...args);
    };
  }
  window.addEventListener('error', (event) => {
    const target = event.target;
    if (target && target !== window && target.tagName) {
      const url = target.currentSrc || target.src || target.href || '';
      post({ kind: 'network', url: String(url), status: null, reason: 'failed to load', resource_type: String(target.tagName).toLowerCase() });
      return;
    }
    const error = event.error;
    const text = error && error.stack ? `${event.message}\n${error.stack}` : String(event.message || 'uncaught error');
    const location = event.filename ? `${event.filename}:${event.lineno || 0}:${event.colno || 0}` : null;
    post({ kind: 'exception', text, location });
  }, true);
  window.addEventListener('unhandledrejection', (event) => {
    post({ kind: 'exception', text: `Unhandled promise rejection: ${show(event.reason)}`, location: null });
  });
  const fetch = window.fetch;
  if (typeof fetch === 'function') {
    window.fetch = function (input, init) {
      const url = typeof input === 'string' ? input : (input && input.url) || String(input);
      return fetch.apply(this, arguments).then((response) => {
        if (response.status >= 400) post({ kind: 'network', url: response.url || url, status: response.status, reason: response.statusText, resource_type: 'fetch' });
        return response;
      }, (error) => {
        if (!(error && error.name === 'AbortError')) post({ kind: 'network', url, status: null, reason: show((error && error.message) || error), resource_type: 'fetch' });
        throw error;
      });
    };
  }
  const XHR = window.XMLHttpRequest;
  if (XHR) {
    const open = XHR.prototype.open;
    const send = XHR.prototype.send;
    const urls = new WeakMap();
    XHR.prototype.open = function (method, url) {
      urls.set(this, String(url));
      return open.apply(this, arguments);
    };
    XHR.prototype.send = function () {
      let aborted = false;
      this.addEventListener('abort', () => { aborted = true; });
      this.addEventListener('loadend', () => {
        const url = this.responseURL || urls.get(this) || '';
        if (this.status >= 400) post({ kind: 'network', url, status: this.status, reason: this.statusText, resource_type: 'xhr' });
        else if (this.status === 0 && !aborted) post({ kind: 'network', url, status: null, reason: 'failed', resource_type: 'xhr' });
      });
      return send.apply(this, arguments);
    };
  }
})();"#;

/// The script the live browser adds at document start of every page (main frame only), posting what it
/// captures to [`MESSAGE_HANDLER`] as JSON strings that [`parse_message`] reads.
pub fn capture_script() -> String {
    CAPTURE
        .replace("__HANDLER__", &js_string(MESSAGE_HANDLER))
        .replace("__MAX__", &MAX_ENTRY_CHARS.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn console(body: &str) -> Option<ConsoleEntry> {
        match parse_message(body)? {
            PageMessage::Console(entry) => Some(entry),
            PageMessage::Network(_) => None,
        }
    }

    fn network(body: &str) -> Option<NetworkError> {
        match parse_message(body)? {
            PageMessage::Network(error) => Some(error),
            PageMessage::Console(_) => None,
        }
    }

    #[test]
    fn console_messages_keep_their_level_text_and_location() {
        let entry = console(
            r#"{"kind":"console","level":"warn","text":"low disk","location":"http://localhost:3000/app.js:4:9"}"#,
        );
        assert_eq!(
            entry,
            Some(ConsoleEntry {
                level: "warning".into(),
                text: "low disk".into(),
                location: Some("http://localhost:3000/app.js:4:9".into()),
            })
        );
        let log = console(r#"{"kind":"console","text":"hi","location":null}"#);
        assert_eq!(log.as_ref().map(|e| e.level.as_str()), Some("log"));
        assert_eq!(log.and_then(|e| e.location), None);
        let odd = console(r#"{"kind":"console","level":"table","text":"x","location":""}"#);
        assert_eq!(odd.as_ref().map(|e| e.level.as_str()), Some("log"));
        assert_eq!(odd.and_then(|e| e.location), None);
        let error = console(r#"{"kind":"console","level":"error","text":"bad"}"#);
        assert!(error.is_some_and(|e| e.is_error() && !e.is_exception()));
    }

    #[test]
    fn exceptions_and_rejections_are_exceptions() {
        let thrown = console(
            r#"{"kind":"exception","level":"log","text":"boom\nat x","location":"http://localhost:3000/:9:1"}"#,
        );
        assert!(thrown.as_ref().is_some_and(ConsoleEntry::is_exception));
        assert_eq!(thrown.map(|e| e.text), Some("boom\nat x".to_owned()));
    }

    #[test]
    fn failed_requests_keep_their_status_or_reason() {
        assert_eq!(
            network(
                r#"{"kind":"network","url":"http://localhost:3000/api","status":404,"reason":"Not Found","resource_type":"fetch"}"#
            ),
            Some(NetworkError {
                url: "http://localhost:3000/api".into(),
                status: Some(404),
                reason: "Not Found".into(),
                resource_type: Some("fetch".into()),
            })
        );
        let refused = network(
            r#"{"kind":"network","url":"http://localhost:9/x","status":null,"reason":"Load failed","resource_type":"xhr"}"#,
        );
        assert_eq!(refused.as_ref().and_then(|e| e.status), None);
        assert_eq!(refused.map(|e| e.reason), Some("Load failed".to_owned()));
        let weird = network(r#"{"kind":"network","status":1e9}"#);
        assert_eq!(weird.as_ref().and_then(|e| e.status), None);
        assert_eq!(weird.and_then(|e| e.resource_type), None);
    }

    #[test]
    fn anything_else_is_ignored() {
        for body in [
            "",
            "not json",
            "42",
            r#"{"level":"log"}"#,
            r#"{"kind":"cookie","text":"x"}"#,
        ] {
            assert_eq!(parse_message(body), None, "{body}");
        }
        let huge = format!(
            r#"{{"kind":"console","text":"{}"}}"#,
            "x".repeat(MAX_MESSAGE_BYTES)
        );
        assert_eq!(parse_message(&huge), None);
    }

    #[test]
    fn long_texts_are_cut() {
        let body = format!(
            r#"{{"kind":"console","text":"{}","location":"{}"}}"#,
            "é".repeat(MAX_ENTRY_CHARS + 10),
            "u".repeat(MAX_FIELD_CHARS + 10)
        );
        let entry = console(&body);
        assert_eq!(
            entry.as_ref().map(|e| e.text.chars().count()),
            Some(MAX_ENTRY_CHARS + 1)
        );
        assert!(entry.as_ref().is_some_and(|e| e.text.ends_with('…')));
        assert_eq!(
            entry.and_then(|e| e.location).map(|l| l.chars().count()),
            Some(MAX_FIELD_CHARS + 1)
        );
    }

    #[test]
    fn the_log_keeps_the_newest_and_clears() {
        let mut log = PageLog::default();
        for i in 0..(MAX_CONSOLE_ENTRIES + 3) {
            log.record(PageMessage::Console(ConsoleEntry {
                level: "log".into(),
                text: i.to_string(),
                location: None,
            }));
        }
        assert_eq!(log.console.len(), MAX_CONSOLE_ENTRIES);
        assert_eq!(log.console.dropped(), 3);
        assert_eq!(
            log.console.iter().next().map(|e| e.text.as_str()),
            Some("3")
        );
        log.clear();
        assert!(log.console.is_empty() && log.network.is_empty());
        assert_eq!(log.console.dropped(), 0);
    }

    #[test]
    fn the_capture_script_posts_to_the_handler() {
        let script = capture_script();
        assert!(script.contains(r#"handlers["xctxPage"]"#), "{script}");
        assert!(script.contains(&format!("slice(0, {MAX_ENTRY_CHARS})")));
        assert!(!script.contains("__HANDLER__") && !script.contains("__MAX__"));
        for hook in [
            "'warn'",
            "console.assert",
            "'unhandledrejection'",
            "window.fetch",
            "XHR.prototype.send",
        ] {
            assert!(script.contains(hook), "{hook}");
        }
    }
}
