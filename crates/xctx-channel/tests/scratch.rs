//! Scratch directories under temporary bases; nothing here touches `/tmp/xctx-scratch`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use xctx_channel::{ChannelError, ScratchDir};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn artifacts_round_trip_list_and_clean() -> TestResult {
    let base = tempfile::tempdir()?;
    let scratch = ScratchDir::in_base(base.path(), "rate-limit")?;
    assert_eq!(scratch.root(), base.path().join("rate-limit"));
    assert!(scratch.list_artifacts()?.is_empty());
    assert!(
        !scratch.root().exists(),
        "nothing is created before a write"
    );

    let written = scratch.write_artifact("api/payload.json", b"{\"limit\":10}")?;
    assert_eq!(written, scratch.root().join("api/payload.json"));
    scratch.write_artifact("notes.txt", b"one")?;
    scratch.write_artifact("notes.txt", b"two")?;
    assert_eq!(
        scratch.read_artifact("api/payload.json")?,
        b"{\"limit\":10}"
    );
    assert_eq!(scratch.read_artifact("./notes.txt")?, b"two");
    assert_eq!(
        scratch.list_artifacts()?,
        [
            PathBuf::from("api/payload.json"),
            PathBuf::from("notes.txt")
        ]
    );

    assert!(scratch.clean()?);
    assert!(!scratch.root().exists());
    assert!(!scratch.clean()?, "cleaning twice is fine");
    assert!(base.path().exists(), "the base is left alone");
    Ok(())
}

#[test]
fn paths_that_leave_the_directory_are_refused() -> TestResult {
    let base = tempfile::tempdir()?;
    let scratch = ScratchDir::in_base(base.path().join("scratch"), "rl")?;
    for bad in [
        "../outside.txt",
        "a/../../outside.txt",
        "/tmp/outside.txt",
        "a\0b",
        "",
    ] {
        assert!(
            matches!(
                scratch.write_artifact(bad, b"x"),
                Err(ChannelError::InvalidArtifactPath { .. })
            ),
            "{bad:?} should be refused"
        );
        assert!(matches!(
            scratch.read_artifact(bad),
            Err(ChannelError::InvalidArtifactPath { .. })
        ));
    }
    assert!(!base.path().join("outside.txt").exists());
    assert!(!base.path().join("scratch").exists());
    Ok(())
}

#[test]
fn unsafe_slugs_are_refused() -> TestResult {
    let base = tempfile::tempdir()?;
    for slug in ["", "..", "../x", "a/b"] {
        assert!(matches!(
            ScratchDir::in_base(base.path(), slug),
            Err(ChannelError::InvalidSlug(_))
        ));
    }
    Ok(())
}

#[test]
fn symbolic_links_are_not_followed() -> TestResult {
    let base = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    fs::write(outside.path().join("secret.txt"), b"secret")?;

    let scratch = ScratchDir::in_base(base.path(), "rl")?;
    fs::create_dir_all(scratch.root())?;
    std::os::unix::fs::symlink(outside.path(), scratch.root().join("link"))?;
    assert!(matches!(
        scratch.read_artifact("link/secret.txt"),
        Err(ChannelError::InvalidArtifactPath { .. })
    ));
    assert!(matches!(
        scratch.write_artifact("link/new.txt", b"x"),
        Err(ChannelError::InvalidArtifactPath { .. })
    ));
    assert!(scratch.list_artifacts()?.is_empty(), "links are not listed");
    assert!(scratch.clean()?);
    assert!(
        outside.path().join("secret.txt").exists(),
        "cleaning does not follow links"
    );

    let hijacked = ScratchDir::in_base(base.path(), "hijacked")?;
    std::os::unix::fs::symlink(outside.path(), hijacked.root())?;
    assert!(matches!(
        hijacked.write_artifact("x.txt", b"x"),
        Err(ChannelError::UnsafeScratchDir { .. })
    ));
    assert!(matches!(
        hijacked.read_artifact("secret.txt"),
        Err(ChannelError::UnsafeScratchDir { .. })
    ));
    assert!(matches!(
        hijacked.list_artifacts(),
        Err(ChannelError::UnsafeScratchDir { .. })
    ));
    assert!(hijacked.clean()?);
    assert!(outside.path().join("secret.txt").exists());
    assert!(!outside.path().join("x.txt").exists());
    Ok(())
}

#[test]
fn a_symlinked_base_is_refused_and_its_target_is_never_touched() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let target = tmp.path().join("target");
    fs::create_dir_all(target.join("rl"))?;
    fs::write(target.join("rl/existing.txt"), b"keep me")?;
    let base = tmp.path().join("base");
    std::os::unix::fs::symlink(&target, &base)?;

    let scratch = ScratchDir::in_base(&base, "rl")?;
    assert!(matches!(
        scratch.write_artifact("new.txt", b"x"),
        Err(ChannelError::UnsafeScratchDir { .. })
    ));
    assert!(matches!(
        scratch.read_artifact("existing.txt"),
        Err(ChannelError::UnsafeScratchDir { .. })
    ));
    assert!(matches!(
        scratch.list_artifacts(),
        Err(ChannelError::UnsafeScratchDir { .. })
    ));
    assert!(matches!(
        scratch.clean(),
        Err(ChannelError::UnsafeScratchDir { .. })
    ));

    assert!(fs::symlink_metadata(&base)?.file_type().is_symlink());
    assert_eq!(fs::read(target.join("rl/existing.txt"))?, b"keep me");
    assert!(!target.join("rl/new.txt").exists());
    assert_eq!(fs::read_dir(target.join("rl"))?.count(), 1);
    Ok(())
}

#[test]
fn a_base_that_is_not_a_directory_is_refused() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let base = tmp.path().join("base");
    fs::write(&base, b"a file")?;
    let scratch = ScratchDir::in_base(&base, "rl")?;
    assert!(matches!(
        scratch.write_artifact("x.txt", b"x"),
        Err(ChannelError::UnsafeScratchDir { .. })
    ));
    assert!(matches!(
        scratch.clean(),
        Err(ChannelError::UnsafeScratchDir { .. })
    ));
    assert_eq!(fs::read(&base)?, b"a file");
    Ok(())
}

#[test]
fn created_directories_are_private_to_the_user() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let base = tmp.path().join("scratch-base");
    let scratch = ScratchDir::in_base(&base, "rl")?;
    scratch.write_artifact("api/payload.json", b"{}")?;
    for dir in [
        base.clone(),
        scratch.root().to_path_buf(),
        scratch.root().join("api"),
    ] {
        let mode = fs::metadata(&dir)?.permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0, "{} is mode {mode:o}", dir.display());
    }
    Ok(())
}
