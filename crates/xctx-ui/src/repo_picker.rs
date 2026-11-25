//! A list of repos to pick for a space or a plan: each row a repo's name and folder with a check mark when it is
//! picked, clicked to pick or unpick it, and "Choose folders…" under it to pick repos from disk.

use std::rc::Rc;

use gpui::{App, ElementId, FontWeight, SharedString, Window, div, prelude::*, px};

use crate::components::{Button, ButtonSize, ButtonVariant};
use crate::theme::Theme;

/// One repo in a [`RepoPicker`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoChoice {
    /// The repo's folder name.
    pub name: SharedString,
    /// Its folder as shown, also what [`RepoPicker::on_toggle`] is given.
    pub path: SharedString,
    /// Whether it is picked (drawn with a check mark).
    pub picked: bool,
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type PathHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// The repo list, drawn from [`RepoChoice`]s in the order given; it scrolls past about six rows.
///
/// Selectors for tests: `repo-choice-<name>` (a row), `repo-picked-<name>` (the check mark of a picked row),
/// `repo-picker-empty` and `repo-picker-browse`.
#[derive(IntoElement)]
pub struct RepoPicker {
    id: SharedString,
    choices: Vec<RepoChoice>,
    empty: SharedString,
    disabled: bool,
    on_toggle: Option<PathHandler>,
    on_browse: Option<Handler>,
}

impl RepoPicker {
    /// A picker whose element ids start with `id`, listing `choices`.
    pub fn new(id: impl Into<SharedString>, choices: Vec<RepoChoice>) -> Self {
        Self {
            id: id.into(),
            choices,
            empty: SharedString::default(),
            disabled: false,
            on_toggle: None,
            on_browse: None,
        }
    }

    /// What to say when there are no choices.
    pub fn empty(mut self, message: impl Into<SharedString>) -> Self {
        self.empty = message.into();
        self
    }

    /// Draw the rows and the button without reacting to clicks, while what was picked is being used.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Call `handler` with a row's [`RepoChoice::path`] when the row is clicked.
    pub fn on_toggle(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }

    /// Offer "Choose folders…", calling `handler` when it is clicked.
    pub fn on_browse(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_browse = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for RepoPicker {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let disabled = self.disabled;
        let rows: Vec<_> = self
            .choices
            .into_iter()
            .enumerate()
            .map(|(i, choice)| {
                let name = choice.name.clone();
                let check = div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(16.0))
                    .rounded(Theme::RADIUS_SM)
                    .border_1()
                    .when(choice.picked, |check| {
                        let name = name.clone();
                        check
                            .debug_selector(move || format!("repo-picked-{name}"))
                            .border_color(theme.accent)
                            .bg(theme.accent)
                            .text_color(theme.on_accent)
                            .text_size(Theme::TEXT_XS)
                            .child("✓")
                    })
                    .when(!choice.picked, |check| {
                        check.border_color(theme.border_strong)
                    });
                let mut row = div()
                    .id(ElementId::NamedInteger(self.id.clone(), i as u64))
                    .debug_selector(move || format!("repo-choice-{name}"))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .px(Theme::SPACE_2)
                    .py(Theme::SPACE_1)
                    .rounded(Theme::RADIUS_MD)
                    .when(choice.picked, |row| row.bg(theme.selection))
                    .child(check)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_size(Theme::TEXT_MD)
                                    .text_color(theme.text)
                                    .child(choice.name.clone()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(Theme::TEXT_XS)
                                    .text_color(theme.text_muted)
                                    .child(choice.path.clone()),
                            ),
                    );
                if let Some(handler) = self.on_toggle.clone().filter(|_| !disabled) {
                    let path = choice.path.clone();
                    row = row
                        .cursor_pointer()
                        .hover(|row| row.bg(theme.raised))
                        .on_click(move |_, window, cx| handler(&path, window, cx));
                }
                row
            })
            .collect();
        let empty = (rows.is_empty() && !self.empty.is_empty()).then(|| {
            div()
                .debug_selector(|| "repo-picker-empty".to_owned())
                .py(Theme::SPACE_1)
                .text_size(Theme::TEXT_SM)
                .text_color(theme.text_subtle)
                .child(self.empty.clone())
        });
        let browse = self.on_browse.map(|handler| {
            let mut button = Button::new(
                SharedString::from(format!("{}-browse", self.id)),
                "Choose folders…",
            )
            .variant(ButtonVariant::Secondary)
            .size(ButtonSize::Compact)
            .disabled(disabled)
            .selector("repo-picker-browse");
            if !disabled {
                button = button.on_click(move |_, window, cx| handler(window, cx));
            }
            div().flex().child(button)
        });
        div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_2)
            .child(
                div()
                    .id(SharedString::from(format!("{}-list", self.id)))
                    .flex()
                    .flex_col()
                    .max_h(px(232.0))
                    .overflow_y_scroll()
                    .children(rows)
                    .children(empty),
            )
            .children(browse)
    }
}
