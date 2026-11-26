//! The browser plugin's live page in the window: one `WKWebView` per open space with a web pane mounted
//! (`xctx_web_view`), which the user browses in the pane and the agents' `page.*` tools drive, so the user
//! watches them work.
//!
//! Threading: AppKit and WebKit live on the main thread, so a `page.*` call does not go to the background
//! executor like other tools. `run_call_job` (in `plugin_jobs.rs`) hands it to [`Workspace::page_work`], which
//! checks it ([`page_action`]: `page.open` refuses anything outside the space's app) and returns a future that
//! the GPUI foreground executor runs: it calls WebKit and awaits the futures WebKit's completion handlers resolve
//! (scripts, snapshots, loads), each bounded by a timer, so the UI never blocks. Its result then lands exactly as
//! a background call's does (busy marks, effects, events, the caller's responder). One call drives a page at a
//! time.
//!
//! What agents see stays inside the agents' policy: every tool refuses a page outside the space's app (the user
//! may browse anywhere), checks again after it acted and before it returns what it read, and masks it.
//!
//! The view draws above GPUI, so each frame the browser pane's page area places it ([`page_area`]) and the end
//! of the frame hides every view that was not placed ([`Browsers::frame_end`]): another tab or Home is shown, the
//! pane is zoomed away, an attempt is shown instead, or a dialog is open. A view is closed when its last web
//! pane is unmounted, its space's tab closes, or xctx quits.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use futures::FutureExt as _;
use futures::StreamExt as _;
use futures::future::{Either, LocalBoxFuture};
use futures::lock::Mutex as AsyncMutex;
use gpui::{AnyElement, BackgroundExecutor, Context, IntoElement, Styled, Window, canvas};
use serde_json::Value;
use xctx_browser::{
    Clicked, Typed, click_script, click_target, page_text, read_value_script, text_script,
    type_script, typed_value,
};
use xctx_core::{Redactor, SpaceId};
use xctx_plugin::Invocation;
use xctx_web_view::{WebView, WindowHost};

use crate::plugin_host::{
    CallEnv, CallJob, CallOutput, PageAction, PageReading, clicked_output, page_action,
    page_output, screenshot_output, text_output, typed_output,
};
use crate::workspace::Workspace;

/// How long a page load (`page.open`, `page.reload`, or one an action started) may take.
pub const PAGE_LOAD_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a script or a screenshot may take.
pub const SCRIPT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a click or a submit waits for a navigation it may have started to begin.
pub const ACTION_SETTLE: Duration = Duration::from_millis(300);

/// How long `page.console` waits for messages still on their way.
pub const CONSOLE_SETTLE: Duration = Duration::from_millis(200);

/// What `page.*` tools say when the space has no browser pane.
pub const NOT_MOUNTED: &str = "the browser is not open in this space";

/// One space's live browser.
struct SpaceBrowser {
    view: WebView,
    /// Held by the `page.*` call driving the page, so calls take turns.
    turn: Rc<AsyncMutex<()>>,
}

/// The live browsers of the open spaces, and the window they are drawn in.
#[derive(Default)]
pub(crate) struct Browsers {
    host: Option<WindowHost>,
    views: HashMap<SpaceId, SpaceBrowser>,
}

impl Browsers {
    /// Remember `window`'s native view, which every browser is added to. Called before each frame; the first
    /// call does the work.
    pub(crate) fn attach(&mut self, window: &Window) {
        if self.host.is_none() {
            self.host = WindowHost::of(window).ok();
        }
    }

    /// The live browser of `space`, if it has one.
    pub(crate) fn view(&self, space: &SpaceId) -> Option<WebView> {
        self.views.get(space).map(|b| b.view.clone())
    }

    /// An element drawn after everything else in the window: its paint hides every browser that no pane
    /// placed this frame.
    pub(crate) fn frame_end(&self) -> AnyElement {
        let views: Vec<WebView> = self.views.values().map(|b| b.view.clone()).collect();
        canvas(
            |_, _, _| {},
            move |_, _, _, _| {
                for view in &views {
                    view.end_frame();
                }
            },
        )
        .absolute()
        .size_0()
        .into_any_element()
    }
}

/// The browser pane's page area: it places `view` over itself every frame it is drawn, unless `place` is false
/// (a dialog is open over the pane), in which case the view is hidden at the end of the frame.
pub(crate) fn page_area(view: WebView, place: bool) -> AnyElement {
    canvas(
        move |bounds, _, _| {
            if place {
                view.place(bounds);
            }
        },
        |_, _, _, _| {},
    )
    .size_full()
    .into_any_element()
}

impl Workspace {
    /// The live browser of open space `id`, made the first time (loading `start` when given) if a web pane is
    /// mounted there. Fails when no web pane is mounted, or the window cannot hold one.
    pub(crate) fn space_browser(
        &mut self,
        id: &SpaceId,
        start: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Result<WebView, String> {
        if let Some(view) = self.browsers.view(id).filter(|v| !v.is_closed()) {
            return Ok(view);
        }
        let space = self.open_space_for_plugins(id).map_err(|e| e.message)?;
        if !self
            .plugins
            .mounted(&space, &self.redactor)
            .iter()
            .any(|m| m.kind == "web")
        {
            return Err(NOT_MOUNTED.to_owned());
        }
        let host = self
            .browsers
            .host
            .clone()
            .ok_or("the window cannot show a browser yet; try again")?;
        let (view, mut changes) = WebView::new(&host).map_err(|e| e.to_string())?;
        if let Some(url) = start
            && let Err(e) = view.load(url)
        {
            self.plugins.runtime(id).web.error =
                Some(self.redactor.redact(&e.to_string()).into_owned());
        }
        // Redraw the pane as the page loads and logs.
        cx.spawn(async move |this, cx| {
            while changes.next().await.is_some() {
                let redrawn = this.update(cx, |this, cx| {
                    this.plugin_panes.changed();
                    cx.notify();
                });
                if redrawn.is_err() {
                    break;
                }
            }
        })
        .detach();
        self.browsers.views.insert(
            id.clone(),
            SpaceBrowser {
                view: view.clone(),
                turn: Rc::new(AsyncMutex::new(())),
            },
        );
        Ok(view)
    }

    /// Close the live browser of `id`, if it has one (its last web pane went, or its tab closed).
    pub(crate) fn close_space_browser(&mut self, id: &SpaceId) {
        if let Some(browser) = self.browsers.views.remove(id) {
            browser.view.close();
        }
    }

    /// Close every live browser (xctx quits).
    pub(crate) fn close_all_browsers(&mut self) {
        for (_, browser) in self.browsers.views.drain() {
            browser.view.close();
        }
    }

    /// The work of `page.*` call `job` on the space's live browser, to run on the foreground executor; or why
    /// it cannot run (a bad argument, a URL outside the space's app, no browser pane). Errors are masked.
    pub(crate) fn page_work(
        &mut self,
        job: &CallJob,
        cx: &mut Context<Self>,
    ) -> Result<LocalBoxFuture<'static, Result<CallOutput, String>>, String> {
        let redactor = job.plan.redactor.clone();
        let mask = |e: String| redactor.redact(&e).into_owned();
        let Invocation::Builtin { name, args } = &job.plan.invocation else {
            return Err(format!("{} is not a browser built-in", job.plan.tool));
        };
        let action = page_action(name, args, &job.env).map_err(mask)?;
        let id = job.env.space.clone();
        let view = self.space_browser(&id, None, cx).map_err(mask)?;
        let turn = self
            .browsers
            .views
            .get(&id)
            .map(|b| Rc::clone(&b.turn))
            .ok_or_else(|| NOT_MOUNTED.to_owned())?;
        let env = job.env.clone();
        let executor = cx.background_executor().clone();
        Ok(async move {
            let _turn = turn.lock().await;
            run_page(&view, action, &env, &redactor, &executor)
                .await
                .map_err(|e| redactor.redact(&e).into_owned())
        }
        .boxed_local())
    }
}

/// `work`, or a timeout error naming `what` after `timeout`.
async fn within<T>(
    executor: &BackgroundExecutor,
    timeout: Duration,
    what: &str,
    work: impl Future<Output = T>,
) -> Result<T, String> {
    match futures::future::select(Box::pin(work), executor.timer(timeout)).await {
        Either::Left((value, _)) => Ok(value),
        Either::Right(_) => Err(xctx_browser::BrowserError::Timeout {
            what: what.to_owned(),
            timeout,
        }
        .to_string()),
    }
}

/// The value of `script` (built by `xctx_browser::script`) run in the page.
async fn evaluate(
    view: &WebView,
    executor: &BackgroundExecutor,
    script: &str,
) -> Result<Value, String> {
    within(
        executor,
        SCRIPT_TIMEOUT,
        "the script",
        view.evaluate(script),
    )
    .await?
    .map_err(|e| e.to_string())
}

/// Wait for a load to finish, for at most [`PAGE_LOAD_TIMEOUT`].
async fn load(
    executor: &BackgroundExecutor,
    settled: impl Future<Output = xctx_web_view::LoadOutcome>,
) -> Result<(), String> {
    within(executor, PAGE_LOAD_TIMEOUT, "the page load", settled).await?
}

/// After a click or a submit: give a navigation it started a moment to begin, then wait for it to load.
async fn settle(view: &WebView, executor: &BackgroundExecutor) -> Result<(), String> {
    let settled = view.next_settle();
    executor.timer(ACTION_SETTLE).await;
    if view.is_loading() {
        load(executor, settled).await?;
    }
    Ok(())
}

/// The page's URL if the agents may use the page (see `UrlPolicy::check_current`), or why not.
fn current(view: &WebView, env: &CallEnv) -> Result<String, String> {
    env.url_policy()
        .check_current(view.url().as_deref())
        .map_err(|e| e.to_string())
}

/// The page's URL again after reading it: the read is only returned if the page is still the one checked.
fn still(view: &WebView, env: &CallEnv, before: &str) -> Result<String, String> {
    let now = current(view, env)?;
    if now == before {
        Ok(now)
    } else {
        Err("the page changed while it was read; try again".to_owned())
    }
}

fn reading(view: &WebView, url: String) -> PageReading {
    PageReading {
        url,
        console: view.console(),
        network_errors: view.network_errors(),
    }
}

/// Run `action` on `view`, for a call in `env`; its output is masked with `redactor`.
async fn run_page(
    view: &WebView,
    action: PageAction,
    env: &CallEnv,
    redactor: &Redactor,
    executor: &BackgroundExecutor,
) -> Result<CallOutput, String> {
    match action {
        PageAction::Open { url } => {
            let document = |u: &str| u.split('#').next().unwrap_or(u).to_owned();
            let same_document = view
                .url()
                .is_some_and(|now| document(&now) == document(&url) && now != url);
            let settled = view.next_settle();
            view.load(&url).map_err(|e| e.to_string())?;
            if same_document {
                // Only the #fragment changes: WebKit scrolls within the page and reports no load.
                executor.timer(ACTION_SETTLE).await;
            } else {
                load(executor, settled).await?;
            }
            if view.url().as_deref() == Some("about:blank") {
                // WebKit answers a blocked load (a restricted port, say) with a blank page and no error.
                return Err(format!("{url} could not be loaded: WebKit refused it"));
            }
            let url = current(view, env)?;
            Ok(page_output(&reading(view, url), false, "opened", redactor))
        }
        PageAction::Reload => {
            current(view, env)?;
            let settled = view.next_settle();
            if !view.reload() {
                return Err("no page is open; page.open a page of the app first".to_owned());
            }
            load(executor, settled).await?;
            let url = current(view, env)?;
            Ok(page_output(
                &reading(view, url),
                false,
                "reloaded",
                redactor,
            ))
        }
        PageAction::Console { errors_only } => {
            let before = current(view, env)?;
            executor.timer(CONSOLE_SETTLE).await;
            let url = still(view, env, &before)?;
            Ok(page_output(
                &reading(view, url),
                errors_only,
                "console",
                redactor,
            ))
        }
        PageAction::Screenshot { full_page } => {
            let before = current(view, env)?;
            let png = within(
                executor,
                SCRIPT_TIMEOUT,
                "the screenshot",
                view.snapshot(full_page),
            )
            .await?
            .map_err(|e| e.to_string())?;
            still(view, env, &before)?;
            screenshot_output(env, &png)
        }
        PageAction::Click { selector } => {
            current(view, env)?;
            let answer = evaluate(view, executor, &click_script(&selector)).await?;
            let (tag, text) = click_target(&selector, &answer).map_err(|e| e.to_string())?;
            settle(view, executor).await?;
            let url = current(view, env).map_err(|e| format!("after the click, {e}"))?;
            Ok(clicked_output(&Clicked { tag, text, url }, redactor))
        }
        PageAction::Type {
            selector,
            text,
            submit,
        } => {
            current(view, env)?;
            let answer = evaluate(view, executor, &type_script(&selector, &text, submit)).await?;
            let mut value = typed_value(&selector, &answer).map_err(|e| e.to_string())?;
            if submit {
                settle(view, executor).await?;
                let before = current(view, env).map_err(|e| format!("after the submit, {e}"))?;
                let answer = evaluate(view, executor, &read_value_script(&selector)).await?;
                still(view, env, &before)?;
                value = answer.as_str().map(str::to_owned);
            }
            let url = current(view, env)?;
            Ok(typed_output(&selector, &Typed { value, url }, redactor))
        }
        PageAction::Text { selector } => {
            let before = current(view, env)?;
            let answer = evaluate(view, executor, &text_script(selector.as_deref())).await?;
            let url = still(view, env, &before)?;
            let read = page_text(selector.as_deref(), &answer, url).map_err(|e| e.to_string())?;
            Ok(text_output(&read, redactor))
        }
    }
}
