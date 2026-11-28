//! The workspace side of agent panes whose runner speaks RPC (`interface = "rpc"`): starting their omp off the UI
//! thread, carrying its frames into the space's graph and the Inbox, starting a crashed one again on its session,
//! and Open in terminal, which hands the session to `omp --resume` in a terminal pane.
//!
//! The pane draws the session with `xctx-omp-view`'s [`OmpView`]: transcript, tool cards and a composer whose
//! messages go to the agent's omp.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    AnyElement, AppContext, Context, Entity, IntoElement, ParentElement, Styled, Subscription,
    Window, div,
};
use xctx_core::{Redactor, SpaceId};
use xctx_graph::AgentNodeId;
use xctx_layout::{PaneId, PaneKind, RolePane, SplitAxis};
use xctx_omp::{Frame, Incoming, list_sessions_in_dir};
use xctx_omp_view::changes::diff_counts;
use xctx_omp_view::{
    ChangeKind, ChangedFile, ModelChoice, OmpIntent, OmpView, SessionChanges, SessionEntry,
};
use xctx_plugin::RunnerManifest;
use xctx_term_view::TerminalView;
use xctx_ui::Theme;

use crate::omp_agent::{
    AgentStatus, AgentUpdate, OmpAgent, Opening, agent_session_dir, session_lease_held,
};
use crate::plugin_host::{GitChange, PreviewText, file_diff, git_status_files};
use crate::space_tab::PaneTerminal;
use crate::spawn::{AgentStart, pane_worktree, rpc_launch, terminal_resume_launch};
use crate::workspace::Workspace;

/// The most files a pane's `@file` completion indexes of its worktree.
const FILE_INDEX_CAP: usize = 50_000;

/// The most `@file` matches offered at once.
const FILE_MATCH_CAP: usize = 20;

/// The files git knows in `root` (tracked, or untracked and not ignored), relative with `/`, capped at
/// [`FILE_INDEX_CAP`]. Empty when `root` is not a git worktree. Blocks: it runs on the background executor.
fn worktree_files(root: &Path) -> Vec<String> {
    let Ok(out) = std::process::Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    out.stdout
        .split(|b| *b == 0)
        .filter(|path| !path.is_empty())
        .take(FILE_INDEX_CAP)
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect()
}

/// The `@file` matches for `query` among `files`: those containing it (ignoring case), the ones whose file name
/// starts with it first, then the shorter paths. At most [`FILE_MATCH_CAP`].
fn file_matches(files: &[String], query: &str) -> Vec<gpui::SharedString> {
    let query = query.to_lowercase();
    let mut hits: Vec<&String> = files
        .iter()
        .filter(|path| path.to_lowercase().contains(&query))
        .collect();
    hits.sort_by_key(|path| {
        let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
        (!name.starts_with(&query), path.len())
    });
    hits.into_iter()
        .take(FILE_MATCH_CAP)
        .map(|path| gpui::SharedString::from(path.clone()))
        .collect()
}

/// The most changed files collected for one in-session Changes snapshot.
const SESSION_CHANGES_CAP: usize = 60;

fn map_change_kind(change: GitChange) -> ChangeKind {
    match change {
        GitChange::Modified => ChangeKind::Modified,
        GitChange::Added => ChangeKind::Added,
        GitChange::Deleted => ChangeKind::Deleted,
        GitChange::Renamed => ChangeKind::Renamed,
        GitChange::Untracked => ChangeKind::Untracked,
        GitChange::Conflicted => ChangeKind::Conflicted,
    }
}

/// Collect uncommitted git changes and redacted unified diffs for `root`. Blocks: runs on the background executor.
fn worktree_changes(root: &Path, branch: Option<String>, redactor: &Redactor) -> SessionChanges {
    let entries = match git_status_files(root) {
        Ok(entries) => entries,
        Err(err) => {
            return SessionChanges {
                branch: branch.map(Into::into),
                files: Vec::new(),
                error: Some(err.into()),
            };
        }
    };
    let mut files = Vec::with_capacity(entries.len().min(SESSION_CHANGES_CAP));
    for (path, change) in entries.into_iter().take(SESSION_CHANGES_CAP) {
        let diff = match file_diff(root, &path, change, redactor) {
            Ok(PreviewText::Text { text, .. }) => text,
            Ok(PreviewText::Binary) => "Binary file differs.".to_owned(),
            Err(err) => format!("Could not read diff: {err}"),
        };
        let (additions, deletions) = diff_counts(&diff);
        files.push(ChangedFile {
            path: path.into(),
            kind: map_change_kind(change),
            additions,
            deletions,
            diff: diff.into(),
        });
    }
    SessionChanges {
        branch: branch.map(Into::into),
        files,
        error: None,
    }
}

/// Format an epoch millisecond timestamp into a compact relative label (`just now`, `5m ago`, `2h ago`, `3d ago`).
fn format_updated_label(updated_ms: u64, now_ms: u64) -> String {
    if updated_ms == 0 || updated_ms >= now_ms {
        return "just now".to_owned();
    }
    let secs = (now_ms - updated_ms) / 1000;
    if secs < 60 {
        "just now".to_owned()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Scan `session_dir` for persisted sessions and map them into redacted [`SessionEntry`] rows.
/// Blocks: runs on the background executor.
fn sessions_for_pane(
    session_dir: &Path,
    active_id: Option<&str>,
    active_file: Option<&str>,
    redactor: &Redactor,
) -> Vec<SessionEntry> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    list_sessions_in_dir(session_dir)
        .into_iter()
        .map(|s| {
            let path_str = s.path.to_string_lossy().into_owned();
            let active = active_id.is_some_and(|id| id == s.id)
                || active_file.is_some_and(|f| f == path_str);
            let display_title = redactor.redact(&s.display_title()).into_owned();
            let preview = s
                .preview
                .as_deref()
                .map(|p| redactor.redact(p).into_owned())
                .filter(|p| !p.is_empty() && *p != display_title)
                .map(Into::into);
            SessionEntry {
                id: s.id.into(),
                path: path_str.into(),
                title: display_title.into(),
                preview,
                message_count: s.message_count,
                updated_label: format_updated_label(s.updated_ms, now_ms).into(),
                active,
            }
        })
        .collect()
}

/// Numbers each start of an RPC agent's child, so frames of a child that was replaced are dropped.
static GENERATION: AtomicU64 = AtomicU64::new(1);

/// An agent pane running its agent over RPC.
pub(crate) struct OmpPane {
    /// The agent and its child.
    pub(crate) agent: OmpAgent,
    /// The runner it runs, for Open in terminal.
    runner: RunnerManifest,
    /// Which start of its child the frames being read belong to.
    generation: u64,
    /// Whether Open in terminal waits for its child to exit.
    open_in_terminal: bool,
    /// Worktree root and branch for the in-session Changes panel.
    worktree: Option<(PathBuf, Option<String>)>,
    /// The chat pane drawing the agent's session.
    pub(crate) view: Entity<OmpView>,
    /// Carries what the user does in [`OmpPane::view`] to the agent's omp.
    _intents: Subscription,
}

/// How an RPC agent opens its conversation for `start`: the space's task as its first prompt, its session
/// folder's newest session, or a new session.
fn opening_for(start: &AgentStart) -> Opening {
    match start {
        AgentStart::Task(task) => Opening::Task(task.clone()),
        AgentStart::Continue => Opening::Resume,
        AgentStart::Fresh => Opening::Fresh,
    }
}

impl Workspace {
    /// Whether agent pane `pane` in `space` runs an RPC runner (`interface = "rpc"`), giving it its own
    /// isolated session folder under the data directory.
    pub(crate) fn pane_is_rpc(&mut self, space: &xctx_core::Space, pane: &RolePane) -> bool {
        pane.role.kind == PaneKind::Agent
            && self
                .pane_runner(space, pane)
                .0
                .or_else(|| self.runners.default_runner().map(|r| r.manifest.clone()))
                .is_some_and(|r| r.is_rpc())
    }

    /// Start agent pane `pane` of open space `id` over RPC when its runner speaks RPC; `false` (nothing done)
    /// when it does not, so the pane gets a terminal.
    pub(crate) fn start_rpc_pane(
        &mut self,
        id: &SpaceId,
        pane: &RolePane,
        start: &AgentStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if pane.role.kind != PaneKind::Agent {
            return false;
        }
        if self.rpc_pane_is_live(id, pane.id) {
            // Already running: a second child would replace it and abort its turn.
            return true;
        }
        let Some(space) = self.tabs.get(id).map(|tab| tab.space.clone()) else {
            return false;
        };
        let runner = self
            .pane_runner(&space, pane)
            .0
            .or_else(|| self.runners.default_runner().map(|r| r.manifest.clone()));
        let Some(runner) = runner.filter(RunnerManifest::is_rpc) else {
            return false;
        };
        let agent = self.pane_agent_of(id, pane);
        // Only a pane actually bound to `node` owns that node's session folder and graph identity; an extra
        // unbound split agent pane gets its own `pane-<id>` folder so it never collides with the repo's writer.
        let node = agent.as_ref().and_then(|agent| {
            let state = self.graphs.load(&space).ok()?;
            state
                .graph()
                .node(&AgentNodeId::new(&agent.id))
                .filter(|n| n.pane_id == Some(pane.id.0))
                .cloned()
        });
        let folder = node.as_ref().map_or_else(
            || format!("pane-{}", pane.id.0),
            |n| n.id.as_str().to_owned(),
        );
        let launched = match &self.data_dir {
            Some(data) => {
                let dir = agent_session_dir(data, id, &folder);
                std::fs::create_dir_all(&dir)
                    .map_err(|e| format!("could not create {}: {e}", dir.display()))
                    .and_then(|()| {
                        rpc_launch(
                            &space,
                            &pane.role,
                            &self.search,
                            &runner,
                            agent.as_ref(),
                            &dir,
                        )
                    })
            }
            None => Err("xctx has no data directory to keep omp's sessions in".to_owned()),
        };
        let launch = match launched {
            Ok(launch) => launch,
            Err(e) => {
                let message = format!("could not start the {} agent: {e}", pane.role.repo);
                self.error(message.clone(), cx);
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.terminals.insert(pane.id, PaneTerminal::Failed(message));
                }
                return true;
            }
        };
        let label = node
            .as_ref()
            .map_or_else(|| format!("{} agent", pane.role.repo), |n| n.label.clone());
        let agent = OmpAgent::new(
            launch,
            id.clone(),
            node.as_ref().map(|n| n.id.clone()),
            label,
            node.and_then(|n| n.session),
        );
        let view = cx.new(|cx| OmpView::new(Arc::new(agent.model().clone()), cx));
        // The footer names the folder and branch the agent works in: the pane's repo, as `pane_worktree` picks it.
        let repo = space
            .repos
            .iter()
            .find(|r| r.name == pane.role.repo)
            .or_else(|| space.repos.first());
        let worktree_info = repo.map(|r| (r.worktree.clone(), Some(r.branch.clone())));
        if let Some(repo) = repo {
            let worktree = repo.worktree.to_string_lossy().into_owned();
            view.update(cx, |view, cx| {
                view.set_location(&worktree, Some(&repo.branch), cx)
            });
        }
        let (space_id, pane_id) = (id.clone(), pane.id);
        let ws = cx.weak_entity();
        let (refresh_space, refresh_pane) = (id.clone(), pane.id);
        let ws_sessions = cx.weak_entity();
        let (sess_space, sess_pane) = (id.clone(), pane.id);
        let ws_split = cx.weak_entity();
        let (split_space, split_pane, split_repo) = (id.clone(), pane.id, pane.role.repo.clone());
        view.update(cx, |view, _| {
            view.set_changes_refresher(Rc::new(move |cx| {
                ws.update(cx, |this, cx| {
                    this.refresh_rpc_changes(&refresh_space, refresh_pane, cx);
                })
                .ok();
            }));
            view.set_sessions_refresher(Rc::new(move |cx| {
                ws_sessions
                    .update(cx, |this, cx| {
                        this.refresh_rpc_sessions(&sess_space, sess_pane, cx);
                    })
                    .ok();
            }));
            view.set_split_agent_handler(Rc::new(move |window, cx| {
                ws_split
                    .update(cx, |this, cx| {
                        if let Err(e) = this.split_in(
                            &split_space,
                            Some(split_pane),
                            SplitAxis::Horizontal,
                            PaneKind::Agent,
                            Some(split_repo.clone()),
                            window,
                            cx,
                        ) {
                            this.show_op_error(e, cx);
                        }
                    })
                    .ok();
            }));
        });
        let intents = cx.subscribe(&view, move |this, _, intent: &OmpIntent, cx| {
            this.rpc_intent(&space_id, pane_id, intent, cx);
        });
        if let Some(tab) = self.tabs.get_mut(id) {
            tab.terminals.insert(
                pane.id,
                PaneTerminal::Omp(Box::new(OmpPane {
                    agent,
                    runner,
                    generation: 0,
                    open_in_terminal: false,
                    worktree: worktree_info,
                    view,
                    _intents: intents,
                })),
            );
        }
        self.index_rpc_files(id, pane.id, &space, &pane.role, window, cx);
        self.refresh_rpc_changes(id, pane.id, cx);
        self.refresh_rpc_sessions(id, pane.id, cx);
        self.boot_rpc_agent(id, pane.id, opening_for(start), window, cx);
        true
    }

    /// Scan persisted `.jsonl` sessions in RPC pane `pane`'s session directory off the UI thread and update its
    /// session strip and switcher drawer.
    fn refresh_rpc_sessions(&mut self, id: &SpaceId, pane: PaneId, cx: &mut Context<Self>) {
        let Some((session_dir, active_id, active_file)) = self.rpc_pane_mut(id, pane).map(|omp| {
            (
                omp.agent.launch().session_dir.clone(),
                omp.agent
                    .model()
                    .session
                    .id
                    .clone()
                    .or_else(|| omp.agent.session_id().map(str::to_owned)),
                omp.agent.model().session.file.clone(),
            )
        }) else {
            return;
        };
        let redactor = self.redactor.clone();
        let executor = cx.background_executor().clone();
        let space_id = id.clone();
        cx.spawn(async move |this, cx| {
            let entries = executor
                .spawn(async move {
                    sessions_for_pane(
                        &session_dir,
                        active_id.as_deref(),
                        active_file.as_deref(),
                        &redactor,
                    )
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(omp) = this.rpc_pane_mut(&space_id, pane) else {
                    return;
                };
                let view = omp.view.clone();
                view.update(cx, |view, cx| view.set_sessions(entries, cx));
            });
        })
        .detach();
    }

    /// Scan git status and diffs for RPC pane `pane`'s worktree off the UI thread and update its Changes panel.
    fn refresh_rpc_changes(&mut self, id: &SpaceId, pane: PaneId, cx: &mut Context<Self>) {
        let Some((root, branch)) = self
            .rpc_pane_mut(id, pane)
            .and_then(|omp| omp.worktree.clone())
        else {
            return;
        };
        let redactor = self.redactor.clone();
        let executor = cx.background_executor().clone();
        let space_id = id.clone();
        cx.spawn(async move |this, cx| {
            let snapshot = executor
                .spawn(async move { worktree_changes(&root, branch, &redactor) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(omp) = this.rpc_pane_mut(&space_id, pane) else {
                    return;
                };
                let view = omp.view.clone();
                view.update(cx, |view, cx| view.set_changes(snapshot, cx));
            });
        })
        .detach();
    }

    /// Index the files of the worktree RPC pane `pane` runs in, off the UI thread, and give its composer `@file`
    /// completion once the index is ready. Nothing is offered before then.
    fn index_rpc_files(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        space: &xctx_core::Space,
        role: &xctx_layout::PaneRole,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = pane_worktree(space, role).map(PathBuf::from) else {
            return;
        };
        let executor = cx.background_executor().clone();
        let space_id = id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let files = executor.spawn(async move { worktree_files(&root) }).await;
            let files = Rc::new(files);
            let _ = this.update(cx, |this, cx| {
                let Some(omp) = this.rpc_pane_mut(&space_id, pane) else {
                    return;
                };
                let view = omp.view.clone();
                view.update(cx, |view, cx| {
                    view.set_file_completer(
                        Rc::new(move |query: &str| file_matches(&files, query)),
                        cx,
                    );
                });
            });
        })
        .detach();
    }

    /// Start the child of RPC pane `pane` off the UI thread, then read its frames into the workspace.
    fn boot_rpc_agent(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        opening: Opening,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(omp) = self.rpc_pane_mut(id, pane) else {
            return;
        };
        let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
        omp.generation = generation;
        let spec = omp.agent.spec();
        let executor = cx.background_executor().clone();
        let space = id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let started = executor
                .spawn(async move { xctx_omp::OmpClient::start(&spec) })
                .await;
            let frames = this.update_in(cx, |this, _window, cx| {
                let omp = this
                    .rpc_pane_mut(&space, pane)
                    .filter(|omp| omp.generation == generation)?;
                let updates = omp.agent.attach(started, opening);
                let frames = omp.agent.take_frames();
                this.rpc_updates(&space, pane, &updates, cx);
                this.refresh_rpc_view(&space, pane, cx);
                frames
            });
            let Ok(Some(frames)) = frames else {
                return;
            };
            let (tx, mut rx) = mpsc::unbounded();
            // Its frames arrive on a blocking channel; a thread of its own carries them over until the child
            // is gone (or nothing reads them any more).
            let forwarded = std::thread::Builder::new()
                .name("xctx-omp-frames".into())
                .spawn(move || {
                    while let Ok(incoming) = frames.recv() {
                        if tx.unbounded_send(incoming).is_err() {
                            return;
                        }
                    }
                });
            if forwarded.is_err() {
                let _ = this.update(cx, |this, cx| {
                    this.error("could not read omp's frames: no thread to read them on", cx);
                });
                return;
            }
            while let Some(first) = rx.next().await {
                let mut gone = false;
                // Everything that arrived by now is applied in one go, so the pane redraws once per wake.
                let applied = this.update_in(cx, |this, window, cx| {
                    let mut next = Some(first);
                    while let Some(incoming) = next.take() {
                        gone = matches!(incoming, Incoming::Exited { .. });
                        this.rpc_incoming(&space, pane, generation, incoming, window, cx);
                        if gone {
                            break;
                        }
                        next = rx.try_recv().ok();
                    }
                    this.refresh_rpc_view(&space, pane, cx);
                });
                if gone || applied.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Apply one thing from the child of RPC pane `pane` (dropped unless it is from start `generation`), and
    /// act on its exit: start a crashed child again on its session, or finish Open in terminal.
    fn rpc_incoming(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        generation: u64,
        incoming: Incoming,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(omp) = self
            .rpc_pane_mut(id, pane)
            .filter(|omp| omp.generation == generation)
        else {
            return;
        };
        let view = omp.view.clone();
        let mut exit = None;
        let mut refresh_changes = false;
        let mut refresh_sessions = false;
        match &incoming {
            Incoming::Malformed(detail) => {
                view.update(cx, |view, cx| view.malformed_frame(detail, cx));
            }
            Incoming::Frame(Frame::Response(r)) if r.command == "get_model_picker" && r.success => {
                let choices = ModelChoice::from_picker(&r.data);
                view.update(cx, |view, cx| view.set_model_choices(choices, cx));
            }
            Incoming::Frame(Frame::Response(r))
                if r.success
                    && matches!(
                        r.command.as_str(),
                        "get_state" | "open_session" | "switch_session" | "new_session" | "fork"
                    ) =>
            {
                refresh_sessions = true;
            }
            Incoming::Frame(Frame::Event { kind, .. })
                if matches!(kind.as_str(), "agent_end" | "tool_execution_end") =>
            {
                refresh_changes = true;
                if kind == "agent_end" {
                    refresh_sessions = true;
                }
            }
            Incoming::Exited { code, stderr } => exit = Some((*code, stderr.clone())),
            Incoming::Frame(_) => {}
        }
        let updates = omp.agent.apply(incoming);
        self.rpc_updates(id, pane, &updates, cx);
        if refresh_changes {
            self.refresh_rpc_changes(id, pane, cx);
        }
        if refresh_sessions {
            self.refresh_rpc_sessions(id, pane, cx);
        }
        let Some(unexpected) = updates.iter().find_map(|update| match update {
            AgentUpdate::Exited { unexpected, .. } => Some(*unexpected),
            _ => None,
        }) else {
            return;
        };
        let Some(omp) = self.rpc_pane_mut(id, pane) else {
            return;
        };
        if !unexpected {
            if omp.open_in_terminal {
                self.hand_over_to_terminal(id, pane, window, cx);
            }
            return;
        }
        if let Some(opening) = omp.agent.restart_opening() {
            self.boot_rpc_agent(id, pane, opening, window, cx);
            return;
        }
        if let Some((code, stderr)) = &exit {
            view.update(cx, |view, cx| view.client_exited(*code, stderr, cx));
        }
        let reason = match omp.agent.status() {
            AgentStatus::Failed { reason } => reason.clone(),
            _ => "it exited".to_owned(),
        };
        self.error(
            format!("{reason}; Restart agent starts it again on its session"),
            cx,
        );
    }

    /// Answer the omp request of Inbox item `item` as the pane's card would: `approve` picks the call's approve
    /// option, else its deny option. It goes to the RPC pane whose agent waits on it. When none does, the item is
    /// stale (omp moved on or exited) and is dismissed.
    pub(crate) fn answer_omp_gate(
        &mut self,
        item: xctx_inbox::ItemId,
        approve: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(found) = self.inbox.as_ref().and_then(|inbox| inbox.get(&item)) else {
            return;
        };
        let xctx_inbox::ItemBody::OmpGate(gate) = &found.body else {
            return;
        };
        if !found.is_pending() {
            return;
        }
        let request_id = gate.request_id.clone();
        let value = if approve {
            gate.approve.clone()
        } else {
            gate.deny.clone()
        };
        let waiting = self.tabs.iter().find_map(|(space, tab)| {
            tab.terminals
                .iter()
                .find_map(|(pane, terminal)| match terminal {
                    PaneTerminal::Omp(omp) if omp.agent.waits_for(&request_id) => {
                        Some((space.clone(), *pane))
                    }
                    _ => None,
                })
        });
        match waiting {
            Some((space, pane)) => {
                let Some(omp) = self.rpc_pane_mut(&space, pane) else {
                    return;
                };
                let answer = xctx_omp::Command::ui_value(&request_id, &value);
                match omp.agent.respond_ui(&answer) {
                    Ok(updates) => self.rpc_updates(&space, pane, &updates, cx),
                    Err(e) => self.error(format!("could not answer the agent: {e}"), cx),
                }
                self.refresh_rpc_view(&space, pane, cx);
                cx.notify();
            }
            None => {
                self.change_inbox(cx, |inbox| inbox.dismiss(&item));
                self.info("omp no longer waits for that answer", cx);
            }
        }
    }

    /// Carry `updates` of RPC pane `pane` of space `id` into its graph and the Inbox, and redraw.
    fn rpc_updates(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        updates: &[AgentUpdate],
        cx: &mut Context<Self>,
    ) {
        if updates.is_empty() {
            return;
        }
        let Some(space) = self.tabs.get(id).map(|tab| tab.space.clone()) else {
            return;
        };
        let graph = self.graphs.load(&space).ok();
        let Some(PaneTerminal::Omp(omp)) = self
            .tabs
            .get_mut(id)
            .and_then(|tab| tab.terminals.get_mut(&pane))
        else {
            return;
        };
        let problems = omp
            .agent
            .apply_updates(updates, graph, self.inbox.as_mut(), &self.redactor);
        for problem in problems {
            self.error(problem, cx);
        }
        cx.notify();
    }

    /// Show the current session of RPC pane `pane` of space `id` in its view.
    fn refresh_rpc_view(&mut self, id: &SpaceId, pane: PaneId, cx: &mut Context<Self>) {
        let Some(omp) = self.rpc_pane_mut(id, pane) else {
            return;
        };
        let session = Arc::new(omp.agent.model().clone());
        omp.view
            .clone()
            .update(cx, |view, cx| view.set_session(session, cx));
    }

    /// Send what the user did in the view of RPC pane `pane` of space `id` to its omp.
    fn rpc_intent(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        intent: &OmpIntent,
        cx: &mut Context<Self>,
    ) {
        let Some(omp) = self.rpc_pane_mut(id, pane) else {
            return;
        };
        let result = match intent {
            OmpIntent::Answer { .. } => omp.agent.respond_ui(&intent.command()),
            _ => omp
                .agent
                .send_command(intent.command())
                .map(|()| Vec::new()),
        };
        match result {
            Ok(updates) => self.rpc_updates(id, pane, &updates, cx),
            Err(e) => self.error(format!("could not send to the agent: {e}"), cx),
        }
        self.refresh_rpc_view(id, pane, cx);
        cx.notify();
    }

    /// The RPC pane `pane` of open space `id`, if it is one.
    pub(crate) fn rpc_pane_mut(&mut self, id: &SpaceId, pane: PaneId) -> Option<&mut OmpPane> {
        match self.tabs.get_mut(id)?.terminals.get_mut(&pane)? {
            PaneTerminal::Omp(omp) => Some(omp),
            _ => None,
        }
    }

    /// Whether pane `pane` of open space `id` is an RPC pane whose child runs. Such a child must not be replaced or
    /// removed: dropping its client closes omp's input, and omp ends the running turn as aborted.
    pub(crate) fn rpc_pane_is_live(&self, id: &SpaceId, pane: PaneId) -> bool {
        matches!(
            self.tabs.get(id).and_then(|tab| tab.terminals.get(&pane)),
            Some(PaneTerminal::Omp(omp)) if omp.agent.is_live()
        )
    }

    /// Open in terminal: stop the RPC child of pane `pane` of open space `id` (closing its input, as omp
    /// expects), and once it exited and its session is not leased, run `omp --resume <session> --session-dir
    /// <folder>` in a terminal in the pane.
    pub(crate) fn open_rpc_agent_in_terminal(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(omp) = self.rpc_pane_mut(id, pane) else {
            return;
        };
        omp.open_in_terminal = true;
        if omp.agent.is_live() {
            omp.agent.close();
            cx.notify();
            return;
        }
        self.hand_over_to_terminal(id, pane, window, cx);
    }

    /// Check that no omp holds the session of stopped RPC pane `pane`, then put the terminal resuming it in its
    /// place.
    fn hand_over_to_terminal(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(omp) = self.rpc_pane_mut(id, pane) else {
            return;
        };
        let launch = omp.agent.launch().clone();
        let session = omp.agent.session_id().map(str::to_owned);
        let executor = cx.background_executor().clone();
        let space = id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let held = match session.clone() {
                Some(session) => {
                    executor
                        .spawn(async move { session_lease_held(&launch, &session) })
                        .await
                }
                None => Ok(false),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_open_in_terminal(&space, pane, session.as_deref(), held, window, cx);
            });
        })
        .detach();
    }

    /// Put `omp --resume` in pane `pane` of space `id` in place of its stopped RPC agent, unless the lease check
    /// (`held`) says another omp holds `session` or could not tell.
    fn finish_open_in_terminal(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        session: Option<&str>,
        held: Result<bool, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(omp) = self
            .rpc_pane_mut(id, pane)
            .filter(|omp| omp.open_in_terminal && !omp.agent.is_live())
        else {
            return;
        };
        omp.open_in_terminal = false;
        let refusal = match held {
            Ok(false) => None,
            Ok(true) => Some(format!(
                "another omp holds session {}; it was not opened in a terminal",
                session.unwrap_or_default()
            )),
            Err(e) => Some(format!(
                "could not check whether another omp holds the session ({e}); it was not opened in a terminal"
            )),
        };
        if let Some(refusal) = refusal {
            self.error(refusal, cx);
            cx.notify();
            return;
        }
        let runner = omp.runner.clone();
        let session_dir = omp.agent.launch().session_dir.clone();
        let Some(tab) = self.tabs.get(id) else {
            return;
        };
        let space = tab.space.clone();
        let Some(role_pane) = tab.layout.panes().into_iter().find(|p| p.id == pane) else {
            return;
        };
        let agent = self.pane_agent_of(id, &role_pane);
        let launch = terminal_resume_launch(
            &space,
            &role_pane.role,
            &self.search,
            &runner,
            agent.as_ref(),
            session,
            &session_dir,
        );
        let program = launch.config.program.clone();
        let terminal = match TerminalView::spawn(
            launch.config,
            self.terminal_font.clone(),
            self.terminal_theme,
            window,
            cx,
        ) {
            Ok(view) => self.running_terminal(id, pane, view, false, window, cx),
            Err(e) => {
                self.error(format!("could not start {program}: {e}"), cx);
                return;
            }
        };
        if let Some(tab) = self.tabs.get_mut(id) {
            tab.terminals.insert(pane, terminal);
        }
        if let Some(notice) = launch.notice {
            self.info(notice, cx);
        }
        if self.may_focus_pane_of(id) {
            self.focus_active_pane(window, cx);
        }
        cx.notify();
    }

    /// What RPC pane `omp` shows in the split: its status line, with the retry badge while omp retries.
    pub(crate) fn render_rpc_pane(&self, omp: &OmpPane, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx);
        // The view shows the conversation; this line only says what it cannot: a start, a block or a failure.
        let status = match omp.agent.status() {
            AgentStatus::Starting => Some(("starting omp…".to_owned(), theme.text_subtle)),
            AgentStatus::Blocked { reason } => Some((reason.clone(), theme.warning)),
            AgentStatus::Failed { reason } => Some((reason.clone(), theme.danger)),
            _ => None,
        };
        let status = match (status, omp.agent.retry_badge()) {
            (Some((text, color)), Some(badge)) => Some((format!("{text} · {badge}"), color)),
            (None, Some(badge)) => Some((badge.to_owned(), theme.warning)),
            (status, None) => status,
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .children(status.map(|(text, color)| {
                div()
                    .flex_none()
                    .px(Theme::SPACE_3)
                    .py(Theme::SPACE_1)
                    .text_size(Theme::TEXT_SM)
                    .text_color(color)
                    .child(self.redactor.redact(&text).into_owned())
            }))
            .child(div().flex_1().min_h_0().child(omp.view.clone()))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::file_matches;

    #[test]
    fn at_file_matches_rank_file_names_that_start_with_the_query_first() {
        let files: Vec<String> = [
            "docs/notes/readme.md",
            "src/app/readme.rs",
            "README.md",
            "src/thread.rs",
        ]
        .map(String::from)
        .to_vec();
        let hits: Vec<String> = file_matches(&files, "READ")
            .into_iter()
            .map(|h| h.to_string())
            .collect();
        // Names starting with the query come first, shorter paths first; a match inside a name ranks last even
        // though its path is the shortest.
        assert_eq!(
            hits,
            vec![
                "README.md",
                "src/app/readme.rs",
                "docs/notes/readme.md",
                "src/thread.rs",
            ]
        );
    }

    #[test]
    fn at_file_matches_are_capped() {
        let files: Vec<String> = (0..100).map(|i| format!("f{i}.txt")).collect();
        assert_eq!(file_matches(&files, "f").len(), super::FILE_MATCH_CAP);
        assert!(file_matches(&files, "missing").is_empty());
    }
}
