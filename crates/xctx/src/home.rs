//! Home: the Parent Agent you tell what you are working on, the space it drafts (launched with ⌘↵), the
//! Inbox, the living spaces, and the indexed folders.

use std::path::PathBuf;

use gpui::{
    AnyElement, Context, Div, ExternalPaths, Focusable, FontWeight, IntoElement, ParentElement,
    Styled, Window, div, prelude::*, px,
};
use xctx_core::{Space, SpaceDraft, SpaceId};
use xctx_graph::AgentRole;
use xctx_ui::{
    Badge, Button, ButtonSize, ButtonVariant, DRAWER_HEIGHT, Icon, IconName, IconSize, PaneLive,
    ParentAgentDrawer, Theme, Tone, card_badge, helper_kind_label, shape_label,
};

use crate::graph_panel::pane_lives;
use crate::lifecycle::{folder_to_index, home_dir, launch_space};
use crate::space_layout::layout_shape;
use crate::workspace::Workspace;

impl Workspace {
    /// Index the folder the user typed, and rescan.
    pub(crate) fn add_root(&mut self, input: String, cx: &mut Context<Self>) {
        let folder = match folder_to_index(&input, home_dir().as_deref()) {
            Ok(folder) => folder,
            Err(e) => {
                self.error(e, cx);
                return;
            }
        };
        let Some(store) = self.store.as_mut() else {
            self.error("the store is busy or unavailable; try again", cx);
            return;
        };
        match store.add_root(folder.clone()) {
            Ok(true) => {
                self.root_input.update(cx, |input, cx| input.clear(cx));
                self.info(format!("indexing {}", folder.display()), cx);
                self.rescan(cx);
            }
            Ok(false) => self.info(format!("{} is already indexed", folder.display()), cx),
            Err(e) => self.error(format!("could not add {}: {e}", folder.display()), cx),
        }
    }

    fn remove_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let Some(store) = self.store.as_mut() else {
            self.error("the store is busy or unavailable; try again", cx);
            return;
        };
        match store.remove_root(&root) {
            Ok(_) => {
                self.info(format!("stopped indexing {}", root.display()), cx);
                self.rescan(cx);
            }
            Err(e) => self.error(format!("could not remove {}: {e}", root.display()), cx),
        }
    }

    /// Launch `draft` in the background, then open and show its space. `launched` runs once the space exists,
    /// before its tab opens; `failed` runs when it could not be launched (the error is shown). Returns false,
    /// with the reason shown, when the store is busy.
    pub(crate) fn start_launch(
        &mut self,
        draft: SpaceDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
        launched: impl FnOnce(&mut Self, &xctx_core::Space, &mut Context<Self>) + 'static,
        failed: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) -> bool {
        let Some(mut store) = self.take_store(&format!("launching {}", draft.title), cx) else {
            return false;
        };
        // The new space's PORT must not be one a live attempt holds.
        let spaces: Vec<xctx_core::SpaceId> = self.spaces.iter().map(|s| s.id.clone()).collect();
        let attempt_ports = self.attempts.live_ports(&spaces);
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            let (store, result) = executor
                .spawn(async move {
                    let result = launch_space(&mut store, draft, attempt_ports);
                    (store, result)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.return_store(store, cx);
                match result {
                    Ok(launched_space) => {
                        let id = launched_space.space.id.clone();
                        let port = launched_space.space.port;
                        launched(this, &launched_space.space, cx);
                        this.refresh_spaces();
                        this.check_mise(vec![launched_space.space.clone()], cx);
                        this.open_launched_space(
                            launched_space.space,
                            launched_space.layout,
                            launched_space.panes,
                            window,
                            cx,
                        );
                        this.show_space(&id, window, cx);
                        match launched_space.warning {
                            Some(warning) => this.error(warning, cx),
                            None => this.info(format!("launched {id} on PORT {port}"), cx),
                        }
                    }
                    Err(e) => {
                        failed(this, cx);
                        this.error(e, cx);
                    }
                }
            });
        })
        .detach();
        true
    }

    /// Index every directory among `paths` dropped from Finder; anything else is skipped with a note. The
    /// directory checks run off the main thread.
    fn drop_roots(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let (dirs, skipped) = executor
                .spawn(async move {
                    let (dirs, others): (Vec<PathBuf>, Vec<PathBuf>) =
                        paths.into_iter().partition(|path| path.is_dir());
                    (dirs, others.len())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                for dir in dirs {
                    this.add_root(dir.display().to_string(), cx);
                }
                match skipped {
                    0 => {}
                    1 => this.info("skipped 1 dropped item: only folders are indexed", cx),
                    n => this.info(
                        format!("skipped {n} dropped items: only folders are indexed"),
                        cx,
                    ),
                }
            });
        })
        .detach();
    }

    /// Home's content: a centred column with the hero, the Parent Agent composer, the indexed folders (also a
    /// drop target for folders from Finder), the Inbox and the living spaces.
    pub(crate) fn render_home(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx);
        let terminal_height = (window.viewport_size().height * 0.38).max(DRAWER_HEIGHT);
        let dragging = cx.has_active_drag();
        let composer = div()
            .id("home-composer")
            .flex()
            .flex_col()
            .gap(Theme::SPACE_3)
            .p(Theme::SPACE_1)
            .rounded(Theme::RADIUS_LG)
            .border_2()
            .border_color(gpui::transparent_black())
            .drag_over::<ExternalPaths>(move |style, _, _, _| {
                style
                    .border_color(theme.accent.alpha(0.8))
                    .bg(theme.accent.alpha(0.06))
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.drop_roots(paths.paths().to_vec(), cx)
            }))
            .child(self.render_parent_agent(terminal_height, window, cx))
            .child(self.render_roots(cx))
            .when(dragging, |area| {
                area.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(Theme::SPACE_2)
                        .py(Theme::SPACE_2)
                        .rounded(Theme::RADIUS_MD)
                        .border_1()
                        .border_color(theme.accent.alpha(0.5))
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.accent)
                        .child(Icon::new(IconName::Folder).color(theme.accent))
                        .child("Drop folders to index them"),
                )
            });
        let mut column = div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_6)
            .w_full()
            .max_w(px(920.0))
            .px(Theme::SPACE_6)
            .pt(px(48.0))
            .pb(Theme::SPACE_6)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(Theme::SPACE_1)
                    .child(
                        div()
                            .text_size(Theme::TEXT_XL)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("What are you working on?"),
                    )
                    .child(
                        div()
                            .text_size(Theme::TEXT_SM)
                            .text_color(theme.text_muted)
                            .child("Tell the Parent Agent: it drafts a space from your indexed repos, and ⌘↵ launches it."),
                    ),
            )
            .child(composer);
        if let Some(plan) = self.render_plan_review(cx) {
            column = column.child(plan);
        }
        if let Some(error) = &self.store_error {
            column = column.child(notice(error.clone(), theme.danger, &theme));
        }
        if let Some(busy) = &self.busy {
            column = column.child(notice(format!("{busy}…"), theme.text_muted, &theme));
        }
        if self.inbox_pending() > 0 {
            column = column
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(Theme::SPACE_2)
                        .child(section_title("Needs you", &theme))
                        .child(self.render_inbox(cx)),
                )
                .child(self.render_spaces(cx));
        } else {
            column = column
                .child(self.render_spaces(cx))
                .child(self.render_inbox(cx));
        }
        div()
            .id("home")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .justify_center()
            // Top-aligned, the column is as tall as its content, so the overflow scrolls here instead of being
            // cut off at the window's height.
            .items_start()
            .child(column.flex_none())
            .into_any_element()
    }

    fn render_parent_agent(
        &self,
        height: gpui::Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> ParentAgentDrawer {
        let restart = cx.entity().downgrade();
        let mut drawer = ParentAgentDrawer::new(self.parent.status.clone())
            .height(height)
            .on_restart(move |window, cx| {
                let _ = restart.update(cx, |this, cx| this.start_parent_agent(window, cx));
            });
        if let Some(view) = &self.parent.view {
            let focused = view.focus_handle(cx).contains_focused(window, cx);
            drawer = drawer.focused(focused).body(view.clone());
        }
        drawer
    }

    fn render_spaces(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx);
        let section = div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_2)
            .child(section_title("Living spaces", &theme));
        if self.spaces.is_empty() {
            return section.child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .p(Theme::SPACE_4)
                    .rounded(Theme::RADIUS_LG)
                    .border_1()
                    .border_color(theme.border_subtle)
                    .text_size(Theme::TEXT_SM)
                    .text_color(theme.text_muted)
                    .child(Icon::new(IconName::Idle).color(theme.text_subtle))
                    .child("No spaces yet — tell the Parent Agent what you're working on."),
            );
        }
        let mut grid = div().flex().flex_wrap().gap(Theme::SPACE_3);
        // In Roster order, so a card's ⌘N is the key that shows it.
        let order: Vec<(SpaceId, Option<usize>)> = self
            .roster_entries(cx)
            .into_iter()
            .map(|entry| (entry.id, entry.item.shortcut))
            .collect();
        let spaces = order.iter().filter_map(|(id, shortcut)| {
            self.spaces
                .iter()
                .find(|space| &space.id == id)
                .map(|space| (space, *shortcut))
        });
        for (index, (space, shortcut)) in spaces.enumerate() {
            let id = space.id.clone();
            let running = self
                .tabs
                .get(&space.id)
                .is_some_and(|tab| tab.agent_running(cx));
            let header = div()
                .id(("space-card", index))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| this.show_space(&id, window, cx)))
                .child(space_summary(space, running, shortcut, &theme));
            grid = grid.child(
                div()
                    .id(("space-card-body", index))
                    .flex()
                    .flex_col()
                    .flex_1()
                    .flex_basis(px(380.0))
                    .min_w_0()
                    .gap(Theme::SPACE_3)
                    .p(Theme::SPACE_4)
                    .rounded(Theme::RADIUS_LG)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .shadow_sm()
                    .hover(move |style| style.bg(theme.hover))
                    .child(header)
                    .child(self.render_space_agents(space, index, cx)),
            );
        }
        section.child(grid)
    }

    /// The agents of `space` under its card, each with what its pane does and Open (or Start when it has no
    /// pane), which shows the space with that agent's pane focused. A space that is not open says so: its agents
    /// start when it opens.
    fn render_space_agents(&self, space: &Space, index: usize, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx);
        let rows = div().flex().flex_col().gap(Theme::SPACE_0_5);
        let (Some(tab), Some(state)) = (self.tabs.get(&space.id), self.graphs.get(&space.id))
        else {
            return rows.child(
                div()
                    .text_size(Theme::TEXT_SM)
                    .text_color(theme.text_subtle)
                    .child("Not open: click it to open the space and start its agents."),
            );
        };
        let live = pane_lives(state.graph(), tab, cx);
        let mut rows = rows;
        for (row_index, node) in state.graph().nodes().iter().enumerate() {
            let status = live.get(&node.id).copied().unwrap_or(PaneLive::NoPane);
            let (label, tone) = card_badge(&node.state, Some(status));
            let role = match &node.role {
                AgentRole::Orchestrator => "orchestrator".to_owned(),
                AgentRole::Writer { repo } => format!("writer · {repo}"),
                AgentRole::Helper { kind } => format!("helper · {}", helper_kind_label(*kind)),
            };
            let (space_id, agent) = (space.id.clone(), node.id.clone());
            let has_pane = node.pane_id.is_some();
            let action = Button::new(
                ("home-agent", index * 1000 + row_index),
                if has_pane { "Open" } else { "Start" },
            )
            .variant(if has_pane {
                ButtonVariant::Ghost
            } else {
                ButtonVariant::Primary
            })
            .size(ButtonSize::Compact)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.show_space(&space_id, window, cx);
                let result = if has_pane {
                    this.open_agent_pane(&space_id, &agent, window, cx)
                } else {
                    this.start_agent_pane(&space_id, &agent, window, cx)
                        .map(|_| ())
                };
                if let Err(e) = result {
                    this.show_op_error(e, cx);
                }
            }));
            rows = rows.child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .h(px(28.0))
                    .px(Theme::SPACE_2)
                    .rounded(Theme::RADIUS_MD)
                    .bg(theme.raised)
                    .text_size(Theme::TEXT_SM)
                    .child(
                        div()
                            .flex_none()
                            .size(px(7.0))
                            .rounded_full()
                            .bg(theme.tone(tone).fg),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(node.label.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text_subtle)
                            .child(role),
                    )
                    .child(Badge::new(label).tone(tone))
                    .child(action),
            );
        }
        rows
    }

    /// The folders xctx indexes for repos, as a compact chip row under the Parent Agent (it drafts from them):
    /// each folder with ×, how many repos were found, Rescan, and where another folder is typed.
    fn render_roots(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx);
        let status = if self.scanning {
            "indexing…".to_owned()
        } else {
            match self.index.repos().len() {
                1 => "1 repo indexed".to_owned(),
                count => format!("{count} repos indexed"),
            }
        };
        let mut chips = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(Theme::SPACE_1)
            .child(
                div()
                    .mr(Theme::SPACE_1)
                    .text_size(Theme::TEXT_XS)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text_subtle)
                    .child("Repos from"),
            );
        if self.roots.is_empty() {
            chips = chips.child(
                div()
                    .text_size(Theme::TEXT_SM)
                    .text_color(theme.warning)
                    .child(
                        "no folder yet: add or drop the one that holds your repos, such as ~/code",
                    ),
            );
        }
        for (index, root) in self.roots.iter().enumerate() {
            let root = root.clone();
            chips = chips.child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .h(px(24.0))
                    .pl(Theme::SPACE_2)
                    .rounded(Theme::RADIUS_FULL)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.raised)
                    .text_size(Theme::TEXT_SM)
                    .text_color(theme.text)
                    .child(
                        Icon::new(IconName::Folder)
                            .size(IconSize::Xs)
                            .color(theme.text_muted),
                    )
                    .child(root.display().to_string())
                    .child(
                        Button::new(("remove-root", index), "")
                            .icon(IconName::Close)
                            .tooltip("Stop indexing this folder")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .rounded_full()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_root(root.clone(), cx)
                            })),
                    ),
            );
        }
        chips = chips
            .child(
                div()
                    .ml(Theme::SPACE_1)
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.meta)
                    .child(status),
            )
            .child(
                Button::new("rescan", "")
                    .icon(IconName::Refresh)
                    .tooltip("Rescan")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .disabled(self.scanning)
                    .on_click(cx.listener(|this, _, _, cx| this.rescan(cx))),
            );
        div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_2)
            .px(Theme::SPACE_1)
            .child(chips)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(div().flex_1().child(self.root_input.clone()))
                    .child(
                        Button::new("add-root", "Add")
                            .icon(IconName::Plus)
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Compact)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let input = this.root_input.read(cx).value().to_owned();
                                this.add_root(input, cx);
                            })),
                    ),
            )
    }
}

/// A living space's card header: state dot, title, shape, its ⌘N key, then slug, branch, port and repos.
fn space_summary(space: &Space, running: bool, shortcut: Option<usize>, theme: &Theme) -> Div {
    let repos = space
        .repos
        .iter()
        .map(|r| r.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let shortcut = shortcut.map(|slot| keycap(format!("⌘{slot}"), theme));
    div()
        .flex()
        .flex_col()
        .gap(Theme::SPACE_1)
        .child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .child(
                    div()
                        .flex_none()
                        .size(px(8.0))
                        .rounded_full()
                        .bg(if running {
                            theme.success
                        } else {
                            theme.text_subtle
                        }),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(Theme::TEXT_LG)
                        .text_color(theme.text)
                        .child(space.title.clone()),
                )
                .child(Badge::new(shape_label(layout_shape(space.shape))).tone(Tone::Accent))
                .child(div().flex_1())
                .children(shortcut),
        )
        .child(
            div()
                .truncate()
                .text_size(Theme::TEXT_SM)
                .text_color(theme.text_muted)
                .child(format!(
                    "{} · {} · :{} · {}",
                    space.id,
                    space.branch(),
                    space.port,
                    repos
                )),
        )
}

/// A key chord drawn as a small keycap.
fn keycap(keys: String, theme: &Theme) -> Div {
    div()
        .flex_none()
        .px(Theme::SPACE_1)
        .rounded(Theme::RADIUS_SM)
        .border_1()
        .border_color(theme.border)
        .bg(theme.raised)
        .text_size(Theme::TEXT_XS)
        .text_color(theme.meta)
        .child(keys)
}

fn notice(message: String, color: gpui::Hsla, theme: &Theme) -> Div {
    div()
        .px(Theme::SPACE_3)
        .py(Theme::SPACE_2)
        .rounded(Theme::RADIUS_MD)
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface)
        .text_size(Theme::TEXT_SM)
        .text_color(color)
        .child(message)
}

fn section_title(title: &'static str, theme: &Theme) -> Div {
    div()
        .text_size(Theme::TEXT_XS)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text_subtle)
        .child(title)
}
