//! One page (a Chrome tab) driven over its own DevTools connection.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cdp::{Connection, Event};
use crate::error::{BrowserError, Result};
use crate::policy::UrlPolicy;

/// How long a page call (and a navigation's wait for its load event) may take unless changed with
/// [`Page::set_timeout`].
pub const DEFAULT_PAGE_TIMEOUT: Duration = Duration::from_secs(15);

/// The most console entries a page keeps; older ones are dropped first.
pub const MAX_CONSOLE_ENTRIES: usize = 500;

/// The most network errors a page keeps; older ones are dropped first.
pub const MAX_NETWORK_ERRORS: usize = 200;

/// The most characters one console entry's text keeps.
pub const MAX_ENTRY_CHARS: usize = 4000;

/// The most in-flight requests whose URL is remembered (to name a failed one); past that the map is cleared.
const MAX_TRACKED_REQUESTS: usize = 2000;

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
    /// The console method (`log`, `info`, `warning`, `error`, `debug`, ...) or `exception` for an uncaught
    /// exception.
    pub level: String,
    /// The message: the call's arguments joined by spaces, or the exception's description (with its stack).
    /// Cut to [`MAX_ENTRY_CHARS`].
    pub text: String,
    /// Where it came from, as `url:line:column` (1-based), when Chrome said.
    pub location: Option<String>,
}

impl ConsoleEntry {
    /// Whether this is an uncaught exception rather than a `console.*` call.
    pub fn is_exception(&self) -> bool {
        self.level == "exception"
    }
}

/// A request of the page that failed: it got an HTTP status of 400 or more, or it never got an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkError {
    /// The request URL (empty if Chrome did not say).
    pub url: String,
    /// The HTTP status, or `None` when the request failed without an answer.
    pub status: Option<u16>,
    /// The status text (`Not Found`) or Chrome's error (`net::ERR_CONNECTION_REFUSED`).
    pub reason: String,
    /// The resource type Chrome reports (`Document`, `Fetch`, `Script`, `Image`, ...), when it said.
    pub resource_type: Option<String>,
}

/// A rectangle of the page, in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

/// What [`Page::screenshot_with`] captures.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ScreenshotOptions {
    /// Only this rectangle (wins over `full_page`).
    pub clip: Option<Clip>,
    /// The whole scrollable page rather than the viewport.
    pub full_page: bool,
}

/// The console, network and load state collected from a page's events.
#[derive(Debug)]
pub(crate) struct PageState {
    pub(crate) console: BoundedLog<ConsoleEntry>,
    pub(crate) network: BoundedLog<NetworkError>,
    requests: HashMap<String, String>,
    pub(crate) loads: u64,
}

impl PageState {
    pub(crate) fn new() -> Self {
        Self {
            console: BoundedLog::new(MAX_CONSOLE_ENTRIES),
            network: BoundedLog::new(MAX_NETWORK_ERRORS),
            requests: HashMap::new(),
            loads: 0,
        }
    }

    /// Forgets the console and network errors (on navigation).
    pub(crate) fn clear(&mut self) {
        self.console.clear();
        self.network.clear();
        self.requests.clear();
    }

    /// Records what `event` says; events this client does not use are ignored.
    pub(crate) fn handle(&mut self, event: Event) {
        let p = &event.params;
        match event.method.as_str() {
            "Page.loadEventFired" => self.loads += 1,
            "Runtime.consoleAPICalled" => {
                let level = str_at(p, "/type").unwrap_or("log").to_owned();
                let text = p
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|args| args.iter().map(remote_object_text).collect::<Vec<_>>())
                    .unwrap_or_default()
                    .join(" ");
                let location = p.pointer("/stackTrace/callFrames/0").and_then(location);
                self.console.push(ConsoleEntry {
                    level,
                    text: truncate(text),
                    location,
                });
            }
            "Runtime.exceptionThrown" => {
                let Some(details) = p.get("exceptionDetails") else {
                    return;
                };
                let text = str_at(details, "/exception/description")
                    .or_else(|| str_at(details, "/text"))
                    .unwrap_or("uncaught exception")
                    .to_owned();
                self.console.push(ConsoleEntry {
                    level: "exception".to_owned(),
                    text: truncate(text),
                    location: location(details),
                });
            }
            "Network.requestWillBeSent" => {
                if let (Some(id), Some(url)) = (str_at(p, "/requestId"), str_at(p, "/request/url"))
                {
                    if self.requests.len() >= MAX_TRACKED_REQUESTS {
                        self.requests.clear();
                    }
                    self.requests.insert(id.to_owned(), url.to_owned());
                }
            }
            "Network.responseReceived" => {
                let status = p
                    .pointer("/response/status")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                if status >= 400.0 {
                    self.network.push(NetworkError {
                        url: str_at(p, "/response/url").unwrap_or_default().to_owned(),
                        status: Some(status as u16),
                        reason: str_at(p, "/response/statusText")
                            .unwrap_or_default()
                            .to_owned(),
                        resource_type: str_at(p, "/type").map(str::to_owned),
                    });
                }
            }
            "Network.loadingFinished" => {
                if let Some(id) = str_at(p, "/requestId") {
                    self.requests.remove(id);
                }
            }
            "Network.loadingFailed" => {
                let url = str_at(p, "/requestId")
                    .and_then(|id| self.requests.remove(id))
                    .unwrap_or_default();
                // Requests the page or a navigation cancelled are not failures of the app.
                if p.get("canceled").and_then(Value::as_bool) == Some(true) {
                    return;
                }
                self.network.push(NetworkError {
                    url,
                    status: None,
                    reason: str_at(p, "/errorText").unwrap_or("failed").to_owned(),
                    resource_type: str_at(p, "/type").map(str::to_owned),
                });
            }
            _ => {}
        }
    }
}

fn str_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(Value::as_str)
}

/// How a `Runtime.RemoteObject` argument of a console call reads.
fn remote_object_text(arg: &Value) -> String {
    match arg.get("value") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => str_at(arg, "/unserializableValue")
            .or_else(|| str_at(arg, "/description"))
            .or_else(|| str_at(arg, "/type"))
            .unwrap_or_default()
            .to_owned(),
    }
}

/// `url:line:column` (1-based) from a call frame or exception details (0-based in CDP).
fn location(v: &Value) -> Option<String> {
    let url = str_at(v, "/url").filter(|u| !u.is_empty())?;
    let line = v.get("lineNumber").and_then(Value::as_u64).unwrap_or(0) + 1;
    let column = v.get("columnNumber").and_then(Value::as_u64).unwrap_or(0) + 1;
    Some(format!("{url}:{line}:{column}"))
}

fn truncate(mut text: String) -> String {
    if let Some((cut, _)) = text.char_indices().nth(MAX_ENTRY_CHARS) {
        text.truncate(cut);
        text.push('…');
    }
    text
}

/// A page (a Chrome tab) on its own DevTools connection, with the page, runtime and network domains
/// enabled so its console and failed requests are collected as they happen.
///
/// Events are read whenever a call waits for its answer and in [`Page::pump`]; nothing reads them in the
/// background, so a caller that wants the latest console pumps first ([`Page::console`] and
/// [`Page::network_errors`] do a short non-blocking pump themselves). Both lists are bounded
/// ([`MAX_CONSOLE_ENTRIES`], [`MAX_NETWORK_ERRORS`]) and cleared by each [`Page::open`] and [`Page::reload`],
/// so they describe the current page. Dropping a `Page` closes its connection but leaves the tab open; call
/// [`Page::close`] to close the tab.
pub struct Page {
    conn: Connection,
    port: u16,
    target_id: String,
    policy: UrlPolicy,
    timeout: Duration,
    state: PageState,
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("port", &self.port)
            .field("target_id", &self.target_id)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl Page {
    /// Connects to the page target `target_id` at `ws_url` on the browser at `port`, and enables the
    /// domains it collects from.
    pub(crate) fn connect(
        port: u16,
        target_id: String,
        ws_url: &str,
        policy: UrlPolicy,
    ) -> Result<Self> {
        let conn = Connection::connect(ws_url)?;
        let mut page = Self {
            conn,
            port,
            target_id,
            policy,
            timeout: DEFAULT_PAGE_TIMEOUT,
            state: PageState::new(),
        };
        for domain in ["Page.enable", "Runtime.enable", "Network.enable"] {
            page.call(domain, json!({}))?;
        }
        Ok(page)
    }

    /// The DevTools id of this page's target.
    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    /// The policy [`Page::open`] checks.
    pub fn policy(&self) -> &UrlPolicy {
        &self.policy
    }

    /// Replaces the policy [`Page::open`] checks (for example to add a space's base URL hosts).
    pub fn set_policy(&mut self, policy: UrlPolicy) {
        self.policy = policy;
    }

    /// How long each call, and each wait for a page load, may take.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Navigates to `url` and waits for its load event. The URL must pass the page's [`UrlPolicy`];
    /// a refused URL fails before anything is sent. Clears the console and network errors first.
    ///
    /// Fails if Chrome cannot load the page (for example `net::ERR_CONNECTION_REFUSED`), or with
    /// [`BrowserError::Timeout`] if the load event does not come in time. A navigation within the same
    /// document (only the `#fragment` changes) returns at once.
    pub fn open(&mut self, url: &str) -> Result<()> {
        let checked = self.policy.check(url)?;
        self.state.clear();
        let loads = self.state.loads;
        let result = self.call("Page.navigate", json!({ "url": checked.as_str() }))?;
        if let Some(error) = result
            .get("errorText")
            .and_then(Value::as_str)
            .filter(|e| !e.is_empty())
        {
            return Err(BrowserError::Navigation {
                url: checked.to_string(),
                reason: error.to_owned(),
            });
        }
        if result.get("loaderId").is_none() {
            return Ok(());
        }
        self.wait_for_load(loads)
    }

    /// Reloads the page and waits for its load event. Clears the console and network errors first.
    pub fn reload(&mut self) -> Result<()> {
        self.state.clear();
        let loads = self.state.loads;
        self.call("Page.reload", json!({ "ignoreCache": true }))?;
        self.wait_for_load(loads)
    }

    /// The page's current URL.
    pub fn url(&mut self) -> Result<String> {
        let value = self.evaluate("location.href")?;
        Ok(value.as_str().unwrap_or_default().to_owned())
    }

    /// A PNG of the viewport.
    pub fn screenshot(&mut self) -> Result<Vec<u8>> {
        self.screenshot_with(ScreenshotOptions::default())
    }

    /// A PNG of the viewport, the whole page or a rectangle of it.
    pub fn screenshot_with(&mut self, options: ScreenshotOptions) -> Result<Vec<u8>> {
        let mut params = json!({ "format": "png" });
        let clip = match options.clip {
            Some(clip) => Some(clip),
            None if options.full_page => {
                let metrics = self.call("Page.getLayoutMetrics", json!({}))?;
                let size = metrics
                    .get("cssContentSize")
                    .or_else(|| metrics.get("contentSize"))
                    .ok_or_else(|| BrowserError::BadMessage("no content size".to_owned()))?;
                let dim = |k: &str| size.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                Some(Clip {
                    x: 0.0,
                    y: 0.0,
                    width: dim("width").max(1.0),
                    height: dim("height").max(1.0),
                })
            }
            None => None,
        };
        if let Some(clip) = clip {
            params["clip"] = json!({
                "x": clip.x, "y": clip.y, "width": clip.width, "height": clip.height, "scale": 1,
            });
            params["captureBeyondViewport"] = json!(true);
        }
        let result = self.call("Page.captureScreenshot", params)?;
        let data = result
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| BrowserError::BadMessage("a screenshot without data".to_owned()))?;
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| BrowserError::BadMessage(format!("screenshot is not base64: {e}")))
    }

    /// The console since the last navigation, oldest first (after reading the events already waiting).
    pub fn console(&mut self) -> Result<Vec<ConsoleEntry>> {
        self.pump(Duration::ZERO)?;
        Ok(self.state.console.iter().cloned().collect())
    }

    /// The failed requests since the last navigation, oldest first (after reading the events already
    /// waiting).
    pub fn network_errors(&mut self) -> Result<Vec<NetworkError>> {
        self.pump(Duration::ZERO)?;
        Ok(self.state.network.iter().cloned().collect())
    }

    /// How many console entries and network errors were dropped to stay within the bounds since the last
    /// navigation, as `(console, network)`.
    pub fn dropped(&self) -> (u64, u64) {
        (self.state.console.dropped(), self.state.network.dropped())
    }

    /// Reads events for `wait` (or only those already waiting, when `wait` is zero), so late console
    /// messages and requests are collected.
    pub fn pump(&mut self, wait: Duration) -> Result<()> {
        let deadline = Instant::now() + wait;
        let state = &mut self.state;
        self.conn.pump(deadline, &mut |e| {
            state.handle(e);
            false
        })?;
        Ok(())
    }

    /// Runs the JavaScript expression `expression` in the page and returns its value as JSON (a promise is
    /// awaited). Values JSON cannot hold (functions, DOM nodes, `undefined`) come back as `null`.
    ///
    /// This runs code with the page's full privileges (its cookies, storage and same-origin requests), so
    /// callers must only pass expressions they wrote, never text from a page, a log or an agent without the
    /// user's approval. A throw is returned as [`BrowserError::Script`].
    pub fn evaluate(&mut self, expression: &str) -> Result<Value> {
        let result = self.call(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
            }),
        )?;
        if let Some(details) = result.get("exceptionDetails") {
            let text = str_at(details, "/exception/description")
                .or_else(|| str_at(details, "/text"))
                .unwrap_or("exception");
            return Err(BrowserError::Script(text.to_owned()));
        }
        Ok(result
            .pointer("/result/value")
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// Closes the tab.
    pub fn close(mut self) -> Result<()> {
        self.conn.close();
        crate::http::get(
            self.port,
            &format!("/json/close/{}", self.target_id),
            self.timeout,
        )
        .map(drop)
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let state = &mut self.state;
        self.conn
            .call(method, params, self.timeout, &mut |e| state.handle(e))
    }

    fn wait_for_load(&mut self, loads_before: u64) -> Result<()> {
        if self.state.loads > loads_before {
            return Ok(());
        }
        let deadline = Instant::now() + self.timeout;
        let state = &mut self.state;
        let loaded = self.conn.pump(deadline, &mut |e| {
            state.handle(e);
            state.loads > loads_before
        })?;
        if loaded {
            Ok(())
        } else {
            Err(BrowserError::Timeout {
                method: "the page load".to_owned(),
                timeout: self.timeout,
            })
        }
    }
}
