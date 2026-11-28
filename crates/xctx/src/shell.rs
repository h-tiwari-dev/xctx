//! The window shell around Home and the spaces: the Roster on the left, the title bar row, the command palette,
//! the window's translucency, and dropping dragged panes and tools onto panes.

use gpui::{App, Context, SharedString, Window, WindowAppearance, WindowBackgroundAppearance};
use xctx_core::SpaceId;
use xctx_graph::{AgentNodeId, AgentState};
use xctx_layout::{DropEdge, PaneId, PaneKind, SplitAxis};
use xctx_nav::palette::{PaletteItem, PaletteKind};
use xctx_nav::roster::{Roster as NavRoster, RosterGroup, SpaceActivity, SpaceInput, SpaceState};
use xctx_ui::roster::{
    ROSTER_RAIL_WIDTH, ROSTER_WIDTH, RosterAgentItem, RosterSection, RosterSpaceItem, RowState,
};
use xctx_ui::title_bar::{SpaceHeaderInfo, TitleBar};
use xctx_ui::{DraggedTool, PaneLive, SHAPES, Theme, shape_label};

use crate::flow::pane_lives;
use crate::space_layout::layout_shape;
use crate::space_tab::PaneTerminal;
use crate::ui_prefs::prefs_path;
use crate::workspace::{View, Workspace, terminal_theme};

/// The theme for the system `appearance`, translucent when `translucent`.
pub(crate) fn theme_for(appearance: WindowAppearance, translucent: bool) -> Theme {
    let theme = match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => Theme::light(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme::dark(),
    };
    if translucent {
        theme.translucent()
    } else {
        theme
    }
}

/// How long ago `secs` seconds is, as a Roster row shows it: `now`, `5m`, `3h`, `2d`.
pub(crate) fn short_age(secs: u64) -> String {
    match secs {
        0..60 => "now".to_owned(),
        60..3_600 => format!("{}m", secs / 60),
        3_600..86_400 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// A Roster row's state for a space whose rolled-up state is `state`, given whether an agent of its open tab
/// is running in its terminal (the graph only knows what agents reported, the terminal knows they run).
fn space_row_state(state: SpaceState, agent_running: bool) -> RowState {
    match state {
        SpaceState::NeedsYou => RowState::NeedsYou,
        SpaceState::Failed => RowState::Failed,
        SpaceState::Running => RowState::Running,
        SpaceState::Idle if agent_running => RowState::Running,
        SpaceState::Idle => RowState::Idle,
    }
}

/// The Roster section a row in `state` is listed under, in display order.
fn row_group(state: RowState) -> RosterGroup {
    match state {
        RowState::NeedsYou | RowState::Failed | RowState::Paused => RosterGroup::NeedsYou,
        RowState::Running => RosterGroup::Running,
        RowState::Idle => RosterGroup::Idle,
    }
}

/// An agent row's state from its graph state and what its pane's terminal is doing.
fn agent_row_state(state: &AgentState, live: Option<PaneLive>) -> RowState {
    match state {
        AgentState::Paused => RowState::Paused,
        AgentState::Blocked { .. } => RowState::NeedsYou,
        AgentState::Failed { .. } => RowState::Failed,
        AgentState::Running => RowState::Running,
        AgentState::Idle | AgentState::Done => match live {
            Some(PaneLive::Running) => RowState::Running,
            _ => RowState::Idle,
        },
    }
}

/// What an agent row says it is doing when the graph has nothing better: its terminal's state.
fn agent_activity(state: &AgentState, live: Option<PaneLive>, from_graph: &str) -> String {
    match (state, live) {
        (AgentState::Idle, Some(PaneLive::Running)) => "running in its pane".to_owned(),
        (AgentState::Idle, Some(PaneLive::Exited(_))) => "exited".to_owned(),
        (AgentState::Idle, Some(PaneLive::NoPane)) => "no pane — start it from ⌘K".to_owned(),
        _ => from_graph.to_owned(),
    }
}

/// One space as the Roster lists it, before it is drawn.
pub(crate) struct RosterEntry {
    pub(crate) id: SpaceId,
    pub(crate) item: RosterSpaceItem,
}

/// Palette item ids: `action:<name>`, `space:<slug>`, `agent:<slug>:<agent>`, `tool:<name>`.
const ACTION: &str = "action:";
const SPACE: &str = "space:";
const AGENT: &str = "agent:";
const TOOL: &str = "tool:";

impl Workspace {
    /// Set the theme for `appearance` (translucent or not, as chosen) and restyle every live terminal.
    pub(crate) fn apply_theme(&mut self, appearance: WindowAppearance, cx: &mut Context<Self>) {
        let theme = theme_for(appearance, self.prefs.translucent);
        cx.set_global(theme);
        self.terminal_theme = terminal_theme(&theme);
        let terminal_theme = self.terminal_theme;
        for tab in self.tabs.values() {
            for pane in tab.terminals.values().chain(tab.attempt_terminals.values()) {
                if let PaneTerminal::Running { view, .. } = pane {
                    view.update(cx, |terminal, cx| terminal.set_theme(terminal_theme, cx));
                }
            }
        }
        if let Some(view) = &self.parent.view {
            view.update(cx, |terminal, cx| terminal.set_theme(terminal_theme, cx));
        }
        cx.notify();
    }

    /// Make the window translucent (the blurred desktop shows through its chrome, panes and terminals), or opaque
    /// again; the choice is kept for the next run.
    pub(crate) fn toggle_translucency(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prefs.translucent = !self.prefs.translucent;
        window.set_background_appearance(if self.prefs.translucent {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Opaque
        });
        self.apply_theme(window.appearance(), cx);
        self.save_prefs(cx);
        self.info(
            if self.prefs.translucent {
                "Translucent window on"
            } else {
                "Translucent window off"
            },
            cx,
        );
    }

    /// Collapse the Roster to its rail of icons, or expand it; kept for the next run.
    pub(crate) fn toggle_roster(&mut self, cx: &mut Context<Self>) {
        self.prefs.roster_collapsed = !self.prefs.roster_collapsed;
        self.save_prefs(cx);
        cx.notify();
    }

    /// Save the window preferences in the background; a failure is shown, the choice stays for this run.
    pub(crate) fn save_prefs(&self, cx: &mut Context<Self>) {
        let Some(dir) = self
            .store
            .as_ref()
            .map(|store| store.dir().to_path_buf())
            .or_else(|| xctx_core::store::data_dir().ok())
        else {
            return;
        };
        let prefs = self.prefs;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let result = executor
                .spawn(async move { prefs.save(&prefs_path(&dir)) })
                .await;
            if let Err(e) = result {
                let _ = this.update(cx, |this, cx| this.error(e, cx));
            }
        })
        .detach();
    }

    /// The active spaces as the Roster lists them: grouped worst-first (needs you, running, idle), most recent
    /// first inside a group. `⌘1`–`⌘9` follow this order.
    pub(crate) fn roster_entries(&self, cx: &App) -> Vec<RosterEntry> {
        let theme = Theme::of(cx);
        let inbox = self.inbox.as_ref().map_or(&[][..], |inbox| inbox.items());
        let inputs = self.spaces.iter().map(|space| {
            let state = self.graphs.get(&space.id);
            let has_dead_pane = self.tabs.get(&space.id).is_some_and(|tab| {
                tab.terminals
                    .values()
                    .any(|terminal| matches!(terminal, PaneTerminal::Failed(_)))
            });
            SpaceInput {
                space,
                graph: state.map(|state| state.graph()),
                loop_run: state.map(|state| state.loop_run()),
                activity: SpaceActivity {
                    has_dead_pane,
                    ..SpaceActivity::default()
                },
            }
        });
        let rows = NavRoster::build(inputs, inbox, crate::attempts::now());
        let active = match &self.view {
            View::Space(id) => Some(id),
            View::Home => None,
        };
        let mut entries: Vec<(RosterGroup, RosterEntry)> = rows
            .spaces()
            .iter()
            .filter_map(|row| {
                let space = self.spaces.iter().find(|space| space.id == row.id)?;
                let tab = self.tabs.get(&row.id);
                let running = tab.is_some_and(|tab| tab.agent_running(cx));
                let state = space_row_state(row.state, running);
                let lives = match (tab, self.graphs.get(&row.id)) {
                    (Some(tab), Some(graph)) => Some(pane_lives(graph.graph(), tab, cx)),
                    _ => None,
                };
                let agents = row
                    .agents
                    .iter()
                    .map(|agent| {
                        let live = lives
                            .as_ref()
                            .and_then(|lives| lives.get(&agent.id).copied());
                        let activity = match tab.and_then(|tab| tab.rpc_retry_badge(&agent.id)) {
                            Some(badge) => badge.to_owned(),
                            None => agent_activity(&agent.state, live, &agent.activity),
                        };
                        RosterAgentItem {
                            id: agent.id.as_str().to_owned().into(),
                            label: agent.label.clone().into(),
                            color: agent
                                .color_index
                                .map_or(theme.accent, |index| theme.agent_color(index)),
                            state: agent_row_state(&agent.state, live),
                            activity: activity.into(),
                        }
                    })
                    .collect();
                let repos = match space.repos.len() {
                    1 => "1 repo".to_owned(),
                    n => format!("{n} repos"),
                };
                let item = RosterSpaceItem {
                    id: row.id.as_str().to_owned().into(),
                    title: row.title.clone().into(),
                    subtitle: format!("{repos} · :{}", space.port).into(),
                    state,
                    age: short_age(row.age_secs).into(),
                    active: active == Some(&row.id),
                    shortcut: None,
                    production: row.production,
                    pending: row.pending_gates,
                    agents,
                };
                Some((
                    row_group(state),
                    RosterEntry {
                        id: row.id.clone(),
                        item,
                    },
                ))
            })
            .collect();
        // Stable, so the rollup's own order (danger first, most recent first) holds inside each group.
        entries.sort_by_key(|(group, _)| *group);
        entries
            .into_iter()
            .enumerate()
            .map(|(index, (_, mut entry))| {
                entry.item.shortcut = (index < 9).then_some(index + 1);
                entry
            })
            .collect()
    }

    /// The space `⌘<index+1>` shows, in Roster order.
    pub(crate) fn roster_space(&self, index: usize, cx: &App) -> Option<SpaceId> {
        self.roster_entries(cx)
            .into_iter()
            .nth(index)
            .map(|entry| entry.id)
    }

    /// The Roster, wired to show Home, a space or an agent's pane.
    pub(crate) fn render_roster(&self, cx: &mut Context<Self>) -> xctx_ui::roster::Roster {
        let mut sections: Vec<RosterSection> = Vec::new();
        for entry in self.roster_entries(cx) {
            let label = row_group(entry.item.state).label();
            match sections.last_mut() {
                Some(section) if section.label.as_ref() == label => section.spaces.push(entry.item),
                _ => sections.push(RosterSection {
                    label: label.into(),
                    spaces: vec![entry.item],
                }),
            }
        }
        let entity = cx.entity().downgrade();
        let (home, select, agent, inbox, new_space) = (
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity,
        );
        xctx_ui::roster::Roster::new(sections)
            .collapsed(self.prefs.roster_collapsed)
            .home_active(self.view == View::Home)
            .inbox_count(self.inbox_pending())
            .on_home(move |window, cx| {
                let _ = home.update(cx, |this, cx| this.show_home(window, cx));
            })
            .on_select_space(move |id, window, cx| {
                let id = SpaceId::new(id.to_string());
                let _ = select.update(cx, |this, cx| this.show_space(&id, window, cx));
            })
            .on_select_agent(move |space, agent_id, window, cx| {
                let space = SpaceId::new(space.to_string());
                let agent_id = AgentNodeId::new(agent_id.to_string());
                let _ = agent.update(cx, |this, cx| {
                    this.go_to_agent(&space, &agent_id, window, cx)
                });
            })
            .on_inbox(move |window, cx| {
                let _ = inbox.update(cx, |this, cx| this.show_inbox(None, window, cx));
            })
            .on_new_space(move |window, cx| {
                let _ = new_space.update(cx, |this, cx| this.show_home(window, cx));
            })
    }

    /// The Roster's width as drawn now.
    pub(crate) fn roster_width(&self) -> gpui::Pixels {
        if self.prefs.roster_collapsed {
            ROSTER_RAIL_WIDTH
        } else {
            ROSTER_WIDTH
        }
    }

    /// Show space `space` with `agent`'s pane focused, starting its pane when it has none.
    pub(crate) fn go_to_agent(
        &mut self,
        space: &SpaceId,
        agent: &AgentNodeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_space(space, window, cx);
        if self.active_space_id().as_ref() != Some(space) {
            return;
        }
        let has_pane = self
            .graphs
            .get(space)
            .and_then(|state| state.graph().node(agent))
            .is_some_and(|node| node.pane_id.is_some());
        let result = if has_pane {
            self.open_agent_pane(space, agent, window, cx)
        } else {
            self.start_agent_pane(space, agent, window, cx).map(|_| ())
        };
        if let Err(e) = result {
            self.show_op_error(e, cx);
        }
    }

    /// The title bar row: the Roster toggle, the shown space's header and actions, and the palette button.
    pub(crate) fn render_title_bar(&self, cx: &mut Context<Self>) -> TitleBar {
        let entity = cx.entity().downgrade();
        let (sidebar, palette, graph, add_agent, add_repo, reshape, ship, discard) = (
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity,
        );
        let (title, info) = match self.active_tab() {
            Some(tab) => {
                let space = &tab.space;
                (
                    space.title.clone(),
                    Some(SpaceHeaderInfo {
                        shape: layout_shape(space.shape),
                        flow_open: self.prefs.inspector_open
                            && self.inspector_tab == xctx_ui::inspector::InspectorTab::Graph,
                        production: space.production,
                        // The status line shows the branch and env; the title bar only warns about production,
                        // so the title and actions keep their room.
                        branch: SharedString::default(),
                        env: if space.production {
                            space.effective_mise_env().into()
                        } else {
                            SharedString::default()
                        },
                    }),
                )
            }
            None => ("Home".to_owned(), None),
        };
        let mut bar = TitleBar::new(title)
            .sidebar(self.roster_width(), self.prefs.roster_collapsed)
            .on_toggle_sidebar(move |_, cx| {
                let _ = sidebar.update(cx, |this, cx| this.toggle_roster(cx));
            })
            .on_palette(move |window, cx| {
                let _ = palette.update(cx, |this, cx| this.toggle_palette(window, cx));
            });
        if let Some(info) = info {
            bar = bar
                .space(info)
                .on_toggle_flow(move |window, cx| {
                    let _ = graph.update(cx, |this, cx| {
                        if this.dialog.is_none() {
                            this.toggle_flow(window, cx);
                        }
                    });
                })
                .on_add_agent(move |window, cx| {
                    let _ = add_agent.update(cx, |this, cx| {
                        if this.dialog.is_none() {
                            this.open_add_agent_dialog(window, cx);
                        }
                    });
                })
                .on_add_repo(move |window, cx| {
                    let _ = add_repo.update(cx, |this, cx| {
                        if this.dialog.is_none() {
                            this.open_add_repo_dialog(window, cx);
                        }
                    });
                })
                .on_shape(move |shape, window, cx| {
                    let _ = reshape.update(cx, |this, cx| this.reshape(shape, window, cx));
                })
                .on_ship(move |window, cx| {
                    let _ = ship.update(cx, |this, cx| this.open_ship(window, cx));
                })
                .on_discard(move |window, cx| {
                    let _ = discard.update(cx, |this, cx| this.open_discard(window, cx));
                });
        }
        bar
    }

    /// ⌘K: open the command palette over whatever is shown, or close it.
    pub(crate) fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette_open {
            self.palette
                .update(cx, |palette, cx| palette.close(window, cx));
            self.palette_closed(window, cx);
            return;
        }
        if self.dialog.is_some() {
            return;
        }
        let items = self.palette_items(cx);
        self.palette_open = true;
        self.palette
            .update(cx, |palette, cx| palette.open(items, window, cx));
        cx.notify();
    }

    /// The palette closed: give the keys back to what was shown.
    pub(crate) fn palette_closed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_open = false;
        match self.view {
            View::Home => self.focus_home(window, cx),
            View::Space(_) => self.focus_active_pane(window, cx),
        }
        cx.notify();
    }

    /// Everything the palette offers now: the actions that apply to what is shown (with their keys), every
    /// space, every agent of a space whose graph is loaded, and the shown space's tools.
    fn palette_items(&mut self, cx: &mut Context<Self>) -> Vec<PaletteItem> {
        let action = |name: &str, title: &str, keys: Option<&str>, keywords: &[&str]| {
            let item = PaletteItem::new(format!("{ACTION}{name}"), PaletteKind::Action, title)
                .with_keywords(keywords.iter().copied());
            match keys {
                Some(keys) => item.with_key_chord(keys),
                None => item,
            }
        };
        let mut items = vec![
            action("home", "Go to Home", Some("⌘0"), &["parent agent", "start"]),
            action(
                "new-space",
                "New space…",
                None,
                &["task", "launch", "draft"],
            ),
            action(
                "inbox",
                "Open Inbox",
                None,
                &["gates", "approve", "notifications"],
            ),
            action(
                "toggle-roster",
                "Toggle sidebar",
                Some("⌘B"),
                &["roster", "collapse"],
            ),
            action(
                "toggle-inspector",
                if self.prefs.inspector_open {
                    "Hide Inspector"
                } else {
                    "Show Inspector"
                },
                Some("⌘I"),
                &["panel", "graph", "agents", "attempts", "tools"],
            ),
            action(
                "inspector-plugins",
                "Show Plugins in Inspector",
                Some("⌘⇧P"),
                &["tools", "plugins", "mcp"],
            ),
            action(
                "toggle-translucency",
                if self.prefs.translucent {
                    "Make window opaque"
                } else {
                    "Make window translucent"
                },
                None,
                &["blur", "transparent", "vibrancy", "appearance"],
            ),
            action(
                "rescan",
                "Rescan indexed folders",
                None,
                &["repos", "index"],
            ),
        ];
        if let Some(tab) = self.active_tab() {
            let can_fork = self.strip_can_fork(tab);
            let current = layout_shape(tab.space.shape);
            items.extend([
                action(
                    "split-right",
                    "Split pane right",
                    Some("⌘D"),
                    &["shell", "terminal"],
                ),
                action(
                    "split-down",
                    "Split pane down",
                    Some("⌘⇧D"),
                    &["shell", "terminal"],
                ),
                action("close-pane", "Close pane", Some("⌘W"), &[]),
                action("zoom", "Zoom pane", Some("⌘⇧Z"), &["maximize", "focus"]),
                action("focus-next", "Focus next pane", Some("⌘]"), &[]),
                action("focus-prev", "Focus previous pane", Some("⌘["), &[]),
                action(
                    "flow",
                    if self.prefs.inspector_open
                        && self.inspector_tab == xctx_ui::inspector::InspectorTab::Graph
                    {
                        "Hide Activity"
                    } else {
                        "Show Activity"
                    },
                    Some("⌘G"),
                    &[
                        "graph", "agents", "loop", "shared", "channel", "contract", "timeline",
                    ],
                ),
                action(
                    "add-agent",
                    "Add agent…",
                    None,
                    &["helper", "lane", "tests", "reviewer"],
                ),
                action("add-repo", "Add repo to space…", None, &["widen"]),
                action(
                    "review",
                    "Review attempts",
                    Some("⌘⇧R"),
                    &["compare", "adopt"],
                ),
                action("ship", "Ship space…", None, &["push", "pr", "pull request"]),
                action("discard", "Discard space…", None, &["delete", "remove"]),
            ]);
            if can_fork {
                items.push(action(
                    "fork",
                    "Fork attempts…",
                    Some("⌘⇧F"),
                    &["branch", "solutions"],
                ));
            }
            for shape in SHAPES {
                if shape != current {
                    let label = shape_label(shape);
                    items.push(action(
                        &format!("shape-{}", label.to_lowercase()),
                        &format!("Reshape to {label}"),
                        None,
                        &["layout", "shape"],
                    ));
                }
            }
        }
        items.push(action("quit", "Quit xctx", Some("⌘Q"), &["exit"]));

        for entry in self.roster_entries(cx) {
            let mut item = PaletteItem::new(
                format!("{SPACE}{}", entry.id),
                PaletteKind::Space,
                entry.item.title.to_string(),
            )
            .with_subtitle(format!("{} · {}", entry.id, entry.item.subtitle))
            .with_keywords([entry.id.as_str().to_owned()]);
            if let Some(slot) = entry.item.shortcut {
                item = item.with_key_chord(format!("⌘{slot}"));
            }
            items.push(item);
            for agent in &entry.item.agents {
                items.push(
                    PaletteItem::new(
                        format!("{AGENT}{}:{}", entry.id, agent.id),
                        PaletteKind::Agent,
                        agent.label.to_string(),
                    )
                    .with_subtitle(format!("{} · {}", entry.item.title, agent.activity)),
                );
            }
        }

        if let Some(id) = self.active_space_id() {
            for tool in self.tool_dock_view(&id).items {
                let verb = if tool.open { "Close" } else { "Open" };
                items.push(
                    PaletteItem::new(
                        format!("{TOOL}{}", tool.name),
                        PaletteKind::Plugin,
                        format!("{verb} {}", tool.title),
                    )
                    .with_subtitle("tool")
                    .with_keywords([tool.name.to_string()]),
                );
            }
        }
        items
    }

    /// Run the palette item `id` (see [`Workspace::palette_items`]).
    pub(crate) fn run_palette_item(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.palette_closed(window, cx);
        if let Some(name) = id.strip_prefix(ACTION) {
            self.run_palette_action(name, window, cx);
        } else if let Some(slug) = id.strip_prefix(SPACE) {
            self.show_space(&SpaceId::new(slug), window, cx);
        } else if let Some(rest) = id.strip_prefix(AGENT) {
            if let Some((slug, agent)) = rest.split_once(':') {
                self.go_to_agent(&SpaceId::new(slug), &AgentNodeId::new(agent), window, cx);
            }
        } else if let Some(name) = id.strip_prefix(TOOL) {
            let Some(space) = self.active_space_id() else {
                return;
            };
            let open = self
                .tool_dock_view(&space)
                .items
                .iter()
                .any(|tool| tool.name.as_ref() == name && tool.open);
            if open {
                self.dock_close(name, window, cx);
            } else {
                self.dock_open(name, window, cx);
            }
        }
    }

    fn run_palette_action(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        match name {
            "home" | "new-space" => self.show_home(window, cx),
            "inbox" => self.show_inbox(None, window, cx),
            "toggle-roster" => self.toggle_roster(cx),
            "toggle-inspector" => {
                let open = !self.prefs.inspector_open;
                self.set_inspector_open(open, cx)
            }
            "inspector-plugins" => {
                self.show_inspector_tab(xctx_ui::inspector::InspectorTab::Plugins, cx)
            }
            "toggle-translucency" => self.toggle_translucency(window, cx),
            "rescan" => self.rescan(cx),
            "split-right" => self.split(SplitAxis::Horizontal, window, cx),
            "split-down" => self.split(SplitAxis::Vertical, window, cx),
            "close-pane" => self.close_focused_pane(window, cx),
            "zoom" => self.toggle_zoom(window, cx),
            "focus-next" => self.cycle_focus(true, window, cx),
            "focus-prev" => self.cycle_focus(false, window, cx),
            "flow" => self.toggle_flow(window, cx),
            "add-agent" => self.open_add_agent_dialog(window, cx),
            "add-repo" => self.open_add_repo_dialog(window, cx),
            "review" => self.open_review(window, cx),
            "fork" => self.fork_from_keys(window, cx),
            "ship" => self.open_ship(window, cx),
            "discard" => self.open_discard(window, cx),
            "quit" => {
                if !self.refuse_while_busy(cx) {
                    cx.quit();
                }
            }
            other => {
                if let Some(shape) = other.strip_prefix("shape-").and_then(|label| {
                    SHAPES
                        .into_iter()
                        .find(|shape| shape_label(*shape).eq_ignore_ascii_case(label))
                }) {
                    self.reshape(shape, window, cx);
                }
            }
        }
    }

    /// A pane header was dropped at `edge` of `target`: move the pane there (or swap the two).
    pub(crate) fn drop_pane(
        &mut self,
        pane: PaneId,
        target: PaneId,
        edge: DropEdge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dialog.is_some() || pane == target {
            return;
        }
        self.change_layout(window, cx, |layout| {
            layout
                .drop_pane(pane, target, edge)
                .map_err(|e| format!("cannot move the pane: {e}"))
        });
    }

    /// A tool was dragged out of the dock and dropped at `edge` of `target`: open it there, or move its pane
    /// there when it is open already.
    pub(crate) fn drop_tool(
        &mut self,
        tool: &DraggedTool,
        target: PaneId,
        edge: DropEdge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dialog.is_some() {
            return;
        }
        let Some(id) = self.active_space_id() else {
            return;
        };
        let open = self.tabs.get(&id).and_then(|tab| {
            tab.layout
                .panes()
                .into_iter()
                .find(|pane| {
                    pane.role.kind == PaneKind::Plugin
                        && pane.role.plugin.as_deref() == Some(tool.name.as_ref())
                })
                .map(|pane| pane.id)
        });
        match open {
            Some(pane) => self.drop_pane(pane, target, edge, window, cx),
            None => {
                if let Some(pane) = self.dock_open_returning(&tool.name, window, cx) {
                    self.drop_pane(pane, target, edge, window, cx);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_are_short() {
        assert_eq!(short_age(5), "now");
        assert_eq!(short_age(125), "2m");
        assert_eq!(short_age(3 * 3_600 + 1), "3h");
        assert_eq!(short_age(2 * 86_400), "2d");
    }

    #[test]
    fn a_running_terminal_lifts_an_idle_space_into_running_but_not_one_that_needs_you() {
        assert_eq!(space_row_state(SpaceState::Idle, true), RowState::Running);
        assert_eq!(space_row_state(SpaceState::Idle, false), RowState::Idle);
        assert_eq!(
            space_row_state(SpaceState::NeedsYou, true),
            RowState::NeedsYou
        );
        assert_eq!(row_group(RowState::Failed), RosterGroup::NeedsYou);
    }

    #[test]
    fn an_idle_agent_reads_as_running_while_its_pane_runs() {
        assert_eq!(
            agent_row_state(&AgentState::Idle, Some(PaneLive::Running)),
            RowState::Running
        );
        assert_eq!(
            agent_row_state(&AgentState::Paused, Some(PaneLive::Running)),
            RowState::Paused
        );
        assert_eq!(agent_row_state(&AgentState::Done, None), RowState::Idle);
    }
}
