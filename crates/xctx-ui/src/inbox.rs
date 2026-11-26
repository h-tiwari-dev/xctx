//! The Inbox panel: everything waiting for the user, across every space, with the actions each kind allows.
//!
//! The panel draws [`InboxEntry`]s the app builds from its Inbox (already redacted) and reports what the user
//! chose through [`InboxPanel::on_action`]. Clicking a row expands its detail through [`InboxPanel::on_toggle`].
//!
//! A danger entry ([`InboxEntry::danger`]: production, a write tool call, mounting a write-capable tool,
//! trusting a repo plugin) cannot be approved from its row: the row offers Review, which expands it, and Approve
//! is a second, deliberate click inside the expanded detail (`inbox-approve-<id>`; the row's button is
//! `inbox-open-<id>`).

use std::rc::Rc;

use gpui::{App, FontWeight, SharedString, Window, div, prelude::*, px};

use crate::components::{Badge, Button, ButtonSize, ButtonVariant};
use crate::theme::{Theme, Tone};

/// What an entry is about, which decides its badge and actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InboxKind {
    /// A proposed space waiting for approval.
    PlanGate,
    /// A request to add repos to a space or switch it to production.
    SpaceWiden,
    /// A request to fork competing attempts of a writer.
    AttemptProposal,
    /// A plugin action waiting for approval: a write tool call, a mount, or trusting a repo plugin.
    PluginGate,
    /// A required mise variable that is not set.
    MissingEnvVar,
    /// Something an agent or the app wants the user to see.
    Notice,
}

impl InboxKind {
    /// The badge text.
    pub fn label(self) -> &'static str {
        match self {
            Self::PlanGate => "PLAN",
            Self::SpaceWiden => "WIDEN",
            Self::AttemptProposal => "FORK",
            Self::PluginGate => "TOOL",
            Self::MissingEnvVar => "ENV",
            Self::Notice => "NOTE",
        }
    }

    /// The badge tone.
    pub fn tone(self) -> Tone {
        match self {
            Self::PlanGate => Tone::Accent,
            Self::SpaceWiden | Self::AttemptProposal | Self::PluginGate => Tone::Warning,
            Self::MissingEnvVar => Tone::Danger,
            Self::Notice => Tone::Neutral,
        }
    }

    /// The actions an entry of this kind offers, in the order they are drawn.
    pub fn actions(self) -> &'static [InboxAction] {
        match self {
            Self::PlanGate => &[InboxAction::Review],
            Self::SpaceWiden | Self::AttemptProposal | Self::PluginGate => {
                &[InboxAction::Reject, InboxAction::Approve]
            }
            Self::MissingEnvVar => &[InboxAction::Dismiss, InboxAction::Resolve],
            Self::Notice => &[InboxAction::Dismiss],
        }
    }
}

/// What the user chose for an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InboxAction {
    /// Open a Plan Gate to look at, edit, approve or reject it.
    Review,
    /// Approve a widening.
    Approve,
    /// Turn a widening down.
    Reject,
    /// Set a missing variable.
    Resolve,
    /// Put a notice or missing variable away.
    Dismiss,
}

impl InboxAction {
    /// The button label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Review => "Review",
            Self::Approve => "Approve",
            Self::Reject => "Deny",
            Self::Resolve => "Set value",
            Self::Dismiss => "Dismiss",
        }
    }

    /// A short lowercase name, used in test selectors: `inbox-<name>-<id>`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::Resolve => "resolve",
            Self::Dismiss => "dismiss",
        }
    }

    fn variant(self) -> ButtonVariant {
        match self {
            Self::Review | Self::Approve | Self::Resolve => ButtonVariant::Primary,
            Self::Reject => ButtonVariant::Danger,
            Self::Dismiss => ButtonVariant::Ghost,
        }
    }
}

/// One entry of the panel. The app passes redacted text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxEntry {
    /// The Inbox item's id, passed back with every action.
    pub id: SharedString,
    /// What it is about.
    pub kind: InboxKind,
    /// One line.
    pub title: SharedString,
    /// The rest, shown when the row is expanded; lines are kept.
    pub detail: Option<SharedString>,
    /// The space it is about.
    pub space: Option<SharedString>,
    /// Mark it as touching production (a widening to `MISE_ENV=production`).
    pub production: bool,
    /// Whether the app is acting on it now (a widening being approved): its action buttons are disabled.
    pub busy: bool,
    /// Whether approving it is high-risk (see the module docs): drawn in the danger tone, and approved only from
    /// its expanded detail.
    pub danger: bool,
}

impl InboxEntry {
    /// An entry with no detail, space or production mark.
    pub fn new(
        id: impl Into<SharedString>,
        kind: InboxKind,
        title: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            title: title.into(),
            detail: None,
            space: None,
            production: false,
            busy: false,
            danger: false,
        }
    }

    /// Set the detail shown when expanded.
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        let detail = detail.into();
        self.detail = (!detail.trim().is_empty()).then_some(detail);
        self
    }

    /// Set the space it is about.
    pub fn space(mut self, space: impl Into<SharedString>) -> Self {
        self.space = Some(space.into());
        self
    }

    /// Mark it as touching production.
    pub fn production(mut self, production: bool) -> Self {
        self.production = production;
        self
    }

    /// Mark it as being acted on, which disables its action buttons.
    pub fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    /// Mark approving it as high-risk: it is drawn in the danger tone and approved only from its detail.
    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }
}

type ActionHandler = Rc<dyn Fn(&SharedString, InboxAction, &mut Window, &mut App)>;
type ToggleHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// The Inbox: pending entries, newest first as given, each with its actions.
#[derive(IntoElement)]
pub struct InboxPanel {
    entries: Vec<InboxEntry>,
    expanded: Option<SharedString>,
    on_action: Option<ActionHandler>,
    on_toggle: Option<ToggleHandler>,
}

impl Default for InboxPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// What the panel says when nothing is waiting.
pub const INBOX_EMPTY: &str = "Nothing needs you right now. Agents post here when they do.";

impl InboxPanel {
    /// An empty panel.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            expanded: None,
            on_action: None,
            on_toggle: None,
        }
    }

    /// The entries to show, in order.
    pub fn entries(mut self, entries: impl IntoIterator<Item = InboxEntry>) -> Self {
        self.entries = entries.into_iter().collect();
        self
    }

    /// Show the detail of the entry with this id.
    pub fn expanded(mut self, id: Option<SharedString>) -> Self {
        self.expanded = id;
        self
    }

    /// Call `handler` with an entry's id and the action its button stands for.
    pub fn on_action(
        mut self,
        handler: impl Fn(&SharedString, InboxAction, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_action = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with an entry's id when its row is clicked, to expand or collapse it.
    pub fn on_toggle(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }

    fn action_button(&self, entry: &InboxEntry, action: InboxAction) -> Button {
        let mut button = Button::new(
            SharedString::from(format!("inbox-{}-{}", action.name(), entry.id)),
            action.label(),
        )
        .variant(action.variant())
        .size(ButtonSize::Regular)
        .disabled(entry.busy)
        .selector(format!("inbox-{}-{}", action.name(), entry.id));
        if let Some(handler) = self.on_action.clone() {
            let id = entry.id.clone();
            button = button.on_click(move |_, window, cx| handler(&id, action, window, cx));
        }
        button
    }

    fn render_entry(&self, entry: &InboxEntry, theme: &Theme) -> gpui::Stateful<gpui::Div> {
        let expanded = self.expanded.as_ref() == Some(&entry.id);
        // A danger entry's Approve moves into its expanded detail; the row gets Review, which expands it.
        let guarded = entry.danger && entry.kind.actions().contains(&InboxAction::Approve);
        let mut actions: Vec<Button> = entry
            .kind
            .actions()
            .iter()
            .filter(|&&action| !(guarded && action == InboxAction::Approve))
            .map(|&action| self.action_button(entry, action))
            .collect();
        if guarded && !expanded {
            let mut review = Button::new(
                SharedString::from(format!("inbox-open-{}", entry.id)),
                "Review",
            )
            .variant(ButtonVariant::Primary)
            .size(ButtonSize::Regular)
            .disabled(entry.busy)
            .selector(format!("inbox-open-{}", entry.id));
            if let Some(handler) = self.on_toggle.clone() {
                let id = entry.id.clone();
                review = review.on_click(move |_, window, cx| handler(&id, window, cx));
            }
            actions.push(review);
        }
        let space = entry.space.clone().map(|space| {
            div()
                .flex_none()
                .text_color(theme.text_subtle)
                .text_size(Theme::TEXT_XS)
                .child(space)
        });
        let production = entry
            .production
            .then(|| Badge::new("PRODUCTION").tone(Tone::Danger));
        let confirm = (guarded && expanded).then(|| {
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .mt(Theme::SPACE_1)
                .p(Theme::SPACE_2)
                .rounded(Theme::RADIUS_MD)
                .border_1()
                .border_color(theme.danger.alpha(0.4))
                .bg(theme.danger.alpha(0.08))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.danger)
                        .child("This reaches production or writes outside the space. Approve only if you meant it."),
                )
                .child(
                    self.action_button(entry, InboxAction::Approve)
                        .variant(ButtonVariant::Danger),
                )
        });
        let detail = expanded
            .then(|| {
                div()
                    .flex()
                    .flex_col()
                    .gap(Theme::SPACE_0_5)
                    .pl(px(52.0))
                    .pr(Theme::SPACE_2)
                    .pb(Theme::SPACE_2)
                    .text_size(Theme::TEXT_SM)
                    .text_color(theme.text_muted)
                    .children(entry.detail.iter().flat_map(|detail| {
                        detail
                            .lines()
                            .map(|line| div().min_h(px(14.0)).child(line.to_owned()))
                            .collect::<Vec<_>>()
                    }))
                    .children(confirm)
            })
            .filter(|_| entry.detail.is_some() || guarded);
        let has_detail = entry.detail.is_some() || guarded;
        let mut header = div()
            .id(SharedString::from(format!("inbox-row-{}", entry.id)))
            .debug_selector(|| format!("inbox-row-{}", entry.id))
            .flex()
            .items_center()
            .gap(Theme::SPACE_2)
            .min_h(px(36.0))
            .px(Theme::SPACE_2)
            .child(
                div()
                    .flex_none()
                    .w(px(44.0))
                    .child(Badge::new(entry.kind.label()).tone(if entry.danger {
                        Tone::Danger
                    } else {
                        entry.kind.tone()
                    })),
            )
            .children(production)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text)
                    .text_size(Theme::TEXT_MD)
                    .child(entry.title.clone()),
            )
            .children(space)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(Theme::SPACE_1)
                    .children(actions),
            );
        if has_detail {
            let hover = theme.raised;
            header = header.cursor_pointer().hover(move |style| style.bg(hover));
            if let Some(handler) = self.on_toggle.clone() {
                let id = entry.id.clone();
                header = header.on_click(move |_, window, cx| handler(&id, window, cx));
            }
        }
        div()
            .id(SharedString::from(format!("inbox-entry-{}", entry.id)))
            .flex()
            .flex_col()
            .rounded(Theme::RADIUS_SM)
            .when(entry.danger, |row| {
                row.border_l_2().border_color(theme.danger.alpha(0.7))
            })
            .when(expanded, |row| row.bg(theme.surface))
            .child(header)
            .children(detail)
    }
}

impl RenderOnce for InboxPanel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let count = self.entries.len();
        let tone = if count > 0 {
            Tone::Warning
        } else {
            Tone::Neutral
        };
        let rows: Vec<_> = self
            .entries
            .iter()
            .map(|entry| self.render_entry(entry, &theme))
            .collect();
        let body = if rows.is_empty() {
            div()
                .px(Theme::SPACE_2)
                .py(Theme::SPACE_2)
                .text_size(Theme::TEXT_SM)
                .text_color(theme.text_muted)
                .child(INBOX_EMPTY)
        } else {
            div().flex().flex_col().gap(Theme::SPACE_0_5).children(rows)
        };
        div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_1)
            .w_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(
                        div()
                            .text_size(Theme::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("Inbox"),
                    )
                    .child(Badge::new(count.to_string()).tone(tone)),
            )
            .child(
                div()
                    .p(Theme::SPACE_1)
                    .rounded(Theme::RADIUS_MD)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.raised)
                    .child(body),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gates_are_decided_and_the_rest_put_away() {
        assert_eq!(InboxKind::PlanGate.actions(), [InboxAction::Review]);
        assert!(
            InboxKind::SpaceWiden
                .actions()
                .contains(&InboxAction::Approve)
        );
        assert!(
            !InboxKind::SpaceWiden
                .actions()
                .contains(&InboxAction::Dismiss)
        );
        assert!(
            InboxKind::MissingEnvVar
                .actions()
                .contains(&InboxAction::Resolve)
        );
        assert_eq!(InboxKind::Notice.actions(), [InboxAction::Dismiss]);
    }

    #[test]
    fn blank_detail_is_no_detail() {
        let entry = InboxEntry::new("inbox-1", InboxKind::Notice, "hi").detail("  \n");
        assert_eq!(entry.detail, None);
        assert!(!entry.busy);
        let entry = entry
            .detail("more")
            .space("rate-limit")
            .production(true)
            .busy(true);
        assert_eq!(entry.detail.as_deref(), Some("more"));
        assert!(entry.production && entry.busy);
    }
}
