use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::error::LayoutError;
use crate::tree::{PaneId, PaneNode, PaneTree, SplitAxis, SplitId};

/// How a space's panes are arranged for the kind of work at hand.
///
/// Every shape shows one agent pane per repo and keeps every existing agent pane; the shapes differ in which
/// auxiliary pane each repo gets and how the two groups are placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    /// Agents side by side across the top (about two thirds of the height), a shell per repo in a row below.
    Dev,
    /// Agents stacked on the left half, a shell per repo stacked on the right half, for logs and repro.
    Debug,
    /// A git pane per repo side by side across the top (about two thirds of the height), agents in a row below.
    Review,
}

/// What runs in a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneKind {
    /// An agent session. Reshaping never drops one.
    Agent,
    /// A shell.
    Shell,
    /// A git tool such as `lazygit`.
    Git,
    /// A mounted plugin's pane; [`PaneRole::plugin`] names the plugin. Reshaping never drops one: only
    /// unmounting the plugin closes it.
    Plugin,
    /// A standalone file viewer panel; [`PaneRole::file`] names the file path. Reshaping never drops one.
    File,
}

/// What a pane is for: its kind and the repo it works in.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaneRole {
    /// What runs in the pane.
    pub kind: PaneKind,
    /// The name of the space's repo the pane works in.
    pub repo: String,
    /// The plugin a [`PaneKind::Plugin`] pane shows, by its manifest name. `None` for every other kind. Absent
    /// from the JSON when `None`, so layouts saved before plugins existed load and save unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    /// The file path a [`PaneKind::File`] pane shows (relative to `repo`'s worktree, or an absolute path).
    /// `None` for every other kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

impl PaneRole {
    /// A role of `kind` in `repo` that shows no plugin or file.
    pub fn new(kind: PaneKind, repo: impl Into<String>) -> PaneRole {
        PaneRole {
            kind,
            repo: repo.into(),
            plugin: None,
            file: None,
        }
    }

    /// The role of plugin `plugin`'s pane, working in `repo`.
    pub fn for_plugin(plugin: impl Into<String>, repo: impl Into<String>) -> PaneRole {
        PaneRole {
            kind: PaneKind::Plugin,
            repo: repo.into(),
            plugin: Some(plugin.into()),
            file: None,
        }
    }

    /// The role of a file viewer panel showing `file` in `repo`.
    pub fn for_file(file: impl Into<String>, repo: impl Into<String>) -> PaneRole {
        PaneRole {
            kind: PaneKind::File,
            repo: repo.into(),
            plugin: None,
            file: Some(file.into()),
        }
    }
}

/// A pane and its role.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RolePane {
    /// The pane.
    pub id: PaneId,
    /// What it is for.
    pub role: PaneRole,
}

/// The result of [`reshape`]: the new tree and what the app has to start and stop to match it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reshape {
    /// The new layout, not zoomed.
    pub tree: PaneTree,
    /// Every pane in `tree` with its role, in leaf order.
    pub panes: Vec<RolePane>,
    /// Panes in `tree` that were not in the space before; the app starts their sessions.
    pub added: Vec<RolePane>,
    /// Panes of the space that the shape has no place for; the app closes them. Never an agent or a plugin pane.
    pub removed: Vec<RolePane>,
}

/// The first side's share of the split between the two groups of a shape.
const DEV_AGENT_SHARE: f32 = 0.68;
const DEBUG_AGENT_SHARE: f32 = 0.5;
const REVIEW_GIT_SHARE: f32 = 0.65;
/// The share of the width the per-repo groups keep when plugin panes sit in a column on the right.
pub const PLUGIN_COLUMN_BASE_SHARE: f32 = 0.65;

/// Lay a space out in `shape`.
///
/// `repos` are the space's repo names, in display order (repeats are ignored). `current` is every pane the space
/// has now with its role: empty when launching. Panes of `current` are reused where the shape has a place for
/// their role, so running sessions survive; every agent in `current` is kept, including ones whose repo is not
/// in `repos` (they go after the per-repo agents). A repo with no agent or no auxiliary pane gets a new pane with
/// an id from `next_pane_id`. Focus stays on `focused` if it is still in the layout, else goes to the first agent
/// (or the first plugin pane when there is no agent).
///
/// Every [`PaneKind::Plugin`] pane of `current` is kept too, with its role unchanged, whatever its repo. Plugin
/// panes go after the per-repo groups: in a column on the right of the whole layout (about a third of the width),
/// stacked in the order of `current`, so they sit in the same place in every shape and come last in leaf order.
///
/// Fails with [`LayoutError::DuplicatePane`] if `current` repeats an id or `next_pane_id` returns one already in
/// use, and with [`LayoutError::NoPanes`] if there are no repos, no agents and no plugin panes.
pub fn reshape(
    shape: Shape,
    repos: &[String],
    current: &[RolePane],
    focused: Option<PaneId>,
    next_pane_id: impl FnMut() -> PaneId,
) -> Result<Reshape, LayoutError> {
    let mut pool = PanePool::new(current, next_pane_id)?;
    let mut unique_repos: Vec<&str> = Vec::new();
    for repo in repos {
        if !unique_repos.contains(&repo.as_str()) {
            unique_repos.push(repo);
        }
    }

    let mut agents = Vec::new();
    for repo in &unique_repos {
        let existing = pool.claim_all(PaneKind::Agent, repo);
        if existing.is_empty() {
            agents.push(pool.add(PaneKind::Agent, repo)?);
        } else {
            agents.extend(existing);
        }
    }
    agents.extend(pool.claim_remaining_agents());

    let aux_kind = match shape {
        Shape::Dev | Shape::Debug => PaneKind::Shell,
        Shape::Review => PaneKind::Git,
    };
    let mut aux = Vec::new();
    for repo in &unique_repos {
        match pool.claim(aux_kind, repo) {
            Some(pane) => aux.push(pane),
            None => aux.push(pool.add(aux_kind, repo)?),
        }
    }
    let plugins = pool.claim_plugins();

    let (outer_axis, line_axis, agents_first, first_share) = match shape {
        Shape::Dev => (
            SplitAxis::Vertical,
            SplitAxis::Horizontal,
            true,
            DEV_AGENT_SHARE,
        ),
        Shape::Debug => (
            SplitAxis::Horizontal,
            SplitAxis::Vertical,
            true,
            DEBUG_AGENT_SHARE,
        ),
        Shape::Review => (
            SplitAxis::Vertical,
            SplitAxis::Horizontal,
            false,
            REVIEW_GIT_SHARE,
        ),
    };
    let ids = |panes: &[RolePane]| panes.iter().map(|p| p.id).collect::<Vec<_>>();
    let mut next_split = 0;
    let agent_line = line(&ids(&agents), line_axis, &mut next_split);
    let aux_line = line(&ids(&aux), line_axis, &mut next_split);
    let (first, second) = if agents_first {
        (agent_line, aux_line)
    } else {
        (aux_line, agent_line)
    };
    let base = match (first, second) {
        (Some(first), Some(second)) => {
            let id = SplitId(next_split);
            next_split += 1;
            Some(PaneNode::Split {
                id,
                axis: outer_axis,
                ratio: first_share,
                first: Box::new(first),
                second: Box::new(second),
            })
        }
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    };
    let plugin_column = line(&ids(&plugins), SplitAxis::Vertical, &mut next_split);
    let root = match (base, plugin_column) {
        (Some(base), Some(column)) => PaneNode::Split {
            id: SplitId(next_split),
            axis: SplitAxis::Horizontal,
            ratio: PLUGIN_COLUMN_BASE_SHARE,
            first: Box::new(base),
            second: Box::new(column),
        },
        (Some(only), None) | (None, Some(only)) => only,
        (None, None) => return Err(LayoutError::NoPanes),
    };

    let mut roles: HashMap<PaneId, RolePane> = agents
        .iter()
        .chain(aux.iter())
        .chain(plugins.iter())
        .map(|p| (p.id, p.clone()))
        .collect();
    let focus = focused
        .filter(|f| roles.contains_key(f))
        .or_else(|| agents.first().map(|a| a.id))
        .or_else(|| plugins.first().map(|p| p.id))
        .ok_or(LayoutError::NoPanes)?;
    let tree = PaneTree::from_parts(root, focus, None)?;
    let panes = tree
        .panes()
        .into_iter()
        .filter_map(|id| roles.remove(&id))
        .collect();
    let (added, removed) = pool.finish();
    Ok(Reshape {
        tree,
        panes,
        added,
        removed,
    })
}

/// `ids` in a row (`Horizontal`) or column (`Vertical`) of equal shares, as a balanced tree. Split ids are taken
/// from `next_split`.
fn line(ids: &[PaneId], axis: SplitAxis, next_split: &mut u64) -> Option<PaneNode> {
    match ids {
        [] => None,
        [only] => Some(PaneNode::Leaf { pane: *only }),
        _ => {
            let mid = ids.len() / 2;
            let id = SplitId(*next_split);
            *next_split += 1;
            let first = line(&ids[..mid], axis, next_split)?;
            let second = line(&ids[mid..], axis, next_split)?;
            Some(PaneNode::Split {
                id,
                axis,
                ratio: mid as f32 / ids.len() as f32,
                first: Box::new(first),
                second: Box::new(second),
            })
        }
    }
}

/// The space's current panes, handed out to the new shape, plus the new panes it needs.
struct PanePool<'a, F> {
    unclaimed: Vec<Option<&'a RolePane>>,
    used: HashSet<PaneId>,
    added: Vec<RolePane>,
    next_pane_id: F,
}

impl<'a, F: FnMut() -> PaneId> PanePool<'a, F> {
    fn new(current: &'a [RolePane], next_pane_id: F) -> Result<Self, LayoutError> {
        let mut used = HashSet::new();
        for pane in current {
            if !used.insert(pane.id) {
                return Err(LayoutError::DuplicatePane(pane.id));
            }
        }
        Ok(Self {
            unclaimed: current.iter().map(Some).collect(),
            used,
            added: Vec::new(),
            next_pane_id,
        })
    }

    fn matches(pane: &RolePane, kind: PaneKind, repo: &str) -> bool {
        pane.role.kind == kind && pane.role.repo == repo
    }

    fn claim(&mut self, kind: PaneKind, repo: &str) -> Option<RolePane> {
        let slot = self
            .unclaimed
            .iter_mut()
            .find(|slot| slot.is_some_and(|p| Self::matches(p, kind, repo)))?;
        slot.take().cloned()
    }

    fn claim_all(&mut self, kind: PaneKind, repo: &str) -> Vec<RolePane> {
        self.claim_where(|p| Self::matches(p, kind, repo))
    }

    fn claim_remaining_agents(&mut self) -> Vec<RolePane> {
        self.claim_where(|p| p.role.kind == PaneKind::Agent)
    }

    fn claim_plugins(&mut self) -> Vec<RolePane> {
        self.claim_where(|p| matches!(p.role.kind, PaneKind::Plugin | PaneKind::File))
    }

    fn claim_where(&mut self, wanted: impl Fn(&RolePane) -> bool) -> Vec<RolePane> {
        self.unclaimed
            .iter_mut()
            .filter(|slot| slot.is_some_and(&wanted))
            .filter_map(|slot| slot.take().cloned())
            .collect()
    }

    fn add(&mut self, kind: PaneKind, repo: &str) -> Result<RolePane, LayoutError> {
        let id = (self.next_pane_id)();
        if !self.used.insert(id) {
            return Err(LayoutError::DuplicatePane(id));
        }
        let pane = RolePane {
            id,
            role: PaneRole::new(kind, repo),
        };
        self.added.push(pane.clone());
        Ok(pane)
    }

    /// The panes added, and the current panes nothing claimed.
    fn finish(self) -> (Vec<RolePane>, Vec<RolePane>) {
        let removed = self.unclaimed.into_iter().flatten().cloned().collect();
        (self.added, removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;

    fn repos(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    fn role_pane(id: u64, kind: PaneKind, repo: &str) -> RolePane {
        RolePane {
            id: PaneId(id),
            role: PaneRole::new(kind, repo),
        }
    }

    fn plugin_pane(id: u64, plugin: &str, repo: &str) -> RolePane {
        RolePane {
            id: PaneId(id),
            role: PaneRole::for_plugin(plugin, repo),
        }
    }

    /// Hands out ids from `start` upwards.
    fn ids_from(start: u64) -> impl FnMut() -> PaneId {
        let mut next = start;
        move || {
            next += 1;
            PaneId(next - 1)
        }
    }

    fn of_kind(panes: &[RolePane], kind: PaneKind) -> Vec<PaneId> {
        panes
            .iter()
            .filter(|p| p.role.kind == kind)
            .map(|p| p.id)
            .collect()
    }

    fn rect(reshape: &Reshape, id: PaneId) -> Rect {
        reshape
            .tree
            .layout(Rect::new(0.0, 0.0, 100.0, 100.0), 0.0)
            .pane(id)
            .unwrap_or_default()
    }

    #[test]
    fn launch_dev_puts_agents_over_shells() {
        let shaped = reshape(Shape::Dev, &repos(&["api", "web"]), &[], None, ids_from(1)).unwrap();
        assert_eq!(
            shaped.panes,
            vec![
                role_pane(1, PaneKind::Agent, "api"),
                role_pane(2, PaneKind::Agent, "web"),
                role_pane(3, PaneKind::Shell, "api"),
                role_pane(4, PaneKind::Shell, "web"),
            ]
        );
        assert_eq!(shaped.added.len(), 4);
        assert!(shaped.removed.is_empty());
        assert_eq!(shaped.tree.focused(), PaneId(1));
        assert!(!shaped.tree.is_zoomed());

        let api_agent = rect(&shaped, PaneId(1));
        let web_agent = rect(&shaped, PaneId(2));
        let api_shell = rect(&shaped, PaneId(3));
        assert!((api_agent.height - 68.0).abs() < 1e-3);
        assert!((api_agent.width - 50.0).abs() < 1e-3);
        assert!(web_agent.x > api_agent.x);
        assert!((api_shell.y - api_agent.bottom()).abs() < 1e-3);
    }

    #[test]
    fn launch_debug_puts_agents_beside_shells() {
        let shaped = reshape(
            Shape::Debug,
            &repos(&["api", "web"]),
            &[],
            None,
            ids_from(1),
        )
        .unwrap();
        let api_agent = rect(&shaped, PaneId(1));
        let web_agent = rect(&shaped, PaneId(2));
        let api_shell = rect(&shaped, PaneId(3));
        assert!((api_agent.width - 50.0).abs() < 1e-3);
        assert!((web_agent.y - api_agent.bottom()).abs() < 1e-3);
        assert!((api_shell.x - api_agent.right()).abs() < 1e-3);
    }

    #[test]
    fn dev_to_debug_reuses_every_pane() {
        let dev = reshape(Shape::Dev, &repos(&["api", "web"]), &[], None, ids_from(1)).unwrap();
        let debug = reshape(
            Shape::Debug,
            &repos(&["api", "web"]),
            &dev.panes,
            Some(PaneId(4)),
            ids_from(100),
        )
        .unwrap();
        assert!(debug.added.is_empty());
        assert!(debug.removed.is_empty());
        assert_eq!(debug.tree.focused(), PaneId(4));
        let mut before = dev.tree.panes();
        let mut after = debug.tree.panes();
        before.sort();
        after.sort();
        assert_eq!(before, after);
    }

    #[test]
    fn review_keeps_agents_and_swaps_shells_for_git() {
        let dev = reshape(Shape::Dev, &repos(&["api", "web"]), &[], None, ids_from(1)).unwrap();
        let review = reshape(
            Shape::Review,
            &repos(&["api", "web"]),
            &dev.panes,
            Some(PaneId(2)),
            ids_from(10),
        )
        .unwrap();
        assert_eq!(
            of_kind(&review.panes, PaneKind::Agent),
            vec![PaneId(1), PaneId(2)]
        );
        assert_eq!(
            review.added,
            vec![
                role_pane(10, PaneKind::Git, "api"),
                role_pane(11, PaneKind::Git, "web")
            ]
        );
        assert_eq!(
            review.removed,
            vec![
                role_pane(3, PaneKind::Shell, "api"),
                role_pane(4, PaneKind::Shell, "web")
            ]
        );
        assert_eq!(review.tree.focused(), PaneId(2));
        // Git panes on top, agents below.
        let git = rect(&review, PaneId(10));
        let agent = rect(&review, PaneId(1));
        assert!((git.height - 65.0).abs() < 1e-3);
        assert!((agent.y - git.bottom()).abs() < 1e-3);

        // Back to dev: the same agents, new shells, git panes go.
        let dev_again = reshape(
            Shape::Dev,
            &repos(&["api", "web"]),
            &review.panes,
            Some(PaneId(10)),
            ids_from(20),
        )
        .unwrap();
        assert_eq!(
            of_kind(&dev_again.panes, PaneKind::Agent),
            vec![PaneId(1), PaneId(2)]
        );
        assert_eq!(
            of_kind(&dev_again.added, PaneKind::Shell),
            vec![PaneId(20), PaneId(21)]
        );
        assert_eq!(
            of_kind(&dev_again.removed, PaneKind::Git),
            vec![PaneId(10), PaneId(11)]
        );
        assert_eq!(dev_again.tree.focused(), PaneId(1));
    }

    #[test]
    fn extra_agents_are_kept_and_extra_auxiliary_panes_removed() {
        let current = vec![
            role_pane(1, PaneKind::Agent, "api"),
            role_pane(2, PaneKind::Shell, "api"),
            role_pane(3, PaneKind::Agent, "api"),
            role_pane(4, PaneKind::Shell, "api"),
            role_pane(5, PaneKind::Agent, "dropped-repo"),
            role_pane(6, PaneKind::Git, "api"),
        ];
        let shaped = reshape(Shape::Debug, &repos(&["api"]), &current, None, ids_from(10)).unwrap();
        assert_eq!(
            of_kind(&shaped.panes, PaneKind::Agent),
            vec![PaneId(1), PaneId(3), PaneId(5)]
        );
        assert_eq!(of_kind(&shaped.panes, PaneKind::Shell), vec![PaneId(2)]);
        assert!(shaped.added.is_empty());
        assert_eq!(
            shaped.removed,
            vec![
                role_pane(4, PaneKind::Shell, "api"),
                role_pane(6, PaneKind::Git, "api")
            ]
        );
    }

    #[test]
    fn no_shape_ever_removes_an_agent() {
        let current = vec![
            role_pane(1, PaneKind::Agent, "api"),
            role_pane(2, PaneKind::Agent, "web"),
            role_pane(3, PaneKind::Agent, "gone"),
            role_pane(4, PaneKind::Shell, "api"),
        ];
        for shape in [Shape::Dev, Shape::Debug, Shape::Review] {
            let shaped =
                reshape(shape, &repos(&["api", "web"]), &current, None, ids_from(10)).unwrap();
            for agent in [1, 2, 3] {
                assert!(
                    shaped.tree.contains(PaneId(agent)),
                    "{shape:?} lost agent {agent}"
                );
            }
            assert!(
                shaped
                    .removed
                    .iter()
                    .all(|p| p.role.kind != PaneKind::Agent)
            );
        }
    }

    #[test]
    fn missing_agent_is_added_back() {
        let current = vec![role_pane(1, PaneKind::Shell, "api")];
        let shaped = reshape(
            Shape::Dev,
            &repos(&["api"]),
            &current,
            Some(PaneId(1)),
            ids_from(5),
        )
        .unwrap();
        assert_eq!(shaped.added, vec![role_pane(5, PaneKind::Agent, "api")]);
        assert_eq!(shaped.tree.focused(), PaneId(1));
    }

    #[test]
    fn repeated_repos_are_ignored() {
        let shaped = reshape(Shape::Dev, &repos(&["api", "api"]), &[], None, ids_from(1)).unwrap();
        assert_eq!(shaped.panes.len(), 2);
    }

    #[test]
    fn no_repos_keeps_only_agents() {
        let current = vec![
            role_pane(1, PaneKind::Agent, "api"),
            role_pane(2, PaneKind::Shell, "api"),
        ];
        let shaped = reshape(Shape::Dev, &[], &current, None, ids_from(10)).unwrap();
        assert_eq!(shaped.tree.panes(), vec![PaneId(1)]);
        assert_eq!(shaped.removed, vec![role_pane(2, PaneKind::Shell, "api")]);
        assert_eq!(
            reshape(Shape::Dev, &[], &current[1..], None, ids_from(10)),
            Err(LayoutError::NoPanes)
        );
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let current = vec![
            role_pane(1, PaneKind::Agent, "api"),
            role_pane(1, PaneKind::Shell, "api"),
        ];
        assert_eq!(
            reshape(Shape::Dev, &repos(&["api"]), &current, None, ids_from(10)),
            Err(LayoutError::DuplicatePane(PaneId(1)))
        );
        let current = vec![role_pane(1, PaneKind::Agent, "api")];
        assert_eq!(
            reshape(Shape::Dev, &repos(&["api"]), &current, None, || PaneId(1)),
            Err(LayoutError::DuplicatePane(PaneId(1)))
        );
    }

    #[test]
    fn many_repos_share_a_row_equally() {
        let names: Vec<String> = (0..13).map(|i| format!("repo{i}")).collect();
        let shaped = reshape(Shape::Dev, &names, &[], None, ids_from(1)).unwrap();
        let layout = shaped.tree.layout(Rect::new(0.0, 0.0, 130.0, 100.0), 0.0);
        let agents = of_kind(&shaped.panes, PaneKind::Agent);
        assert_eq!(agents.len(), 13);
        for agent in agents {
            let width = layout.pane(agent).map(|r| r.width).unwrap_or_default();
            assert!((width - 10.0).abs() < 1e-3, "{agent:?} is {width} wide");
        }
    }

    #[test]
    fn every_shape_keeps_plugin_panes_and_their_names() {
        let dev = reshape(Shape::Dev, &repos(&["api", "web"]), &[], None, ids_from(1)).unwrap();
        let mut current = dev.panes.clone();
        current.push(plugin_pane(5, "http", "api"));
        current.push(plugin_pane(6, "logs", "gone-repo"));
        let mut panes = current;
        let mut next = 100;
        for shape in [Shape::Debug, Shape::Review, Shape::Dev, Shape::Review] {
            let shaped = reshape(
                shape,
                &repos(&["api", "web"]),
                &panes,
                Some(PaneId(6)),
                ids_from(next),
            )
            .unwrap();
            next += 10;
            let plugins: Vec<&RolePane> = shaped
                .panes
                .iter()
                .filter(|p| p.role.kind == PaneKind::Plugin)
                .collect();
            assert_eq!(
                plugins,
                vec![
                    &plugin_pane(5, "http", "api"),
                    &plugin_pane(6, "logs", "gone-repo")
                ],
                "{shape:?}"
            );
            assert!(
                shaped
                    .removed
                    .iter()
                    .all(|p| p.role.kind != PaneKind::Plugin),
                "{shape:?}"
            );
            assert!(shaped.added.iter().all(|p| p.role.plugin.is_none()));
            assert_eq!(shaped.tree.focused(), PaneId(6));
            // Plugin panes come last in leaf order, in a column on the right.
            let leaves = shaped.tree.panes();
            assert_eq!(leaves[leaves.len() - 2..], [PaneId(5), PaneId(6)]);
            let http = rect(&shaped, PaneId(5));
            let logs = rect(&shaped, PaneId(6));
            assert!((http.x - 65.0).abs() < 1e-3, "{shape:?}: {http:?}");
            assert!((http.width - 35.0).abs() < 1e-3);
            assert!((logs.y - http.bottom()).abs() < 1e-3);
            // No new pane reuses a plugin pane's id, and every id is distinct.
            let mut ids = leaves.clone();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), leaves.len());
            panes = shaped.panes;
        }
    }

    #[test]
    fn plugin_ids_are_not_reused_for_new_panes() {
        let current = vec![
            role_pane(1, PaneKind::Agent, "api"),
            plugin_pane(2, "http", "api"),
        ];
        // The id allocator hands out the plugin's id: reshape refuses instead of reusing it.
        assert_eq!(
            reshape(Shape::Dev, &repos(&["api"]), &current, None, ids_from(2)),
            Err(LayoutError::DuplicatePane(PaneId(2)))
        );
        let shaped = reshape(Shape::Dev, &repos(&["api"]), &current, None, ids_from(3)).unwrap();
        assert_eq!(shaped.added, vec![role_pane(3, PaneKind::Shell, "api")]);
        assert!(shaped.tree.contains(PaneId(2)));
    }

    #[test]
    fn a_space_of_only_plugin_panes_still_lays_out() {
        let current = vec![plugin_pane(4, "logs", "api"), plugin_pane(7, "sql", "api")];
        let shaped = reshape(Shape::Debug, &[], &current, None, ids_from(10)).unwrap();
        assert_eq!(shaped.tree.panes(), vec![PaneId(4), PaneId(7)]);
        assert_eq!(shaped.tree.focused(), PaneId(4));
        assert!(shaped.added.is_empty() && shaped.removed.is_empty());
    }

    #[test]
    fn roles_saved_before_plugins_load_and_save_unchanged() {
        let old = serde_json::json!({ "id": 3, "role": { "kind": "shell", "repo": "api" } });
        let pane: RolePane = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(pane, role_pane(3, PaneKind::Shell, "api"));
        assert_eq!(serde_json::to_value(&pane).unwrap(), old);
        let plugin = plugin_pane(9, "http", "web");
        let value = serde_json::to_value(&plugin).unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "id": 9, "role": { "kind": "plugin", "repo": "web", "plugin": "http" } })
        );
        assert_eq!(serde_json::from_value::<RolePane>(value).unwrap(), plugin);
    }

    #[test]
    fn reshape_result_round_trips_through_json() {
        let shaped = reshape(
            Shape::Review,
            &repos(&["api", "web"]),
            &[],
            None,
            ids_from(1),
        )
        .unwrap();
        let json = serde_json::to_string(&shaped).unwrap();
        let back: Reshape = serde_json::from_str(&json).unwrap();
        assert_eq!(back, shaped);
        let shape: Shape = serde_json::from_str("\"debug\"").unwrap();
        assert_eq!(shape, Shape::Debug);
        assert_eq!(
            serde_json::to_value(role_pane(3, PaneKind::Git, "api")).unwrap(),
            serde_json::json!({ "id": 3, "role": { "kind": "git", "repo": "api" } })
        );
    }
}
