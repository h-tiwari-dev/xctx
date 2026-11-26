//! Drives the xctx-ui components in GPUI's headless test window: typing into the prompt, dragging a split
//! divider, clicking a pane, and drawing the bars.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    ClipboardItem, Context, Entity, Focusable, IntoElement, Modifiers, MouseButton, Render,
    Subscription, TestAppContext, VisualTestContext, Window, div, point, prelude::*, px,
};
use xctx_layout::{PaneId, PaneKind, PaneTree, Rect, Shape, SplitAxis};
use xctx_ui::{
    PaneView, PromptEvent, PromptInput, SpaceHeaderInfo, SplitView, StatusBanner, StatusBar, Theme,
    TitleBar, ratio_for_drag,
};

struct PromptHost {
    prompt: Entity<PromptInput>,
    events: Vec<PromptEvent>,
    _subscription: Subscription,
}

impl Render for PromptHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.prompt.clone())
    }
}

fn prompt_host(cx: &mut TestAppContext) -> (Entity<PromptHost>, &mut VisualTestContext) {
    let (host, cx) = cx.add_window_view(|_, cx| {
        let prompt = cx.new(|cx| PromptInput::new(cx).placeholder("What should change?"));
        let subscription = cx.subscribe(&prompt, |host: &mut PromptHost, _, event, _| {
            host.events.push(event.clone());
        });
        PromptHost {
            prompt,
            events: Vec::new(),
            _subscription: subscription,
        }
    });
    let prompt = host.read_with(cx, |host, _| host.prompt.clone());
    cx.update(|window, cx| prompt.focus_handle(cx).focus(window, cx));
    cx.run_until_parked();
    (host, cx)
}

fn value(host: &Entity<PromptHost>, cx: &mut VisualTestContext) -> String {
    host.read_with(cx, |host, cx| host.prompt.read(cx).value().to_owned())
}

#[gpui::test]
fn prompt_types_edits_and_submits(cx: &mut TestAppContext) {
    let (host, cx) = prompt_host(cx);

    cx.simulate_input("fix retry");
    assert_eq!(value(&host, cx), "fix retry");

    cx.simulate_keystrokes("alt-backspace");
    assert_eq!(value(&host, cx), "fix ");

    cx.simulate_keystrokes("cmd-left");
    cx.simulate_input("x");
    cx.simulate_keystrokes("cmd-right backspace");
    assert_eq!(value(&host, cx), "xfix");

    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("escape");
    host.read_with(cx, |host, _| {
        assert_eq!(
            host.events,
            vec![PromptEvent::Submit("xfix".to_owned()), PromptEvent::Cancel]
        );
    });
}

#[gpui::test]
fn prompt_does_not_submit_blank_text(cx: &mut TestAppContext) {
    let (host, cx) = prompt_host(cx);
    cx.simulate_keystrokes("enter space enter");
    host.read_with(cx, |host, _| assert!(host.events.is_empty()));
}

#[gpui::test]
fn prompt_selects_all_copies_and_pastes(cx: &mut TestAppContext) {
    let (host, cx) = prompt_host(cx);
    cx.simulate_input("abc");
    cx.simulate_keystrokes("cmd-a cmd-c backspace");
    assert_eq!(value(&host, cx), "");
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("abc".to_owned())
    );

    cx.write_to_clipboard(ClipboardItem::new_string("one\ntwo".to_owned()));
    cx.simulate_keystrokes("cmd-v");
    assert_eq!(value(&host, cx), "one two");

    let prompt = host.read_with(cx, |host, _| host.prompt.clone());
    prompt.update(cx, |prompt, cx| prompt.clear(cx));
    assert_eq!(value(&host, cx), "");
    prompt.update(cx, |prompt, cx| prompt.set_value("again", cx));
    cx.simulate_input("!");
    assert_eq!(value(&host, cx), "again!");
}

struct SplitHost {
    tree: Rc<RefCell<PaneTree>>,
}

impl Render for SplitHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tree = self.tree.borrow().clone();
        let panes = tree
            .panes()
            .into_iter()
            .map(|id| {
                PaneView::new(id, format!("pane {}", id.0), div().size_full()).kind(PaneKind::Shell)
            })
            .collect();
        let entity = cx.entity().downgrade();
        let resize_tree = self.tree.clone();
        let focus_tree = self.tree.clone();
        let zoom_tree = self.tree.clone();
        div().size_full().child(
            SplitView::new("split", &tree, panes)
                .on_resize(move |split, ratio, _, cx| {
                    if resize_tree
                        .borrow_mut()
                        .set_split_ratio(split, ratio)
                        .is_ok()
                    {
                        entity.update(cx, |_, cx| cx.notify()).ok();
                    }
                })
                .on_focus(move |pane, _, _| {
                    focus_tree.borrow_mut().focus(pane).ok();
                })
                .on_zoom(move |pane, _, _| {
                    zoom_tree.borrow_mut().set_zoom(Some(pane)).ok();
                })
                .on_close(|_, _, _| {}),
        )
    }
}

#[gpui::test]
fn dragging_a_divider_resizes_and_clicking_a_pane_focuses_it(cx: &mut TestAppContext) {
    let mut tree = PaneTree::new(PaneId(1));
    assert!(
        tree.split(PaneId(1), SplitAxis::Horizontal, PaneId(2), 0.5)
            .is_ok()
    );
    let tree = Rc::new(RefCell::new(tree));
    let host_tree = tree.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| SplitHost { tree: host_tree });
    cx.run_until_parked();

    let viewport = cx.update(|window, _| window.viewport_size());
    let bounds = Rect::new(0.0, 0.0, viewport.width.into(), viewport.height.into());
    let layout = tree
        .borrow()
        .layout(bounds, Theme::DIVIDER_THICKNESS.into());
    let Some(divider) = layout.dividers.first().copied() else {
        panic!("a split tree has a divider");
    };
    let centre = point(
        px(divider.rect.x + divider.rect.width / 2.0),
        px(divider.rect.y + divider.rect.height / 2.0),
    );
    let target_x = bounds.width * 0.3;
    let target = point(px(target_x), centre.y);

    cx.simulate_mouse_move(centre, None, Modifiers::default());
    cx.simulate_mouse_down(centre, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(target, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::default());

    let expected = ratio_for_drag(&divider, target_x, centre.y.into());
    let width = tree
        .borrow()
        .layout(bounds, Theme::DIVIDER_THICKNESS.into())
        .pane(PaneId(1))
        .map(|rect| rect.width)
        .unwrap_or_default();
    let expected_width = (bounds.width - divider.rect.width) * expected;
    assert!(
        (width - expected_width).abs() < 0.5,
        "pane 1 is {width} wide, expected {expected_width}"
    );

    // Moving after the release does not keep dragging.
    cx.simulate_mouse_move(centre, None, Modifiers::default());
    let width_after = tree
        .borrow()
        .layout(bounds, Theme::DIVIDER_THICKNESS.into())
        .pane(PaneId(1))
        .map(|rect| rect.width)
        .unwrap_or_default();
    assert!((width_after - width).abs() < 0.01);

    assert_eq!(tree.borrow().focused(), PaneId(2));
    let inside_first = point(px(20.0), px(bounds.height / 2.0));
    cx.simulate_click(inside_first, Modifiers::default());
    assert_eq!(tree.borrow().focused(), PaneId(1));
}

struct BarsHost;

impl Render for BarsHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                TitleBar::new("make checkout retry safe")
                    .space(SpaceHeaderInfo {
                        shape: Shape::Review,
                        flow_open: false,
                        production: true,
                        branch: "space/fix-retry".into(),
                        env: "production,fix-retry".into(),
                    })
                    .on_toggle_sidebar(|_, _| {})
                    .on_palette(|_, _| {})
                    .on_shape(|_, _, _| {})
                    .on_ship(|_, _| {})
                    .on_discard(|_, _| {}),
            )
            .child(div().flex_1())
            .child(
                StatusBar::new()
                    .space("fix-retry")
                    .mise_env("fix-retry")
                    .port(4101)
                    .branch("space/fix-retry")
                    .shape(Shape::Review)
                    .zoomed(true)
                    .banner(Some(StatusBanner::error("push failed")))
                    .on_dismiss_banner(|_, _| {}),
            )
    }
}

#[gpui::test]
fn bars_draw_with_every_item(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(Theme::dark()));
    let (_host, cx) = cx.add_window_view(|_, _| BarsHost);
    cx.run_until_parked();
    for selector in [
        "title-sidebar",
        "title-palette",
        "title-shape-2",
        "title-discard",
        "title-ship",
        "status-dismiss",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} not drawn");
    }
    // A press on a control must not reach the window-move area behind it (the test window cannot move).
    if let Some(center) = cx.debug_bounds("title-palette").map(|b| b.center()) {
        cx.simulate_click(center, Modifiers::default());
    }
}

struct ModeHost {
    toggles: Rc<RefCell<usize>>,
}

impl Render for ModeHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let toggles = self.toggles.clone();
        let bar = TitleBar::new("add rate limiting")
            .space(SpaceHeaderInfo {
                shape: Shape::Dev,
                flow_open: true,
                production: false,
                branch: "space/rate-limit".into(),
                env: "rate-limit".into(),
            })
            .on_toggle_flow(move |_, _| *toggles.borrow_mut() += 1);
        div().w(px(1400.0)).h_full().child(bar)
    }
}

#[gpui::test]
fn the_title_bar_mode_switch_flips_only_from_the_other_side(cx: &mut TestAppContext) {
    let toggles = Rc::new(RefCell::new(0));
    let host_toggles = toggles.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| ModeHost {
        toggles: host_toggles,
    });
    cx.run_until_parked();
    // Flow is shown: clicking Flow again does nothing, Panes flips back.
    for selector in ["title-mode-flow", "title-mode-panes"] {
        let center = cx.debug_bounds(selector).map(|b| b.center());
        assert!(center.is_some(), "{selector} not drawn");
        if let Some(center) = center {
            cx.simulate_click(center, Modifiers::default());
        }
    }
    assert_eq!(*toggles.borrow(), 1);
    // Shapes arrange panes, so the shape control is hidden while Flow is shown.
    assert!(cx.debug_bounds("title-shape-0").is_none());
}
