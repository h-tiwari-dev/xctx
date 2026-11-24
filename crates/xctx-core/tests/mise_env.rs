//! `MiseRunner::inspect` against the real mise, isolated from the user's config, state and trust list.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{TestResult, temp_root};
use xctx_core::{Error, MiseRunner, Redactor};

/// A mise that sees only `dir`: its own config, state and cache directories, `dir` trusted without writing to
/// the user's trust list, and no global config.
fn isolated(dir: &Path) -> MiseRunner {
    let home = dir.join(".mise-home");
    MiseRunner::new()
        .with_env("MISE_TRUSTED_CONFIG_PATHS", dir)
        .with_env("MISE_CEILING_PATHS", dir.parent().unwrap_or(dir))
        .with_env("MISE_GLOBAL_CONFIG_FILE", home.join("config.toml"))
        .with_env("MISE_CONFIG_DIR", home.join("config"))
        .with_env("MISE_DATA_DIR", home.join("data"))
        .with_env("MISE_STATE_DIR", home.join("state"))
        .with_env("MISE_CACHE_DIR", home.join("cache"))
        .with_env("MISE_YES", "1")
}

fn project(files: &[(&str, &str)]) -> TestResult<(common::TempRoot, PathBuf)> {
    let root = temp_root()?;
    let dir = root.path.join("app");
    fs::create_dir_all(&dir)?;
    for (name, text) in files {
        fs::write(dir.join(name), text)?;
    }
    Ok((root, dir))
}

const CONFIG: &str = r#"
redactions = ["*_TOKEN"]
[env]
PLAIN = "hello"
API_TOKEN = "tok-12345"
DB_PASSWORD = { value = "hunter22", redact = true }
XCTX_TEST_NEEDED = { required = "Ask the team for the staging key" }
XCTX_TEST_NEEDED_TOO = { required = true }
"#;

#[test]
fn every_missing_required_var_is_found_with_its_help() -> TestResult {
    let (_root, dir) = project(&[("mise.toml", CONFIG)])?;
    let report = isolated(&dir).inspect(&dir, None)?;
    let names: Vec<_> = report.missing.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["XCTX_TEST_NEEDED", "XCTX_TEST_NEEDED_TOO"]);
    assert_eq!(
        report.missing[0].help.as_deref(),
        Some("Ask the team for the staging key")
    );
    assert_eq!(report.missing[1].help, None);
    let required_in = report.missing[0]
        .required_in
        .as_deref()
        .map(fs::canonicalize);
    assert_eq!(
        required_in.transpose()?,
        Some(fs::canonicalize(dir.join("mise.toml"))?)
    );
    assert!(!report.is_complete());
    Ok(())
}

#[test]
fn secrets_are_the_redacted_vars_and_placeholders_are_not_among_them() -> TestResult {
    let (_root, dir) = project(&[("mise.toml", CONFIG)])?;
    let report = isolated(&dir).inspect(&dir, None)?;
    let names: Vec<_> = report.secrets.keys().map(String::as_str).collect();
    assert_eq!(names, ["API_TOKEN", "DB_PASSWORD"]);
    let redactor = Redactor::from(&report.secrets);
    assert_eq!(
        redactor.redact("token tok-12345, password hunter22, plain hello"),
        "token [REDACTED:API_TOKEN], password [REDACTED:DB_PASSWORD], plain hello"
    );
    Ok(())
}

#[test]
fn the_mise_env_overlay_is_included() -> TestResult {
    let (_root, dir) = project(&[
        ("mise.toml", "[env]\nPLAIN = \"x\"\n"),
        (
            "mise.staging.toml",
            "[env]\nSTAGE_SECRET = { value = \"s3cr3t-stage\", redact = true }\nSTAGE_KEY = { required = \"staging only\" }\n",
        ),
    ])?;
    let plain = isolated(&dir).inspect(&dir, None)?;
    assert!(plain.is_complete());
    assert!(plain.secrets.is_empty());
    let staging = isolated(&dir).inspect(&dir, Some("staging"))?;
    assert_eq!(staging.missing.len(), 1);
    assert_eq!(staging.missing[0].help.as_deref(), Some("staging only"));
    assert_eq!(
        staging.secrets.get("STAGE_SECRET").map(String::as_str),
        Some("s3cr3t-stage")
    );
    Ok(())
}

#[test]
fn a_set_variable_is_not_missing() -> TestResult {
    let (_root, dir) = project(&[("mise.toml", CONFIG)])?;
    let report = isolated(&dir)
        .with_env("XCTX_TEST_NEEDED", "given")
        .inspect(&dir, None)?;
    let names: Vec<_> = report.missing.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["XCTX_TEST_NEEDED_TOO"]);
    Ok(())
}

#[test]
fn other_mise_failures_are_errors_with_mises_message() -> TestResult {
    let (_root, dir) = project(&[("mise.toml", "[env\nbroken")])?;
    let result = isolated(&dir).inspect(&dir, None);
    assert!(
        matches!(&result, Err(Error::Command { program, .. }) if program == "mise"),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_missing_program_is_a_spawn_error() -> TestResult {
    let (_root, dir) = project(&[])?;
    let result = MiseRunner::new()
        .with_program(dir.join("no-such-mise"))
        .inspect(&dir, None);
    assert!(matches!(result, Err(Error::Spawn { .. })), "{result:?}");
    Ok(())
}
