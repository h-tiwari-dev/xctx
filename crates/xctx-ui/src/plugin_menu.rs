//! The Plugins menu of a space (`⌘⇧P`): what is in the space (its mounted plugins, each with what it shows, Show
//! and Remove), what can be added (grouped into panes, tools, connectors and the rest, one click to add, or an
//! inline setting for a plugin that takes one), and the plugin folders whose manifest could not be read.

use std::rc::Rc;

use gpui::{AnyElement, App, FontWeight, SharedString, Window, div, prelude::*, px};

use crate::components::{Badge, Button, ButtonSize, ButtonVariant};
use crate::icon::{Icon, IconName, IconSize};
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
    /// The badge a row shows for it, with its tone: none for a built-in plugin, "Yours" for the user's own and
    /// "Repo" for the repo's.
    pub fn badge(self) -> Option<(&'static str, Tone)> {
        match self {
            Self::Builtin => None,
            Self::User => Some(("Yours", Tone::Accent)),
            Self::Repo => Some(("Repo", Tone::Neutral)),
        }
    }
}

/// The group a plugin is listed under in the Add section, in the order the groups are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PluginGroup {
    /// Panes that show the space's work: a terminal, logs, a browser, files.
    Panes,
    /// Tools to poke the space's app: the API client, a database.
    Tools,
    /// Outside services, such as Jira or Figma.
    Connectors,
    /// Anything else.
    Other,
}

impl PluginGroup {
    /// Every group, in the order drawn.
    pub const ALL: [Self; 4] = [Self::Panes, Self::Tools, Self::Connectors, Self::Other];

    /// The group's heading.
    pub fn title(self) -> &'static str {
        match self {
            Self::Panes => "Panes",
            Self::Tools => "Tools",
            Self::Connectors => "Connectors",
            Self::Other => "Other",
        }
    }
}

/// The one setting a plugin asks for when it is added, such as a terminal's command or the browser's page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSetting {
    /// What the input is for, drawn above it ("Command", "Page").
    pub label: SharedString,
    /// The text the host puts in its input when the row opens. Already masked by the caller.
    pub default: SharedString,
    /// A line under the input saying what a blank or another value does. Drawn as given.
    pub hint: Option<SharedString>,
}

/// A plugin that can be added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginEntry {
    /// Its manifest name, unique in the menu; passed back by the handlers and used in selectors.
    pub name: SharedString,
    /// The name shown, such as "API client".
    pub title: SharedString,
    /// What it does. Drawn as given: already masked by the caller.
    pub description: SharedString,
    /// The icon drawn before its title.
    pub icon: IconName,
    /// Where it comes from: a non-built-in plugin gets a small badge.
    pub source: PluginSource,
    /// The group it is listed under.
    pub group: PluginGroup,
    /// It is a repo plugin not trusted yet: adding it posts a gate to the Inbox instead of mounting at once. A
    /// "Needs approval" badge and [`PLUGIN_TRUST_HINT`] say so.
    pub needs_trust: bool,
    /// The setting it asks for; `None` mounts on one click.
    pub setting: Option<PluginSetting>,
}

/// A plugin mounted in the space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountedEntry {
    /// Its manifest name; passed back by the handlers and used in selectors.
    pub name: SharedString,
    /// The name shown, such as "Browser".
    pub title: SharedString,
    /// The icon drawn before its title.
    pub icon: IconName,
    /// What it shows, such as the browser's page or the terminal's command, when known. Drawn as given: already
    /// masked by the caller.
    pub showing: Option<SharedString>,
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
    /// The plugins mounted in the space, in mount order. The "In this space" section is hidden when empty.
    pub mounted: Vec<MountedEntry>,
    /// The plugins that can be added, in the order shown within each group.
    pub plugins: Vec<PluginEntry>,
    /// The plugin whose setting row is open, if any: only one is open at a time.
    pub expanded: Option<SharedString>,
    /// The manifests that could not be used.
    pub problems: Vec<PluginProblemRow>,
    /// A mount or unmount runs: no row or button reacts.
    pub busy: bool,
    /// Why the last mount or unmount failed. Drawn as given: already masked by the caller.
    pub error: Option<SharedString>,
}

/// What the menu says when there are no plugins to add.
pub const PLUGIN_MENU_EMPTY: &str = "No plugins found.";

/// The line under a plugin that needs approval.
pub const PLUGIN_TRUST_HINT: &str = "Mounting asks for your approval in the Inbox first.";

/// The most rows drawn in each section; the rest are counted in a note.
pub const PLUGIN_ROW_LIMIT: usize = 200;

/// The most problems drawn; the rest are counted in a note.
pub const PLUGIN_PROBLEM_LIMIT: usize = 50;

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type NameHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;
type ExpandHandler = Rc<dyn Fn(Option<&SharedString>, &mut Window, &mut App)>;

/// The Plugins menu, drawn from a [`PluginMenuView`].
///
/// Clicking an Add row of a plugin with no setting calls [`PluginMenu::on_add`]. Clicking one with a setting calls
/// [`PluginMenu::on_expand`] with its name (or `None` when it is the open row); the host then sets
/// [`PluginMenuView::expanded`], puts [`PluginSetting::default`] in its input and passes that input to
/// [`PluginMenu::setting_input`], and the open row's Add button calls [`PluginMenu::on_submit`] (Enter and Escape
/// in the input are the host's to handle). Nothing reacts while [`PluginMenuView::busy`], except Close.
///
/// Selectors for tests: `plugin-mounted-<name>` (a mounted row), `plugin-show-<name>`, `plugin-remove-<name>`,
/// `plugin-row-<name>` (an Add row with its open setting), `plugin-add-<name>` (the clickable part of an Add
/// row), `plugin-setting-<name>` (the open setting's input), `plugin-submit-<name>` (its Add button),
/// `plugin-source-<name>` (the Yours / Repo badge), `plugin-trust-<name>` (the Needs approval badge),
/// `plugin-group-<group>` (an Add group's heading, lowercase), `plugin-problem-<i>`, `plugin-mounted-more`,
/// `plugin-more`, `plugin-problems-more`, `plugin-menu-empty`, `plugin-menu-error` and `plugin-menu-close`.
#[derive(IntoElement)]
pub struct PluginMenu {
    view: PluginMenuView,
    setting_input: Option<AnyElement>,
    on_add: Option<NameHandler>,
    on_expand: Option<ExpandHandler>,
    on_submit: Option<NameHandler>,
    on_show: Option<NameHandler>,
    on_remove: Option<NameHandler>,
    on_close: Option<Handler>,
}

impl PluginMenu {
    /// A menu for `view` with no handlers.
    pub fn new(view: PluginMenuView) -> Self {
        Self {
            view,
            setting_input: None,
            on_add: None,
            on_expand: None,
            on_submit: None,
            on_show: None,
            on_remove: None,
            on_close: None,
        }
    }

    /// Draw `input` (the host's text input holding the setting) in the open setting row.
    pub fn setting_input(mut self, input: impl IntoElement) -> Self {
        self.setting_input = Some(input.into_any_element());
        self
    }

    /// Call `handler` with the plugin's name when an Add row of a plugin with no setting is clicked.
    pub fn on_add(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_add = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when an Add row of a plugin with a setting is clicked: with its name to open it, or `None`
    /// when it is the open row, to close it.
    pub fn on_expand(
        mut self,
        handler: impl Fn(Option<&SharedString>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_expand = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the plugin's name when the open setting row's Add button is clicked.
    pub fn on_submit(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_submit = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the plugin's name when a mounted row's Show is clicked.
    pub fn on_show(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_show = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the plugin's name when a mounted row's Remove is clicked.
    pub fn on_remove(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_remove = Some(Rc::new(handler));
        self
    }

    /// Show a Close button and call `handler` when it is clicked.
    pub fn on_close(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(handler));
        self
    }
}

fn section_title(text: impl Into<SharedString>, theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .pt(Theme::SPACE_1)
        .text_size(Theme::TEXT_XS)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text_subtle)
        .child(text.into())
}

fn note(selector: &'static str, text: String, theme: &Theme) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_owned())
        .text_size(Theme::TEXT_XS)
        .text_color(theme.text_subtle)
        .child(text)
}

/// A button for a row, `name`d in its `selector`, calling `handler` unless `busy`.
fn row_button(
    selector: String,
    label: &'static str,
    variant: ButtonVariant,
    busy: bool,
    name: &SharedString,
    handler: Option<NameHandler>,
) -> Button {
    let mut button = Button::new(SharedString::from(selector.clone()), label)
        .variant(variant)
        .size(ButtonSize::Compact)
        .disabled(busy)
        .selector(selector);
    if let Some(handler) = handler.filter(|_| !busy) {
        let name = name.clone();
        button = button.on_click(move |_, window, cx| handler(&name, window, cx));
    }
    button
}

impl RenderOnce for PluginMenu {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let view = self.view;
        let busy = view.busy;

        let mounted_rows: Vec<_> = view
            .mounted
            .iter()
            .take(PLUGIN_ROW_LIMIT)
            .map(|plugin| {
                let name = plugin.name.clone();
                let show = row_button(
                    format!("plugin-show-{name}"),
                    "Show",
                    ButtonVariant::Secondary,
                    busy,
                    &name,
                    self.on_show.clone(),
                );
                let remove = row_button(
                    format!("plugin-remove-{name}"),
                    "Remove",
                    ButtonVariant::Ghost,
                    busy,
                    &name,
                    self.on_remove.clone(),
                );
                let showing = plugin.showing.clone().map(|text| {
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family("Menlo")
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_muted)
                        .child(text)
                });
                let selector = format!("plugin-mounted-{name}");
                div()
                    .debug_selector(move || selector)
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .py(Theme::SPACE_1)
                    .child(
                        Icon::new(plugin.icon)
                            .size(IconSize::Md)
                            .color(theme.text_muted),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text)
                                    .child(plugin.title.clone()),
                            )
                            .children(showing),
                    )
                    .child(show)
                    .child(remove)
            })
            .collect();
        let mounted_more = view.mounted.len().saturating_sub(PLUGIN_ROW_LIMIT);
        let in_space = (!view.mounted.is_empty()).then(|| {
            div()
                .flex()
                .flex_none()
                .flex_col()
                .pb(Theme::SPACE_1)
                .border_b_1()
                .border_color(theme.border_subtle)
                .child(section_title("In this space", &theme))
                .children(mounted_rows)
                .children((mounted_more > 0).then(|| {
                    note(
                        "plugin-mounted-more",
                        format!("… {mounted_more} more mounted"),
                        &theme,
                    )
                }))
        });

        let mut setting_input = self.setting_input;
        let shown: Vec<&PluginEntry> = view.plugins.iter().take(PLUGIN_ROW_LIMIT).collect();
        let groups: Vec<_> = PluginGroup::ALL
            .into_iter()
            .filter_map(|group| {
                let rows: Vec<_> = shown
                    .iter()
                    .filter(|plugin| plugin.group == group)
                    .map(|plugin| {
                        let name = plugin.name.clone();
                        let open = plugin.setting.is_some()
                            && view.expanded.as_ref() == Some(&plugin.name);
                        let source = plugin.source.badge().map(|(label, tone)| {
                            let name = name.clone();
                            div()
                                .debug_selector(move || format!("plugin-source-{name}"))
                                .child(Badge::new(label).tone(tone))
                        });
                        let trust = plugin.needs_trust.then(|| {
                            let name = name.clone();
                            div()
                                .debug_selector(move || format!("plugin-trust-{name}"))
                                .child(Badge::new("Needs approval").tone(Tone::Warning))
                        });
                        let trust_hint = plugin.needs_trust.then(|| {
                            div()
                                .text_size(Theme::TEXT_XS)
                                .text_color(theme.text_subtle)
                                .child(PLUGIN_TRUST_HINT)
                        });
                        let action: AnyElement = match &plugin.setting {
                            Some(_) => Icon::new(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .size(IconSize::Sm)
                            .color(theme.text_muted)
                            .into_any_element(),
                            None => div()
                                .flex()
                                .items_center()
                                .gap(Theme::SPACE_0_5)
                                .text_size(Theme::TEXT_SM)
                                .text_color(theme.accent)
                                .child(
                                    Icon::new(IconName::Plus)
                                        .size(IconSize::Xs)
                                        .color(theme.accent),
                                )
                                .child(if plugin.needs_trust { "Request" } else { "Add" })
                                .into_any_element(),
                        };
                        let add_selector = format!("plugin-add-{name}");
                        let mut header = div()
                            .id(SharedString::from(add_selector.clone()))
                            .debug_selector(move || add_selector)
                            .flex()
                            .items_center()
                            .gap(Theme::SPACE_2)
                            .px(Theme::SPACE_1)
                            .py(Theme::SPACE_1)
                            .rounded(Theme::RADIUS_SM)
                            .child(
                                Icon::new(plugin.icon)
                                    .size(IconSize::Md)
                                    .color(theme.text_muted),
                            )
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
                                                    .child(plugin.title.clone()),
                                            )
                                            .children(source)
                                            .children(trust),
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
                            .child(action);
                        if busy {
                            header = header.opacity(0.45);
                        } else {
                            let raised = theme.raised;
                            header = header.cursor_pointer().hover(move |s| s.bg(raised));
                            let click: Option<Handler> = if plugin.setting.is_some() {
                                self.on_expand.clone().map(|handler| {
                                    let name = name.clone();
                                    Rc::new(move |window: &mut Window, cx: &mut App| {
                                        handler((!open).then_some(&name), window, cx)
                                    }) as Handler
                                })
                            } else {
                                self.on_add.clone().map(|handler| {
                                    let name = name.clone();
                                    Rc::new(move |window: &mut Window, cx: &mut App| {
                                        handler(&name, window, cx)
                                    }) as Handler
                                })
                            };
                            if let Some(click) = click {
                                header = header.on_click(move |_, window, cx| click(window, cx));
                            }
                        }
                        let setting = plugin.setting.as_ref().filter(|_| open).map(|setting| {
                            let input_selector = format!("plugin-setting-{name}");
                            let submit = row_button(
                                format!("plugin-submit-{name}"),
                                if plugin.needs_trust { "Request" } else { "Add" },
                                ButtonVariant::Primary,
                                busy,
                                &name,
                                self.on_submit.clone(),
                            );
                            div()
                                .flex()
                                .flex_col()
                                .gap(Theme::SPACE_1)
                                .pl(px(28.0))
                                .pr(Theme::SPACE_1)
                                .pb(Theme::SPACE_2)
                                .child(
                                    div()
                                        .text_size(Theme::TEXT_XS)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.text_muted)
                                        .child(setting.label.clone()),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(Theme::SPACE_2)
                                        .child(
                                            div()
                                                .debug_selector(move || input_selector)
                                                .flex_1()
                                                .min_w_0()
                                                .children(setting_input.take()),
                                        )
                                        .child(submit),
                                )
                                .children(setting.hint.clone().map(|hint| {
                                    div()
                                        .text_size(Theme::TEXT_XS)
                                        .text_color(theme.text_subtle)
                                        .child(hint)
                                }))
                        });
                        let row_selector = format!("plugin-row-{name}");
                        div()
                            .debug_selector(move || row_selector)
                            .flex()
                            .flex_none()
                            .flex_col()
                            .rounded(Theme::RADIUS_SM)
                            .when(open, |row| row.bg(theme.raised))
                            .child(header)
                            .children(setting)
                    })
                    .collect();
                (!rows.is_empty()).then(|| {
                    let selector = format!("plugin-group-{}", group.title().to_lowercase());
                    div()
                        .flex()
                        .flex_none()
                        .flex_col()
                        .child(
                            section_title(group.title(), &theme).debug_selector(move || selector),
                        )
                        .children(rows)
                })
            })
            .collect();
        let more = (view.plugins.len() > PLUGIN_ROW_LIMIT).then(|| {
            note(
                "plugin-more",
                format!("… {} more plugins", view.plugins.len() - PLUGIN_ROW_LIMIT),
                &theme,
            )
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
                .pt(Theme::SPACE_1)
                .border_t_1()
                .border_color(theme.border_subtle)
                .child(section_title(
                    format!("Could not load ({})", view.problems.len()),
                    &theme,
                ))
                .children(rows)
                .children((hidden > 0).then(|| {
                    note(
                        "plugin-problems-more",
                        format!("… {hidden} more problems"),
                        &theme,
                    )
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
            .bg(theme.overlay)
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
            .children(in_space)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_col()
                    .gap(Theme::SPACE_1)
                    .min_h(px(0.0))
                    .child(section_title("Add", &theme).text_size(Theme::TEXT_SM))
                    .children(groups)
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
    fn only_plugins_not_built_in_get_a_source_badge() {
        assert_eq!(PluginSource::Builtin.badge(), None);
        assert_eq!(PluginSource::User.badge(), Some(("Yours", Tone::Accent)));
        assert_eq!(
            PluginSource::Repo.badge().map(|(label, _)| label),
            Some("Repo")
        );
    }

    #[test]
    fn groups_are_drawn_panes_first() {
        let mut groups = PluginGroup::ALL.to_vec();
        groups.sort();
        assert_eq!(groups, PluginGroup::ALL);
        assert_eq!(PluginGroup::Connectors.title(), "Connectors");
    }
}
