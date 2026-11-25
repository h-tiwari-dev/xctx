//! The Shared Space panel: the space's Contract, its Channel between agents, and the Findings and Notes the
//! helpers and agents left, one tab at a time.
//!
//! The app passes text already masked by `xctx_core::Redactor` (as `SharedSpaceStore::contract`,
//! `list_messages`, `findings` and `notes` return it) and reports what the user did through
//! [`SharedSpacePanel::on_tab`] and [`SharedSpacePanel::on_resolve_finding`]. The panel keeps no state.

use std::rc::Rc;

use gpui::{App, FontWeight, SharedString, Window, div, prelude::*, px};
use xctx_channel::{ChannelMessage, Contract, Finding, FindingSeverity, MessageKind, Note};

use crate::components::{Badge, Button, ButtonSize, ButtonVariant, Pill};
use crate::theme::{Theme, Tone};

/// A tab of the Shared Space panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SharedTab {
    /// The versioned contract the writers agree on.
    #[default]
    Contract,
    /// The messages between the space's agents.
    Channel,
    /// What helpers found and what the space should remember.
    FindingsNotes,
}

/// Every tab, in the order the panel shows them.
pub const SHARED_TABS: [SharedTab; 3] = [
    SharedTab::Contract,
    SharedTab::Channel,
    SharedTab::FindingsNotes,
];

/// What the Contract tab says while no contract has been proposed.
pub const CONTRACT_EMPTY: &str =
    "No contract yet. Writers propose one on the Channel, and every revision bumps its version.";

/// What the Channel tab says while no message has been posted.
pub const CHANNEL_EMPTY: &str =
    "No messages yet. Agents post proposals, questions and status here.";

impl SharedTab {
    /// The tab's label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Contract => "Contract",
            Self::Channel => "Channel",
            Self::FindingsNotes => "Findings & Notes",
        }
    }

    /// A short lowercase name, used in test selectors: `shared-tab-<name>`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Contract => "contract",
            Self::Channel => "channel",
            Self::FindingsNotes => "findings",
        }
    }
}

/// The badge text of a message kind.
pub fn message_kind_label(kind: MessageKind) -> &'static str {
    match kind {
        MessageKind::Proposal => "PROPOSAL",
        MessageKind::Question => "QUESTION",
        MessageKind::Finding => "FINDING",
        MessageKind::Status => "STATUS",
        MessageKind::ContractUpdate => "CONTRACT",
    }
}

/// The badge tone of a message kind.
pub fn message_kind_tone(kind: MessageKind) -> Tone {
    match kind {
        MessageKind::Proposal => Tone::Accent,
        MessageKind::Question => Tone::Warning,
        MessageKind::Finding => Tone::Danger,
        MessageKind::Status => Tone::Neutral,
        MessageKind::ContractUpdate => Tone::Success,
    }
}

/// The badge text of a finding's severity.
pub fn severity_label(severity: FindingSeverity) -> &'static str {
    match severity {
        FindingSeverity::Info => "INFO",
        FindingSeverity::Warning => "WARN",
        FindingSeverity::Error => "ERROR",
    }
}

/// The badge tone of a finding's severity.
pub fn severity_tone(severity: FindingSeverity) -> Tone {
    match severity {
        FindingSeverity::Info => Tone::Neutral,
        FindingSeverity::Warning => Tone::Warning,
        FindingSeverity::Error => Tone::Danger,
    }
}

/// Where a finding points, as `repo:file`, `repo`, `file`, or `None` when it names neither.
pub fn finding_location(finding: &Finding) -> Option<String> {
    match (finding.repo.as_deref(), finding.file.as_deref()) {
        (Some(repo), Some(file)) => Some(format!("{repo}:{file}")),
        (Some(repo), None) => Some(repo.to_owned()),
        (None, Some(file)) => Some(file.to_owned()),
        (None, None) => None,
    }
}

type TabHandler = Rc<dyn Fn(SharedTab, &mut Window, &mut App)>;
type ResolveHandler = Rc<dyn Fn(u64, &mut Window, &mut App)>;

/// The Shared Space panel with its three tabs.
///
/// Test selectors: `shared-tab-<name>` ([`SharedTab::name`]) on the tab pills, `shared-message-<id>` on channel
/// messages, `shared-finding-<id>` on findings and `shared-resolve-<id>` on an unresolved finding's Resolve button.
#[derive(IntoElement)]
pub struct SharedSpacePanel {
    tab: SharedTab,
    contract: Option<Contract>,
    messages: Vec<ChannelMessage>,
    findings: Vec<Finding>,
    notes: Vec<Note>,
    graph: Option<xctx_graph::AgentGraph>,
    on_tab: Option<TabHandler>,
    on_resolve_finding: Option<ResolveHandler>,
}

impl Default for SharedSpacePanel {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedSpacePanel {
    /// An empty panel on the Contract tab.
    pub fn new() -> Self {
        Self {
            tab: SharedTab::default(),
            contract: None,
            messages: Vec::new(),
            findings: Vec::new(),
            notes: Vec::new(),
            graph: None,
            on_tab: None,
            on_resolve_finding: None,
        }
    }

    /// The space's agent graph, used to colour channel messages by agent identity.
    pub fn graph(mut self, graph: xctx_graph::AgentGraph) -> Self {
        self.graph = Some(graph);
        self
    }
    /// The tab to show.
    pub fn tab(mut self, tab: SharedTab) -> Self {
        self.tab = tab;
        self
    }

    /// The current contract, already masked, or `None` while none has been proposed.
    pub fn contract(mut self, contract: Option<Contract>) -> Self {
        self.contract = contract;
        self
    }

    /// The channel messages, already masked, oldest first as the store lists them. The panel shows the newest
    /// first.
    pub fn messages(mut self, messages: impl IntoIterator<Item = ChannelMessage>) -> Self {
        self.messages = messages.into_iter().collect();
        self
    }

    /// The findings, already masked, in the order to show them.
    pub fn findings(mut self, findings: impl IntoIterator<Item = Finding>) -> Self {
        self.findings = findings.into_iter().collect();
        self
    }

    /// The shared notes, already masked, in the order to show them.
    pub fn notes(mut self, notes: impl IntoIterator<Item = Note>) -> Self {
        self.notes = notes.into_iter().collect();
        self
    }

    /// Call `handler` with the tab whose pill was clicked (also when it is the current one).
    pub fn on_tab(mut self, handler: impl Fn(SharedTab, &mut Window, &mut App) + 'static) -> Self {
        self.on_tab = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with a finding's id when its Resolve button is clicked.
    pub fn on_resolve_finding(
        mut self,
        handler: impl Fn(u64, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_resolve_finding = Some(Rc::new(handler));
        self
    }

    fn render_contract(&self, theme: &Theme) -> gpui::Div {
        let Some(contract) = &self.contract else {
            return empty_state(CONTRACT_EMPTY, theme);
        };
        let agreed: Vec<_> = contract
            .agreed_by
            .iter()
            .map(|agent| Badge::new(agent.clone()).tone(Tone::Success))
            .collect();
        let summary = (!contract.summary.trim().is_empty()).then(|| {
            div()
                .text_size(Theme::TEXT_SM)
                .text_color(theme.text_muted)
                .child(contract.summary.clone())
        });
        div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_2)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(Badge::new(format!("v{}", contract.version)).tone(Tone::Accent))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(Theme::TEXT_MD)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(contract.title.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(180.0))
                            .truncate()
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_subtle)
                            .child(format!("by {}", contract.author)),
                    ),
            )
            .children(summary)
            .child(
                div()
                    .id("shared-contract-spec")
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .overflow_x_scroll()
                    .p(Theme::SPACE_2)
                    .rounded(Theme::RADIUS_SM)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.chrome)
                    .font_family("Menlo")
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text)
                    .children(
                        contract
                            .spec
                            .lines()
                            .map(|line| div().min_h(px(14.0)).child(line.to_owned()))
                            .collect::<Vec<_>>(),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .child(
                        div()
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_subtle)
                            .child("Agreed by"),
                    )
                    .children(agreed),
            )
    }

    fn render_channel(&self, theme: &Theme) -> gpui::Div {
        if self.messages.is_empty() {
            return empty_state(CHANNEL_EMPTY, theme);
        }
        let rows: Vec<_> = self
            .messages
            .iter()
            .rev()
            .map(|message| {
                let identity_color = self.graph.as_ref().and_then(|graph| {
                    crate::agent_graph::agent_identity_color(graph, &message.from, theme)
                });
                let to = message.to.clone().map(|to| {
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_subtle)
                        .child(format!("-> {to}"))
                });
                div()
                    .id(SharedString::from(format!("shared-message-{}", message.id)))
                    .debug_selector(|| format!("shared-message-{}", message.id))
                    .flex()
                    .gap(Theme::SPACE_2)
                    .py(Theme::SPACE_1)
                    .children(
                        identity_color
                            .map(|color| div().flex_none().w(px(2.0)).rounded(px(1.0)).bg(color)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(Theme::SPACE_0_5)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(Theme::SPACE_2)
                                    .min_w_0()
                                    .child(
                                        Badge::new(message_kind_label(message.kind))
                                            .tone(message_kind_tone(message.kind)),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(Theme::TEXT_SM)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(identity_color.unwrap_or(theme.text))
                                            .child(message.from.clone()),
                                    )
                                    .children(to),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .text_size(Theme::TEXT_SM)
                                    .text_color(theme.text_muted)
                                    .children(
                                        message
                                            .body
                                            .lines()
                                            .map(|line| {
                                                div().min_h(px(14.0)).child(line.to_owned())
                                            })
                                            .collect::<Vec<_>>(),
                                    ),
                            ),
                    )
            })
            .collect();
        div().flex().flex_col().gap(Theme::SPACE_1).children(rows)
    }

    fn render_findings_notes(&self, theme: &Theme) -> gpui::Div {
        let findings: Vec<_> = self
            .findings
            .iter()
            .map(|finding| {
                let location = finding_location(finding).map(|location| {
                    div()
                        .flex_none()
                        .max_w(px(220.0))
                        .truncate()
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_subtle)
                        .child(location)
                });
                let action = if finding.resolved {
                    Badge::new("RESOLVED")
                        .tone(Tone::Success)
                        .into_any_element()
                } else {
                    let selector = format!("shared-resolve-{}", finding.id);
                    let mut button = Button::new(SharedString::from(selector.clone()), "Resolve")
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Compact)
                        .selector(selector);
                    if let Some(handler) = self.on_resolve_finding.clone() {
                        let id = finding.id;
                        button = button.on_click(move |_, window, cx| handler(id, window, cx));
                    }
                    button.into_any_element()
                };
                div()
                    .id(SharedString::from(format!("shared-finding-{}", finding.id)))
                    .debug_selector(|| format!("shared-finding-{}", finding.id))
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .py(Theme::SPACE_1)
                    .when(finding.resolved, |row| row.opacity(0.6))
                    .child(
                        div().flex_none().w(px(44.0)).child(
                            Badge::new(severity_label(finding.severity))
                                .tone(severity_tone(finding.severity)),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(Theme::TEXT_SM)
                            .text_color(theme.text)
                            .child(finding.summary.clone()),
                    )
                    .children(location)
                    .child(action)
            })
            .collect();
        let notes: Vec<_> = self
            .notes
            .iter()
            .map(|note| {
                div()
                    .flex()
                    .gap(Theme::SPACE_2)
                    .py(Theme::SPACE_1)
                    .child(
                        div()
                            .flex_none()
                            .min_w_0()
                            .max_w(px(140.0))
                            .truncate()
                            .text_size(Theme::TEXT_XS)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_muted)
                            .child(note.author.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(Theme::TEXT_SM)
                            .text_color(theme.text)
                            .child(note.body.clone()),
                    )
            })
            .collect();
        let findings_body = if findings.is_empty() {
            empty_state("No findings. Helpers report what they find here.", theme)
        } else {
            div().flex().flex_col().children(findings)
        };
        let notes_body = if notes.is_empty() {
            empty_state("No notes yet.", theme)
        } else {
            div().flex().flex_col().children(notes)
        };
        div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_2)
            .child(section_heading("FINDINGS", theme))
            .child(findings_body)
            .child(
                div()
                    .pt(Theme::SPACE_2)
                    .border_t_1()
                    .border_color(theme.border_subtle)
                    .child(section_heading("NOTES", theme)),
            )
            .child(notes_body)
    }
}

fn empty_state(text: &'static str, theme: &Theme) -> gpui::Div {
    div()
        .py(Theme::SPACE_2)
        .text_size(Theme::TEXT_SM)
        .text_color(theme.text_muted)
        .child(text)
}

fn section_heading(text: &'static str, theme: &Theme) -> gpui::Div {
    div()
        .text_size(Theme::TEXT_XS)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text_subtle)
        .child(text)
}

impl RenderOnce for SharedSpacePanel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let current = self.tab;
        let pills = SHARED_TABS.into_iter().map(|tab| {
            let selector = format!("shared-tab-{}", tab.name());
            let mut pill = Pill::new(SharedString::from(selector.clone()), tab.label())
                .selected(tab == current)
                .selector(selector);
            if let Some(handler) = self.on_tab.clone() {
                pill = pill.on_click(move |_, window, cx| handler(tab, window, cx));
            }
            pill
        });
        let unresolved = self.findings.iter().filter(|f| !f.resolved).count();
        let unresolved_badge =
            (unresolved > 0).then(|| Badge::new(format!("{unresolved} open")).tone(Tone::Warning));
        let body = match current {
            SharedTab::Contract => self.render_contract(&theme),
            SharedTab::Channel => self.render_channel(&theme),
            SharedTab::FindingsNotes => self.render_findings_notes(&theme),
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
                            .child("Shared Space"),
                    )
                    .children(unresolved_badge),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(Theme::SPACE_2)
                    .p(Theme::SPACE_2)
                    .rounded(Theme::RADIUS_MD)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.raised)
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(Theme::SPACE_0_5)
                            .p(Theme::SPACE_0_5)
                            .rounded(Theme::RADIUS_FULL)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.surface)
                            .children(pills),
                    )
                    .child(body),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(repo: Option<&str>, file: Option<&str>) -> Finding {
        Finding {
            id: 1,
            author: "helper-tests-1".into(),
            severity: FindingSeverity::Warning,
            repo: repo.map(str::to_owned),
            file: file.map(str::to_owned),
            summary: "retry is not idempotent".into(),
            resolved: false,
            created_at: 0,
        }
    }

    #[test]
    fn locations_join_repo_and_file() {
        assert_eq!(
            finding_location(&finding(Some("api"), Some("src/retry.rs"))).as_deref(),
            Some("api:src/retry.rs")
        );
        assert_eq!(
            finding_location(&finding(Some("api"), None)).as_deref(),
            Some("api")
        );
        assert_eq!(
            finding_location(&finding(None, Some("a.rs"))).as_deref(),
            Some("a.rs")
        );
        assert_eq!(finding_location(&finding(None, None)), None);
    }

    #[test]
    fn tabs_are_listed_once_with_labels() {
        let labels: Vec<_> = SHARED_TABS.into_iter().map(SharedTab::label).collect();
        assert_eq!(labels, ["Contract", "Channel", "Findings & Notes"]);
        assert_eq!(SharedTab::default(), SharedTab::Contract);
    }

    #[test]
    fn contract_updates_are_labelled_contract() {
        assert_eq!(message_kind_label(MessageKind::ContractUpdate), "CONTRACT");
        assert_eq!(severity_label(FindingSeverity::Warning), "WARN");
        assert_eq!(severity_tone(FindingSeverity::Error), Tone::Danger);
    }
}
