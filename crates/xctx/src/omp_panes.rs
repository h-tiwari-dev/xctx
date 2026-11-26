//! The workspace side of agent panes whose runner speaks RPC (`interface = "rpc"`): starting their omp off the UI
//! thread, carrying its frames into the space's graph and the Inbox, starting a crashed one again on its session,
//! and Open in terminal, which hands the session to `omp --resume` in a terminal pane.
//!
//! The pane itself shows only a status line.

use std::sync::atomic::{AtomicU64, Ordering};

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, Window, div};
use xctx_core::SpaceId;
use xctx_graph::AgentNodeId;
use xctx_layout::{PaneId, PaneKind, RolePane};
use xctx_plugin::RunnerManifest;
use xctx_term_view::TerminalView;
use xctx_ui::Theme;

use crate::omp_agent::{
    AgentStatus, AgentUpdate, OmpAgent, Opening, agent_session_dir, session_lease_held,
};
use crate::space_tab::PaneTerminal;
use crate::spawn::{AgentStart, rpc_launch, terminal_resume_launch};
use crate::workspace::Workspace;

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
        let node = agent.as_ref().and_then(|agent| {
            let state = self.graphs.load(&space).ok()?;
            state.graph().node(&AgentNodeId::new(&agent.id)).cloned()
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
        if let Some(tab) = self.tabs.get_mut(id) {
            tab.terminals.insert(
                pane.id,
                PaneTerminal::Omp(Box::new(OmpPane {
                    agent,
                    runner,
                    generation: 0,
                    open_in_terminal: false,
                })),
            );
        }
        self.boot_rpc_agent(id, pane.id, opening_for(start), window, cx);
        true
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
            while let Some(incoming) = rx.next().await {
                let gone = matches!(incoming, xctx_omp::Incoming::Exited { .. });
                let applied = this.update_in(cx, |this, window, cx| {
                    this.rpc_incoming(&space, pane, generation, incoming, window, cx)
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
        incoming: xctx_omp::Incoming,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(omp) = self
            .rpc_pane_mut(id, pane)
            .filter(|omp| omp.generation == generation)
        else {
            return;
        };
        let updates = omp.agent.apply(incoming);
        self.rpc_updates(id, pane, &updates, cx);
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
        let reason = match omp.agent.status() {
            AgentStatus::Failed { reason } => reason.clone(),
            _ => "it exited".to_owned(),
        };
        self.error(
            format!("{reason}; Restart agent starts it again on its session"),
            cx,
        );
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

    /// The RPC pane `pane` of open space `id`, if it is one.
    pub(crate) fn rpc_pane_mut(&mut self, id: &SpaceId, pane: PaneId) -> Option<&mut OmpPane> {
        match self.tabs.get_mut(id)?.terminals.get_mut(&pane)? {
            PaneTerminal::Omp(omp) => Some(omp),
            _ => None,
        }
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
        let (status, color) = match omp.agent.status() {
            AgentStatus::Starting => ("starting omp…".to_owned(), theme.text_subtle),
            AgentStatus::Running => ("working".to_owned(), theme.text),
            AgentStatus::Idle => ("waiting for a message".to_owned(), theme.text_subtle),
            AgentStatus::Done => ("done".to_owned(), theme.text_subtle),
            AgentStatus::Blocked { reason } => (reason.clone(), theme.warning),
            AgentStatus::Failed { reason } => (reason.clone(), theme.danger),
            AgentStatus::Stopped => ("stopped".to_owned(), theme.text_subtle),
        };
        let status = match omp.agent.retry_badge() {
            Some(badge) => format!("{status} · {badge}"),
            None => status,
        };
        div()
            .size_full()
            .p(Theme::SPACE_3)
            .text_size(Theme::TEXT_SM)
            .text_color(color)
            .child(self.redactor.redact(&status).into_owned())
            .into_any_element()
    }
}
