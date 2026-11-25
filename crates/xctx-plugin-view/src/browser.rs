//! The browser pane: a toolbar (Back, Forward, Reload, the URL with Open, a loading badge, and the load or
//! agent error when there is one), the live page filling the rest of the pane, and a collapsible console strip
//! at the bottom with the page's console messages and failed requests.
//!
//! The page itself is a native view the app places over the element passed to [`BrowserPaneView::page`] (it
//! draws above the window, so the pane only reserves its area); without one the pane draws
//! [`BROWSER_NO_PAGE`] there.

use std::rc::Rc;

use gpui::{AnyElement, App, SharedString, Window, div, prelude::*, px};
use xctx_ui::{Badge, Button, Theme, Tone, cut_line};

use crate::common::{error_line, field_text, note, tone_text};
use crate::logs::LogLevel;

/// The most console messages drawn: the last ones. A note counts the rest.
pub const CONSOLE_ROW_LIMIT: usize = 200;

/// The most network errors drawn: the last ones. A note counts the rest.
pub const NETWORK_ROW_LIMIT: usize = 200;

/// The most characters drawn of one console message or network error.
pub const BROWSER_LINE_CHARS: usize = 400;

/// What the page area says when the app passes no page element (the live browser could not be made).
pub const BROWSER_NO_PAGE: &str = "The live browser is not available in this window.";

/// The height of the open console strip's list.
pub const CONSOLE_STRIP_HEIGHT: f32 = 160.0;

/// One console message of the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleRow {
    /// Its level, if known: errors and warnings are tinted and lettered.
    pub level: Option<LogLevel>,
    /// The message. Already masked by the caller.
    pub text: SharedString,
}

/// One failed request of the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkErrorRow {
    /// The URL. Already masked by the caller.
    pub url: SharedString,
    /// What went wrong: a status such as "404 Not Found" or a reason such as "Could not connect to the server."
    pub detail: SharedString,
}

/// What the browser pane shows. Every text is already masked by the caller.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BrowserView {
    /// The page's URL, drawn when the app passes no URL input.
    pub url: SharedString,
    /// The page's title, if it has one.
    pub title: Option<SharedString>,
    /// Whether the page is loading, and how far (0 to 1).
    pub loading: Option<f64>,
    /// Whether Back goes anywhere.
    pub can_go_back: bool,
    /// Whether Forward goes anywhere.
    pub can_go_forward: bool,
    /// Whether a page was loaded (Reload has something to reload).
    pub has_page: bool,
    /// Why the last load failed or was refused, until the next one starts.
    pub load_error: Option<SharedString>,
    /// Whether an agent's `page.*` tool is driving the page now.
    pub agent_busy: bool,
    /// Why the last agent tool on this page failed.
    pub error: Option<SharedString>,
    /// The page's console messages, oldest first; the last [`CONSOLE_ROW_LIMIT`] are drawn.
    pub console: Vec<ConsoleRow>,
    /// The page's failed requests, oldest first; the last [`NETWORK_ROW_LIMIT`] are drawn.
    pub network_errors: Vec<NetworkErrorRow>,
    /// Whether the console strip is open.
    pub console_open: bool,
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The browser pane, drawn from a [`BrowserView`].
///
/// Back and Forward are enabled when the history goes that way, Reload once a page was loaded, Open always.
///
/// Selectors for tests: `browser-back`, `browser-forward`, `browser-reload`, `browser-open`, `browser-url`,
/// `browser-loading`, `browser-agent`, `browser-load-error`, `browser-error`, `browser-page`,
/// `browser-no-page`, `browser-console-toggle`, `browser-console`, `browser-console-row-<i>` and
/// `browser-network-row-<i>` (of the rows drawn), `browser-console-more` and `browser-network-more`.
#[derive(IntoElement)]
pub struct BrowserPaneView {
    view: BrowserView,
    url_input: Option<AnyElement>,
    page: Option<AnyElement>,
    on_back: Option<Handler>,
    on_forward: Option<Handler>,
    on_reload: Option<Handler>,
    on_open: Option<Handler>,
    on_toggle_console: Option<Handler>,
}

impl BrowserPaneView {
    /// A pane for `view` with no input, no page element and no handlers.
    pub fn new(view: BrowserView) -> Self {
        Self {
            view,
            url_input: None,
            page: None,
            on_back: None,
            on_forward: None,
            on_reload: None,
            on_open: None,
            on_toggle_console: None,
        }
    }

    /// Draw `element` (a prompt input, say) as the URL bar instead of [`BrowserView::url`].
    pub fn url_input(mut self, element: impl IntoElement) -> Self {
        self.url_input = Some(element.into_any_element());
        self
    }

    /// Fill the page area with `element`, over which the app places the live page.
    pub fn page(mut self, element: impl IntoElement) -> Self {
        self.page = Some(element.into_any_element());
        self
    }

    /// Call `handler` when Back is clicked.
    pub fn on_back(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_back = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Forward is clicked.
    pub fn on_forward(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_forward = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Reload is clicked.
    pub fn on_reload(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_reload = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Open is clicked: open what the URL bar holds.
    pub fn on_open(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when the console strip's header is clicked: open or close it.
    pub fn on_toggle_console(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle_console = Some(Rc::new(handler));
        self
    }

    /// Whether Back is enabled.
    pub fn can_go_back(&self) -> bool {
        self.view.can_go_back
    }

    /// Whether Forward is enabled.
    pub fn can_go_forward(&self) -> bool {
        self.view.can_go_forward
    }

    /// Whether Reload is enabled.
    pub fn can_reload(&self) -> bool {
        self.view.has_page
    }
}

/// The last `limit` items of `items`, with how many were left out before them.
fn tail<T>(items: &[T], limit: usize) -> (&[T], usize) {
    let start = items.len().saturating_sub(limit);
    (&items[start..], start)
}

/// The loading badge's text for `progress` (0 to 1).
pub fn loading_label(progress: f64) -> SharedString {
    let percent = (progress.clamp(0.0, 1.0) * 100.0).round() as u32;
    if percent == 0 || percent == 100 {
        "Loading…".into()
    } else {
        format!("Loading {percent}%").into()
    }
}

fn tool_button(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    handler: Option<Handler>,
) -> Button {
    let mut button = Button::new(id, label).disabled(!enabled).selector(id);
    if let Some(handler) = handler.filter(|_| enabled) {
        button = button.on_click(move |_, window, cx| handler(window, cx));
    }
    button
}

impl RenderOnce for BrowserPaneView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let (can_back, can_forward, can_reload) =
            (self.can_go_back(), self.can_go_forward(), self.can_reload());
        let view = self.view;

        let url = self.url_input.unwrap_or_else(|| {
            field_text(view.url.clone(), "A URL, or a path of the app", &theme).into_any_element()
        });
        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::SPACE_1)
            .child(tool_button("browser-back", "Back", can_back, self.on_back))
            .child(tool_button(
                "browser-forward",
                "Forward",
                can_forward,
                self.on_forward,
            ))
            .child(tool_button(
                "browser-reload",
                "Reload",
                can_reload,
                self.on_reload,
            ))
            .child(
                div()
                    .debug_selector(|| "browser-url".to_owned())
                    .flex_1()
                    .min_w_0()
                    .child(url),
            )
            .child(tool_button("browser-open", "Open", true, self.on_open))
            .children(view.loading.map(|progress| {
                div()
                    .debug_selector(|| "browser-loading".to_owned())
                    .flex_none()
                    .child(Badge::new(loading_label(progress)).tone(Tone::Warning))
            }))
            .children(view.agent_busy.then(|| {
                div()
                    .debug_selector(|| "browser-agent".to_owned())
                    .flex_none()
                    .child(Badge::new("Agent").tone(Tone::Accent))
            }));

        let load_error = view
            .load_error
            .clone()
            .map(|text| error_line("browser-load-error", text, &theme));
        let error = view
            .error
            .clone()
            .map(|text| error_line("browser-error", text, &theme));

        let page_frame = div()
            .debug_selector(|| "browser-page".to_owned())
            .flex()
            .flex_1()
            .min_h(px(80.0))
            .overflow_hidden()
            .rounded(Theme::RADIUS_SM)
            .border_1()
            .border_color(theme.border)
            .bg(theme.chrome);
        let page = match self.page {
            Some(element) => page_frame.child(element),
            None => page_frame.items_center().justify_center().child(
                div()
                    .debug_selector(|| "browser-no-page".to_owned())
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_subtle)
                    .child(BROWSER_NO_PAGE),
            ),
        };

        let errors = view
            .console
            .iter()
            .filter(|c| c.level == Some(LogLevel::Error))
            .count();
        let mut header = div()
            .id("browser-console-toggle")
            .debug_selector(|| "browser-console-toggle".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::SPACE_2)
            .cursor_pointer()
            .text_size(Theme::TEXT_XS)
            .text_color(theme.text_subtle)
            .child(if view.console_open {
                "Hide console"
            } else {
                "Console"
            })
            .child(format!("{} messages", view.console.len()))
            .children((errors > 0).then(|| {
                div()
                    .text_color(theme.danger)
                    .child(format!("{errors} errors"))
            }))
            .children((!view.network_errors.is_empty()).then(|| {
                div()
                    .text_color(theme.danger)
                    .child(format!("{} failed requests", view.network_errors.len()))
            }));
        if let Some(handler) = self.on_toggle_console.clone() {
            header = header.on_click(move |_, window, cx| handler(window, cx));
        }

        let strip = view.console_open.then(|| {
            let (console, console_hidden) = tail(&view.console, CONSOLE_ROW_LIMIT);
            let console_rows = console.iter().enumerate().map(|(i, row)| {
                let tone = row.level.map_or(Tone::Neutral, LogLevel::tone);
                div()
                    .debug_selector(move || format!("browser-console-row-{i}"))
                    .flex()
                    .flex_none()
                    .gap(Theme::SPACE_2)
                    .px(Theme::SPACE_2)
                    .whitespace_nowrap()
                    .child(
                        div()
                            .flex_none()
                            .w(px(10.0))
                            .text_color(tone_text(&theme, tone))
                            .child(row.level.map_or(" ", LogLevel::letter)),
                    )
                    .child(
                        div()
                            .text_color(match row.level {
                                Some(LogLevel::Error | LogLevel::Warn) => tone_text(&theme, tone),
                                _ => theme.text,
                            })
                            .child(cut_line(&row.text, BROWSER_LINE_CHARS)),
                    )
            });
            let (network, network_hidden) = tail(&view.network_errors, NETWORK_ROW_LIMIT);
            let network_rows = network.iter().enumerate().map(|(i, row)| {
                div()
                    .debug_selector(move || format!("browser-network-row-{i}"))
                    .flex()
                    .flex_none()
                    .gap(Theme::SPACE_2)
                    .px(Theme::SPACE_2)
                    .whitespace_nowrap()
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.danger)
                            .child(cut_line(&row.detail, 60)),
                    )
                    .child(
                        div()
                            .text_color(theme.text)
                            .child(cut_line(&row.url, BROWSER_LINE_CHARS)),
                    )
            });
            div()
                .id("browser-console")
                .debug_selector(|| "browser-console".to_owned())
                .flex()
                .flex_col()
                .flex_none()
                .h(px(CONSOLE_STRIP_HEIGHT))
                .overflow_y_scroll()
                .py(Theme::SPACE_1)
                .rounded(Theme::RADIUS_SM)
                .border_1()
                .border_color(theme.border)
                .bg(theme.chrome)
                .font_family("Menlo")
                .text_size(Theme::TEXT_XS)
                .children((console_hidden > 0).then(|| {
                    note(
                        "browser-console-more",
                        format!("… {console_hidden} earlier messages"),
                        &theme,
                    )
                }))
                .children(console_rows)
                .children((network_hidden > 0).then(|| {
                    note(
                        "browser-network-more",
                        format!("… {network_hidden} earlier failed requests"),
                        &theme,
                    )
                }))
                .children(network_rows)
        });

        div()
            .id("browser-pane")
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .gap(Theme::SPACE_1)
            .p(Theme::SPACE_2)
            .bg(theme.surface)
            .text_color(theme.text)
            .text_size(Theme::TEXT_SM)
            .child(toolbar)
            .children(load_error)
            .children(error)
            .child(page)
            .child(header)
            .children(strip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(change: impl FnOnce(&mut BrowserView)) -> BrowserPaneView {
        let mut view = BrowserView::default();
        change(&mut view);
        BrowserPaneView::new(view)
    }

    #[test]
    fn the_toolbar_follows_the_history_and_the_page() {
        let blank = pane(|_| {});
        assert!(!blank.can_go_back() && !blank.can_go_forward() && !blank.can_reload());
        let browsed = pane(|v| {
            v.can_go_back = true;
            v.has_page = true;
        });
        assert!(browsed.can_go_back() && !browsed.can_go_forward() && browsed.can_reload());
        assert!(pane(|v| v.can_go_forward = true).can_go_forward());
    }

    #[test]
    fn loading_shows_its_progress() {
        assert_eq!(loading_label(0.0).as_ref(), "Loading…");
        assert_eq!(loading_label(0.42).as_ref(), "Loading 42%");
        assert_eq!(loading_label(1.0).as_ref(), "Loading…");
        assert_eq!(loading_label(7.0).as_ref(), "Loading…");
    }

    #[test]
    fn tails_keep_the_last_items() {
        let items: Vec<u32> = (0..10).collect();
        assert_eq!(tail(&items, 3), (&items[7..], 7));
        assert_eq!(tail(&items, 20), (&items[..], 0));
    }
}
