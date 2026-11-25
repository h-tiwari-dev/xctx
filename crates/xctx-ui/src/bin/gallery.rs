//! Interactive component gallery for `xctx-ui` (`mise run gallery`).
//!
//! Displays every M6 UI kit primitive (`Icon`, `Spinner`, `Overlay`, `Toast` / `ToastStack`,
//! `FrameOverlay`) alongside `Button`, `Badge`, and `Pill` in every state.

use std::time::Duration;

use gpui::{
    App, AppContext, Bounds, Context, Entity, FocusHandle, FontWeight, KeyBinding, Menu, MenuItem,
    SharedString, SystemMenuType, TitlebarOptions, Window, WindowBounds, WindowOptions, actions,
    div, prelude::*, px, size,
};
use xctx_ui::{
    Badge, Button, ButtonSize, ButtonVariant, FrameOverlay, Icon, IconName, IconSize,
    MotionSettings, Overlay, OverlayPlacement, Pill, Spinner, Theme, Toast, ToastAction,
    ToastStack, Tone, UiAssets,
};

actions!(
    gallery,
    [
        /// Quit the component gallery.
        Quit,
        /// Toggle the frame-time debug HUD (`⌘⇧.`).
        ToggleFrameOverlay,
    ]
);

/// Which demo overlay is currently open in the gallery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveOverlay {
    /// Centred modal dialog overlay.
    Center,
    /// Top-centred command-palette style overlay.
    TopCenter,
    /// Right-anchored Inbox style overlay.
    Right,
}

struct GalleryView {
    focus_handle: FocusHandle,
    overlay_focus: FocusHandle,
    active_overlay: Option<ActiveOverlay>,
    spinners_active: bool,
    spinner_xs: Entity<Spinner>,
    spinner_sm: Entity<Spinner>,
    spinner_md: Entity<Spinner>,
    spinner_lg: Entity<Spinner>,
    spinner_success: Entity<Spinner>,
    spinner_warning: Entity<Spinner>,
    spinner_inactive: Entity<Spinner>,
    toasts: Entity<ToastStack>,
    frame_overlay: Entity<FrameOverlay>,
    last_event: SharedString,
}

impl GalleryView {
    fn new(cx: &mut Context<Self>) -> Self {
        let spinner_xs = cx.new(|_| Spinner::new().size(IconSize::Xs).tone(Tone::Accent));
        let spinner_sm = cx.new(|_| Spinner::new().size(IconSize::Sm).tone(Tone::Accent));
        let spinner_md = cx.new(|_| Spinner::new().size(IconSize::Md).tone(Tone::Accent));
        let spinner_lg = cx.new(|_| Spinner::new().size(IconSize::Lg).tone(Tone::Accent));
        let spinner_success = cx.new(|_| Spinner::new().size(IconSize::Md).tone(Tone::Success));
        let spinner_warning = cx.new(|_| Spinner::new().size(IconSize::Md).tone(Tone::Warning));
        let spinner_inactive = cx.new(|_| {
            Spinner::new()
                .size(IconSize::Md)
                .tone(Tone::Neutral)
                .active(false)
        });
        let toasts = cx.new(|_| ToastStack::new());
        let frame_overlay = cx.new(|_| {
            FrameOverlay::new("GalleryView")
                .visible(true)
                .auto_sample(true)
        });

        Self {
            focus_handle: cx.focus_handle(),
            overlay_focus: cx.focus_handle(),
            active_overlay: None,
            spinners_active: true,
            spinner_xs,
            spinner_sm,
            spinner_md,
            spinner_lg,
            spinner_success,
            spinner_warning,
            spinner_inactive,
            toasts,
            frame_overlay,
            last_event: "Ready — try ⌘⇧. to toggle FrameOverlay".into(),
        }
    }

    fn toggle_spinners(&mut self, cx: &mut Context<Self>) {
        self.spinners_active = !self.spinners_active;
        let active = self.spinners_active;
        for spinner in [
            &self.spinner_xs,
            &self.spinner_sm,
            &self.spinner_md,
            &self.spinner_lg,
            &self.spinner_success,
            &self.spinner_warning,
        ] {
            spinner.update(cx, |s, cx| s.set_active(active, cx));
        }
        self.last_event = if active {
            "Spinners running".into()
        } else {
            "Spinners paused".into()
        };
        cx.notify();
    }

    fn open_overlay(&mut self, which: ActiveOverlay, window: &mut Window, cx: &mut Context<Self>) {
        self.active_overlay = Some(which);
        window.focus(&self.overlay_focus, cx);
        self.last_event = format!("Opened {which:?} overlay (Esc or click scrim to close)").into();
        cx.notify();
    }

    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_overlay.take().is_some() {
            window.focus(&self.focus_handle, cx);
            self.last_event = "Overlay dismissed".into();
            cx.notify();
        }
    }

    fn push_sample_toast(&mut self, tone: Tone, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        let toast = match tone {
            Tone::Neutral => Toast::new("Workspace snapshot saved")
                .message("Saved 3 spaces and pane layouts to disk.")
                .ttl(Some(Duration::from_secs(4))),
            Tone::Accent => Toast::info("Attempt 2 ready for review")
                .message("writer · api finished tests (14/14 passing).")
                .action(
                    ToastAction::new("Open", move |_, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.last_event = "Clicked Open on Accent toast".into();
                            cx.notify();
                        });
                    })
                    .variant(ButtonVariant::Primary),
                ),
            Tone::Success => Toast::success("Space shipped")
                .message("Merged checkout-retry into main and cleaned up worktrees."),
            Tone::Warning => {
                let view_approve = view.clone();
                let view_open = view;
                Toast::warning("Gate waiting: checkout-retry")
                    .message("writer · api requests permission to run database migration.")
                    .action(
                        ToastAction::new("Approve", move |_, cx| {
                            let _ = view_approve.update(cx, |this, cx| {
                                this.last_event = "Approved gate from toast".into();
                                cx.notify();
                            });
                        })
                        .variant(ButtonVariant::Primary),
                    )
                    .action(ToastAction::new("Open", move |_, cx| {
                        let _ = view_open.update(cx, |this, cx| {
                            this.last_event = "Opened gate detail from toast".into();
                            cx.notify();
                        });
                    }))
            }
            Tone::Danger => Toast::danger("Production widen gate")
                .message("Adding production environment requires explicit confirmation in Inbox.")
                .action(
                    ToastAction::new("Inspect", move |_, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.last_event = "Opened production gate detail".into();
                            cx.notify();
                        });
                    })
                    .variant(ButtonVariant::Danger),
                ),
        };
        self.toasts.update(cx, |stack, cx| {
            stack.push(toast, cx);
        });
    }

    fn section(title: &'static str, subtitle: &'static str, theme: &Theme) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_3)
            .p(Theme::SPACE_4)
            .rounded(Theme::RADIUS_LG)
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(Theme::SPACE_2)
                    .child(
                        div()
                            .text_size(Theme::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_muted)
                            .child(subtitle),
                    ),
            )
    }

    fn render_overlay(&self, cx: &mut Context<Self>) -> Option<Overlay> {
        let which = self.active_overlay?;
        let theme = Theme::of(cx);
        let (placement, width, heading, body) = match which {
            ActiveOverlay::Center => (
                OverlayPlacement::Center,
                px(440.0),
                "Centred Modal Overlay",
                "Fades and slides in over 140ms (snaps when reduced motion is enabled). Press Escape or click the scrim to dismiss.",
            ),
            ActiveOverlay::TopCenter => (
                OverlayPlacement::TopCenter,
                px(520.0),
                "Top-Centred Palette Overlay",
                "Anchored near the top of the viewport for the command palette (⌘K). Input acts immediately even mid-animation.",
            ),
            ActiveOverlay::Right => (
                OverlayPlacement::Right,
                px(380.0),
                "Right-Anchored Drawer Overlay",
                "Slides in from the right edge for the Inbox overlay (⌘⇧I) and narrow-window Inspector.",
            ),
        };

        let close_btn = Button::new("gallery-overlay-close", "Close")
            .variant(ButtonVariant::Primary)
            .on_click(cx.listener(|this, _, window, cx| {
                this.close_overlay(window, cx);
            }));

        let content = div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_3)
            .p(Theme::SPACE_4)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(Theme::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(heading),
                    )
                    .child(Icon::new(IconName::Close).size(IconSize::Sm)),
            )
            .child(
                div()
                    .text_size(Theme::TEXT_SM)
                    .text_color(theme.text_muted)
                    .child(body),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .pt(Theme::SPACE_2)
                    .child(close_btn),
            );

        let entity = cx.entity().downgrade();
        Some(
            Overlay::new("gallery-demo-overlay", content)
                .placement(placement)
                .width(width)
                .focus_handle(self.overlay_focus.clone())
                .on_close(move |window, cx| {
                    let _ = entity.update(cx, |this, cx| this.close_overlay(window, cx));
                }),
        )
    }
}

impl Render for GalleryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let reduced = MotionSettings::is_reduced(cx);
        let hud_visible = self.frame_overlay.read(cx).is_visible();

        // Header toolbar
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap(Theme::SPACE_3)
            .px(Theme::SPACE_4)
            .py(Theme::SPACE_3)
            .border_b_1()
            .border_color(theme.divider)
            .bg(theme.surface)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(
                        Icon::new(IconName::Orchestrator)
                            .size(IconSize::Lg)
                            .tone(Tone::Accent),
                    )
                    .child(
                        div()
                            .text_size(Theme::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("xctx-ui Component Gallery"),
                    )
                    .child(Badge::new("m6-kit").tone(Tone::Accent)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(
                        Pill::new(
                            "toggle-reduced-motion",
                            if reduced {
                                "Reduced Motion: ON"
                            } else {
                                "Reduced Motion: OFF"
                            },
                        )
                        .selected(reduced)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            MotionSettings::set_reduced(cx, !reduced);
                            this.last_event = if !reduced {
                                "Reduced motion enabled".into()
                            } else {
                                "Reduced motion disabled".into()
                            };
                            cx.notify();
                        })),
                    )
                    .child(
                        Pill::new("toggle-frame-hud", "Frame HUD (⌘⇧.)")
                            .selected(hud_visible)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.frame_overlay.update(cx, |hud, cx| hud.toggle(cx));
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("simulate-slow-frame", "Simulate 20ms frame")
                            .size(ButtonSize::Compact)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.frame_overlay.update(cx, |hud, cx| {
                                    hud.record_frame(
                                        Duration::from_millis(20),
                                        Some("GalleryView"),
                                        cx,
                                    );
                                });
                                this.last_event = "Recorded synthetic 20ms slow frame".into();
                                cx.notify();
                            })),
                    ),
            );

        // 1. Icons grid
        let icon_tiles = IconName::ALL.iter().copied().map(|icon_name| {
            div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(Theme::SPACE_1)
                .w(px(96.0))
                .h(px(64.0))
                .rounded(Theme::RADIUS_MD)
                .border_1()
                .border_color(theme.border)
                .bg(theme.elevated)
                .child(Icon::new(icon_name).size(IconSize::Md).color(theme.text))
                .child(
                    div()
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_muted)
                        .child(icon_name.label()),
                )
        });

        let icons_section = Self::section(
            "Icons",
            "33 embedded Lucide SVGs via UiAssets (zero runtime file I/O)",
            &theme,
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(Theme::SPACE_2)
                .children(icon_tiles),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_4)
                .pt(Theme::SPACE_2)
                .child(
                    div()
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_subtle)
                        .child("Sizes:"),
                )
                .child(
                    Icon::new(IconName::Ship)
                        .size(IconSize::Xs)
                        .tone(Tone::Accent),
                )
                .child(
                    Icon::new(IconName::Ship)
                        .size(IconSize::Sm)
                        .tone(Tone::Accent),
                )
                .child(
                    Icon::new(IconName::Ship)
                        .size(IconSize::Md)
                        .tone(Tone::Accent),
                )
                .child(
                    Icon::new(IconName::Ship)
                        .size(IconSize::Lg)
                        .tone(Tone::Accent),
                )
                .child(
                    div()
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_subtle)
                        .child("Tones:"),
                )
                .child(
                    Icon::new(IconName::Idle)
                        .size(IconSize::Md)
                        .tone(Tone::Neutral),
                )
                .child(
                    Icon::new(IconName::Running)
                        .size(IconSize::Md)
                        .tone(Tone::Accent),
                )
                .child(
                    Icon::new(IconName::Check)
                        .size(IconSize::Md)
                        .tone(Tone::Success),
                )
                .child(
                    Icon::new(IconName::Warning)
                        .size(IconSize::Md)
                        .tone(Tone::Warning),
                )
                .child(
                    Icon::new(IconName::Error)
                        .size(IconSize::Md)
                        .tone(Tone::Danger),
                ),
        );

        // 2. Spinner section
        let spinner_section = Self::section(
            "Spinner",
            "Isolated GPUI entity (Performance Rule 4); static when reduced motion is on",
            &theme,
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_4)
                .child(self.spinner_xs.clone())
                .child(self.spinner_sm.clone())
                .child(self.spinner_md.clone())
                .child(self.spinner_lg.clone())
                .child(self.spinner_success.clone())
                .child(self.spinner_warning.clone())
                .child(self.spinner_inactive.clone())
                .child(
                    Button::new(
                        "toggle-spinners",
                        if self.spinners_active {
                            "Pause spinners"
                        } else {
                            "Resume spinners"
                        },
                    )
                    .size(ButtonSize::Compact)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_spinners(cx))),
                ),
        );

        // 3. Buttons section
        let button_variants = [
            ("Primary", ButtonVariant::Primary),
            ("Secondary", ButtonVariant::Secondary),
            ("Ghost", ButtonVariant::Ghost),
            ("Danger", ButtonVariant::Danger),
        ];
        let button_rows = button_variants.into_iter().map(|(label, variant)| {
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .child(
                    div()
                        .w(px(80.0))
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_muted)
                        .child(label),
                )
                .child(
                    Button::new(SharedString::from(format!("btn-{label}-reg")), "Regular")
                        .variant(variant),
                )
                .child(
                    Button::new(SharedString::from(format!("btn-{label}-sel")), "Selected")
                        .variant(variant)
                        .selected(true),
                )
                .child(
                    Button::new(SharedString::from(format!("btn-{label}-dis")), "Disabled")
                        .variant(variant)
                        .disabled(true),
                )
                .child(
                    Button::new(SharedString::from(format!("btn-{label}-cmp")), "Compact")
                        .variant(variant)
                        .size(ButtonSize::Compact),
                )
        });

        let buttons_section = Self::section(
            "Buttons, Pills & Badges",
            "Every variant, size, selection, disabled state and semantic tone",
            &theme,
        )
        .children(button_rows)
        .child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .pt(Theme::SPACE_2)
                .child(
                    div()
                        .w(px(80.0))
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_muted)
                        .child("Pills"),
                )
                .child(Pill::new("pill-default", "Unselected"))
                .child(Pill::new("pill-selected", "Selected").selected(true))
                .child(Pill::new("pill-disabled", "Disabled").disabled(true)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .child(
                    div()
                        .w(px(80.0))
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_muted)
                        .child("Badges"),
                )
                .child(Badge::new("Neutral").tone(Tone::Neutral))
                .child(Badge::new("Accent").tone(Tone::Accent))
                .child(Badge::new("Success").tone(Tone::Success))
                .child(Badge::new("Warning").tone(Tone::Warning))
                .child(Badge::new("Danger").tone(Tone::Danger)),
        );

        // 4. Overlay & Toast triggers
        let interactive_section = Self::section(
            "Overlay & Toasts",
            "140ms opacity + offset transitions; Escape / backdrop dismiss; isolated ToastStack entity",
            &theme,
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(Theme::SPACE_2)
                .child(
                    Button::new("open-center-overlay", "Open Centred Overlay")
                        .variant(ButtonVariant::Primary)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_overlay(ActiveOverlay::Center, window, cx);
                        })),
                )
                .child(
                    Button::new("open-top-overlay", "Open Palette Overlay").on_click(
                        cx.listener(|this, _, window, cx| {
                            this.open_overlay(ActiveOverlay::TopCenter, window, cx);
                        }),
                    ),
                )
                .child(
                    Button::new("open-right-overlay", "Open Right Overlay").on_click(
                        cx.listener(|this, _, window, cx| {
                            this.open_overlay(ActiveOverlay::Right, window, cx);
                        }),
                    ),
                ),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(Theme::SPACE_2)
                .child(
                    Button::new("toast-neutral", "Neutral Toast").on_click(cx.listener(
                        |this, _, _, cx| this.push_sample_toast(Tone::Neutral, cx),
                    )),
                )
                .child(
                    Button::new("toast-accent", "Accent Toast").on_click(cx.listener(
                        |this, _, _, cx| this.push_sample_toast(Tone::Accent, cx),
                    )),
                )
                .child(
                    Button::new("toast-success", "Success Toast").on_click(cx.listener(
                        |this, _, _, cx| this.push_sample_toast(Tone::Success, cx),
                    )),
                )
                .child(
                    Button::new("toast-warning", "Warning Gate Toast").on_click(cx.listener(
                        |this, _, _, cx| this.push_sample_toast(Tone::Warning, cx),
                    )),
                )
                .child(
                    Button::new("toast-danger", "Danger Gate Toast")
                        .variant(ButtonVariant::Danger)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.push_sample_toast(Tone::Danger, cx);
                        })),
                )
                .child(
                    Button::new("toast-clear", "Clear Toasts")
                        .variant(ButtonVariant::Ghost)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toasts.update(cx, |stack, cx| stack.clear(cx));
                        })),
                ),
        );

        // Status footer
        let footer = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .h(Theme::STATUS_BAR_HEIGHT)
            .px(Theme::SPACE_3)
            .border_t_1()
            .border_color(theme.divider)
            .bg(theme.surface)
            .text_size(Theme::TEXT_XS)
            .text_color(theme.text_muted)
            .child(self.last_event.clone())
            .child("⌘Q quit · ⌘⇧. frame overlay");

        let body = div()
            .id("gallery-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(Theme::SPACE_4)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(Theme::SPACE_4)
                    .max_w(px(1100.0))
                    .mx_auto()
                    .child(icons_section)
                    .child(spinner_section)
                    .child(buttons_section)
                    .child(interactive_section),
            );

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.bg)
            .text_color(theme.text)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &ToggleFrameOverlay, _, cx| {
                this.frame_overlay.update(cx, |hud, cx| hud.toggle(cx));
                cx.notify();
            }))
            .child(header)
            .child(body)
            .child(footer)
            // Floating bottom-right toast stack (isolated entity).
            .child(
                div()
                    .absolute()
                    .right(Theme::SPACE_4)
                    .bottom(px(36.0))
                    .child(self.toasts.clone()),
            )
            // Floating bottom-left debug frame HUD (isolated entity).
            .child(
                div()
                    .absolute()
                    .left(Theme::SPACE_4)
                    .bottom(px(36.0))
                    .child(self.frame_overlay.clone()),
            )
            .children(self.render_overlay(cx))
    }
}

fn main() {
    gpui_platform::application()
        .with_assets(UiAssets)
        .run(|cx: &mut App| {
            cx.set_global(Theme::dark());
            cx.set_global(MotionSettings::default());
            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("cmd-shift-.", ToggleFrameOverlay, None),
            ]);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.set_menus(vec![Menu::new("xctx-ui Gallery").items([
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Quit Gallery", Quit),
            ])]);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let bounds = Bounds::centered(None, size(px(1160.0), px(820.0)), cx);
            let opened = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("xctx-ui Component Gallery".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let view = cx.new(GalleryView::new);
                    let focus = view.read(cx).focus_handle.clone();
                    window.focus(&focus, cx);
                    view
                },
            );
            if let Err(e) = opened {
                eprintln!("gallery: could not open window: {e}");
                cx.quit();
                return;
            }
            cx.activate(true);
        });
}
