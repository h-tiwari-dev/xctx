//! The browser pane: the page's URL with Open and Reload, the latest screenshot from Chrome, and the page's
//! console messages and network errors. The page itself opens in a normal browser window.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Image, ImageFormat, ObjectFit, SharedString, Window, div, img, prelude::*, px,
};
use xctx_ui::{Badge, Button, ButtonVariant, Theme, Tone, cut_line};

use crate::common::{error_line, field_text, note, pane_frame, section, tone_text};
use crate::logs::LogLevel;

/// The most console messages drawn: the last ones. A note counts the rest.
pub const CONSOLE_ROW_LIMIT: usize = 200;

/// The most network errors drawn: the last ones. A note counts the rest.
pub const NETWORK_ROW_LIMIT: usize = 200;

/// The most characters drawn of one console message or network error.
pub const BROWSER_LINE_CHARS: usize = 400;

/// What the pane says when no Chrome or Chromium was found.
pub const CHROME_NOT_FOUND: &str = "Chrome not found";

/// How to fix [`CHROME_NOT_FOUND`].
pub const CHROME_INSTALL_HINT: &str = "Install Google Chrome or Chromium (for example `brew install --cask google-chrome`), then reopen the pane.";

/// What the screenshot area says before there is one.
pub const BROWSER_NO_SCREENSHOT: &str = "No screenshot yet. Open a page.";

/// Where the pane is with Chrome.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BrowserStatus {
    /// Chrome has not been started yet: Open starts it; Reload is disabled.
    Idle,
    /// No Chrome or Chromium is installed: Open and Reload are disabled and the install hint shows.
    ChromeNotFound,
    /// Chrome is starting or a page is loading: Open and Reload are disabled.
    #[default]
    Connecting,
    /// A page is open.
    Ready,
    /// Chrome stopped or the connection dropped, with why. Already masked by the caller. Open retries.
    Failed(SharedString),
}

impl BrowserStatus {
    /// The status line's text.
    pub fn label(&self) -> SharedString {
        match self {
            Self::Idle => "Not started".into(),
            Self::ChromeNotFound => CHROME_NOT_FOUND.into(),
            Self::Connecting => "Connecting…".into(),
            Self::Ready => "Ready".into(),
            Self::Failed(why) => format!("Disconnected: {why}").into(),
        }
    }

    /// The status badge's tone.
    pub fn tone(&self) -> Tone {
        match self {
            Self::ChromeNotFound | Self::Failed(_) => Tone::Danger,
            Self::Connecting => Tone::Warning,
            Self::Ready => Tone::Success,
            Self::Idle => Tone::Neutral,
        }
    }
}

/// The latest screenshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screenshot {
    /// An encoded image in memory; build one from PNG bytes with [`Screenshot::png`] once per new screenshot
    /// (not every frame: GPUI caches the decoded image by its id, a hash of the bytes).
    Image(Arc<Image>),
    /// An image file on disk, such as a PNG the browser plugin saved.
    Path(PathBuf),
}

impl Screenshot {
    /// A screenshot from PNG bytes.
    pub fn png(bytes: Vec<u8>) -> Self {
        Self::Image(Arc::new(Image::from_bytes(ImageFormat::Png, bytes)))
    }
}

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
    /// What went wrong: a status such as "404" or a reason such as "net::ERR_CONNECTION_REFUSED".
    pub detail: SharedString,
}

/// What the browser pane shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BrowserView {
    /// The page's URL, drawn when the app passes no URL input. Already masked by the caller.
    pub url: SharedString,
    /// Where the pane is with Chrome.
    pub status: BrowserStatus,
    /// The latest screenshot, if any.
    pub screenshot: Option<Screenshot>,
    /// The page's console messages, oldest first; the last [`CONSOLE_ROW_LIMIT`] are drawn.
    pub console: Vec<ConsoleRow>,
    /// The page's failed requests, oldest first; the last [`NETWORK_ROW_LIMIT`] are drawn.
    pub network_errors: Vec<NetworkErrorRow>,
    /// Why the last action failed. Already masked by the caller.
    pub error: Option<SharedString>,
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The browser pane, drawn from a [`BrowserView`].
///
/// Open is enabled unless Chrome was not found or the pane is connecting; Reload only once a page is
/// [`BrowserStatus::Ready`].
///
/// Selectors for tests: `browser-open`, `browser-reload`, `browser-status`, `browser-chrome-missing`,
/// `browser-screenshot`, `browser-no-screenshot`, `browser-console`, `browser-console-row-<i>` and
/// `browser-network-row-<i>` (of the rows drawn), `browser-console-more`, `browser-network-more` and
/// `browser-error`.
#[derive(IntoElement)]
pub struct BrowserPaneView {
    view: BrowserView,
    url_input: Option<AnyElement>,
    on_open: Option<Handler>,
    on_reload: Option<Handler>,
}

impl BrowserPaneView {
    /// A pane for `view` with no input and no handlers.
    pub fn new(view: BrowserView) -> Self {
        Self {
            view,
            url_input: None,
            on_open: None,
            on_reload: None,
        }
    }

    /// Draw `element` (a prompt input, say) as the URL bar instead of [`BrowserView::url`].
    pub fn url_input(mut self, element: impl IntoElement) -> Self {
        self.url_input = Some(element.into_any_element());
        self
    }

    /// Call `handler` when Open is clicked: open the URL in Chrome.
    pub fn on_open(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Reload is clicked.
    pub fn on_reload(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_reload = Some(Rc::new(handler));
        self
    }

    /// Whether Open is enabled.
    pub fn can_open(&self) -> bool {
        !matches!(
            self.view.status,
            BrowserStatus::ChromeNotFound | BrowserStatus::Connecting
        )
    }

    /// Whether Reload is enabled.
    pub fn can_reload(&self) -> bool {
        self.view.status == BrowserStatus::Ready
    }
}

/// The last `limit` items of `items`, with how many were left out before them.
fn tail<T>(items: &[T], limit: usize) -> (&[T], usize) {
    let start = items.len().saturating_sub(limit);
    (&items[start..], start)
}

impl RenderOnce for BrowserPaneView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let can_open = self.can_open();
        let can_reload = self.can_reload();
        let view = self.view;

        let url = self.url_input.unwrap_or_else(|| {
            field_text(view.url.clone(), "http://localhost:PORT", &theme).into_any_element()
        });
        let mut open = Button::new("browser-open", "Open")
            .variant(ButtonVariant::Primary)
            .disabled(!can_open)
            .selector("browser-open");
        if let Some(handler) = self.on_open.clone().filter(|_| can_open) {
            open = open.on_click(move |_, window, cx| handler(window, cx));
        }
        let mut reload = Button::new("browser-reload", "Reload")
            .disabled(!can_reload)
            .selector("browser-reload");
        if let Some(handler) = self.on_reload.clone().filter(|_| can_reload) {
            reload = reload.on_click(move |_, window, cx| handler(window, cx));
        }

        let status = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::SPACE_2)
            .child(
                div()
                    .debug_selector(|| "browser-status".to_owned())
                    .child(Badge::new(view.status.label()).tone(view.status.tone())),
            )
            .children((view.status == BrowserStatus::ChromeNotFound).then(|| {
                div()
                    .debug_selector(|| "browser-chrome-missing".to_owned())
                    .flex_1()
                    .min_w_0()
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_muted)
                    .child(CHROME_INSTALL_HINT)
            }));

        let error = view
            .error
            .clone()
            .map(|text| error_line("browser-error", text, &theme));

        let shot_frame = div()
            .flex()
            .flex_1()
            .min_h(px(120.0))
            .items_center()
            .justify_center()
            .rounded(Theme::RADIUS_SM)
            .border_1()
            .border_color(theme.border)
            .bg(theme.chrome)
            .overflow_hidden();
        let screenshot = match view.screenshot.clone() {
            Some(shot) => {
                let image = match shot {
                    Screenshot::Image(image) => img(image),
                    Screenshot::Path(path) => img(path),
                };
                shot_frame
                    .debug_selector(|| "browser-screenshot".to_owned())
                    .child(
                        image
                            .size_full()
                            .object_fit(ObjectFit::Contain)
                            .with_fallback({
                                let color = theme.text_subtle;
                                move || {
                                    div()
                                        .text_size(Theme::TEXT_XS)
                                        .text_color(color)
                                        .child("The screenshot could not be drawn.")
                                        .into_any_element()
                                }
                            }),
                    )
            }
            None => shot_frame
                .debug_selector(|| "browser-no-screenshot".to_owned())
                .text_size(Theme::TEXT_XS)
                .text_color(theme.text_subtle)
                .child(BROWSER_NO_SCREENSHOT),
        };

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
        let list = |id: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .flex()
                .flex_col()
                .max_h(px(140.0))
                .overflow_scroll()
                .py(Theme::SPACE_1)
                .rounded(Theme::RADIUS_SM)
                .border_1()
                .border_color(theme.border)
                .bg(theme.chrome)
                .font_family("Menlo")
                .text_size(Theme::TEXT_XS)
        };

        pane_frame(&theme)
            .id("browser-pane")
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(div().flex_1().min_w_0().child(url))
                    .child(open)
                    .child(reload),
            )
            .child(status)
            .children(error)
            .child(screenshot)
            .child(section(format!("Console ({})", view.console.len()), &theme))
            .children((console_hidden > 0).then(|| {
                note(
                    "browser-console-more",
                    format!("… {console_hidden} earlier messages"),
                    &theme,
                )
            }))
            .child(list("browser-console").children(console_rows))
            .child(section(
                format!("Network errors ({})", view.network_errors.len()),
                &theme,
            ))
            .children((network_hidden > 0).then(|| {
                note(
                    "browser-network-more",
                    format!("… {network_hidden} earlier errors"),
                    &theme,
                )
            }))
            .child(list("browser-network").children(network_rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(status: BrowserStatus) -> BrowserPaneView {
        BrowserPaneView::new(BrowserView {
            status,
            ..BrowserView::default()
        })
    }

    #[test]
    fn open_and_reload_follow_the_status() {
        assert!(!pane(BrowserStatus::ChromeNotFound).can_open());
        assert!(!pane(BrowserStatus::Connecting).can_open());
        assert!(pane(BrowserStatus::Ready).can_open());
        assert!(pane(BrowserStatus::Idle).can_open());
        assert!(!pane(BrowserStatus::Idle).can_reload());
        assert!(pane(BrowserStatus::Failed("gone".into())).can_open());
        assert!(pane(BrowserStatus::Ready).can_reload());
        assert!(!pane(BrowserStatus::Failed("gone".into())).can_reload());
        assert!(!pane(BrowserStatus::ChromeNotFound).can_reload());
    }

    #[test]
    fn statuses_have_labels_and_tones() {
        assert_eq!(
            BrowserStatus::ChromeNotFound.label().as_ref(),
            CHROME_NOT_FOUND
        );
        assert_eq!(BrowserStatus::Ready.tone(), Tone::Success);
        assert_eq!(BrowserStatus::Connecting.tone(), Tone::Warning);
        assert!(
            BrowserStatus::Failed("crashed".into())
                .label()
                .contains("crashed")
        );
    }

    #[test]
    fn tails_keep_the_last_items() {
        let items: Vec<u32> = (0..10).collect();
        assert_eq!(tail(&items, 3), (&items[7..], 7));
        assert_eq!(tail(&items, 20), (&items[..], 0));
    }

    #[test]
    fn png_screenshots_are_images() {
        let Screenshot::Image(image) = Screenshot::png(vec![1, 2, 3]) else {
            panic!("expected an image");
        };
        assert_eq!(image.format, ImageFormat::Png);
    }
}
