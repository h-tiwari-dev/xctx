//! A space's panes without their terminals: the [`PaneTree`], what each pane is for, and the next free pane id.
//! The app keeps the terminals keyed by [`PaneId`] next to it and starts or stops them as these operations report.

use std::collections::BTreeMap;

use xctx_core::{Space, SpaceShape};
use xctx_layout::{
    FocusDirection, LayoutError, PaneId, PaneKind, PaneRole, PaneTree, Reshape, RolePane, Shape,
    SplitAxis, SplitId, reshape,
};

/// The share the existing pane keeps when it is split.
pub const SPLIT_RATIO: f32 = 0.5;

/// How far one resize keystroke moves a divider, as a fraction of the split.
pub const RESIZE_STEP: f32 = 0.05;

/// The layout shape for a space shape.
pub fn layout_shape(shape: SpaceShape) -> Shape {
    match shape {
        SpaceShape::Dev => Shape::Dev,
        SpaceShape::Debug => Shape::Debug,
        SpaceShape::Review => Shape::Review,
    }
}

/// The space shape for a layout shape.
pub fn space_shape(shape: Shape) -> SpaceShape {
    match shape {
        Shape::Dev => SpaceShape::Dev,
        Shape::Debug => SpaceShape::Debug,
        Shape::Review => SpaceShape::Review,
    }
}

/// The space's repo names in display order, as [`reshape`] takes them.
pub fn repo_names(space: &Space) -> Vec<String> {
    space.repos.iter().map(|r| r.name.clone()).collect()
}

/// A saved layout that cannot be used as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidLayout(pub String);

impl std::fmt::Display for InvalidLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InvalidLayout {}

/// A space's pane tree with a role for every pane. Every pane in the tree has a role, and pane ids are never
/// reused: new panes take ids from `next_pane_id` upwards.
#[derive(Clone, Debug, PartialEq)]
pub struct SpaceLayout {
    tree: PaneTree,
    roles: BTreeMap<PaneId, PaneRole>,
    next_pane_id: u64,
}

impl SpaceLayout {
    /// The first layout of a space being launched in `shape`: an agent and an auxiliary pane per repo. Returns
    /// the layout and every pane in it, all of which need starting.
    pub fn launch(
        shape: Shape,
        repos: &[String],
    ) -> Result<(SpaceLayout, Vec<RolePane>), LayoutError> {
        let mut next = 1;
        let result = reshape(shape, repos, &[], None, || {
            let id = PaneId(next);
            next += 1;
            id
        })?;
        let layout = SpaceLayout {
            roles: result
                .panes
                .iter()
                .map(|p| (p.id, p.role.clone()))
                .collect(),
            tree: result.tree,
            next_pane_id: next,
        };
        Ok((layout, result.added))
    }

    /// A layout from saved parts, after checking that every pane of `tree` has a role. Roles of panes not in the
    /// tree are dropped, and `next_pane_id` is raised above every pane id in use.
    pub fn from_parts(
        tree: PaneTree,
        roles: Vec<RolePane>,
        next_pane_id: u64,
    ) -> Result<SpaceLayout, InvalidLayout> {
        let mut by_id: BTreeMap<PaneId, PaneRole> = BTreeMap::new();
        for pane in roles {
            if tree.contains(pane.id) {
                by_id.insert(pane.id, pane.role);
            }
        }
        let panes = tree.panes();
        if let Some(missing) = panes.iter().find(|p| !by_id.contains_key(p)) {
            return Err(InvalidLayout(format!("pane {} has no role", missing.0)));
        }
        let highest = panes.iter().map(|p| p.0).max().unwrap_or(0);
        Ok(SpaceLayout {
            tree,
            roles: by_id,
            next_pane_id: next_pane_id.max(highest.saturating_add(1)),
        })
    }

    /// The pane tree.
    pub fn tree(&self) -> &PaneTree {
        &self.tree
    }

    /// The id the next new pane gets.
    pub fn next_pane_id(&self) -> u64 {
        self.next_pane_id
    }

    /// What `pane` is for.
    pub fn role(&self, pane: PaneId) -> Option<&PaneRole> {
        self.roles.get(&pane)
    }

    /// Every pane with its role, in leaf order.
    pub fn panes(&self) -> Vec<RolePane> {
        self.tree
            .panes()
            .into_iter()
            .filter_map(|id| {
                self.roles.get(&id).map(|role| RolePane {
                    id,
                    role: role.clone(),
                })
            })
            .collect()
    }

    /// The agent panes, in leaf order.
    pub fn agent_panes(&self) -> Vec<PaneId> {
        self.panes()
            .into_iter()
            .filter(|p| p.role.kind == PaneKind::Agent)
            .map(|p| p.id)
            .collect()
    }

    /// The focused pane.
    pub fn focused(&self) -> PaneId {
        self.tree.focused()
    }

    fn take_pane_id(&mut self) -> PaneId {
        let id = PaneId(self.next_pane_id);
        self.next_pane_id += 1;
        id
    }

    /// Lay the space out in `shape`. Every agent pane is kept; the result's `added` panes need starting and its
    /// `removed` panes stopping. Focus stays on the focused pane when the shape keeps it.
    pub fn reshape(&mut self, shape: Shape, repos: &[String]) -> Result<Reshape, LayoutError> {
        let current = self.panes();
        let focused = Some(self.tree.focused());
        let mut next = self.next_pane_id;
        let result = reshape(shape, repos, &current, focused, || {
            let id = PaneId(next);
            next += 1;
            id
        })?;
        self.next_pane_id = next;
        self.tree = result.tree.clone();
        self.roles = result
            .panes
            .iter()
            .map(|p| (p.id, p.role.clone()))
            .collect();
        Ok(result)
    }

    /// Split the focused pane along `axis` with a new shell in the same repo, which takes the focus. Returns
    /// the new pane, which needs starting.
    #[cfg(test)]
    pub fn split_focused(&mut self, axis: SplitAxis) -> Result<RolePane, LayoutError> {
        self.split(self.tree.focused(), axis, PaneKind::Shell, None)
    }

    /// Split `target` along `axis` with a new pane of `kind` in `repo` (the target's repo when `None`), which
    /// takes the focus. Returns the new pane, which needs starting.
    pub fn split(
        &mut self,
        target: PaneId,
        axis: SplitAxis,
        kind: PaneKind,
        repo: Option<String>,
    ) -> Result<RolePane, LayoutError> {
        let target_repo = self
            .role(target)
            .map(|r| r.repo.clone())
            .ok_or(LayoutError::UnknownPane(target))?;
        let id = PaneId(self.next_pane_id);
        self.tree.split(target, axis, id, SPLIT_RATIO)?;
        self.take_pane_id();
        let role = PaneRole::new(kind, repo.unwrap_or(target_repo));
        self.roles.insert(id, role.clone());
        Ok(RolePane { id, role })
    }

    /// Close `pane`. Returns the focused pane afterwards. Fails with [`LayoutError::LastPane`] for the only pane.
    pub fn close(&mut self, pane: PaneId) -> Result<PaneId, LayoutError> {
        let focused = self.tree.close(pane)?;
        self.roles.remove(&pane);
        Ok(focused)
    }

    /// Focus `pane`.
    pub fn focus(&mut self, pane: PaneId) -> Result<(), LayoutError> {
        self.tree.focus(pane)
    }

    /// Move focus to the neighbouring pane in `direction`; `None` when there is none.
    pub fn move_focus(&mut self, direction: FocusDirection) -> Option<PaneId> {
        self.tree.move_focus(direction)
    }

    /// Focus the next pane in leaf order, wrapping.
    pub fn focus_next(&mut self) -> PaneId {
        self.tree.focus_next()
    }

    /// Focus the previous pane in leaf order, wrapping.
    pub fn focus_prev(&mut self) -> PaneId {
        self.tree.focus_prev()
    }

    /// Grow the focused pane by `delta` of its split along `axis` (negative shrinks it).
    pub fn resize_focused(&mut self, axis: SplitAxis, delta: f32) -> Result<SplitId, LayoutError> {
        self.tree.resize(self.tree.focused(), axis, delta)
    }

    /// Set a split's ratio, as dragging its divider does.
    pub fn set_split_ratio(&mut self, split: SplitId, ratio: f32) -> Result<(), LayoutError> {
        self.tree.set_split_ratio(split, ratio)
    }

    /// Zoom the focused pane, or end the zoom. Returns whether a pane is zoomed afterwards.
    pub fn toggle_zoom(&mut self) -> bool {
        self.tree.toggle_zoom()
    }

    /// Zoom `pane` (focusing it), or with `None` end the zoom. Returns the zoomed pane afterwards.
    pub fn set_zoom(&mut self, pane: Option<PaneId>) -> Result<Option<PaneId>, LayoutError> {
        self.tree.set_zoom(pane)?;
        Ok(self.tree.zoomed())
    }

    /// Zoom `pane` (focusing it), or end the zoom if `pane` is already zoomed. Returns whether a pane is zoomed
    /// afterwards.
    pub fn toggle_zoom_of(&mut self, pane: PaneId) -> Result<bool, LayoutError> {
        if self.tree.zoomed() == Some(pane) {
            self.tree.set_zoom(None)?;
            Ok(false)
        } else {
            self.tree.set_zoom(Some(pane))?;
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repos() -> Vec<String> {
        vec!["api".to_owned(), "web".to_owned()]
    }

    fn launched() -> Result<SpaceLayout, LayoutError> {
        SpaceLayout::launch(Shape::Dev, &repos()).map(|(layout, _)| layout)
    }

    #[test]
    fn launch_gives_each_repo_an_agent_and_a_shell() -> Result<(), LayoutError> {
        let (layout, added) = SpaceLayout::launch(Shape::Dev, &repos())?;
        assert_eq!(added.len(), 4);
        assert_eq!(layout.panes(), added);
        assert_eq!(layout.agent_panes().len(), 2);
        assert_eq!(layout.next_pane_id(), 5);
        let focused = layout.focused();
        assert_eq!(layout.role(focused).map(|r| r.kind), Some(PaneKind::Agent));
        Ok(())
    }

    #[test]
    fn reshape_keeps_agents_and_reports_what_changed() -> Result<(), LayoutError> {
        let mut layout = launched()?;
        let agents = layout.agent_panes();
        let result = layout.reshape(Shape::Review, &repos())?;
        assert_eq!(layout.agent_panes(), agents);
        assert_eq!(result.added.len(), 2);
        assert!(result.added.iter().all(|p| p.role.kind == PaneKind::Git));
        assert_eq!(result.removed.len(), 2);
        assert!(
            result
                .removed
                .iter()
                .all(|p| p.role.kind == PaneKind::Shell)
        );
        assert!(result.added.iter().all(|p| p.id.0 >= 5));
        assert_eq!(layout.next_pane_id(), 7);
        for pane in &result.removed {
            assert_eq!(layout.role(pane.id), None);
        }
        let back = layout.reshape(Shape::Dev, &repos())?;
        assert_eq!(layout.agent_panes(), agents);
        assert!(back.added.iter().all(|p| p.role.kind == PaneKind::Shell));
        Ok(())
    }

    #[test]
    fn split_adds_a_shell_in_the_focused_repo_and_close_removes_it() -> Result<(), LayoutError> {
        let mut layout = launched()?;
        let focused = layout.focused();
        let repo = layout.role(focused).map(|r| r.repo.clone());
        let pane = layout.split_focused(SplitAxis::Horizontal)?;
        assert_eq!(pane.role.kind, PaneKind::Shell);
        assert_eq!(Some(pane.role.repo.clone()), repo);
        assert_eq!(layout.focused(), pane.id);
        assert_eq!(layout.next_pane_id(), pane.id.0 + 1);
        assert_eq!(layout.close(pane.id)?, focused);
        assert_eq!(layout.role(pane.id), None);
        assert_eq!(layout.tree().panes().len(), 4);
        Ok(())
    }

    #[test]
    fn the_last_pane_cannot_be_closed() -> Result<(), LayoutError> {
        let (mut layout, _) = SpaceLayout::launch(Shape::Dev, &["api".to_owned()])?;
        let shell = layout
            .panes()
            .into_iter()
            .find(|p| p.role.kind == PaneKind::Shell)
            .map(|p| p.id)
            .ok_or(LayoutError::NoPanes)?;
        layout.close(shell)?;
        let agent = layout.focused();
        assert_eq!(layout.close(agent), Err(LayoutError::LastPane(agent)));
        assert!(layout.role(agent).is_some());
        Ok(())
    }

    #[test]
    fn zoom_toggles_on_the_given_pane() -> Result<(), LayoutError> {
        let mut layout = launched()?;
        let pane = layout.panes()[3].id;
        assert!(layout.toggle_zoom_of(pane)?);
        assert_eq!(layout.focused(), pane);
        assert!(!layout.toggle_zoom_of(pane)?);
        assert!(layout.toggle_zoom());
        assert!(!layout.toggle_zoom());
        Ok(())
    }

    #[test]
    fn parts_need_a_role_for_every_pane() -> Result<(), LayoutError> {
        let layout = launched()?;
        let mut roles = layout.panes();
        roles.push(RolePane {
            id: PaneId(99),
            role: PaneRole::new(PaneKind::Shell, "api"),
        });
        let rebuilt = SpaceLayout::from_parts(layout.tree().clone(), roles.clone(), 1);
        assert_eq!(
            rebuilt.as_ref().map(SpaceLayout::next_pane_id),
            Ok(5),
            "next id is raised and the stray role dropped"
        );
        assert_eq!(rebuilt.map(|l| l.panes()), Ok(layout.panes()));
        roles.remove(0);
        assert!(SpaceLayout::from_parts(layout.tree().clone(), roles, 5).is_err());
        Ok(())
    }

    #[test]
    fn shapes_convert_both_ways() {
        for shape in [SpaceShape::Dev, SpaceShape::Debug, SpaceShape::Review] {
            assert_eq!(space_shape(layout_shape(shape)), shape);
        }
    }
}
