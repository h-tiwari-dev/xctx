mod common;

use std::path::PathBuf;

use common::{TestResult, make_repos, temp_root};
use serde_json::json;
use xctx_core::space::{DraftRepo, SpaceDraft, SpaceShape};
use xctx_core::{Space, SpaceId, Store, launch, launch_avoiding};

#[test]
fn spaces_roots_ports_and_layouts_survive_reopening() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let data = root.path.join("data");
    let mut store = Store::open(&data)?;
    assert!(store.spaces().is_empty());

    assert!(store.add_root(repos.code.clone())?);
    assert!(!store.add_root(repos.code.clone())?);
    assert!(store.add_root(PathBuf::from("/elsewhere"))?);
    assert!(store.remove_root(&PathBuf::from("/elsewhere"))?);
    assert!(!store.remove_root(&PathBuf::from("/elsewhere"))?);

    let mut draft = SpaceDraft::new("checkout retry");
    draft.add_repo(DraftRepo::new(repos.checkout_web.clone(), "name"));
    let mut space = launch(&mut store, draft)?;
    space.shape = SpaceShape::Review;
    store.upsert_space(space.clone())?;
    let layout = json!({"tree": {"split": "h", "a": 1, "b": 2}, "agents": [{"session": "abc"}]});
    store.save_layout(&space.id, &layout)?;

    let reopened = Store::open(&data)?;
    assert_eq!(reopened.roots(), std::slice::from_ref(&repos.code));
    assert_eq!(reopened.spaces(), std::slice::from_ref(&space));
    assert_eq!(
        reopened.space(&space.id).map(|s| s.shape),
        Some(SpaceShape::Review)
    );
    assert_eq!(reopened.port_allocations().get(&4001), Some(&space.id));
    assert_eq!(reopened.allocate_port()?, 4002);
    // Ports the store does not record (attempts') are avoided when given.
    assert_eq!(reopened.allocate_port_avoiding([4002, 4003])?, 4004);
    assert_eq!(reopened.allocate_port_avoiding([4001])?, 4002);
    assert_eq!(reopened.load_layout(&space.id)?, Some(layout));
    assert_eq!(reopened.load_layout(&SpaceId::new("other"))?, None);
    assert_eq!(reopened.unique_slug("checkout retry"), "checkout-retry-2");

    let mut store = reopened;
    let removed: Option<Space> = store.remove_space(&space.id)?;
    assert_eq!(removed.as_ref().map(|s| &s.id), Some(&space.id));
    assert_eq!(
        store.load_layout(&space.id)?,
        None,
        "its layout goes with it"
    );
    assert_eq!(store.remove_space(&space.id)?, None);
    assert!(Store::open(&data)?.spaces().is_empty());
    Ok(())
}

#[test]
fn a_launch_avoids_ports_the_store_does_not_record() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let mut store = Store::open(root.path.join("data"))?;
    let mut draft = SpaceDraft::new("checkout retry");
    draft.add_repo(DraftRepo::new(repos.checkout_web.clone(), "name"));
    // An attempt of another space holds 4001 and 4002.
    let space = launch_avoiding(&mut store, draft, [4001, 4002])?;
    assert_eq!(space.port, 4003);
    let mise = std::fs::read_to_string(
        space.repos[0]
            .worktree
            .join(format!("mise.{}.toml", space.id)),
    )?;
    assert!(mise.contains("4003"), "{mise}");
    Ok(())
}

#[test]
fn the_spaces_file_is_readable_json() -> TestResult {
    let root = temp_root()?;
    let data = root.path.join("data");
    let mut store = Store::open(&data)?;
    store.add_root(PathBuf::from("/code"))?;
    let text = std::fs::read_to_string(data.join("spaces.json"))?;
    let value: serde_json::Value = serde_json::from_str(&text)?;
    assert_eq!(value["version"], 1);
    assert_eq!(value["roots"], json!(["/code"]));
    Ok(())
}
