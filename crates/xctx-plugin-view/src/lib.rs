//! The panes of xctx's first-party plugins, drawn with GPUI: the API client ([`HttpPaneView`]), the logs tail
//! ([`LogsPaneView`]), the read-only database ([`SqlPaneView`]) and the browser ([`BrowserPaneView`]).
//!
//! Like the components of `xctx-ui`, each pane is a `RenderOnce` drawn from a plain view type the app fills in
//! (`HttpView`, `LogsView`, `SqlView`, `BrowserView`) and reports what the user did through callbacks. The panes
//! hold no state and do not depend on the plugin crates: the app converts its exchanges, log lines, query
//! results and browser state into these types. Every text they draw is drawn as given, so the app masks secrets
//! before passing it in. Long content is capped (see each module's limits) with a note counting what was left
//! out. Inputs (a URL, a body, a filter, a query) are elements the app passes in, typically `xctx-ui`'s
//! `PromptInput`; without one the pane draws the view's text in its place.
//!
//! Colours come from the `xctx-ui` [`xctx_ui::Theme`] global, as everywhere else.

mod common;

pub mod browser;
pub mod http;
pub mod logs;
pub mod sql;

pub use browser::{
    BROWSER_LINE_CHARS, BROWSER_NO_SCREENSHOT, BrowserPaneView, BrowserStatus, BrowserView,
    CHROME_INSTALL_HINT, CHROME_NOT_FOUND, CONSOLE_ROW_LIMIT, ConsoleRow, NETWORK_ROW_LIMIT,
    NetworkErrorRow, Screenshot,
};
pub use common::{Caps, Excerpt, excerpt};
pub use http::{
    BODY_CAPS, HEADER_LIMIT, HISTORY_ROW_LIMIT, HTTP_DIFF_SAME, HTTP_METHODS, HTTP_NO_SELECTION,
    HttpDiffView, HttpPaneView, HttpResponseView, HttpRow, HttpView, REPLAY_COUNTS, StatusClass,
    next_method, status_label,
};
pub use logs::{
    LOG_LEVELS, LOG_LINE_CHARS, LOG_LINE_LIMIT, LOGS_EMPTY, LogLevel, LogLine, LogsPaneView,
    LogsView, detect_level, level_passes, tail_note,
};
pub use sql::{
    SQL_CELL_CHARS, SQL_COL_LIMIT, SQL_COL_WIDTH, SQL_NO_RESULT, SQL_ROW_LIMIT, SQL_WRITE_HINT,
    SqlPaneView, SqlResultView, SqlView, cut_cell,
};
