//! What opens over any view without leaving it: the Inbox overlay (`⌘⇧I`), the Parent Agent dock (`⌘J`), and
//! the toasts that announce a new gate.

use std::collections::HashSet;
use std::time::Duration;

use gpui::{
    AnyElement, Context, Focusable, FontWeight, IntoElement, ParentElement, SharedString, Styled,
    Window, div, prelude::*, px,
};
use xctx_inbox::{ItemId, ItemKind};
use xctx_ui::{
    Button, ButtonSize, ButtonVariant, Icon, IconName, IconSize, InboxAction, Overlay,
    OverlayPlacement, ParentAgentDrawer, Theme, Toast, ToastAction,
};

use crate::workspace::{View, Workspace};

/// How often the Inbox is looked at for gates that arrived (it is in memory; this reads no file).
const GATE_WATCH_INTERVAL: Duration = Duration::from_millis(800);

/// The width of the Inbox overlay.
const INBOX_OVERLAY_WIDTH: f32 = 520.0;

/// The share of the window's height the Parent Agent dock takes.
const DOCK_HEIGHT_SHARE: f32 = 0.42;

/// Whether a new item of `kind` deserves a toast: gates and missing variables wait for the user; notices do not.
fn announces(kind: ItemKind) -> bool {
    kind.is_gate() || kind == ItemKind::MissingEnvVar
}

/// Whether a toast may approve an item of `kind` in one click: widenings, attempt proposals and tool gates that
/// are not high-risk. Plans are reviewed on Home; missing variables are set in the Inbox.
fn approvable_from_toast(kind: ItemKind, danger: bool) -> bool {
    !danger
        && matches!(
            kind,
            ItemKind::SpaceWiden | ItemKind::AttemptProposal | ItemKind::PluginGate
        )
}

impl Workspace {
    /// Watch the Inbox for gates that arrive (from agents, plugins, the loop) and raise a toast for each, so a gate
    /// is seen from whatever view is shown. Items pending at startup are counted once, not toasted one by one.
    pub(crate) fn start_gate_toasts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(GATE_WATCH_INTERVAL).await;
                if this
                    .update_in(cx, |this, window, cx| this.toast_new_gates(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn toast_new_gates(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(inbox) = &self.inbox else {
            return;
        };
        let pending: Vec<(ItemId, ItemKind, String, bool)> = inbox
            .items()
            .iter()
            .filter(|item| item.is_pending() && announces(item.kind()))
            .map(|item| {
                let preview = item.preview(&self.redactor);
                (
                    item.id.clone(),
                    item.kind(),
                    preview.title,
                    xctx_nav::is_danger_inbox_item(item),
                )
            })
            .collect();
        let Some(seen) = self.seen_gates.as_mut() else {
            // First look: everything already waiting is old news; say how much once.
            if !pending.is_empty() {
                let count = pending.len();
                let entity = cx.entity().downgrade();
                let toast = Toast::warning(if count == 1 {
                    "1 item waits in the Inbox".to_owned()
                } else {
                    format!("{count} items wait in the Inbox")
                })
                .ttl(Some(Duration::from_secs(8)))
                .action(
                    ToastAction::new("Open Inbox", move |window, cx| {
                        let _ = entity.update(cx, |this, cx| this.show_inbox(None, window, cx));
                    })
                    .variant(ButtonVariant::Primary),
                );
                self.toasts.update(cx, |stack, cx| {
                    stack.push(toast, cx);
                });
            }
            self.seen_gates = Some(pending.into_iter().map(|(id, ..)| id).collect());
            return;
        };
        // Forget what is no longer pending, so the set stays as small as the Inbox.
        let still: HashSet<&ItemId> = pending.iter().map(|(id, ..)| id).collect();
        seen.retain(|id| still.contains(id));
        let fresh: Vec<(ItemId, ItemKind, String, bool)> = pending
            .into_iter()
            .filter(|(id, ..)| seen.insert(id.clone()))
            .collect();
        for (id, kind, title, danger) in fresh {
            let entity = cx.entity().downgrade();
            let open_id = id.clone();
            let mut toast = if danger {
                Toast::danger(title).message("High-risk: review it in the Inbox before approving.")
            } else {
                Toast::warning(title)
            }
            .action(
                ToastAction::new(if danger { "Review" } else { "Open" }, move |window, cx| {
                    let id = open_id.clone();
                    let _ = entity.update(cx, |this, cx| this.show_inbox(Some(id), window, cx));
                })
                .variant(if danger {
                    ButtonVariant::Primary
                } else {
                    ButtonVariant::Secondary
                }),
            );
            if approvable_from_toast(kind, danger) {
                let entity = cx.entity().downgrade();
                toast = toast.action(
                    ToastAction::new("Approve", move |window, cx| {
                        let id = id.as_str().to_owned();
                        let _ = entity.update(cx, |this, cx| {
                            this.on_inbox_action(&id, InboxAction::Approve, window, cx)
                        });
                    })
                    .variant(ButtonVariant::Primary),
                );
            }
            self.toasts.update(cx, |stack, cx| {
                stack.push(toast, cx);
            });
        }
    }

    /// Open the Inbox over whatever is shown, with `expand` (an item's id) opened, or close it when it is open
    /// and nothing is to be expanded.
    pub(crate) fn show_inbox(
        &mut self,
        expand: Option<ItemId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.inbox_open && expand.is_none() {
            self.close_inbox(window, cx);
            return;
        }
        if self.dialog.is_some() {
            return;
        }
        if self.palette_open {
            self.palette
                .update(cx, |palette, cx| palette.close(window, cx));
            self.palette_open = false;
        }
        if let Some(id) = expand {
            self.inbox_expanded = Some(id.as_str().to_owned().into());
        }
        self.inbox_open = true;
        self.overlay_focus.focus(window, cx);
        cx.notify();
    }

    /// Close the Inbox overlay and give the keys back to what is shown.
    pub(crate) fn close_inbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.inbox_open = false;
        self.refocus_view(window, cx);
        cx.notify();
    }

    /// ⌘J: show the Parent Agent from any space as a dock along the bottom, or hide it. On Home, where the Parent
    /// Agent is the composer, it focuses it instead.
    pub(crate) fn toggle_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.view == View::Home {
            self.dock_open = false;
            self.focus_home(window, cx);
            cx.notify();
            return;
        }
        self.dock_open = !self.dock_open;
        if self.dock_open {
            if let Some(view) = &self.parent.view {
                view.focus_handle(cx).focus(window, cx);
            }
        } else {
            self.refocus_view(window, cx);
        }
        cx.notify();
    }

    /// Give the keys to what is shown: Home's composer, or the shown space's focused pane.
    fn refocus_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.view {
            View::Home => self.focus_home(window, cx),
            View::Space(_) => self.focus_active_pane(window, cx),
        }
    }

    /// The Inbox overlay, when it is open.
    pub(crate) fn render_inbox_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.inbox_open {
            return None;
        }
        let theme = Theme::of(cx);
        let (close, close_button) = (cx.entity().downgrade(), cx.entity().downgrade());
        let content = div()
            .id("inbox-overlay")
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .h(Theme::TITLE_BAR_HEIGHT)
                    .px(Theme::SPACE_3)
                    .border_b_1()
                    .border_color(theme.border_subtle)
                    .child(
                        Icon::new(IconName::Inbox)
                            .size(IconSize::Sm)
                            .color(theme.text_muted),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(Theme::TEXT_SM)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Needs you"),
                    )
                    .child(
                        div()
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_subtle)
                            .child("⌘⇧I or Esc to close"),
                    )
                    .child(
                        Button::new("inbox-overlay-close", "")
                            .icon(IconName::Close)
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .tooltip("Close ⌘⇧I")
                            .selector("inbox-overlay-close")
                            .on_click(move |_, window, cx| {
                                let _ = close_button
                                    .update(cx, |this, cx| this.close_inbox(window, cx));
                            }),
                    ),
            )
            .child(
                div()
                    .id("inbox-overlay-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(Theme::SPACE_3)
                    .child(self.render_inbox(cx)),
            );
        Some(
            Overlay::new("inbox-overlay-layer", content)
                .placement(OverlayPlacement::Right)
                .width(px(INBOX_OVERLAY_WIDTH))
                .focus_handle(self.overlay_focus.clone())
                .surface_selector("inbox-overlay")
                .on_close(move |window, cx| {
                    let _ = close.update(cx, |this, cx| this.close_inbox(window, cx));
                })
                .into_any_element(),
        )
    }

    /// The Parent Agent dock, when it is open and a space is shown (Home has the Parent Agent as its composer).
    pub(crate) fn render_dock(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.dock_open || self.view == View::Home {
            return None;
        }
        let theme = Theme::of(cx);
        let height = (window.viewport_size().height * DOCK_HEIGHT_SHARE).max(px(220.0));
        let (restart, hide) = (cx.entity().downgrade(), cx.entity().downgrade());
        let mut drawer = ParentAgentDrawer::new(self.parent.status.clone())
            .height(height)
            .on_restart(move |window, cx| {
                let _ = restart.update(cx, |this, cx| this.start_parent_agent(window, cx));
            });
        if let Some(view) = &self.parent.view {
            let focused = view.focus_handle(cx).contains_focused(window, cx);
            drawer = drawer.focused(focused).body(view.clone());
        }
        let content = div()
            .flex()
            .flex_col()
            .w_full()
            .p(Theme::SPACE_3)
            .gap(Theme::SPACE_2)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_subtle)
                    .child(SharedString::from(
                        "Talk to the Parent Agent from any space. Esc goes to it; ⌘J hides it.",
                    ))
                    .child(div().flex_1())
                    .child(
                        Button::new("dock-hide", "")
                            .icon(IconName::Close)
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .tooltip("Hide ⌘J")
                            .selector("dock-hide")
                            .on_click(move |_, window, cx| {
                                let _ = hide.update(cx, |this, cx| this.toggle_dock(window, cx));
                            }),
                    ),
            )
            .child(drawer);
        // Esc belongs to the terminal (docs/m6.md D5), and the panes above stay usable, so this is a plain panel
        // along the bottom (painted over the panes), not a modal overlay that would catch every click.
        Some(
            gpui::deferred(
                div()
                    .id("parent-dock")
                    .debug_selector(|| "parent-dock".to_owned())
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .occlude()
                    .bg(theme.overlay)
                    .border_t_1()
                    .border_color(theme.border_strong)
                    .shadow_lg()
                    .child(content),
            )
            .with_priority(5)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_gates_and_missing_variables_raise_toasts() {
        assert!(announces(ItemKind::SpaceWiden));
        assert!(announces(ItemKind::PlanGate));
        assert!(announces(ItemKind::MissingEnvVar));
        assert!(!announces(ItemKind::Notice));
    }

    #[test]
    fn a_toast_never_approves_a_high_risk_gate_or_a_plan() {
        assert!(approvable_from_toast(ItemKind::SpaceWiden, false));
        assert!(!approvable_from_toast(ItemKind::SpaceWiden, true));
        assert!(!approvable_from_toast(ItemKind::PluginGate, true));
        assert!(!approvable_from_toast(ItemKind::PlanGate, false));
        assert!(!approvable_from_toast(ItemKind::MissingEnvVar, false));
    }
}
