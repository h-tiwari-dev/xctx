//! A live `WKWebView` inside a GPUI window: the browser plugin's pane shows it, the user browses in it, and
//! the agents' `page.*` tools drive the same view, so the user watches them work.
//!
//! The view is a subview of GPUI's native `NSView` ([`WindowHost`]), so it draws above GPUI's Metal layer: the
//! app places it over the pane every frame it draws the pane ([`WebView::place`]) and hides it on every frame it
//! does not ([`WebView::end_frame`]). Web Inspector is on (right-click → Inspect Element, macOS 13.3 and
//! later). Each view has its own non-persistent website data store, so spaces share no cookies and nothing is
//! kept on disk.
//!
//! A navigation delegate lets the user go to any web page ([`xctx_browser::user_may_open`]; other schemes are
//! refused, and why is kept as the view's error) and records the main document's failed loads. A script
//! added at document start ([`xctx_browser::capture_script`]) reports the console, uncaught errors and failed
//! requests to a script message handler, kept in a bounded [`PageLog`]. The agents' URL policy is not
//! enforced here: the app checks it before every tool reads or acts on the page.
//!
//! Everything runs on the main thread, and nothing blocks: [`WebView::evaluate`], [`WebView::snapshot`] and
//! [`WebView::next_settle`] return futures that WebKit's completion handlers resolve, for the GPUI foreground
//! executor to await (with a timeout of the caller's choosing). Changes the pane should redraw for are
//! signalled on the [`Changes`] receiver made with the view.

#![warn(missing_docs)]

mod geometry;

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::{DynBlock, RcBlock};
use futures::channel::{mpsc, oneshot};
use gpui::{Bounds, Pixels};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSEvent, NSEventMask, NSImage, NSView,
};
use objc2_foundation::{
    NSDictionary, NSError, NSHTTPURLResponse, NSPoint, NSRect, NSSize, NSString, NSURL,
    NSURLRequest,
};
use objc2_web_kit::{
    WKContentWorld, WKNavigation, WKNavigationAction, WKNavigationActionPolicy,
    WKNavigationDelegate, WKNavigationResponse, WKNavigationResponsePolicy, WKScriptMessage,
    WKScriptMessageHandler, WKSnapshotConfiguration, WKUserContentController, WKUserScript,
    WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration, WKWebsiteDataStore,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::Value;
use xctx_browser::{
    BrowserError, ConsoleEntry, MESSAGE_HANDLER, NetworkError, PageLog, Result, capture_script,
    evaluation_script, parse_evaluation, parse_message, user_may_open,
};

pub use geometry::{Frame, host_frame};

/// The tallest full-page screenshot, in points; a longer page is cut there.
pub const MAX_FULL_PAGE_HEIGHT: f64 = 16_384.0;

/// `NSURLErrorCancelled`: a load stopped because another one replaced it.
const URL_ERROR_CANCELLED: isize = -999;

/// A signal, sent (coalesced) whenever what a view shows changed: a load started, committed, finished or
/// failed, or the page reported to its console. Redraw the pane when one arrives.
pub type Changes = mpsc::Receiver<()>;

/// GPUI's native view of a window, which the browser views are added to. Keep one per window; it is cheap to
/// clone.
#[derive(Clone)]
pub struct WindowHost {
    view: Retained<NSView>,
    mtm: MainThreadMarker,
}

impl std::fmt::Debug for WindowHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowHost").finish_non_exhaustive()
    }
}

impl WindowHost {
    /// The native view of `window`. Fails off the main thread, or when the window has no AppKit view.
    pub fn of(window: &gpui::Window) -> Result<WindowHost> {
        let unavailable = |why: &str| BrowserError::Unavailable(why.to_owned());
        let mtm = MainThreadMarker::new().ok_or_else(|| unavailable("not on the main thread"))?;
        let handle = HasWindowHandle::window_handle(window)
            .map_err(|e| BrowserError::Unavailable(format!("no window handle: {e}")))?;
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
            return Err(unavailable("the window is not an AppKit window"));
        };
        // SAFETY: the handle's `ns_view` is the window's live NSView; retaining it keeps it valid for as long
        // as the host is kept.
        let view = unsafe { Retained::retain(appkit.ns_view.as_ptr().cast::<NSView>()) }
            .ok_or_else(|| unavailable("the window has no view"))?;
        Ok(WindowHost { view, mtm })
    }
}

/// What a view shows now, read from WebKit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageStatus {
    /// The page's URL; `None` before anything was loaded.
    pub url: Option<String>,
    /// The page's title, when it has one.
    pub title: Option<String>,
    /// Whether a load is in progress.
    pub loading: bool,
    /// How far the load is, 0 to 1.
    pub progress: f64,
    /// Whether Back goes anywhere.
    pub can_go_back: bool,
    /// Whether Forward goes anywhere.
    pub can_go_forward: bool,
    /// Why the last load failed or was refused, until the next one starts.
    pub error: Option<String>,
}

/// How a load ended: `Ok` once it finished, or why it failed.
pub type LoadOutcome = std::result::Result<(), String>;

/// What the delegate keeps, shared with the view.
struct State {
    log: PageLog,
    /// The main document's failed status, recorded once its document commits.
    document_error: Option<NetworkError>,
    error: Option<String>,
    /// A local directory this view may read. Only document previews set it; browser views leave it unset.
    local_root: Option<String>,
    settle_waiters: Vec<oneshot::Sender<LoadOutcome>>,
    changes: mpsc::Sender<()>,
}

impl State {
    fn changed(&mut self) {
        // A full channel already holds a signal: the receiver redraws once for both.
        let _ = self.changes.try_send(());
    }

    fn settle(&mut self, outcome: LoadOutcome) {
        for waiter in self.settle_waiters.drain(..) {
            let _ = waiter.send(outcome.clone());
        }
        self.changed();
    }
}

struct DelegateIvars {
    state: Rc<RefCell<State>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and the delegate implements no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "XctxWebViewDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl WKNavigationDelegate for Delegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_action(
            &self,
            web_view: &WKWebView,
            action: &WKNavigationAction,
            decision: &DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            // SAFETY: plain property reads of the action WebKit passed in.
            let (request, target) = unsafe { (action.request(), action.targetFrame()) };
            let url = request_url(&request);
            // SAFETY: as above.
            let main_frame = target.as_ref().is_none_or(|t| unsafe { t.isMainFrame() });
            if !main_frame {
                decision.call((WKNavigationActionPolicy::Allow,));
                return;
            }
            let local_allowed = self
                .ivars()
                .state
                .borrow()
                .local_root
                .as_ref()
                .is_some_and(|root| url == root.trim_end_matches('/') || url.starts_with(root));
            if let Err(why) = user_may_open(&url)
                && !local_allowed
            {
                let mut state = self.ivars().state.borrow_mut();
                state.error = Some(format!("{url} was not opened: {why}"));
                state.changed();
                decision.call((WKNavigationActionPolicy::Cancel,));
                return;
            }
            if target.is_none() {
                // A link or script asking for a new window opens in this view instead.
                decision.call((WKNavigationActionPolicy::Cancel,));
                // SAFETY: loading a request in the view the delegate belongs to, on the main thread.
                let _ = unsafe { web_view.loadRequest(&request) };
                return;
            }
            decision.call((WKNavigationActionPolicy::Allow,));
        }

        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn decide_response(
            &self,
            _web_view: &WKWebView,
            navigation: &WKNavigationResponse,
            decision: &DynBlock<dyn Fn(WKNavigationResponsePolicy)>,
        ) {
            // SAFETY: plain property reads of the response WebKit passed in.
            let (main, response) = unsafe { (navigation.isForMainFrame(), navigation.response()) };
            if main && let Ok(http) = response.downcast::<NSHTTPURLResponse>() {
                let status = http.statusCode();
                if status >= 400 {
                    let url = http.URL().map(|u| url_string(&u)).unwrap_or_default();
                    let reason = NSHTTPURLResponse::localizedStringForStatusCode(status);
                    self.ivars().state.borrow_mut().document_error = Some(NetworkError {
                        url,
                        status: u16::try_from(status).ok(),
                        reason: reason.to_string(),
                        resource_type: Some("document".to_owned()),
                    });
                }
            }
            decision.call((WKNavigationResponsePolicy::Allow,));
        }

        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        fn did_start(&self, _web_view: &WKWebView, _navigation: Option<&WKNavigation>) {
            let mut state = self.ivars().state.borrow_mut();
            state.error = None;
            state.document_error = None;
            state.changed();
        }

        #[unsafe(method(webView:didCommitNavigation:))]
        fn did_commit(&self, _web_view: &WKWebView, _navigation: Option<&WKNavigation>) {
            let mut state = self.ivars().state.borrow_mut();
            // A new document: what the old one logged is not this page's.
            state.log.clear();
            if let Some(error) = state.document_error.take() {
                state.log.network.push(error);
            }
            state.changed();
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, _web_view: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.ivars().state.borrow_mut().settle(Ok(()));
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional(
            &self,
            web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            self.failed(web_view, error, true);
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail(
            &self,
            web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            self.failed(web_view, error, false);
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn did_terminate(&self, _web_view: &WKWebView) {
            let why = "the page's process stopped; reload it".to_owned();
            let mut state = self.ivars().state.borrow_mut();
            state.error = Some(why.clone());
            state.settle(Err(why));
        }
    }

    unsafe impl WKScriptMessageHandler for Delegate {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn did_receive(&self, _controller: &WKUserContentController, message: &WKScriptMessage) {
            // SAFETY: reading the body of the message WebKit passed in.
            let body = unsafe { message.body() };
            let Some(text) = body.downcast_ref::<NSString>() else {
                return;
            };
            if let Some(parsed) = parse_message(&text.to_string()) {
                let mut state = self.ivars().state.borrow_mut();
                state.log.record(parsed);
                state.changed();
            }
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker, state: Rc<RefCell<State>>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars { state });
        // SAFETY: NSObject's designated initialiser.
        unsafe { msg_send![super(this), init] }
    }

    fn failed(&self, web_view: &WKWebView, error: &NSError, provisional: bool) {
        let mut state = self.ivars().state.borrow_mut();
        if error.code() == URL_ERROR_CANCELLED && error.domain().to_string() == "NSURLErrorDomain" {
            // Replaced by another load (a redirect by script, a click, Stop): wait for that one.
            // SAFETY: a property read on the main thread.
            if !unsafe { web_view.isLoading() } {
                state.settle(Err("the load was stopped".to_owned()));
            }
            return;
        }
        let url = failing_url(error)
            .or_else(|| {
                // SAFETY: a property read on the main thread.
                unsafe { web_view.URL() }.map(|u| url_string(&u))
            })
            .unwrap_or_default();
        let reason = error.localizedDescription().to_string();
        if provisional {
            // The new page did not load (the old one stays): record it as a failed request of the view.
            state.log.network.push(NetworkError {
                url: url.clone(),
                status: None,
                reason: reason.clone(),
                resource_type: Some("document".to_owned()),
            });
        }
        let why = BrowserError::Navigation { url, reason }.to_string();
        state.error = Some(why.clone());
        state.settle(Err(why));
    }
}

fn url_string(url: &NSURL) -> String {
    url.absoluteString()
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn request_url(request: &NSURLRequest) -> String {
    request.URL().map(|u| url_string(&u)).unwrap_or_default()
}

/// The URL a load failed on, from the error's user info (`NSErrorFailingURLKey`, an `NSURL`, or the
/// string form).
fn failing_url(error: &NSError) -> Option<String> {
    let info = error.userInfo();
    if let Some(url) = info.objectForKey(&NSString::from_str("NSErrorFailingURLKey"))
        && let Some(url) = url.downcast_ref::<NSURL>()
    {
        return Some(url_string(url));
    }
    let value = info.objectForKey(&NSString::from_str("NSErrorFailingURLStringKey"))?;
    value.downcast_ref::<NSString>().map(|s| s.to_string())
}

struct Inner {
    mtm: MainThreadMarker,
    view: Retained<WKWebView>,
    host: Retained<NSView>,
    /// WebKit holds its navigation delegate weakly: this keeps it alive for as long as the view.
    _delegate: Retained<Delegate>,
    controller: Retained<WKUserContentController>,
    state: Rc<RefCell<State>>,
    monitor: RefCell<Option<Retained<AnyObject>>>,
    closed: Cell<bool>,
    hidden: Cell<bool>,
    drawn: Cell<bool>,
    frame: Cell<Option<Frame>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.close();
    }
}

impl Inner {
    fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        self.release_focus();
        if let Some(monitor) = self.monitor.borrow_mut().take() {
            // SAFETY: the monitor was returned by addLocalMonitorForEventsMatchingMask and not removed yet.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        // SAFETY: AppKit and WebKit calls on the main thread (the view is MainThreadOnly).
        unsafe {
            self.view.stopLoading();
            self.view.setNavigationDelegate(None);
            self.controller
                .removeScriptMessageHandlerForName(&NSString::from_str(MESSAGE_HANDLER));
            self.controller.removeAllUserScripts();
        }
        self.view.removeFromSuperview();
        self.state
            .borrow_mut()
            .settle(Err("the browser was closed".to_owned()));
    }

    /// Hand the keyboard back to GPUI's view if the web view has it.
    fn release_focus(&self) {
        let Some(window) = self.view.window() else {
            return;
        };
        let inside = window
            .firstResponder()
            .and_then(|r| r.downcast::<NSView>().ok())
            .is_some_and(|r| r.isDescendantOf(&self.view));
        if inside {
            window.makeFirstResponder(Some(&self.host));
        }
    }
}

/// One live browser view. Cloning it gives another handle to the same view; it is closed (taken off the
/// window, its loads stopped) by [`WebView::close`] or when the last handle is dropped. Main thread only.
#[derive(Clone)]
pub struct WebView {
    inner: Rc<Inner>,
}

impl std::fmt::Debug for WebView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebView")
            .field("closed", &self.inner.closed.get())
            .field("hidden", &self.inner.hidden.get())
            .finish_non_exhaustive()
    }
}

impl WebView {
    /// A new view in `host`'s window, hidden until it is placed, with Web Inspector on, its own
    /// non-persistent data store, the capture script and the navigation delegate. Returns the view and the
    /// receiver of its [`Changes`].
    pub fn new(host: &WindowHost) -> Result<(WebView, Changes)> {
        let mtm = host.mtm;
        let (changes, receiver) = mpsc::channel(0);
        let state = Rc::new(RefCell::new(State {
            log: PageLog::default(),
            document_error: None,
            error: None,
            local_root: None,
            settle_waiters: Vec::new(),
            changes,
        }));
        let delegate = Delegate::new(mtm, Rc::clone(&state));
        // SAFETY: WebKit and AppKit calls on the main thread (`mtm`), with objects made here.
        let (view, controller) = unsafe {
            let configuration = WKWebViewConfiguration::new(mtm);
            configuration.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(mtm));
            let controller = configuration.userContentController();
            let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
                WKUserScript::alloc(mtm),
                &NSString::from_str(&capture_script()),
                WKUserScriptInjectionTime::AtDocumentStart,
                true,
            );
            controller.addUserScript(&script);
            controller.addScriptMessageHandler_name(
                ProtocolObject::from_ref(&*delegate),
                &NSString::from_str(MESSAGE_HANDLER),
            );
            let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0));
            let view = WKWebView::initWithFrame_configuration(
                WKWebView::alloc(mtm),
                frame,
                &configuration,
            );
            view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            view.setAllowsBackForwardNavigationGestures(true);
            // Web Inspector (right-click → Inspect Element) needs macOS 13.3 or later.
            if view.respondsToSelector(sel!(setInspectable:)) {
                view.setInspectable(true);
            }
            (view, controller)
        };
        view.setHidden(true);
        host.view.addSubview(&view);
        let inner = Rc::new(Inner {
            mtm,
            view,
            host: host.view.clone(),
            _delegate: delegate,
            controller,
            state,
            monitor: RefCell::new(None),
            closed: Cell::new(false),
            hidden: Cell::new(true),
            drawn: Cell::new(false),
            frame: Cell::new(None),
        });
        *inner.monitor.borrow_mut() = focus_monitor(&inner);
        Ok((WebView { inner }, receiver))
    }

    /// Whether the view was closed.
    pub fn is_closed(&self) -> bool {
        self.inner.closed.get()
    }

    /// Takes the view off its window and stops it. Every handle sees it closed; waiting futures fail.
    pub fn close(&self) {
        self.inner.close();
    }

    /// Puts the view over `bounds` (GPUI window coordinates) and shows it, for this frame. Call it from the
    /// prepaint of the element that holds the pane's page area, every frame that element is drawn.
    pub fn place(&self, bounds: Bounds<Pixels>) {
        if self.is_closed() {
            return;
        }
        self.inner.drawn.set(true);
        self.set_frame(bounds);
        self.set_hidden(false);
    }

    /// Ends a frame: hides the view unless [`WebView::place`] was called since the last end. Call it once per
    /// frame after every pane was prepainted (from the paint of an element drawn after them).
    pub fn end_frame(&self) {
        if !self.inner.drawn.replace(false) {
            self.set_hidden(true);
        }
    }

    /// Moves the view over `bounds`, given in GPUI window coordinates (logical pixels, top-left origin).
    pub fn set_frame(&self, bounds: Bounds<Pixels>) {
        let host = &self.inner.host;
        let wanted = host_frame(
            Frame {
                x: bounds.origin.x.to_f64(),
                y: bounds.origin.y.to_f64(),
                width: bounds.size.width.to_f64(),
                height: bounds.size.height.to_f64(),
            },
            host.bounds().size.height,
            host.isFlipped(),
        );
        if self.inner.frame.get() == Some(wanted) {
            return;
        }
        self.inner.frame.set(Some(wanted));
        self.inner.view.setFrame(NSRect::new(
            NSPoint::new(wanted.x, wanted.y),
            NSSize::new(wanted.width, wanted.height),
        ));
    }

    /// Shows or hides the view. Hiding it hands the keyboard back to GPUI if the page had it.
    pub fn set_hidden(&self, hidden: bool) {
        if self.is_closed() || self.inner.hidden.get() == hidden {
            return;
        }
        if hidden {
            self.inner.release_focus();
        }
        self.inner.hidden.set(hidden);
        self.inner.view.setHidden(hidden);
    }

    /// Whether the view is hidden.
    pub fn is_hidden(&self) -> bool {
        self.inner.hidden.get()
    }

    /// Starts loading `url`. Fails for a URL that is not valid or that the user's browsing may not open
    /// ([`xctx_browser::user_may_open`]); the agents' policy is the caller's to check.
    pub fn load(&self, url: &str) -> Result<()> {
        self.check_open()?;
        user_may_open(url)?;
        let invalid = || BrowserError::InvalidUrl {
            url: url.to_owned(),
            reason: "WebKit cannot read it".to_owned(),
        };
        let ns_url = NSURL::URLWithString(&NSString::from_str(url)).ok_or_else(invalid)?;
        let request = NSURLRequest::requestWithURL(&ns_url);
        // SAFETY: a WebKit call on the main thread.
        let _ = unsafe { self.inner.view.loadRequest(&request) };
        Ok(())
    }

    /// Load local `file`, allowing WebKit to read only `root` and its descendants. This is for document previews,
    /// not browsing: both paths are canonicalized and a file outside `root` is refused.
    pub fn load_local_file(&self, file: &Path, root: &Path) -> Result<()> {
        self.check_open()?;
        let invalid = |path: &Path, reason: &str| BrowserError::InvalidUrl {
            url: path.display().to_string(),
            reason: reason.to_owned(),
        };
        let root = root
            .canonicalize()
            .map_err(|e| invalid(root, &format!("cannot resolve the preview root: {e}")))?;
        let file = file
            .canonicalize()
            .map_err(|e| invalid(file, &format!("cannot resolve the document: {e}")))?;
        if !file.starts_with(&root) || !file.is_file() {
            return Err(invalid(
                &file,
                "the document is not a regular file inside the preview root",
            ));
        }
        let file_url = NSURL::fileURLWithPath(&NSString::from_str(&file.to_string_lossy()));
        let root_url =
            NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&root.to_string_lossy()), true);
        let mut allowed = url_string(&root_url);
        if !allowed.ends_with('/') {
            allowed.push('/');
        }
        self.inner.state.borrow_mut().local_root = Some(allowed);
        // SAFETY: a WebKit call on the main thread; both URLs were made above and stay alive for the call.
        let _ = unsafe {
            self.inner
                .view
                .loadFileURL_allowingReadAccessToURL(&file_url, &root_url)
        };
        Ok(())
    }

    /// Reloads the page. Returns `false` when there is nothing to reload.
    pub fn reload(&self) -> bool {
        // SAFETY: WebKit calls on the main thread.
        !self.is_closed() && unsafe { self.inner.view.reload() }.is_some()
    }

    /// Goes back in the view's history, if it can.
    pub fn go_back(&self) {
        if !self.is_closed() {
            // SAFETY: a WebKit call on the main thread.
            let _ = unsafe { self.inner.view.goBack() };
        }
    }

    /// Goes forward in the view's history, if it can.
    pub fn go_forward(&self) {
        if !self.is_closed() {
            // SAFETY: a WebKit call on the main thread.
            let _ = unsafe { self.inner.view.goForward() };
        }
    }

    /// Stops the load in progress.
    pub fn stop(&self) {
        if !self.is_closed() {
            // SAFETY: a WebKit call on the main thread.
            unsafe { self.inner.view.stopLoading() };
        }
    }

    /// The page's URL, `None` before anything was loaded.
    pub fn url(&self) -> Option<String> {
        // SAFETY: a WebKit property read on the main thread.
        unsafe { self.inner.view.URL() }
            .map(|u| url_string(&u))
            .filter(|u| !u.is_empty())
    }

    /// Whether a load is in progress.
    pub fn is_loading(&self) -> bool {
        // SAFETY: a WebKit property read on the main thread.
        !self.is_closed() && unsafe { self.inner.view.isLoading() }
    }

    /// What the view shows now.
    pub fn status(&self) -> PageStatus {
        let view = &self.inner.view;
        // SAFETY: WebKit property reads on the main thread.
        unsafe {
            PageStatus {
                url: self.url(),
                title: view
                    .title()
                    .map(|t| t.to_string())
                    .filter(|t| !t.trim().is_empty()),
                loading: self.is_loading(),
                progress: view.estimatedProgress(),
                can_go_back: view.canGoBack(),
                can_go_forward: view.canGoForward(),
                error: self.inner.state.borrow().error.clone(),
            }
        }
    }

    /// The current page's console, oldest first.
    pub fn console(&self) -> Vec<ConsoleEntry> {
        self.inner
            .state
            .borrow()
            .log
            .console
            .iter()
            .cloned()
            .collect()
    }

    /// The current page's failed requests (its own document's first, when it failed), oldest first.
    pub fn network_errors(&self) -> Vec<NetworkError> {
        self.inner
            .state
            .borrow()
            .log
            .network
            .iter()
            .cloned()
            .collect()
    }

    /// Resolves when the next load settles: it finished, failed, or the view closed. A load that is replaced
    /// by another waits for that one.
    pub fn next_settle(&self) -> impl Future<Output = LoadOutcome> + use<> {
        let (sender, receiver) = oneshot::channel();
        if self.is_closed() {
            let _ = sender.send(Err("the browser was closed".to_owned()));
        } else {
            self.inner.state.borrow_mut().settle_waiters.push(sender);
        }
        async move {
            receiver
                .await
                .unwrap_or_else(|_| Err("the browser was closed".to_owned()))
        }
    }

    /// Runs the JavaScript expression `script` in the main frame and returns its value as JSON (`undefined` as
    /// `null`). It runs in WebKit's client content world: it shares the page's DOM but not its JavaScript
    /// globals, so the page cannot tamper with what the script uses. Only pass scripts the app built (see
    /// `xctx_browser::script`). A throw is [`BrowserError::Script`].
    pub fn evaluate(&self, script: &str) -> impl Future<Output = Result<Value>> + use<> {
        let (sender, receiver) = oneshot::channel::<std::result::Result<String, String>>();
        if self.is_closed() {
            let _ = sender.send(Err("the browser was closed".to_owned()));
        } else {
            let sender = Cell::new(Some(sender));
            let handler = RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
                let Some(sender) = sender.take() else {
                    return;
                };
                // SAFETY: WebKit passes valid objects (or null) that live for the duration of the call.
                let outcome = unsafe {
                    if let Some(error) = error.as_ref() {
                        Err(error.localizedDescription().to_string())
                    } else {
                        match result.as_ref().and_then(|r| r.downcast_ref::<NSString>()) {
                            Some(answer) => Ok(answer.to_string()),
                            None => Err("the script gave no answer".to_owned()),
                        }
                    }
                };
                let _ = sender.send(outcome);
            });
            let world = unsafe { WKContentWorld::defaultClientWorld(self.inner.mtm) };
            // SAFETY: a WebKit call on the main thread; the block is copied by WebKit.
            unsafe {
                self.inner
                    .view
                    .evaluateJavaScript_inFrame_inContentWorld_completionHandler(
                        &NSString::from_str(&evaluation_script(script)),
                        None,
                        &world,
                        Some(&handler),
                    );
            }
        }
        async move {
            let answer = receiver
                .await
                .map_err(|_| BrowserError::Script("the page went away".to_owned()))?
                .map_err(BrowserError::Script)?;
            parse_evaluation(&answer)
        }
    }

    /// A PNG of the page as the view shows it, or of the whole page from its top (at most
    /// [`MAX_FULL_PAGE_HEIGHT`] points tall) with `full_page`.
    pub fn snapshot(&self, full_page: bool) -> impl Future<Output = Result<Vec<u8>>> + use<> {
        let this = self.clone();
        async move {
            this.check_open()?;
            let size = if full_page {
                let answer = this
                    .evaluate(
                        "({ width: Math.max(document.documentElement.clientWidth, window.innerWidth), \
                         height: Math.max(document.documentElement.scrollHeight, \
                         document.body ? document.body.scrollHeight : 0, window.innerHeight) })",
                    )
                    .await?;
                let dim = |k: &str| answer.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                Some((
                    dim("width").max(1.0),
                    dim("height").clamp(1.0, MAX_FULL_PAGE_HEIGHT),
                ))
            } else {
                None
            };
            let (sender, receiver) = oneshot::channel::<std::result::Result<Vec<u8>, String>>();
            let sender = Cell::new(Some(sender));
            let handler = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
                let Some(sender) = sender.take() else {
                    return;
                };
                // SAFETY: WebKit passes a valid image or error (or null) for the duration of the call.
                let outcome = unsafe {
                    match (image.as_ref(), error.as_ref()) {
                        (_, Some(error)) => Err(error.localizedDescription().to_string()),
                        (Some(image), None) => png_of(image),
                        (None, None) => Err("WebKit returned no image".to_owned()),
                    }
                };
                let _ = sender.send(outcome);
            });
            // SAFETY: WebKit calls on the main thread; the block is copied by WebKit.
            unsafe {
                let configuration = WKSnapshotConfiguration::new(this.inner.mtm);
                if let Some((width, height)) = size {
                    configuration.setRect(NSRect::new(
                        NSPoint::new(0.0, 0.0),
                        NSSize::new(width, height),
                    ));
                }
                this.inner
                    .view
                    .takeSnapshotWithConfiguration_completionHandler(
                        Some(&configuration),
                        &handler,
                    );
            }
            receiver
                .await
                .map_err(|_| BrowserError::Snapshot("the page went away".to_owned()))?
                .map_err(BrowserError::Snapshot)
        }
    }

    fn check_open(&self) -> Result<()> {
        if self.is_closed() {
            Err(BrowserError::Unavailable(
                "the browser pane was closed".to_owned(),
            ))
        } else {
            Ok(())
        }
    }
}

/// The PNG encoding of `image`.
fn png_of(image: &NSImage) -> std::result::Result<Vec<u8>, String> {
    let tiff = image
        .TIFFRepresentation()
        .ok_or("the image has no bitmap")?;
    let bitmap = NSBitmapImageRep::imageRepWithData(&tiff).ok_or("the image has no bitmap")?;
    // SAFETY: an empty properties dictionary is valid for every file type.
    let png = unsafe {
        bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }
    .ok_or("the image could not be encoded as PNG")?;
    Ok(png.to_vec())
}

/// A local event monitor that hands the keyboard back to GPUI's view when the user clicks outside the web view
/// while the page has it (GPUI's view does not take first responder on its own).
fn focus_monitor(inner: &Rc<Inner>) -> Option<Retained<AnyObject>> {
    let view = inner.view.clone();
    let host = inner.host.clone();
    let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit passes a valid event for the duration of the call.
        let ev = unsafe { event.as_ref() };
        if let Some(mtm) = MainThreadMarker::new()
            && !view.isHidden()
            && let Some(window) = view.window()
            && ev.window(mtm).is_some_and(|w| w == window)
            && window
                .firstResponder()
                .and_then(|r| r.downcast::<NSView>().ok())
                .is_some_and(|r| r.isDescendantOf(&view))
        {
            let point = view.convertPoint_fromView(ev.locationInWindow(), None);
            let bounds = view.bounds();
            let inside = point.x >= bounds.origin.x
                && point.y >= bounds.origin.y
                && point.x <= bounds.origin.x + bounds.size.width
                && point.y <= bounds.origin.y + bounds.size.height;
            if !inside {
                window.makeFirstResponder(Some(&host));
            }
        }
        event.as_ptr()
    });
    // SAFETY: installing a monitor on the main thread; AppKit copies the block.
    unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(
            NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown,
            &handler,
        )
    }
}
