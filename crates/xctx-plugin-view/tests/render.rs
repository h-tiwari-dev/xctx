//! The plugin panes in GPUI's headless test window: every control clicked through its selector, what they must
//! not do while busy or unavailable (each against a positive control in the same window), the caps on long
//! content and the error lines.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AnyElement, Context, Entity, IntoElement, Modifiers, Render, TestAppContext, VisualTestContext,
    Window, div, prelude::*,
};
use xctx_plugin_view::{
    BrowserPaneView, BrowserStatus, BrowserView, ConsoleRow, HttpDiffView, HttpPaneView,
    HttpResponseView, HttpRow, HttpView, LOG_LINE_LIMIT, LogLevel, LogLine, LogsPaneView, LogsView,
    NetworkErrorRow, SQL_COL_LIMIT, SQL_ROW_LIMIT, Screenshot, SqlPaneView, SqlResultView, SqlView,
};
use xctx_ui::Theme;

/// What the hosts' handlers saw, in order.
type Log = Rc<RefCell<Vec<String>>>;

fn push(log: &Log, entry: impl Into<String>) {
    log.borrow_mut().push(entry.into());
}

/// Click the element named `selector`. False when it was not drawn.
fn click(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    let Some(bounds) = cx.debug_bounds(selector) else {
        return false;
    };
    cx.simulate_click(bounds.center(), Modifiers::default());
    true
}

/// Whether the element named `selector` was drawn.
fn drawn(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.debug_bounds(selector).is_some()
}

/// A window drawing one pane from a view the test can change.
struct Host<V: 'static> {
    view: V,
    log: Log,
    build: fn(V, Log) -> AnyElement,
}

impl<V: Clone + 'static> Render for Host<V> {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child((self.build)(self.view.clone(), self.log.clone()))
    }
}

fn host<V: Clone + 'static>(
    cx: &mut TestAppContext,
    view: V,
    build: fn(V, Log) -> AnyElement,
) -> (Log, Entity<Host<V>>, &mut VisualTestContext) {
    cx.update(|cx| cx.set_global(Theme::dark()));
    let events: Log = Rc::default();
    let log = events.clone();
    let (host, cx) = cx.add_window_view(move |_, _| Host { view, log, build });
    cx.run_until_parked();
    (events, host, cx)
}

fn update<V: Clone + 'static>(
    cx: &mut VisualTestContext,
    host: &Entity<Host<V>>,
    change: impl FnOnce(&mut V),
) {
    host.update(cx, |host, cx| {
        change(&mut host.view);
        cx.notify();
    });
    cx.run_until_parked();
}

// ---------------------------------------------------------------------------------------------------------------
// HTTP

fn http_view() -> HttpView {
    HttpView {
        method: "GET".into(),
        url: "http://localhost:3000/health".into(),
        body: "".into(),
        history: vec![
            HttpRow {
                id: 2,
                method: "GET".into(),
                path: "/health".into(),
                status: Some(503),
                elapsed_ms: 12,
            },
            HttpRow {
                id: 1,
                method: "GET".into(),
                path: "/health".into(),
                status: Some(200),
                elapsed_ms: 8,
            },
        ],
        selected: Some(2),
        compare: Some(1),
        response: Some(HttpResponseView {
            id: 2,
            status: Some(503),
            headers: vec![("content-type".into(), "text/plain".into())],
            body: "down".into(),
            truncated: false,
            elapsed_ms: 12,
        }),
        headers_open: false,
        diff: None,
        busy: false,
        error: None,
    }
}

fn build_http(view: HttpView, log: Log) -> AnyElement {
    let (send, replay, select, compare, diff, method, headers) = (
        log.clone(),
        log.clone(),
        log.clone(),
        log.clone(),
        log.clone(),
        log.clone(),
        log,
    );
    HttpPaneView::new(view)
        .url_input(
            div()
                .debug_selector(|| "app-url-input".to_owned())
                .child("url"),
        )
        .on_send(move |_, _| push(&send, "send"))
        .on_replay(move |id, n, _, _| push(&replay, format!("replay {id} x{n}")))
        .on_select(move |id, _, _| push(&select, format!("select {id}")))
        .on_compare(move |id, _, _| push(&compare, format!("compare {id}")))
        .on_diff(move |a, b, _, _| push(&diff, format!("diff {a} {b}")))
        .on_method(move |m, _, _| push(&method, format!("method {m}")))
        .on_headers(move |open, _, _| push(&headers, format!("headers {open}")))
        .into_any_element()
}

#[gpui::test]
fn the_api_client_sends_replays_selects_and_diffs(cx: &mut TestAppContext) {
    let (events, _host, cx) = host(cx, http_view(), build_http);
    assert!(drawn(cx, "app-url-input"), "the app's URL input is drawn");
    assert!(drawn(cx, "http-response") && drawn(cx, "http-status") && drawn(cx, "http-body"));
    for selector in [
        "http-method",
        "http-send",
        "http-replay-1",
        "http-replay-5",
        "http-replay-11",
        "http-row-1",
        "http-compare-1",
        "http-diff",
        "http-headers",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(
        *events.borrow(),
        [
            "method POST",
            "send",
            "replay 2 x1",
            "replay 2 x5",
            "replay 2 x11",
            "select 1",
            "compare 1",
            "diff 1 2",
            "headers true",
        ]
    );
    // The selected row has no "vs" button: it cannot be compared with itself.
    assert!(!drawn(cx, "http-compare-2"));
}

#[gpui::test]
fn a_busy_api_client_sends_and_replays_nothing(cx: &mut TestAppContext) {
    let (events, host, cx) = host(cx, http_view(), build_http);
    update(cx, &host, |view| view.busy = true);
    for selector in ["http-send", "http-replay-1", "http-replay-5", "http-diff"] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    // Positive control: rows still select while busy.
    assert!(click(cx, "http-row-1"));
    assert_eq!(*events.borrow(), ["select 1"]);
    events.borrow_mut().clear();

    // Not busy but nothing selected: Send works, Replay and Diff do not.
    update(cx, &host, |view| {
        view.busy = false;
        view.selected = None;
    });
    for selector in ["http-replay-1", "http-diff", "http-send"] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(*events.borrow(), ["send"]);
}

#[gpui::test]
fn the_api_client_shows_errors_caps_and_the_diff(cx: &mut TestAppContext) {
    let (_events, host, cx) = host(cx, http_view(), build_http);
    assert!(!drawn(cx, "http-error"));
    assert!(!drawn(cx, "http-body-more"));
    assert!(!drawn(cx, "http-body-truncated"));
    assert!(!drawn(cx, "http-diff-view"));
    update(cx, &host, |view| {
        view.error = Some("refused: production is out of scope".into());
        let response = view.response.as_mut().expect("the fixture has a response");
        response.body = (0..5000)
            .map(|i| format!("line {i}\n"))
            .collect::<String>()
            .into();
        response.truncated = true;
        view.headers_open = true;
        view.diff = Some(HttpDiffView {
            a: 1,
            b: 2,
            a_status: Some(200),
            b_status: Some(503),
            headers: vec!["+ retry-after: 5".into()],
            body: vec![
                "--- #1".into(),
                "+++ #2".into(),
                "-ok".into(),
                "+down".into(),
            ],
        });
    });
    for selector in [
        "http-error",
        "http-body-more",
        "http-body-truncated",
        "http-diff-view",
    ] {
        assert!(drawn(cx, selector), "{selector} is drawn");
    }
    assert!(!drawn(cx, "http-diff-same"));

    update(cx, &host, |view| {
        view.diff = Some(HttpDiffView {
            a: 1,
            b: 2,
            a_status: Some(200),
            b_status: Some(200),
            headers: vec![],
            body: vec![],
        });
        view.response = None;
    });
    assert!(drawn(cx, "http-diff-same"));
    assert!(drawn(cx, "http-empty"));
}

#[gpui::test]
fn a_long_history_is_capped(cx: &mut TestAppContext) {
    let mut view = http_view();
    view.history = (1..=250)
        .map(|id| HttpRow {
            id,
            method: "GET".into(),
            path: "/".into(),
            status: Some(200),
            elapsed_ms: 1,
        })
        .collect();
    let (_events, host, cx) = host(cx, view, build_http);
    assert!(drawn(cx, "http-history-more"));
    update(cx, &host, |view| view.history.truncate(3));
    assert!(!drawn(cx, "http-history-more"));
}

// ---------------------------------------------------------------------------------------------------------------
// Logs

fn logs_view() -> LogsView {
    LogsView {
        source: "tail -f log/dev.log".into(),
        lines: vec![
            LogLine {
                text: "INFO listening".into(),
                level: Some(LogLevel::Info),
            },
            LogLine {
                text: "ERROR db timeout".into(),
                level: Some(LogLevel::Error),
            },
            LogLine {
                text: "plain".into(),
                level: None,
            },
        ],
        total: 0,
        filter: "".into(),
        level: None,
        follow: true,
        paused: false,
        error: None,
    }
}

fn build_logs(view: LogsView, log: Log) -> AnyElement {
    let (level, follow, pause, clear) = (log.clone(), log.clone(), log.clone(), log);
    LogsPaneView::new(view)
        .on_level(move |l, _, _| push(&level, format!("level {l:?}")))
        .on_follow(move |on, _, _| push(&follow, format!("follow {on}")))
        .on_pause(move |on, _, _| push(&pause, format!("pause {on}")))
        .on_clear(move |_, _| push(&clear, "clear"))
        .into_any_element()
}

#[gpui::test]
fn the_logs_pane_filters_follows_pauses_and_clears(cx: &mut TestAppContext) {
    let (events, _host, cx) = host(cx, logs_view(), build_logs);
    for selector in ["logs-lines", "logs-line-0", "logs-line-2"] {
        assert!(drawn(cx, selector), "{selector} is drawn");
    }
    for selector in [
        "logs-level-error",
        "logs-level-warn",
        "logs-level-info",
        "logs-level-debug",
        "logs-level-all",
        "logs-follow",
        "logs-pause",
        "logs-clear",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(
        *events.borrow(),
        [
            "level Some(Error)",
            "level Some(Warn)",
            "level Some(Info)",
            "level Some(Debug)",
            "level None",
            "follow false",
            "pause true",
            "clear",
        ]
    );
}

#[gpui::test]
fn the_logs_pane_caps_the_tail_and_shows_errors(cx: &mut TestAppContext) {
    let (events, host, cx) = host(cx, logs_view(), build_logs);
    assert!(!drawn(cx, "logs-more"));
    assert!(!drawn(cx, "logs-error"));
    assert!(!drawn(cx, "logs-paused"));
    update(cx, &host, |view| {
        view.lines = (0..LOG_LINE_LIMIT + 10)
            .map(|i| LogLine {
                text: format!("line {i}").into(),
                level: None,
            })
            .collect();
        view.error = Some("tail: log/dev.log: No such file".into());
        view.paused = true;
        view.follow = false;
    });
    assert!(drawn(cx, "logs-more"));
    assert!(drawn(cx, "logs-error"));
    assert!(drawn(cx, "logs-paused"));
    // Paused, the toggles flip the other way.
    assert!(click(cx, "logs-pause"));
    assert!(click(cx, "logs-follow"));
    assert_eq!(*events.borrow(), ["pause false", "follow true"]);

    update(cx, &host, |view| view.lines.clear());
    assert!(drawn(cx, "logs-empty"));
}

// ---------------------------------------------------------------------------------------------------------------
// SQL

fn sql_view() -> SqlView {
    SqlView {
        engine: "sqlite".into(),
        database: "db/dev.sqlite3".into(),
        query: "select id, email from users".into(),
        result: Some(SqlResultView {
            columns: vec!["id".into(), "email".into()],
            rows: vec![
                vec!["1".into(), "a@example.com".into()],
                vec!["2".into(), "b@example.com".into()],
            ],
            total_rows: 0,
            elapsed_ms: Some(3),
        }),
        busy: false,
        write_pending: false,
        error: None,
    }
}

fn build_sql(view: SqlView, log: Log) -> AnyElement {
    let (run, write) = (log.clone(), log);
    SqlPaneView::new(view)
        .query_input(
            div()
                .debug_selector(|| "app-query-input".to_owned())
                .child("query"),
        )
        .on_run(move |_, _| push(&run, "run"))
        .on_request_write(move |_, _| push(&write, "request write"))
        .into_any_element()
}

#[gpui::test]
fn the_database_pane_runs_and_requests_writes(cx: &mut TestAppContext) {
    let (events, _host, cx) = host(cx, sql_view(), build_sql);
    for selector in [
        "app-query-input",
        "sql-read-only",
        "sql-table",
        "sql-row-count",
        "sql-cell-1-1",
    ] {
        assert!(drawn(cx, selector), "{selector} is drawn");
    }
    for selector in ["sql-run", "sql-request-write"] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(*events.borrow(), ["run", "request write"]);
}

#[gpui::test]
fn a_busy_or_pending_database_pane_runs_nothing(cx: &mut TestAppContext) {
    let (events, host, cx) = host(cx, sql_view(), build_sql);
    update(cx, &host, |view| view.busy = true);
    for selector in ["sql-run", "sql-request-write"] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert!(events.borrow().is_empty(), "{:?}", events.borrow());

    // A write waits: Run works (the positive control), Request write does not.
    update(cx, &host, |view| {
        view.busy = false;
        view.write_pending = true;
    });
    assert!(drawn(cx, "sql-write-pending"));
    for selector in ["sql-request-write", "sql-run"] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(*events.borrow(), ["run"]);
}

#[gpui::test]
fn the_database_pane_caps_the_table_and_shows_errors(cx: &mut TestAppContext) {
    let (_events, host, cx) = host(cx, sql_view(), build_sql);
    for selector in ["sql-rows-more", "sql-cols-more", "sql-error", "sql-empty"] {
        assert!(!drawn(cx, selector), "no {selector}");
    }
    update(cx, &host, |view| {
        let cols = SQL_COL_LIMIT + 3;
        view.result = Some(SqlResultView {
            columns: (0..cols).map(|c| format!("c{c}").into()).collect(),
            rows: (0..SQL_ROW_LIMIT + 5)
                .map(|r| (0..cols).map(|c| format!("{r}:{c}").into()).collect())
                .collect(),
            total_rows: 10_000,
            elapsed_ms: None,
        });
        view.error = Some("no such table: user".into());
    });
    for selector in ["sql-rows-more", "sql-cols-more", "sql-error"] {
        assert!(drawn(cx, selector), "{selector} is drawn");
    }
    update(cx, &host, |view| view.result = None);
    assert!(drawn(cx, "sql-empty"));
    assert!(!drawn(cx, "sql-table"));
}

// ---------------------------------------------------------------------------------------------------------------
// Browser

fn browser_view(status: BrowserStatus) -> BrowserView {
    BrowserView {
        url: "http://localhost:3000".into(),
        status,
        screenshot: None,
        console: vec![
            ConsoleRow {
                level: Some(LogLevel::Error),
                text: "Uncaught TypeError".into(),
            },
            ConsoleRow {
                level: None,
                text: "hello".into(),
            },
        ],
        network_errors: vec![NetworkErrorRow {
            url: "http://localhost:3000/api".into(),
            detail: "500".into(),
        }],
        error: None,
    }
}

fn build_browser(view: BrowserView, log: Log) -> AnyElement {
    let (open, reload) = (log.clone(), log);
    BrowserPaneView::new(view)
        .on_open(move |_, _| push(&open, "open"))
        .on_reload(move |_, _| push(&reload, "reload"))
        .into_any_element()
}

#[gpui::test]
fn the_browser_pane_opens_reloads_and_lists_the_console(cx: &mut TestAppContext) {
    let (events, _host, cx) = host(cx, browser_view(BrowserStatus::Ready), build_browser);
    for selector in [
        "browser-status",
        "browser-no-screenshot",
        "browser-console-row-0",
        "browser-console-row-1",
        "browser-network-row-0",
    ] {
        assert!(drawn(cx, selector), "{selector} is drawn");
    }
    assert!(!drawn(cx, "browser-chrome-missing"));
    for selector in ["browser-open", "browser-reload"] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(*events.borrow(), ["open", "reload"]);
}

#[gpui::test]
fn without_chrome_the_browser_pane_says_how_to_install_it(cx: &mut TestAppContext) {
    let (events, host, cx) = host(
        cx,
        browser_view(BrowserStatus::ChromeNotFound),
        build_browser,
    );
    assert!(drawn(cx, "browser-chrome-missing"));
    for selector in ["browser-open", "browser-reload"] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert!(events.borrow().is_empty(), "{:?}", events.borrow());

    // Connecting: still nothing. Disconnected: Open retries, Reload waits for a page.
    update(cx, &host, |view| view.status = BrowserStatus::Connecting);
    assert!(!drawn(cx, "browser-chrome-missing"));
    assert!(click(cx, "browser-open"));
    assert!(events.borrow().is_empty(), "{:?}", events.borrow());
    update(cx, &host, |view| {
        view.status = BrowserStatus::Failed("Chrome exited".into());
        view.error = Some("websocket closed".into());
    });
    assert!(drawn(cx, "browser-error"));
    for selector in ["browser-reload", "browser-open"] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(*events.borrow(), ["open"]);
}

#[gpui::test]
fn the_browser_pane_draws_a_screenshot_and_caps_its_lists(cx: &mut TestAppContext) {
    let (_events, host, cx) = host(cx, browser_view(BrowserStatus::Ready), build_browser);
    assert!(!drawn(cx, "browser-console-more"));
    assert!(!drawn(cx, "browser-network-more"));
    update(cx, &host, |view| {
        view.screenshot = Some(Screenshot::png(TINY_PNG.to_vec()));
        view.console = (0..300)
            .map(|i| ConsoleRow {
                level: Some(LogLevel::Warn),
                text: format!("warning {i}").into(),
            })
            .collect();
        view.network_errors = (0..300)
            .map(|i| NetworkErrorRow {
                url: format!("http://localhost:3000/{i}").into(),
                detail: "net::ERR_CONNECTION_REFUSED".into(),
            })
            .collect();
    });
    for selector in [
        "browser-screenshot",
        "browser-console-more",
        "browser-network-more",
    ] {
        assert!(drawn(cx, selector), "{selector} is drawn");
    }
    assert!(!drawn(cx, "browser-no-screenshot"));
}

/// A 1×1 PNG.
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x01, 0xe5, 0x27, 0xde, 0xfc, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];
