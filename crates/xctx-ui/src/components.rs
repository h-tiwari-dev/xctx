//! Buttons, pills and badges with the theme's colours and hover states, shared by the bars and by the app's
//! Home, draft and ship/discard views.

use std::rc::Rc;

use gpui::{
    App, ClickEvent, ElementId, FontWeight, Hsla, Pixels, SharedString, Window, div, prelude::*,
    px, transparent_black,
};

use crate::theme::{Theme, Tone};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// How prominent a [`Button`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ButtonVariant {
    /// The main action of a view, filled with the accent.
    Primary,
    /// An ordinary action on a raised surface with an outline.
    #[default]
    Secondary,
    /// A quiet action with no fill until hovered, for toolbars and pane headers.
    Ghost,
    /// A destructive action, tinted with the danger colour.
    Danger,
}

/// The colours of a button in one state, from [`ButtonVariant::colors`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ButtonColors {
    /// Resting fill.
    pub bg: Hsla,
    /// Fill under the mouse.
    pub bg_hover: Hsla,
    /// Label colour.
    pub fg: Hsla,
    /// Outline.
    pub border: Hsla,
}

/// Raise a colour's lightness by `amount` (0 to 1), keeping hue, saturation and alpha.
fn lighten(color: Hsla, amount: f32) -> Hsla {
    Hsla {
        l: (color.l + amount).clamp(0.0, 1.0),
        ..color
    }
}

impl ButtonVariant {
    /// The colours of this variant in `theme`. A `selected` button (a toggled pill, the current tab) shows the
    /// accent whatever its variant.
    pub fn colors(self, theme: &Theme, selected: bool) -> ButtonColors {
        if selected {
            return ButtonColors {
                bg: theme.accent_muted,
                bg_hover: theme.accent.alpha(0.32),
                fg: theme.text,
                border: theme.accent.alpha(0.55),
            };
        }
        match self {
            Self::Primary => ButtonColors {
                bg: theme.accent,
                bg_hover: lighten(theme.accent, 0.06),
                fg: theme.bg,
                border: theme.accent,
            },
            Self::Secondary => ButtonColors {
                bg: theme.elevated,
                bg_hover: theme.surface_hover,
                fg: theme.text,
                border: theme.border,
            },
            Self::Ghost => ButtonColors {
                bg: transparent_black(),
                bg_hover: theme.surface_hover,
                fg: theme.text_muted,
                border: transparent_black(),
            },
            Self::Danger => {
                let tone = theme.tone(Tone::Danger);
                ButtonColors {
                    bg: tone.bg,
                    bg_hover: theme.danger.alpha(0.24),
                    fg: tone.fg,
                    border: tone.border,
                }
            }
        }
    }
}

/// How big a [`Button`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ButtonSize {
    /// A small square-ish button for glyphs in pane headers (18px high).
    Compact,
    /// The bar and dialog button (24px high).
    #[default]
    Regular,
    /// A call to action such as Launch (32px high).
    Large,
}

impl ButtonSize {
    /// The button's height.
    pub fn height(self) -> Pixels {
        match self {
            Self::Compact => px(18.0),
            Self::Regular => px(24.0),
            Self::Large => px(32.0),
        }
    }

    /// The horizontal padding on each side of the label.
    pub fn padding_x(self) -> Pixels {
        match self {
            Self::Compact => px(4.0),
            Self::Regular => Theme::SPACE_2,
            Self::Large => Theme::SPACE_4,
        }
    }

    /// The label's font size.
    pub fn text_size(self) -> Pixels {
        match self {
            Self::Compact => Theme::TEXT_XS,
            Self::Regular => Theme::TEXT_SM,
            Self::Large => Theme::TEXT_MD,
        }
    }
}

/// A clickable text button. Its `id` must be unique among its siblings' ids, as GPUI tracks clicks by id.
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: SharedString,
    variant: ButtonVariant,
    size: ButtonSize,
    selected: bool,
    disabled: bool,
    rounded_full: bool,
    on_click: Option<ClickHandler>,
}

impl Button {
    /// A [`ButtonVariant::Secondary`], [`ButtonSize::Regular`] button showing `label`.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            variant: ButtonVariant::default(),
            size: ButtonSize::default(),
            selected: false,
            disabled: false,
            rounded_full: false,
            on_click: None,
        }
    }

    /// Set how prominent the button is.
    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    /// Set the button's size.
    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    /// Show the button as toggled on.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Dim the button and ignore clicks.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Round the ends fully, as a pill.
    pub fn rounded_full(mut self) -> Self {
        self.rounded_full = true;
        self
    }

    /// Call `handler` when the button is clicked (not while disabled).
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let colors = self.variant.colors(&theme, self.selected);
        let radius = if self.rounded_full {
            Theme::RADIUS_FULL
        } else {
            Theme::RADIUS_SM
        };
        let weight = match self.variant {
            ButtonVariant::Primary => FontWeight::SEMIBOLD,
            _ if self.selected => FontWeight::SEMIBOLD,
            _ => FontWeight::MEDIUM,
        };
        let mut button = div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(self.size.height())
            .min_w(self.size.height())
            .px(self.size.padding_x())
            .rounded(radius)
            .border_1()
            .border_color(colors.border)
            .bg(colors.bg)
            .text_color(colors.fg)
            .text_size(self.size.text_size())
            .font_weight(weight)
            .whitespace_nowrap()
            .child(self.label);
        if self.disabled {
            button = button.opacity(0.45);
        } else {
            let hover_bg = colors.bg_hover;
            button = button
                .cursor_pointer()
                .hover(move |style| style.bg(hover_bg));
            if let Some(handler) = self.on_click {
                button = button.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx);
                });
            }
        }
        button
    }
}

/// A fully rounded toggle, such as a shape in the tab bar's shape selector.
#[derive(IntoElement)]
pub struct Pill {
    button: Button,
}

impl Pill {
    /// An unselected pill showing `label`.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            button: Button::new(id, label)
                .variant(ButtonVariant::Ghost)
                .rounded_full(),
        }
    }

    /// Show the pill as the current choice.
    pub fn selected(mut self, selected: bool) -> Self {
        self.button = self.button.selected(selected);
        self
    }

    /// Call `handler` when the pill is clicked.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.button = self.button.on_click(handler);
        self
    }
}

impl RenderOnce for Pill {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.button
    }
}

/// A small non-interactive label tinted by its [`Tone`], such as a pane's role or a repo count.
#[derive(IntoElement)]
pub struct Badge {
    label: SharedString,
    tone: Tone,
}

impl Badge {
    /// A [`Tone::Neutral`] badge showing `label`.
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            tone: Tone::Neutral,
        }
    }

    /// Set the badge's tone.
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }
}

impl RenderOnce for Badge {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = Theme::of(cx).tone(self.tone);
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(16.0))
            .px(Theme::SPACE_1)
            .rounded(Theme::RADIUS_SM)
            .border_1()
            .border_color(colors.border)
            .bg(colors.bg)
            .text_color(colors.fg)
            .text_size(px(10.0))
            .font_weight(FontWeight::SEMIBOLD)
            .whitespace_nowrap()
            .child(self.label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VARIANTS: [ButtonVariant; 4] = [
        ButtonVariant::Primary,
        ButtonVariant::Secondary,
        ButtonVariant::Ghost,
        ButtonVariant::Danger,
    ];

    #[test]
    fn every_variant_changes_on_hover() {
        let theme = Theme::dark();
        for variant in VARIANTS {
            for selected in [false, true] {
                let colors = variant.colors(&theme, selected);
                assert_ne!(
                    colors.bg, colors.bg_hover,
                    "{variant:?} selected={selected}"
                );
            }
        }
    }

    #[test]
    fn selected_looks_the_same_for_every_variant() {
        let theme = Theme::dark();
        let selected = ButtonVariant::Ghost.colors(&theme, true);
        for variant in VARIANTS {
            assert_eq!(variant.colors(&theme, true), selected);
        }
        assert_eq!(selected.bg, theme.accent_muted);
    }

    #[test]
    fn primary_is_filled_with_the_accent_and_danger_uses_danger() {
        let theme = Theme::dark();
        let primary = ButtonVariant::Primary.colors(&theme, false);
        assert_eq!(primary.bg, theme.accent);
        assert!(primary.bg_hover.l > primary.bg.l);
        assert_eq!(ButtonVariant::Danger.colors(&theme, false).fg, theme.danger);
        assert_eq!(ButtonVariant::Ghost.colors(&theme, false).bg.a, 0.0);
    }

    #[test]
    fn lighten_clamps_and_keeps_alpha() {
        let color = Hsla {
            h: 0.5,
            s: 0.5,
            l: 0.97,
            a: 0.4,
        };
        let lighter = lighten(color, 0.1);
        assert_eq!(lighter.l, 1.0);
        assert_eq!(lighter.a, 0.4);
        assert_eq!(lighter.h, 0.5);
    }

    #[test]
    fn sizes_grow() {
        let sizes = [ButtonSize::Compact, ButtonSize::Regular, ButtonSize::Large];
        for pair in sizes.windows(2) {
            assert!(pair[0].height() < pair[1].height());
            assert!(pair[0].padding_x() < pair[1].padding_x());
            assert!(pair[0].text_size() < pair[1].text_size());
        }
    }
}
