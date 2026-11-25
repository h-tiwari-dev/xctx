//! "+ Repo" in a space: the user adds another repo (an indexed name or a folder path) to an open space. It gets a
//! worktree on the space's branch and its mise file, a writer in the space's graph, and an agent pane started with
//! that writer's brief. It runs `xctx_core::widen` directly: the user asked for it, so no Inbox gate is needed (an
//! agent's request to widen still waits in the Inbox).

use gpui::{AnyElement, Context, Entity, Focusable, ParentElement, Window, prelude::*};
use xctx_core::{SpaceId, WidenReport, WidenRequest};
use xctx_graph::AgentNodeId;
use xctx_ui::{Button, ButtonVariant, PromptEvent, PromptInput, Theme};

use crate::dialogs::Dialog;
use crate::inbox_view::{repo_from_input, widen_message};
use crate::lifecycle::home_dir;
use crate::space_graph::add_writer_lanes;
use crate::workspace::Workspace;

/// The open "+ Repo" dialog of a space.
pub(crate) struct AddRepoState {
    /// The space the repo is added to.
    pub(crate) space: SpaceId,
    /// Where the repo is typed: an indexed repo's name or a folder path.
    input: Entity<PromptInput>,
    /// Why the last attempt failed.
    error: Option<String>,
    /// Whether the worktree is being made.
    pub(crate) running: bool,
}

impl Workspace {
    /// Open the "+ Repo" dialog for the space being shown. Refused while another dialog's job runs, the space is
    /// being shipped or discarded, or the store is busy.
    pub(crate) fn open_add_repo_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space) = self.active_space_id() else {
            return;
        };
        if self.dialog.as_ref().is_some_and(Dialog::is_running) {
            return;
        }
        if self.in_flight_spaces.contains(&space) {
            self.info(format!("{space} is being shipped or discarded"), cx);
            return;
        }
        let input = cx.new(|cx| {
            PromptInput::new(cx)
                .placeholder("An indexed repo's name, or a folder path such as ~/code/web")
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &PromptEvent, window, cx| match event {
                PromptEvent::Submit(value) => this.confirm_add_repo(value.clone(), window, cx),
                PromptEvent::Cancel => this.dismiss_dialog(window, cx),
            },
        );
        self.dialog_subscription = Some(subscription);
        let focus = input.focus_handle(cx);
        self.dialog = Some(Dialog::AddRepo(AddRepoState {
            space,
            input,
            error: None,
            running: false,
        }));
        focus.focus(window, cx);
        cx.notify();
    }

    /// Add the repo typed in the dialog to its space in the background, then give it a writer and an agent pane.
    fn confirm_add_repo(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::AddRepo(state)) = self.dialog.as_mut() else {
            return;
        };
        if state.running {
            return;
        }
        let space = state.space.clone();
        let repo = match repo_from_input(&self.index, &value, home_dir().as_deref()) {
            Ok(repo) => repo,
            Err(e) => {
                state.error = Some(e);
                cx.notify();
                return;
            }
        };
        let name = repo.name.clone();
        let Some(mut store) = self.take_store(&format!("adding {name} to {space}"), cx) else {
            return;
        };
        if let Some(Dialog::AddRepo(state)) = self.dialog.as_mut() {
            state.running = true;
            state.error = None;
        }
        let executor = cx.background_executor().clone();
        let job_space = space.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (store, result) = executor
                .spawn(async move {
                    let request = WidenRequest {
                        repos: vec![repo],
                        production: false,
                    };
                    let result = xctx_core::widen(&mut store, &job_space, request);
                    (store, result)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.return_store(store, cx);
                match result {
                    Ok(report) => {
                        if matches!(&this.dialog, Some(Dialog::AddRepo(s)) if s.space == space) {
                            this.dialog = None;
                            this.dialog_subscription = None;
                        }
                        this.repo_added(&space, &report, window, cx);
                    }
                    Err(e) => {
                        let message = match e {
                            xctx_core::Error::EmptyWiden(_) => {
                                format!("{space} already has {name}")
                            }
                            other => format!("could not add {name}: {other}"),
                        };
                        match this.dialog.as_mut() {
                            Some(Dialog::AddRepo(state)) => {
                                state.running = false;
                                state.error = Some(message);
                            }
                            _ => this.error(message, cx),
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Repos were added to space `id`: refresh its tab, give each a writer, start the writers that have no pane
    /// when the space is open, and check the space with mise again.
    fn repo_added(
        &mut self,
        id: &SpaceId,
        report: &WidenReport,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(space) = self.spaces.iter().find(|s| &s.id == id).cloned() else {
            return;
        };
        let open = match self.tabs.get_mut(id) {
            Some(tab) => {
                tab.space = space.clone();
                true
            }
            None => false,
        };
        if let Err(e) = self
            .graphs
            .load(&space)
            .map_err(xctx_control::RpcError::internal)
            .and_then(|state| add_writer_lanes(state, id, &report.added_repos))
        {
            self.error(e.message, cx);
        }
        if open {
            let writers: Vec<AgentNodeId> = self
                .graphs
                .get(id)
                .map(|state| {
                    state
                        .graph()
                        .writers()
                        .filter(|w| w.pane_id.is_none())
                        .filter(|w| {
                            report
                                .added_repos
                                .iter()
                                .any(|r| w.role.repo() == Some(r.name.as_str()))
                        })
                        .map(|w| w.id.clone())
                        .collect()
                })
                .unwrap_or_default();
            for writer in writers {
                if let Err(e) = self.start_agent_pane(id, &writer, window, cx) {
                    self.show_op_error(e, cx);
                }
            }
        }
        self.check_mise(vec![space], cx);
        self.info(widen_message(id, report, false), cx);
    }

    /// The "+ Repo" dialog's panel.
    pub(crate) fn render_add_repo(
        &self,
        state: &AddRepoState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx);
        let space = &state.space;
        let running = state.running;
        crate::dialogs::panel(&theme)
            .child(crate::dialogs::heading(format!("Add a repo to {space}")))
            .child(crate::dialogs::text(
                format!(
                    "Makes a worktree on space/{space} in that repo with the space's PORT and MISE_ENV, adds its \
                     writer to the graph and starts the writer's agent with the space's task."
                ),
                &theme,
            ))
            .child(state.input.clone())
            .children(
                state
                    .error
                    .clone()
                    .map(|e| crate::dialogs::error_text(e, &theme)),
            )
            .child(
                crate::dialogs::actions()
                    .child(
                        Button::new("cancel-add-repo", "Cancel")
                            .variant(ButtonVariant::Ghost)
                            .disabled(running)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.dismiss_dialog(window, cx)),
                            ),
                    )
                    .child(
                        Button::new(
                            "confirm-add-repo",
                            if running { "Adding…" } else { "Add repo" },
                        )
                        .variant(ButtonVariant::Primary)
                        .disabled(running)
                        .on_click(cx.listener(|this, _, window, cx| {
                            let value = match &this.dialog {
                                Some(Dialog::AddRepo(state)) => {
                                    state.input.read(cx).value().to_owned()
                                }
                                _ => return,
                            };
                            this.confirm_add_repo(value, window, cx);
                        })),
                    ),
            )
            .into_any_element()
    }
}
