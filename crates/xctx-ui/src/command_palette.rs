//! The ⌘K command palette: a top-anchored overlay with a query input and a fuzzy-ranked result list over
//! actions, spaces, agents and plugins.
//!
//! Matching, prefixes (`>` actions, `@` agents, `#` spaces) and recents ordering come from
//! [`xctx_nav::palette::search`]; this module draws the results and turns keys and clicks into
//! [`PaletteEvent`]s.

use std::ops::Range;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, HighlightStyle,
    KeyBinding, ScrollStrategy, SharedString, StyledText, Subscription, UniformListScrollHandle,
    Window, actions, div, prelude::*, px, uniform_list,
};
use xctx_nav::palette::{PaletteItem, PaletteKind, PaletteMatch, Recents, search};

use crate::icon::{Icon, IconName, IconSize};
use crate::overlay::{Overlay, OverlayPlacement};
use crate::prompt::{PromptEvent, PromptInput};
use crate::theme::Theme;

/// The GPUI key context the palette's bindings live in. It wraps the query input, so the bindings reach the
/// palette while the input has focus.
pub const PALETTE_CONTEXT: &str = "CommandPalette";

const PALETTE_WIDTH: f32 = 640.0;
const INPUT_HEIGHT: f32 = 44.0;
const ROW_HEIGHT: f32 = 32.0;
const MAX_VISIBLE_ROWS: usize = 10;
const PLACEHOLDER: &str = "Search actions, spaces and agents…";
const FOOTER: &str = "↑↓ navigate · ↵ run · esc close · > actions  @ agents  # spaces";

actions!(
    command_palette,
    [
        /// Move the selection to the previous result.
        SelectPrev,
        /// Move the selection to the next result.
        SelectNext,
        /// Run the selected result.
        Confirm,
        /// Close the palette without running anything.
        Dismiss,
    ]
);

/// What the user did with an open [`CommandPalette`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteEvent {
    /// The user ran the item with this [`PaletteItem::id`]. The palette has already closed itself.
    Run(String),
    /// The user dismissed the palette (Escape or a click on the scrim). The palette has already closed.
    Dismissed,
}

/// The command palette view. Create it once with `cx.new(|cx| CommandPalette::new(window, cx))`, render it as
/// a child of the window root (it draws nothing while closed and floats above everything while open), and call
/// [`CommandPalette::open`] with the current items on ⌘K.
pub struct CommandPalette {
    input: Entity<PromptInput>,
    items: Vec<PaletteItem>,
    matches: Vec<PaletteMatch>,
    selected: usize,
    query: String,
    recents: Recents,
    open: bool,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PaletteEvent> for CommandPalette {}

impl Focusable for CommandPalette {
    /// The query input's handle: focusing the palette puts the caret in its input.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl CommandPalette {
    /// A closed palette with no items and empty recents.
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            PromptInput::new(cx)
                .placeholder(PLACEHOLDER)
                .submit_blank(true)
                .borderless(true)
        });
        let subscriptions = vec![
            cx.observe(&input, |this, input, cx| {
                let text = input.read(cx).value().to_owned();
                if text != this.query {
                    this.query = text;
                    this.refresh();
                    cx.notify();
                }
            }),
            cx.subscribe(&input, |this, _, event: &PromptEvent, cx| match event {
                PromptEvent::Submit(_) => this.confirm_selected(cx),
                PromptEvent::Cancel => this.dismiss(cx),
            }),
        ];
        Self {
            input,
            items: Vec::new(),
            matches: Vec::new(),
            selected: 0,
            query: String::new(),
            recents: Recents::default(),
            open: false,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Show the palette over `items`: clears the query, selects the first result (recents first) and focuses
    /// the input. Calling it while open replaces the items the same way.
    pub fn open(&mut self, items: Vec<PaletteItem>, window: &mut Window, cx: &mut Context<Self>) {
        self.items = items;
        self.query.clear();
        self.input.update(cx, |input, cx| input.clear(cx));
        self.refresh();
        self.open = true;
        let handle = self.input.focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Hide the palette without emitting an event. Focus is left to the caller to restore.
    pub fn close(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.hide(cx);
    }

    /// Whether the palette is showing.
    pub fn is_open(&self) -> bool {
        self.open
    }

    fn hide(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            cx.notify();
        }
    }

    fn refresh(&mut self) {
        self.matches = search(&self.items, &self.query, &self.recents);
        self.selected = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.matches.len() && index != self.selected {
            self.selected = index;
            cx.notify();
        }
    }

    fn move_selection(&mut self, forward: bool, cx: &mut Context<Self>) {
        let len = self.matches.len();
        if len == 0 {
            return;
        }
        self.selected = if forward {
            (self.selected + 1) % len
        } else {
            (self.selected + len - 1) % len
        };
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn run(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        let Some(item) = self
            .matches
            .get(index)
            .and_then(|m| self.items.get(m.index))
        else {
            return;
        };
        let id = item.id.clone();
        self.recents.record(id.clone());
        self.hide(cx);
        cx.emit(PaletteEvent::Run(id));
    }

    fn confirm_selected(&mut self, cx: &mut Context<Self>) {
        self.run(self.selected, cx);
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.hide(cx);
            cx.emit(PaletteEvent::Dismissed);
        }
    }

    fn render_row(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some((hit, item)) = self
            .matches
            .get(index)
            .and_then(|hit| Some((hit, self.items.get(hit.index)?)))
        else {
            return div().into_any_element();
        };
        let selected = index == self.selected;
        let icon = match item.kind {
            PaletteKind::Action => IconName::Command,
            PaletteKind::Space => IconName::Folder,
            PaletteKind::Agent => IconName::Writer,
            PaletteKind::Plugin => IconName::Plugin,
        };
        let highlight = HighlightStyle {
            color: Some(theme.accent),
            font_weight: Some(FontWeight::SEMIBOLD),
            ..HighlightStyle::default()
        };
        let title = StyledText::new(item.title.clone()).with_highlights(
            char_ranges(&item.title, &hit.title_indices)
                .into_iter()
                .map(|range| (range, highlight)),
        );
        let keycaps = item.key_chord.as_ref().map(|chord| {
            div()
                .flex()
                .flex_none()
                .gap(Theme::SPACE_0_5)
                .children(chord.chars().map(|key| keycap(key.to_string(), theme)))
        });
        div()
            .id(("palette-row", index))
            .w_full()
            .h(px(ROW_HEIGHT))
            .px(Theme::SPACE_2)
            .flex()
            .items_center()
            .gap(Theme::SPACE_2)
            .rounded(Theme::RADIUS_MD)
            .cursor_pointer()
            .when(selected, |row| row.bg(theme.selection))
            .text_color(theme.text)
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.select(index, cx);
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.run(index, cx)))
            .child(Icon::new(icon).size(IconSize::Sm).color(if selected {
                theme.text
            } else {
                theme.text_muted
            }))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_baseline()
                    .gap(Theme::SPACE_2)
                    .overflow_hidden()
                    .child(div().flex_none().text_size(Theme::TEXT_MD).child(title))
                    .children(item.subtitle.clone().map(|subtitle| {
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(Theme::TEXT_SM)
                            .text_color(theme.text_subtle)
                            .child(subtitle)
                    })),
            )
            .children(keycaps)
            .into_any_element()
    }
}

/// Byte ranges in `text` for the characters at `indices` (char indices, ascending), merging adjacent ones.
fn char_ranges(text: &str, indices: &[usize]) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut wanted = indices.iter().copied().peekable();
    for (char_index, (byte, ch)) in text.char_indices().enumerate() {
        if wanted.peek().is_none() {
            break;
        }
        if wanted.peek() == Some(&char_index) {
            wanted.next();
            let end = byte + ch.len_utf8();
            match ranges.last_mut() {
                Some(last) if last.end == byte => last.end = end,
                _ => ranges.push(byte..end),
            }
        }
    }
    ranges
}

fn keycap(label: impl Into<SharedString>, theme: &Theme) -> gpui::Div {
    div()
        .min_w(px(18.0))
        .h(px(18.0))
        .px(Theme::SPACE_1)
        .flex()
        .items_center()
        .justify_center()
        .rounded(Theme::RADIUS_SM)
        .bg(theme.raised)
        .border_1()
        .border_color(theme.border)
        .text_size(Theme::TEXT_XS)
        .text_color(theme.meta)
        .child(label.into())
}

impl Render for CommandPalette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        let theme = Theme::of(cx);
        let count = self.matches.len();

        let input_row = div()
            .h(px(INPUT_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap(Theme::SPACE_2)
            .px(Theme::SPACE_3)
            .border_b_1()
            .border_color(theme.border)
            .text_size(Theme::TEXT_LG)
            .child(
                Icon::new(IconName::Search)
                    .size(IconSize::Md)
                    .color(theme.text_subtle),
            )
            .child(div().flex_1().min_w_0().child(self.input.clone()));

        let results = if count == 0 {
            div()
                .h(px(ROW_HEIGHT * 2.0))
                .flex()
                .items_center()
                .justify_center()
                .text_size(Theme::TEXT_MD)
                .text_color(theme.text_subtle)
                .child("No matches")
                .into_any_element()
        } else {
            let visible = count.min(MAX_VISIBLE_ROWS) as f32;
            div()
                .py(Theme::SPACE_1)
                .px(Theme::SPACE_1)
                .child(
                    uniform_list(
                        "palette-results",
                        count,
                        cx.processor(move |this, range: Range<usize>, _, cx| {
                            range
                                .map(|index| this.render_row(index, &theme, cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .w_full()
                    .h(px(ROW_HEIGHT * visible)),
                )
                .into_any_element()
        };

        let footer = div()
            .flex_none()
            .h(px(28.0))
            .px(Theme::SPACE_3)
            .flex()
            .items_center()
            .border_t_1()
            .border_color(theme.border)
            .text_size(Theme::TEXT_XS)
            .text_color(theme.text_subtle)
            .child(FOOTER);

        let content = div()
            .key_context(PALETTE_CONTEXT)
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(false, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(true, cx)))
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.confirm_selected(cx)))
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.dismiss(cx)))
            .w_full()
            .flex()
            .flex_col()
            .shadow_lg()
            .child(input_row)
            .child(results)
            .child(footer);

        let view = cx.entity().downgrade();
        Overlay::new("command-palette", content)
            .placement(OverlayPlacement::TopCenter)
            .width(px(PALETTE_WIDTH))
            .max_width(px(PALETTE_WIDTH))
            .on_close(move |_, cx| {
                view.update(cx, |this, cx| this.dismiss(cx)).ok();
            })
            .into_any_element()
    }
}

/// Bind the palette's own keys in [`PALETTE_CONTEXT`]: up/down and ctrl-p/ctrl-n move the selection, enter runs
/// the selected item, escape dismisses. Call once at startup next to the app's other bindings.
pub fn bind_palette_keys(cx: &mut App) {
    let context = Some(PALETTE_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, context),
        KeyBinding::new("ctrl-p", SelectPrev, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("ctrl-n", SelectNext, context),
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("escape", Dismiss, context),
    ]);
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::TestAppContext;

    use super::*;
    use crate::motion::MotionSettings;

    #[test]
    fn char_ranges_merge_adjacent_and_respect_utf8() {
        assert_eq!(char_ranges("hello", &[0, 1, 3]), vec![0..2, 3..4]);
        assert_eq!(char_ranges("é-x", &[0, 2]), vec![0..2, 3..4]);
        assert!(char_ranges("abc", &[]).is_empty());
    }

    #[gpui::test]
    fn typing_moving_and_enter_runs_the_selected_item(cx: &mut TestAppContext) {
        cx.update(|cx| {
            MotionSettings::set_reduced(cx, true);
            bind_palette_keys(cx);
        });
        let (palette, cx) = cx.add_window_view(CommandPalette::new);
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        cx.update(|_, cx| {
            cx.subscribe(&palette, move |_, event: &PaletteEvent, _| {
                sink.borrow_mut().push(event.clone());
            })
            .detach();
        });
        let items = vec![
            PaletteItem::new("split.right", PaletteKind::Action, "Split Right"),
            PaletteItem::new("split.down", PaletteKind::Action, "Split Down"),
            PaletteItem::new("space.home", PaletteKind::Space, "Home"),
        ];
        palette.update_in(cx, |palette, window, cx| palette.open(items, window, cx));
        cx.run_until_parked();

        cx.simulate_input("split");
        let matched: Vec<String> = palette.read_with(cx, |p, _| {
            p.matches
                .iter()
                .map(|m| p.items[m.index].id.clone())
                .collect()
        });
        assert_eq!(matched.len(), 2, "query should narrow to the split actions");

        cx.simulate_keystrokes("down enter");
        assert_eq!(
            *events.borrow(),
            vec![PaletteEvent::Run(matched[1].clone())]
        );
        assert!(!palette.read_with(cx, |p, _| p.is_open()));
        assert_eq!(
            palette.read_with(cx, |p, _| p.recents.rank(&matched[1])),
            Some(0)
        );
    }
}
