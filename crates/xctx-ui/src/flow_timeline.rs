//! The Flow view's timeline: one column of what happened in a space, newest first: channel messages, findings,
//! shared notes, the contract and the loop's stage changes ([`xctx_nav::flow::timeline`]), optionally narrowed to
//! one agent or one pair (the header then shows a focus chip that clears it). The space's contract is pinned on
//! top as a card.
//!
//! The column draws the entries the app passes in and reports what the user did through callbacks; it keeps no
//! state of its own. The entries are a virtualised [`gpui::list`] whose [`ListState`] the caller keeps per space
//! and brings up to date with [`sync_list_state`] before rendering. It draws no background or outline: the caller
//! sets its width (about 360px) and places it.
//!
//! Test selectors: `timeline-clear-focus` on the focus chip, `timeline-actor-<entry key>` and
//! `timeline-target-<entry key>` on the agent names of an entry (clickable only for agents of the graph),
//! `timeline-resolve-<finding id>` on an unresolved finding's Resolve button, `timeline-contract` on the pinned
//! contract card and `timeline-contract-agent-<agent id>` on its agreed-by chips.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, FontWeight, Hsla, ListState, SharedString, Window, div, prelude::*, px,
};
use xctx_channel::{Contract, FindingSeverity, MessageKind};
use xctx_graph::{AgentGraph, AgentNodeId};
use xctx_nav::flow::{TimelineItem, TimelineKind};

use crate::agents::agent_identity_color;
use crate::components::{Badge, Button, ButtonSize, ButtonVariant, Tooltip};
use crate::icon::{Icon, IconName, IconSize};
use crate::stage_track::loop_stage_label;
use crate::theme::{Theme, Tone};

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

/// The most entries the timeline draws; older ones are dropped.
pub const TIMELINE_LIMIT: usize = 1500;

/// What the timeline says when it has no entries.
pub const TIMELINE_EMPTY: &str = "Nothing yet — messages, findings and stage changes show up here.";

/// Bring `state` up to `item_count` entries (at most [`TIMELINE_LIMIT`]) before the timeline renders. It is reset
/// only when the count changed, which puts the scroll back at the top, where the newest entry is; otherwise the
/// scroll position is kept.
pub fn sync_list_state(state: &ListState, item_count: usize) {
    let count = item_count.min(TIMELINE_LIMIT);
    if state.item_count() != count {
        state.reset(count);
    }
}

/// How long before `now` the moment `at` was, compactly: `now` under a minute, then `5m`, `3h`, `2d`. Empty when
/// either time is unknown (0). Both are seconds since the Unix epoch; a time after `now` reads as `now`.
pub fn relative_time(at: u64, now: u64) -> String {
    if at == 0 || now == 0 {
        return String::new();
    }
    let ago = now.saturating_sub(at);
    match ago {
        0..60 => "now".to_owned(),
        60..3_600 => format!("{}m", ago / 60),
        3_600..86_400 => format!("{}h", ago / 3_600),
        _ => format!("{}d", ago / 86_400),
    }
}

/// The badge of an entry: the message kind, the finding's severity, `NOTE` or `CONTRACT vN`. `None` for stage
/// changes, which are drawn as dividers.
pub fn timeline_badge(kind: &TimelineKind) -> Option<(SharedString, Tone)> {
    match kind {
        TimelineKind::Message(kind) => {
            Some((message_kind_label(*kind).into(), message_kind_tone(*kind)))
        }
        TimelineKind::Finding { severity, .. } => {
            Some((severity_label(*severity).into(), severity_tone(*severity)))
        }
        TimelineKind::Note => Some(("NOTE".into(), Tone::Neutral)),
        TimelineKind::Contract { version } => {
            Some((format!("CONTRACT v{version}").into(), Tone::Success))
        }
        TimelineKind::Stage { .. } => None,
    }
}

/// An agent of the graph as the timeline names it.
#[derive(Clone)]
struct AgentRef {
    id: AgentNodeId,
    label: SharedString,
    color: Hsla,
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type AgentHandler = Rc<dyn Fn(&AgentNodeId, &mut Window, &mut App)>;
type FindingHandler = Rc<dyn Fn(u64, &mut Window, &mut App)>;

/// What every entry needs, shared by the list's rows.
struct EntryContext {
    /// The graph's agents by id and by label.
    agents: HashMap<String, AgentRef>,
    now: u64,
    on_select_agent: Option<AgentHandler>,
    on_resolve_finding: Option<FindingHandler>,
}

/// The Flow view's timeline column. See the module docs for its test selectors.
#[derive(IntoElement)]
pub struct FlowTimeline {
    items: Vec<TimelineItem>,
    state: ListState,
    graph: Option<AgentGraph>,
    contract: Option<Contract>,
    focus_label: Option<SharedString>,
    now: u64,
    on_clear_focus: Option<Handler>,
    on_select_agent: Option<AgentHandler>,
    on_resolve_finding: Option<FindingHandler>,
}

impl FlowTimeline {
    /// A timeline of `items` (newest first, as [`xctx_nav::flow::timeline`] returns them; at most
    /// [`TIMELINE_LIMIT`] are drawn) scrolled by `state`, which the caller keeps per space and passes to
    /// [`sync_list_state`] with `items.len()` first.
    pub fn new(mut items: Vec<TimelineItem>, state: ListState) -> Self {
        items.truncate(TIMELINE_LIMIT);
        Self {
            items,
            state,
            graph: None,
            contract: None,
            focus_label: None,
            now: 0,
            on_clear_focus: None,
            on_select_agent: None,
            on_resolve_finding: None,
        }
    }

    /// The space's agents, to name the agents of each entry by label with their identity colour. Without it,
    /// entries show the raw ids and their names are not clickable.
    pub fn graph(mut self, graph: AgentGraph) -> Self {
        self.graph = Some(graph);
        self
    }

    /// Pin the contract's current revision on top: version, title, summary and who agreed to it. Pass `None`
    /// while the timeline is narrowed to a pair: what two agents said to each other is not about the contract.
    pub fn contract(mut self, contract: Option<Contract>) -> Self {
        self.contract = contract;
        self
    }

    /// What the timeline is narrowed to, such as `api ↔ web` or `tests`, shown as a chip that clears it
    /// ([`FlowTimeline::on_clear_focus`]). `None` when it shows everything.
    pub fn focus_label(mut self, label: Option<SharedString>) -> Self {
        self.focus_label = label;
        self
    }

    /// The current time in seconds since the Unix epoch, for the entries' relative times ([`relative_time`]).
    /// Without it no times are shown.
    pub fn now(mut self, unix_secs: u64) -> Self {
        self.now = unix_secs;
        self
    }

    /// Call `handler` when the focus chip is clicked.
    pub fn on_clear_focus(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_clear_focus = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with an agent's id when its name is clicked, in an entry or among the contract's agreements.
    pub fn on_select_agent(
        mut self,
        handler: impl Fn(&AgentNodeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select_agent = Some(Rc::new(handler));
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
}

/// The graph's agents by id and by label, with their identity colours.
fn agent_refs(graph: Option<&AgentGraph>, theme: &Theme) -> HashMap<String, AgentRef> {
    let mut agents = HashMap::new();
    let Some(graph) = graph else {
        return agents;
    };
    for node in graph.nodes() {
        let agent = AgentRef {
            id: node.id.clone(),
            label: node.label.clone().into(),
            color: agent_identity_color(graph, node.id.as_str(), theme)
                .unwrap_or(theme.text_subtle),
        };
        agents
            .entry(node.label.clone())
            .or_insert_with(|| agent.clone());
        agents.insert(node.id.as_str().to_owned(), agent);
    }
    agents
}

/// A 6px round dot of `color`.
fn dot(color: Hsla) -> gpui::Div {
    div()
        .flex_none()
        .size(px(6.0))
        .rounded(Theme::RADIUS_FULL)
        .bg(color)
}

/// An agent's name: identity dot and label, clickable (`selector`) when the agent is in the graph.
fn agent_name(raw: &str, selector: String, ctx: &EntryContext, theme: &Theme) -> AnyElement {
    let agent = ctx.agents.get(raw);
    let (label, color): (SharedString, Hsla) = match agent {
        Some(agent) => (agent.label.clone(), agent.color),
        None => (raw.to_owned().into(), theme.text_subtle),
    };
    let mut name = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(|| selector)
        .flex()
        .flex_shrink_1()
        .items_center()
        .gap(Theme::SPACE_1)
        .min_w(px(24.0))
        .px(Theme::SPACE_0_5)
        .rounded(Theme::RADIUS_SM)
        .text_size(Theme::TEXT_XS)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text)
        .child(dot(color))
        .child(div().min_w_0().truncate().child(label));
    if let (Some(agent), Some(handler)) = (agent, ctx.on_select_agent.clone()) {
        let id = agent.id.clone();
        let hover = theme.hover;
        name = name
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                handler(&id, window, cx);
            });
    }
    name.into_any_element()
}

/// The relative time of an entry, quiet.
fn time_label(at: u64, now: u64, theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .text_size(Theme::TEXT_XS)
        .text_color(theme.text_subtle)
        .child(relative_time(at, now))
}

/// A stage change: a thin divider reading `Iteration 2 · Test ✗ → Write`.
fn stage_divider(
    item: &TimelineItem,
    (from, to, passed, iteration): (xctx_graph::LoopStage, xctx_graph::LoopStage, bool, u32),
    ctx: &EntryContext,
    theme: &Theme,
) -> AnyElement {
    let color = if passed { theme.success } else { theme.danger };
    let rule = || {
        div()
            .flex_1()
            .min_w(px(8.0))
            .h(px(1.0))
            .bg(theme.border_subtle)
    };
    div()
        .id(SharedString::from(item.key.clone()))
        .flex()
        .items_center()
        .gap(Theme::SPACE_1 + px(2.0))
        .px(Theme::SPACE_3)
        .py(Theme::SPACE_2)
        .text_size(Theme::TEXT_XS)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .child(rule())
        .child(div().flex_none().child(format!(
            "Iteration {iteration} · {}",
            loop_stage_label(from)
        )))
        .child(
            Icon::new(if passed {
                IconName::Check
            } else {
                IconName::Error
            })
            .size(IconSize::Xs)
            .color(color),
        )
        .child(
            Icon::new(IconName::ChevronRight)
                .size(IconSize::Xs)
                .color(theme.text_subtle),
        )
        .child(div().flex_none().child(loop_stage_label(to)))
        .child(rule())
        .child(time_label(item.at, ctx.now, theme))
        .into_any_element()
}

/// One entry of the list.
fn render_entry(item: &TimelineItem, ctx: &EntryContext, theme: &Theme) -> AnyElement {
    if let TimelineKind::Stage {
        from,
        to,
        passed,
        iteration,
    } = item.kind
    {
        return stage_divider(item, (from, to, passed, iteration), ctx, theme);
    }

    let mut who = div()
        .flex()
        .flex_1()
        .items_center()
        .gap(Theme::SPACE_1)
        .min_w_0()
        .overflow_hidden();
    if let Some(actor) = &item.actor {
        who = who.child(agent_name(
            actor,
            format!("timeline-actor-{}", item.key),
            ctx,
            theme,
        ));
    }
    let everyone = matches!(item.kind, TimelineKind::Message(_)) && item.target.is_none();
    if item.target.is_some() || everyone {
        who = who.child(
            Icon::new(IconName::ChevronRight)
                .size(IconSize::Xs)
                .color(theme.text_subtle),
        );
    }
    if let Some(target) = &item.target {
        who = who.child(agent_name(
            target,
            format!("timeline-target-{}", item.key),
            ctx,
            theme,
        ));
    } else if everyone {
        who = who.child(
            div()
                .flex_none()
                .text_size(Theme::TEXT_XS)
                .text_color(theme.text_muted)
                .child("everyone"),
        );
    }

    let resolution: Option<AnyElement> = match item.kind {
        TimelineKind::Finding {
            id,
            resolved: false,
            ..
        } => {
            let selector = format!("timeline-resolve-{id}");
            let mut button = Button::new(SharedString::from(selector.clone()), "Resolve")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .icon(IconName::Check)
                .selector(selector);
            if let Some(handler) = ctx.on_resolve_finding.clone() {
                button = button.on_click(move |_, window, cx| handler(id, window, cx));
            }
            Some(button.into_any_element())
        }
        TimelineKind::Finding { resolved: true, .. } => Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(Theme::SPACE_0_5)
                .text_size(Theme::TEXT_XS)
                .text_color(theme.text_subtle)
                .child(
                    Icon::new(IconName::Check)
                        .size(IconSize::Xs)
                        .color(theme.success),
                )
                .child("resolved")
                .into_any_element(),
        ),
        _ => None,
    };

    let header = div()
        .flex()
        .items_center()
        .gap(Theme::SPACE_1 + px(2.0))
        .min_w_0()
        .h(px(18.0))
        .child(who)
        .children(timeline_badge(&item.kind).map(|(label, tone)| Badge::new(label).tone(tone)))
        .children(resolution)
        .child(time_label(item.at, ctx.now, theme));

    let resolved = matches!(item.kind, TimelineKind::Finding { resolved: true, .. });
    let body = (!item.body.is_empty()).then(|| {
        div()
            .w_full()
            .min_w_0()
            .line_clamp(4)
            .text_size(Theme::TEXT_SM)
            .text_color(if resolved {
                theme.text_muted
            } else {
                theme.text
            })
            .child(item.body.clone())
    });

    let hover = theme.hover;
    div()
        .id(SharedString::from(item.key.clone()))
        .flex()
        .flex_col()
        .gap(Theme::SPACE_1)
        .w_full()
        .min_w_0()
        .px(Theme::SPACE_3)
        .py(Theme::SPACE_2)
        .border_b_1()
        .border_color(theme.border_subtle)
        .hover(move |style| style.bg(hover))
        .child(header)
        .children(body)
        .into_any_element()
}

/// The contract card pinned above the entries.
fn contract_card(contract: &Contract, ctx: &EntryContext, theme: &Theme) -> AnyElement {
    let agreed = contract.agreed_by.iter().map(|raw| {
        let agent = ctx.agents.get(raw.as_str());
        let (label, color): (SharedString, Hsla) = match agent {
            Some(agent) => (agent.label.clone(), agent.color),
            None => (raw.clone().into(), theme.text_subtle),
        };
        let id = agent.map_or_else(|| raw.clone(), |agent| agent.id.as_str().to_owned());
        let selector = format!("timeline-contract-agent-{id}");
        let mut chip = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(|| selector)
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::SPACE_1)
            .h(px(18.0))
            .px(Theme::SPACE_1 + px(2.0))
            .rounded(Theme::RADIUS_FULL)
            .border_1()
            .border_color(theme.border_subtle)
            .bg(theme.raised)
            .text_size(Theme::TEXT_XS)
            .text_color(theme.text)
            .child(dot(color))
            .child(div().max_w(px(120.0)).truncate().child(label));
        if let (Some(agent), Some(handler)) = (agent, ctx.on_select_agent.clone()) {
            let id = agent.id.clone();
            let hover = theme.hover;
            chip = chip
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    handler(&id, window, cx);
                });
        }
        chip
    });

    div()
        .id("timeline-contract")
        .debug_selector(|| "timeline-contract".to_owned())
        .flex()
        .flex_col()
        .flex_none()
        .gap(Theme::SPACE_1 + px(2.0))
        .mx(Theme::SPACE_3)
        .mb(Theme::SPACE_2)
        .p(Theme::SPACE_3 - px(2.0))
        .rounded(Theme::RADIUS_LG)
        .border_1()
        .border_color(theme.border_subtle)
        .bg(theme.surface)
        .child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_1 + px(2.0))
                .child(
                    Icon::new(IconName::FileText)
                        .size(IconSize::Xs)
                        .color(theme.accent),
                )
                .child(
                    div()
                        .flex_1()
                        .text_size(Theme::TEXT_XS)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text_subtle)
                        .child("Contract"),
                )
                .child(Badge::new(format!("v{}", contract.version)).tone(Tone::Accent))
                .child(time_label(contract.updated_at, ctx.now, theme)),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(Theme::TEXT_SM)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(contract.title.clone()),
        )
        .when(!contract.summary.is_empty(), |card| {
            card.child(
                div()
                    .min_w_0()
                    .line_clamp(3)
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_muted)
                    .child(contract.summary.clone()),
            )
        })
        .when(!contract.agreed_by.is_empty(), |card| {
            card.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .child(
                        div()
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.meta)
                            .child("Agreed"),
                    )
                    .children(agreed),
            )
        })
        .into_any_element()
}

impl RenderOnce for FlowTimeline {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let ctx = Rc::new(EntryContext {
            agents: agent_refs(self.graph.as_ref(), &theme),
            now: self.now,
            on_select_agent: self.on_select_agent,
            on_resolve_finding: self.on_resolve_finding,
        });

        let focus = self.focus_label.map(|label| {
            let colors = theme.tone(Tone::Accent);
            let mut chip = div()
                .id("timeline-clear-focus")
                .debug_selector(|| "timeline-clear-focus".to_owned())
                .flex()
                .flex_shrink_1()
                .items_center()
                .gap(Theme::SPACE_1)
                .min_w_0()
                .max_w(px(200.0))
                .h(px(20.0))
                .pl(Theme::SPACE_2)
                .pr(Theme::SPACE_1)
                .rounded(Theme::RADIUS_FULL)
                .border_1()
                .border_color(colors.border)
                .bg(theme.selection)
                .text_size(Theme::TEXT_XS)
                .text_color(theme.text)
                .cursor_pointer()
                .tooltip(|_, cx| Tooltip::view("Show everything", cx))
                .child(div().min_w_0().truncate().child(label))
                .child(
                    Icon::new(IconName::Close)
                        .size(IconSize::Xs)
                        .color(theme.text_muted),
                );
            if let Some(handler) = self.on_clear_focus {
                chip = chip.on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    handler(window, cx);
                });
            }
            chip
        });

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::SPACE_2)
            .h(px(32.0))
            .px(Theme::SPACE_3)
            .child(
                div()
                    .flex_none()
                    .text_size(Theme::TEXT_XS)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text_subtle)
                    .child("Activity"),
            )
            .when(!self.items.is_empty(), |header| {
                header.child(
                    div()
                        .flex_none()
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.meta)
                        .child(self.items.len().to_string()),
                )
            })
            .child(div().flex_1())
            .children(focus);

        let contract = self
            .contract
            .as_ref()
            .map(|contract| contract_card(contract, &ctx, &theme));

        let entries = if self.items.is_empty() {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(Theme::SPACE_2)
                .px(Theme::SPACE_6)
                .child(
                    Icon::new(IconName::Inbox)
                        .size(IconSize::Lg)
                        .color(theme.text_subtle),
                )
                .child(
                    div()
                        .text_center()
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_subtle)
                        .child(TIMELINE_EMPTY),
                )
                .into_any_element()
        } else {
            let items = Rc::new(self.items);
            let list = gpui::list(self.state, move |index, _window, cx| {
                let theme = Theme::of(cx);
                match items.get(index) {
                    Some(item) => render_entry(item, &ctx, &theme),
                    None => div().into_any_element(),
                }
            })
            .size_full();
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .border_t_1()
                .border_color(theme.border_subtle)
                .child(list)
                .into_any_element()
        };

        div()
            .id("flow-timeline")
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .child(header)
            .children(contract)
            .child(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::ListAlignment;
    use xctx_channel::{FindingSeverity, MessageKind};

    #[test]
    fn relative_times_are_compact() {
        let now = 1_000_000;
        assert_eq!(relative_time(0, now), "");
        assert_eq!(relative_time(now, 0), "");
        assert_eq!(relative_time(now - 30, now), "now");
        assert_eq!(relative_time(now + 30, now), "now");
        assert_eq!(relative_time(now - 5 * 60, now), "5m");
        assert_eq!(relative_time(now - 3 * 3_600 - 10, now), "3h");
        assert_eq!(relative_time(now - 2 * 86_400, now), "2d");
    }

    #[test]
    fn the_list_state_follows_the_count_up_to_the_limit() {
        let state = ListState::new(0, ListAlignment::Top, px(100.0));
        sync_list_state(&state, 3);
        assert_eq!(state.item_count(), 3);
        sync_list_state(&state, 3);
        assert_eq!(state.item_count(), 3);
        sync_list_state(&state, TIMELINE_LIMIT + 10);
        assert_eq!(state.item_count(), TIMELINE_LIMIT);
    }

    #[test]
    fn badges_name_the_kind() {
        assert_eq!(
            timeline_badge(&TimelineKind::Message(MessageKind::Question)),
            Some(("QUESTION".into(), Tone::Warning))
        );
        assert_eq!(
            timeline_badge(&TimelineKind::Finding {
                id: 1,
                severity: FindingSeverity::Error,
                resolved: false
            }),
            Some(("ERROR".into(), Tone::Danger))
        );
        assert_eq!(
            timeline_badge(&TimelineKind::Contract { version: 3 }),
            Some(("CONTRACT v3".into(), Tone::Success))
        );
        assert_eq!(
            timeline_badge(&TimelineKind::Stage {
                from: xctx_graph::LoopStage::Test,
                to: xctx_graph::LoopStage::Write,
                passed: false,
                iteration: 2
            }),
            None
        );
    }
}
