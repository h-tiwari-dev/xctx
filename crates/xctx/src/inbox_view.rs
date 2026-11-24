//! The Inbox on Home: its entries, the Plan Gate card a Review opens (edit, approve and launch, or reject), and
//! the input that sets a missing mise variable for one space.

use std::path::{Path, PathBuf};

use gpui::{
    Context, Div, Entity, Focusable, FontWeight, ParentElement, SharedString, Styled, Subscription,
    Window, div, prelude::*,
};
use xctx_core::{DraftRepo, Redactor, RepoIndex, SpaceDraft, SpaceId, mise};
use xctx_inbox::{Filter, Inbox, InboxItem, ItemBody, ItemId, ItemKind, Source};
use xctx_layout::Shape;
use xctx_ui::{
    Button, ButtonVariant, InboxAction, InboxEntry, InboxKind, InboxPanel, PlanGateCard,
    PlanGateView, PlanRepo, PromptEvent, PromptInput, Theme,
};

use crate::lifecycle::{ADDED_BY_HAND, home_dir, repo_for_draft};
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

// -------------------------------------------------------------------------------------------------------------
// Pure helpers

/// The panel's kind for an Inbox item kind.
pub fn entry_kind(kind: ItemKind) -> InboxKind {
    match kind {
        ItemKind::PlanGate => InboxKind::PlanGate,
        ItemKind::SpaceWiden => InboxKind::SpaceWiden,
        ItemKind::MissingEnvVar => InboxKind::MissingEnvVar,
        ItemKind::Notice => InboxKind::Notice,
    }
}

/// The panel entries for the pending items, newest first, with secrets masked.
pub fn inbox_entries(inbox: &Inbox, redactor: &Redactor) -> Vec<InboxEntry> {
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
            .production(production);
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
        match action {
            InboxAction::Review => {
                self.plan_review = Some(id);
                cx.notify();
            }
            InboxAction::Approve => self.info(
                "Widening a running space is not supported yet; the request stays in the Inbox",
                cx,
            ),
            InboxAction::Reject => {
                self.change_inbox(cx, |inbox| inbox.reject(&id, None));
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

    fn edit_reviewed_plan(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut SpaceDraft)) {
        let Some(id) = self.plan_review.clone() else {
            return;
        };
        self.change_inbox(cx, |inbox| inbox.edit_plan(&id, edit));
    }

    /// Add the repo typed under the plan being reviewed.
    pub(crate) fn add_plan_repo(&mut self, input: String, cx: &mut Context<Self>) {
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

    /// Approve the plan being reviewed: launch it, and mark it approved once the space exists.
    fn approve_plan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.plan_launching = true;
        let started = self.start_launch(
            draft,
            window,
            cx,
            move |this, _, cx| {
                this.plan_launching = false;
                this.plan_review = None;
                this.change_inbox(cx, |inbox| inbox.approve(&id));
            },
            |this, _| this.plan_launching = false,
        );
        if !started {
            self.plan_launching = false;
        }
        cx.notify();
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
    /// item resolved and check the space with mise again.
    fn save_env_value(&mut self, value: String, cx: &mut Context<Self>) {
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
                    .busy(self.plan_launching || self.busy.is_some())
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
                    .bg(theme.elevated)
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
                                "Written to {file} in the space's worktree, which git ignores and Ship or Discard removes. The value is masked from agents from now on. Enter saves, Esc cancels."
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
            Some(inbox) => inbox_entries(inbox, &self.redactor),
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
    use xctx_inbox::{MissingEnvVar, Notice, PlanGate, SpaceWiden};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

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
        inbox.post(
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
        let entries = inbox_entries(&inbox, &redactor);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].kind, InboxKind::MissingEnvVar);
        assert!(entries[1].production && entries[1].space.as_deref() == Some("s"));
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
