//! Scrim + anchored/centered [`Overlay`] component with focus-trap hook, Escape/backdrop
//! dismissal, and 140ms opacity + offset entrance motion.
//!
//! Input is never delayed during the entrance animation (Performance Rule 5): Escape, backdrop
//! clicks, and interactive children inside the overlay respond from frame 0.

use std::rc::Rc;

use gpui::{
    Animation, AnimationExt, AnyElement, App, ElementId, FocusHandle, KeyDownEvent, MouseButton,
    Pixels, SharedString, Window, black, deferred, div, prelude::*, px,
};

use crate::motion::{
    MotionSettings, OVERLAY_DURATION, OVERLAY_OFFSET_PX, ease_out_cubic, offset_px, opacity_for,
};
use crate::theme::Theme;

type CloseHandler = Rc<dyn Fn(&mut Window, &mut App)>;
type FocusTrapHandler = Rc<dyn Fn(&KeyDownEvent, &mut Window, &mut App)>;

/// Where an [`Overlay`] positions its surface within the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OverlayPlacement {
    /// Centred horizontally and vertically (dialogs, review sheets).
    #[default]
    Center,
    /// Centred horizontally near the top of the viewport (command palette).
    TopCenter,
    /// Anchored flush to the right edge at full height (Inbox overlay, narrow Inspector).
    Right,
    /// Anchored flush to the bottom edge at full width (Parent Agent dock).
    Bottom,
}

impl OverlayPlacement {
    /// Default initial pixel slide offset for this placement's entrance motion.
    pub fn entrance_offset_px(self) -> f32 {
        match self {
            Self::Center => OVERLAY_OFFSET_PX,
            Self::TopCenter => -OVERLAY_OFFSET_PX,
            Self::Right => OVERLAY_OFFSET_PX * 1.5,
            Self::Bottom => OVERLAY_OFFSET_PX * 1.5,
        }
    }
}

/// A modal or anchored overlay drawn above the ambient workspace with a translucent scrim.
///
/// The overlay handles:
/// - **Scrim backdrop** that dims underlying panes and optionally closes on click.
/// - **Focus tracking and trapping** via [`Overlay::focus_handle`] and [`Overlay::on_focus_trap`],
///   keeping `Tab` inside the overlay so focus never leaks behind the scrim.
/// - **Escape dismissal** (when focus is within the overlay and [`Overlay::close_on_escape`] is
///   enabled), stopping propagation so underlying views do not receive the key.
/// - **140ms opacity + offset entrance** via [`OVERLAY_DURATION`] and [`ease_out_cubic`], snapped
///   immediately when [`MotionSettings::is_reduced`] is true.
#[derive(IntoElement)]
pub struct Overlay {
    id: ElementId,
    placement: OverlayPlacement,
    content: AnyElement,
    focus_handle: Option<FocusHandle>,
    key_context: Option<SharedString>,
    on_close: Option<CloseHandler>,
    on_focus_trap: Option<FocusTrapHandler>,
    close_on_backdrop: bool,
    close_on_escape: bool,
    show_scrim: bool,
    use_deferred: bool,
    deferred_priority: usize,
    width: Option<Pixels>,
    max_width: Option<Pixels>,
    backdrop_selector: Option<String>,
    surface_selector: Option<String>,
}

impl Overlay {
    /// Create a centred overlay with `id` wrapping `content`.
    pub fn new(id: impl Into<ElementId>, content: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            placement: OverlayPlacement::Center,
            content: content.into_any_element(),
            focus_handle: None,
            key_context: None,
            on_close: None,
            on_focus_trap: None,
            close_on_backdrop: true,
            close_on_escape: true,
            show_scrim: true,
            use_deferred: true,
            deferred_priority: 10,
            width: None,
            max_width: None,
            backdrop_selector: None,
            surface_selector: None,
        }
    }

    /// Position the overlay surface (centred, top-centred, right-anchored or bottom-anchored).
    pub fn placement(mut self, placement: OverlayPlacement) -> Self {
        self.placement = placement;
        self
    }

    /// Track focus on the overlay container so keyboard events (including Escape and Tab trapping)
    /// route to the overlay while it is open.
    pub fn focus_handle(mut self, handle: FocusHandle) -> Self {
        self.focus_handle = Some(handle);
        self
    }

    /// Attach a GPUI key context string to the overlay container.
    pub fn key_context(mut self, context: impl Into<SharedString>) -> Self {
        self.key_context = Some(context.into());
        self
    }

    /// Callback invoked when the user presses Escape or clicks the scrim backdrop.
    pub fn on_close(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(handler));
        self
    }

    /// Custom hook called when `Tab` (with or without `Shift`) is pressed inside the overlay so
    /// the caller can cycle focus between interactive descendants without escaping behind the
    /// scrim.
    pub fn on_focus_trap(
        mut self,
        handler: impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_focus_trap = Some(Rc::new(handler));
        self
    }

    /// Whether clicking the scrim backdrop triggers [`Overlay::on_close`] (default `true`).
    pub fn close_on_backdrop(mut self, enabled: bool) -> Self {
        self.close_on_backdrop = enabled;
        self
    }

    /// Whether pressing unmodified Escape triggers [`Overlay::on_close`] (default `true`).
    pub fn close_on_escape(mut self, enabled: bool) -> Self {
        self.close_on_escape = enabled;
        self
    }

    /// Whether to draw the dimmed scrim behind the overlay surface (default `true`).
    pub fn show_scrim(mut self, show: bool) -> Self {
        self.show_scrim = show;
        self
    }

    /// Whether to wrap the overlay in `gpui::deferred` so it always paints above sibling elements
    /// (default `true`).
    pub fn deferred(mut self, enabled: bool) -> Self {
        self.use_deferred = enabled;
        self
    }

    /// Set the `gpui::deferred` stacking priority (default `10`). Higher values paint on top.
    pub fn priority(mut self, priority: usize) -> Self {
        self.deferred_priority = priority;
        self
    }

    /// Set an explicit surface width.
    pub fn width(mut self, width: Pixels) -> Self {
        self.width = Some(width);
        self
    }

    /// Set a maximum surface width.
    pub fn max_width(mut self, max_width: Pixels) -> Self {
        self.max_width = Some(max_width);
        self
    }

    /// Name the scrim backdrop for GPUI's test `debug_bounds`.
    pub fn backdrop_selector(mut self, selector: impl Into<String>) -> Self {
        self.backdrop_selector = Some(selector.into());
        self
    }

    /// Name the overlay surface for GPUI's test `debug_bounds`.
    pub fn surface_selector(mut self, selector: impl Into<String>) -> Self {
        self.surface_selector = Some(selector.into());
        self
    }

    /// Returns whether `event` is an unmodified Escape keystroke that should dismiss an overlay.
    pub fn is_escape_key(event: &KeyDownEvent) -> bool {
        event.keystroke.key == "escape" && !event.keystroke.modifiers.modified()
    }

    /// Returns whether `event` is a `Tab` (or `Shift+Tab`) keystroke subject to focus trapping.
    pub fn is_tab_key(event: &KeyDownEvent) -> bool {
        event.keystroke.key == "tab"
            && !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.alt
            && !event.keystroke.modifiers.platform
            && !event.keystroke.modifiers.function
    }
}

impl RenderOnce for Overlay {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let reduced = MotionSettings::is_reduced(cx);
        let placement = self.placement;

        // Post-merge with `m6-theme`, swap this colour to `theme.scrim` and the surface
        // background/border to `theme.overlay` / `theme.border_strong`.
        let scrim_color = black().alpha(0.50);

        let mut surface = div()
            .id((self.id.clone(), "surface"))
            .relative()
            .flex()
            .flex_col()
            .bg(theme.elevated)
            .border_1()
            .border_color(theme.border)
            .text_color(theme.text)
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .child(self.content);

        surface = match placement {
            OverlayPlacement::Center | OverlayPlacement::TopCenter => {
                surface.rounded(Theme::RADIUS_LG)
            }
            OverlayPlacement::Right => surface.h_full().border_r_0().border_t_0().border_b_0(),
            OverlayPlacement::Bottom => surface.w_full().border_l_0().border_r_0().border_b_0(),
        };

        if let Some(width) = self.width {
            surface = surface.w(width);
        }
        if let Some(max_width) = self.max_width {
            surface = surface.max_w(max_width);
        }
        if let Some(selector) = self.surface_selector {
            surface = surface.debug_selector(|| selector);
        }

        let initial_offset = placement.entrance_offset_px();
        let animated_surface: AnyElement = if reduced {
            surface.into_any_element()
        } else {
            surface
                .with_animation(
                    (self.id.clone(), "surface-motion"),
                    Animation::new(OVERLAY_DURATION).with_easing(ease_out_cubic),
                    move |el, delta| {
                        let offset = offset_px(initial_offset, delta);
                        let el = el.opacity(opacity_for(delta));
                        match placement {
                            OverlayPlacement::Center
                            | OverlayPlacement::TopCenter
                            | OverlayPlacement::Bottom => el.top(offset),
                            OverlayPlacement::Right => el.left(offset),
                        }
                    },
                )
                .into_any_element()
        };

        let mut backdrop = div()
            .id((self.id.clone(), "backdrop"))
            .absolute()
            .inset_0()
            .flex();

        backdrop = match placement {
            OverlayPlacement::Center => backdrop.items_center().justify_center().p(Theme::SPACE_4),
            OverlayPlacement::TopCenter => backdrop
                .items_start()
                .justify_center()
                .pt(px(72.0))
                .px(Theme::SPACE_4),
            OverlayPlacement::Right => backdrop.items_stretch().justify_end(),
            OverlayPlacement::Bottom => backdrop.items_end().justify_center(),
        };

        if let Some(selector) = self.backdrop_selector {
            backdrop = backdrop.debug_selector(|| selector);
        }

        if let Some(key_context) = self.key_context {
            backdrop = backdrop.key_context(key_context.as_ref());
        }

        let focus_for_trap = self.focus_handle.clone();
        if let Some(handle) = &self.focus_handle {
            backdrop = backdrop.track_focus(handle);
        }

        if self.close_on_backdrop
            && let Some(on_close) = self.on_close.clone()
        {
            backdrop = backdrop.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                on_close(window, cx);
            });
        } else {
            backdrop = backdrop.on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            });
        }

        let close_on_escape = self.close_on_escape;
        let on_close_key = self.on_close;
        let on_focus_trap = self.on_focus_trap;
        backdrop = backdrop.on_key_down(move |event, window, cx| {
            if close_on_escape && Self::is_escape_key(event) {
                cx.stop_propagation();
                if let Some(handler) = &on_close_key {
                    handler(window, cx);
                }
                return;
            }
            if Self::is_tab_key(event) {
                if let Some(trap) = &on_focus_trap {
                    cx.stop_propagation();
                    trap(event, window, cx);
                } else if let Some(handle) = &focus_for_trap {
                    cx.stop_propagation();
                    if !handle.contains_focused(window, cx) {
                        window.focus(handle, cx);
                    }
                }
            }
        });

        let scrim_layer = self.show_scrim.then(|| {
            let scrim = div().absolute().inset_0().bg(scrim_color);
            if reduced {
                scrim.into_any_element()
            } else {
                scrim
                    .with_animation(
                        (self.id.clone(), "scrim-motion"),
                        Animation::new(OVERLAY_DURATION).with_easing(ease_out_cubic),
                        |el, delta| el.opacity(opacity_for(delta)),
                    )
                    .into_any_element()
            }
        });

        let root = backdrop.children(scrim_layer).child(animated_surface);

        if self.use_deferred {
            deferred(root)
                .with_priority(self.deferred_priority)
                .into_any_element()
        } else {
            root.into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Keystroke, Modifiers};

    use super::*;

    fn key_event(key: &str, modifiers: Modifiers) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke {
                modifiers,
                key: key.to_owned(),
                key_char: None,
            },
            is_held: false,
            prefer_character_input: false,
        }
    }

    #[test]
    fn escape_and_tab_key_classification() {
        let none = Modifiers::default();
        let shift = Modifiers {
            shift: true,
            ..none
        };
        let cmd = Modifiers {
            platform: true,
            ..none
        };

        assert!(Overlay::is_escape_key(&key_event("escape", none)));
        assert!(!Overlay::is_escape_key(&key_event("escape", shift)));
        assert!(!Overlay::is_escape_key(&key_event("escape", cmd)));
        assert!(!Overlay::is_escape_key(&key_event("enter", none)));

        assert!(Overlay::is_tab_key(&key_event("tab", none)));
        assert!(Overlay::is_tab_key(&key_event("tab", shift)));
        assert!(!Overlay::is_tab_key(&key_event("tab", cmd)));
    }

    #[test]
    fn placement_offsets_have_expected_directions() {
        assert!(OverlayPlacement::Center.entrance_offset_px() > 0.0);
        assert!(OverlayPlacement::TopCenter.entrance_offset_px() < 0.0);
        assert!(OverlayPlacement::Right.entrance_offset_px() > 0.0);
        assert!(OverlayPlacement::Bottom.entrance_offset_px() > 0.0);
    }
}
