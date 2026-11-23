mod common;

use std::fs;

use common::{TestResult, commit_file, git, make_repos, temp_root};
use xctx_core::RepoIndex;
use xctx_core::index::find_repos;
use xctx_core::space::DraftRepo;

#[test]
fn scan_finds_repositories_and_reads_what_they_are() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    git(&repos.payments, &["branch", "retry-charges"])?;
    commit_file(
        &repos.payments,
        "src/retry.ts",
        "",
        "retry declined charges",
    )?;

    let mut index = RepoIndex::new(vec![repos.code.clone()]);
    index.scan();
    let mut names: Vec<&str> = index.repos().iter().map(|r| r.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["checkout-web", "payments"], "notes has no .git");

    let payments = index.repo(&repos.payments).ok_or("payments not indexed")?;
    assert_eq!(
        payments.description.as_deref(),
        Some("Payments service: charges carts.")
    );
    assert_eq!(payments.languages, ["TypeScript"]);
    assert_eq!(payments.top_level_files, ["README.md", "src/"]);
    assert_eq!(
        payments.recent_commits,
        ["retry declined charges", "first commit"]
    );
    assert!(
        payments
            .recent_branches
            .contains(&"retry-charges".to_owned())
    );
    assert!(payments.touched.is_some());
    assert!(payments.files().contains(&"src/limit.ts".to_owned()));
    Ok(())
}

#[test]
fn search_ranks_name_matches_first_and_says_why() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let mut index = RepoIndex::new(vec![repos.code.clone()]);
    index.scan();

    let found = index.search("add rate limiting to checkout payments", 5);
    let mut names: Vec<&str> = found.iter().map(|m| m.repo.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["checkout-web", "payments"]);
    let payments = found
        .iter()
        .find(|m| m.repo.name == "payments")
        .ok_or("payments not found")?;
    assert_eq!(
        payments.reason(),
        "name matches \"payments\"; README mentions \"payments\"; files src/limit.ts"
    );

    let charge = index.search("charge", 5);
    assert_eq!(charge.len(), 1);
    assert_eq!(charge[0].repo.name, "payments");
    assert!(
        charge[0].reason().contains("src/charge.ts"),
        "{}",
        charge[0].reason()
    );

    assert!(
        index.search("the and for", 5).is_empty(),
        "common words alone match nothing"
    );
    assert!(index.search("kubernetes", 5).is_empty());
    assert_eq!(index.search("checkout payments", 1).len(), 1);
    Ok(())
}

#[test]
fn a_draft_proposes_matches_with_reasons_and_takes_additions_and_drops() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let mut index = RepoIndex::new(vec![repos.code.clone()]);
    index.scan();

    let mut draft = index.draft("make the checkout page faster");
    assert_eq!(draft.task, "make the checkout page faster");
    assert_eq!(draft.repos.len(), 1);
    assert_eq!(draft.repos[0].path, repos.checkout_web);
    assert!(
        draft.repos[0]
            .reason
            .starts_with("name matches \"checkout\"")
    );

    assert!(draft.add_repo(DraftRepo::new(repos.payments.clone(), "added by you")));
    assert!(!draft.add_repo(DraftRepo::new(repos.payments.clone(), "again")));
    assert_eq!(draft.repos[1].name, "payments");
    assert!(draft.drop_repo(&repos.checkout_web));
    assert_eq!(draft.repos.len(), 1);
    assert_eq!(draft.repos[0].path, repos.payments);
    Ok(())
}

#[test]
fn roots_can_be_added_and_removed() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let web = repos.code.join("web");
    let mut index = RepoIndex::new(Vec::new());
    assert!(index.repos().is_empty());

    assert!(index.add_root(web.clone()));
    assert!(!index.add_root(web.clone()));
    assert_eq!(index.repos().len(), 1);
    assert!(index.add_root(repos.code.clone()));
    assert_eq!(index.repos().len(), 2, "checkout-web is not indexed twice");

    assert!(index.remove_root(&web));
    assert_eq!(index.repos().len(), 2, "code still holds checkout-web");
    assert!(index.remove_root(&repos.code));
    assert!(index.repos().is_empty());
    assert!(!index.remove_root(&repos.code));
    assert!(index.roots().is_empty());
    Ok(())
}

#[test]
fn space_worktrees_hidden_and_dependency_folders_are_not_indexed() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    for skipped in [
        "payments-worktrees/task",
        "node_modules/dep",
        ".hidden/repo",
    ] {
        let dir = repos.code.join(skipped);
        fs::create_dir_all(&dir)?;
        common::make_repo(&dir, &[("a.txt", "a")])?;
    }
    let found = find_repos(&repos.code, 4);
    assert_eq!(found, [repos.payments.clone(), repos.checkout_web.clone()]);
    assert!(find_repos(&root.path.join("missing"), 4).is_empty());
    assert_eq!(
        find_repos(&repos.payments, 0),
        std::slice::from_ref(&repos.payments)
    );
    Ok(())
}
