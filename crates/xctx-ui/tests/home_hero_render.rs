//! Headless GPUI render tests for [`HomeBackdrop`], [`HomeHero`], [`ComposerCard`] and
//! [`ComposerChip`].

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Context, Modifiers, Render, TestAppContext, VisualTestContext, Window, div, prelude::*,
};
use xctx_ui::{
    ComposerCard, ComposerChip, HomeBackdrop, HomeHero, IconName, MotionSettings, Theme, Tone,
};

struct HomeHeroHost {
    can_submit: bool,
    compact: bool,
    chip_clicks: Rc<Cell<usize>>,
    submit_clicks: Rc<Cell<usize>>,
}

impl Render for HomeHeroHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let chip_clicks = self.chip_clicks.clone();
        let submit_clicks = self.submit_clicks.clone();

        let card = ComposerCard::new(div().child("What are we building?"))
            .focused(self.can_submit)
            .can_submit(self.can_submit)
            .left_readout("parent agent")
            .right_readout("0 spaces")
            .chip(
                ComposerChip::new("chip-agent", "omp")
                    .icon(IconName::Orchestrator)
                    .tone(Tone::Accent)
                    .pill(true)
                    .on_click(move |_, _, _| {
                        chip_clicks.set(chip_clicks.get() + 1);
                    }),
            )
            .chip(
                ComposerChip::new("chip-folder", "xctx")
                    .icon(IconName::Folder)
                    .tooltip("Working folder"),
            )
            .on_submit(move |_, _, _| {
                submit_clicks.set(submit_clicks.get() + 1);
            });

        div()
            .relative()
            .size_full()
            .bg(theme.chrome)
            .child(HomeBackdrop::new())
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .items_center()
                    .size_full()
                    .px(Theme::SPACE_4)
                    .child(HomeHero::new("v0.1 · parent agent").compact(self.compact))
                    .child(card),
            )
    }
}

fn click_point(vcx: &mut VisualTestContext, pt: gpui::Point<gpui::Pixels>) {
    vcx.simulate_click(pt, Modifiers::default());
    vcx.run_until_parked();
}

#[gpui::test]
fn composer_chip_and_submit_handlers_fire_only_when_expected(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        MotionSettings::set_reduced(cx, true);
    });

    let chip_clicks = Rc::new(Cell::new(0usize));
    let submit_clicks = Rc::new(Cell::new(0usize));

    let chip_clicks_host = chip_clicks.clone();
    let submit_clicks_host = submit_clicks.clone();
    let (host, vcx) = cx.add_window_view(|_, _| HomeHeroHost {
        can_submit: false,
        compact: false,
        chip_clicks: chip_clicks_host,
        submit_clicks: submit_clicks_host,
    });

    vcx.run_until_parked();

    let agent_chip_bounds = vcx
        .debug_bounds("chip-agent")
        .expect("agent chip must be drawn");
    let folder_chip_bounds = vcx
        .debug_bounds("chip-folder")
        .expect("folder chip must be drawn");
    let submit_bounds = vcx
        .debug_bounds("composer-submit")
        .expect("submit button must be drawn");

    // Submit must NOT fire when can_submit is false.
    click_point(vcx, submit_bounds.center());
    assert_eq!(submit_clicks.get(), 0);
    assert_eq!(chip_clicks.get(), 0);

    // A chip without an on_click handler must not fire any handler.
    click_point(vcx, folder_chip_bounds.center());
    assert_eq!(chip_clicks.get(), 0);
    assert_eq!(submit_clicks.get(), 0);

    // Clicking the interactive chip fires its click handler only.
    click_point(vcx, agent_chip_bounds.center());
    assert_eq!(chip_clicks.get(), 1);
    assert_eq!(submit_clicks.get(), 0);

    // Enabling can_submit allows the submit handler to fire.
    vcx.update(|_, cx| {
        host.update(cx, |this, cx| {
            this.can_submit = true;
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let submit_bounds = vcx
        .debug_bounds("composer-submit")
        .expect("submit button must remain drawn");
    click_point(vcx, submit_bounds.center());
    assert_eq!(submit_clicks.get(), 1);
    assert_eq!(chip_clicks.get(), 1);

    // Compact hero mode renders cleanly alongside the composer.
    vcx.update(|_, cx| {
        host.update(cx, |this, cx| {
            this.compact = true;
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("composer-submit").is_some());
}
