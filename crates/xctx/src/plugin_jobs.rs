//! Plugins in the live workspace: mounting and unmounting plugin panes, starting what they run, tool calls in
//! the background, write gates and their approval, plugin events routed to the loop and the shared space, and
//! the agent runner each agent pane runs.
//!
//! The decisions are made by [`PluginHost`] (GPUI-free, in `plugin_host.rs`); this applies them. The plugin
//! panes themselves read their data through the getters at the end ([`Workspace::plugin_http`] and the rest)
//! and act through the pane methods next to them.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gpui::{Context, Window};
use serde_json::{Map, Value, json};
use xctx_control::RpcError;
use xctx_core::{Space, SpaceId};
use xctx_graph::{AgentNodeId, LoopDecision};
use xctx_http::Request;
use xctx_inbox::{ItemBody, ItemId, Level, Notice, PluginGate, PluginGateKind, Source};
use xctx_layout::{PaneId, PaneKind, PaneRole, RolePane, SplitAxis};
use xctx_plugin::{
    CallPlan, Capability, Effect, Invocation, PaneSpec, RunnerError, RunnerManifest, check_scope,
};
use xctx_term_view::{TerminalView, TerminalViewEvent};

use crate::plugin_backend::BrowserSnapshot;
use crate::plugin_host::{
    CallJob, CallOutput, EventReport, HttpPaneData, LogsPaneData, LoopEffect, MountCheck,
    MountSettings, MountedPlugin, PendingMount, PendingWrite, Prepared, SqlPaneData, call_context,
    cut_chars, event_author, event_severity, pane_settings,
};
use crate::space_tab::PaneTerminal;
use crate::spawn::plugin_pane_launch;
use crate::workspace::Workspace;

/// How often the workspace looks for new log lines and the events tails matched.
pub const TAIL_TICK: Duration = Duration::from_millis(300);

/// The most characters of a write's result put in its Inbox notice.
pub const MAX_RESULT_NOTICE_CHARS: usize = 2000;

/// What mounting did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MountOutcome {
    /// The plugin's pane, mounted now.
    Mounted(PaneId),
    /// The gate posted to the Inbox; it mounts once the user approves.
    Gated(ItemId),
}

/// What to unmount.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UnmountTarget {
    /// Every pane of this plugin.
    Plugin(String),
    /// This plugin pane.
    Pane(PaneId),
}

/// What a tool call did.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum ToolCallResult {
    /// A read (or an approved write) ran; its output, masked.
    Output(CallOutput),
    /// A write waits in the Inbox, as this gate.
    Gated(ItemId),
}

/// What runs once a call finished, on the UI thread.
type CallDone =
    Box<dyn FnOnce(&mut Workspace, Result<ToolCallResult, RpcError>, &mut Context<Workspace>)>;

impl Workspace {
    // ---------------------------------------------------------------------------------------------------------
    // Registry

    /// Show the load problems of `space`'s plugins not shown before.
    pub(crate) fn show_plugin_problems(&mut self, space: &Space, cx: &mut Context<Self>) {
        if let Some(e) = self.plugins.trust_error.take() {
            self.error(format!("plugin trust: {e}"), cx);
        }
        let problems = self.plugins.new_problems(space);
        if let Some(first) = problems.first() {
            let more = problems.len() - 1;
            let message = if more == 0 {
                first.clone()
            } else {
                format!("{first} (and {more} more plugin problems)")
            };
            self.error(message, cx);
        }
        let runner_problems: Vec<String> = self
            .runners
            .problems()
            .iter()
            .map(|problem| match &problem.path {
                Some(path) => format!("runner {}: {}", path.display(), problem.error),
                None => format!("runner: {}", problem.error),
            })
            .collect();
        for message in runner_problems {
            if self.plugins.shown_once(&message) {
                self.error(message, cx);
            }
        }
    }

    /// The plugins mounted in space `id`, in mount order (for the Plugins menu and pane headers).
    pub(crate) fn mounted_plugins(&mut self, id: &SpaceId) -> Vec<MountedPlugin> {
        match self.active_space(id) {
            Some(space) => self.plugins.mounted(&space),
            None => Vec::new(),
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // Mount and unmount

    /// Mount `plugin` into the open space `id`, working in `repo` (the focused pane's repo when `None`), with
    /// `settings` chosen when mounting. Its pane is split from the focused pane, the layout and the mount are
    /// saved, and what it runs is started. An untrusted repo plugin posts a trust gate to the Inbox instead, and
    /// so does a connector with write tools when `from_agent` (an agent asked, through `plugin.mount`); either
    /// mounts once the user approves it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mount_plugin(
        &mut self,
        id: &SpaceId,
        plugin: &str,
        repo: Option<String>,
        settings: MountSettings,
        from_agent: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<MountOutcome, RpcError> {
        let space = self.open_tab_mut(id)?.space.clone();
        if let Some(repo) = &repo
            && !space.repos.iter().any(|r| &r.name == repo)
        {
            let names: Vec<&str> = space.repos.iter().map(|r| r.name.as_str()).collect();
            return Err(RpcError::invalid_params(format!(
                "space {id} has no repo {repo}; its repos are {}",
                names.join(", ")
            )));
        }
        let (check, _) = self.plugins.mount_check(&space, plugin, from_agent)?;
        let kind = match check {
            MountCheck::Ready => {
                return self
                    .mount_now(id, plugin, repo, settings, window, cx)
                    .map(MountOutcome::Mounted);
            }
            MountCheck::NeedsTrust { repo } => PluginGateKind::TrustRepoPlugin { repo },
            MountCheck::NeedsApproval(reason) => PluginGateKind::Mount { reason },
        };
        let gate = PluginGate {
            space: id.clone(),
            plugin: plugin.to_owned(),
            kind,
            rationale: None,
        };
        let item = self.post_plugin_gate(gate, cx)?;
        self.plugins.pending_mounts.insert(
            item.clone(),
            PendingMount {
                space: id.clone(),
                plugin: plugin.to_owned(),
                repo,
                settings,
            },
        );
        Ok(MountOutcome::Gated(item))
    }

    fn post_plugin_gate(
        &mut self,
        gate: PluginGate,
        cx: &mut Context<Self>,
    ) -> Result<ItemId, RpcError> {
        let space = gate.space.clone();
        let title = ItemBody::PluginGate(gate.clone());
        let Some(inbox) = self.inbox.as_mut() else {
            return Err(RpcError::internal(
                self.inbox_error
                    .clone()
                    .unwrap_or_else(|| "the Inbox is not available".to_owned()),
            ));
        };
        let item = inbox
            .post(title, Some(space), Source::Agent)
            .map_err(|e| RpcError::internal(format!("could not post to the Inbox: {e}")))?;
        let what = match &gate.kind {
            PluginGateKind::WriteCall { tool, .. } => format!("{tool} ({})", gate.plugin),
            PluginGateKind::Mount { .. } => format!("mounting {}", gate.plugin),
            PluginGateKind::TrustRepoPlugin { .. } => format!("trusting {}", gate.plugin),
        };
        self.info(
            format!(
                "{what} in {} is waiting in the Inbox for your approval",
                gate.space
            ),
            cx,
        );
        cx.notify();
        Ok(item)
    }

    /// Mount `plugin` now, with no gate (see [`Workspace::mount_plugin`]).
    fn mount_now(
        &mut self,
        id: &SpaceId,
        plugin: &str,
        repo: Option<String>,
        settings: MountSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<PaneId, RpcError> {
        let tab = self.open_tab_mut(id)?;
        let focused = tab.layout.focused();
        let repo = repo
            .or_else(|| tab.layout.role(focused).map(|r| r.repo.clone()))
            .or_else(|| tab.space.repos.first().map(|r| r.name.clone()))
            .unwrap_or_default();
        let pane = tab
            .layout
            .split_role(
                focused,
                SplitAxis::Horizontal,
                PaneRole::for_plugin(plugin, repo.clone()),
            )
            .map_err(|e| crate::space_tab::layout_error(id, e))?;
        if let Err(e) = self.plugins.add_mount(
            id,
            crate::plugin_host::Mount {
                pane: pane.id,
                plugin: plugin.to_owned(),
                repo,
                settings,
            },
        ) {
            self.error(format!("could not record the mount of {plugin}: {e}"), cx);
        }
        self.start_plugin_pane(id, &pane, window, cx);
        self.layout_changed(id, window, cx);
        self.info(format!("Mounted {plugin} in {id}"), cx);
        Ok(pane.id)
    }

    /// Unmount `target` from the open space `id`: close its panes (stopping what they run). Returns the panes
    /// closed. The space's last pane cannot be closed.
    pub(crate) fn unmount_plugin(
        &mut self,
        id: &SpaceId,
        target: UnmountTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Vec<PaneId>, RpcError> {
        let tab = self.open_tab_mut(id)?;
        let panes: Vec<PaneId> = tab
            .layout
            .panes()
            .into_iter()
            .filter(|p| p.role.kind == PaneKind::Plugin)
            .filter(|p| match &target {
                UnmountTarget::Plugin(name) => p.role.plugin.as_deref() == Some(name.as_str()),
                UnmountTarget::Pane(pane) => p.id == *pane,
            })
            .map(|p| p.id)
            .collect();
        if panes.is_empty() {
            return Err(RpcError::not_found(match target {
                UnmountTarget::Plugin(name) => format!("{name} is not mounted in {id}"),
                UnmountTarget::Pane(pane) => format!("space {id} has no plugin pane {}", pane.0),
            }));
        }
        let mut closed = Vec::new();
        for pane in panes {
            self.close_pane_in(id, pane, window, cx)?;
            closed.push(pane);
        }
        Ok(closed)
    }

    /// A plugin pane of `id` was closed: forget its mount and stop what it ran.
    pub(crate) fn plugin_pane_closed(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        cx: &mut Context<Self>,
    ) {
        if let Some(space) = self.active_space(id)
            && let Some(browser) = self.plugins.stop_backend(&space, pane)
        {
            cx.background_executor()
                .spawn(async move { browser.close() })
                .detach();
        }
        if let Err(e) = self.plugins.remove_mount(id, pane) {
            self.error(format!("could not record the unmount: {e}"), cx);
        }
    }

    /// Stop what the plugins of `id` run, as its tab closes.
    pub(crate) fn stop_space_plugins(&mut self, id: &SpaceId, cx: &mut Context<Self>) {
        if let Some(browser) = self.plugins.stop_space(id) {
            cx.background_executor()
                .spawn(async move { browser.close() })
                .detach();
        }
    }

    /// Start plugin pane `pane` of open space `id`: a terminal plugin's command in a terminal (no shell; the
    /// login shell when the plugin names none), a logs plugin's tail. HTTP, database and browser panes need
    /// nothing until they are used. A plugin that is not known or not trusted shows why in its pane.
    pub(crate) fn start_plugin_pane(
        &mut self,
        id: &SpaceId,
        pane: &RolePane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(space) = self.tabs.get(id).map(|tab| tab.space.clone()) else {
            return;
        };
        let name = pane.role.plugin.clone().unwrap_or_default();
        let registry = self.plugins.registry(&space);
        let fail = |this: &mut Self, message: String, cx: &mut Context<Self>| {
            this.error(message.clone(), cx);
            if let Some(tab) = this.tabs.get_mut(id) {
                tab.terminals.insert(pane.id, PaneTerminal::Failed(message));
            }
        };
        let Some(plugin) = registry.get(&name) else {
            fail(
                self,
                format!("the plugin {name:?} of pane {} is not installed", pane.id.0),
                cx,
            );
            return;
        };
        if !plugin.trusted {
            fail(
                self,
                format!(
                    "{name} is a repo plugin you have not trusted yet; mount it again to be asked"
                ),
                cx,
            );
            return;
        }
        let manifest = plugin.manifest.clone();
        let mount = match self.plugins.mount_of(id, &name, Some(pane.id)) {
            Ok(mount) => mount,
            Err(_) => crate::plugin_host::Mount {
                pane: pane.id,
                plugin: name.clone(),
                repo: pane.role.repo.clone(),
                settings: MountSettings::default(),
            },
        };
        match &manifest.pane {
            PaneSpec::Terminal { .. } => {
                let ctx = call_context(
                    &space,
                    &mount.repo,
                    &self.plugins.secrets(id),
                    &self.redactor,
                    &manifest,
                );
                let argv = match pane_settings(&manifest, &mount, &ctx) {
                    Ok(settings) => settings.terminal,
                    Err(e) => {
                        fail(self, format!("{name}: {e}"), cx);
                        return;
                    }
                };
                let launch = plugin_pane_launch(&space, &pane.role, argv.as_deref(), &self.search);
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
                        let pane_id = pane.id;
                        let subscription = cx.subscribe_in(
                            &view,
                            window,
                            move |this, _view, event: &TerminalViewEvent, window, cx| {
                                this.on_terminal_event(&space_id, pane_id, event, window, cx);
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
                        let message = format!("could not start {program} for {name}: {e}");
                        self.error(message.clone(), cx);
                        PaneTerminal::Failed(message)
                    }
                };
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.terminals.insert(pane.id, terminal);
                }
                if let Some(notice) = launch.notice {
                    self.info(notice, cx);
                }
            }
            PaneSpec::Logs { .. } => {
                let path = Some(self.search.pane_path(None));
                if let Some(e) =
                    self.plugins
                        .start_backend(&space, &mount, &manifest, &self.redactor, path)
                {
                    self.error(format!("{name}: {e}"), cx);
                }
                self.ensure_plugin_ticker(cx);
            }
            PaneSpec::Http { .. } | PaneSpec::Sql { .. } | PaneSpec::Web { .. } => {}
        }
        cx.notify();
    }

    /// Keep looking for new log lines and tail events while any logs pane tails something.
    fn ensure_plugin_ticker(&mut self, cx: &mut Context<Self>) {
        if self.plugin_ticker {
            return;
        }
        self.plugin_ticker = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TAIL_TICK).await;
                let keep_going = this.update(cx, |this, cx| {
                    for event in this.plugins.drain_tail_events(Instant::now()) {
                        let _ = this.fire_plugin_event(
                            &event.space,
                            &event.plugin,
                            &event.event,
                            &event.detail,
                            cx,
                        );
                    }
                    if this.plugins.dirty.swap(false, Ordering::Relaxed) {
                        this.plugin_panes.logs_changed();
                        cx.notify();
                    }
                    let tailing = this.plugins.any_tails();
                    if !tailing {
                        this.plugin_ticker = false;
                    }
                    tailing
                });
                if !matches!(keep_going, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    // ---------------------------------------------------------------------------------------------------------
    // Tool calls

    /// Call `plugin`'s `tool` with `args` in space `id` (through its pane `pane` when given, else its first),
    /// then call `done` on the UI thread with what happened. A read runs in the background and `done` gets its
    /// masked output; its events fire first. A write that is not `approved` posts a gate to the Inbox and `done`
    /// gets the gate at once; only approving it in the Inbox runs it (with `approved`). Nothing on the control
    /// socket passes `approved`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn call_plugin_tool(
        &mut self,
        id: &SpaceId,
        plugin: &str,
        tool: &str,
        args: Map<String, Value>,
        pane: Option<PaneId>,
        approved: bool,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<ToolCallResult, RpcError>, &mut Context<Self>) + 'static,
    ) {
        let Some(space) = self.active_space(id) else {
            done(
                self,
                Err(RpcError::not_found(format!("no active space named {id}"))),
                cx,
            );
            return;
        };
        let prepared =
            self.plugins
                .prepare_call(&space, plugin, tool, &args, &self.redactor, approved, pane);
        match prepared {
            Err(e) => done(self, Err(e), cx),
            Ok(Prepared::Gate(gate, pending)) => {
                let result = self.post_plugin_gate(gate, cx).map(|item| {
                    if let Some(pane) = pending.pane
                        && tool == "sql.write"
                    {
                        self.plugins
                            .runtime(id)
                            .sql
                            .entry(pane)
                            .or_default()
                            .write_pending = Some(item.clone());
                    }
                    self.plugins.pending_writes.insert(item.clone(), pending);
                    self.plugin_panes.changed();
                    ToolCallResult::Gated(item)
                });
                done(self, result, cx);
            }
            Ok(Prepared::Run(job)) => self.run_call_job(*job, Box::new(done), cx),
        }
    }

    /// Run `job` in the background; on the UI thread, apply what it changes in its pane, fire its events, then
    /// call `done`.
    fn run_call_job(&mut self, mut job: CallJob, done: CallDone, cx: &mut Context<Self>) {
        // A GUI app's own PATH lacks Homebrew and the user's tools; tools get the PATH panes get.
        job.plan
            .env
            .entry("PATH".to_owned())
            .or_insert_with(|| self.search.pane_path(None));
        let id = job.env.space.clone();
        let pane = job.env.pane;
        let plugin = job.plan.plugin.clone();
        let builtin = job.plan.builtin().map(str::to_owned).unwrap_or_default();
        self.set_pane_busy(&id, pane, &builtin, true);
        self.plugin_panes.changed();
        cx.notify();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let result = executor.spawn(async move { job.run() }).await;
            let _ = this.update(cx, |this, cx| {
                this.set_pane_busy(&id, pane, &builtin, false);
                this.plugin_panes.changed();
                let result = match result {
                    Ok(output) => {
                        this.plugins.apply_effect(&id, pane, &output.effect);
                        for (event, detail) in &output.events {
                            if let Err(e) = this.fire_plugin_event(&id, &plugin, event, detail, cx)
                            {
                                this.error(e.message, cx);
                            }
                        }
                        Ok(ToolCallResult::Output(output))
                    }
                    Err(message) => {
                        this.set_pane_error(&id, pane, &builtin, message.clone());
                        Err(RpcError::conflict(message))
                    }
                };
                done(this, result, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn set_pane_busy(&mut self, id: &SpaceId, pane: Option<PaneId>, builtin: &str, busy: bool) {
        let runtime = self.plugins.runtime(id);
        let counter = if builtin.starts_with("http.") {
            Some(&mut runtime.http_state.busy)
        } else if builtin.starts_with("sql.") {
            pane.map(|p| &mut runtime.sql.entry(p).or_default().busy)
        } else {
            None
        };
        if let Some(counter) = counter {
            *counter = if busy {
                counter.saturating_add(1)
            } else {
                counter.saturating_sub(1)
            };
        }
    }

    fn set_pane_error(
        &mut self,
        id: &SpaceId,
        pane: Option<PaneId>,
        builtin: &str,
        message: String,
    ) {
        let runtime = self.plugins.runtime(id);
        if builtin.starts_with("http.") {
            runtime.http_state.error = Some(message);
        } else if builtin.starts_with("sql.")
            && let Some(pane) = pane
        {
            runtime.sql.entry(pane).or_default().error = Some(message);
        }
    }

    /// Run built-in `builtin` with `args` for the first mounted pane of kind `kind` in `id` (or `pane`), as
    /// the plugin panes do (a send from the API client, a query from the database pane): checked against the
    /// plugin's scope like any call, but not against its tool list, so a pane can replay several times or reload
    /// a page. Errors show in the pane and the status bar.
    fn run_pane_builtin(
        &mut self,
        id: &SpaceId,
        pane: Option<PaneId>,
        kind: &str,
        builtin: &str,
        args: Map<String, Value>,
        cx: &mut Context<Self>,
    ) {
        match self.pane_job(id, pane, kind, builtin, args) {
            Ok(job) => self.run_call_job(
                job,
                Box::new(|this, result, cx| {
                    if let Err(e) = result {
                        this.error(e.message, cx);
                    }
                }),
                cx,
            ),
            Err(e) => {
                self.set_pane_error(id, pane, builtin, e.message.clone());
                self.plugin_panes.changed();
                self.error(e.message, cx);
            }
        }
    }

    fn pane_job(
        &mut self,
        id: &SpaceId,
        pane: Option<PaneId>,
        kind: &str,
        builtin: &str,
        args: Map<String, Value>,
    ) -> Result<CallJob, RpcError> {
        let space = self
            .active_space(id)
            .ok_or_else(|| RpcError::not_found(format!("no active space named {id}")))?;
        let registry = self.plugins.registry(&space);
        let mounted = self.plugins.mounted(&space);
        let mount = mounted
            .iter()
            .find(|m| m.kind == kind && pane.is_none_or(|p| p == m.pane))
            .ok_or_else(|| RpcError::conflict(format!("no {kind} plugin is mounted in {id}")))?;
        let plugin = registry
            .usable(&mount.plugin)
            .ok_or_else(|| RpcError::conflict(format!("{} is not usable", mount.plugin)))?;
        let manifest = plugin.manifest.clone();
        let ctx = call_context(
            &space,
            &mount.repo,
            &self.plugins.secrets(id),
            &self.redactor,
            &manifest,
        );
        check_scope(&manifest, &ctx).map_err(|e| RpcError::conflict(e.to_string()))?;
        // The plan of a pane action: the built-in, run as the plugin's read tools are.
        let plan = CallPlan {
            plugin: manifest.name.clone(),
            tool: builtin.to_owned(),
            effect: Effect::Read,
            invocation: Invocation::Builtin {
                name: builtin.to_owned(),
                args,
            },
            env: [
                ("MISE_ENV".to_owned(), ctx.mise_env.clone()),
                (xctx_plugin::SPACE_ENV.to_owned(), ctx.space.clone()),
                ("PORT".to_owned(), space.port.to_string()),
            ]
            .into_iter()
            .chain(ctx.secrets.clone())
            .collect(),
            cwd: ctx.worktree.clone(),
            redactor: ctx.redactor.clone(),
        };
        let env =
            self.plugins
                .call_env(&space, &mount.plugin, mount.pane, &manifest, &ctx, false)?;
        Ok(CallJob {
            plan,
            manifest,
            env,
        })
    }

    // ---------------------------------------------------------------------------------------------------------
    // Events

    /// Fire `plugin`'s `event` in space `id` with `detail` (see [`crate::plugin_host::PluginHost::fire_event`]):
    /// a finding in the shared space, the loop failed or moved on as the manifest maps it, an Inbox notice, and
    /// a line in the status bar. Repeats within a few seconds do nothing.
    pub(crate) fn fire_plugin_event(
        &mut self,
        id: &SpaceId,
        plugin: &str,
        event: &str,
        detail: &str,
        cx: &mut Context<Self>,
    ) -> Result<EventReport, RpcError> {
        let space = self
            .active_space(id)
            .ok_or_else(|| RpcError::not_found(format!("no active space named {id}")))?;
        let state = self.graphs.load(&space).ok();
        let report = self.plugins.fire_event(
            state,
            self.inbox.as_mut(),
            &space,
            plugin,
            event,
            detail,
            &self.redactor,
            Instant::now(),
        )?;
        if report.debounced {
            return Ok(report);
        }
        match report.loop_effect {
            LoopEffect::Blocked => self.error(format!("Loop stopped: {}", report.summary), cx),
            LoopEffect::Advanced(LoopDecision::NeedsApproval(_)) => self.info(
                format!("{}; the loop waits for your approval", report.summary),
                cx,
            ),
            LoopEffect::Advanced(_) => {
                self.info(format!("{}; the loop moved on", report.summary), cx)
            }
            LoopEffect::Unchanged if report.notice.is_some() => {
                self.info(format!("Inbox: {}", report.summary), cx)
            }
            LoopEffect::Unchanged => {}
        }
        for e in &report.errors {
            self.error(e.clone(), cx);
        }
        cx.notify();
        Ok(report)
    }

    // ---------------------------------------------------------------------------------------------------------
    // Gates

    /// Carry out an approved plugin gate: run the write (with `approved`) and post its result as a notice and a
    /// finding; mount the plugin; or record the trust (and mount, when a mount asked for it). The gate is marked
    /// approved once that is done; when it cannot start (the space is not open, the call's arguments were lost
    /// to a restart) it stays pending and the status bar says why.
    pub(crate) fn approve_plugin_gate(
        &mut self,
        item: ItemId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(found) = self.inbox.as_ref().and_then(|inbox| inbox.get(&item)) else {
            return;
        };
        let ItemBody::PluginGate(gate) = &found.body else {
            return;
        };
        if !found.is_pending() {
            return;
        }
        if self.widen_approvals.contains(&item) {
            self.info("this request is being approved already", cx);
            return;
        }
        let gate = gate.clone();
        let id = gate.space.clone();
        self.plugin_panes.changed();
        match &gate.kind {
            PluginGateKind::WriteCall { tool, args } => {
                let pending = self
                    .plugins
                    .pending_writes
                    .get(&item)
                    .cloned()
                    .or_else(|| restored_write(&gate, tool, args));
                let Some(pending) = pending else {
                    self.error(
                        "the arguments of this call were masked and xctx restarted since it was asked; reject it and ask again",
                        cx,
                    );
                    return;
                };
                self.widen_approvals.insert(item.clone());
                let (plugin, tool) = (pending.plugin.clone(), pending.tool.clone());
                let approved = item.clone();
                self.call_plugin_tool(
                    &id,
                    &pending.plugin,
                    &pending.tool,
                    pending.args.clone(),
                    pending.pane,
                    true,
                    cx,
                    move |this, result, cx| {
                        this.widen_approvals.remove(&approved);
                        this.plugins.pending_writes.remove(&approved);
                        this.plugins.clear_write_pending(&pending.space, &approved);
                        this.write_finished(&pending.space, &approved, &plugin, &tool, result, cx);
                    },
                );
            }
            PluginGateKind::Mount { .. } => {
                let pending = self.plugins.pending_mounts.get(&item).cloned();
                let (repo, settings) = pending.map(|p| (p.repo, p.settings)).unwrap_or_default();
                match self.mount_now(&id, &gate.plugin, repo, settings, window, cx) {
                    Ok(_) => {
                        self.plugins.pending_mounts.remove(&item);
                        self.settle_approved(&item, cx);
                    }
                    Err(e) => self.error(
                        format!("could not mount {}: {}", gate.plugin, e.message),
                        cx,
                    ),
                }
            }
            PluginGateKind::TrustRepoPlugin { .. } => {
                let Some(space) = self.active_space(&id) else {
                    self.error(format!("space {id} is not active"), cx);
                    return;
                };
                if let Err(e) = self.plugins.trust_repo_plugin(&space, &gate.plugin) {
                    self.error(format!("could not trust {}: {e}", gate.plugin), cx);
                    return;
                }
                self.settle_approved(&item, cx);
                match self.plugins.pending_mounts.remove(&item) {
                    Some(pending) if self.tabs.contains_key(&id) => {
                        if let Err(e) = self.mount_now(
                            &id,
                            &gate.plugin,
                            pending.repo,
                            pending.settings,
                            window,
                            cx,
                        ) {
                            self.error(
                                format!("could not mount {}: {}", gate.plugin, e.message),
                                cx,
                            );
                        }
                    }
                    _ => self.info(
                        format!(
                            "Trusted {}; mount it from the Plugins menu (⌘⇧P)",
                            gate.plugin
                        ),
                        cx,
                    ),
                }
            }
        }
        cx.notify();
    }

    /// The user turned down a plugin gate: forget the call or mount it held.
    pub(crate) fn plugin_gate_rejected(&mut self, item: &ItemId) {
        self.plugin_panes.changed();
        if let Some(pending) = self.plugins.pending_writes.remove(item) {
            self.plugins.clear_write_pending(&pending.space, item);
        }
        self.plugins.pending_mounts.remove(item);
    }

    fn settle_approved(&mut self, item: &ItemId, cx: &mut Context<Self>) {
        if let Some(inbox) = self.inbox.as_mut()
            && let Err(e) = inbox.approve(item)
        {
            self.error(format!("could not update the Inbox: {e}"), cx);
        }
    }

    /// An approved write finished: settle its gate, and post what it did as a notice and a finding.
    fn write_finished(
        &mut self,
        id: &SpaceId,
        item: &ItemId,
        plugin: &str,
        tool: &str,
        result: Result<ToolCallResult, RpcError>,
        cx: &mut Context<Self>,
    ) {
        self.settle_approved(item, cx);
        let (notice, ok) = match &result {
            Ok(ToolCallResult::Output(out)) => (
                Notice {
                    title: format!("Ran {tool} ({plugin}) in {id}"),
                    body: Some(cut_chars(&out.text, MAX_RESULT_NOTICE_CHARS)),
                    level: Level::Info,
                },
                true,
            ),
            Ok(ToolCallResult::Gated(_)) => return,
            Err(e) => (
                Notice {
                    title: format!("{tool} ({plugin}) failed in {id}"),
                    body: Some(cut_chars(&e.message, MAX_RESULT_NOTICE_CHARS)),
                    level: Level::Error,
                },
                false,
            ),
        };
        let summary = format!(
            "{}: {}",
            notice.title,
            notice.body.clone().unwrap_or_default()
        );
        if let Some(space) = self.active_space(id)
            && let Ok(state) = self.graphs.load(&space)
        {
            let severity = if ok {
                event_severity(None)
            } else {
                event_severity(Some(xctx_plugin::LoopAction::Fail))
            };
            let _ = state.shared_mut().add_finding(
                &event_author(plugin),
                severity,
                None,
                None,
                &cut_chars(&summary, MAX_RESULT_NOTICE_CHARS),
            );
        }
        let title = notice.title.clone();
        if let Some(inbox) = self.inbox.as_mut()
            && let Err(e) = inbox.post(ItemBody::Notice(notice), Some(id.clone()), Source::App)
        {
            self.error(format!("could not post to the Inbox: {e}"), cx);
        }
        if ok {
            self.info(title, cx);
        } else {
            self.error(title, cx);
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // Runners

    /// The runner chosen for agent `agent` of `space` (its lane's, else the space's), `None` for `pi`. Fails
    /// when the chosen runner is not installed.
    pub(crate) fn lane_runner(
        &mut self,
        space: &Space,
        agent: &AgentNodeId,
    ) -> Result<Option<RunnerManifest>, String> {
        let Ok(state) = self.graphs.load(space) else {
            return Ok(None);
        };
        let chosen = state
            .graph()
            .node(agent)
            .and_then(|n| n.runner.clone())
            .or_else(|| state.doc().runner.clone());
        self.runner_named(chosen)
    }

    fn runner_named(&self, chosen: Option<String>) -> Result<Option<RunnerManifest>, String> {
        match chosen {
            None => Ok(None),
            Some(name) => self
                .runners
                .get(&name)
                .map(|r| Some(r.manifest.clone()))
                .ok_or_else(|| format!("the runner {name} is not installed")),
        }
    }

    /// The runner agent pane `pane` of `space` runs (see [`Workspace::lane_runner`]), with a notice when the one
    /// chosen is not installed and `pi` runs instead.
    pub(crate) fn pane_runner(
        &mut self,
        space: &Space,
        pane: &RolePane,
    ) -> (Option<RunnerManifest>, Option<String>) {
        let Ok(state) = self.graphs.load(space) else {
            return (None, None);
        };
        let chosen = state
            .graph()
            .nodes()
            .iter()
            .find(|n| n.pane_id == Some(pane.id.0))
            .and_then(|n| n.runner.clone())
            .or_else(|| state.doc().runner.clone());
        match self.runner_named(chosen) {
            Ok(runner) => (runner, None),
            Err(e) => (
                None,
                Some(format!("{e}; the {} agent runs pi", pane.role.repo)),
            ),
        }
    }

    /// Why writer `agent` of `space` cannot be forked into attempts because of its runner, if it cannot: the
    /// runner's own "cannot fork" message.
    pub(crate) fn fork_refusal(&mut self, space: &Space, agent: &AgentNodeId) -> Option<String> {
        let state = self.graphs.load(space).ok()?;
        let chosen = state
            .graph()
            .node(agent)
            .and_then(|n| n.runner.clone())
            .or_else(|| state.doc().runner.clone());
        runner_fork_refusal(chosen.as_deref(), &self.runners)
    }

    /// Why each writer of the loaded graph `state` cannot be forked because of its runner, for the writers that
    /// cannot: the attempt entry points hide or disable Fork for them, with this as the reason.
    pub(crate) fn fork_refusals_in(
        &self,
        state: &crate::space_graph::SpaceGraphState,
    ) -> std::collections::HashMap<AgentNodeId, String> {
        let space_runner = state.doc().runner.as_deref();
        state
            .graph()
            .writers()
            .filter_map(|w| {
                let chosen = w.runner.as_deref().or(space_runner);
                runner_fork_refusal(chosen, &self.runners).map(|why| (w.id.clone(), why))
            })
            .collect()
    }
}

/// Why a writer running the runner named `chosen` (its lane's, else its space's; `None` for `pi`, which forks)
/// cannot be forked into attempts, if it cannot: the runner is not installed, or its own "cannot fork
/// sessions" message.
pub(crate) fn runner_fork_refusal(
    chosen: Option<&str>,
    runners: &xctx_plugin::RunnerRegistry,
) -> Option<String> {
    let name = chosen?;
    match runners.get(name) {
        None => Some(format!("the runner {name} is not installed")),
        Some(runner) if !runner.manifest.can_fork() => Some(
            RunnerError::Unsupported {
                runner: runner.manifest.name.clone(),
                capability: Capability::ForkSession,
            }
            .to_string(),
        ),
        Some(_) => None,
    }
}

/// The plugin panes' side: what they show and what they do. The panes (`plugin_panes.rs`) read these before a
/// frame and call them from their buttons; the Plugins menu (`plugin_menu_dialog.rs`) lists the plugins.
impl Workspace {
    /// Choose the runner of space `id` (`agent` `None`) or of one of its lanes, `None` for the default (`pi`,
    /// or the space's). The change applies the next time the agent's pane starts. Refused for a runner that is
    /// not installed.
    #[allow(dead_code)] // No caller yet: the runner picker and its control method are still to come.
    pub(crate) fn set_runner(
        &mut self,
        id: &SpaceId,
        agent: Option<&str>,
        runner: Option<&str>,
    ) -> Result<(), String> {
        if let Some(name) = runner
            && self.runners.get(name).is_none()
        {
            let names: Vec<&str> = self.runners.runners().iter().map(|r| r.name()).collect();
            return Err(format!(
                "no runner named {name}; runners: {}",
                names.join(", ")
            ));
        }
        let space = self
            .active_space(id)
            .ok_or_else(|| format!("space {id} is not active"))?;
        let state = self.graphs.load(&space)?;
        let runner = runner.map(str::to_owned);
        state
            .change(|doc| match agent {
                None => {
                    doc.runner = runner;
                    Ok(())
                }
                Some(agent) => doc
                    .graph
                    .set_runner(&AgentNodeId::new(agent), runner)
                    .map_err(|e| crate::space_graph::graph_error(id, e)),
            })
            .map_err(|e| e.message)
    }

    /// The plugins `space` can use, for the Plugins menu.
    pub(crate) fn plugin_menu_entries(
        &self,
        space: &Space,
    ) -> Vec<xctx_control::protocol::PluginSummary> {
        self.plugins
            .registry(space)
            .plugins()
            .iter()
            .map(|p| crate::plugin_host::plugin_summary(p, Some(space), Vec::new()))
            .collect()
    }

    /// What the API client pane of space `id` shows: its history (masked), the selected exchange, the diff, and
    /// whether a send runs.
    pub(crate) fn plugin_http(&mut self, id: &SpaceId) -> HttpPaneData {
        match self.active_space(id) {
            Some(space) => self.plugins.http_data(&space, &self.redactor),
            None => HttpPaneData::default(),
        }
    }

    /// What logs pane `pane` of space `id` shows: its lines (masked), what it tails, and its status.
    pub(crate) fn plugin_logs(&self, id: &SpaceId, pane: PaneId) -> Option<LogsPaneData> {
        self.plugins.logs_data(id, pane, &self.redactor)
    }

    /// What database pane `pane` of space `id` shows: its database, the last result or error, and whether a
    /// query runs or a write waits in the Inbox.
    pub(crate) fn plugin_sql(&mut self, id: &SpaceId, pane: PaneId) -> SqlPaneData {
        match self.active_space(id) {
            Some(space) => self.plugins.sql_data(&space, pane, &self.redactor),
            None => SqlPaneData::default(),
        }
    }

    /// What the browser pane of space `id` shows: where Chrome is, the page, its last screenshot (PNG bytes
    /// with a generation counter), console and failed requests, all masked.
    pub(crate) fn plugin_browser(&self, id: &SpaceId) -> BrowserSnapshot {
        self.plugins.browser_data(id)
    }

    /// Select exchange `exchange` in the API client of `id`.
    pub(crate) fn http_select(
        &mut self,
        id: &SpaceId,
        exchange: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        let state = &mut self.plugins.runtime(id).http_state;
        state.selected = exchange;
        state.diff = None;
        self.plugin_panes.changed();
        cx.notify();
    }

    /// Send `request` from the API client of `id` (a relative URL is joined to the pane's base URL).
    pub(crate) fn http_send(&mut self, id: &SpaceId, request: Request, cx: &mut Context<Self>) {
        let mut args = Map::new();
        args.insert("method".into(), json!(request.method));
        args.insert("path".into(), json!(request.url));
        let headers: Vec<String> = request
            .headers
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect();
        args.insert("headers".into(), json!(headers));
        if let Some(body) = request.body {
            args.insert("body".into(), json!(body));
        }
        self.run_pane_builtin(id, None, "http", "http.send", args, cx);
    }

    /// Send history entry `exchange` again, `times` times (`POST /checkout ×11`).
    pub(crate) fn http_replay(
        &mut self,
        id: &SpaceId,
        exchange: u64,
        times: u32,
        cx: &mut Context<Self>,
    ) {
        let mut args = Map::new();
        args.insert("id".into(), json!(exchange));
        args.insert("times".into(), json!(times));
        self.run_pane_builtin(id, None, "http", "http.replay", args, cx);
    }

    /// Diff history entries `a` and `b` of `id`'s API client; the pane then shows the diff.
    pub(crate) fn http_diff(&mut self, id: &SpaceId, a: u64, b: u64, cx: &mut Context<Self>) {
        let mut args = Map::new();
        args.insert("from".into(), json!(a));
        args.insert("to".into(), json!(b));
        self.run_pane_builtin(id, None, "http", "http.diff", args, cx);
    }

    /// Run a read-only `query` in database pane `pane` of `id` (the first database pane when `None`).
    pub(crate) fn sql_run(
        &mut self,
        id: &SpaceId,
        pane: Option<PaneId>,
        query: &str,
        cx: &mut Context<Self>,
    ) {
        let mut args = Map::new();
        args.insert("query".into(), json!(query));
        self.run_pane_builtin(id, pane, "sql", "sql.read", args, cx);
    }

    /// Ask to run `statement`, which changes data, in database pane `pane` of `id`: a gate waits in the Inbox
    /// and the pane shows it waiting; it runs only once the user approves it there.
    pub(crate) fn sql_request_write(
        &mut self,
        id: &SpaceId,
        pane: Option<PaneId>,
        statement: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(mount) = self
            .mounted_plugins(id)
            .into_iter()
            .find(|m| m.kind == "sql" && pane.is_none_or(|p| p == m.pane))
        else {
            self.error(format!("no database plugin is mounted in {id}"), cx);
            return;
        };
        let mut args = Map::new();
        args.insert("statement".into(), json!(statement));
        args.insert("reason".into(), json!("asked from the database pane"));
        self.call_plugin_tool(
            id,
            &mount.plugin,
            "sql.write",
            args,
            Some(mount.pane),
            false,
            cx,
            |this, result, cx| {
                if let Err(e) = result {
                    this.error(e.message, cx);
                }
            },
        );
    }

    /// Open `url` (a path of the space's app, or a full local URL) in the browser of `id`, starting Chrome the
    /// first time.
    pub(crate) fn browser_open(&mut self, id: &SpaceId, url: &str, cx: &mut Context<Self>) {
        let mut args = Map::new();
        args.insert("path".into(), json!(url));
        self.run_pane_builtin(id, None, "web", "page.open", args, cx);
    }

    /// Reload the page open in the browser of `id`, and take a new screenshot.
    pub(crate) fn browser_reload(&mut self, id: &SpaceId, cx: &mut Context<Self>) {
        self.run_pane_builtin(id, None, "web", "page.reload", Map::new(), cx);
    }

    /// Drop the lines of logs pane `pane` of `id`.
    pub(crate) fn logs_clear(&mut self, id: &SpaceId, pane: PaneId, cx: &mut Context<Self>) {
        if self.plugins.logs_clear(id, pane) {
            self.plugin_panes.changed();
            cx.notify();
        }
    }
}

/// The pending write behind a gate whose call was not kept (xctx restarted): its shown arguments, when they are
/// a JSON object with nothing masked in them.
fn restored_write(gate: &PluginGate, tool: &str, args: &str) -> Option<PendingWrite> {
    if args.contains("[REDACTED:") || args.ends_with('…') {
        return None;
    }
    match serde_json::from_str::<Value>(args).ok()? {
        Value::Object(map) => Some(PendingWrite {
            space: gate.space.clone(),
            plugin: gate.plugin.clone(),
            tool: tool.to_owned(),
            args: map,
            pane: None,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(args: &str) -> (PluginGate, String) {
        (
            PluginGate {
                space: SpaceId::new("s"),
                plugin: "database".into(),
                kind: PluginGateKind::WriteCall {
                    tool: "sql.write".into(),
                    args: args.into(),
                },
                rationale: None,
            },
            args.to_owned(),
        )
    }

    #[test]
    fn a_write_survives_a_restart_only_when_nothing_was_masked() {
        let (g, args) = gate(r#"{"statement":"DELETE FROM t","reason":"r"}"#);
        let restored = restored_write(&g, "sql.write", &args);
        assert_eq!(
            restored.map(|p| p.args.get("statement").cloned()),
            Some(Some(json!("DELETE FROM t")))
        );
        let (g, args) = gate(r#"{"statement":"UPDATE t SET pw='[REDACTED:PW]'"}"#);
        assert!(restored_write(&g, "sql.write", &args).is_none());
        let (g, args) = gate("not json");
        assert!(restored_write(&g, "sql.write", &args).is_none());
    }
}
