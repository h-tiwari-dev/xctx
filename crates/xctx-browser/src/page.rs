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

/// How many loader ids that reached their load event are remembered, newest kept.
const MAX_LOADED_LOADERS: usize = 32;

/// The most characters [`Page::text`] returns.
pub const MAX_PAGE_TEXT_CHARS: usize = 20_000;

/// How many characters of a clicked element's text [`Clicked::text`] keeps.
const CLICKED_TEXT_CHARS: usize = 80;

/// How long an action waits for a navigation it may have started to commit.
const ACTION_SETTLE: Duration = Duration::from_millis(300);

/// What [`Page::click`] clicked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clicked {
    /// The element's tag name, lowercase (`button`, `a`).
    pub tag: String,
    /// The element's visible text (or value), whitespace collapsed, cut to 80 characters.
    pub text: String,
    /// The page's URL after the click (and any navigation it caused).
    pub url: String,
}

/// What [`Page::type_text`] left in the field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Typed {
    /// The field's value afterwards, or `None` when it is gone (a submit navigated away).
    pub value: Option<String>,
    /// The page's URL afterwards.
    pub url: String,
}

/// What [`Page::text`] read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageText {
    /// The visible text, at most [`MAX_PAGE_TEXT_CHARS`] characters.
    pub text: String,
    /// Whether the text was cut.
    pub truncated: bool,
    /// How many characters the whole text had.
    pub chars: usize,
    /// The page's URL.
    pub url: String,
}

/// `value` as a JavaScript string literal: JSON-encoded, with `<` escaped too so `</script>` cannot close
/// anything should the script ever be embedded in HTML.
pub(crate) fn js_string(value: &str) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| "\"\"".to_owned())
        .replace('<', "\\u003c")
}

/// A script that runs `body` with `el` bound to the first element matching `selector` (or to
/// `document.body` when `selector` is `None`), returning `null` when nothing matches.
pub(crate) fn element_script(selector: Option<&str>, body: &str) -> String {
    let find = match selector {
        Some(selector) => format!("document.querySelector({})", js_string(selector)),
        None => "document.body".to_owned(),
    };
    format!("(() => {{ const el = {find}; if (!el) return null; {body} }})()")
}

/// `text` cut to `max` characters, and whether it was cut.
pub(crate) fn cap_text(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((at, _)) => (text[..at].to_owned(), true),
        None => (text.to_owned(), false),
    }
}

const CLICK_TARGET: &str = "el.scrollIntoView({ block: 'center', inline: 'center' }); \
    const r = el.getBoundingClientRect(); \
    const text = (el.innerText || el.value || '').replace(/\\s+/g, ' ').trim(); \
    return { tag: el.tagName.toLowerCase(), text, x: r.left + r.width / 2, y: r.top + r.height / 2, \
    boxed: r.width > 0 && r.height > 0 };";

const CLEAR_FIELD: &str = "el.focus(); \
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype \
    : el instanceof HTMLInputElement ? HTMLInputElement.prototype : null; \
    if (proto) { Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ''); } \
    else if (el.isContentEditable) { el.textContent = ''; } \
    else { return { editable: false }; } \
    el.dispatchEvent(new Event('input', { bubbles: true })); \
    return { editable: document.activeElement === el };";

const FIELD_VALUE: &str = "el.dispatchEvent(new Event('change', { bubbles: true })); \
    return 'value' in el && typeof el.value === 'string' ? el.value : el.innerText;";

const READ_VALUE: &str =
    "return 'value' in el && typeof el.value === 'string' ? el.value : el.innerText;";

fn element_error(selector: &str, reason: &str) -> BrowserError {
    BrowserError::Element {
        selector: selector.to_owned(),
        reason: reason.to_owned(),
    }
}

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
    /// The main frame's id, once a navigation said.
    main_frame: Option<String>,
    /// The loader of the main frame's latest committed navigation.
    main_loader: Option<String>,
    /// How many main-frame navigations committed.
    navigations: u64,
    /// Loader ids whose `load` lifecycle event fired, newest last. A navigation waits for its own loader, so
    /// a late load of the previous document cannot end the wait early.
    loaded: VecDeque<String>,
}

impl PageState {
    pub(crate) fn new() -> Self {
        Self {
            console: BoundedLog::new(MAX_CONSOLE_ENTRIES),
            network: BoundedLog::new(MAX_NETWORK_ERRORS),
            requests: HashMap::new(),
            main_frame: None,
            main_loader: None,
            navigations: 0,
            loaded: VecDeque::new(),
        }
    }

    /// Whether the document of loader `loader_id` reached its load event.
    pub(crate) fn is_loaded(&self, loader_id: &str) -> bool {
        self.loaded.iter().any(|l| l == loader_id)
    }

    /// Whether a main-frame navigation committed after `navigations_before` and its document loaded.
    pub(crate) fn reloaded(&self, navigations_before: u64) -> bool {
        self.navigations > navigations_before
            && self
                .main_loader
                .as_deref()
                .is_some_and(|l| self.is_loaded(l))
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
            "Page.lifecycleEvent" => {
                let is_main = match (&self.main_frame, str_at(p, "/frameId")) {
                    (Some(main), Some(frame)) => main == frame,
                    _ => true,
                };
                if str_at(p, "/name") == Some("load")
                    && is_main
                    && let Some(loader) = str_at(p, "/loaderId")
                {
                    if self.loaded.len() >= MAX_LOADED_LOADERS {
                        self.loaded.pop_front();
                    }
                    self.loaded.push_back(loader.to_owned());
                }
            }
            "Page.frameNavigated" => {
                let Some(frame) = p.get("frame") else {
                    return;
                };
                if frame.get("parentId").is_some() {
                    return;
                }
                self.main_frame = str_at(frame, "/id").map(str::to_owned);
                self.main_loader = str_at(frame, "/loaderId").map(str::to_owned);
                self.navigations += 1;
            }
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
        // Lifecycle events name the loader they belong to, so a wait can tell this navigation's load from a
        // late one of the previous document.
        page.call("Page.setLifecycleEventsEnabled", json!({ "enabled": true }))?;
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
        if let Some(frame) = result.get("frameId").and_then(Value::as_str) {
            self.state.main_frame = Some(frame.to_owned());
        }
        let Some(loader) = result.get("loaderId").and_then(Value::as_str) else {
            return Ok(());
        };
        let loader = loader.to_owned();
        self.wait_for_load(|state| state.is_loaded(&loader))
    }

    /// Reloads the page and waits for the reloaded document's load event. Clears the console and network
    /// errors first.
    pub fn reload(&mut self) -> Result<()> {
        self.state.clear();
        let before = self.state.navigations;
        self.call("Page.reload", json!({ "ignoreCache": true }))?;
        self.wait_for_load(|state| state.reloaded(before))
    }

    /// The page's current URL, as [`Page::current_url`] reads it.
    pub fn url(&mut self) -> Result<String> {
        self.current_url()
    }

    /// The URL the page is on now, asked of Chrome (`Page.getNavigationHistory`) rather than of the page's
    /// scripts, so it follows redirects, links the user clicked and scripts that navigated, and the page cannot
    /// fake it. Check it against the policy before showing what the page holds: see
    /// [`Page::allowed_current_url`].
    pub fn current_url(&mut self) -> Result<String> {
        let history = self.call("Page.getNavigationHistory", json!({}))?;
        let index = history
            .get("currentIndex")
            .and_then(Value::as_u64)
            .and_then(|i| usize::try_from(i).ok())
            .ok_or_else(|| BrowserError::BadMessage("a history without currentIndex".to_owned()))?;
        history
            .pointer(&format!("/entries/{index}/url"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                BrowserError::BadMessage("a history without the current entry".to_owned())
            })
    }

    /// [`Page::current_url`] if the page's [`UrlPolicy`] allows it (or it is `about:blank`, a tab that has
    /// not opened anything), and otherwise the policy's error ([`BrowserError::HostNotAllowed`] or
    /// [`BrowserError::UnsupportedScheme`]): call it before a screenshot or reading the console, so a page that
    /// redirected or was clicked away to another site is not captured.
    pub fn allowed_current_url(&mut self) -> Result<String> {
        let url = self.current_url()?;
        if url == "about:blank" {
            return Ok(url);
        }
        self.policy.check(&url)?;
        Ok(url)
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

    /// Clicks the first element matching the CSS `selector`: scrolls it into view and sends a real
    /// (trusted) left click at its centre through `Input.dispatchMouseEvent`, then waits for a navigation the
    /// click started. The page must be on an allowed URL before, and still be after (see
    /// [`Page::allowed_current_url`]); a click that leaves the policy is an error.
    ///
    /// Fails with [`BrowserError::Element`] when nothing matches or the element has no box.
    pub fn click(&mut self, selector: &str) -> Result<Clicked> {
        self.allowed_current_url()?;
        let target = self.evaluate(&element_script(Some(selector), CLICK_TARGET))?;
        if target.is_null() {
            return Err(element_error(selector, "no element matches"));
        }
        if target.get("boxed").and_then(Value::as_bool) != Some(true) {
            return Err(element_error(
                selector,
                "the element has no box to click (hidden or empty)",
            ));
        }
        let at = |k: &str| target.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        let (x, y) = (at("x"), at("y"));
        let before = self.state.navigations;
        self.call(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": x, "y": y }),
        )?;
        for kind in ["mousePressed", "mouseReleased"] {
            self.call(
                "Input.dispatchMouseEvent",
                json!({ "type": kind, "x": x, "y": y, "button": "left", "buttons": 1, "clickCount": 1 }),
            )?;
        }
        self.settle(before)?;
        let text = target.get("text").and_then(Value::as_str).unwrap_or("");
        Ok(Clicked {
            tag: str_at(&target, "/tag").unwrap_or_default().to_owned(),
            text: cap_text(text, CLICKED_TEXT_CHARS).0,
            url: self.allowed_current_url()?,
        })
    }

    /// Types `text` into the first element matching `selector` (an input, a textarea or an editable
    /// element): focuses it, clears it (firing `input` so frameworks see it), inserts the text with
    /// `Input.insertText` and fires `change`. With `submit`, presses Enter afterwards and waits for a
    /// navigation it started. The page must be on an allowed URL before and after, as for [`Page::click`].
    pub fn type_text(&mut self, selector: &str, text: &str, submit: bool) -> Result<Typed> {
        self.allowed_current_url()?;
        let cleared = self.evaluate(&element_script(Some(selector), CLEAR_FIELD))?;
        if cleared.is_null() {
            return Err(element_error(selector, "no element matches"));
        }
        if cleared.get("editable").and_then(Value::as_bool) != Some(true) {
            return Err(element_error(selector, "the element cannot take text"));
        }
        if !text.is_empty() {
            self.call("Input.insertText", json!({ "text": text }))?;
        }
        let mut value = self.evaluate(&element_script(Some(selector), FIELD_VALUE))?;
        if submit {
            let before = self.state.navigations;
            for kind in ["keyDown", "keyUp"] {
                let mut key = json!({
                    "type": kind, "key": "Enter", "code": "Enter",
                    "windowsVirtualKeyCode": 13, "nativeVirtualKeyCode": 13,
                });
                if kind == "keyDown" {
                    key["text"] = json!("\r");
                }
                self.call("Input.dispatchKeyEvent", key)?;
            }
            self.settle(before)?;
            value = self.evaluate(&element_script(Some(selector), READ_VALUE))?;
        }
        Ok(Typed {
            value: value.as_str().map(str::to_owned),
            url: self.allowed_current_url()?,
        })
    }

    /// The visible text (`innerText`) of the first element matching `selector`, or of the whole body,
    /// cut to [`MAX_PAGE_TEXT_CHARS`]. The page must be on an allowed URL.
    pub fn text(&mut self, selector: Option<&str>) -> Result<PageText> {
        let url = self.allowed_current_url()?;
        let value = self.evaluate(&element_script(selector, "return el.innerText;"))?;
        let Some(full) = value.as_str() else {
            return Err(element_error(
                selector.unwrap_or("body"),
                "no element matches",
            ));
        };
        let (text, truncated) = cap_text(full, MAX_PAGE_TEXT_CHARS);
        Ok(PageText {
            text,
            truncated,
            chars: full.chars().count(),
            url,
        })
    }

    /// After an input action: reads events for a moment and, when a main-frame navigation committed since
    /// `navigations_before`, waits for its load.
    fn settle(&mut self, navigations_before: u64) -> Result<()> {
        self.pump(ACTION_SETTLE)?;
        if self.state.navigations > navigations_before {
            self.wait_for_load(|state| state.reloaded(navigations_before))?;
        }
        Ok(())
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

    fn wait_for_load(&mut self, done: impl Fn(&PageState) -> bool) -> Result<()> {
        if done(&self.state) {
            return Ok(());
        }
        let deadline = Instant::now() + self.timeout;
        let state = &mut self.state;
        let loaded = self.conn.pump(deadline, &mut |e| {
            state.handle(e);
            done(state)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn event(method: &str, params: Value) -> Event {
        Event {
            method: method.to_owned(),
            params,
        }
    }

    fn lifecycle(frame: &str, loader: &str, name: &str) -> Event {
        event(
            "Page.lifecycleEvent",
            json!({ "frameId": frame, "loaderId": loader, "name": name, "timestamp": 1.0 }),
        )
    }

    fn navigated(frame: &str, loader: &str, parent: Option<&str>) -> Event {
        let mut f = json!({ "id": frame, "loaderId": loader, "url": "http://localhost:3000/" });
        if let Some(parent) = parent {
            f["parentId"] = json!(parent);
        }
        event("Page.frameNavigated", json!({ "frame": f }))
    }

    #[test]
    fn a_late_load_of_the_previous_document_does_not_end_the_next_wait() {
        let mut state = PageState::new();
        state.handle(navigated("MAIN", "L1", None));
        // The next navigation (loader L2) starts; the first document's load arrives late.
        state.handle(lifecycle("MAIN", "L1", "load"));
        state.handle(event("Page.loadEventFired", json!({ "timestamp": 2.0 })));
        assert!(!state.is_loaded("L2"));
        state.handle(lifecycle("MAIN", "L2", "DOMContentLoaded"));
        assert!(!state.is_loaded("L2"));
        // An iframe's load is not the page's.
        state.handle(lifecycle("CHILD", "L2", "load"));
        assert!(!state.is_loaded("L2"));
        state.handle(lifecycle("MAIN", "L2", "load"));
        assert!(state.is_loaded("L2"));
    }

    #[test]
    fn a_reload_waits_for_the_new_documents_load() {
        let mut state = PageState::new();
        state.handle(navigated("MAIN", "L1", None));
        state.handle(lifecycle("MAIN", "L1", "load"));
        let before = state.navigations;
        assert!(!state.reloaded(before), "the old load does not count");
        state.handle(navigated("CHILD", "C1", Some("MAIN")));
        assert!(
            !state.reloaded(before),
            "an iframe navigating is not the reload"
        );
        state.handle(navigated("MAIN", "L3", None));
        assert!(!state.reloaded(before));
        state.handle(lifecycle("MAIN", "L3", "load"));
        assert!(state.reloaded(before));
    }

    #[test]
    fn only_the_newest_loaders_are_remembered() {
        let mut state = PageState::new();
        for i in 0..(MAX_LOADED_LOADERS + 5) {
            state.handle(lifecycle("MAIN", &format!("L{i}"), "load"));
        }
        assert_eq!(state.loaded.len(), MAX_LOADED_LOADERS);
        assert!(!state.is_loaded("L0"));
        assert!(state.is_loaded(&format!("L{}", MAX_LOADED_LOADERS + 4)));
    }
}

#[cfg(test)]
mod action_tests {
    use super::*;

    const NASTY: &str = r#"a"b'c\d</script><script>alert(1)</script>`${x}`"#;

    #[test]
    fn selectors_and_text_are_json_encoded_into_scripts() {
        let literal = js_string(NASTY);
        assert!(!literal.contains("</script>"), "{literal}");
        assert!(!literal.contains('<'), "{literal}");
        let back: String = serde_json::from_str(&literal).unwrap_or_default();
        assert_eq!(back, NASTY, "the literal reads back as the value");

        let script = element_script(Some(NASTY), "return el.innerText;");
        assert!(script.contains(&format!("document.querySelector({literal})")));
        assert!(
            !script.contains(NASTY),
            "the raw selector is never spliced in"
        );
        assert!(!script.contains("</script>"));
        assert!(element_script(None, "return 1;").contains("const el = document.body;"));
    }

    #[test]
    fn text_is_capped_on_a_character_boundary() {
        assert_eq!(cap_text("héllo", 10), ("héllo".to_owned(), false));
        assert_eq!(cap_text("héllo", 5), ("héllo".to_owned(), false));
        assert_eq!(cap_text("héllo", 2), ("hé".to_owned(), true));
        assert_eq!(cap_text("", 0), (String::new(), false));
        let long = "x".repeat(MAX_PAGE_TEXT_CHARS + 7);
        let (cut, truncated) = cap_text(&long, MAX_PAGE_TEXT_CHARS);
        assert!(truncated);
        assert_eq!(cut.chars().count(), MAX_PAGE_TEXT_CHARS);
    }
}
