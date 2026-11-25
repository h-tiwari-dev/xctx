//! `MiseRunner::inspect` against the real mise, isolated from the user's config, state and trust list.

mod common;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestResult, temp_root};
use xctx_core::space::{Space, SpaceId, SpaceShape, SpaceStatus};
use xctx_core::{Error, MiseRunner, Redactor, mise};

/// The environment that makes mise see only `dir`: its own config, state and cache directories, `dir` trusted
/// without writing to the user's trust list, and no global config.
fn isolation_env(dir: &Path) -> Vec<(&'static str, OsString)> {
    let home = dir.join(".mise-home");
    vec![
        ("MISE_TRUSTED_CONFIG_PATHS", dir.into()),
        ("MISE_CEILING_PATHS", dir.parent().unwrap_or(dir).into()),
        ("MISE_GLOBAL_CONFIG_FILE", home.join("config.toml").into()),
        ("MISE_CONFIG_DIR", home.join("config").into()),
        ("MISE_DATA_DIR", home.join("data").into()),
        ("MISE_STATE_DIR", home.join("state").into()),
        ("MISE_CACHE_DIR", home.join("cache").into()),
        ("MISE_YES", "1".into()),
    ]
}

/// A [`MiseRunner`] isolated by [`isolation_env`].
fn isolated(dir: &Path) -> MiseRunner {
    isolation_env(dir)
        .into_iter()
        .fold(MiseRunner::new(), |runner, (key, value)| {
            runner.with_env(key, value)
        })
}

/// The unredacted environment an isolated mise builds in `dir` with `MISE_ENV=<mise_env>`.
fn isolated_env_values(dir: &Path, mise_env: &str) -> TestResult<serde_json::Value> {
    let output = Command::new("mise")
        .args(["env", "--json"])
        .current_dir(dir)
        .envs(isolation_env(dir))
        .env("MISE_ENV", mise_env)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
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
fn a_value_set_for_a_space_is_a_secret_and_satisfies_the_requirement() -> TestResult {
    let (_root, dir) = project(&[
        (
            "mise.toml",
            "[env]\nSTRIPE_KEY = { required = \"from 1Password\" }\n",
        ),
        ("mise.s.toml", &mise::mise_file_contents("s", "S", 4001)),
    ])?;
    let before = isolated(&dir).inspect(&dir, Some("s"))?;
    let names: Vec<_> = before.missing.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["STRIPE_KEY"]);

    mise::set_space_var(&dir, "s", "STRIPE_KEY", "sk_live_\"42\"")?;
    let after = isolated(&dir).inspect(&dir, Some("s"))?;
    assert!(after.is_complete(), "{after:?}");
    assert_eq!(
        after.secrets.get("STRIPE_KEY").map(String::as_str),
        Some("sk_live_\"42\"")
    );
    assert!(!after.secrets.contains_key("PORT"));
    Ok(())
}

#[test]
fn a_production_space_layers_its_own_mise_file_over_mise_production_toml() -> TestResult {
    let (_root, dir) = project(&[
        (
            "mise.toml",
            "[env]\nSTRIPE_KEY = { required = \"from 1Password\" }\n",
        ),
        (
            "mise.production.toml",
            "[env]\nPORT = \"80\"\nPROD_ONLY = \"yes\"\nPROD_TOKEN = { value = \"prod-s3cr3t\", redact = true }\n",
        ),
        ("mise.s.toml", &mise::mise_file_contents("s", "S", 4001)),
    ])?;
    mise::set_space_var(&dir, "s", "STRIPE_KEY", "sk_live_42")?;
    let space = Space {
        id: SpaceId::new("s"),
        title: "S".into(),
        task: "s".into(),
        shape: SpaceShape::Dev,
        status: SpaceStatus::Active,
        port: 4001,
        created_at: 0,
        repos: Vec::new(),
        production: true,
    };
    let mise_env = space.effective_mise_env();
    assert_eq!(mise_env, "production,s");

    let values = isolated_env_values(&dir, &mise_env)?;
    assert_eq!(values["PROD_ONLY"], "yes", "mise.production.toml is loaded");
    assert_eq!(values["PORT"], "4001", "the space's PORT wins: {values}");
    assert_eq!(values["XCTX_SPACE"], "s");
    assert_eq!(
        values["XCTX_DATA_DIR"],
        mise::space_data_dir("s").to_string_lossy().as_ref()
    );
    assert_eq!(values["STRIPE_KEY"], "sk_live_42");
    let report = isolated(&dir).inspect(&dir, Some(&mise_env))?;
    assert!(report.is_complete(), "{report:?}");
    let names: Vec<_> = report.secrets.keys().map(String::as_str).collect();
    assert_eq!(names, ["PROD_TOKEN", "STRIPE_KEY"]);
    Ok(())
}

#[test]
fn forced_colour_does_not_leak_into_missing_variable_help() -> TestResult {
    let (_root, dir) = project(&[("mise.toml", CONFIG)])?;
    let report = isolated(&dir)
        .with_env("CLICOLOR_FORCE", "1")
        .with_env("FORCE_COLOR", "1")
        .inspect(&dir, None)?;
    let names: Vec<_> = report.missing.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["XCTX_TEST_NEEDED", "XCTX_TEST_NEEDED_TOO"]);
    assert_eq!(
        report.missing[0].help.as_deref(),
        Some("Ask the team for the staging key")
    );
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
