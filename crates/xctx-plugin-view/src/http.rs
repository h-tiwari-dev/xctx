//! The API client pane: a request line (method, URL and body), Send and Replay, the history of exchanges, the
//! selected exchange's response, and the diff between two exchanges.

use std::rc::Rc;

use gpui::{AnyElement, App, FontWeight, SharedString, Window, div, prelude::*, px};
use xctx_ui::{Badge, Button, ButtonSize, ButtonVariant, Pill, Theme, Tone, diff_line_tone};

use crate::common::{Caps, Excerpt, error_line, excerpt, field_text, note, pane_frame, section};

/// The methods the method pill cycles through, in order.
pub const HTTP_METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

/// The replay counts offered, each its own button (`http-replay-<n>`).
pub const REPLAY_COUNTS: [u32; 3] = [1, 5, 11];

/// The most history rows drawn; the rest are counted in a note (`http-history-more`).
pub const HISTORY_ROW_LIMIT: usize = 200;

/// The most response headers drawn when they are open; the rest are counted in a note.
pub const HEADER_LIMIT: usize = 100;

/// How much of a response body, or of a diff, the pane draws.
pub const BODY_CAPS: Caps = Caps {
    lines: 2000,
    line_chars: 500,
    total_chars: 200_000,
};

/// What the response area says before an exchange is selected.
pub const HTTP_NO_SELECTION: &str = "Send a request, or pick one from the history.";

/// What the diff says when the two exchanges match.
pub const HTTP_DIFF_SAME: &str = "Same status, headers and body.";

/// The class of an HTTP status, which picks its badge's tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusClass {
    /// 1xx.
    Informational,
    /// 2xx.
    Success,
    /// 3xx.
    Redirect,
    /// 4xx.
    ClientError,
    /// 5xx.
    ServerError,
    /// No response (a timeout, a refused connection), or a code outside 100 to 599.
    Failed,
}

impl StatusClass {
    /// The class of `status`; `None` (no response) is [`StatusClass::Failed`].
    pub fn of(status: Option<u16>) -> Self {
        match status {
            Some(100..=199) => Self::Informational,
            Some(200..=299) => Self::Success,
            Some(300..=399) => Self::Redirect,
            Some(400..=499) => Self::ClientError,
            Some(500..=599) => Self::ServerError,
            _ => Self::Failed,
        }
    }

    /// The badge tone: success for 2xx, accent for 3xx, warning for 4xx, danger for 5xx and failures.
    pub fn tone(self) -> Tone {
        match self {
            Self::Informational => Tone::Neutral,
            Self::Success => Tone::Success,
            Self::Redirect => Tone::Accent,
            Self::ClientError => Tone::Warning,
            Self::ServerError | Self::Failed => Tone::Danger,
        }
    }
}

/// The status badge text: the code, or "ERR" when there was no response.
pub fn status_label(status: Option<u16>) -> SharedString {
    match status {
        Some(code) => code.to_string().into(),
        None => "ERR".into(),
    }
}

/// The method after `method` in [`HTTP_METHODS`] (wrapping), or `GET` for one not in the list.
pub fn next_method(method: &str) -> &'static str {
    HTTP_METHODS
        .iter()
        .position(|m| m.eq_ignore_ascii_case(method))
        .map_or(HTTP_METHODS[0], |i| {
            HTTP_METHODS[(i + 1) % HTTP_METHODS.len()]
        })
}

/// One exchange in the history list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRow {
    /// Its id in the space's history; passed back by the handlers.
    pub id: u64,
    /// The method, upper-cased.
    pub method: SharedString,
    /// The path (or the URL). Drawn as given: already masked by the caller.
    pub path: SharedString,
    /// The status, or `None` when it got no response.
    pub status: Option<u16>,
    /// How long it took, in milliseconds.
    pub elapsed_ms: u64,
}

/// The response of the selected exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponseView {
    /// The exchange's id.
    pub id: u64,
    /// The status, or `None` when it got no response.
    pub status: Option<u16>,
    /// The response headers in order. Already masked by the caller.
    pub headers: Vec<(SharedString, SharedString)>,
    /// The body, or the failure's message when there was no response. Already masked by the caller. Drawn in
    /// monospace within [`BODY_CAPS`].
    pub body: SharedString,
    /// The client cut the body at its size limit: the pane says so (`http-body-truncated`).
    pub truncated: bool,
    /// How long it took, in milliseconds.
    pub elapsed_ms: u64,
}

/// How exchange `b` differs from exchange `a`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpDiffView {
    /// The first exchange's id.
    pub a: u64,
    /// The second exchange's id.
    pub b: u64,
    /// The first exchange's status.
    pub a_status: Option<u16>,
    /// The second exchange's status.
    pub b_status: Option<u16>,
    /// One line per changed header, such as `+ x-cache: hit`, `- etag: "1"` or `~ server: a → b`. Already
    /// masked by the caller.
    pub headers: Vec<SharedString>,
    /// The unified diff of the bodies, one line per entry; `+` and `-` lines are tinted. Already masked by the
    /// caller.
    pub body: Vec<SharedString>,
}

impl HttpDiffView {
    /// Whether the two exchanges have the same status, headers and body.
    pub fn is_same(&self) -> bool {
        self.a_status == self.b_status && self.headers.is_empty() && self.body.is_empty()
    }
}

/// What the API client pane shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HttpView {
    /// The method of the request being edited.
    pub method: SharedString,
    /// The URL, drawn when the app passes no URL input. Already masked by the caller.
    pub url: SharedString,
    /// The body, drawn when the app passes no body input. Already masked by the caller.
    pub body: SharedString,
    /// The history, newest first or in the order the app wants shown.
    pub history: Vec<HttpRow>,
    /// The selected exchange: its row is highlighted and Replay replays it.
    pub selected: Option<u64>,
    /// The exchange picked to compare against the selected one; Diff compares the two.
    pub compare: Option<u64>,
    /// The selected exchange's response, if loaded.
    pub response: Option<HttpResponseView>,
    /// Whether the response headers are open; they are collapsed by default.
    pub headers_open: bool,
    /// The diff open, if any.
    pub diff: Option<HttpDiffView>,
    /// A send or replay runs: Send, Replay and Diff are disabled.
    pub busy: bool,
    /// Why the last action failed. Already masked by the caller.
    pub error: Option<SharedString>,
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type IdHandler = Rc<dyn Fn(u64, &mut Window, &mut App)>;
type ReplayHandler = Rc<dyn Fn(u64, u32, &mut Window, &mut App)>;
type DiffHandler = Rc<dyn Fn(u64, u64, &mut Window, &mut App)>;
type MethodHandler = Rc<dyn Fn(&'static str, &mut Window, &mut App)>;
type BoolHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// The API client pane, drawn from an [`HttpView`].
///
/// Clicking a history row calls [`HttpPaneView::on_select`]; its "vs" button calls [`HttpPaneView::on_compare`]. Diff
/// calls [`HttpPaneView::on_diff`] with `(compare, selected)` and is enabled only when both are set and differ.
/// Send, Replay and Diff are disabled while [`HttpView::busy`]; Replay also needs a selected exchange.
///
/// Selectors for tests: `http-method`, `http-send`, `http-replay-<n>` for each of [`REPLAY_COUNTS`],
/// `http-row-<id>`, `http-compare-<id>`, `http-history`, `http-history-more`, `http-diff`, `http-diff-view`,
/// `http-diff-more`, `http-diff-same`, `http-response`, `http-status`, `http-headers`, `http-body`,
/// `http-body-more`, `http-body-truncated`, `http-empty` and `http-error`.
#[derive(IntoElement)]
pub struct HttpPaneView {
    view: HttpView,
    url_input: Option<AnyElement>,
    body_input: Option<AnyElement>,
    on_method: Option<MethodHandler>,
    on_send: Option<Handler>,
    on_replay: Option<ReplayHandler>,
    on_select: Option<IdHandler>,
    on_compare: Option<IdHandler>,
    on_diff: Option<DiffHandler>,
    on_headers: Option<BoolHandler>,
}

impl HttpPaneView {
    /// A pane for `view` with no inputs and no handlers.
    pub fn new(view: HttpView) -> Self {
        Self {
            view,
            url_input: None,
            body_input: None,
            on_method: None,
            on_send: None,
            on_replay: None,
            on_select: None,
            on_compare: None,
            on_diff: None,
            on_headers: None,
        }
    }

    /// Draw `element` (a prompt input, say) as the URL field instead of [`HttpView::url`].
    pub fn url_input(mut self, element: impl IntoElement) -> Self {
        self.url_input = Some(element.into_any_element());
        self
    }

    /// Draw `element` as the body field instead of [`HttpView::body`].
    pub fn body_input(mut self, element: impl IntoElement) -> Self {
        self.body_input = Some(element.into_any_element());
        self
    }

    /// Call `handler` with the next method in [`HTTP_METHODS`] when the method pill is clicked.
    pub fn on_method(
        mut self,
        handler: impl Fn(&'static str, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_method = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Send is clicked.
    pub fn on_send(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_send = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the selected exchange's id and the count when a Replay ×n is clicked.
    pub fn on_replay(
        mut self,
        handler: impl Fn(u64, u32, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_replay = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with an exchange's id when its history row is clicked.
    pub fn on_select(mut self, handler: impl Fn(u64, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with an exchange's id when its "vs" button is clicked: compare it with the selected one.
    /// The buttons show only with this handler.
    pub fn on_compare(mut self, handler: impl Fn(u64, &mut Window, &mut App) + 'static) -> Self {
        self.on_compare = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with `(compare, selected)` when Diff is clicked.
    pub fn on_diff(mut self, handler: impl Fn(u64, u64, &mut Window, &mut App) + 'static) -> Self {
        self.on_diff = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the new open state when the response's Headers toggle is clicked.
    pub fn on_headers(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_headers = Some(Rc::new(handler));
        self
    }

    /// Whether Send is enabled.
    pub fn can_send(&self) -> bool {
        !self.view.busy
    }

    /// Whether the Replay buttons are enabled: not busy, and an exchange is selected.
    pub fn can_replay(&self) -> bool {
        !self.view.busy && self.view.selected.is_some()
    }

    /// The `(a, b)` Diff compares, when it is enabled: not busy, with a compare and a selected exchange that
    /// differ.
    pub fn diff_pair(&self) -> Option<(u64, u64)> {
        match (self.view.compare, self.view.selected) {
            (Some(a), Some(b)) if a != b && !self.view.busy => Some((a, b)),
            _ => None,
        }
    }
}

fn mono_line(line: SharedString, theme: &Theme) -> gpui::Div {
    let mut row = div()
        .min_h(px(14.0))
        .px(Theme::SPACE_2)
        .whitespace_nowrap()
        .text_color(theme.text)
        .child(line.clone());
    if let Some(tone) = diff_line_tone(&line) {
        let colors = theme.tone(tone);
        row = row.text_color(colors.fg).bg(colors.bg);
    }
    row
}

fn mono_box(id: &'static str, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex()
        .flex_col()
        .min_h_0()
        .overflow_scroll()
        .py(Theme::SPACE_1)
        .rounded(Theme::RADIUS_SM)
        .border_1()
        .border_color(theme.border)
        .bg(theme.chrome)
        .font_family("Menlo")
        .text_size(Theme::TEXT_XS)
}

impl RenderOnce for HttpPaneView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let can_send = self.can_send();
        let can_replay = self.can_replay();
        let diff_pair = self.diff_pair();
        let view = self.view;

        // The request line.
        let method: SharedString = if view.method.is_empty() {
            HTTP_METHODS[0].into()
        } else {
            view.method.clone()
        };
        let mut method_pill = Pill::new("http-method", method.clone())
            .selected(true)
            .selector("http-method");
        if let Some(handler) = self.on_method.clone() {
            let next = next_method(&method);
            method_pill = method_pill.on_click(move |_, window, cx| handler(next, window, cx));
        }
        let url = self
            .url_input
            .unwrap_or_else(|| field_text(view.url.clone(), "URL", &theme).into_any_element());
        let mut send = Button::new("http-send", "Send")
            .variant(ButtonVariant::Primary)
            .disabled(!can_send)
            .selector("http-send");
        if let Some(handler) = self.on_send.clone().filter(|_| can_send) {
            send = send.on_click(move |_, window, cx| handler(window, cx));
        }
        let replays = REPLAY_COUNTS.iter().map(|&n| {
            let selector = format!("http-replay-{n}");
            let mut button =
                Button::new(SharedString::from(selector.clone()), format!("Replay ×{n}"))
                    .variant(ButtonVariant::Secondary)
                    .size(ButtonSize::Compact)
                    .disabled(!can_replay)
                    .selector(selector);
            if let (Some(handler), Some(id)) =
                (self.on_replay.clone().filter(|_| can_replay), view.selected)
            {
                button = button.on_click(move |_, window, cx| handler(id, n, window, cx));
            }
            button
        });
        let mut diff_button = Button::new("http-diff", "Diff")
            .variant(ButtonVariant::Secondary)
            .size(ButtonSize::Compact)
            .disabled(diff_pair.is_none())
            .selector("http-diff");
        if let (Some(handler), Some((a, b))) = (self.on_diff.clone(), diff_pair) {
            diff_button = diff_button.on_click(move |_, window, cx| handler(a, b, window, cx));
        }
        let body_field = self.body_input.unwrap_or_else(|| {
            field_text(view.body.clone(), "Body (optional)", &theme).into_any_element()
        });
        let request = div()
            .flex()
            .flex_none()
            .flex_col()
            .gap(Theme::SPACE_1)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(method_pill)
                    .child(div().flex_1().min_w_0().child(url))
                    .child(send),
            )
            .child(div().flex().child(body_field))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .children(replays)
                    .child(div().flex_1())
                    .child(diff_button),
            );

        let error = view
            .error
            .clone()
            .map(|text| error_line("http-error", text, &theme));

        // The history list.
        let rows: Vec<_> = view
            .history
            .iter()
            .take(HISTORY_ROW_LIMIT)
            .map(|row| {
                let id = row.id;
                let selector = format!("http-row-{id}");
                let selected = view.selected == Some(id);
                let compared = view.compare == Some(id);
                let hover = theme.raised;
                let compare = self
                    .on_compare
                    .clone()
                    .filter(|_| !selected)
                    .map(|handler| {
                        Button::new(SharedString::from(format!("http-compare-{id}")), "vs")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .selected(compared)
                            .selector(format!("http-compare-{id}"))
                            .on_click(move |_, window, cx| handler(id, window, cx))
                    });
                let mut element = div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector)
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .h(px(22.0))
                    .px(Theme::SPACE_1)
                    .rounded(Theme::RADIUS_SM)
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .font_family("Menlo")
                    .text_size(Theme::TEXT_XS)
                    .child(
                        div()
                            .flex_none()
                            .w(px(28.0))
                            .text_color(theme.text_subtle)
                            .child(format!("#{id}")),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(48.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_muted)
                            .child(row.method.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text)
                            .child(row.path.clone()),
                    )
                    .child(
                        Badge::new(status_label(row.status))
                            .tone(StatusClass::of(row.status).tone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(56.0))
                            .text_color(theme.text_subtle)
                            .child(format!("{} ms", row.elapsed_ms)),
                    )
                    .children(compare);
                if selected {
                    element = element.bg(theme.selection);
                }
                if let Some(handler) = self.on_select.clone() {
                    element = element.on_click(move |_, window, cx| handler(id, window, cx));
                }
                element
            })
            .collect();
        let history_more = (view.history.len() > HISTORY_ROW_LIMIT).then(|| {
            note(
                "http-history-more",
                format!(
                    "… {} older exchanges",
                    view.history.len() - HISTORY_ROW_LIMIT
                ),
                &theme,
            )
        });
        let history = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(Theme::SPACE_1)
            .child(section(format!("History ({})", view.history.len()), &theme))
            .child(
                div()
                    .id("http-history")
                    .debug_selector(|| "http-history".to_owned())
                    .flex()
                    .flex_col()
                    .max_h(px(180.0))
                    .overflow_y_scroll()
                    .children(rows)
                    .children(history_more),
            );

        // The selected response.
        let response: AnyElement = match view.response.clone() {
            None => note("http-empty", HTTP_NO_SELECTION, &theme).into_any_element(),
            Some(response) => {
                let Excerpt { lines, hidden } = excerpt(response.body.lines(), BODY_CAPS);
                let headers_open = view.headers_open;
                let mut toggle = Button::new(
                    "http-headers",
                    format!(
                        "{} Headers ({})",
                        if headers_open { "▾" } else { "▸" },
                        response.headers.len()
                    ),
                )
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .selector("http-headers");
                if let Some(handler) = self.on_headers.clone() {
                    toggle =
                        toggle.on_click(move |_, window, cx| handler(!headers_open, window, cx));
                }
                let headers = headers_open.then(|| {
                    div()
                        .flex()
                        .flex_col()
                        .font_family("Menlo")
                        .text_size(Theme::TEXT_XS)
                        .children(response.headers.iter().take(HEADER_LIMIT).map(
                            |(name, value)| {
                                div()
                                    .flex()
                                    .gap(Theme::SPACE_1)
                                    .min_w_0()
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_color(theme.text_muted)
                                            .child(format!("{name}:")),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(theme.text)
                                            .child(value.clone()),
                                    )
                            },
                        ))
                        .children((response.headers.len() > HEADER_LIMIT).then(|| {
                            note(
                                "http-headers-more",
                                format!("… {} more headers", response.headers.len() - HEADER_LIMIT),
                                &theme,
                            )
                        }))
                });
                let class = StatusClass::of(response.status);
                let body_more = (hidden > 0)
                    .then(|| note("http-body-more", format!("… {hidden} more lines"), &theme));
                let truncated = response.truncated.then(|| {
                    note(
                        "http-body-truncated",
                        "The body was cut at the client's size limit.",
                        &theme,
                    )
                });
                div()
                    .debug_selector(|| "http-response".to_owned())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap(Theme::SPACE_1)
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(Theme::SPACE_2)
                            .child(section(format!("Response #{}", response.id), &theme))
                            .child(div().debug_selector(|| "http-status".to_owned()).child(
                                Badge::new(status_label(response.status)).tone(class.tone()),
                            ))
                            .child(
                                div()
                                    .text_size(Theme::TEXT_XS)
                                    .text_color(theme.text_subtle)
                                    .child(format!("{} ms", response.elapsed_ms)),
                            )
                            .child(div().flex_1())
                            .child(toggle),
                    )
                    .children(headers)
                    .child(
                        mono_box("http-body", &theme)
                            .flex_1()
                            .max_h(px(360.0))
                            .children(lines.into_iter().map(|line| {
                                div()
                                    .min_h(px(14.0))
                                    .px(Theme::SPACE_2)
                                    .whitespace_nowrap()
                                    .child(line)
                            })),
                    )
                    .children(body_more)
                    .children(truncated)
                    .into_any_element()
            }
        };

        // The diff.
        let diff = view.diff.clone().map(|diff| {
            let header = div()
                .flex()
                .flex_none()
                .items_center()
                .gap(Theme::SPACE_2)
                .child(section(format!("Diff #{} → #{}", diff.a, diff.b), &theme))
                .child(
                    Badge::new(status_label(diff.a_status))
                        .tone(StatusClass::of(diff.a_status).tone()),
                )
                .child(div().text_color(theme.text_subtle).child("→"))
                .child(
                    Badge::new(status_label(diff.b_status))
                        .tone(StatusClass::of(diff.b_status).tone()),
                );
            let body: AnyElement =
                if diff.is_same() {
                    note("http-diff-same", HTTP_DIFF_SAME, &theme).into_any_element()
                } else {
                    let lines = diff
                        .headers
                        .iter()
                        .chain(diff.body.iter())
                        .map(|line| line.as_ref());
                    let Excerpt { lines, hidden } = excerpt(lines, BODY_CAPS);
                    div()
                        .flex()
                        .flex_col()
                        .gap(Theme::SPACE_1)
                        .child(
                            mono_box("http-diff-view", &theme)
                                .max_h(px(300.0))
                                .children(lines.into_iter().map(|line| mono_line(line, &theme))),
                        )
                        .children((hidden > 0).then(|| {
                            note("http-diff-more", format!("… {hidden} more lines"), &theme)
                        }))
                        .into_any_element()
                };
            div()
                .flex()
                .flex_none()
                .flex_col()
                .gap(Theme::SPACE_1)
                .child(header)
                .child(body)
        });

        pane_frame(&theme)
            .id("http-pane")
            .overflow_y_scroll()
            .child(request)
            .children(error)
            .child(history)
            .child(response)
            .children(diff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_fall_into_their_class() {
        assert_eq!(StatusClass::of(Some(101)), StatusClass::Informational);
        assert_eq!(StatusClass::of(Some(200)), StatusClass::Success);
        assert_eq!(StatusClass::of(Some(299)), StatusClass::Success);
        assert_eq!(StatusClass::of(Some(304)), StatusClass::Redirect);
        assert_eq!(StatusClass::of(Some(404)), StatusClass::ClientError);
        assert_eq!(StatusClass::of(Some(503)), StatusClass::ServerError);
        assert_eq!(StatusClass::of(None), StatusClass::Failed);
        assert_eq!(StatusClass::of(Some(42)), StatusClass::Failed);
        assert_eq!(StatusClass::of(Some(700)), StatusClass::Failed);
    }

    #[test]
    fn classes_have_tones_and_labels() {
        assert_eq!(StatusClass::Success.tone(), Tone::Success);
        assert_eq!(StatusClass::Redirect.tone(), Tone::Accent);
        assert_eq!(StatusClass::ClientError.tone(), Tone::Warning);
        assert_eq!(StatusClass::ServerError.tone(), Tone::Danger);
        assert_eq!(StatusClass::Failed.tone(), Tone::Danger);
        assert_eq!(status_label(Some(201)).as_ref(), "201");
        assert_eq!(status_label(None).as_ref(), "ERR");
    }

    #[test]
    fn the_method_pill_cycles() {
        assert_eq!(next_method("GET"), "POST");
        assert_eq!(next_method("post"), "PUT");
        assert_eq!(next_method("DELETE"), "GET");
        assert_eq!(next_method("OPTIONS"), "GET");
    }

    fn pane(busy: bool, selected: Option<u64>, compare: Option<u64>) -> HttpPaneView {
        HttpPaneView::new(HttpView {
            busy,
            selected,
            compare,
            ..HttpView::default()
        })
    }

    #[test]
    fn busy_disables_send_replay_and_diff() {
        assert!(pane(false, None, None).can_send());
        assert!(!pane(true, None, None).can_send());
        assert!(!pane(false, None, None).can_replay());
        assert!(pane(false, Some(1), None).can_replay());
        assert!(!pane(true, Some(1), None).can_replay());
        assert_eq!(pane(false, Some(2), Some(1)).diff_pair(), Some((1, 2)));
        assert_eq!(pane(false, Some(2), Some(2)).diff_pair(), None);
        assert_eq!(pane(true, Some(2), Some(1)).diff_pair(), None);
        assert_eq!(pane(false, Some(2), None).diff_pair(), None);
    }

    #[test]
    fn a_diff_with_nothing_changed_is_the_same() {
        let mut diff = HttpDiffView {
            a: 1,
            b: 2,
            a_status: Some(200),
            b_status: Some(200),
            headers: vec![],
            body: vec![],
        };
        assert!(diff.is_same());
        diff.b_status = Some(500);
        assert!(!diff.is_same());
    }
}
