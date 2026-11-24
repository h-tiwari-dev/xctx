//! The steps of a space's life that touch disk or git, without any window: indexing folders, adding repos to a
//! draft, launching, reshaping, and shipping or discarding. The app runs the slow ones off the UI thread.

use std::path::{Path, PathBuf};

use xctx_core::{
    DiscardReport, DraftRepo, RepoIndex, ShipOptions, ShipReport, Space, SpaceDraft, SpaceId,
    Store, git,
};
use xctx_layout::{Reshape, RolePane, Shape};

use crate::persist::save_layout;
use crate::space_layout::{SpaceLayout, layout_shape, repo_names, space_shape};

/// The reason shown for a repo the user added to a draft by hand.
pub const ADDED_BY_HAND: &str = "added by you";

/// `input` with a leading `~` replaced by `home`, and surrounding whitespace removed.
pub fn expand_home(input: &str, home: Option<&Path>) -> PathBuf {
    let input = input.trim();
    match (input.strip_prefix('~'), home) {
        (Some(""), Some(home)) => home.to_path_buf(),
        (Some(rest), Some(home)) if rest.starts_with('/') => {
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(input),
    }
}

/// The user's home directory from `HOME`.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// The folder the user typed to index, as an absolute path to an existing directory.
pub fn folder_to_index(input: &str, home: Option<&Path>) -> Result<PathBuf, String> {
    if input.trim().is_empty() {
        return Err("type a folder to index, such as ~/code".to_owned());
    }
    let path = expand_home(input, home);
    if !path.is_absolute() {
        return Err(format!(
            "{} is not an absolute path; start it with / or ~",
            path.display()
        ));
    }
    if !path.is_dir() {
        return Err(format!("{} is not a folder", path.display()));
    }
    Ok(path)
}

/// The repo at the path the user typed, to add to a draft.
pub fn repo_for_draft(input: &str, home: Option<&Path>) -> Result<DraftRepo, String> {
    if input.trim().is_empty() {
        return Err("type the path of a git repository".to_owned());
    }
    let path = expand_home(input, home);
    let root = git::repo_root(&path).map_err(|e| e.to_string())?;
    Ok(DraftRepo::new(root, ADDED_BY_HAND))
}

/// An index of `roots`, scanned. Slow for large folders: run it off the UI thread.
pub fn scan_index(roots: Vec<PathBuf>) -> RepoIndex {
    let mut index = RepoIndex::new(roots);
    index.scan();
    index
}

/// The indexed repos that are not in `draft`, to offer for adding.
pub fn addable_repos<'a>(
    index: &'a RepoIndex,
    draft: &SpaceDraft,
) -> Vec<&'a xctx_core::IndexedRepo> {
    index
        .repos()
        .iter()
        .filter(|repo| draft.repos.iter().all(|r| r.path != repo.path))
        .collect()
}

/// A space [`launch_space`] made, ready to open.
#[derive(Clone, Debug, PartialEq)]
pub struct LaunchedSpace {
    /// The space, as saved in the store.
    pub space: Space,
    /// Its first layout.
    pub layout: SpaceLayout,
    /// Every pane of the layout; all need starting.
    pub panes: Vec<RolePane>,
    /// Why the layout could not be saved, if it could not; the space is launched regardless.
    pub warning: Option<String>,
}

/// Launch `draft` (worktrees, mise files, the space in the store) and save its first layout. Fails, with nothing
/// left behind, when the launch fails. A layout that cannot be saved is a warning, since the worktrees exist.
/// The space's `PORT` avoids the active spaces' ports and `attempt_ports` (those live attempts hold).
pub fn launch_space(
    store: &mut Store,
    draft: SpaceDraft,
    attempt_ports: Vec<u16>,
) -> Result<LaunchedSpace, String> {
    let space = xctx_core::launch_avoiding(store, draft, attempt_ports)
        .map_err(|e| format!("launch failed: {e}"))?;
    let (layout, panes) = SpaceLayout::launch(layout_shape(space.shape), &repo_names(&space))
        .map_err(|e| format!("launched {} but could not lay it out: {e}", space.id))?;
    let warning = save_layout(store, &space.id, &layout).err();
    Ok(LaunchedSpace {
        space,
        layout,
        panes,
        warning,
    })
}

/// Reshape `space` to `shape`: the layout changes (agents kept), the space's shape is saved to the store, and the
/// new layout is saved. Returns what to start and stop, and what could not be saved, if anything; the layout
/// and `space` are changed regardless. Fails, changing nothing, when the layout cannot take the shape.
pub fn reshape_space(
    store: &mut Store,
    space: &mut Space,
    layout: &mut SpaceLayout,
    shape: Shape,
) -> Result<(Reshape, Option<String>), String> {
    let result = layout
        .reshape(shape, &repo_names(space))
        .map_err(|e| format!("could not reshape {}: {e}", space.id))?;
    space.shape = space_shape(shape);
    let mut errors = Vec::new();
    if let Err(e) = store.upsert_space(space.clone()) {
        errors.push(format!("could not save the shape of {}: {e}", space.id));
    }
    if let Err(e) = save_layout(store, &space.id, layout) {
        errors.push(e);
    }
    let warning = (!errors.is_empty()).then(|| errors.join("; "));
    Ok((result, warning))
}

/// The names of the space's repos whose worktree has uncommitted changes. Worktrees that are gone are skipped.
pub fn dirty_repos(space: &Space) -> xctx_core::Result<Vec<String>> {
    let mut dirty = Vec::new();
    for repo in &space.repos {
        if repo.worktree.is_dir() && git::is_dirty(&repo.worktree)? {
            dirty.push(repo.name.clone());
        }
    }
    Ok(dirty)
}

/// Ship the space: with a non-empty `pr_title` different from the space's title, the title is changed first
/// (pull requests are titled after the space). On success the space's layout document is removed too.
pub fn ship_space(
    store: &mut Store,
    id: &SpaceId,
    pr_title: Option<&str>,
    options: &ShipOptions,
) -> xctx_core::Result<ShipReport> {
    if let Some(title) = pr_title.map(str::trim).filter(|t| !t.is_empty())
        && let Some(space) = store.space(id)
        && space.title != title
    {
        let mut space = space.clone();
        space.title = title.to_owned();
        store.upsert_space(space)?;
    }
    let report = xctx_core::ship(store, id, options)?;
    // The space is shipped either way; a layout file left behind is harmless.
    let _ = store.remove_layout(id);
    Ok(report)
}

/// Discard the space (with `force`, even with uncommitted changes) and remove its layout document.
pub fn discard_space(
    store: &mut Store,
    id: &SpaceId,
    force: bool,
) -> xctx_core::Result<DiscardReport> {
    let report = xctx_core::discard(store, id, force)?;
    let _ = store.remove_layout(id);
    Ok(report)
}

/// What a ship did, one line per repo, then the worktrees removed and the branches kept.
pub fn ship_report_lines(report: &ShipReport) -> Vec<String> {
    let mut lines: Vec<String> = report.repos.iter().map(ToString::to_string).collect();
    for removed in &report.removed {
        let gone = if removed.already_gone {
            " (already gone)"
        } else {
            ""
        };
        lines.push(format!(
            "removed worktree {}{gone}; kept branch {}",
            removed.worktree.display(),
            removed.branch
        ));
    }
    lines
}

/// One line saying what a discard did.
pub fn discard_message(report: &DiscardReport) -> String {
    let count = report.removed.len();
    let worktrees = if count == 1 { "worktree" } else { "worktrees" };
    format!(
        "Discarded {}: removed {count} {worktrees}; kept branch {}",
        report.space,
        xctx_core::space::space_branch(report.space.as_str())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_is_expanded_only_at_the_start() {
        let home = Some(Path::new("/Users/me"));
        assert_eq!(
            expand_home(" ~/Desktop ", home),
            PathBuf::from("/Users/me/Desktop")
        );
        assert_eq!(expand_home("~", home), PathBuf::from("/Users/me"));
        assert_eq!(expand_home("~other", home), PathBuf::from("~other"));
        assert_eq!(expand_home("/a/~/b", home), PathBuf::from("/a/~/b"));
        assert_eq!(expand_home("~/x", None), PathBuf::from("~/x"));
    }

    #[test]
    fn folders_to_index_must_be_existing_absolute_directories()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        assert_eq!(
            folder_to_index(&dir.path().to_string_lossy(), None)?,
            dir.path()
        );
        assert!(folder_to_index("", None).is_err());
        assert!(folder_to_index("relative/path", None).is_err());
        let file = dir.path().join("file");
        std::fs::write(&file, "")?;
        assert!(folder_to_index(&file.to_string_lossy(), None).is_err());
        assert!(folder_to_index(&dir.path().join("missing").to_string_lossy(), None).is_err());
        Ok(())
    }

    #[test]
    fn a_path_outside_any_repo_cannot_join_a_draft() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        assert!(repo_for_draft(&dir.path().to_string_lossy(), None).is_err());
        assert!(repo_for_draft("  ", None).is_err());
        Ok(())
    }
}
