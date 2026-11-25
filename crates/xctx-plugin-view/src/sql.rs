//! The read-only database pane: a query, Run, and the result as a table. Writes are never run from here: the
//! "Request write" button asks the app to post a gate to the Inbox.

use std::rc::Rc;

use gpui::{AnyElement, App, FontWeight, SharedString, Window, div, prelude::*, px};
use xctx_ui::{Badge, Button, ButtonSize, ButtonVariant, Theme, Tone, cut_line};

use crate::common::{error_line, field_text, note, pane_frame};

/// The most rows drawn; the rest are counted in a note (`sql-rows-more`).
pub const SQL_ROW_LIMIT: usize = 500;

/// The most columns drawn; the rest are counted in a note (`sql-cols-more`).
pub const SQL_COL_LIMIT: usize = 40;

/// The most characters drawn of one cell; longer cells end in "…".
pub const SQL_CELL_CHARS: usize = 120;

/// The width of one table column.
pub const SQL_COL_WIDTH: gpui::Pixels = px(140.0);

/// The line under Request write.
pub const SQL_WRITE_HINT: &str =
    "This pane is read-only. A write waits in the Inbox and runs only when you approve it.";

/// What the table area says before a query ran.
pub const SQL_NO_RESULT: &str = "Run a query to see rows.";

/// A cell as drawn: on one line (line breaks and tabs shown as spaces) and cut to `max` characters, ending in
/// "…" when cut.
pub fn cut_cell(cell: &str, max: usize) -> SharedString {
    if cell.contains(['\n', '\r', '\t']) {
        let flat: String = cell
            .chars()
            .map(|c| {
                if matches!(c, '\n' | '\r' | '\t') {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        cut_line(&flat, max)
    } else {
        cut_line(cell, max)
    }
}

/// The result of the last query.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SqlResultView {
    /// The column names. Already masked by the caller.
    pub columns: Vec<SharedString>,
    /// The rows, each with one cell per column. Already masked by the caller.
    pub rows: Vec<Vec<SharedString>>,
    /// How many rows the query returned, when the app kept fewer than that in `rows`; the pane shows the larger
    /// of this and `rows.len()`.
    pub total_rows: usize,
    /// How long the query took, in milliseconds.
    pub elapsed_ms: Option<u64>,
}

impl SqlResultView {
    /// The row count shown: the larger of [`SqlResultView::total_rows`] and the rows given.
    pub fn row_count(&self) -> usize {
        self.total_rows.max(self.rows.len())
    }
}

/// What the database pane shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SqlView {
    /// The engine, such as "sqlite" or "postgres".
    pub engine: SharedString,
    /// The database: a file name or a connection label. Already masked by the caller.
    pub database: SharedString,
    /// The query, drawn when the app passes no query input. Already masked by the caller.
    pub query: SharedString,
    /// The last result, if any.
    pub result: Option<SqlResultView>,
    /// A query runs: Run and Request write are disabled.
    pub busy: bool,
    /// A write request already waits in the Inbox: Request write is disabled and a badge says so.
    pub write_pending: bool,
    /// Why the last query failed. Already masked by the caller.
    pub error: Option<SharedString>,
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The read-only database pane, drawn from a [`SqlView`].
///
/// Selectors for tests: `sql-run`, `sql-request-write`, `sql-write-pending`, `sql-table`, `sql-cell-<row>-<col>`
/// (of those drawn), `sql-rows-more`, `sql-cols-more`, `sql-row-count`, `sql-empty`, `sql-read-only` and
/// `sql-error`.
#[derive(IntoElement)]
pub struct SqlPaneView {
    view: SqlView,
    query_input: Option<AnyElement>,
    on_run: Option<Handler>,
    on_request_write: Option<Handler>,
}

impl SqlPaneView {
    /// A pane for `view` with no input and no handlers.
    pub fn new(view: SqlView) -> Self {
        Self {
            view,
            query_input: None,
            on_run: None,
            on_request_write: None,
        }
    }

    /// Draw `element` (a prompt input, say) as the query field instead of [`SqlView::query`].
    pub fn query_input(mut self, element: impl IntoElement) -> Self {
        self.query_input = Some(element.into_any_element());
        self
    }

    /// Call `handler` when Run is clicked: run the query read-only.
    pub fn on_run(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_run = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Request write is clicked: the app should post the query as a write gate to the
    /// Inbox, never run it directly.
    pub fn on_request_write(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_request_write = Some(Rc::new(handler));
        self
    }

    /// Whether Run is enabled.
    pub fn can_run(&self) -> bool {
        !self.view.busy
    }

    /// Whether Request write is enabled: not busy, and no request waits already.
    pub fn can_request_write(&self) -> bool {
        !self.view.busy && !self.view.write_pending
    }
}

fn cell(text: SharedString, header: bool, theme: &Theme) -> gpui::Div {
    let mut cell = div()
        .flex_none()
        .w(SQL_COL_WIDTH)
        .px(Theme::SPACE_1)
        .truncate()
        .border_r_1()
        .border_color(theme.border_subtle)
        .child(text);
    if header {
        cell = cell
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.text_muted);
    }
    cell
}

impl RenderOnce for SqlPaneView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let can_run = self.can_run();
        let can_write = self.can_request_write();
        let view = self.view;

        let mut run = Button::new("sql-run", "Run")
            .variant(ButtonVariant::Primary)
            .disabled(!can_run)
            .selector("sql-run");
        if let Some(handler) = self.on_run.clone().filter(|_| can_run) {
            run = run.on_click(move |_, window, cx| handler(window, cx));
        }
        let mut write = Button::new("sql-request-write", "Request write")
            .variant(ButtonVariant::Danger)
            .size(ButtonSize::Compact)
            .disabled(!can_write)
            .selector("sql-request-write");
        if let Some(handler) = self.on_request_write.clone().filter(|_| can_write) {
            write = write.on_click(move |_, window, cx| handler(window, cx));
        }
        let query = self.query_input.unwrap_or_else(|| {
            field_text(view.query.clone(), "SELECT …", &theme).into_any_element()
        });

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::SPACE_2)
            .child(Badge::new(view.engine.to_uppercase()).tone(Tone::Accent))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family("Menlo")
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_muted)
                    .child(view.database.clone()),
            )
            .child(
                div()
                    .debug_selector(|| "sql-read-only".to_owned())
                    .child(Badge::new("READ ONLY").tone(Tone::Success)),
            );

        let error = view
            .error
            .clone()
            .map(|text| error_line("sql-error", text, &theme));

        let table: AnyElement = match &view.result {
            None => note("sql-empty", SQL_NO_RESULT, &theme).into_any_element(),
            Some(result) => {
                let cols = result.columns.len().min(SQL_COL_LIMIT);
                let head = div()
                    .flex()
                    .flex_none()
                    .h(px(20.0))
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .children(
                        result.columns[..cols]
                            .iter()
                            .map(|name| cell(cut_cell(name, SQL_CELL_CHARS), true, &theme)),
                    );
                let rows = result
                    .rows
                    .iter()
                    .take(SQL_ROW_LIMIT)
                    .enumerate()
                    .map(|(r, row)| {
                        let mut line = div()
                            .flex()
                            .flex_none()
                            .h(px(18.0))
                            .items_center()
                            .children(row.iter().take(cols).enumerate().map(|(c, text)| {
                                cell(cut_cell(text, SQL_CELL_CHARS), false, &theme)
                                    .debug_selector(move || format!("sql-cell-{r}-{c}"))
                            }));
                        if r % 2 == 1 {
                            line = line.bg(theme.surface);
                        }
                        line
                    });
                let rows_more = (result.rows.len() > SQL_ROW_LIMIT).then(|| {
                    note(
                        "sql-rows-more",
                        format!("Showing {SQL_ROW_LIMIT} of {} rows", result.row_count()),
                        &theme,
                    )
                });
                let cols_more = (result.columns.len() > SQL_COL_LIMIT).then(|| {
                    note(
                        "sql-cols-more",
                        format!("… {} more columns", result.columns.len() - SQL_COL_LIMIT),
                        &theme,
                    )
                });
                let mut count = format!("{} rows", result.row_count());
                if let Some(ms) = result.elapsed_ms {
                    count.push_str(&format!(" · {ms} ms"));
                }
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap(Theme::SPACE_1)
                    .child(
                        div()
                            .debug_selector(|| "sql-row-count".to_owned())
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_subtle)
                            .child(count),
                    )
                    .child(
                        div()
                            .id("sql-table")
                            .debug_selector(|| "sql-table".to_owned())
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .max_h(px(420.0))
                            .overflow_scroll()
                            .rounded(Theme::RADIUS_SM)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.chrome)
                            .font_family("Menlo")
                            .text_size(Theme::TEXT_XS)
                            .child(head)
                            .children(rows),
                    )
                    .children(rows_more)
                    .children(cols_more)
                    .into_any_element()
            }
        };

        let pending = view.write_pending.then(|| {
            div()
                .debug_selector(|| "sql-write-pending".to_owned())
                .child(Badge::new("WRITE WAITS IN INBOX").tone(Tone::Warning))
        });

        pane_frame(&theme)
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(div().flex_1().min_w_0().child(query))
                    .child(run),
            )
            .children(error)
            .child(table)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .pt(Theme::SPACE_1)
                    .border_t_1()
                    .border_color(theme.border_subtle)
                    .child(write)
                    .children(pending)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_subtle)
                            .child(SQL_WRITE_HINT),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_are_flattened_and_cut() {
        assert_eq!(cut_cell("abc", 5).as_ref(), "abc");
        assert_eq!(cut_cell("a\nb\tc", 10).as_ref(), "a b c");
        assert_eq!(cut_cell("abcdefgh", 4).as_ref(), "abc…");
        assert_eq!(cut_cell("ééééé", 3).as_ref(), "éé…");
        let long = "x".repeat(SQL_CELL_CHARS * 2);
        assert_eq!(
            cut_cell(&long, SQL_CELL_CHARS).chars().count(),
            SQL_CELL_CHARS
        );
    }

    #[test]
    fn the_row_count_is_the_larger_of_the_total_and_the_rows() {
        let mut result = SqlResultView {
            rows: vec![vec![]; 3],
            ..SqlResultView::default()
        };
        assert_eq!(result.row_count(), 3);
        result.total_rows = 10;
        assert_eq!(result.row_count(), 10);
    }

    #[test]
    fn busy_or_pending_disables_writes() {
        let pane = |busy, write_pending| {
            SqlPaneView::new(SqlView {
                busy,
                write_pending,
                ..SqlView::default()
            })
        };
        assert!(pane(false, false).can_run() && pane(false, false).can_request_write());
        assert!(!pane(true, false).can_run() && !pane(true, false).can_request_write());
        assert!(pane(false, true).can_run() && !pane(false, true).can_request_write());
    }
}
