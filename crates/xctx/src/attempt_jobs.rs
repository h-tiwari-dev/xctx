//! Competing attempts in the window: the named `pi` session each agent pane runs, each attempt's agent terminal,
//! and the fork, adopt and discard jobs, run in the background the way Ship and Discard are.
//!
//! What the attempt UI calls:
//! - [`Workspace::fork_attempts_then`] to fork a writer (the fork dialog and an approved Inbox proposal);
//! - [`Workspace::adopt_attempt`] and [`Workspace::discard_attempt`] from the review sheet;
//! - [`Workspace::attempt_terminal`] for the terminal to show when the attempt strip switches to an attempt, and
//!   [`Workspace::attempt_set`] for the records the strip and the review sheet are drawn from.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, Entity, Window};
use xctx_attempt::{Attempt, AttemptSet, ForkPlan};
use xctx_core::{AttemptWorktree, Space, SpaceId};
use xctx_graph::{AgentNodeId, AgentRole};
use xctx_inbox::{ItemBody, ItemId};
use xctx_layout::{PaneKind, RolePane};
use xctx_term::TerminalSession;
use xctx_term_view::{TerminalView, TerminalViewEvent};

use crate::agent_session::{
    AgentSession, adopted_session_id, adopted_winner, fork_source, fork_source_arg, forked_session,
    is_adopted_session, pane_session,
};
use crate::attempts::{
    allocate_attempt_ports, attempt_agent_id, attempt_scratch, copy_scratch, fork_instructions,
    live_attempts_refusal, new_attempt_token, now, plan_fork_skipping_leftovers,
    unrecorded_cleanup_message, unsaved_adoption_message, unsaved_adoption_refusal,
};
use crate::space_graph::{agent_role_name, graph_error, scratch_dir_for};
use crate::space_tab::PaneTerminal;
use crate::spawn::{AgentStart, AttemptPane, PaneAgent, attempt_launch, pane_worktree};
use crate::workspace::{Workspace, in_flight_message, wait_message};

/// How long a stopped attempt's agent gets to exit before its worktree is removed.
const ATTEMPT_EXIT_TIMEOUT: Duration = Duration::from_secs(2);

/// How long an agent that did not exit in time gets before xctx gives up starting it again.
const RESTART_WAIT: Duration = Duration::from_secs(15);

/// How often an agent that is still exiting is checked.
const RESTART_POLL: Duration = Duration::from_millis(250);

/// What a fork needs, checked before the store is taken.
struct ForkPrep {
    space: Space,
    repo: String,
    plan: ForkPlan,
    source: String,
}

/// Remove an attempt's worktree, branch and mise file, and its scratch folder (best effort).
fn remove_attempt(slug: &str, worktree: &AttemptWorktree) -> xctx_core::Result<()> {
    xctx_core::discard_attempt(slug, worktree)?;
    if let Ok(scratch) = scratch_dir_for(slug)
        && let Ok(dir) = attempt_scratch(&scratch, worktree.number)
    {
        let _ = dir.clean();
    }
    Ok(())
}

/// Stop `session` and wait for it to exit; `true` when it did (or there was none).
fn stop_and_wait(session: Option<Arc<TerminalSession>>) -> bool {
    session.is_none_or(|s| s.shutdown_and_wait(ATTEMPT_EXIT_TIMEOUT))
}

impl Workspace {
    // ---------------------------------------------------------------------------------------------------------
    // Sessions

    /// The `pi` session an agent pane runs: the session of the graph agent bound to it (see
    /// [`crate::agent_session`]). Nothing is recorded here: [`Workspace::record_pane_session`] records the id once
    /// `pi` itself has started. Panes that are not agent panes or not bound to an agent run
    /// [`AgentSession::Unnamed`]. Fails, saying why, when the writer's session comes from an adopted attempt
    /// whose records cannot be read, rather than opening an empty session in its place.
    pub(crate) fn pane_session_of(
        &mut self,
        id: &SpaceId,
        pane: &RolePane,
        start: &AgentStart,
    ) -> Result<AgentSession, String> {
        if pane.role.kind != PaneKind::Agent {
            return Ok(AgentSession::Unnamed);
        }
        let Some(space) = self.tabs.get(id).map(|tab| tab.space.clone()) else {
            return Ok(AgentSession::Unnamed);
        };
        let cwd: Option<PathBuf> = pane_worktree(&space, &pane.role).map(Path::to_path_buf);
        let Ok(state) = self.graphs.load(&space) else {
            return Ok(AgentSession::Unnamed);
        };
        let Some(node) = state
            .graph()
            .nodes()
            .iter()
            .find(|n| n.pane_id == Some(pane.id.0))
            .cloned()
        else {
            return Ok(AgentSession::Unnamed);
        };
        let slug = space.id.as_str();
        let storage = self.sessions.clone();
        let Some(recorded) = node.session.as_deref() else {
            return Ok(pane_session(slug, node.id.as_str(), None, start, || {
                storage
                    .zip(cwd)
                    .and_then(|(s, cwd)| s.newest_session_in(&cwd))
            }));
        };
        let started = match (&storage, &cwd) {
            (Some(storage), Some(cwd)) => storage.has_session(cwd, recorded),
            _ => false,
        };
        if started || !is_adopted_session(slug, node.id.as_str(), recorded) {
            return Ok(AgentSession::Open(recorded.to_owned()));
        }
        // A writer that adopted an attempt forks the winner's session the first time it starts.
        let set = self.attempts.load(id).map_err(|e| {
            format!(
                "{e}; the agent {} was not started, since it would open an empty session instead of the \
                 adopted attempt's",
                node.id
            )
        })?;
        Ok(
            match adopted_winner(set, slug, node.id.as_str(), recorded) {
                Some(winner) => {
                    let file = storage.as_ref().and_then(|s| s.find(&winner.session));
                    AgentSession::Fork {
                        source: fork_source_arg(&winner.session, file.as_deref()),
                        id: recorded.to_owned(),
                    }
                }
                None => AgentSession::Open(recorded.to_owned()),
            },
        )
    }

    /// Record on the graph agent bound to `pane` the id of `session`, which its pane now runs (`pi` started; see
    /// [`crate::spawn::runs_pi`]). Best effort: an unsaved id is recorded again the next time the pane starts.
    pub(crate) fn record_pane_session(
        &mut self,
        id: &SpaceId,
        pane: &RolePane,
        session: &AgentSession,
    ) {
        let Some(named) = session.named_id() else {
            return;
        };
        let Some(space) = self.tabs.get(id).map(|tab| tab.space.clone()) else {
            return;
        };
        let Ok(state) = self.graphs.load(&space) else {
            return;
        };
        let Some(node) = state
            .graph()
            .nodes()
            .iter()
            .find(|n| n.pane_id == Some(pane.id.0))
            .cloned()
        else {
            return;
        };
        if node.session.as_deref() == Some(named) {
            return;
        }
        let _ = state.change(|doc| {
            doc.graph
                .set_session(&node.id, Some(named.to_owned()))
                .map_err(|e| graph_error(id, e))
        });
    }

    /// Run `then` once `session` (an agent that was told to stop) has exited: now when it has (or there is
    /// none), else after it does, polling in the background. `what` names the agent in the status bar. When it
    /// does not exit within [`RESTART_WAIT`], `then` does not run and the user is told how to start it again, so
    /// a second `pi` never runs on a session the first one still writes.
    fn when_exited(
        &mut self,
        session: Option<Arc<TerminalSession>>,
        what: String,
        then: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = session.filter(|s| s.is_alive()) else {
            then(self, window, cx);
            return;
        };
        self.info(
            format!("{what} is still exiting; it starts again once it has"),
            cx,
        );
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            let mut waited = Duration::ZERO;
            while session.is_alive() && waited < RESTART_WAIT {
                executor.timer(RESTART_POLL).await;
                waited += RESTART_POLL;
            }
            let exited = !session.is_alive();
            drop(session);
            let _ = this.update_in(cx, |this, window, cx| {
                if exited {
                    then(this, window, cx);
                } else {
                    this.error(
                        format!("{what} did not exit; quit and reopen xctx to start it again"),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Start attempt `number`'s agent of open space `id` again once `session`, its stopped agent, has exited
    /// (see [`Workspace::when_exited`]), if the attempt is still live and has no terminal by then.
    fn restart_attempt_agent(
        &mut self,
        id: &SpaceId,
        number: u32,
        session: Option<Arc<TerminalSession>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = id.clone();
        self.when_exited(
            session,
            format!("the agent of attempt {number} of {id}"),
            move |this, window, cx| {
                let idle = this
                    .tabs
                    .get(&id)
                    .is_some_and(|tab| !tab.attempt_terminals.contains_key(&number));
                let attempt = this
                    .attempts
                    .get(&id)
                    .and_then(|set| set.get(number))
                    .filter(|a| a.state.is_live())
                    .cloned();
                if idle && let Some(attempt) = attempt {
                    this.spawn_attempt_terminal(&id, &attempt, window, cx);
                }
            },
            window,
            cx,
        );
    }

    /// The pane of the open tab of `id` that writer `agent` is bound to, and the session its terminal runs.
    fn writer_pane(
        &mut self,
        id: &SpaceId,
        agent: &str,
        cx: &Context<Self>,
    ) -> Option<(RolePane, Option<Arc<TerminalSession>>)> {
        let space = self.tabs.get(id)?.space.clone();
        let pane_id = self
            .graphs
            .load(&space)
            .ok()?
            .graph()
            .node(&AgentNodeId::new(agent))?
            .pane_id?;
        let tab = self.tabs.get(id)?;
        let pane = tab.layout.panes().into_iter().find(|p| p.id.0 == pane_id)?;
        let session = match tab.terminals.get(&pane.id) {
            Some(PaneTerminal::Running { view, .. }) => Some(Arc::clone(view.read(cx).session())),
            _ => None,
        };
        Some((pane, session))
    }

    /// Start the writer's `pane` of open space `id` again in its session once `session`, its stopped agent, has
    /// exited (see [`Workspace::when_exited`]).
    fn restart_writer(
        &mut self,
        id: &SpaceId,
        pane: RolePane,
        session: Option<Arc<TerminalSession>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = id.clone();
        self.when_exited(
            session,
            format!("the writer's agent of {id}"),
            move |this, window, cx| {
                let Some(tab) = this.tabs.get_mut(&id) else {
                    return;
                };
                if !tab.layout.panes().iter().any(|p| p.id == pane.id) {
                    return;
                }
                tab.terminals.remove(&pane.id);
                this.spawn_pane(&id, &pane, &AgentStart::Continue, window, cx);
            },
            window,
            cx,
        );
    }

    // ---------------------------------------------------------------------------------------------------------
    // Attempt terminals

    /// The attempts of space `id`, loading them on first use: what the attempt strip and review sheet show.
    pub(crate) fn attempt_set(&mut self, id: &SpaceId) -> Result<&AttemptSet, String> {
        self.attempts.load(id)
    }

    /// The agent terminal of attempt `number` of open space `id`, once it has started.
    pub(crate) fn attempt_terminal(
        &self,
        id: &SpaceId,
        number: u32,
    ) -> Option<Entity<TerminalView>> {
        match self.tabs.get(id)?.attempt_terminals.get(&number)? {
            PaneTerminal::Running { view, .. } => Some(view.clone()),
            PaneTerminal::Failed(_) => None,
        }
    }

    /// Start the agent terminal of every live attempt of open space `id` that has none. A live attempt without a
    /// report token (forked before tokens) is given one first.
    pub(crate) fn start_live_attempts(
        &mut self,
        id: &SpaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let untokened: Vec<u32> = match self.attempts.load(id) {
            Ok(set) => set
                .live()
                .filter(|a| a.token.is_empty())
                .map(Attempt::number)
                .collect(),
            Err(e) => {
                self.error(e, cx);
                return;
            }
        };
        if !untokened.is_empty() {
            let given = untokened
                .iter()
                .map(|&n| new_attempt_token().map(|token| (n, token)))
                .collect::<Result<Vec<_>, String>>()
                .and_then(|tokens| {
                    self.attempts.change(id, |set| {
                        tokens
                            .into_iter()
                            .try_for_each(|(n, token)| set.set_token(n, token))
                    })
                });
            if let Err(e) = given {
                self.error(
                    format!("{e}; attempts of {id} without a report token cannot report"),
                    cx,
                );
            }
        }
        let live: Vec<Attempt> = match self.attempts.load(id) {
            Ok(set) => set.live().cloned().collect(),
            Err(e) => {
                self.error(e, cx);
                return;
            }
        };
        for attempt in &live {
            let running = self
                .tabs
                .get(id)
                .is_some_and(|tab| tab.attempt_terminals.contains_key(&attempt.number()));
            if !running {
                self.spawn_attempt_terminal(id, attempt, window, cx);
            }
        }
    }

    /// Start the agent of `attempt` in its worktree, in open space `id`: a fork of its writer's session the first
    /// time (with its instruction as the first message; the source by its file when pi has written it), its own
    /// session afterwards. It runs as its own agent (`<writer>#attempt-<n>`) with its report token.
    fn spawn_attempt_terminal(
        &mut self,
        id: &SpaceId,
        attempt: &Attempt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(space) = self.tabs.get(id).map(|tab| tab.space.clone()) else {
            return;
        };
        let number = attempt.number();
        let worktree = attempt.worktree.worktree.clone();
        let started = self
            .sessions
            .as_ref()
            .is_some_and(|s| s.has_session(&worktree, &attempt.session));
        let session = if started {
            forked_session(&attempt.session, &attempt.source_session, true)
        } else {
            let file = self
                .sessions
                .as_ref()
                .and_then(|s| s.find(&attempt.source_session));
            let source = fork_source_arg(&attempt.source_session, file.as_deref());
            forked_session(&attempt.session, &source, false)
        };
        let scratch = scratch_dir_for(id.as_str())
            .and_then(|s| attempt_scratch(&s, number))
            .ok()
            .map(|s| s.root().to_path_buf());
        if let Some(dir) = &scratch {
            // Best effort, as for a space's own scratch directory.
            let _ = std::fs::create_dir_all(dir);
        }
        let pane = AttemptPane {
            number,
            repo: attempt.worktree.repo.clone(),
            worktree,
            port: attempt.worktree.port,
            agent: PaneAgent {
                id: attempt_agent_id(&attempt.agent, number),
                role: agent_role_name(&AgentRole::Writer {
                    repo: attempt.worktree.repo.clone(),
                }),
            },
            token: attempt.token.clone(),
            session,
            instruction: attempt.instruction.clone(),
            scratch,
        };
        let launch = attempt_launch(&space, &pane, &self.search);
        let program = launch.config.program.clone();
        let terminal = match TerminalView::spawn(
            launch.config,
            self.terminal_font.clone(),
            self.terminal_theme,
            window,
            cx,
        ) {
            Ok(view) => {
                let space_id = id.clone();
                let subscription = cx.subscribe_in(
                    &view,
                    window,
                    move |this, _view, event: &TerminalViewEvent, _window, cx| {
                        this.on_attempt_terminal_event(&space_id, number, event, cx);
                    },
                );
                PaneTerminal::Running {
                    view,
                    title: None,
                    exited: None,
                    _subscription: subscription,
                }
            }
            Err(e) => {
                let message = format!("could not start {program} for attempt {number}: {e}");
                self.error(message.clone(), cx);
                PaneTerminal::Failed(message)
            }
        };
        if let Some(tab) = self.tabs.get_mut(id) {
            tab.attempt_terminals.insert(number, terminal);
        }
        if let Some(notice) = launch.notice {
            self.info(notice, cx);
        }
        cx.notify();
    }

    fn on_attempt_terminal_event(
        &mut self,
        id: &SpaceId,
        number: u32,
        event: &TerminalViewEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(PaneTerminal::Running { title, exited, .. }) = self
            .tabs
            .get_mut(id)
            .and_then(|tab| tab.attempt_terminals.get_mut(&number))
        else {
            return;
        };
        match event {
            TerminalViewEvent::TitleChanged(new_title) => {
                *title = (!new_title.is_empty()).then(|| new_title.clone());
            }
            TerminalViewEvent::Exited(code) => *exited = Some(*code),
            TerminalViewEvent::Focused | TerminalViewEvent::Bell => return,
        }
        cx.notify();
    }

    /// Take attempt `number`'s terminal out of open space `id`, stopping it. Returns its session, to wait for.
    fn stop_attempt_terminal(
        &mut self,
        id: &SpaceId,
        number: u32,
        cx: &mut Context<Self>,
    ) -> Option<Arc<TerminalSession>> {
        let terminal = self.tabs.get_mut(id)?.attempt_terminals.remove(&number)?;
        cx.notify();
        match terminal {
            PaneTerminal::Running { view, .. } => Some(Arc::clone(view.read(cx).session())),
            PaneTerminal::Failed(_) => None,
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // Refusals

    /// Why an attempt job cannot run in space `id` now, if it cannot: the space is not active, is being shipped
    /// or discarded, or another attempt job of it runs.
    fn attempt_job_refusal(&self, id: &SpaceId) -> Option<String> {
        if self.in_flight_spaces.contains(id) {
            return Some(in_flight_message(id));
        }
        if self.attempt_jobs.contains(id) {
            return Some(format!(
                "attempts of {id} are being forked, adopted or discarded; wait until that finishes"
            ));
        }
        if !self.spaces.iter().any(|s| &s.id == id) {
            return Some(format!("space {id} is not active"));
        }
        None
    }

    /// Why space `id` cannot be shipped or discarded (`action`) now because of its attempts, if it cannot: an
    /// attempt job runs, or it has live attempts, which must be adopted or discarded first.
    pub(crate) fn attempts_block_end(&mut self, id: &SpaceId, action: &str) -> Option<String> {
        if self.attempt_jobs.contains(id) {
            return Some(format!(
                "attempts of {id} are being forked, adopted or discarded; wait until that finishes"
            ));
        }
        match self.attempts.load(id) {
            Ok(set) => live_attempts_refusal(id, set, action),
            Err(e) => Some(format!(
                "{e}; cannot {action} {id} without knowing its attempts"
            )),
        }
    }

    /// The space `id` as the window knows it: its open tab's copy, else the store's.
    pub(crate) fn active_space(&self, id: &SpaceId) -> Option<Space> {
        self.tabs
            .get(id)
            .map(|tab| tab.space.clone())
            .or_else(|| self.spaces.iter().find(|s| &s.id == id).cloned())
    }

    // ---------------------------------------------------------------------------------------------------------
    // Fork

    /// Fork the writer `agent` of space `id` into one attempt per entry of `instructions` (each optionally told
    /// what to try), in the background: worktrees from a snapshot of the writer's, a `PORT` each, a copy of the
    /// space's scratch folder, and an agent forked from the writer's session. Then call `done` with whether the
    /// attempts were forked, or why not (it is not called when the job did not start). Refused, with the reason
    /// returned, while another job has the store or the space. Every failure is also shown in the status bar.
    pub(crate) fn fork_attempts_then(
        &mut self,
        id: &SpaceId,
        agent: &AgentNodeId,
        instructions: Vec<Option<String>>,
        done: impl FnOnce(&mut Self, Result<(), String>, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let prep = match self.prepare_fork(id, agent, instructions) {
            Ok(prep) => prep,
            Err(e) => {
                self.error(e.clone(), cx);
                return Err(e);
            }
        };
        let why_no_store = self
            .busy
            .as_deref()
            .map(wait_message)
            .or_else(|| self.store_error.clone())
            .unwrap_or_else(|| "the store is not available".to_owned());
        let Some(store) = self.take_store(&format!("forking attempts of {id}"), cx) else {
            return Err(why_no_store);
        };
        let mut taken: Vec<u16> = store.port_allocations().into_keys().collect();
        let spaces: Vec<SpaceId> = self.spaces.iter().map(|s| s.id.clone()).collect();
        taken.extend(self.attempts.live_ports(&spaces));
        let ports = match allocate_attempt_ports(prep.plan.numbers.len(), taken) {
            Ok(ports) => ports,
            Err(e) => {
                self.return_store(store, cx);
                self.error(e.clone(), cx);
                return Err(e);
            }
        };
        self.attempt_jobs.insert(id.clone());
        let pairs: Vec<(u32, u16)> = prep.plan.numbers.iter().copied().zip(ports).collect();
        let space_scratch = scratch_dir_for(id.as_str()).ok();
        let executor = cx.background_executor().clone();
        let id = id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let ForkPrep {
                space,
                repo,
                plan,
                source,
            } = prep;
            let (store, result) = executor
                .spawn(async move {
                    let result = xctx_core::fork_attempts(&space, &repo, &pairs);
                    if let (Ok(made), Some(scratch)) = (&result, &space_scratch) {
                        for worktree in made {
                            // Best effort: an attempt without the space's artifacts still works.
                            if let Ok(dest) = attempt_scratch(scratch, worktree.number) {
                                let _ = copy_scratch(scratch, &dest);
                            }
                        }
                    }
                    (store, result)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.return_store(store, cx);
                let forked = match result {
                    // The job stays in `attempt_jobs` until what was made is recorded or removed again.
                    Ok(worktrees) => this.record_forked(&id, &plan, worktrees, &source, window, cx),
                    Err(e) => {
                        this.attempt_jobs.remove(&id);
                        let message =
                            format!("could not fork attempts of {} in {id}: {e}", plan.agent);
                        this.error(message.clone(), cx);
                        Err(message)
                    }
                };
                done(this, forked, window, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
        Ok(())
    }

    /// Check a fork of `agent` in `id` and plan it: the agent must be a writer whose recorded `pi` session (or,
    /// right after an adoption, the winning attempt's) pi has written (see [`fork_source`]). Attempt numbers a
    /// failed fork left a branch or folder for are skipped.
    fn prepare_fork(
        &mut self,
        id: &SpaceId,
        agent: &AgentNodeId,
        instructions: Vec<Option<String>>,
    ) -> Result<ForkPrep, String> {
        if let Some(why) = self.attempt_job_refusal(id) {
            return Err(why);
        }
        let space = self
            .active_space(id)
            .ok_or_else(|| format!("space {id} is not active"))?;
        let node = self
            .graphs
            .load(&space)?
            .graph()
            .node(agent)
            .cloned()
            .ok_or_else(|| format!("space {id} has no agent {agent}"))?;
        let repo = match &node.role {
            AgentRole::Writer { repo } => repo.clone(),
            _ => {
                return Err(format!(
                    "{agent} is not a writer; only writers can be forked"
                ));
            }
        };
        let worktree = space
            .repos
            .iter()
            .find(|r| r.name == repo)
            .map(|r| r.worktree.clone())
            .ok_or_else(|| format!("space {id} has no repo {repo}"))?;
        let slug = id.as_str();
        let set = self.attempts.load(id)?;
        let winner = node
            .session
            .as_deref()
            .and_then(|recorded| adopted_winner(set, slug, agent.as_str(), recorded))
            .map(|a| a.session.clone());
        let storage = self.sessions.as_ref();
        let source = fork_source(
            agent.as_str(),
            node.session.as_deref(),
            winner.as_deref(),
            |session| storage.is_some_and(|s| s.find(session).is_some()),
        )?;
        let plan =
            plan_fork_skipping_leftovers(set, slug, agent.as_str(), &repo, &worktree, instructions)
                .map_err(|e| format!("cannot fork {agent} in {id}: {e}"))?;
        Ok(ForkPrep {
            space,
            repo,
            plan,
            source,
        })
    }

    /// The worktrees of a fork were made: record them, each with a new report token, then start their agents
    /// (opening the space's tab when it is not open, which starts them), and end the fork job. When they cannot
    /// be recorded, the reason is returned (and shown) and they are removed again in the background; the job
    /// ends once they are, and whatever could not be removed is shown.
    fn record_forked(
        &mut self,
        id: &SpaceId,
        plan: &ForkPlan,
        worktrees: Vec<AttemptWorktree>,
        source: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let at = now();
        let made = worktrees
            .iter()
            .map(|_| new_attempt_token())
            .collect::<Result<Vec<String>, String>>()
            .and_then(|tokens| {
                self.attempts.change(id, |set| {
                    let mut made = set.record_fork(plan, worktrees.clone(), source, at)?;
                    for (attempt, token) in made.iter_mut().zip(tokens) {
                        set.set_token(attempt.number(), token.clone())?;
                        attempt.token = token;
                    }
                    Ok(made)
                })
            });
        let made = match made {
            Ok(made) => made,
            Err(e) => {
                let message = format!("could not record the attempts of {id}: {e}");
                self.error(message.clone(), cx);
                self.remove_unrecorded(id, worktrees, cx);
                return Err(message);
            }
        };
        self.attempt_jobs.remove(id);
        if self.tabs.contains_key(id) {
            for attempt in &made {
                self.spawn_attempt_terminal(id, attempt, window, cx);
            }
        } else {
            self.open_space(id, window, cx);
        }
        let count = made.len();
        let noun = if count == 1 { "attempt" } else { "attempts" };
        self.info(
            format!("Forked {count} {noun} of {} in {id}", plan.agent),
            cx,
        );
        Ok(())
    }

    /// Remove, in the background, the worktrees a fork of `id` made but could not record, as the fork job of the
    /// space (in [`Workspace::attempt_jobs`] until it is done, so quitting waits for it). Whatever cannot be
    /// removed is shown; later forks skip its numbers.
    fn remove_unrecorded(
        &mut self,
        id: &SpaceId,
        worktrees: Vec<AttemptWorktree>,
        cx: &mut Context<Self>,
    ) {
        self.attempt_jobs.insert(id.clone());
        let slug = id.as_str().to_owned();
        let executor = cx.background_executor().clone();
        let id = id.clone();
        cx.spawn(async move |this, cx| {
            let failures: Vec<String> = executor
                .spawn(async move {
                    worktrees
                        .iter()
                        .rev()
                        .filter_map(|worktree| {
                            remove_attempt(&slug, worktree).err().map(|e| {
                                format!(
                                    "attempt {} ({}): {e}",
                                    worktree.number,
                                    worktree.worktree.display()
                                )
                            })
                        })
                        .collect()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.attempt_jobs.remove(&id);
                if let Some(message) = unrecorded_cleanup_message(&id, &failures) {
                    this.error(message, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Approve the attempt proposal in Inbox item `item`: fork the attempts it asks for, and mark it approved
    /// once they are forked. It cannot be turned down while the fork runs.
    pub(crate) fn approve_attempt_proposal(
        &mut self,
        item: ItemId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(found) = self.inbox.as_ref().and_then(|inbox| inbox.get(&item)) else {
            return;
        };
        let ItemBody::AttemptProposal(proposal) = &found.body else {
            return;
        };
        if !found.is_pending() {
            return;
        }
        if self.widen_approvals.contains(&item) {
            self.info("this proposal is being approved already", cx);
            return;
        }
        let proposal = proposal.clone();
        let instructions = fork_instructions(proposal.count, &proposal.instructions);
        self.widen_approvals.insert(item.clone());
        let approved = item.clone();
        let started = self.fork_attempts_then(
            &proposal.space,
            &AgentNodeId::new(proposal.agent.clone()),
            instructions,
            move |this, forked, _window, cx| {
                this.widen_approvals.remove(&approved);
                if forked.is_ok()
                    && let Some(inbox) = this.inbox.as_mut()
                    && let Err(e) = inbox.approve(&approved)
                {
                    this.error(format!("could not update the Inbox: {e}"), cx);
                }
            },
            window,
            cx,
        );
        if started.is_err() {
            self.widen_approvals.remove(&item);
        }
        cx.notify();
    }

    // ---------------------------------------------------------------------------------------------------------
    // Adopt and discard

    /// Adopt attempt `number` of space `id` in the background: stop its agent and the space's writer agent and
    /// wait for both to exit, so the snapshot holds all the attempt's work and the writer writes nothing during
    /// the merge. Then merge the attempt into the space's branch (refused, changing nothing, when the space
    /// changed since the fork or the merge would conflict; the reason is given as git and xctx word it, and both
    /// agents start again). Then mark it adopted, remove its worktree, and continue the writer's pane from the
    /// winning session (`pi --fork <winner> --session-id xctx-<slug>-<agent>-adopted-<n>` in the space's
    /// worktree). When the adoption cannot be saved, nothing is removed: the error is shown, both agents start
    /// again, and adopting the same attempt again retries saving it (without merging twice). The other attempts
    /// stay for the user to discard. When the job ends, the review sheet hears how (see
    /// [`Workspace::attempt_job_done`]). Returns why it did not start, if it did not; every failure is also
    /// shown in the status bar.
    pub(crate) fn adopt_attempt(
        &mut self,
        id: &SpaceId,
        number: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let unsaved = self.unsaved_adoptions.get(id).cloned();
        let prepared = self.attempt_job_refusal(id).map_or_else(
            || {
                if let Some((other, _)) = &unsaved
                    && *other != number
                {
                    return Err(unsaved_adoption_refusal(id, *other));
                }
                let space = self
                    .active_space(id)
                    .ok_or_else(|| format!("space {id} is not active"))?;
                let attempt = self
                    .attempts
                    .load(id)?
                    .check_adopt(number)
                    .cloned()
                    .map_err(|e| format!("cannot adopt attempt {number} of {id}: {e}"))?;
                Ok((space, attempt))
            },
            Err,
        );
        let (space, attempt) = match prepared {
            Ok(prepared) => prepared,
            Err(e) => {
                self.error(e.clone(), cx);
                return Err(e);
            }
        };
        // Merged already, but not recorded: only the record (and the cleanup) is retried.
        let merged = unsaved.map(|(_, report)| report);
        self.attempt_jobs.insert(id.clone());
        // Both agents stop first: work either wrote during the merge would be lost.
        let session = self.stop_attempt_terminal(id, number, cx);
        let writer = self.writer_pane(id, &attempt.agent, cx);
        let writer_session = writer.as_ref().and_then(|(_, s)| s.clone());
        let executor = cx.background_executor().clone();
        let id = id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let worktree = attempt.worktree.clone();
            let (stopping, stopping_writer) = (session.clone(), writer_session.clone());
            let result = executor
                .spawn(async move {
                    if !stop_and_wait(stopping) {
                        return Err("its agent did not exit in time".to_owned());
                    }
                    if !stop_and_wait(stopping_writer) {
                        return Err("the writer's agent did not exit in time".to_owned());
                    }
                    match merged {
                        Some(report) => Ok(report),
                        None => {
                            xctx_core::adopt_attempt(&space, &worktree).map_err(|e| e.to_string())
                        }
                    }
                })
                .await;
            let report = this.update_in(cx, |this, window, cx| {
                // On any failure both agents carry on: the attempt in its worktree, the writer in its session.
                let restart = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    if this.tabs.contains_key(&id) {
                        this.restart_attempt_agent(&id, number, session.clone(), window, cx);
                        if let Some((pane, _)) = writer.clone() {
                            this.restart_writer(&id, pane, writer_session.clone(), window, cx);
                        }
                    }
                };
                match result {
                    Ok(report) => {
                        let marked = this
                            .attempts
                            .change(&id, |set| set.mark_adopted(number, now()).map(|_| ()));
                        match marked {
                            Ok(()) => {
                                this.unsaved_adoptions.remove(&id);
                                Some(report)
                            }
                            Err(e) => {
                                this.unsaved_adoptions
                                    .insert(id.clone(), (number, report));
                                this.attempt_jobs.remove(&id);
                                let message = unsaved_adoption_message(&id, number, &e);
                                this.error(message.clone(), cx);
                                restart(this, window, cx);
                                this.attempt_job_done(&id, number, Err(message), window, cx);
                                None
                            }
                        }
                    }
                    Err(e) => {
                        this.attempt_jobs.remove(&id);
                        let message = format!("could not adopt attempt {number} of {id}: {e}");
                        this.error(message.clone(), cx);
                        // Nothing was merged.
                        restart(this, window, cx);
                        this.attempt_job_done(&id, number, Err(message), window, cx);
                        None
                    }
                }
            });
            let Ok(Some(report)) = report else {
                return;
            };
            let slug = id.as_str().to_owned();
            let worktree = attempt.worktree.clone();
            let removed = executor
                .spawn(async move { remove_attempt(&slug, &worktree) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.attempt_jobs.remove(&id);
                this.continue_writer_from(&id, &attempt, window, cx);
                if let Err(e) = removed {
                    this.error(
                        format!("adopted attempt {number} into {id}, but could not remove its worktree: {e}"),
                        cx,
                    );
                }
                let how = if report.fast_forward {
                    "fast-forward"
                } else {
                    "merge commit"
                };
                let others = this
                    .attempts
                    .get(&id)
                    .map(|set| set.live().count())
                    .unwrap_or(0);
                let rest = match others {
                    0 => String::new(),
                    1 => "; 1 other attempt is still live, discard it on the review sheet".to_owned(),
                    n => format!(
                        "; {n} other attempts are still live, discard them on the review sheet"
                    ),
                };
                this.info(
                    format!("Adopted attempt {number} into {id} ({how}){rest}"),
                    cx,
                );
                this.attempt_job_done(&id, number, Ok(()), window, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
        Ok(())
    }

    /// After `attempt` was adopted: its writer runs `xctx-<slug>-<agent>-adopted-<n>` from now on, forked from the
    /// winning session the first time it starts (see [`Workspace::pane_session_of`]), and its pane, stopped for
    /// the merge, restarts in it now when the tab is open (in its old session when the new id cannot be saved),
    /// or the next time it opens.
    fn continue_writer_from(
        &mut self,
        id: &SpaceId,
        attempt: &Attempt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(space) = self.active_space(id) else {
            return;
        };
        let agent = AgentNodeId::new(attempt.agent.clone());
        let session = adopted_session_id(id.as_str(), &attempt.agent, attempt.number());
        let bound = self
            .graphs
            .load(&space)
            .map_err(xctx_control::RpcError::internal)
            .and_then(|state| {
                state.change(|doc| {
                    doc.graph
                        .set_session(&agent, Some(session.clone()))
                        .map_err(|e| graph_error(id, e))
                })
            });
        if let Err(e) = bound {
            self.error(
                format!(
                    "the writer of {id} could not switch to the adopted session: {}",
                    e.message
                ),
                cx,
            );
        }
        let Some((pane, _)) = self.writer_pane(id, &attempt.agent, cx) else {
            return;
        };
        if let Some(tab) = self.tabs.get_mut(id) {
            tab.terminals.remove(&pane.id);
        }
        self.spawn_pane(id, &pane, &AgentStart::Continue, window, cx);
    }

    /// Discard attempt `number` of space `id`: stop its agent and wait for it to exit, then remove its worktree,
    /// branch, mise file and scratch folder in the background, and mark it discarded. When removing fails, its
    /// agent starts again once the old one has exited. When the job ends, the review sheet hears how (see
    /// [`Workspace::attempt_job_done`]). Returns why it did not start, if it did not; every failure is also shown
    /// in the status bar.
    pub(crate) fn discard_attempt(
        &mut self,
        id: &SpaceId,
        number: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let prepared = self.attempt_job_refusal(id).map_or_else(
            || {
                if let Some((merged, _)) = self.unsaved_adoptions.get(id)
                    && *merged == number
                {
                    return Err(unsaved_adoption_refusal(id, number));
                }
                let attempt = self
                    .attempts
                    .load(id)?
                    .get(number)
                    .cloned()
                    .ok_or_else(|| format!("{id} has no attempt {number}"))?;
                if !attempt.state.is_live() {
                    return Err(format!(
                        "attempt {number} of {id} is {} already",
                        attempt.state.name()
                    ));
                }
                Ok(attempt)
            },
            Err,
        );
        let attempt = match prepared {
            Ok(attempt) => attempt,
            Err(e) => {
                self.error(e.clone(), cx);
                return Err(e);
            }
        };
        self.attempt_jobs.insert(id.clone());
        let session = self.stop_attempt_terminal(id, number, cx);
        let executor = cx.background_executor().clone();
        let id = id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let slug = id.as_str().to_owned();
            let worktree = attempt.worktree.clone();
            let stopping = session.clone();
            let result = executor
                .spawn(async move {
                    if !stop_and_wait(stopping) {
                        return Err("its agent did not exit in time".to_owned());
                    }
                    remove_attempt(&slug, &worktree).map_err(|e| e.to_string())
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.attempt_jobs.remove(&id);
                let outcome = match result {
                    Ok(()) => {
                        match this
                            .attempts
                            .change(&id, |set| set.mark_discarded(number, now()).map(|_| ()))
                        {
                            Ok(()) => {
                                this.info(format!("Discarded attempt {number} of {id}"), cx);
                                Ok(())
                            }
                            Err(e) => {
                                let message = format!("removed attempt {number} of {id}, but {e}");
                                this.error(message.clone(), cx);
                                Err(message)
                            }
                        }
                    }
                    Err(e) => {
                        let message = format!("could not discard attempt {number} of {id}: {e}");
                        this.error(message.clone(), cx);
                        if this.tabs.contains_key(&id) {
                            this.restart_attempt_agent(&id, number, session, window, cx);
                        }
                        Err(message)
                    }
                };
                this.attempt_job_done(&id, number, outcome, window, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
        Ok(())
    }
}
