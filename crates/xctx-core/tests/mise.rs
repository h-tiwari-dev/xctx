mod common;

use std::fs;
use std::path::Path;

use common::{TestResult, git, make_repo, temp_root};
use xctx_core::{git as xgit, mise};

#[test]
fn the_space_mise_file_holds_the_port_and_is_ignored_without_touching_tracked_files() -> TestResult
{
    let root = temp_root()?;
    let repo = root.path.join("app");
    make_repo(
        &repo,
        &[("README.md", "app\n"), (".gitignore", "target/\n")],
    )?;
    let worktree = root.path.join("app-worktrees").join("rate-limit");
    xgit::create_worktree(&repo, &worktree, "space/rate-limit", "HEAD")?;

    let path = mise::write_space_mise_file(&worktree, "rate-limit", "Rate limit", 4002)?;
    assert_eq!(path, worktree.join("mise.rate-limit.toml"));
    let text = fs::read_to_string(&path)?;
    assert!(text.contains("[env]\nPORT = \"4002\"\n"), "{text}");
    assert!(text.contains("XCTX_SPACE = \"rate-limit\""), "{text}");

    assert!(xgit::is_ignored(
        &worktree,
        Path::new("mise.rate-limit.toml")
    )?);
    assert!(!xgit::is_dirty(&worktree)?, "the worktree stays clean");
    assert!(!xgit::is_dirty(&repo)?, "the main checkout stays clean");
    assert_eq!(fs::read_to_string(repo.join(".gitignore"))?, "target/\n");
    assert_eq!(
        fs::read_to_string(worktree.join(".gitignore"))?,
        "target/\n"
    );

    let exclude = repo.join(".git").join("info").join("exclude");
    let lines = |text: String| {
        text.lines()
            .filter(|l| *l == "/mise.rate-limit.toml")
            .count()
    };
    assert_eq!(
        lines(fs::read_to_string(&exclude)?),
        1,
        "the shared exclude file holds the line"
    );
    mise::write_space_mise_file(&worktree, "rate-limit", "Rate limit", 4002)?;
    assert_eq!(
        lines(fs::read_to_string(&exclude)?),
        1,
        "writing again adds no second line"
    );

    mise::remove_space_mise_file(&worktree, &repo, "rate-limit")?;
    assert!(!path.exists());
    assert_eq!(lines(fs::read_to_string(&exclude)?), 0);
    assert!(!xgit::is_dirty(&worktree)?);
    mise::remove_space_mise_file(&worktree, &repo, "rate-limit")?;
    Ok(())
}

#[test]
fn excludes_keep_the_users_own_lines() -> TestResult {
    let root = temp_root()?;
    let repo = root.path.join("app");
    make_repo(&repo, &[("README.md", "app\n")])?;
    let exclude = repo.join(".git").join("info").join("exclude");
    fs::write(&exclude, "# mine\n*.log")?;

    mise::ensure_git_excluded(&repo, "mise.a.toml")?;
    assert_eq!(
        fs::read_to_string(&exclude)?,
        "# mine\n*.log\n/mise.a.toml\n"
    );
    mise::remove_git_exclude(&repo, "mise.a.toml")?;
    assert_eq!(fs::read_to_string(&exclude)?, "# mine\n*.log\n");
    Ok(())
}

#[test]
fn removing_the_mise_file_of_a_worktree_that_is_gone_still_cleans_the_exclude() -> TestResult {
    let root = temp_root()?;
    let repo = root.path.join("app");
    make_repo(&repo, &[("README.md", "app\n")])?;
    let worktree = root.path.join("wt");
    xgit::create_worktree(&repo, &worktree, "space/gone", "HEAD")?;
    mise::write_space_mise_file(&worktree, "gone", "Gone", 4001)?;
    fs::remove_dir_all(&worktree)?;
    git(&repo, &["worktree", "prune"])?;

    mise::remove_space_mise_file(&worktree, &repo, "gone")?;
    let exclude = fs::read_to_string(repo.join(".git").join("info").join("exclude"))?;
    assert!(!exclude.contains("mise.gone.toml"), "{exclude}");
    Ok(())
}
