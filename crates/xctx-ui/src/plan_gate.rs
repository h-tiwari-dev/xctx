//! The Plan Gate card: a proposed space, which the user edits and then approves (launching it) or rejects.
//! Nothing runs until Approve.

use std::rc::Rc;

use gpui::{AnyElement, App, FontWeight, SharedString, Window, div, prelude::*, px};
use xctx_layout::Shape;

use crate::components::{Badge, Button, ButtonSize, ButtonVariant, Pill};
use crate::tab_bar::{SHAPES, shape_label};
use crate::theme::{Theme, Tone};

/// A repo of the proposed space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRepo {
    /// The name shown.
    pub name: SharedString,
    /// Its checkout, passed back by [`PlanGateCard::on_drop_repo`].
    pub path: SharedString,
    /// Why it is in the plan.
    pub reason: SharedString,
}

/// What the card shows: one Plan Gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanGateView {
    /// The Inbox item's id.
    pub id: SharedString,
    /// The proposed title.
    pub title: SharedString,
    /// The task, as given.
    pub task: SharedString,
    /// The proposed shape.
    pub shape: Shape,
    /// The proposed repos.
    pub repos: Vec<PlanRepo>,
    /// Why it looks like this, from whoever drafted it.
    pub rationale: Option<SharedString>,
    /// Who drafted it, e.g. "the Parent Agent".
    pub proposed_by: SharedString,
}

/// What the card says while there is no repo to launch in.
pub const PLAN_NEEDS_REPO: &str = "Add a repo before approving; the plan has none.";

/// The line under the buttons.
pub const PLAN_GATE_HINT: &str =
    "Nothing runs until you approve. Each repo gets its own worktree on space/<slug>.";

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type ShapeHandler = Rc<dyn Fn(Shape, &mut Window, &mut App)>;
type RepoHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// The Plan Gate card. Approve is disabled while the plan has no repos or while `busy` (launching).
#[derive(IntoElement)]
pub struct PlanGateCard {
    plan: PlanGateView,
    busy: bool,
    add_repo: Option<AnyElement>,
    on_shape: Option<ShapeHandler>,
    on_drop_repo: Option<RepoHandler>,
    on_approve: Option<Handler>,
    on_reject: Option<Handler>,
    on_close: Option<Handler>,
}

impl PlanGateCard {
    /// A card for `plan` with no handlers.
    pub fn new(plan: PlanGateView) -> Self {
        Self {
            plan,
            busy: false,
            add_repo: None,
            on_shape: None,
            on_drop_repo: None,
            on_approve: None,
            on_reject: None,
            on_close: None,
        }
    }

    /// Show the plan as launching: every control is disabled.
    pub fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    /// Put `element` (a repo path input, say) under the repo list.
    pub fn add_repo(mut self, element: impl IntoElement) -> Self {
        self.add_repo = Some(element.into_any_element());
        self
    }

    /// Call `handler` with the shape pill clicked.
    pub fn on_shape(mut self, handler: impl Fn(Shape, &mut Window, &mut App) + 'static) -> Self {
        self.on_shape = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with a repo's path when its Drop button is clicked.
    pub fn on_drop_repo(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_drop_repo = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Approve & launch is clicked.
    pub fn on_approve(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_approve = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Reject is clicked.
    pub fn on_reject(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_reject = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Later is clicked, leaving the plan pending.
    pub fn on_close(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(handler));
        self
    }

    /// Whether Approve is enabled.
    pub fn can_approve(&self) -> bool {
        !self.busy && !self.plan.repos.is_empty()
    }
}

fn label(text: &'static str, theme: &Theme) -> gpui::Div {
    div()
        .w(px(64.0))
        .flex_none()
        .text_size(Theme::TEXT_XS)
        .text_color(theme.text_subtle)
        .child(text)
}

impl RenderOnce for PlanGateCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let can_approve = self.can_approve();
        let busy = self.busy;
        let plan = self.plan;

        let shapes = SHAPES.iter().map(|&shape| {
            let mut pill = Pill::new(
                SharedString::from(format!("plan-shape-{}", shape_label(shape))),
                shape_label(shape),
            )
            .selected(shape == plan.shape)
            .selector(format!("plan-shape-{}", shape_label(shape)));
            if let Some(handler) = self.on_shape.clone().filter(|_| !busy) {
                pill = pill.on_click(move |_, window, cx| handler(shape, window, cx));
            }
            pill
        });

        let repos: Vec<_> = plan
            .repos
            .iter()
            .enumerate()
            .map(|(i, repo)| {
                let mut drop = Button::new(SharedString::from(format!("plan-drop-{i}")), "Drop")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .disabled(busy)
                    .selector(format!("plan-drop-{i}"));
                if let Some(handler) = self.on_drop_repo.clone() {
                    let path = repo.path.clone();
                    drop = drop.on_click(move |_, window, cx| handler(&path, window, cx));
                }
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .py(Theme::SPACE_1)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .gap(Theme::SPACE_2)
                                    .items_center()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.text)
                                            .child(repo.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(Theme::TEXT_XS)
                                            .text_color(theme.text_subtle)
                                            .child(repo.path.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(Theme::TEXT_SM)
                                    .text_color(theme.text_muted)
                                    .child(repo.reason.clone()),
                            ),
                    )
                    .child(drop)
            })
            .collect();
        let no_repos = repos.is_empty().then(|| {
            div()
                .text_size(Theme::TEXT_SM)
                .text_color(theme.warning)
                .child(PLAN_NEEDS_REPO)
        });

        let rationale = plan.rationale.clone().map(|text| {
            div()
                .flex()
                .gap(Theme::SPACE_2)
                .child(label("Why", &theme))
                .child(
                    div()
                        .flex_1()
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_muted)
                        .child(text),
                )
        });

        let mut approve = Button::new("plan-approve", "Approve & launch")
            .variant(ButtonVariant::Primary)
            .size(ButtonSize::Large)
            .disabled(!can_approve)
            .selector("plan-approve");
        if let Some(handler) = self.on_approve {
            approve = approve.on_click(move |_, window, cx| handler(window, cx));
        }
        let mut reject = Button::new("plan-reject", "Reject")
            .variant(ButtonVariant::Danger)
            .size(ButtonSize::Large)
            .disabled(busy)
            .selector("plan-reject");
        if let Some(handler) = self.on_reject {
            reject = reject.on_click(move |_, window, cx| handler(window, cx));
        }
        let later = self.on_close.map(|handler| {
            Button::new("plan-later", "Later")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Large)
                .selector("plan-later")
                .on_click(move |_, window, cx| handler(window, cx))
        });

        div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_3)
            .w_full()
            .p(Theme::SPACE_4)
            .rounded(Theme::RADIUS_LG)
            .border_1()
            .border_color(theme.accent.alpha(0.55))
            .bg(theme.elevated)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(Badge::new("PLAN GATE").tone(Tone::Accent))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(Theme::TEXT_XL)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(plan.title.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_subtle)
                            .child(format!("proposed by {}", plan.proposed_by)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(Theme::SPACE_2)
                    .child(label("Task", &theme))
                    .child(
                        div()
                            .flex_1()
                            .text_size(Theme::TEXT_MD)
                            .text_color(theme.text)
                            .child(plan.task.clone()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(label("Shape", &theme))
                    .child(div().flex().gap(Theme::SPACE_1).children(shapes)),
            )
            .child(
                div()
                    .flex()
                    .gap(Theme::SPACE_2)
                    .child(label("Repos", &theme))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .children(repos)
                            .children(no_repos)
                            .children(self.add_repo),
                    ),
            )
            .children(rationale)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .pt(Theme::SPACE_2)
                    .border_t_1()
                    .border_color(theme.divider)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_subtle)
                            .child(PLAN_GATE_HINT),
                    )
                    .children(later)
                    .child(reject)
                    .child(approve),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(repos: usize) -> PlanGateView {
        PlanGateView {
            id: "inbox-1".into(),
            title: "add rate limiting".into(),
            task: "add rate limiting to the api".into(),
            shape: Shape::Dev,
            repos: (0..repos)
                .map(|i| PlanRepo {
                    name: format!("repo{i}").into(),
                    path: format!("/code/repo{i}").into(),
                    reason: "name matches".into(),
                })
                .collect(),
            rationale: None,
            proposed_by: "the Parent Agent".into(),
        }
    }

    #[test]
    fn approve_needs_a_repo_and_no_launch_in_progress() {
        assert!(!PlanGateCard::new(plan(0)).can_approve());
        assert!(PlanGateCard::new(plan(1)).can_approve());
        assert!(!PlanGateCard::new(plan(1)).busy(true).can_approve());
    }
}
