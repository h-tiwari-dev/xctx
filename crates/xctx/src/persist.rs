//! The layout document saved per space with [`Store::save_layout`], so a reopened app gets its panes back:
//! the pane tree, each pane's role and the next pane id.

use serde::{Deserialize, Serialize};
use xctx_core::{Space, SpaceId, Store};
use xctx_layout::{LayoutError, PaneTree, RolePane};

use crate::space_layout::{SpaceLayout, layout_shape, repo_names};

/// The version written into new layout documents.
pub const LAYOUT_VERSION: u32 = 1;

/// What is saved for a space: enough to rebuild its [`SpaceLayout`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PersistedSpaceLayout {
    /// The document's format version.
    #[serde(default = "default_version")]
    pub version: u32,
    /// The pane tree, including focus and zoom.
    pub tree: PaneTree,
    /// What each pane is for.
    pub roles: Vec<RolePane>,
    /// The id the next new pane gets.
    pub next_pane_id: u64,
}

fn default_version() -> u32 {
    LAYOUT_VERSION
}

impl PersistedSpaceLayout {
    /// The document for `layout`.
    pub fn from_layout(layout: &SpaceLayout) -> PersistedSpaceLayout {
        PersistedSpaceLayout {
            version: LAYOUT_VERSION,
            tree: layout.tree().clone(),
            roles: layout.panes(),
            next_pane_id: layout.next_pane_id(),
        }
    }

    /// The document as JSON.
    pub fn to_json(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }

    /// Read a document from JSON. Fails when it is not a layout document or its tree breaks the tree's rules.
    pub fn from_json(value: serde_json::Value) -> Result<PersistedSpaceLayout, serde_json::Error> {
        serde_json::from_value(value)
    }
}

/// Save `layout` as the space's layout document.
pub fn save_layout(store: &Store, id: &SpaceId, layout: &SpaceLayout) -> Result<(), String> {
    let json = PersistedSpaceLayout::from_layout(layout)
        .to_json()
        .map_err(|e| format!("could not encode the layout of {id}: {e}"))?;
    store
        .save_layout(id, &json)
        .map_err(|e| format!("could not save the layout of {id}: {e}"))
}

/// A space's layout rebuilt for reopening it.
#[derive(Clone, Debug, PartialEq)]
pub struct RestoredLayout {
    /// The layout.
    pub layout: SpaceLayout,
    /// Why the saved layout was not used and a fresh one was made instead; `None` when it was used.
    pub fallback: Option<String>,
}

/// The layout to reopen `space` with: what `saved` holds (the result of [`Store::load_layout`]) when it is a
/// usable document, else a fresh layout in the space's shape with the reason in [`RestoredLayout::fallback`].
/// Fails only when even a fresh layout cannot be made, which needs a space with no repos.
pub fn restore_layout(
    saved: xctx_core::Result<Option<serde_json::Value>>,
    space: &Space,
) -> Result<RestoredLayout, LayoutError> {
    let reason = match saved {
        Ok(Some(value)) => match PersistedSpaceLayout::from_json(value) {
            Ok(doc) => match SpaceLayout::from_parts(doc.tree, doc.roles, doc.next_pane_id) {
                Ok(layout) => {
                    return Ok(RestoredLayout {
                        layout,
                        fallback: None,
                    });
                }
                Err(e) => format!("the saved layout of {} is unusable ({e})", space.id),
            },
            Err(e) => format!("the saved layout of {} is unreadable ({e})", space.id),
        },
        Ok(None) => format!("{} had no saved layout", space.id),
        Err(e) => format!("could not read the layout of {} ({e})", space.id),
    };
    let (layout, _) = SpaceLayout::launch(layout_shape(space.shape), &repo_names(space))?;
    Ok(RestoredLayout {
        layout,
        fallback: Some(reason),
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;
    use xctx_core::{SpaceRepo, SpaceShape, SpaceStatus};
    use xctx_layout::{PaneKind, Shape, SplitAxis};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn space(shape: SpaceShape) -> Space {
        Space {
            id: SpaceId::new("fix-retry"),
            title: "fix retry".into(),
            task: "fix retry".into(),
            shape,
            status: SpaceStatus::Active,
            port: 4001,
            created_at: 0,
            repos: vec![SpaceRepo {
                name: "api".into(),
                repo: PathBuf::from("/code/api"),
                worktree: PathBuf::from("/code/api-worktrees/fix-retry"),
                branch: "space/fix-retry".into(),
                base_branch: "main".into(),
                base_commit: "abc".into(),
                branch_created: true,
                reason: String::new(),
            }],
            production: false,
        }
    }

    fn edited_layout() -> Result<SpaceLayout, Box<dyn std::error::Error>> {
        let (mut layout, _) = SpaceLayout::launch(Shape::Dev, &["api".to_owned()])?;
        layout.split_focused(SplitAxis::Vertical)?;
        layout.toggle_zoom();
        Ok(layout)
    }

    #[test]
    fn layouts_round_trip_through_json() -> TestResult {
        let layout = edited_layout()?;
        let doc = PersistedSpaceLayout::from_layout(&layout);
        let json = doc.to_json()?;
        let text = serde_json::to_string(&json)?;
        let back = PersistedSpaceLayout::from_json(serde_json::from_str(&text)?)?;
        assert_eq!(back, doc);
        let restored = restore_layout(Ok(Some(json)), &space(SpaceShape::Dev))?;
        assert_eq!(restored.fallback, None);
        assert_eq!(restored.layout, layout);
        assert!(restored.layout.tree().is_zoomed());
        Ok(())
    }

    #[test]
    fn documents_without_a_version_read_as_the_current_one() -> TestResult {
        let layout = edited_layout()?;
        let mut json = PersistedSpaceLayout::from_layout(&layout).to_json()?;
        if let Some(object) = json.as_object_mut() {
            object.remove("version");
        }
        assert_eq!(
            PersistedSpaceLayout::from_json(json)?.version,
            LAYOUT_VERSION
        );
        Ok(())
    }

    #[test]
    fn missing_or_corrupt_layouts_fall_back_to_a_fresh_one_in_the_space_shape() -> TestResult {
        let review = space(SpaceShape::Review);
        let corrupt = [
            Ok(None),
            Ok(Some(json!({"not": "a layout"}))),
            Ok(Some(json!({
                "tree": {"root": {"type": "leaf", "pane": 1}, "focused": 2},
                "roles": [],
                "next_pane_id": 2
            }))),
            Ok(Some(json!({
                "tree": {"root": {"type": "leaf", "pane": 1}, "focused": 1},
                "roles": [],
                "next_pane_id": 2
            }))),
            Err(xctx_core::Error::NoDataDir),
        ];
        for saved in corrupt {
            let restored = restore_layout(saved, &review)?;
            assert!(restored.fallback.is_some());
            let kinds: Vec<_> = restored
                .layout
                .panes()
                .iter()
                .map(|p| p.role.kind)
                .collect();
            assert_eq!(kinds, [PaneKind::Git, PaneKind::Agent]);
        }
        Ok(())
    }

    #[test]
    fn layouts_are_saved_to_the_store() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path())?;
        let layout = edited_layout()?;
        let id = SpaceId::new("fix-retry");
        save_layout(&store, &id, &layout)?;
        let restored = restore_layout(store.load_layout(&id), &space(SpaceShape::Dev))?;
        assert_eq!(restored.layout, layout);
        Ok(())
    }
}
