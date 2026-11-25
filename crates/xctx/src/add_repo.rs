//! "+ Repo" in a space: the user picks one or more repos (from the indexed ones, by typing to search, or with
//! "Choose folders…") and adds them to an open space in one go. Each gets a worktree on the space's branch and its
//! mise file, a writer in the space's graph, and an agent pane started with that writer's brief. It runs
//! `xctx_core::widen` directly: the user asked for it, so no Inbox gate is needed (an agent's request to widen
//! still waits in the Inbox).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, Context, Entity, Focusable, ParentElement, PathPromptOptions, Subscription, Window,
    prelude::*,
};
use xctx_core::{DraftRepo, SpaceId, WidenReport, WidenRequest};
use xctx_graph::AgentNodeId;
use xctx_ui::{Button, ButtonVariant, PromptEvent, PromptInput, RepoChoice, RepoPicker, Theme};

use crate::dialogs::Dialog;
use crate::inbox_view::{repo_from_input, widen_message};
use crate::lifecycle::{ADDED_BY_HAND, home_dir, repo_key, repos_in_folders};
use crate::space_graph::add_writer_lanes;
use crate::workspace::Workspace;

/// The most indexed repos listed under the search at once.
const CHOICE_LIMIT: usize = 40;

/// What is done with the repos found in folders the user chose, once they are known.
type FoldersChosen = Box<
    dyn FnOnce(&mut Workspace, Vec<DraftRepo>, Vec<String>, &mut Window, &mut Context<Workspace>),
>;

/// The open "+ Repo" dialog of a space.
pub(crate) struct AddRepoState {
    /// The space the repos are added to.
    pub(crate) space: SpaceId,
    /// The search over indexed repos, or a folder path.
    input: Entity<PromptInput>,
    /// The repos picked so far, in the order picked.
    picked: Vec<DraftRepo>,
    /// Why the last attempt failed.
    error: Option<String>,
    /// Whether the worktrees are being made.
    pub(crate) running: bool,
    /// Redraws the list as the search changes.
    _observe: Subscription,
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
                .placeholder("Search your repos, or type a folder such as ~/code/web")
                .submit_blank(true)
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &PromptEvent, window, cx| match event {
                PromptEvent::Submit(value) => this.submit_add_repo(value.clone(), window, cx),
                PromptEvent::Cancel => this.dismiss_dialog(window, cx),
            },
        );
        let observe = cx.observe(&input, |_, _, cx| cx.notify());
        self.dialog_subscription = Some(subscription);
        let focus = input.focus_handle(cx);
        self.dialog = Some(Dialog::AddRepo(AddRepoState {
            space,
            input,
            picked: Vec::new(),
            error: None,
            running: false,
            _observe: observe,
        }));
        focus.focus(window, cx);
        cx.notify();
    }

    /// The resolved paths (see [`repo_key`]) of space `id`'s repos.
    fn space_repo_keys(&self, id: &SpaceId) -> HashSet<PathBuf> {
        self.spaces
            .iter()
            .find(|s| &s.id == id)
            .map(|s| s.repos.iter().map(|r| repo_key(&r.repo)).collect())
            .unwrap_or_default()
    }

    /// Enter in the dialog: typed text picks the best matching repo (or the folder typed); blank text adds what
    /// was picked.
    fn submit_add_repo(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        if value.trim().is_empty() {
            self.confirm_add_repo(window, cx);
            return;
        }
        let Some(Dialog::AddRepo(state)) = &self.dialog else {
            return;
        };
        if state.running {
            return;
        }
        let mut taken = self.space_repo_keys(&state.space);
        taken.extend(state.picked.iter().map(|r| repo_key(&r.path)));
        let found = repo_from_input(&self.index, &value, home_dir().as_deref(), |path| {
            taken.contains(&repo_key(path))
        });
        match found {
            Ok(repo) => self.pick_repos(vec![repo], Vec::new(), cx),
            Err(e) => self.set_add_repo_error(Some(e), cx),
        }
    }

    /// Pick or unpick the repo at `path` (as a row shows it).
    fn toggle_add_repo(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(Dialog::AddRepo(state)) = self.dialog.as_mut() else {
            return;
        };
        if state.running {
            return;
        }
        let before = state.picked.len();
        state.picked.retain(|r| r.path.to_string_lossy() != path);
        if state.picked.len() != before {
            state.error = None;
            cx.notify();
            return;
        }
        self.pick_repos(
            vec![DraftRepo::new(PathBuf::from(path), ADDED_BY_HAND)],
            Vec::new(),
            cx,
        );
    }

    /// Add `repos` to what the dialog has picked, skipping the space's own repos and repeats, and clear the
    /// search. `problems` (folders with no repo) and repos the space has already are shown as the error.
    fn pick_repos(
        &mut self,
        repos: Vec<DraftRepo>,
        mut problems: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(Dialog::AddRepo(state)) = &self.dialog else {
            return;
        };
        let space = state.space.clone();
        let in_space = self.space_repo_keys(&space);
        let mut picked: HashSet<PathBuf> = state.picked.iter().map(|r| repo_key(&r.path)).collect();
        let (new, present): (Vec<DraftRepo>, Vec<DraftRepo>) = repos
            .into_iter()
            .partition(|r| !in_space.contains(&repo_key(&r.path)));
        if !present.is_empty() {
            let names: Vec<&str> = present.iter().map(|r| r.name.as_str()).collect();
            problems.push(format!("{space} already has {}", names.join(", ")));
        }
        let Some(Dialog::AddRepo(state)) = self.dialog.as_mut() else {
            return;
        };
        for repo in new {
            if picked.insert(repo_key(&repo.path)) {
                state.picked.push(repo);
            }
        }
        state.error = (!problems.is_empty()).then(|| problems.join("; "));
        state.input.update(cx, |input, cx| input.clear(cx));
        cx.notify();
    }

    fn set_add_repo_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        if let Some(Dialog::AddRepo(state)) = self.dialog.as_mut() {
            state.error = error;
            cx.notify();
        }
    }

    /// Show the folder chooser (several folders at once), find the repos in what was chosen off the UI thread,
    /// then hand them to `done` with the folders that held none. Nothing happens when the chooser is cancelled.
    pub(crate) fn choose_repo_folders(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: FoldersChosen,
    ) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Add repos".into()),
        });
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            let folders = match chosen.await {
                Ok(Ok(Some(folders))) => folders,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(e)) => {
                    let _ = this.update(cx, |this, cx| {
                        this.error(format!("could not choose folders: {e}"), cx);
                    });
                    return;
                }
            };
            let (repos, problems) = executor
                .spawn(async move { repos_in_folders(&folders) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                done(this, repos, problems, window, cx);
            });
        })
        .detach();
    }

    /// "Choose folders…" in the dialog: pick every repo in the folders chosen.
    fn browse_add_repo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(&self.dialog, Some(Dialog::AddRepo(state)) if !state.running) {
            return;
        }
        self.choose_repo_folders(
            window,
            cx,
            Box::new(|this, repos, problems, _, cx| this.pick_repos(repos, problems, cx)),
        );
    }

    /// Add the repos picked in the dialog to its space in the background, then give each a writer and an agent
    /// pane.
    fn confirm_add_repo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::AddRepo(state)) = self.dialog.as_mut() else {
            return;
        };
        if state.running {
            return;
        }
        if state.picked.is_empty() {
            state.error =
                Some("pick a repo first: click one, or type its name and press Enter".to_owned());
            cx.notify();
            return;
        }
        let space = state.space.clone();
        let repos = state.picked.clone();
        let names: Vec<String> = repos.iter().map(|r| r.name.clone()).collect();
        let names = names.join(", ");
        let Some(mut store) = self.take_store(&format!("adding {names} to {space}"), cx) else {
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
                        repos,
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
                                format!("{space} already has {names}")
                            }
                            other => format!("could not add {names}: {other}"),
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

    /// The rows of the dialog: what is picked, then the indexed repos matching the search that the space does not
    /// have yet.
    fn add_repo_choices(&self, state: &AddRepoState, query: &str) -> Vec<RepoChoice> {
        let choice = |name: &str, path: &Path, picked: bool| RepoChoice {
            name: name.to_owned().into(),
            path: path.to_string_lossy().into_owned().into(),
            picked,
        };
        let mut choices: Vec<RepoChoice> = state
            .picked
            .iter()
            .map(|r| choice(&r.name, &r.path, true))
            .collect();
        let mut taken = self.space_repo_keys(&state.space);
        taken.extend(state.picked.iter().map(|r| repo_key(&r.path)));
        choices.extend(
            self.index
                .lookup(query, usize::MAX)
                .into_iter()
                .filter(|r| !taken.contains(&repo_key(&r.path)))
                .take(CHOICE_LIMIT)
                .map(|r| choice(&r.name, &r.path, false)),
        );
        choices
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
        let query = state.input.read(cx).value().trim().to_owned();
        let choices = self.add_repo_choices(state, &query);
        let empty = if self.index.repos().is_empty() {
            "No repos indexed yet. Choose folders below, type a folder path, or index a folder on Home."
                .to_owned()
        } else if query.is_empty() {
            format!("Every indexed repo is in {space} already. Choose folders to add others.")
        } else if query.contains('/') || query.starts_with('~') {
            "Press Enter to pick the repo in this folder.".to_owned()
        } else {
            format!("No indexed repo matches \"{query}\". Type its folder, or choose folders.")
        };
        let count = state.picked.len();
        let confirm_label = match (running, count) {
            (true, _) => "Adding…".to_owned(),
            (false, 0) => "Add repos".to_owned(),
            (false, 1) => "Add 1 repo".to_owned(),
            (false, n) => format!("Add {n} repos"),
        };
        crate::dialogs::panel(&theme)
            .child(crate::dialogs::heading(format!("Add repos to {space}")))
            .child(crate::dialogs::text(
                format!(
                    "Pick as many as you need: click a repo, or type to search and press Enter for the top \
                     match. Each gets a worktree on space/{space} with the space's PORT and MISE_ENV, and its own \
                     writer agent with the space's task."
                ),
                &theme,
            ))
            .child(state.input.clone())
            .child(
                RepoPicker::new("add-repo", choices)
                    .empty(empty)
                    .disabled(running)
                    .on_toggle(cx.listener(|this, path: &gpui::SharedString, _, cx| {
                        this.toggle_add_repo(path, cx);
                    }))
                    .on_browse({
                        let this = cx.entity().downgrade();
                        move |window, cx| {
                            let _ = this.update(cx, |this, cx| this.browse_add_repo(window, cx));
                        }
                    }),
            )
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
                        Button::new("confirm-add-repo", confirm_label)
                            .variant(ButtonVariant::Primary)
                            .selector("confirm-add-repo")
                            .disabled(running || count == 0)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.confirm_add_repo(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }
}
