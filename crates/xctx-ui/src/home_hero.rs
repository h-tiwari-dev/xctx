//! Landing screen chrome for Home: the dot-grid [`HomeBackdrop`], the pixel-logo [`HomeHero`],
//! and the centred [`ComposerCard`] with its footer [`ComposerChip`]s, glowing horizon line and
//! soft under-card glow.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, BoxShadow, ClickEvent, ElementId, FontWeight, Hsla, SharedString,
    Window, canvas, div, fill, linear_color_stop, linear_gradient, point, prelude::*, px, relative,
    size,
};

use crate::components::Tooltip;
use crate::icon::{Icon, IconName, IconSize};
use crate::spinner::Spinner;
use crate::theme::{Theme, Tone};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

const DOT_STEP_PX: f32 = 22.0;
const DOT_SIZE_PX: f32 = 2.0;
const DOT_MAX_ALPHA: f32 = 0.5;
const DOT_MIN_ALPHA: f32 = 0.02;
const DOT_MAX_COUNT: usize = 6000;

const LOGO_GRID: usize = 5;
const COMPOSER_MAX_WIDTH: f32 = 780.0;
const COMPOSER_RADIUS: f32 = 22.0;
const HORIZON_OVERHANG: f32 = 320.0;
const SUBMIT_SIZE: f32 = 36.0;
const CHIP_HEIGHT: f32 = 26.0;

/// Decorative, non-interactive backdrop; absolutely fills its parent (parent must be `.relative()`),
/// drawn behind siblings added after it.
#[derive(IntoElement, Default)]
pub struct HomeBackdrop;

impl HomeBackdrop {
    /// Create a new dot-grid backdrop.
    pub fn new() -> Self {
        Self
    }
}

impl RenderOnce for HomeBackdrop {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let dot_color = Theme::of(cx).text_subtle;
        div().absolute().inset_0().child(
            canvas(
                |_bounds, _window, _cx| {},
                move |bounds, (), window, _cx| {
                    paint_dot_grid(bounds, dot_color, window);
                },
            )
            .size_full(),
        )
    }
}

fn paint_dot_grid(bounds: Bounds<gpui::Pixels>, dot_color: Hsla, window: &mut Window) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    if width <= 0.0 || height <= 0.0 {
        return;
    }

    let step = ((width * height) / DOT_MAX_COUNT as f32)
        .sqrt()
        .max(DOT_STEP_PX);
    let half_w = width * 0.5;
    let half_h = height * 0.5;
    let dot_size = size(px(DOT_SIZE_PX), px(DOT_SIZE_PX));

    let mut painted = 0usize;
    let mut y = step * 0.5;
    while y < height {
        let ny = ((y - half_h) / half_h).abs().min(1.0);
        let fy = (1.0 - ny * ny).max(0.0);
        let fy = fy * fy;
        if DOT_MAX_ALPHA * fy >= DOT_MIN_ALPHA {
            let mut x = step * 0.5;
            while x < width {
                let nx = ((x - half_w) / half_w).abs().min(1.0);
                let fx = (1.0 - nx * nx).max(0.0);
                let alpha = DOT_MAX_ALPHA * fx * fy;
                if alpha >= DOT_MIN_ALPHA {
                    if painted >= DOT_MAX_COUNT {
                        return;
                    }
                    let origin = point(bounds.origin.x + px(x), bounds.origin.y + px(y));
                    window.paint_quad(fill(Bounds::new(origin, dot_size), dot_color.alpha(alpha)));
                    painted += 1;
                }
                x += step;
            }
        }
        y += step;
    }
}

/// Pixel logo + `"xctx"` wordmark + version/tagline line, centred.
#[derive(IntoElement)]
pub struct HomeHero {
    tagline: SharedString,
    compact: bool,
}

impl HomeHero {
    /// Create a hero header with `tagline` shown below the wordmark in normal (non-compact) mode.
    pub fn new(tagline: impl Into<SharedString>) -> Self {
        Self {
            tagline: tagline.into(),
            compact: false,
        }
    }

    /// Use a smaller logo and wordmark with less vertical padding and no tagline.
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }
}

fn logo_row_color(theme: &Theme, row: usize) -> Hsla {
    let top = theme.agent[2];
    let mid = theme.agent[0];
    let bot = theme.agent[4];
    match row {
        0 => top,
        1 => top.blend(mid.alpha(0.5)),
        2 => mid,
        3 => mid.blend(bot.alpha(0.5)),
        _ => bot,
    }
}

impl RenderOnce for HomeHero {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let (cell_size, cell_gap, cell_radius) = if self.compact {
            (px(4.0), px(1.5), px(1.0))
        } else {
            (px(7.0), px(2.0), px(2.0))
        };

        let logo = div()
            .flex()
            .flex_col()
            .gap(cell_gap)
            .children((0..LOGO_GRID).map(|row| {
                let color = logo_row_color(&theme, row);
                div()
                    .flex()
                    .flex_row()
                    .gap(cell_gap)
                    .children((0..LOGO_GRID).map(move |col| {
                        let is_x = col == row || col + row == LOGO_GRID - 1;
                        if is_x {
                            div().size(cell_size).rounded(cell_radius).bg(color)
                        } else {
                            div().size(cell_size)
                        }
                    }))
            }));

        let wordmark = div()
            .text_size(if self.compact { px(22.0) } else { px(40.0) })
            .line_height(if self.compact { px(24.0) } else { px(42.0) })
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.text)
            .child("xctx");

        let mut text_col = div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_0_5)
            .child(wordmark);
        if !self.compact && !self.tagline.is_empty() {
            text_col = text_col.child(
                div()
                    .font_family("Menlo")
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_subtle)
                    .child(self.tagline),
            );
        }

        let (pad_top, pad_bottom, group_gap) = if self.compact {
            (Theme::SPACE_3, Theme::SPACE_2, Theme::SPACE_2)
        } else {
            (px(56.0), px(36.0), Theme::SPACE_4)
        };

        div()
            .flex()
            .w_full()
            .items_center()
            .justify_center()
            .pt(pad_top)
            .pb(pad_bottom)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(group_gap)
                    .child(logo)
                    .child(text_col),
            )
    }
}

/// The composer: rounded card holding the input, a chip row and the submit button, with the
/// horizon line and glow drawn around it.
#[derive(IntoElement)]
pub struct ComposerCard {
    input: AnyElement,
    focused: bool,
    can_submit: bool,
    left_readout: Option<SharedString>,
    right_readout: Option<SharedString>,
    chips: Vec<ComposerChip>,
    on_submit: Option<ClickHandler>,
}

impl ComposerCard {
    /// Create a composer card wrapping `input` (typically a borderless, multiline `PromptInput`).
    pub fn new(input: impl IntoElement) -> Self {
        Self {
            input: input.into_any_element(),
            focused: false,
            can_submit: false,
            left_readout: None,
            right_readout: None,
            chips: Vec::new(),
            on_submit: None,
        }
    }

    /// Highlight the card's border when the embedded input has focus.
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Enable or dim the round submit button. When `false`, clicks do not invoke `on_submit`.
    pub fn can_submit(mut self, can: bool) -> Self {
        self.can_submit = can;
        self
    }

    /// Small monospace readout shown above the card's top-left corner.
    pub fn left_readout(mut self, text: impl Into<SharedString>) -> Self {
        self.left_readout = Some(text.into());
        self
    }

    /// Small monospace readout shown above the card's top-right corner.
    pub fn right_readout(mut self, text: impl Into<SharedString>) -> Self {
        self.right_readout = Some(text.into());
        self
    }

    /// Append a [`ComposerChip`] to the card's footer row, left to right.
    pub fn chip(mut self, chip: ComposerChip) -> Self {
        self.chips.push(chip);
        self
    }

    /// Register the click handler for the submit button (invoked only when [`Self::can_submit`] is `true`).
    pub fn on_submit(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_submit = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ComposerCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);

        let horizon = div()
            .absolute()
            .top(relative(0.5))
            .left(px(-HORIZON_OVERHANG))
            .right(px(-HORIZON_OVERHANG))
            .h(px(1.0))
            .flex()
            .flex_row()
            // Each end fades in toward the card's edge; the card hides the gap between them.
            .child(
                div()
                    .flex_none()
                    .w(px(HORIZON_OVERHANG))
                    .h_full()
                    .bg(linear_gradient(
                        90.0,
                        linear_color_stop(theme.agent[2].alpha(0.0), 0.0),
                        linear_color_stop(theme.agent[2].alpha(0.95), 1.0),
                    )),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .w(px(HORIZON_OVERHANG))
                    .h_full()
                    .bg(linear_gradient(
                        90.0,
                        linear_color_stop(theme.agent[4].alpha(0.95), 0.0),
                        linear_color_stop(theme.agent[4].alpha(0.0), 1.0),
                    )),
            );

        let left_glow = div()
            .absolute()
            .bottom(px(-2.0))
            .left(px(48.0))
            .w(relative(0.45))
            .h(px(14.0))
            .rounded_full()
            .bg(theme.agent[4].alpha(0.01))
            .shadow(vec![
                BoxShadow::new(px(0.0), px(18.0), theme.agent[4].alpha(0.8))
                    .blur_radius(px(44.0))
                    .spread_radius(px(6.0)),
            ]);

        let right_glow = div()
            .absolute()
            .bottom(px(-2.0))
            .right(px(48.0))
            .w(relative(0.45))
            .h(px(14.0))
            .rounded_full()
            .bg(theme.agent[0].alpha(0.01))
            .shadow(vec![
                BoxShadow::new(px(0.0), px(18.0), theme.agent[0].alpha(0.8))
                    .blur_radius(px(44.0))
                    .spread_radius(px(6.0)),
            ]);

        let mut submit = div()
            .id("composer-submit")
            .debug_selector(|| "composer-submit".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(SUBMIT_SIZE))
            .rounded_full();

        if self.can_submit {
            let hover_bg = theme.accent_hover;
            submit = submit
                .bg(theme.accent)
                .cursor_pointer()
                .hover(move |style| style.bg(hover_bg))
                .child(
                    Icon::new(IconName::Send)
                        .size(IconSize::Sm)
                        .color(theme.on_accent),
                );
            if let Some(handler) = self.on_submit {
                submit = submit.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx);
                });
            }
        } else {
            submit = submit.bg(theme.raised).child(
                Icon::new(IconName::Send)
                    .size(IconSize::Sm)
                    .color(theme.text_subtle),
            );
        }

        let footer = div()
            .flex()
            .items_center()
            .gap(Theme::SPACE_3)
            .pt(Theme::SPACE_1)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(Theme::SPACE_3)
                    .min_w_0()
                    .children(self.chips),
            )
            .child(div().flex_1())
            .child(submit);

        let card = div()
            .relative()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_2)
            .w_full()
            .px(px(18.0))
            .py(px(14.0))
            .rounded(px(COMPOSER_RADIUS))
            .border_1()
            .border_color(if self.focused {
                theme.border_strong
            } else {
                theme.border
            })
            .bg(theme.surface.alpha(1.0))
            .shadow(vec![
                BoxShadow::new(px(0.0), px(6.0), theme.chrome.alpha(0.3))
                    .blur_radius(px(32.0))
                    .spread_radius(px(0.0)),
            ])
            .child(div().min_h(px(72.0)).w_full().child(self.input))
            .child(footer);

        let left_text = self.left_readout.unwrap_or_default();
        let right_text = self.right_readout.unwrap_or_default();
        let show_readouts = !left_text.is_empty() || !right_text.is_empty();

        let mut wrapper = div()
            .relative()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_1)
            .w_full()
            .max_w(px(COMPOSER_MAX_WIDTH))
            .mx_auto();

        if show_readouts {
            wrapper = wrapper.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(Theme::SPACE_2)
                    .font_family("Menlo")
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.meta)
                    .child(div().child(left_text))
                    .child(div().child(right_text)),
            );
        }

        wrapper.child(
            div()
                .relative()
                .w_full()
                .child(horizon)
                .child(left_glow)
                .child(right_glow)
                .child(card),
        )
    }
}

/// One chip in the [`ComposerCard`]'s footer row.
#[derive(IntoElement)]
pub struct ComposerChip {
    id: ElementId,
    label: SharedString,
    icon: Option<IconName>,
    tone: Option<Tone>,
    pill: bool,
    busy: bool,
    active: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl ComposerChip {
    /// Create a chip with a unique element `id` and `label`.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            tone: None,
            pill: false,
            busy: false,
            active: false,
            tooltip: None,
            on_click: None,
        }
    }

    /// Show `icon` before the label (replaced by a spinner when [`Self::busy`] is `true`).
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Tint the chip's label and icon with `tone`'s foreground colour (defaults to muted text).
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }

    /// Draw the chip as an outlined `rounded_full` pill instead of a flat text chip.
    pub fn pill(mut self, pill: bool) -> Self {
        self.pill = pill;
        self
    }

    /// Show an animated [`Spinner`] in place of the icon while `busy` is `true`.
    pub fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    /// Draw the chip with an active selection background.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Show `text` in a tooltip while the mouse rests on the chip.
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// Make the chip clickable with hover styling and a pointer cursor.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ComposerChip {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let fg = match self.tone {
            Some(tone) => theme.tone(tone).fg,
            None => theme.text_muted,
        };

        let leading: Option<AnyElement> = if self.busy {
            let spinner_tone = self.tone.unwrap_or(Tone::Neutral);
            let suffix = match spinner_tone {
                Tone::Neutral => "spinner-neutral",
                Tone::Accent => "spinner-accent",
                Tone::Success => "spinner-success",
                Tone::Warning => "spinner-warning",
                Tone::Danger => "spinner-danger",
            };
            Some(
                window
                    .use_keyed_state((self.id.clone(), suffix), cx, |_, _| {
                        Spinner::new().size(IconSize::Xs).tone(spinner_tone)
                    })
                    .into_any_element(),
            )
        } else {
            self.icon.map(|icon| {
                Icon::new(icon)
                    .size(IconSize::Xs)
                    .color(fg)
                    .into_any_element()
            })
        };

        let selector = self.id.to_string();
        let mut chip = div()
            .id(self.id)
            .debug_selector(move || selector)
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::SPACE_1)
            .h(px(CHIP_HEIGHT))
            .text_size(Theme::TEXT_SM)
            .text_color(fg)
            .whitespace_nowrap();

        if self.pill {
            chip = chip
                .rounded_full()
                .border_1()
                .border_color(theme.border)
                .px(Theme::SPACE_2 + px(2.0));
        } else {
            chip = chip.rounded(Theme::RADIUS_SM).px(Theme::SPACE_1 + px(2.0));
        }

        if self.active {
            chip = chip.bg(theme.selection);
        }

        chip = chip
            .children(leading)
            .when(!self.label.is_empty(), |chip| chip.child(self.label));

        if let Some(text) = self.tooltip {
            chip = chip.tooltip(move |_, cx| Tooltip::view(text.clone(), cx));
        }

        if let Some(handler) = self.on_click {
            let hover_bg = if self.active {
                theme.selection.blend(theme.hover)
            } else {
                theme.hover
            };
            chip = chip
                .cursor_pointer()
                .hover(move |style| style.bg(hover_bg))
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx);
                });
        }

        chip
    }
}
