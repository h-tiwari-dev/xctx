//! The Plan Gate card: a proposed space, which the user edits and then launches (approving it) or rejects.
//! Nothing runs until Launch.

use std::rc::Rc;

use gpui::{AnyElement, App, FontWeight, SharedString, Styled, Window, div, prelude::*, px};
use xctx_layout::Shape;

use crate::components::{Badge, Button, ButtonSize, ButtonVariant, Pill};
use crate::icon::{Icon, IconName, IconSize};
use crate::theme::{Theme, Tone};
use crate::title_bar::{SHAPES, shape_label};

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
pub const PLAN_NEEDS_REPO: &str = "Add a repo before launching; the plan has none.";

/// The line under the buttons.
pub const PLAN_GATE_HINT: &str =
    "Nothing runs until you launch. Each repo gets its own worktree on space/<slug>.";

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type ShapeHandler = Rc<dyn Fn(Shape, &mut Window, &mut App)>;
type RepoHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// The Plan Gate card. Approve is disabled while the plan has no repos or while `busy` (launching).
#[derive(IntoElement)]
pub struct PlanGateCard {
    plan: PlanGateView,
    busy: bool,
    add_open: bool,
    add_repo: Option<AnyElement>,
    on_toggle_add: Option<Handler>,
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
            add_open: false,
            add_repo: None,
            on_toggle_add: None,
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

    /// Whether the repo picker (the element given to [`PlanGateCard::add_repo`]) is shown. It is always shown while
    /// the plan has no repo.
    pub fn add_open(mut self, open: bool) -> Self {
        self.add_open = open;
        self
    }

    /// Call `handler` when Add repo (or Done) is clicked.
    pub fn on_toggle_add(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle_add = Some(Rc::new(handler));
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

    /// Call `handler` when Launch is clicked.
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
        .w(px(48.0))
        .flex_none()
        .pt(px(2.0))
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
        let no_repos = plan.repos.is_empty();

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

        // One chip per repo: its name, where it is, and a button that drops it.
        let chips: Vec<_> = plan
            .repos
            .iter()
            .enumerate()
            .map(|(i, repo)| {
                let mut drop = Button::new(SharedString::from(format!("plan-drop-{i}")), "")
                    .icon(IconName::Close)
                    .tooltip("Drop this repo")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .rounded_full()
                    .disabled(busy)
                    .selector(format!("plan-drop-{i}"));
                if let Some(handler) = self.on_drop_repo.clone() {
                    let path = repo.path.clone();
                    drop = drop.on_click(move |_, window, cx| handler(&path, window, cx));
                }
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .max_w_full()
                    .h(px(26.0))
                    .pl(Theme::SPACE_2)
                    .rounded(Theme::RADIUS_FULL)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.raised)
                    .child(
                        Icon::new(IconName::Folder)
                            .size(IconSize::Xs)
                            .color(theme.text_muted),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(Theme::TEXT_SM)
                            .text_color(theme.text)
                            .child(repo.name.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(260.0))
                            .truncate()
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_subtle)
                            .child(repo.path.clone()),
                    )
                    .child(drop)
            })
            .collect();

        let add_label = if self.add_open || no_repos {
            "Done"
        } else {
            "Add repo"
        };
        let mut add_toggle = Button::new("plan-add-toggle", add_label)
            .icon(if self.add_open || no_repos {
                IconName::Close
            } else {
                IconName::Plus
            })
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Compact)
            .disabled(busy || no_repos)
            .selector("plan-add-toggle");
        if let Some(handler) = self.on_toggle_add.clone() {
            add_toggle = add_toggle.on_click(move |_, window, cx| handler(window, cx));
        }
        let picker = (self.add_open || no_repos)
            .then_some(self.add_repo)
            .flatten();
        let needs_repo = no_repos.then(|| {
            div()
                .text_size(Theme::TEXT_SM)
                .text_color(theme.warning)
                .child(PLAN_NEEDS_REPO)
        });

        let rationale = plan.rationale.clone().map(|text| {
            div()
                .flex()
                .gap(Theme::SPACE_2)
                .min_w_0()
                .child(label("Why", &theme))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_muted)
                        .child(text),
                )
        });

        let mut approve = Button::new("plan-approve", "Launch ⌘↵")
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

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::SPACE_2)
            .min_w_0()
            .px(Theme::SPACE_4)
            .py(Theme::SPACE_3)
            .border_b_1()
            .border_color(theme.border_subtle)
            .child(Badge::new("PLAN GATE").tone(Tone::Accent))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(Theme::TEXT_LG)
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
            );
        let body = div()
            .id("plan-gate-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .gap(Theme::SPACE_3)
            .p(Theme::SPACE_4)
            .child(
                div()
                    .min_w_0()
                    .text_size(Theme::TEXT_MD)
                    .text_color(theme.text)
                    .child(plan.task.clone()),
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
                    .min_w_0()
                    .child(label("Repos", &theme))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(Theme::SPACE_2)
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap(Theme::SPACE_1)
                                    .children(chips)
                                    .child(add_toggle),
                            )
                            .children(needs_repo)
                            .children(picker),
                    ),
            )
            .children(rationale);
        let footer = div()
            .debug_selector(|| "plan-gate-footer".to_owned())
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap(Theme::SPACE_2)
            .px(Theme::SPACE_4)
            .py(Theme::SPACE_3)
            .border_t_1()
            .border_color(theme.border_subtle)
            .bg(theme.raised)
            .child(
                div()
                    .flex_1()
                    .min_w(px(200.0))
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_subtle)
                    .child(PLAN_GATE_HINT),
            )
            .children(later)
            .child(reject)
            .child(approve);

        div()
            .debug_selector(|| "plan-gate".to_owned())
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .max_h(px(620.0))
            .overflow_hidden()
            .rounded(Theme::RADIUS_LG)
            .border_1()
            .border_color(theme.accent.alpha(0.55))
            .bg(theme.overlay)
            .child(header)
            .child(body)
            .child(footer)
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
