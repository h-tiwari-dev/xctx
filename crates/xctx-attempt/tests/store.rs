//! The attempts file in temporary data directories.

use std::fs;
use std::path::PathBuf;

use xctx_attempt::store::{ATTEMPTS_FILE, attempts_path, load, save};
use xctx_attempt::{AttemptError, AttemptSet, Outcome};
use xctx_core::{AttemptWorktree, SpaceId};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn set_with_two() -> Result<AttemptSet, AttemptError> {
    let mut set = AttemptSet::new(SpaceId::new("rate-limit"));
    let plan = set.plan_fork("writer-api", "api", vec![Some("token bucket".into()), None])?;
    let trees = plan
        .numbers
        .iter()
        .map(|&number| AttemptWorktree {
            number,
            repo: "api".into(),
            repo_path: "/code/api".into(),
            worktree: PathBuf::from(format!("/code/api-worktrees/rate-limit--attempt-{number}")),
            branch: format!("attempt/rate-limit/{number}"),
            fork_commit: "abc123".into(),
            port: 4100 + number as u16,
        })
        .collect();
    set.record_fork(&plan, trees, "xctx-rate-limit-writer-api", 100)?;
    set.report(
        1,
        Outcome {
            summary: "done".into(),
            ..Outcome::default()
        },
        false,
        101,
    )?;
    Ok(set)
}

#[test]
fn a_space_without_a_file_has_no_attempts_and_a_saved_set_comes_back() -> TestResult {
    let dir = tempfile::tempdir()?;
    let space = SpaceId::new("rate-limit");
    let empty = load(dir.path(), &space)?;
    assert!(empty.attempts.is_empty());
    assert_eq!(empty.next_number, 1);

    let set = set_with_two()?;
    save(dir.path(), &set)?;
    assert!(
        dir.path()
            .join("spaces/rate-limit")
            .join(ATTEMPTS_FILE)
            .exists()
    );
    assert_eq!(load(dir.path(), &space)?, set);
    Ok(())
}

#[test]
fn slugs_cannot_leave_the_spaces_directory() {
    for bad in ["", "../x", "a/b", "a b"] {
        assert!(matches!(
            attempts_path(std::path::Path::new("/d"), bad),
            Err(AttemptError::InvalidSlug(_))
        ));
    }
}

#[test]
fn damaged_or_newer_files_are_errors_and_left_alone() -> TestResult {
    let dir = tempfile::tempdir()?;
    let space = SpaceId::new("s");
    let path = attempts_path(dir.path(), "s")?;
    fs::create_dir_all(path.parent().ok_or("parent")?)?;
    fs::write(&path, b"{not json")?;
    assert!(matches!(
        load(dir.path(), &space),
        Err(AttemptError::Store(_))
    ));
    assert_eq!(fs::read(&path)?, b"{not json");
    fs::write(
        &path,
        br#"{"version": 9, "space": "s", "next_number": 1, "next_round": 1, "attempts": []}"#,
    )?;
    assert!(matches!(
        load(dir.path(), &space),
        Err(AttemptError::UnsupportedVersion { found: 9, .. })
    ));
    Ok(())
}
