//! Importing the Swift prototype's spaces into a temporary store.

mod common;

use std::fs;

use common::{TestResult, git, make_repo, temp_root};
use xctx_core::prototype::import_prototype;
use xctx_core::{SpaceShape, SpaceStatus, Store};

#[test]
fn living_prototype_spaces_are_imported_once_and_the_prototype_file_is_left_alone() -> TestResult {
    let root = temp_root()?;
    let repo = root.path.join("code").join("portfolio");
    make_repo(&repo, &[("README.md", "portfolio\n")])?;
    let worktree = root
        .path
        .join("code")
        .join("portfolio-worktrees")
        .join("lets-start");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "space/lets-start",
            worktree.to_str().ok_or("path")?,
        ],
    )?;
    let gone = root
        .path
        .join("code")
        .join("portfolio-worktrees")
        .join("old");
    let proto = root.path.join("PixelWorkspace-spaces.json");
    let text = format!(
        r#"{{
  "inbox" : [],
  "roots" : [ "{code}" ],
  "spaces" : [
    {{ "created" : "2026-11-23T18:42:33Z", "id" : "lets-start", "name" : "lets start", "port" : 4001,
       "repos" : [ {{ "base" : "abc", "repo" : "{repo}", "worktree" : "{worktree}" }} ],
       "shape" : "new", "task" : "lets start working" }},
    {{ "id" : "old", "name" : "old", "port" : 4002,
       "repos" : [ {{ "repo" : "{repo}", "worktree" : "{gone}" }} ] }},
    {{ "id" : "Bad Slug", "name" : "x", "port" : 4003, "repos" : [] }}
  ]
}}"#,
        code = root.path.join("code").display(),
        repo = repo.display(),
        worktree = worktree.display(),
        gone = gone.display(),
    );
    fs::write(&proto, &text)?;

    let mut store = Store::open(root.path.join("data"))?;
    let report = import_prototype(&mut store, &proto)?;
    assert_eq!(report.imported.len(), 1);
    assert_eq!(report.roots_added, 1);
    assert_eq!(report.skipped.len(), 2, "{:?}", report.skipped);

    let space = store.spaces().first().ok_or("imported")?.clone();
    assert_eq!(space.id.as_str(), "lets-start");
    assert_eq!(
        (space.title.as_str(), space.task.as_str()),
        ("lets start", "lets start working")
    );
    assert_eq!(
        (space.status, space.shape, space.port),
        (SpaceStatus::Active, SpaceShape::Dev, 4001)
    );
    assert_eq!(space.created_at, 1_795_459_353);
    let r = &space.repos[0];
    assert_eq!(
        (r.name.as_str(), r.branch.as_str(), r.base_branch.as_str()),
        ("portfolio", "space/lets-start", "main")
    );
    assert_eq!(r.base_commit, "abc");
    assert!(worktree.join("mise.lets-start.toml").exists());
    assert!(xctx_core::git::is_ignored(
        &worktree,
        std::path::Path::new("mise.lets-start.toml")
    )?);

    // Again: nothing new, and the prototype's file is unchanged.
    let again = import_prototype(&mut store, &proto)?;
    assert!(again.imported.is_empty());
    assert_eq!(store.spaces().len(), 1);
    assert_eq!(fs::read_to_string(&proto)?, text);
    // A store that survives a reopen.
    assert_eq!(Store::open(root.path.join("data"))?.spaces().len(), 1);
    Ok(())
}

#[test]
fn a_taken_port_is_replaced_and_a_missing_file_imports_nothing() -> TestResult {
    let root = temp_root()?;
    let mut store = Store::open(root.path.join("data"))?;
    let none = import_prototype(&mut store, &root.path.join("missing.json"))?;
    assert!(none.imported.is_empty() && none.skipped.is_empty());

    let repo = root.path.join("api");
    make_repo(&repo, &[("README.md", "api\n")])?;
    let a = root.path.join("api-worktrees").join("a");
    let b = root.path.join("api-worktrees").join("b");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "space/a",
            a.to_str().ok_or("path")?,
        ],
    )?;
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "space/b",
            b.to_str().ok_or("path")?,
        ],
    )?;
    let proto = root.path.join("p.json");
    fs::write(
        &proto,
        format!(
            r#"{{"spaces":[{{"id":"a","port":4001,"repos":[{{"repo":"{r}","worktree":"{a}"}}]}},
                          {{"id":"b","port":4001,"repos":[{{"repo":"{r}","worktree":"{b}"}}]}}]}}"#,
            r = repo.display(),
            a = a.display(),
            b = b.display()
        ),
    )?;
    let report = import_prototype(&mut store, &proto)?;
    assert_eq!(report.imported.len(), 2);
    let ports: Vec<u16> = store.spaces().iter().map(|s| s.port).collect();
    assert_eq!(ports, [4001, 4002]);
    Ok(())
}
