//! The Ship and Discard confirmations shown over a space, the ship summary, the "+ Repo" dialog's slot, and the
//! background jobs they run.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    AnyElement, Context, Entity, Focusable, FontWeight, IntoElement, ParentElement, Styled, Window,
    div, hsla, prelude::*,
};
use xctx_core::{ShipOptions, SpaceId, Store, git};
use xctx_ui::{Button, ButtonVariant, PromptEvent, PromptInput, Theme};

use crate::add_repo::AddRepoState;
use crate::attempt_ui::{ForkState, ReviewState};
use crate::attempts::delete_pre_adopt_refs;
use crate::keymap::DIALOG_CONTEXT;
use crate::lifecycle::{
    dirty_repos, discard_message, discard_space, ship_report_lines, ship_space,
};
use crate::plugin_menu_dialog::PluginsState;
use crate::space_graph::clean_scratch;
use crate::workspace::{View, Workspace, in_flight_message, wait_message};

/// An open dialog.
pub(crate) enum Dialog {
    /// Confirm shipping a space, with the pull request title to use.
    Ship {
        space: SpaceId,
        title: Entity<PromptInput>,
        gh: Option<std::path::PathBuf>,
        error: Option<String>,
        running: bool,
    },
    /// Confirm discarding a space; `dirty` lists repos with uncommitted changes once a discard refused.
    Discard {
        space: SpaceId,
        dirty: Option<Vec<String>>,
        error: Option<String>,
        running: bool,
    },
    /// What a ship did.
    Shipped { space: SpaceId, lines: Vec<String> },
    /// Fork a writer into competing attempts (see `attempt_ui.rs`). Its fork job does not keep it open.
    Fork(ForkState),
    /// Compare a round of attempts, and adopt or discard them (see `attempt_ui.rs`). Its jobs do not keep it
    /// open.
    Review(ReviewState),
    /// The space's Plugins menu (see `plugin_menu_dialog.rs`).
    Plugins(PluginsState),
    /// Add a repo to the space (see `add_repo.rs`).
    AddRepo(AddRepoState),
}

impl Dialog {
    fn space(&self) -> &SpaceId {
        match self {
            Dialog::Ship { space, .. }
            | Dialog::Discard { space, .. }
            | Dialog::Shipped { space, .. } => space,
            Dialog::Fork(fork) => &fork.space,
            Dialog::Review(review) => &review.space,
            Dialog::Plugins(plugins) => &plugins.space,
            Dialog::AddRepo(add) => &add.space,
        }
    }

    /// Whether the dialog's ship or discard is running.
    pub(crate) fn is_running(&self) -> bool {
        matches!(
            self,
            Dialog::Ship { running: true, .. }
                | Dialog::Discard { running: true, .. }
                | Dialog::AddRepo(AddRepoState { running: true, .. })
        )
    }

    /// What the dialog's running job is doing, such as "shipping fix-retry"; `None` when none is running.
    fn running_job(&self) -> Option<String> {
        match self {
            Dialog::Ship {
                space,
                running: true,
                ..
            } => Some(format!("shipping {space}")),
            Dialog::Discard {
                space,
                running: true,
                ..
            } => Some(format!("discarding {space}")),
            _ => None,
        }
    }

    fn set_outcome(&mut self, outcome: Option<String>, dirty_repos: Option<Vec<String>>) {
        match self {
            Dialog::Ship { error, running, .. } => {
                *error = outcome;
                *running = false;
            }
            Dialog::Discard {
                error,
                running,
                dirty,
                ..
            } => {
                *error = outcome;
                *running = false;
                if dirty_repos.is_some() {
                    *dirty = dirty_repos;
                }
            }
            Dialog::Shipped { .. }
            | Dialog::Fork(_)
            | Dialog::Review(_)
            | Dialog::Plugins(_)
            | Dialog::AddRepo(_) => {}
        }
    }
}

/// Why a ship or discard did not happen.
enum JobError {
    /// These repos have uncommitted changes.
    Dirty(Vec<String>),
    /// Anything else, as shown to the user.
    Failed(String),
}

impl From<xctx_core::Error> for JobError {
    fn from(error: xctx_core::Error) -> Self {
        match error {
            xctx_core::Error::DirtyWorktrees { repos } => JobError::Dirty(repos),
            other => JobError::Failed(other.to_string()),
        }
    }
}

impl JobError {
    fn message(&self, action: &str) -> String {
        match self {
            JobError::Dirty(repos) => format!(
                "cannot {action}: uncommitted changes in {}",
                repos.join(", ")
            ),
            JobError::Failed(message) => format!("{action} failed: {message}"),
        }
    }
}

impl Workspace {
    /// Whether the open dialog is about space `id`.
    fn dialog_is_for(&self, id: &SpaceId) -> bool {
        self.dialog
            .as_ref()
            .is_some_and(|dialog| dialog.space() == id)
    }

    /// When a Ship or Discard (`action`) dialog cannot open for space `id` now, say why and return `true`: another
    /// dialog's job is running, the space's own ship or discard is, or the space has live attempts (see
    /// [`Workspace::attempts_block_end`]).
    fn refuse_space_dialog(&mut self, id: &SpaceId, action: &str, cx: &mut Context<Self>) -> bool {
        if let Some(job) = self.dialog.as_ref().and_then(Dialog::running_job) {
            self.info(wait_message(&job), cx);
            return true;
        }
        if self.in_flight_spaces.contains(id) {
            self.info(in_flight_message(id), cx);
            return true;
        }
        if let Some(why) = self.attempts_block_end(id, action) {
            self.error(why, cx);
            return true;
        }
        false
    }

    /// Ask to ship the space being shown.
    pub(crate) fn open_ship(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((space, current_title)) = self
            .active_tab()
            .map(|tab| (tab.space.id.clone(), tab.space.title.clone()))
        else {
            return;
        };
        if self.refuse_space_dialog(&space, "ship", cx) {
            return;
        }
        let title = cx.new(|cx| {
            let mut input = PromptInput::new(cx).placeholder("Pull request title");
            input.set_value(&current_title, cx);
            input
        });
        let subscription = cx.subscribe_in(
            &title,
            window,
            |this, _, event: &PromptEvent, window, cx| match event {
                PromptEvent::Submit(_) => this.confirm_ship(window, cx),
                PromptEvent::Cancel => this.dismiss_dialog(window, cx),
            },
        );
        self.dialog_subscription = Some(subscription);
        let focus = title.focus_handle(cx);
        self.dialog = Some(Dialog::Ship {
            space,
            title,
            gh: git::find_gh(),
            error: None,
            running: false,
        });
        focus.focus(window, cx);
        cx.notify();
    }

    /// Ask to discard the space being shown.
    pub(crate) fn open_discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space) = self.active_tab().map(|tab| tab.space.id.clone()) else {
            return;
        };
        if self.refuse_space_dialog(&space, "discard", cx) {
            return;
        }
        self.dialog = Some(Dialog::Discard {
            space,
            dirty: None,
            error: None,
            running: false,
        });
        self.dialog_subscription = None;
        self.dialog_focus.focus(window, cx);
        cx.notify();
    }

    /// Close the dialog, unless its job is running.
    pub(crate) fn dismiss_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.dialog {
            None => return,
            Some(dialog) if dialog.is_running() => return,
            Some(_) => {}
        }
        self.dialog = None;
        self.dialog_subscription = None;
        match self.view {
            View::Home => self.focus_home(window, cx),
            View::Space(_) => self.focus_active_pane(window, cx),
        }
        cx.notify();
    }

    fn confirm_ship(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::Ship {
            space,
            title,
            gh,
            running,
            error,
        }) = self.dialog.as_mut()
        else {
            return;
        };
        if *running {
            return;
        }
        *running = true;
        *error = None;
        let id = space.clone();
        let pr_title = title.read(cx).value().to_owned();
        let options = ShipOptions { gh: gh.clone() };
        let busy = format!("shipping {id}");
        self.run_space_job(
            id.clone(),
            busy,
            true,
            move |store| ship_space(store, &id, Some(&pr_title), &options),
            |this, report, cx| {
                if this.dialog_is_for(&report.space) {
                    this.dialog = Some(Dialog::Shipped {
                        space: report.space.clone(),
                        lines: ship_report_lines(&report),
                    });
                    this.dialog_subscription = None;
                }
                this.info(format!("shipped {}", report.space), cx);
            },
            "ship",
            window,
            cx,
        );
    }

    fn confirm_discard(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::Discard {
            space,
            running,
            error,
            ..
        }) = self.dialog.as_mut()
        else {
            return;
        };
        if *running {
            return;
        }
        *running = true;
        *error = None;
        let id = space.clone();
        let busy = format!("discarding {id}");
        self.run_space_job(
            id.clone(),
            busy,
            !force,
            move |store| discard_space(store, &id, force),
            |this, report, cx| {
                if this.dialog_is_for(&report.space) {
                    this.dialog = None;
                    this.dialog_subscription = None;
                }
                this.info(discard_message(&report), cx);
            },
            "discard",
            window,
            cx,
        );
    }

    /// Ship or discard the space `id` in the background. With `check_dirty`, worktrees with uncommitted changes
    /// stop it before anything is closed. Otherwise the space's layout is saved, its terminals are stopped, the
    /// store is handed to `job`, and on success Home is shown and `done` runs; on failure the space's panes come
    /// back and the dialog shows the error. The space is in [`Workspace::in_flight_spaces`] until the job ends.
    #[allow(clippy::too_many_arguments)]
    fn run_space_job<T: Send + 'static>(
        &mut self,
        id: SpaceId,
        busy: String,
        check_dirty: bool,
        job: impl FnOnce(&mut Store) -> xctx_core::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        action: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Attempts may have been forked since the dialog opened (say, from the Inbox).
        if let Some(why) = self.attempts_block_end(&id, action) {
            self.fail_job(&id, JobError::Failed(why), action, cx);
            return;
        }
        let Some(space) = self.spaces.iter().find(|s| s.id == id).cloned() else {
            self.fail_job(
                &id,
                JobError::Failed(format!("{id} is not active")),
                action,
                cx,
            );
            return;
        };
        self.in_flight_spaces.insert(id.clone());
        let repos: Vec<PathBuf> = space.repos.iter().map(|r| r.repo.clone()).collect();
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            if check_dirty {
                let dirty = executor.spawn(async move { dirty_repos(&space) }).await;
                let refusal = match dirty {
                    Ok(repos) if repos.is_empty() => None,
                    Ok(repos) => Some(JobError::Dirty(repos)),
                    Err(e) => Some(JobError::from(e)),
                };
                if let Some(refusal) = refusal {
                    let _ = this.update(cx, |this, cx| {
                        this.in_flight_spaces.remove(&id);
                        this.fail_job(&id, refusal, action, cx)
                    });
                    return;
                }
            }
            let store_and_sessions = this.update(cx, |this, cx| {
                // Save the layout while the store is here: once it is taken, closing the tab would drop the save.
                this.persist(&id, cx);
                let store = this.take_store(&busy, cx)?;
                let sessions = this.close_space_tab(&id, cx);
                Some((store, sessions))
            });
            let Ok(Some((mut store, sessions))) = store_and_sessions else {
                let _ = this.update(cx, |this, cx| {
                    this.in_flight_spaces.remove(&id);
                    this.fail_job(
                        &id,
                        JobError::Failed("the store is busy".into()),
                        action,
                        cx,
                    )
                });
                return;
            };
            let space_id_for_err = id.clone();
            let (store, result) = executor
                .spawn(async move {
                    let mut all_exited = true;
                    for session in &sessions {
                        if !session.shutdown_and_wait(Duration::from_secs(2)) {
                            all_exited = false;
                        }
                    }
                    drop(sessions);
                    let result = if all_exited {
                        job(&mut store).map_err(JobError::from)
                    } else {
                        Err(JobError::Failed(format!(
                            "a terminal in {space_id_for_err} did not exit in time"
                        )))
                    };
                    (store, result)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.in_flight_spaces.remove(&id);
                if result.is_ok() {
                    // The tab was closed before the job; close it again in case anything reopened it since. The
                    // store is still away, so no layout is saved for a space that is gone.
                    let _ = this.close_space_tab(&id, cx);
                }
                this.return_store(store, cx);
                match result {
                    Ok(value) => {
                        if !this.spaces.iter().any(|s| s.id == id) {
                            this.space_ended(&id, repos, cx);
                        }
                        done(this, value, cx);
                        if this.dialog.is_none() {
                            this.focus_home(window, cx);
                        } else {
                            this.dialog_focus.focus(window, cx);
                        }
                    }
                    Err(e) => {
                        // The space is still active: bring its panes back, agents resuming.
                        if this.view == View::Home {
                            this.show_space(&id, window, cx);
                        } else {
                            this.open_space(&id, window, cx);
                        }
                        this.fail_job(&id, e, action, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The space `id` was shipped or discarded: drop its agent graph, attempts and plugins from memory, and in the
    /// background remove its scratch directory (best effort: a leftover directory under `/tmp` harms nothing), its
    /// plugins' files (`spaces/<slug>/plugins.json` and its browser profile `browser-profiles/<slug>`), and the
    /// backups adopting attempts kept in its `repos` (`refs/xctx/pre-adopt/<slug>/*`), saying which could not be
    /// removed.
    fn space_ended(&mut self, id: &SpaceId, repos: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.graphs.forget(id);
        self.attempts.forget(id);
        self.unsaved_adoptions.remove(id);
        if let Some(browser) = self.plugins.stop_space(id) {
            // Normally closed with the tab already; close it before its profile goes.
            browser.close_if_idle();
        }
        let plugin_files = self.plugins.forget_space(id);
        let slug = id.as_str().to_owned();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let failures: Vec<String> = executor
                .spawn(async move {
                    let _ = clean_scratch(&slug);
                    let mut failures: Vec<String> = repos
                        .iter()
                        .filter_map(|repo| delete_pre_adopt_refs(repo, &slug).err())
                        .collect();
                    if let Err(e) = crate::plugin_host::remove_paths(&plugin_files) {
                        failures.push(e);
                    }
                    failures
                })
                .await;
            if !failures.is_empty() {
                let _ = this.update(cx, |this, cx| this.error(failures.join("; "), cx));
            }
        })
        .detach();
    }

    fn fail_job(&mut self, id: &SpaceId, error: JobError, action: &str, cx: &mut Context<Self>) {
        let message = error.message(action);
        if let Some(dialog) = self.dialog.as_mut()
            && dialog.space() == id
        {
            let dirty = match error {
                JobError::Dirty(repos) => Some(repos),
                JobError::Failed(_) => None,
            };
            dialog.set_outcome(Some(message.clone()), dirty);
        }
        self.error(message, cx);
    }

    /// The open dialog over a dimmed backdrop, if one is open.
    pub(crate) fn render_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.dialog.as_ref()?;
        let theme = Theme::of(cx);
        let body: AnyElement = match dialog {
            Dialog::Fork(fork) => self
                .render_fork_dialog(fork, cx)
                .key_context(DIALOG_CONTEXT)
                .track_focus(&self.dialog_focus)
                .into_any_element(),
            Dialog::Review(review) => self
                .render_review_sheet(review, cx)
                .key_context(DIALOG_CONTEXT)
                .track_focus(&self.dialog_focus)
                .into_any_element(),
            Dialog::Plugins(plugins) => self
                .render_plugins_menu(plugins, cx)
                .key_context(DIALOG_CONTEXT)
                .track_focus(&self.dialog_focus)
                .into_any_element(),
            Dialog::AddRepo(add) => div()
                .key_context(DIALOG_CONTEXT)
                .track_focus(&self.dialog_focus)
                .child(self.render_add_repo(add, cx))
                .into_any_element(),
            _ => self
                .render_confirmation(dialog, &theme, cx)
                .key_context(DIALOG_CONTEXT)
                .track_focus(&self.dialog_focus)
                .into_any_element(),
        };
        Some(
            div()
                .id("dialog-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(hsla(0.0, 0.0, 0.0, 0.55))
                .occlude()
                .child(body)
                .into_any_element(),
        )
    }

    /// The panel of a Ship, Discard or Shipped dialog.
    fn render_confirmation(
        &self,
        dialog: &Dialog,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let theme = *theme;
        match dialog {
            Dialog::Ship {
                space,
                title,
                gh,
                error,
                running,
            } => {
                let repos = self
                    .spaces
                    .iter()
                    .find(|s| &s.id == space)
                    .map(|s| {
                        s.repos
                            .iter()
                            .map(|r| r.name.clone())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                let pr_note = match gh {
                    Some(_) => "Opens a pull request in each pushed repo with gh, titled:",
                    None => {
                        "gh was not found, so branches are pushed and no pull request is opened. Title:"
                    }
                };
                panel(&theme)
                    .child(heading(format!("Ship {space}?")))
                    .child(text(
                        format!(
                            "Pushes space/{space} in {repos} where it has new commits, then stops this space's terminals, removes its worktrees and keeps the branches."
                        ),
                        &theme,
                    ))
                    .child(text(pr_note.to_owned(), &theme))
                    .child(title.clone())
                    .children(error.clone().map(|e| error_text(e, &theme)))
                    .child(
                        actions()
                            .child(
                                Button::new("cancel-ship", "Cancel")
                                    .variant(ButtonVariant::Ghost)
                                    .disabled(*running)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.dismiss_dialog(window, cx)
                                    })),
                            )
                            .child(
                                Button::new(
                                    "confirm-ship",
                                    if *running { "Shipping…" } else { "Ship" },
                                )
                                .variant(ButtonVariant::Primary)
                                .disabled(*running)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_ship(window, cx)
                                })),
                            ),
                    )
            }
            Dialog::Discard {
                space,
                dirty,
                error,
                running,
            } => {
                let mut panel = panel(&theme)
                    .child(heading(format!("Discard {space}?")))
                    .child(text(
                        format!(
                            "Stops this space's terminals and removes its worktrees and mise files. The space/{space} branches are kept."
                        ),
                        &theme,
                    ));
                if let Some(repos) = dirty {
                    panel = panel.child(error_text(
                        format!(
                            "Uncommitted changes in {}. Force discard to throw them away.",
                            repos.join(", ")
                        ),
                        &theme,
                    ));
                } else if let Some(e) = error {
                    panel = panel.child(error_text(e.clone(), &theme));
                }
                let mut buttons = actions().child(
                    Button::new("cancel-discard", "Cancel")
                        .variant(ButtonVariant::Ghost)
                        .disabled(*running)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.dismiss_dialog(window, cx)),
                        ),
                );
                if dirty.is_some() {
                    buttons = buttons.child(
                        Button::new("force-discard", "Force discard uncommitted changes")
                            .variant(ButtonVariant::Danger)
                            .disabled(*running)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm_discard(true, window, cx)
                            })),
                    );
                } else {
                    buttons = buttons.child(
                        Button::new(
                            "confirm-discard",
                            if *running { "Discarding…" } else { "Discard" },
                        )
                        .variant(ButtonVariant::Danger)
                        .disabled(*running)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm_discard(false, window, cx)
                        })),
                    );
                }
                panel.child(buttons)
            }
            Dialog::Shipped { space, lines } => {
                let mut panel = panel(&theme).child(heading(format!("Shipped {space}")));
                if lines.is_empty() {
                    panel = panel.child(text("Nothing to report.".to_owned(), &theme));
                }
                for line in lines {
                    panel = panel.child(text(line.clone(), &theme));
                }
                panel.child(
                    actions().child(
                        Button::new("close-ship-report", "Done")
                            .variant(ButtonVariant::Primary)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.dismiss_dialog(window, cx)),
                            ),
                    ),
                )
            }
            // Drawn by `render_dialog` itself.
            Dialog::Fork(_) | Dialog::Review(_) | Dialog::Plugins(_) | Dialog::AddRepo(_) => {
                panel(&theme)
            }
        }
    }
}

pub(crate) fn panel(theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(Theme::SPACE_3)
        .w(gpui::px(520.0))
        .p(Theme::SPACE_6)
        .rounded(Theme::RADIUS_LG)
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface)
        .shadow_lg()
}

pub(crate) fn heading(title: String) -> gpui::Div {
    div()
        .text_size(Theme::TEXT_LG)
        .font_weight(FontWeight::SEMIBOLD)
        .child(title)
}

pub(crate) fn text(message: String, theme: &Theme) -> gpui::Div {
    div()
        .text_size(Theme::TEXT_SM)
        .text_color(theme.text_muted)
        .child(message)
}

pub(crate) fn error_text(message: String, theme: &Theme) -> gpui::Div {
    div()
        .text_size(Theme::TEXT_SM)
        .text_color(theme.danger)
        .child(message)
}

pub(crate) fn actions() -> gpui::Div {
    div().flex().justify_end().gap(Theme::SPACE_2)
}
