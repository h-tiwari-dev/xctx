//! The Inspector: a panel beside a space's panes with four tabs (Graph, Shared, Attempts, Plugins). It draws the
//! data it is given and reports each click as an [`InspectorAction`]; the host does the work.

use std::rc::Rc;

use gpui::{
    AnyElement, App, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div,
    prelude::*, px,
};

use crate::attempt_strip::AttemptChip;
use crate::tool_dock::DockItem;
use crate::{Badge, Button, ButtonSize, ButtonVariant, IconName, PaneLive, Theme};

/// The panel's width in pixels.
pub const INSPECTOR_WIDTH: f32 = 300.0;

/// One of the Inspector's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorTab {
    /// The space's agents: what each is doing, focus its pane, pause or resume it.
    Graph,
    /// The shared contract, and how much is open in the channel.
    Shared,
    /// The space's competing attempts: select one, fork, or review the latest round.
    Attempts,
    /// The space's tools: open or close each one.
    Plugins,
}

impl InspectorTab {
    /// Every tab, in the order the strip shows them.
    pub const ALL: [InspectorTab; 4] = [
        InspectorTab::Graph,
        InspectorTab::Shared,
        InspectorTab::Attempts,
        InspectorTab::Plugins,
    ];

    /// The tab's name on its button.
    pub fn label(self) -> &'static str {
        match self {
            Self::Graph => "Graph",
            Self::Shared => "Shared",
            Self::Attempts => "Attempts",
            Self::Plugins => "Plugins",
        }
    }

    fn selector(self) -> &'static str {
        match self {
            Self::Graph => "inspector-tab-graph",
            Self::Shared => "inspector-tab-shared",
            Self::Attempts => "inspector-tab-attempts",
            Self::Plugins => "inspector-tab-plugins",
        }
    }
}

/// An agent of the space's graph, as the Graph tab lists it.
#[derive(Clone)]
pub struct InspectorAgent {
    /// Its node id; pause and resume name it.
    pub id: SharedString,
    /// Its name.
    pub label: SharedString,
    /// Its role, e.g. "Writer · api".
    pub role: SharedString,
    /// What the graph says it is doing, e.g. "running" or "paused".
    pub state: SharedString,
    /// Whether its pane runs, has exited, or has none. `None` when it runs over RPC and has no terminal.
    pub pane: Option<PaneLive>,
    /// The pane it runs in, which Focus shows; `None` when it has none.
    pub pane_id: Option<u64>,
    /// Whether the user paused it: the button then resumes it.
    pub paused: bool,
}

/// The shared contract, as the Shared tab shows it.
#[derive(Clone)]
pub struct InspectorContract {
    /// Its short name.
    pub title: SharedString,
    /// Its revision number.
    pub version: u32,
    /// The agent that wrote this revision.
    pub author: SharedString,
    /// What it is for. Already masked by the caller.
    pub summary: SharedString,
}

/// What the Inspector shows: the tab it is on and the data of each tab. The host builds it for the space shown.
pub struct InspectorData {
    /// The tab shown.
    pub tab: InspectorTab,
    /// The agents of the space's graph, in graph order.
    pub agents: Vec<InspectorAgent>,
    /// The space's attempts, as the attempt strip shows them.
    pub attempts: Vec<AttemptChip>,
    /// Whether Fork can start another attempt.
    pub can_fork: bool,
    /// Whether the latest round of attempts can be reviewed.
    pub can_review: bool,
    /// The shared contract, once there is one.
    pub contract: Option<InspectorContract>,
    /// Findings in the channel that are not resolved.
    pub open_findings: usize,
    /// Notes in the channel.
    pub notes: usize,
    /// The space's tools, in dock order.
    pub tools: Vec<DockItem>,
}

/// What a click in the Inspector asks the host to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectorAction {
    /// Show `tab`.
    SelectTab(InspectorTab),
    /// Hide the Inspector.
    Close,
    /// Show the pane with this id.
    Focus(u64),
    /// Pause the agent with this node id, or resume it when `paused` is false.
    SetPaused {
        /// The agent's node id.
        agent: SharedString,
        /// Whether to pause (true) or resume (false).
        paused: bool,
    },
    /// Show this attempt's agent in the pane area.
    SelectAttempt(u32),
    /// Start another attempt.
    Fork,
    /// Open the review sheet on the latest round.
    Review,
    /// Open the tool `name`, or close it when `open` is true.
    ToggleTool {
        /// The tool's manifest name.
        name: SharedString,
        /// Whether it is open now.
        open: bool,
    },
}

type ActionHandler = Rc<dyn Fn(InspectorAction, &mut Window, &mut App)>;

/// The Inspector panel for an [`InspectorData`]. Its buttons call the handler given to [`Inspector::on_action`].
#[derive(IntoElement)]
pub struct Inspector {
    data: InspectorData,
    on_action: Option<ActionHandler>,
}

impl Inspector {
    /// The panel showing `data`, with no handler.
    pub fn new(data: InspectorData) -> Self {
        Self {
            data,
            on_action: None,
        }
    }

    /// Call `handler` with each click's [`InspectorAction`].
    pub fn on_action(
        mut self,
        handler: impl Fn(InspectorAction, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_action = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Inspector {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let emit: ActionHandler = self.on_action.unwrap_or_else(|| Rc::new(|_, _, _| {}));
        let data = self.data;

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .h(px(32.0))
            .px(Theme::SPACE_3)
            .border_b_1()
            .border_color(theme.border_subtle)
            .child(
                div()
                    .text_size(Theme::TEXT_SM)
                    .text_color(theme.text_muted)
                    .child("Inspector"),
            )
            .child({
                let emit = emit.clone();
                Button::new("inspector-close", "")
                    .icon(IconName::Close)
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .tooltip("Hide the Inspector (⌘I)")
                    .selector("inspector-close")
                    .on_click(move |_, window, cx| emit(InspectorAction::Close, window, cx))
            });

        let tabs = InspectorTab::ALL
            .into_iter()
            .enumerate()
            .map(|(index, tab)| {
                let emit = emit.clone();
                Button::new(("inspector-tab", index), tab.label())
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .selected(data.tab == tab)
                    .selector(tab.selector())
                    .on_click(move |_, window, cx| {
                        emit(InspectorAction::SelectTab(tab), window, cx)
                    })
            });

        let body = match data.tab {
            InspectorTab::Graph => graph_rows(&data.agents, &emit, &theme),
            InspectorTab::Shared => shared_rows(&data, &theme),
            InspectorTab::Attempts => attempt_rows(&data, &emit, &theme),
            InspectorTab::Plugins => tool_rows(&data.tools, &emit, &theme),
        };

        div()
            .debug_selector(|| "inspector".to_owned())
            .flex()
            .flex_col()
            .flex_none()
            .w(px(INSPECTOR_WIDTH))
            .h_full()
            .min_h_0()
            .bg(theme.surface)
            .border_l_1()
            .border_color(theme.border_subtle)
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(2.0))
                    .px(Theme::SPACE_2)
                    .py(Theme::SPACE_1)
                    .border_b_1()
                    .border_color(theme.border_subtle)
                    .children(tabs),
            )
            .child(
                div()
                    .id("inspector-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(Theme::SPACE_3)
                    .flex()
                    .flex_col()
                    .gap(Theme::SPACE_2)
                    .children(body),
            )
    }
}

/// A muted line for an empty tab.
fn empty(theme: &Theme, text: &'static str) -> AnyElement {
    div()
        .text_size(Theme::TEXT_SM)
        .text_color(theme.text_muted)
        .child(text)
        .into_any_element()
}

fn graph_rows(agents: &[InspectorAgent], emit: &ActionHandler, theme: &Theme) -> Vec<AnyElement> {
    if agents.is_empty() {
        return vec![empty(theme, "This space has no agents yet.")];
    }
    agents
        .iter()
        .enumerate()
        .map(|(index, agent)| {
            let dot = match agent.pane {
                Some(PaneLive::Running) => theme.success,
                Some(PaneLive::Exited(_)) => theme.text_subtle,
                Some(PaneLive::NoPane) | None => theme.border,
            };
            let pane_text: SharedString = match agent.pane {
                Some(PaneLive::Running) => "pane running".into(),
                Some(PaneLive::Exited(Some(code))) => format!("pane exited {code}").into(),
                Some(PaneLive::Exited(None)) => "pane exited".into(),
                Some(PaneLive::NoPane) => "no pane".into(),
                None => "runs over RPC".into(),
            };
            let mut actions = div().flex().flex_none().gap(Theme::SPACE_1);
            if let Some(pane) = agent.pane_id {
                let emit = emit.clone();
                actions = actions.child(
                    Button::new(("inspector-focus", index), "Focus")
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Compact)
                        .selector(format!("inspector-focus-{index}"))
                        .on_click(move |_, window, cx| {
                            emit(InspectorAction::Focus(pane), window, cx)
                        }),
                );
            }
            let (label, paused) = if agent.paused {
                ("Resume", false)
            } else {
                ("Pause", true)
            };
            let emit = emit.clone();
            let id = agent.id.clone();
            actions = actions.child(
                Button::new(("inspector-pause", index), label)
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .selector(format!("inspector-pause-{index}"))
                    .on_click(move |_, window, cx| {
                        emit(
                            InspectorAction::SetPaused {
                                agent: id.clone(),
                                paused,
                            },
                            window,
                            cx,
                        )
                    }),
            );
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .py(px(4.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(Theme::SPACE_2)
                        .child(div().size(px(6.0)).rounded_full().flex_none().bg(dot))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(Theme::TEXT_SM)
                                .child(agent.label.clone()),
                        )
                        .child(actions),
                )
                .child(
                    div()
                        .pl(px(14.0))
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_muted)
                        .truncate()
                        .child(format!("{} · {} · {}", agent.role, agent.state, pane_text)),
                )
                .into_any_element()
        })
        .collect()
}

fn shared_rows(data: &InspectorData, theme: &Theme) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    match &data.contract {
        Some(contract) => rows.push(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(Theme::TEXT_SM)
                        .child(format!("{} · v{}", contract.title, contract.version)),
                )
                .child(
                    div()
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_muted)
                        .child(format!("by {}", contract.author)),
                )
                .child(
                    div()
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_muted)
                        .child(contract.summary.clone()),
                )
                .into_any_element(),
        ),
        None => rows.push(empty(theme, "No shared contract yet.")),
    }
    rows.push(
        div()
            .text_size(Theme::TEXT_XS)
            .text_color(theme.text_muted)
            .child(format!(
                "{} open · {}",
                plural(data.open_findings, "finding"),
                plural(data.notes, "note")
            ))
            .into_any_element(),
    );
    rows
}

fn attempt_rows(data: &InspectorData, emit: &ActionHandler, theme: &Theme) -> Vec<AnyElement> {
    let mut rows: Vec<AnyElement> = if data.attempts.is_empty() {
        vec![empty(
            theme,
            "No attempts yet. Fork a writer to compare approaches.",
        )]
    } else {
        data.attempts
            .iter()
            .map(|chip| {
                let number = chip.number;
                let emit = emit.clone();
                let accent = if chip.active {
                    theme.accent
                } else {
                    theme.text
                };
                div()
                    .id(("inspector-attempt", number as usize))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .p(px(6.0))
                    .rounded(Theme::RADIUS_SM)
                    .border_1()
                    .border_color(if chip.active {
                        theme.accent
                    } else {
                        theme.border_subtle
                    })
                    .cursor_pointer()
                    .on_click(move |_, window, cx| {
                        emit(InspectorAction::SelectAttempt(number), window, cx)
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(Theme::SPACE_2)
                            .child(Badge::new(chip.state.label()).tone(chip.state.tone()))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(Theme::TEXT_SM)
                                    .text_color(accent)
                                    .child(chip.label.clone()),
                            ),
                    )
                    .into_any_element()
            })
            .collect()
    };
    let fork = {
        let emit = emit.clone();
        Button::new("inspector-fork", "Fork")
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Compact)
            .disabled(!data.can_fork)
            .tooltip("Start another attempt (⌘⇧F)")
            .selector("inspector-fork")
            .on_click(move |_, window, cx| emit(InspectorAction::Fork, window, cx))
    };
    let review = {
        let emit = emit.clone();
        Button::new("inspector-review", "Review")
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Compact)
            .disabled(!data.can_review)
            .tooltip("Compare the latest round (⌘⇧R)")
            .selector("inspector-review")
            .on_click(move |_, window, cx| emit(InspectorAction::Review, window, cx))
    };
    rows.push(
        div()
            .flex()
            .gap(Theme::SPACE_1)
            .pt(Theme::SPACE_1)
            .child(fork)
            .child(review)
            .into_any_element(),
    );
    rows
}

fn tool_rows(tools: &[DockItem], emit: &ActionHandler, theme: &Theme) -> Vec<AnyElement> {
    if tools.is_empty() {
        return vec![empty(theme, "No tools for this space.")];
    }
    tools
        .iter()
        .enumerate()
        .map(|(index, tool)| {
            let label = if tool.locked {
                "Approve…"
            } else if tool.open {
                "Close"
            } else {
                "Open"
            };
            let emit = emit.clone();
            let name = tool.name.clone();
            let open = tool.open;
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .py(px(4.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(Theme::TEXT_SM)
                                .truncate()
                                .child(tool.title.clone()),
                        )
                        .children(tool.detail.clone().map(|detail| {
                            div()
                                .text_size(Theme::TEXT_XS)
                                .text_color(theme.text_muted)
                                .truncate()
                                .child(detail)
                        })),
                )
                .child(
                    Button::new(("inspector-tool", index), label)
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Compact)
                        .selected(tool.open)
                        .selector(format!("inspector-tool-{}", tool.name))
                        .on_click(move |_, window, cx| {
                            emit(
                                InspectorAction::ToggleTool {
                                    name: name.clone(),
                                    open,
                                },
                                window,
                                cx,
                            )
                        }),
                )
                .into_any_element()
        })
        .collect()
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
}
