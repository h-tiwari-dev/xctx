//! The Plugins menu of a space (`⌘⇧P`): the plugins that can be mounted, with where each comes from and whether
//! it is mounted, Mount and Unmount for each, and the plugin folders whose manifest could not be read.

use std::rc::Rc;

use gpui::{App, FontWeight, SharedString, Window, div, prelude::*, px};

use crate::components::{Badge, Button, ButtonSize, ButtonVariant};
use crate::theme::{Theme, Tone};

/// Where a plugin comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginSource {
    /// Shipped with xctx.
    Builtin,
    /// The user's own, under `~/.config/xctx/plugins/`.
    User,
    /// The repo's own, under `.xctx/plugins/`; needs trust the first time.
    Repo,
}

impl PluginSource {
    /// The badge text.
    pub fn label(self) -> &'static str {
        match self {
            Self::Builtin => "BUILT-IN",
            Self::User => "USER",
            Self::Repo => "REPO",
        }
    }

    /// The badge tone: built-in neutral, user accent, repo warning.
    pub fn tone(self) -> Tone {
        match self {
            Self::Builtin => Tone::Neutral,
            Self::User => Tone::Accent,
            Self::Repo => Tone::Warning,
        }
    }
}

/// One plugin in the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginEntry {
    /// Its name, unique in the menu; passed back by the handlers and used in selectors.
    pub name: SharedString,
    /// What it does. Drawn as given: already masked by the caller.
    pub description: SharedString,
    /// Where it comes from.
    pub source: PluginSource,
    /// Its pane kind, such as "http" or "terminal", if it has a pane.
    pub pane: Option<SharedString>,
    /// It is a repo plugin not trusted yet, or a connector with write tools: mounting posts a gate to the Inbox
    /// instead of mounting at once. A NEEDS TRUST badge says so.
    pub needs_trust: bool,
    /// It is mounted in this space: Unmount shows instead of Mount.
    pub mounted: bool,
}

/// A plugin folder whose manifest could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginProblemRow {
    /// The manifest's path. Drawn as given: already masked by the caller.
    pub path: SharedString,
    /// What is wrong with it. Drawn as given: already masked by the caller.
    pub message: SharedString,
}

/// What the Plugins menu shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PluginMenuView {
    /// The plugins, in the order shown.
    pub plugins: Vec<PluginEntry>,
    /// The manifests that could not be used.
    pub problems: Vec<PluginProblemRow>,
    /// A mount or unmount runs: every Mount and Unmount is disabled.
    pub busy: bool,
    /// Why the last mount or unmount failed. Drawn as given: already masked by the caller.
    pub error: Option<SharedString>,
}

/// What the menu says when there are no plugins.
pub const PLUGIN_MENU_EMPTY: &str = "No plugins found.";

/// The line under a plugin that needs trust.
pub const PLUGIN_TRUST_HINT: &str = "Mounting asks for your approval in the Inbox first.";

/// The most plugins drawn; the rest are counted in a note.
pub const PLUGIN_ROW_LIMIT: usize = 200;

/// The most problems drawn; the rest are counted in a note.
pub const PLUGIN_PROBLEM_LIMIT: usize = 50;

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type NameHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// The Plugins menu, drawn from a [`PluginMenuView`].
///
/// Selectors for tests: `plugin-row-<name>`, `plugin-mount-<name>`, `plugin-unmount-<name>`,
/// `plugin-trust-<name>` (the NEEDS TRUST badge), `plugin-mounted-<name>`, `plugin-problem-<i>`,
/// `plugin-more`, `plugin-problems-more`, `plugin-menu-empty`, `plugin-menu-error` and `plugin-menu-close`.
#[derive(IntoElement)]
pub struct PluginMenu {
    view: PluginMenuView,
    on_mount: Option<NameHandler>,
    on_unmount: Option<NameHandler>,
    on_close: Option<Handler>,
}

impl PluginMenu {
    /// A menu for `view` with no handlers.
    pub fn new(view: PluginMenuView) -> Self {
        Self {
            view,
            on_mount: None,
            on_unmount: None,
            on_close: None,
        }
    }

    /// Call `handler` with the plugin's name when its Mount is clicked.
    pub fn on_mount(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_mount = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the plugin's name when its Unmount is clicked.
    pub fn on_unmount(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_unmount = Some(Rc::new(handler));
        self
    }

    /// Show a Close button and call `handler` when it is clicked.
    pub fn on_close(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for PluginMenu {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let view = self.view;
        let busy = view.busy;

        let rows: Vec<_> = view
            .plugins
            .iter()
            .take(PLUGIN_ROW_LIMIT)
            .map(|plugin| {
                let name = plugin.name.clone();
                let (selector, label, handler, variant) = if plugin.mounted {
                    (
                        format!("plugin-unmount-{name}"),
                        "Unmount",
                        self.on_unmount.clone(),
                        ButtonVariant::Secondary,
                    )
                } else {
                    (
                        format!("plugin-mount-{name}"),
                        if plugin.needs_trust {
                            "Request mount"
                        } else {
                            "Mount"
                        },
                        self.on_mount.clone(),
                        ButtonVariant::Primary,
                    )
                };
                let mut button = Button::new(SharedString::from(selector.clone()), label)
                    .variant(variant)
                    .size(ButtonSize::Compact)
                    .disabled(busy)
                    .selector(selector);
                if let Some(handler) = handler.filter(|_| !busy) {
                    let name = name.clone();
                    button = button.on_click(move |_, window, cx| handler(&name, window, cx));
                }
                let trust = plugin.needs_trust.then(|| {
                    let name = name.clone();
                    div()
                        .debug_selector(move || format!("plugin-trust-{name}"))
                        .child(Badge::new("NEEDS TRUST").tone(Tone::Warning))
                });
                let mounted = plugin.mounted.then(|| {
                    let name = name.clone();
                    div()
                        .debug_selector(move || format!("plugin-mounted-{name}"))
                        .child(Badge::new("MOUNTED").tone(Tone::Success))
                });
                let trust_hint = (plugin.needs_trust && !plugin.mounted).then(|| {
                    div()
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_subtle)
                        .child(PLUGIN_TRUST_HINT)
                });
                let row_selector = format!("plugin-row-{name}");
                div()
                    .debug_selector(move || row_selector)
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .py(Theme::SPACE_1)
                    .border_b_1()
                    .border_color(theme.divider)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(Theme::SPACE_0_5)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(Theme::SPACE_1)
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.text)
                                            .child(name.clone()),
                                    )
                                    .child(
                                        Badge::new(plugin.source.label())
                                            .tone(plugin.source.tone()),
                                    )
                                    .children(plugin.pane.clone().map(Badge::new))
                                    .children(trust)
                                    .children(mounted),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(Theme::TEXT_SM)
                                    .text_color(theme.text_muted)
                                    .child(plugin.description.clone()),
                            )
                            .children(trust_hint),
                    )
                    .child(button)
            })
            .collect();
        let more = (view.plugins.len() > PLUGIN_ROW_LIMIT).then(|| {
            div()
                .debug_selector(|| "plugin-more".to_owned())
                .text_size(Theme::TEXT_XS)
                .text_color(theme.text_subtle)
                .child(format!(
                    "… {} more plugins",
                    view.plugins.len() - PLUGIN_ROW_LIMIT
                ))
        });
        let empty = view.plugins.is_empty().then(|| {
            div()
                .debug_selector(|| "plugin-menu-empty".to_owned())
                .text_size(Theme::TEXT_SM)
                .text_color(theme.text_subtle)
                .child(PLUGIN_MENU_EMPTY)
        });

        let problems = (!view.problems.is_empty()).then(|| {
            let rows = view
                .problems
                .iter()
                .take(PLUGIN_PROBLEM_LIMIT)
                .enumerate()
                .map(|(i, problem)| {
                    div()
                        .debug_selector(move || format!("plugin-problem-{i}"))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_family("Menlo")
                                .text_size(Theme::TEXT_XS)
                                .text_color(theme.text_muted)
                                .child(problem.path.clone()),
                        )
                        .child(
                            div()
                                .text_size(Theme::TEXT_SM)
                                .text_color(theme.danger)
                                .child(problem.message.clone()),
                        )
                });
            let hidden = view.problems.len().saturating_sub(PLUGIN_PROBLEM_LIMIT);
            div()
                .flex()
                .flex_none()
                .flex_col()
                .gap(Theme::SPACE_1)
                .child(
                    div()
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_subtle)
                        .child(format!("Problems ({})", view.problems.len())),
                )
                .children(rows)
                .children((hidden > 0).then(|| {
                    div()
                        .debug_selector(|| "plugin-problems-more".to_owned())
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_subtle)
                        .child(format!("… {hidden} more problems"))
                }))
        });

        let error = view.error.clone().map(|text| {
            div()
                .debug_selector(|| "plugin-menu-error".to_owned())
                .text_size(Theme::TEXT_SM)
                .text_color(theme.danger)
                .child(text)
        });
        let close = self.on_close.map(|handler| {
            Button::new("plugin-menu-close", "Close")
                .variant(ButtonVariant::Ghost)
                .selector("plugin-menu-close")
                .on_click(move |_, window, cx| handler(window, cx))
        });

        div()
            .id("plugin-menu")
            .flex()
            .flex_col()
            .gap(Theme::SPACE_2)
            .w_full()
            .max_h_full()
            .overflow_y_scroll()
            .p(Theme::SPACE_3)
            .rounded(Theme::RADIUS_LG)
            .border_1()
            .border_color(theme.border)
            .bg(theme.elevated)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(
                        div()
                            .flex_1()
                            .text_size(Theme::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("Plugins"),
                    )
                    .children(close),
            )
            .children(error)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_col()
                    .min_h(px(0.0))
                    .children(rows)
                    .children(more)
                    .children(empty),
            )
            .children(problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_have_labels_and_tones() {
        assert_eq!(PluginSource::Builtin.label(), "BUILT-IN");
        assert_eq!(PluginSource::Repo.tone(), Tone::Warning);
        assert_eq!(PluginSource::User.tone(), Tone::Accent);
    }
}
