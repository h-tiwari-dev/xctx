//! Home: the Parent Agent you tell what you are working on, the space it drafts (launched with ⌘↵), the
//! Inbox, the living spaces, and the indexed folders.

use std::path::PathBuf;

use gpui::{
    AnyElement, Context, Div, FontWeight, IntoElement, ParentElement, Styled, Window, div,
    prelude::*,
};
use xctx_core::{Space, SpaceDraft};
use xctx_ui::{
    Badge, Button, ButtonSize, ButtonVariant, ParentAgentDrawer, Theme, Tone, shape_label,
};

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

    /// Home's content.
    pub(crate) fn render_home(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx);
        let mut column = div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_6)
            .w_full()
            .max_w(gpui::px(880.0))
            .px(Theme::SPACE_6)
            .py(Theme::SPACE_6)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(Theme::SPACE_2)
                    .child(
                        div()
                            .text_size(Theme::TEXT_XL)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("What are you working on?"),
                    )
                    .child(
                        div()
                            .text_size(Theme::TEXT_SM)
                            .text_color(theme.text_subtle)
                            .child("Tell the Parent Agent. It drafts a space from your indexed repos and shows it here; ⌘↵ launches it."),
                    )
                    .child(self.render_parent_agent(cx)),
            );
        if let Some(plan) = self.render_plan_review(cx) {
            column = column.child(plan);
        }
        if let Some(error) = &self.store_error {
            column = column.child(notice(error.clone(), theme.danger, &theme));
        }
        if let Some(busy) = &self.busy {
            column = column.child(notice(format!("{busy}…"), theme.text_muted, &theme));
        }
        column = column
            .child(self.render_inbox(cx))
            .child(self.render_spaces(cx))
            .child(self.render_roots(cx));
        div()
            .id("home")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .justify_center()
            .child(column)
            .into_any_element()
    }

    fn render_parent_agent(&self, cx: &mut Context<Self>) -> ParentAgentDrawer {
        let restart = cx.entity().downgrade();
        let mut drawer =
            ParentAgentDrawer::new(self.parent.status.clone()).on_restart(move |window, cx| {
                let _ = restart.update(cx, |this, cx| this.start_parent_agent(window, cx));
            });
        if let Some(view) = &self.parent.view {
            drawer = drawer.body(view.clone());
        }
        drawer
    }

    fn render_spaces(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx);
        let mut list = div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_2)
            .child(section_title("Living spaces", &theme));
        if self.spaces.is_empty() {
            list = list.child(
                div()
                    .text_color(theme.text_muted)
                    .child("No spaces yet. Tell the Parent Agent what you are working on."),
            );
        }
        for (index, space) in self.spaces.iter().enumerate() {
            let id = space.id.clone();
            let running = self
                .tabs
                .get(&space.id)
                .is_some_and(|tab| tab.agent_running(cx));
            list = list.child(
                card(&theme)
                    .id(("space-card", index))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.raised))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.show_space(&id, window, cx)),
                    )
                    .child(space_summary(space, running, index, &theme)),
            );
        }
        list
    }

    fn render_roots(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx);
        let roots = self.roots.clone();
        let status = if self.scanning {
            "indexing…".to_owned()
        } else {
            let count = self.index.repos().len();
            if count == 1 {
                "1 repo indexed".to_owned()
            } else {
                format!("{count} repos indexed")
            }
        };
        let mut list = div().flex().flex_col().gap(Theme::SPACE_2).child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .child(section_title("Indexed folders", &theme))
                .child(
                    div()
                        .flex_1()
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_subtle)
                        .child(status),
                )
                .child(
                    Button::new("rescan", "Rescan")
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Compact)
                        .disabled(self.scanning)
                        .on_click(cx.listener(|this, _, _, cx| this.rescan(cx))),
                ),
        );
        if roots.is_empty() {
            list = list.child(notice(
                "xctx finds repos in the folders you index. Add the folder that holds your repos, such as ~/code or ~/Desktop.".to_owned(),
                theme.warning,
                &theme,
            ));
        }
        for (index, root) in roots.into_iter().enumerate() {
            let label = root.display().to_string();
            list = list.child(
                row(&theme)
                    .child(div().flex_1().truncate().child(label))
                    .child(
                        Button::new(("remove-root", index), "Remove")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_root(root.clone(), cx)
                            })),
                    ),
            );
        }
        list.child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .child(div().flex_1().child(self.root_input.clone()))
                .child(
                    Button::new("add-root", "Add folder")
                        .variant(ButtonVariant::Secondary)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let input = this.root_input.read(cx).value().to_owned();
                            this.add_root(input, cx);
                        })),
                ),
        )
    }
}

/// A living space's card body: title, slug, shape, port, branch and repos.
fn space_summary(space: &Space, running: bool, index: usize, theme: &Theme) -> Div {
    let repos = space
        .repos
        .iter()
        .map(|r| r.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let shortcut = (index < 9).then(|| format!("⌘{}", index + 1));
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
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(Theme::TEXT_LG)
                        .child(space.title.clone()),
                )
                .child(Badge::new(shape_label(layout_shape(space.shape))).tone(Tone::Accent))
                .when(running, |row| {
                    row.child(Badge::new("running").tone(Tone::Success))
                })
                .child(div().flex_1())
                .children(shortcut.map(|keys| {
                    div()
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_subtle)
                        .child(keys)
                })),
        )
        .child(
            div()
                .text_size(Theme::TEXT_SM)
                .text_color(theme.text_muted)
                .child(format!(
                    "{}  ·  {}  ·  PORT {}  ·  {}",
                    space.id,
                    space.branch(),
                    space.port,
                    repos
                )),
        )
}

fn card(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(Theme::SPACE_2)
        .p(Theme::SPACE_4)
        .rounded(Theme::RADIUS_LG)
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface)
}

fn row(theme: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap(Theme::SPACE_2)
        .px(Theme::SPACE_3)
        .py(Theme::SPACE_2)
        .rounded(Theme::RADIUS_MD)
        .bg(theme.raised)
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
        .text_size(Theme::TEXT_LG)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text)
        .child(title)
}
