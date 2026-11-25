//! Headless GPUI render tests for the Milestone 6 UI kit: `Spinner` entity isolation, `Overlay`
//! scrim/Escape/focus-trap behaviour, `ToastStack` actions and isolation, and `FrameOverlay`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Context, Entity, FocusHandle, IntoElement, Modifiers, Render, StyleRefinement, TestAppContext,
    VisualTestContext, Window, div, prelude::*, px,
};
use xctx_ui::{
    FrameOverlay, Icon, IconName, IconSize, MotionSettings, Overlay, OverlayPlacement, Spinner,
    Theme, Toast, ToastAction, ToastStack, Tone,
};

struct SiblingWorkspace {
    renders: Rc<Cell<usize>>,
}

impl Render for SiblingWorkspace {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        div().size_full().child("ambient workspace")
    }
}

struct IsolationHost {
    sibling: Entity<SiblingWorkspace>,
    spinner: Entity<Spinner>,
    toasts: Entity<ToastStack>,
    hud: Entity<FrameOverlay>,
}

impl Render for IsolationHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                self.sibling
                    .clone()
                    .cached(StyleRefinement::default().size(px(200.0))),
            )
            .child(Icon::new(IconName::Home).size(IconSize::Sm))
            .child(self.spinner.clone())
            .child(self.toasts.clone())
            .child(self.hud.clone())
    }
}

#[gpui::test]
fn spinner_toast_and_frame_overlay_do_not_rerender_parent(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        MotionSettings::set_reduced(cx, true);
    });

    let sibling_renders = Rc::new(Cell::new(0usize));
    let renders_clone = sibling_renders.clone();
    let (host, vcx) = cx.add_window_view(|_, cx| {
        let sibling = cx.new(|_| SiblingWorkspace {
            renders: renders_clone,
        });
        let spinner = cx.new(|_| Spinner::new());
        let toasts = cx.new(|_| ToastStack::new());
        let hud = cx.new(|_| FrameOverlay::new("Workspace"));
        IsolationHost {
            sibling,
            spinner,
            toasts,
            hud,
        }
    });

    vcx.run_until_parked();
    let initial_renders = sibling_renders.get();
    assert!(initial_renders >= 1);

    // Updating Spinner notifies only the Spinner entity, never the parent host.
    vcx.update(|_, cx| {
        let spinner = host.read(cx).spinner.clone();
        spinner.update(cx, |s, cx| s.set_active(false, cx));
    });
    vcx.run_until_parked();
    assert_eq!(sibling_renders.get(), initial_renders);

    // Pushing and dismissing a Toast notifies only ToastStack, never the cached sibling.
    let toast_id = vcx.update(|_, cx| {
        let toasts = host.read(cx).toasts.clone();
        toasts.update(cx, |stack, cx| {
            stack.push(Toast::warning("Gate pending").message("Needs approval"), cx)
        })
    });
    vcx.run_until_parked();
    assert_eq!(sibling_renders.get(), initial_renders);
    assert!(vcx.debug_bounds("toast-card-1").is_some());

    vcx.update(|_, cx| {
        let toasts = host.read(cx).toasts.clone();
        toasts.update(cx, |stack, cx| {
            assert!(stack.dismiss(toast_id, cx));
        });
    });
    vcx.run_until_parked();
    assert_eq!(sibling_renders.get(), initial_renders);
    vcx.read(|cx| {
        assert!(host.read(cx).toasts.read(cx).is_empty());
    });

    // Toggling and recording frames in FrameOverlay never re-renders the cached sibling.
    vcx.update(|_, cx| {
        let hud = host.read(cx).hud.clone();
        hud.update(cx, |h, cx| {
            h.set_visible(true, cx);
            h.record_frame(Duration::from_millis(5), Some("Workspace"), cx);
        });
    });
    vcx.run_until_parked();
    assert_eq!(sibling_renders.get(), initial_renders);
    assert!(vcx.debug_bounds("frame-overlay-hud").is_some());

    // Pushing another toast and dismissing it via its close button works interactively.
    vcx.update(|_, cx| {
        let toasts = host.read(cx).toasts.clone();
        toasts.update(cx, |stack, cx| {
            stack.push(Toast::info("Dismiss via button"), cx)
        });
    });
    vcx.run_until_parked();
    let close_bounds = vcx
        .debug_bounds("toast-2-close")
        .expect("close button must be rendered");
    vcx.simulate_click(close_bounds.center(), Modifiers::default());
    vcx.run_until_parked();
    vcx.read(|cx| {
        assert!(host.read(cx).toasts.read(cx).is_empty());
    });
}

struct OverlayHost {
    open: bool,
    focus_handle: FocusHandle,
    events: Rc<RefCell<Vec<String>>>,
}

impl Render for OverlayHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let events_close = self.events.clone();
        let events_trap = self.events.clone();
        let entity = cx.entity().downgrade();

        let overlay = self.open.then(|| {
            Overlay::new(
                "test-overlay",
                div()
                    .p_4()
                    .debug_selector(|| "overlay-inner".to_owned())
                    .child("Overlay content"),
            )
            .placement(OverlayPlacement::Center)
            .width(px(300.0))
            .focus_handle(self.focus_handle.clone())
            .backdrop_selector("overlay-backdrop")
            .surface_selector("overlay-surface")
            .on_focus_trap(move |event, _, _| {
                events_trap
                    .borrow_mut()
                    .push(format!("trap:{}", event.keystroke.key));
            })
            .on_close(move |_, cx| {
                events_close.borrow_mut().push("close".to_owned());
                let _ = entity.update(cx, |this, cx| {
                    this.open = false;
                    cx.notify();
                });
            })
        });

        div().relative().size_full().children(overlay)
    }
}

fn click_point(vcx: &mut VisualTestContext, pt: gpui::Point<gpui::Pixels>) {
    vcx.simulate_click(pt, Modifiers::default());
    vcx.run_until_parked();
}

#[gpui::test]
fn overlay_handles_surface_click_backdrop_click_tab_trap_and_escape(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        MotionSettings::set_reduced(cx, true);
    });

    let events = Rc::new(RefCell::new(Vec::<String>::new()));
    let events_clone = events.clone();
    let (host, vcx) = cx.add_window_view(|window, cx| {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        OverlayHost {
            open: true,
            focus_handle,
            events: events_clone,
        }
    });

    vcx.run_until_parked();
    let surface_bounds = vcx
        .debug_bounds("overlay-surface")
        .expect("surface must be drawn");
    let backdrop_bounds = vcx
        .debug_bounds("overlay-backdrop")
        .expect("backdrop must be drawn");

    // Clicking inside the surface does not close the overlay.
    click_point(vcx, surface_bounds.center());
    assert!(events.borrow().is_empty());
    vcx.read(|cx| assert!(host.read(cx).open));

    // Pressing Tab triggers the focus trap hook rather than escaping.
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    assert_eq!(events.borrow().as_slice(), ["trap:tab"]);

    // Pressing Escape invokes on_close and closes the overlay.
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert_eq!(events.borrow().as_slice(), ["trap:tab", "close"]);
    vcx.read(|cx| assert!(!host.read(cx).open));

    // Re-open and test clicking on the scrim backdrop outside the surface.
    vcx.update(|window, cx| {
        host.update(cx, |this, cx| {
            this.open = true;
            window.focus(&this.focus_handle, cx);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    // Top-left corner of the backdrop is outside the centred surface.
    let scrim_pt = gpui::point(
        backdrop_bounds.origin.x + px(4.0),
        backdrop_bounds.origin.y + px(4.0),
    );
    click_point(vcx, scrim_pt);
    assert_eq!(events.borrow().as_slice(), ["trap:tab", "close", "close"]);
    vcx.read(|cx| assert!(!host.read(cx).open));
}

#[gpui::test]
fn toast_action_button_invokes_callback_and_dismisses(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        MotionSettings::set_reduced(cx, true);
    });

    let clicked = Rc::new(Cell::new(false));
    let clicked_clone = clicked.clone();

    let (stack, vcx) = cx.add_window_view(|_, cx| {
        let mut stack = ToastStack::new();
        stack.push(
            Toast::new("Gate waiting")
                .tone(Tone::Warning)
                .action(ToastAction::new("Approve", move |_, _| {
                    clicked_clone.set(true);
                })),
            cx,
        );
        stack
    });

    vcx.run_until_parked();
    let action_bounds = vcx
        .debug_bounds("toast-1-action-0")
        .expect("action button must be rendered");
    vcx.simulate_click(action_bounds.center(), Modifiers::default());
    vcx.run_until_parked();

    assert!(clicked.get());
    vcx.read(|cx| assert!(stack.read(cx).is_empty()));
}
