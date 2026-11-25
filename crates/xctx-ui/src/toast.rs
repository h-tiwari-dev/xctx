//! Non-blocking [`Toast`] notifications and the isolated [`ToastStack`] GPUI entity.
//!
//! Per Performance Rule 4 in `docs/m6.md`, [`ToastStack`] is its own GPUI entity so pushing,
//! dismissing, or expiring toasts notifies only the stack and never re-renders the workspace.

use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, App, Context, FontWeight, SharedString, Window, div, prelude::*, px,
};

use crate::components::{Button, ButtonSize, ButtonVariant};
use crate::icon::{Icon, IconName, IconSize};
use crate::motion::{
    MotionSettings, OVERLAY_DURATION, OVERLAY_OFFSET_PX, ease_out_cubic, offset_px, opacity_for,
};
use crate::theme::{Theme, Tone};

/// Default maximum number of toasts rendered simultaneously.
pub const DEFAULT_MAX_VISIBLE_TOASTS: usize = 4;

/// Default maximum number of toasts kept in the bounded queue.
pub const DEFAULT_MAX_QUEUED_TOASTS: usize = 16;

/// Default auto-dismiss duration for transient informational toasts.
pub const DEFAULT_TOAST_TTL: Duration = Duration::from_secs(5);

type ActionHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Unique identifier assigned to each toast pushed into a [`ToastQueue`] or [`ToastStack`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ToastId(pub u64);

/// An interactive action button displayed on a [`Toast`], such as Approve or Open.
#[derive(Clone)]
pub struct ToastAction {
    /// Button text.
    pub label: SharedString,
    /// Visual prominence of the action button.
    pub variant: ButtonVariant,
    /// Whether clicking this action also dismisses the toast (default `true`).
    pub dismiss_on_click: bool,
    on_click: ActionHandler,
}

impl ToastAction {
    /// Create a secondary toast action that dismisses the toast when clicked.
    pub fn new(
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            variant: ButtonVariant::Secondary,
            dismiss_on_click: true,
            on_click: Rc::new(on_click),
        }
    }

    /// Set the button variant (e.g. [`ButtonVariant::Primary`] or [`ButtonVariant::Danger`]).
    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    /// Control whether clicking this action dismisses the toast.
    pub fn dismiss_on_click(mut self, dismiss: bool) -> Self {
        self.dismiss_on_click = dismiss;
        self
    }
}

/// Content and behaviour of a single notification toast.
#[derive(Clone)]
pub struct Toast {
    /// Short headline shown in bold.
    pub title: SharedString,
    /// Optional supporting detail shown beneath the title.
    pub message: Option<SharedString>,
    /// Semantic tone (`Neutral`, `Accent`, `Success`, `Warning`, `Danger`).
    pub tone: Tone,
    /// Optional action buttons (e.g. Approve / Open).
    pub actions: Vec<ToastAction>,
    /// Whether the user can dismiss the toast via its close button (default `true`).
    pub dismissible: bool,
    /// Optional auto-dismiss timeout. `None` keeps the toast until acted on or dismissed.
    pub ttl: Option<Duration>,
}

impl Toast {
    /// Create a dismissible [`Tone::Neutral`] toast with no auto-dismiss timeout.
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            message: None,
            tone: Tone::Neutral,
            actions: Vec::new(),
            dismissible: true,
            ttl: None,
        }
    }

    /// Create an [`Tone::Accent`] toast with [`DEFAULT_TOAST_TTL`].
    pub fn info(title: impl Into<SharedString>) -> Self {
        Self::new(title)
            .tone(Tone::Accent)
            .ttl(Some(DEFAULT_TOAST_TTL))
    }

    /// Create a [`Tone::Success`] toast with [`DEFAULT_TOAST_TTL`].
    pub fn success(title: impl Into<SharedString>) -> Self {
        Self::new(title)
            .tone(Tone::Success)
            .ttl(Some(DEFAULT_TOAST_TTL))
    }

    /// Create a [`Tone::Warning`] toast (sticky by default so gates are not missed).
    pub fn warning(title: impl Into<SharedString>) -> Self {
        Self::new(title).tone(Tone::Warning)
    }

    /// Create a [`Tone::Danger`] toast (sticky by default).
    pub fn danger(title: impl Into<SharedString>) -> Self {
        Self::new(title).tone(Tone::Danger)
    }

    /// Set the supporting detail message.
    pub fn message(mut self, message: impl Into<SharedString>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Set the semantic tone.
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    /// Append an interactive action button.
    pub fn action(mut self, action: ToastAction) -> Self {
        self.actions.push(action);
        self
    }

    /// Set whether the close button is shown.
    pub fn dismissible(mut self, dismissible: bool) -> Self {
        self.dismissible = dismissible;
        self
    }

    /// Set an optional auto-dismiss duration (`None` = sticky).
    pub fn ttl(mut self, ttl: Option<Duration>) -> Self {
        self.ttl = ttl;
        self
    }
}

/// A queued [`Toast`] together with its assigned [`ToastId`].
#[derive(Clone)]
pub struct ToastEntry {
    /// Identifier assigned when pushed.
    pub id: ToastId,
    /// The toast payload.
    pub toast: Toast,
}

/// Pure, UI-free bounded queue backing [`ToastStack`].
///
/// Up to `max_queued` entries are retained; when full, the oldest non-[`Tone::Danger`] toast is
/// evicted first (falling back to the oldest entry if all queued toasts are danger).
pub struct ToastQueue {
    next_id: u64,
    max_visible: usize,
    max_queued: usize,
    entries: VecDeque<ToastEntry>,
}

impl Default for ToastQueue {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_VISIBLE_TOASTS, DEFAULT_MAX_QUEUED_TOASTS)
    }
}

impl ToastQueue {
    /// Create a bounded queue with at least 1 visible and 1 queued slot.
    pub fn new(max_visible: usize, max_queued: usize) -> Self {
        let max_visible = max_visible.max(1);
        let max_queued = max_queued.max(max_visible);
        Self {
            next_id: 1,
            max_visible,
            max_queued,
            entries: VecDeque::with_capacity(max_queued),
        }
    }

    /// Enqueue `toast`, evicting an older entry if at capacity, and return its [`ToastId`].
    pub fn push(&mut self, toast: Toast) -> ToastId {
        if self.entries.len() >= self.max_queued {
            if let Some(idx) = self
                .entries
                .iter()
                .position(|entry| entry.toast.tone != Tone::Danger)
            {
                self.entries.remove(idx);
            } else {
                self.entries.pop_front();
            }
        }
        let id = ToastId(self.next_id);
        self.next_id += 1;
        self.entries.push_back(ToastEntry { id, toast });
        id
    }

    /// Remove the toast with `id`. Returns `true` if one was removed.
    pub fn dismiss(&mut self, id: ToastId) -> bool {
        if let Some(idx) = self.entries.iter().position(|entry| entry.id == id) {
            self.entries.remove(idx);
            true
        } else {
            false
        }
    }

    /// Remove every queued toast.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Total number of queued toasts.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no toasts are queued.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Maximum toasts displayed at once.
    pub fn max_visible(&self) -> usize {
        self.max_visible
    }

    /// Maximum toasts retained in the queue.
    pub fn max_queued(&self) -> usize {
        self.max_queued
    }

    /// Iterate over the currently visible toasts (at most [`ToastQueue::max_visible`]).
    pub fn visible(&self) -> impl Iterator<Item = &ToastEntry> {
        self.entries.iter().take(self.max_visible)
    }

    /// Number of queued toasts waiting behind the visible window.
    pub fn overflow_count(&self) -> usize {
        self.entries.len().saturating_sub(self.max_visible)
    }
}

/// The icon paired with a toast's semantic [`Tone`].
pub fn toast_icon(tone: Tone) -> IconName {
    match tone {
        Tone::Neutral => IconName::Inbox,
        Tone::Accent => IconName::Inbox,
        Tone::Success => IconName::Check,
        Tone::Warning => IconName::Warning,
        Tone::Danger => IconName::Error,
    }
}

/// Isolated GPUI entity rendering a vertical stack of [`Toast`] notifications.
///
/// Mount an `Entity<ToastStack>` inside a positioned corner container (e.g. bottom-right) so
/// pushing or dismissing toasts repaints only the toast stack.
pub struct ToastStack {
    queue: ToastQueue,
    width: gpui::Pixels,
}

impl Default for ToastStack {
    fn default() -> Self {
        Self::new()
    }
}

impl ToastStack {
    /// Create a toast stack with [`DEFAULT_MAX_VISIBLE_TOASTS`] and [`DEFAULT_MAX_QUEUED_TOASTS`].
    pub fn new() -> Self {
        Self {
            queue: ToastQueue::default(),
            width: px(340.0),
        }
    }

    /// Create a toast stack with custom visible and total queue bounds.
    pub fn with_limits(max_visible: usize, max_queued: usize) -> Self {
        Self {
            queue: ToastQueue::new(max_visible, max_queued),
            width: px(340.0),
        }
    }

    /// Set the toast card width (default `340px`).
    pub fn width(mut self, width: gpui::Pixels) -> Self {
        self.width = width;
        self
    }

    /// Push a toast into the stack, scheduling auto-dismissal if `toast.ttl` is `Some`.
    pub fn push(&mut self, toast: Toast, cx: &mut Context<Self>) -> ToastId {
        let ttl = toast.ttl;
        let id = self.queue.push(toast);
        cx.notify();

        if let Some(duration) = ttl {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(duration).await;
                let _ = this.update(cx, |stack, cx| {
                    stack.dismiss(id, cx);
                });
            })
            .detach();
        }

        id
    }

    /// Dismiss a toast by [`ToastId`]. Returns `true` if found and removed.
    pub fn dismiss(&mut self, id: ToastId, cx: &mut Context<Self>) -> bool {
        if self.queue.dismiss(id) {
            cx.notify();
            true
        } else {
            false
        }
    }

    /// Dismiss all toasts.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if !self.queue.is_empty() {
            self.queue.clear();
            cx.notify();
        }
    }

    /// Total queued toasts.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether the stack has no active toasts.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Read-only access to the underlying [`ToastQueue`].
    pub fn queue(&self) -> &ToastQueue {
        &self.queue
    }
}

impl Render for ToastStack {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.queue.is_empty() {
            return div().into_any_element();
        }

        let theme = Theme::of(cx);
        let reduced = MotionSettings::is_reduced(cx);
        let width = self.width;
        let overflow = self.queue.overflow_count();

        let cards: Vec<_> = self
            .queue
            .visible()
            .map(|entry| {
                let id = entry.id;
                let toast = &entry.toast;
                let tone_colors = theme.tone(toast.tone);
                let accent_bar = match toast.tone {
                    Tone::Neutral => theme.border,
                    _ => tone_colors.fg,
                };

                let action_buttons: Vec<_> = toast
                    .actions
                    .iter()
                    .enumerate()
                    .map(|(idx, action)| {
                        let handler = action.on_click.clone();
                        let dismiss_after = action.dismiss_on_click;
                        let entity = cx.entity().downgrade();
                        Button::new(
                            SharedString::from(format!("toast-{}-action-{idx}", id.0)),
                            action.label.clone(),
                        )
                        .variant(action.variant)
                        .size(ButtonSize::Compact)
                        .selector(format!("toast-{}-action-{idx}", id.0))
                        .on_click(move |_, window, cx| {
                            handler(window, cx);
                            if dismiss_after {
                                let _ = entity.update(cx, |stack, cx| {
                                    stack.dismiss(id, cx);
                                });
                            }
                        })
                    })
                    .collect();

                let close_button = toast.dismissible.then(|| {
                    div()
                        .id(SharedString::from(format!("toast-{}-close", id.0)))
                        .debug_selector(move || format!("toast-{}-close", id.0))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(20.0))
                        .rounded(Theme::RADIUS_SM)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.raised))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.dismiss(id, cx);
                        }))
                        .child(Icon::new(IconName::Close).size(IconSize::Xs))
                });

                let body = div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .flex_col()
                    .gap(Theme::SPACE_1)
                    .child(
                        div()
                            .text_size(Theme::TEXT_SM)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(toast.title.clone()),
                    )
                    .children(toast.message.clone().map(|msg| {
                        div()
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_muted)
                            .child(msg)
                    }))
                    .when(!action_buttons.is_empty(), |el| {
                        el.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(Theme::SPACE_1)
                                .pt(Theme::SPACE_1)
                                .children(action_buttons),
                        )
                    });

                let card = div()
                    .id(SharedString::from(format!("toast-card-{}", id.0)))
                    .debug_selector(move || format!("toast-card-{}", id.0))
                    .relative()
                    .flex()
                    .w(width)
                    .rounded(Theme::RADIUS_MD)
                    .border_1()
                    .border_color(tone_colors.border)
                    .bg(theme.overlay)
                    .overflow_hidden()
                    .child(div().w(px(3.0)).flex_none().bg(accent_bar))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_start()
                            .gap(Theme::SPACE_2)
                            .p(Theme::SPACE_2)
                            .child(
                                div().pt(px(1.0)).child(
                                    Icon::new(toast_icon(toast.tone))
                                        .size(IconSize::Sm)
                                        .tone(toast.tone),
                                ),
                            )
                            .child(body)
                            .children(close_button),
                    );

                if reduced {
                    card.into_any_element()
                } else {
                    card.with_animation(
                        SharedString::from(format!("toast-enter-{}", id.0)),
                        Animation::new(OVERLAY_DURATION).with_easing(ease_out_cubic),
                        |el, delta| {
                            el.opacity(opacity_for(delta))
                                .left(offset_px(OVERLAY_OFFSET_PX, delta))
                        },
                    )
                    .into_any_element()
                }
            })
            .collect();

        let overflow_pill = (overflow > 0).then(|| {
            div()
                .flex()
                .justify_end()
                .text_size(Theme::TEXT_XS)
                .text_color(theme.text_subtle)
                .child(format!("+{overflow} more queued"))
        });

        div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_2)
            .children(cards)
            .children(overflow_pill)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_assigns_monotonic_ids_and_dismisses() {
        let mut q = ToastQueue::new(2, 4);
        assert!(q.is_empty());

        let a = q.push(Toast::info("first"));
        let b = q.push(Toast::success("second"));
        let c = q.push(Toast::warning("third"));
        assert!(a < b && b < c);
        assert_eq!(q.len(), 3);
        assert_eq!(q.visible().count(), 2);
        assert_eq!(q.overflow_count(), 1);

        assert!(q.dismiss(a));
        assert!(!q.dismiss(a));
        assert_eq!(q.len(), 2);
        let visible_ids: Vec<_> = q.visible().map(|e| e.id).collect();
        assert_eq!(visible_ids, vec![b, c]);
    }

    #[test]
    fn bounded_queue_preserves_danger_toasts_when_evicting() {
        let mut q = ToastQueue::new(2, 3);
        let danger = q.push(Toast::danger("production gate"));
        let info1 = q.push(Toast::info("info 1"));
        let info2 = q.push(Toast::info("info 2"));
        assert_eq!(q.len(), 3);

        // Fourth toast evicts the oldest non-danger toast (`info1`), keeping `danger`.
        let info3 = q.push(Toast::info("info 3"));
        assert_eq!(q.len(), 3);
        let ids: Vec<_> = q.entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![danger, info2, info3]);
        assert!(!ids.contains(&info1));
    }

    #[test]
    fn bounded_queue_evicts_oldest_when_all_danger() {
        let mut q = ToastQueue::new(2, 2);
        let d1 = q.push(Toast::danger("first danger"));
        let d2 = q.push(Toast::danger("second danger"));
        let d3 = q.push(Toast::danger("third danger"));
        let ids: Vec<_> = q.entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![d2, d3]);
        assert!(!ids.contains(&d1));
    }
}
