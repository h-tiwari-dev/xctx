//! The Inbox on Home: its entries, the Plan Gate card a Review opens (edit, approve and launch, or reject), and
//! the input that sets a missing mise variable for one space.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui::{
    Context, Div, Entity, Focusable, FontWeight, ParentElement, SharedString, Styled, Subscription,
    Window, div, prelude::*,
};
use xctx_control::RpcError;
use xctx_core::{
    DraftRepo, Redactor, RepoIndex, Space, SpaceDraft, SpaceId, WidenReport, WidenRequest, mise,
};
use xctx_inbox::{Filter, Inbox, InboxItem, ItemBody, ItemId, ItemKind, Source, SpaceWiden};
use xctx_layout::{PaneKind, Shape, SplitAxis};
use xctx_ui::{
    Button, ButtonVariant, InboxAction, InboxEntry, InboxKind, InboxPanel, PlanGateCard,
    PlanGateView, PlanRepo, PromptEvent, PromptInput, Theme,
};

use crate::lifecycle::{ADDED_BY_HAND, home_dir, repo_for_draft};
use crate::space_graph::add_writer_lanes;
use crate::space_layout::{layout_shape, space_shape};
use crate::workspace::Workspace;

/// The Set value input for one missing variable.
pub(crate) struct EnvPrompt {
    item: ItemId,
    name: String,
    repo: String,
    space: SpaceId,
    dir: PathBuf,
    input: Entity<PromptInput>,
    _subscription: Subscription,
}

/// The Plan Gates launched from this window, so none launches twice or changes while it launches. A plan stays
/// launched once its space exists, even if marking it approved in the Inbox failed.
#[derive(Debug, Default)]
pub(crate) struct PlanLaunches {
    /// The plan launching now.
    launching: Option<ItemId>,
    /// The plans whose launch started and did not fail.
    launched: HashSet<ItemId>,
}

impl PlanLaunches {
    /// Whether a plan is launching, so the reviewed plan cannot be edited or approved.
    pub(crate) fn is_launching(&self) -> bool {
        self.launching.is_some()
    }

    /// Start launching plan `id`. Refused, with why, while another plan launches or when `id` was launched
    /// already.
    pub(crate) fn begin(&mut self, id: &ItemId) -> Result<(), String> {
        if self.launching.is_some() {
            return Err("a plan is launching; wait until it finishes".to_owned());
        }
        if !self.launched.insert(id.clone()) {
            return Err(
                "this plan was launched already; reject or dismiss it instead of approving it again"
                    .to_owned(),
            );
        }
        self.launching = Some(id.clone());
        Ok(())
    }

    /// The launch of `id` created its space: it stays launched.
    pub(crate) fn succeeded(&mut self, id: &ItemId) {
        if self.launching.as_ref() == Some(id) {
            self.launching = None;
        }
    }

    /// The launch of `id` failed or never started: it may be approved again.
    pub(crate) fn failed(&mut self, id: &ItemId) {
        if self.launching.as_ref() == Some(id) {
            self.launching = None;
        }
        self.launched.remove(id);
    }
}

// -------------------------------------------------------------------------------------------------------------
// Pure helpers

/// Why `value` typed for the mise variable `name` cannot be saved, if it cannot: an empty (or blank) value would
/// leave the variable effectively unset while marking it resolved.
pub fn env_value_error(name: &str, value: &str) -> Option<String> {
    value
        .trim()
        .is_empty()
        .then(|| format!("{name} needs a value; type it, or press Esc to cancel"))
}

/// The panel's kind for an Inbox item kind.
pub fn entry_kind(kind: ItemKind) -> InboxKind {
    match kind {
        ItemKind::PlanGate => InboxKind::PlanGate,
        ItemKind::SpaceWiden => InboxKind::SpaceWiden,
        ItemKind::AttemptProposal => InboxKind::AttemptProposal,
        ItemKind::PluginGate => InboxKind::PluginGate,
        ItemKind::MissingEnvVar => InboxKind::MissingEnvVar,
        ItemKind::Notice => InboxKind::Notice,
    }
}

/// The panel entries for the pending items, newest first, with secrets masked. The widenings in `approving`
/// (being approved now) are drawn busy, so they cannot be approved or denied again until it finishes.
pub fn inbox_entries(
    inbox: &Inbox,
    redactor: &Redactor,
    approving: &HashSet<ItemId>,
) -> Vec<InboxEntry> {
    inbox
        .list(&Filter::default())
        .into_iter()
        .map(|item| {
            let preview = item.preview(redactor);
            let production = matches!(&item.body, ItemBody::SpaceWiden(w) if w.production);
            let mut entry = InboxEntry::new(
                preview.id.as_str().to_owned(),
                entry_kind(preview.kind),
                preview.title,
            )
            .detail(preview.detail)
            .production(production)
            .busy(approving.contains(&item.id));
            if let Some(space) = preview.space {
                entry = entry.space(space.as_str().to_owned());
            }
            entry
        })
        .collect()
}

/// Who an item came from, as the Plan Gate card says it.
pub fn proposed_by(source: Source) -> &'static str {
    match source {
        Source::ParentAgent => "the Parent Agent",
        Source::Agent => "an agent",
        Source::User => "you",
        Source::App => "xctx",
    }
}

/// The card for a Plan Gate item, with secrets masked; `None` for other kinds.
pub fn plan_view(item: &InboxItem, redactor: &Redactor) -> Option<PlanGateView> {
    let ItemBody::PlanGate(plan) = &item.body else {
        return None;
    };
    let masked = |text: &str| SharedString::from(redactor.redact(text).into_owned());
    Some(PlanGateView {
        id: item.id.as_str().to_owned().into(),
        title: masked(&plan.draft.title),
        task: masked(&plan.draft.task),
        shape: layout_shape(plan.draft.shape),
        repos: plan
            .draft
            .repos
            .iter()
            .map(|r| PlanRepo {
                name: r.name.clone().into(),
                path: r.path.to_string_lossy().into_owned().into(),
                reason: masked(&r.reason),
            })
            .collect(),
        rationale: plan.rationale.as_deref().map(masked),
        proposed_by: proposed_by(item.source).into(),
    })
}

/// The repo the user typed under a plan: an indexed repo's name, else the path of a git repository.
pub fn repo_from_input(
    index: &RepoIndex,
    input: &str,
    home: Option<&Path>,
) -> Result<DraftRepo, String> {
    let input = input.trim();
    if let Some(found) = index.repos().iter().find(|r| r.name == input) {
        return Ok(DraftRepo::new(found.path.clone(), ADDED_BY_HAND));
    }
    repo_for_draft(input, home)
}

/// Drop the repo whose path displays as `path` from `draft`.
pub fn drop_repo_by_display(draft: &mut SpaceDraft, path: &str) {
    draft.repos.retain(|r| r.path.to_string_lossy() != path);
}

/// What `xctx_core::widen` is asked to do for an approved widening.
pub fn widen_request(widen: &SpaceWiden) -> WidenRequest {
    WidenRequest {
        repos: widen.add_repos.clone(),
        production: widen.production,
    }
}

/// Why the widening in Inbox item `item` of space `space` cannot be approved now, if it cannot: the same request
/// is being approved already (`approving`), or the space is not active or is being shipped or discarded.
pub fn widen_refusal(
    approving: &HashSet<ItemId>,
    item: &ItemId,
    spaces: &[Space],
    in_flight: impl Fn(&SpaceId) -> bool,
    space: &SpaceId,
) -> Option<String> {
    if approving.contains(item) {
        return Some(format!(
            "the request to widen {space} is being approved already"
        ));
    }
    if in_flight(space) {
        return Some(format!(
            "{space} is being shipped or discarded; it cannot be widened"
        ));
    }
    if !spaces.iter().any(|s| &s.id == space) {
        return Some(format!(
            "{space} is not an active space; it cannot be widened"
        ));
    }
    None
}

/// Why Inbox item `item` cannot be denied or dismissed now, if it cannot: it is a widening being approved
/// (`approving`), and turning it down while its worktrees are being added would leave the space widened under a
/// denied request.
pub fn settle_refusal(approving: &HashSet<ItemId>, item: &ItemId) -> Option<String> {
    approving.contains(item).then(|| {
        "this request is being approved; wait until it finishes before denying or dismissing it"
            .to_owned()
    })
}

/// What approving a widening ends with, from what `xctx_core::widen` returned. A space that already has
/// everything asked for ([`xctx_core::Error::EmptyWiden`], say because an earlier approval widened it but
/// marking the request approved failed) counts as widened with nothing added, so the request is settled instead
/// of staying pending for good. Other errors are kept.
pub fn widen_outcome(
    result: xctx_core::Result<WidenReport>,
) -> Result<WidenReport, xctx_core::Error> {
    match result {
        Err(xctx_core::Error::EmptyWiden(_)) => Ok(WidenReport {
            added_repos: Vec::new(),
            production_enabled: false,
        }),
        other => other,
    }
}

/// What the status bar says once space `id` is widened; `production_requested` is whether the widening asked
/// for production (a space may have run with it before).
pub fn widen_message(id: &SpaceId, report: &WidenReport, production_requested: bool) -> String {
    let mut parts = Vec::new();
    if !report.added_repos.is_empty() {
        let names: Vec<&str> = report.added_repos.iter().map(|r| r.name.as_str()).collect();
        parts.push(format!("added {}", names.join(", ")));
    }
    if production_requested && report.production_enabled {
        parts.push(
            "new panes run with MISE_ENV=production; panes started before now need a restart"
                .to_owned(),
        );
    }
    if parts.is_empty() {
        format!("{id} already had everything asked for")
    } else {
        format!("Widened {id}: {}", parts.join("; "))
    }
}

// -------------------------------------------------------------------------------------------------------------
// The workspace side

impl Workspace {
    /// How many Inbox items wait for the user.
    pub(crate) fn inbox_pending(&self) -> usize {
        self.inbox.as_ref().map_or(0, Inbox::pending_count)
    }

    /// Run `change` on the Inbox and show its error. Returns its result.
    fn change_inbox<T>(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut Inbox) -> xctx_inbox::Result<T>,
    ) -> Option<T> {
        let Some(inbox) = self.inbox.as_mut() else {
            let message = self
                .inbox_error
                .clone()
                .unwrap_or_else(|| "the Inbox is not available".to_owned());
            self.error(message, cx);
            return None;
        };
        match change(inbox) {
            Ok(value) => {
                cx.notify();
                Some(value)
            }
            Err(e) => {
                self.error(e.to_string(), cx);
                None
            }
        }
    }

    /// What the user chose for an Inbox entry.
    pub(crate) fn on_inbox_action(
        &mut self,
        id: &str,
        action: InboxAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = ItemId::new(id);
        if matches!(action, InboxAction::Reject | InboxAction::Dismiss)
            && let Some(why) = settle_refusal(&self.widen_approvals, &id)
        {
            self.info(why, cx);
            return;
        }
        match action {
            InboxAction::Review => {
                self.plan_review = Some(id);
                cx.notify();
            }
            InboxAction::Approve => {
                let kind = self
                    .inbox
                    .as_ref()
                    .and_then(|i| i.get(&id))
                    .map(|i| i.kind());
                if kind == Some(ItemKind::AttemptProposal) {
                    self.approve_attempt_proposal(id, window, cx);
                } else if kind == Some(ItemKind::PluginGate) {
                    self.approve_plugin_gate(id, window, cx);
                } else {
                    self.approve_widen(id, window, cx);
                }
            }
            InboxAction::Reject => {
                if self
                    .change_inbox(cx, |inbox| inbox.reject(&id, None))
                    .is_some()
                {
                    self.plugin_gate_rejected(&id);
                }
            }
            InboxAction::Dismiss => {
                self.change_inbox(cx, |inbox| inbox.dismiss(&id));
                if self.env_prompt.as_ref().is_some_and(|p| p.item == id) {
                    self.env_prompt = None;
                }
            }
            InboxAction::Resolve => self.open_env_prompt(id, window, cx),
        }
    }

    /// Expand or collapse an entry's detail.
    pub(crate) fn toggle_inbox_entry(&mut self, id: &SharedString, cx: &mut Context<Self>) {
        self.inbox_expanded = if self.inbox_expanded.as_ref() == Some(id) {
            None
        } else {
            Some(id.clone())
        };
        cx.notify();
    }

    /// The Plan Gate under review, while it is still pending.
    fn reviewed_plan(&self) -> Option<&InboxItem> {
        let id = self.plan_review.as_ref()?;
        self.inbox
            .as_ref()?
            .get(id)
            .filter(|item| item.is_pending() && item.kind() == ItemKind::PlanGate)
    }

    /// Edit the plan being reviewed. Does nothing while a plan launches, so what is stored is what ran.
    fn edit_reviewed_plan(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut SpaceDraft)) {
        if self.plan_launches.is_launching() {
            return;
        }
        let Some(id) = self.plan_review.clone() else {
            return;
        };
        self.change_inbox(cx, |inbox| inbox.edit_plan(&id, edit));
    }

    /// Add the repo typed under the plan being reviewed. Does nothing while a plan launches.
    pub(crate) fn add_plan_repo(&mut self, input: String, cx: &mut Context<Self>) {
        if self.plan_launches.is_launching() {
            return;
        }
        match repo_from_input(&self.index, &input, home_dir().as_deref()) {
            Ok(repo) => {
                self.plan_repo_input.update(cx, |input, cx| input.clear(cx));
                let name = repo.name.clone();
                let mut added = true;
                self.edit_reviewed_plan(cx, |draft| added = draft.add_repo(repo));
                if !added {
                    self.info(format!("{name} is already in the plan"), cx);
                }
            }
            Err(e) => self.error(e, cx),
        }
    }

    /// Approve the plan being reviewed: launch it, and mark it approved once the space exists. Does nothing while
    /// a plan launches, and refuses a plan launched already (see [`PlanLaunches`]).
    fn approve_plan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.plan_launches.is_launching() {
            return;
        }
        let Some(item) = self.reviewed_plan() else {
            return;
        };
        let ItemBody::PlanGate(plan) = &item.body else {
            return;
        };
        if plan.draft.repos.is_empty() {
            self.error("add a repo to the plan before approving it", cx);
            return;
        }
        let (id, draft) = (item.id.clone(), plan.draft.clone());
        if let Err(why) = self.plan_launches.begin(&id) {
            self.error(why, cx);
            return;
        }
        let (launched_id, failed_id) = (id.clone(), id.clone());
        let started = self.start_launch(
            draft,
            window,
            cx,
            move |this, _, cx| {
                this.plan_launches.succeeded(&launched_id);
                if this.plan_review.as_ref() == Some(&launched_id) {
                    this.plan_review = None;
                }
                this.change_inbox(cx, |inbox| inbox.approve(&launched_id));
            },
            move |this, _| this.plan_launches.failed(&failed_id),
        );
        if !started {
            self.plan_launches.failed(&id);
        }
        cx.notify();
    }

    /// Approve a request to widen a space: add the worktrees (and switch to production) in the background, then
    /// mark it approved, give each new repo a writer and, when the space is open, an agent pane, and check the
    /// space with mise again. Refused while the same request is being approved or the space is not active. A
    /// space that already has everything asked for settles the request (see [`widen_outcome`]).
    fn approve_widen(&mut self, id: ItemId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.inbox.as_ref().and_then(|inbox| inbox.get(&id)) else {
            return;
        };
        let ItemBody::SpaceWiden(widen) = &item.body else {
            self.info("only requests to widen a space are approved here", cx);
            return;
        };
        if !item.is_pending() {
            return;
        }
        let widen = widen.clone();
        let space_id = widen.space.clone();
        if let Some(why) = widen_refusal(
            &self.widen_approvals,
            &id,
            &self.spaces,
            |s| self.in_flight_spaces.contains(s),
            &space_id,
        ) {
            self.error(why, cx);
            return;
        }
        let Some(mut store) = self.take_store(&format!("widening {space_id}"), cx) else {
            return;
        };
        self.widen_approvals.insert(id.clone());
        let request = widen_request(&widen);
        let production = widen.production;
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            let job_id = space_id.clone();
            let (store, result) = executor
                .spawn(async move {
                    let result = xctx_core::widen(&mut store, &job_id, request);
                    (store, result)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.return_store(store, cx);
                this.widen_approvals.remove(&id);
                match widen_outcome(result) {
                    Ok(report) => {
                        this.space_widened(&id, &space_id, &report, production, window, cx)
                    }
                    Err(e) => this.error(format!("could not widen {space_id}: {e}"), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The widening of `space_id` asked for by Inbox item `item` is done: mark it approved, give each added repo a
    /// writer and (when the space is open) an agent pane, check the space with mise again, and say what changed.
    fn space_widened(
        &mut self,
        item: &ItemId,
        space_id: &SpaceId,
        report: &WidenReport,
        production: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_inbox(cx, |inbox| inbox.approve(item));
        let Some(space) = self.spaces.iter().find(|s| &s.id == space_id).cloned() else {
            return;
        };
        let open = match self.tabs.get_mut(space_id) {
            Some(tab) => {
                tab.space = space.clone();
                true
            }
            None => false,
        };
        let lanes = self
            .graphs
            .load(&space)
            .map_err(RpcError::internal)
            .and_then(|state| add_writer_lanes(state, space_id, &report.added_repos));
        if let Err(e) = lanes {
            self.error(e.message, cx);
        }
        if open {
            for repo in &report.added_repos {
                if let Err(e) = self.split_in(
                    space_id,
                    None,
                    SplitAxis::Horizontal,
                    PaneKind::Agent,
                    Some(repo.name.clone()),
                    window,
                    cx,
                ) {
                    self.show_op_error(e, cx);
                }
            }
        }
        self.check_mise(vec![space], cx);
        self.info(widen_message(space_id, report, production), cx);
    }

    fn reject_plan(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.plan_review.take() else {
            return;
        };
        self.change_inbox(cx, |inbox| inbox.reject(&id, None));
    }

    /// Open the input that sets a missing variable for its space.
    fn open_env_prompt(&mut self, id: ItemId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.inbox.as_ref().and_then(|inbox| inbox.get(&id)) else {
            return;
        };
        let ItemBody::MissingEnvVar(missing) = &item.body else {
            return;
        };
        let Some(space) = item.space.clone() else {
            self.info(
                format!(
                    "{} is not for a space; set it in your shell or mise.local.toml",
                    missing.var.name
                ),
                cx,
            );
            return;
        };
        let (name, repo, dir) = (
            missing.var.name.clone(),
            missing.repo.clone(),
            missing.dir.clone(),
        );
        let input = cx.new(|cx| PromptInput::new(cx).placeholder(format!("Value for {name}")));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &PromptEvent, _, cx| match event {
                PromptEvent::Submit(value) => this.save_env_value(value.clone(), cx),
                PromptEvent::Cancel => {
                    this.env_prompt = None;
                    cx.notify();
                }
            },
        );
        let focus = input.focus_handle(cx);
        self.env_prompt = Some(EnvPrompt {
            item: id,
            name,
            repo,
            space,
            dir,
            input,
            _subscription: subscription,
        });
        focus.focus(window, cx);
        cx.notify();
    }

    /// Write the value typed for a missing variable into its space's mise file, mask it from now on, mark the
    /// item resolved and check the space with mise again. An empty value is refused and the input stays open.
    fn save_env_value(&mut self, value: String, cx: &mut Context<Self>) {
        let Some(prompt) = self.env_prompt.as_ref() else {
            return;
        };
        if let Some(why) = env_value_error(&prompt.name, &value) {
            self.error(why, cx);
            return;
        }
        let Some(prompt) = self.env_prompt.take() else {
            return;
        };
        if let Err(e) =
            mise::set_space_var(&prompt.dir, prompt.space.as_str(), &prompt.name, &value)
        {
            self.error(format!("could not set {}: {e}", prompt.name), cx);
            self.env_prompt = Some(prompt);
            return;
        }
        self.redactor.extend([(prompt.name.clone(), value)]);
        let item = prompt.item.clone();
        self.change_inbox(cx, |inbox| inbox.resolve(&item));
        self.info(
            format!(
                "{} set for {} in {}; panes started before now need a restart to see it",
                prompt.name, prompt.repo, prompt.space
            ),
            cx,
        );
        if let Some(space) = self.spaces.iter().find(|s| s.id == prompt.space).cloned() {
            self.check_mise(vec![space], cx);
        }
        cx.notify();
    }

    /// Home's Inbox section: the plan under review, the Set value input, and the entries.
    pub(crate) fn render_inbox(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx);
        let mut section = div().flex().flex_col().gap(Theme::SPACE_3);

        if let Some(view) = self
            .reviewed_plan()
            .and_then(|item| plan_view(item, &self.redactor))
        {
            let entity = cx.entity().downgrade();
            let (shape, drop, approve, reject, later) = (
                entity.clone(),
                entity.clone(),
                entity.clone(),
                entity.clone(),
                entity,
            );
            section = section.child(
                PlanGateCard::new(view)
                    .busy(self.plan_launches.is_launching() || self.busy.is_some())
                    .add_repo(div().pt(Theme::SPACE_1).child(self.plan_repo_input.clone()))
                    .on_shape(move |s: Shape, _, cx| {
                        let _ = shape.update(cx, |this, cx| {
                            this.edit_reviewed_plan(cx, |draft| draft.shape = space_shape(s));
                        });
                    })
                    .on_drop_repo(move |path, _, cx| {
                        let path = path.to_string();
                        let _ = drop.update(cx, |this, cx| {
                            this.edit_reviewed_plan(cx, |draft| drop_repo_by_display(draft, &path));
                        });
                    })
                    .on_approve(move |window, cx| {
                        let _ = approve.update(cx, |this, cx| this.approve_plan(window, cx));
                    })
                    .on_reject(move |_, cx| {
                        let _ = reject.update(cx, |this, cx| this.reject_plan(cx));
                    })
                    .on_close(move |_, cx| {
                        let _ = later.update(cx, |this, cx| {
                            this.plan_review = None;
                            cx.notify();
                        });
                    }),
            );
        }

        if let Some(prompt) = &self.env_prompt {
            let file = mise::mise_file_name(prompt.space.as_str());
            section = section.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(Theme::SPACE_2)
                    .p(Theme::SPACE_3)
                    .rounded(Theme::RADIUS_MD)
                    .border_1()
                    .border_color(theme.danger.alpha(0.5))
                    .bg(theme.raised)
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("Set {} for {} in {}", prompt.name, prompt.repo, prompt.space)),
                    )
                    .child(
                        div()
                            .text_size(Theme::TEXT_SM)
                            .text_color(theme.text_muted)
                            .child(format!(
                                "Written to {file} in the space's worktree (git ignores it; Ship or Discard removes it) and marked redact = true. xctx masks the value in control responses and Inbox previews, also after a restart; the file itself is plain text the space's agents can read. Enter saves, Esc cancels."
                            )),
                    )
                    .child(prompt.input.clone())
                    .child(
                        div().flex().justify_end().child(
                            Button::new("env-cancel", "Cancel")
                                .variant(ButtonVariant::Ghost)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.env_prompt = None;
                                    cx.notify();
                                })),
                        ),
                    ),
            );
        }

        let entries = match &self.inbox {
            Some(inbox) => inbox_entries(inbox, &self.redactor, &self.widen_approvals),
            None => Vec::new(),
        };
        let entity = cx.entity().downgrade();
        let toggle = entity.clone();
        section.child(
            InboxPanel::new()
                .entries(entries)
                .expanded(self.inbox_expanded.clone())
                .on_action(move |id, action, window, cx| {
                    let id = id.to_string();
                    let _ =
                        entity.update(cx, |this, cx| this.on_inbox_action(&id, action, window, cx));
                })
                .on_toggle(move |id, _, cx| {
                    let id = id.clone();
                    let _ = toggle.update(cx, |this, cx| this.toggle_inbox_entry(&id, cx));
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use xctx_core::RequiredVar;
    use xctx_inbox::{MissingEnvVar, Notice, PlanGate};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn widen_requests_carry_repos_and_production_and_say_what_changed() {
        let widen = SpaceWiden {
            space: SpaceId::new("s"),
            add_repos: vec![DraftRepo::new("/code/web", "named by the agent")],
            production: true,
            reason: "needs prod data".into(),
        };
        let request = widen_request(&widen);
        assert_eq!(request.repos, widen.add_repos);
        assert!(request.production);

        let id = SpaceId::new("s");
        let report = WidenReport {
            added_repos: Vec::new(),
            production_enabled: true,
        };
        assert_eq!(
            widen_message(&id, &report, true),
            "Widened s: new panes run with MISE_ENV=production; panes started before now need a restart"
        );
        assert_eq!(
            widen_message(&id, &report, false),
            "s already had everything asked for"
        );
    }

    #[test]
    fn widening_is_refused_twice_or_for_spaces_that_are_not_active() {
        let id = SpaceId::new("s");
        let item = ItemId::new("i1");
        let none = HashSet::new();
        assert!(
            widen_refusal(&none, &item, &[], |_| false, &id)
                .is_some_and(|w| w.contains("not an active space"))
        );
        let busy = HashSet::from([item.clone()]);
        assert!(
            widen_refusal(&busy, &item, &[], |_| false, &id).is_some_and(|w| w.contains("already"))
        );
        assert!(
            widen_refusal(&none, &item, &[], |_| true, &id).is_some_and(|w| w.contains("shipped"))
        );
    }

    #[test]
    fn a_widening_being_approved_cannot_be_denied_or_dismissed() {
        let item = ItemId::new("inbox-1");
        let approving = HashSet::from([item.clone()]);
        assert!(settle_refusal(&approving, &item).is_some_and(|w| w.contains("being approved")));
        assert_eq!(settle_refusal(&approving, &ItemId::new("inbox-2")), None);
        assert_eq!(settle_refusal(&HashSet::new(), &item), None);
    }

    #[test]
    fn a_space_that_already_has_everything_settles_the_widening() {
        let id = SpaceId::new("s");
        let settled = widen_outcome(Err(xctx_core::Error::EmptyWiden(id.clone())));
        let report = settled.ok();
        assert!(
            report
                .as_ref()
                .is_some_and(|r| r.added_repos.is_empty() && !r.production_enabled)
        );
        if let Some(report) = report {
            assert_eq!(
                widen_message(&id, &report, true),
                "s already had everything asked for"
            );
        }
        let failed = widen_outcome(Err(xctx_core::Error::SpaceNotFound(id.clone())));
        assert!(matches!(failed, Err(xctx_core::Error::SpaceNotFound(_))));
    }

    #[test]
    fn entries_are_pending_redacted_and_marked() -> TestResult {
        let dir = tempfile::tempdir()?;
        let mut inbox = Inbox::open(dir.path())?;
        let notice = inbox.post(
            ItemBody::Notice(Notice {
                title: "used tok-55555".into(),
                body: None,
                level: Default::default(),
            }),
            None,
            Source::Agent,
        )?;
        let widen = inbox.post(
            ItemBody::SpaceWiden(SpaceWiden {
                space: SpaceId::new("s"),
                add_repos: vec![],
                production: true,
                reason: "repro".into(),
            }),
            Some(SpaceId::new("s")),
            Source::Agent,
        )?;
        inbox.post(
            ItemBody::MissingEnvVar(MissingEnvVar {
                repo: "api".into(),
                dir: "/code/api".into(),
                mise_env: None,
                var: RequiredVar {
                    name: "K".into(),
                    help: None,
                    required_in: None,
                },
            }),
            None,
            Source::App,
        )?;
        let settled = inbox.post(
            ItemBody::Notice(Notice {
                title: "old".into(),
                body: None,
                level: Default::default(),
            }),
            None,
            Source::Agent,
        )?;
        inbox.dismiss(&settled)?;
        let redactor = Redactor::new([("T".to_owned(), "tok-55555".to_owned())]);
        let entries = inbox_entries(&inbox, &redactor, &HashSet::new());
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].kind, InboxKind::MissingEnvVar);
        assert!(entries[1].production && entries[1].space.as_deref() == Some("s"));
        assert!(entries.iter().all(|e| !e.busy));
        let approving = HashSet::from([widen]);
        let entries = inbox_entries(&inbox, &redactor, &approving);
        let busy: Vec<bool> = entries.iter().map(|e| e.busy).collect();
        assert_eq!(busy, [false, true, false]);
        assert_eq!(entries[2].id.as_ref(), notice.as_str());
        assert_eq!(entries[2].title.as_ref(), "used [REDACTED:T]");
        Ok(())
    }

    #[test]
    fn a_plan_view_masks_secrets_and_says_who_proposed_it() -> TestResult {
        let dir = tempfile::tempdir()?;
        let mut inbox = Inbox::open(dir.path())?;
        let mut draft = SpaceDraft::new("call the api with tok-55555");
        draft.add_repo(DraftRepo::new("/code/api", "name matches"));
        let id = inbox.post(
            ItemBody::PlanGate(PlanGate {
                draft,
                rationale: Some("why".into()),
            }),
            None,
            Source::ParentAgent,
        )?;
        let redactor = Redactor::new([("T".to_owned(), "tok-55555".to_owned())]);
        let item = inbox.get(&id).ok_or("posted")?;
        let view = plan_view(item, &redactor).ok_or("a plan")?;
        assert_eq!(view.task.as_ref(), "call the api with [REDACTED:T]");
        assert_eq!(view.proposed_by.as_ref(), "the Parent Agent");
        assert_eq!(view.repos[0].path.as_ref(), "/code/api");
        assert_eq!(view.shape, Shape::Dev);
        Ok(())
    }

    #[test]
    fn an_empty_env_value_is_refused() {
        assert!(env_value_error("API_KEY", "").is_some_and(|e| e.contains("API_KEY")));
        assert!(env_value_error("API_KEY", "  \t").is_some());
        assert_eq!(env_value_error("API_KEY", "tok-1"), None);
    }

    #[test]
    fn a_plan_launches_once_and_only_one_at_a_time() {
        let (a, b) = (ItemId::new("inbox-1"), ItemId::new("inbox-2"));
        let mut launches = PlanLaunches::default();
        assert!(!launches.is_launching());
        assert!(launches.begin(&a).is_ok());
        assert!(launches.is_launching());
        // A second click, or another plan, while it launches.
        assert!(launches.begin(&a).is_err());
        assert!(launches.begin(&b).is_err());
        launches.succeeded(&a);
        assert!(!launches.is_launching());
        // Launched, even if marking it approved failed: never again.
        assert!(
            launches
                .begin(&a)
                .is_err_and(|e| e.contains("launched already"))
        );
        assert!(!launches.is_launching());
        assert!(launches.begin(&b).is_ok());
    }

    #[test]
    fn a_failed_plan_launch_may_be_approved_again() {
        let id = ItemId::new("inbox-1");
        let mut launches = PlanLaunches::default();
        assert!(launches.begin(&id).is_ok());
        launches.failed(&id);
        assert!(!launches.is_launching());
        assert!(launches.begin(&id).is_ok());
    }

    #[test]
    fn repos_drop_by_their_displayed_path_and_unknown_input_is_an_error() {
        let mut draft = SpaceDraft::new("x");
        draft.add_repo(DraftRepo::new("/code/api", ""));
        draft.add_repo(DraftRepo::new("/code/web", ""));
        drop_repo_by_display(&mut draft, "/code/api");
        assert_eq!(draft.repos.len(), 1);
        assert_eq!(draft.repos[0].name, "web");
        assert!(repo_from_input(&RepoIndex::default(), "nope-not-a-repo", None).is_err());
    }
}
