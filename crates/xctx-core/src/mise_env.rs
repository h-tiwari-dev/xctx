//! What mise's environment for a directory still needs, and which of its values are secret.
//!
//! [`MiseRunner::inspect`] runs `mise env --json --redacted` in a directory (with `MISE_ENV` when given). mise
//! stops at the first `required` variable that is not set, naming it, the config file that requires it and its
//! help text; the runner records that variable, gives it a throwaway placeholder for the next run only, and asks
//! again until mise succeeds. The successful run lists every variable mise marks for redaction (`redact = true`
//! or a matching `redactions` pattern), which [`crate::redact::Redactor`] masks.
//!
//! Placeholders exist only in the environment of the `mise` child process; nothing is written to disk.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{Error, Result};
use crate::mise::is_env_var_name;

/// The most missing variables one inspection will step through before giving up, so a misbehaving mise cannot
/// loop forever.
const MAX_MISSING: usize = 64;

/// The value a missing variable is given while looking for the next one. Never shown to a pane.
const PLACEHOLDER: &str = "xctx-placeholder";

/// A variable mise's config marks `required` that is not set.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RequiredVar {
    /// The variable's name.
    pub name: String,
    /// The config's help text for it (`required = "<help>"`), as mise prints it, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// The config file that requires it, as mise reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_in: Option<PathBuf>,
}

/// What [`MiseRunner::inspect`] found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvReport {
    /// Required variables that are not set, in the order mise asked for them.
    pub missing: Vec<RequiredVar>,
    /// Every variable mise redacts, with its value. Missing variables are not in it.
    pub secrets: BTreeMap<String, String>,
}

impl EnvReport {
    /// True when nothing required is missing.
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }
}

/// How to run mise: the program and extra environment for it. The default runs `mise` from `PATH` with the
/// app's own environment.
#[derive(Clone, Debug)]
pub struct MiseRunner {
    program: OsString,
    env: Vec<(OsString, OsString)>,
}

impl Default for MiseRunner {
    fn default() -> Self {
        MiseRunner {
            program: OsString::from("mise"),
            env: Vec::new(),
        }
    }
}

impl MiseRunner {
    /// Run `mise` from `PATH`.
    pub fn new() -> MiseRunner {
        MiseRunner::default()
    }

    /// Run `program` instead of `mise` from `PATH`.
    pub fn with_program(mut self, program: impl Into<OsString>) -> MiseRunner {
        self.program = program.into();
        self
    }

    /// Set `key=value` in mise's environment, for example to isolate it from the user's config in tests.
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> MiseRunner {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Inspect the environment mise builds in `dir`, with `MISE_ENV=<mise_env>` when given (a space's slug, or
    /// `production`). Fails with [`Error::Spawn`] when mise cannot be started and [`Error::Command`] when it
    /// fails for a reason other than a missing required variable (an untrusted config, a syntax error).
    pub fn inspect(&self, dir: &Path, mise_env: Option<&str>) -> Result<EnvReport> {
        let mut missing: Vec<RequiredVar> = Vec::new();
        loop {
            let mut command = Command::new(&self.program);
            command
                .args(["env", "--json", "--redacted"])
                .current_dir(dir)
                .stdin(Stdio::null());
            for (key, value) in &self.env {
                command.env(key, value);
            }
            // Plain stderr: a user's CLICOLOR_FORCE would otherwise wrap "mise ERROR" in ANSI codes, and the
            // missing-variable and boilerplate filters would miss.
            command.env("NO_COLOR", "1").env("MISE_COLOR", "0");
            match mise_env {
                Some(name) => command.env("MISE_ENV", name),
                None => command.env_remove("MISE_ENV"),
            };
            for var in &missing {
                command.env(&var.name, PLACEHOLDER);
            }
            let output = command.output().map_err(|source| Error::Spawn {
                program: self.program_name(),
                source,
            })?;
            if output.status.success() {
                let secrets = parse_secrets(&output.stdout, dir, &missing)?;
                return Ok(EnvReport { missing, secrets });
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            match parse_missing_required(&stderr) {
                Some(var)
                    if missing.len() < MAX_MISSING
                        && !missing.iter().any(|m| m.name == var.name) =>
                {
                    missing.push(var)
                }
                _ => return Err(self.failure(dir, &stderr)),
            }
        }
    }

    fn program_name(&self) -> String {
        Path::new(&self.program)
            .file_name()
            .unwrap_or(OsStr::new("mise"))
            .to_string_lossy()
            .into_owned()
    }

    fn failure(&self, dir: &Path, stderr: &str) -> Error {
        let message: Vec<&str> = stderr
            .lines()
            .map(|l| l.trim_start_matches("mise ERROR").trim())
            .filter(|l| {
                !l.is_empty() && !l.starts_with("Version:") && !l.starts_with("Run with --verbose")
            })
            .collect();
        Error::Command {
            program: self.program_name(),
            args: "env --json --redacted".into(),
            dir: dir.to_path_buf(),
            message: if message.is_empty() {
                "exited unsuccessfully".into()
            } else {
                message.join("\n")
            },
        }
    }
}

fn parse_secrets(
    stdout: &[u8],
    dir: &Path,
    missing: &[RequiredVar],
) -> Result<BTreeMap<String, String>> {
    let map: BTreeMap<String, serde_json::Value> =
        serde_json::from_slice(stdout).map_err(|source| Error::Json {
            path: dir.to_path_buf(),
            source,
        })?;
    Ok(map
        .into_iter()
        .filter(|(name, _)| !missing.iter().any(|m| &m.name == name))
        .filter_map(|(name, value)| match value {
            serde_json::Value::String(s) => Some((name, s)),
            _ => None,
        })
        .collect())
}

/// Read mise's error for a missing required variable, e.g.
///
/// ```text
/// mise ERROR Required environment variable 'API_KEY' is not defined. It must be set before mise runs or in a
/// later config file. (Required in: /code/app/mise.toml)
/// Help: Ask the team for the staging key
/// mise ERROR Version: ...
/// ```
///
/// Returns `None` when `stderr` is about something else, or when the name it gives is not a valid environment
/// variable name (see [`crate::mise::is_env_var_name`]), since it is later set in mise's environment.
pub fn parse_missing_required(stderr: &str) -> Option<RequiredVar> {
    const MARK: &str = "Required environment variable '";
    let start = stderr.find(MARK)? + MARK.len();
    let rest = &stderr[start..];
    let name = &rest[..rest.find('\'')?];
    if !is_env_var_name(name) {
        return None;
    }
    let required_in = rest.find("(Required in: ").and_then(|i| {
        let tail = &rest[i + "(Required in: ".len()..];
        let line = tail.lines().next()?;
        let path = line.strip_suffix(')').unwrap_or(line).trim();
        (!path.is_empty()).then(|| PathBuf::from(path))
    });
    let mut help_lines = Vec::new();
    let mut in_help = false;
    for line in rest.lines() {
        if let Some(text) = line.trim_start().strip_prefix("Help:") {
            in_help = true;
            help_lines.push(text.trim().to_owned());
        } else if in_help {
            if line.starts_with("mise ") {
                break;
            }
            help_lines.push(line.trim().to_owned());
        }
    }
    let help = help_lines.join("\n").trim().to_owned();
    Some(RequiredVar {
        name: name.to_owned(),
        help: (!help.is_empty()).then_some(help),
        required_in,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WITH_HELP: &str = "mise ERROR Required environment variable 'NEEDED' is not defined. It must be set \
        before mise runs or in a later config file. (Required in: /tmp/app/mise.toml)\nHelp: Ask Harsh for the \
        staging key\nmise ERROR Version: 2026.7.1 macos-arm64 (2026-07-07)\nmise ERROR Run with --verbose or \
        MISE_VERBOSE=1 for more information\n";

    #[test]
    fn reads_the_name_file_and_help() {
        assert_eq!(
            parse_missing_required(WITH_HELP),
            Some(RequiredVar {
                name: "NEEDED".into(),
                help: Some("Ask Harsh for the staging key".into()),
                required_in: Some(PathBuf::from("/tmp/app/mise.toml")),
            })
        );
    }

    #[test]
    fn help_is_optional_and_other_errors_are_not_missing_vars() {
        let bare = "mise ERROR Required environment variable 'X' is not defined. (Required in: /a/mise.toml)\n\
                    mise ERROR Version: 1\n";
        assert_eq!(
            parse_missing_required(bare).map(|v| (v.name, v.help)),
            Some(("X".into(), None))
        );
        assert_eq!(
            parse_missing_required("mise ERROR Config files in /a are not trusted."),
            None
        );
    }

    #[test]
    fn names_that_are_not_env_var_names_are_not_missing_vars() {
        for name in ["A=B", "A B", "1A", "A\0B", ""] {
            let stderr = format!(
                "mise ERROR Required environment variable '{name}' is not defined. (Required in: /a/mise.toml)\n"
            );
            assert_eq!(parse_missing_required(&stderr), None, "{name:?}");
        }
    }

    #[test]
    fn failure_messages_drop_mise_boilerplate() {
        let err = MiseRunner::new().failure(
            Path::new("/a"),
            "mise ERROR not trusted\nmise ERROR Version: 1\n",
        );
        assert!(
            matches!(err, Error::Command { ref message, .. } if message == "not trusted"),
            "{err:?}"
        );
    }
}
