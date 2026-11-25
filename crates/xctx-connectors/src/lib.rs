//! The built-in plugin tools that need no window: `logs.query`, `sql.read`, `sql.write`, and the Jira
//! (`issue.read`, `issue.comment`) and Figma (`frame.read`, `frame.export`) connectors.
//!
//! The app plans a call with `xctx_plugin::plan_call`; when the plan is an `Invocation::Builtin` whose name is
//! one of [`BUILTINS`], it builds a [`BuiltinContext`] for the space and calls [`run_builtin`] from a background
//! thread (everything here blocks). `http.*` and `page.*` built-ins live in `xctx-http` and `xctx-browser`.
//!
//! **Writes.** `sql.write` and `issue.comment` are write tools: they run only when
//! [`BuiltinContext::approved`] is true. The app sets it only after the user approved the call in an Inbox
//! gate; nothing on the control socket may set it.
//!
//! **Masking.** The returned text, JSON, event details and error messages are masked with the context's
//! redactor and with the tokens the connectors used, so they can be shown as they are.

#![warn(missing_docs)]

mod args;
mod error;
mod figma;
mod jira;
mod logs;
mod rest;
mod sql;

#[cfg(test)]
mod mock;

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Map, Value};
use xctx_core::Redactor;

pub use error::{ConnectorError, Result};
pub use figma::{DEFAULT_FIGMA_API_URL, FIGMA_IMAGE_HOSTS};
pub use logs::{
    DEFAULT_LOG_CAPACITY, LOGS_ERROR, Level, LogBuffer, LogLine, LogSource, detect_level,
};
pub use rest::flatten_adf;
pub use sql::{Database, SqlDialect, check_read_statement, check_write_statement};
pub use xctx_plugin::SqlEngine;

/// The built-in names [`run_builtin`] runs, as the built-in manifests name them. The short tool names
/// (`issue.read`, `issue.comment`, `frame.read`, `frame.export`) are accepted too.
pub const BUILTINS: &[&str] = &[
    "logs.query",
    "sql.read",
    "sql.write",
    "jira.issue.read",
    "jira.issue.comment",
    "figma.frame.read",
    "figma.frame.export",
];

/// Settings a manifest may list under `secrets` so the app passes them, but which are not masked by this crate
/// (the app's redactor may still mask them): they say where a service is, not how to get in.
pub const PLAIN_SETTINGS: &[&str] = &["JIRA_BASE_URL", "JIRA_EMAIL", "FIGMA_API_URL"];

/// The timeout [`BuiltinContext::new`] uses.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The output cap [`BuiltinContext::new`] uses: 256 KiB.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// The space a built-in call runs for, and what the app mounted for it.
#[derive(Clone, Debug)]
pub struct BuiltinContext {
    /// The space's worktree. Relative paths (log files, sqlite databases) are resolved against it and may not
    /// leave it.
    pub worktree: PathBuf,
    /// The space's slug.
    pub space: String,
    /// The space's port, if it has one.
    pub port: Option<u16>,
    /// The call's environment, such as the plan's `env` (`MISE_ENV`, `PORT`, `XCTX_SPACE`). Settings that are
    /// not secret (`JIRA_BASE_URL`, `JIRA_EMAIL`, `FIGMA_API_URL`, `DATABASE_URL`) are looked up here when they
    /// are not in [`BuiltinContext::secrets`].
    pub env: BTreeMap<String, String>,
    /// The secrets the manifest declares, by env name: `JIRA_API_TOKEN`, `FIGMA_TOKEN`, and so on.
    pub secrets: BTreeMap<String, String>,
    /// Masks the space's secrets in everything returned.
    pub redactor: Redactor,
    /// The longest a command or a request may take.
    pub timeout: Duration,
    /// The most bytes of text returned, and of a command's output read.
    pub max_output_bytes: usize,
    /// Hosts the connectors may reach besides the host of their configured base URL (for example a Figma image
    /// host not in [`FIGMA_IMAGE_HOSTS`]).
    pub allowed_hosts: Vec<String>,
    /// True only when the user approved this call in the Inbox. Write built-ins refuse to run without it.
    pub approved: bool,
    /// Where `logs.query` reads: the logs pane's buffer or file.
    pub logs: Option<LogSource>,
    /// Which database `sql.read` and `sql.write` use: the sql pane's engine and database.
    pub database: Option<Database>,
    /// A folder `frame.export` downloads the image into (the space's artifacts). Without it, only the URL is
    /// returned.
    pub scratch_dir: Option<PathBuf>,
}

impl BuiltinContext {
    /// A context for a space whose worktree is `worktree`, with the default timeout and output cap, nothing
    /// mounted, no secrets, and not approved.
    pub fn new(worktree: impl Into<PathBuf>) -> BuiltinContext {
        BuiltinContext {
            worktree: worktree.into(),
            space: String::new(),
            port: None,
            env: BTreeMap::new(),
            secrets: BTreeMap::new(),
            redactor: Redactor::default(),
            timeout: DEFAULT_TIMEOUT,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            allowed_hosts: Vec::new(),
            approved: false,
            logs: None,
            database: None,
            scratch_dir: None,
        }
    }

    /// A setting from the secrets, else from the env; None when absent or blank.
    pub(crate) fn setting(&self, name: &str) -> Option<&str> {
        self.secrets
            .get(name)
            .or_else(|| self.env.get(name))
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
    }

    /// `path` resolved against the worktree. A relative path that leaves the worktree (through `..` or a
    /// symlink) is refused; an absolute path is taken as given, since the user or the app chose it.
    pub(crate) fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        let given = Path::new(path);
        if path.trim().is_empty() {
            return Err(ConnectorError::BadArgs("the path is empty".to_owned()));
        }
        if given.is_absolute() {
            return Ok(given.to_path_buf());
        }
        if given
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(ConnectorError::Refused(format!(
                "{path} leaves the worktree; give an absolute path to use a file outside it"
            )));
        }
        let joined = self.worktree.join(given);
        if let (Ok(real), Ok(root)) = (joined.canonicalize(), self.worktree.canonicalize())
            && !real.starts_with(&root)
        {
            return Err(ConnectorError::Refused(format!(
                "{path} points outside the worktree; give an absolute path to use a file outside it"
            )));
        }
        Ok(joined)
    }
}

/// An event a built-in raised, such as `logs.error`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BuiltinEvent {
    /// The event's name, as the manifest's `[[events]]` names it.
    pub name: String,
    /// What happened, masked, for the Inbox and the shared space.
    pub detail: String,
}

/// What a built-in call produced.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BuiltinOutput {
    /// The structured result, masked; its shape is documented per built-in.
    pub json: Value,
    /// A plain-text rendering for agents and panes, masked, at most [`BuiltinContext::max_output_bytes`].
    pub text: String,
    /// Events raised by the call.
    pub events: Vec<BuiltinEvent>,
}

/// Run the built-in `name` with `args` (already checked against the manifest by `plan_call`, and checked again
/// here) for the space in `ctx`.
///
/// - `logs.query` (`pattern`, `level`, `since_secs`, `limit`, `error_threshold`): lines from
///   [`BuiltinContext::logs`]; raises `logs.error` when more than `error_threshold` error lines fall in the
///   window.
/// - `sql.read` (`query`, `limit`): one read statement through `sqlite3 -readonly -safe` or `psql` with
///   `default_transaction_read_only=on`; JSON `{engine, columns, rows, row_count, truncated}`.
/// - `sql.write` (`statement`, `reason`): one statement, without the read-only flags, only when approved.
/// - `jira.issue.read` (`key`) and `jira.issue.comment` (`key`, `body`, only when approved), with
///   `JIRA_BASE_URL`, `JIRA_EMAIL` and `JIRA_API_TOKEN`.
/// - `figma.frame.read` and `figma.frame.export` (`file`, `node`, `format`, `scale`), with `FIGMA_TOKEN`.
///
/// Fails with [`ConnectorError::UnknownBuiltin`] for any other name. Errors are masked like the output.
pub fn run_builtin(
    name: &str,
    args: &Map<String, Value>,
    ctx: &BuiltinContext,
) -> Result<BuiltinOutput> {
    let mut masker = ctx.redactor.clone();
    let result = dispatch(name, args, ctx, &mut masker);
    match result {
        Ok(mut out) => {
            masker.redact_json(&mut out.json);
            out.text = cap_text(masker.redact(&out.text).into_owned(), ctx.max_output_bytes);
            for event in &mut out.events {
                event.detail = masker.redact(&event.detail).into_owned();
            }
            Ok(out)
        }
        Err(e) => Err(e.map_text(|s| masker.redact(s).into_owned())),
    }
}

/// True when `name` is a built-in [`run_builtin`] runs.
pub fn is_builtin(name: &str) -> bool {
    canonical(name).is_some()
}

fn canonical(name: &str) -> Option<&'static str> {
    let full = match name {
        "issue.read" => "jira.issue.read",
        "issue.comment" => "jira.issue.comment",
        "frame.read" => "figma.frame.read",
        "frame.export" => "figma.frame.export",
        other => other,
    };
    BUILTINS.iter().copied().find(|b| *b == full)
}

fn dispatch(
    name: &str,
    args: &Map<String, Value>,
    ctx: &BuiltinContext,
    masker: &mut Redactor,
) -> Result<BuiltinOutput> {
    // Mask every declared secret whatever the app's redactor holds, except the settings that only say where a
    // service is.
    masker.extend(
        ctx.secrets
            .iter()
            .filter(|(k, _)| !PLAIN_SETTINGS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    let Some(name) = canonical(name) else {
        return Err(ConnectorError::UnknownBuiltin(name.to_owned()));
    };
    match name {
        "logs.query" => logs::query(args, ctx),
        "sql.read" => sql::read(args, ctx, masker),
        "sql.write" => {
            require_approval(name, ctx)?;
            sql::write(args, ctx, masker)
        }
        "jira.issue.read" => jira::issue_read(args, ctx, masker),
        "jira.issue.comment" => {
            require_approval(name, ctx)?;
            jira::issue_comment(args, ctx, masker)
        }
        "figma.frame.read" => figma::frame_read(args, ctx, masker),
        "figma.frame.export" => figma::frame_export(args, ctx, masker),
        other => Err(ConnectorError::UnknownBuiltin(other.to_owned())),
    }
}

fn require_approval(name: &str, ctx: &BuiltinContext) -> Result<()> {
    if ctx.approved {
        Ok(())
    } else {
        Err(ConnectorError::Refused(format!(
            "{name} changes data and runs only after the user approves it in the Inbox"
        )))
    }
}

/// `text` cut to at most `cap` bytes on a character boundary, with a note when cut.
pub(crate) fn cap_text(mut text: String, cap: usize) -> String {
    const NOTE: &str = "\n[output truncated]";
    if text.len() <= cap {
        return text;
    }
    let mut end = cap.saturating_sub(NOTE.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    if cap >= NOTE.len() {
        text.push_str(NOTE);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_builtins_are_refused_by_name() {
        let ctx = BuiltinContext::new("/nonexistent");
        for name in ["http.send", "page.open", "nope"] {
            match run_builtin(name, &Map::new(), &ctx) {
                Err(ConnectorError::UnknownBuiltin(n)) => assert_eq!(n, name),
                other => panic!("{name}: {other:?}"),
            }
        }
        assert!(is_builtin("issue.read") && is_builtin("figma.frame.export"));
        assert!(!is_builtin("http.send"));
    }

    #[test]
    fn writes_need_approval() {
        let ctx = BuiltinContext::new("/nonexistent");
        for name in ["sql.write", "issue.comment"] {
            match run_builtin(name, &Map::new(), &ctx) {
                Err(ConnectorError::Refused(m)) => assert!(m.contains("Inbox"), "{m}"),
                other => panic!("{name}: {other:?}"),
            }
        }
    }

    #[test]
    fn relative_paths_stay_in_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = BuiltinContext::new(dir.path());
        assert!(ctx.resolve_path("../x").is_err());
        assert_eq!(ctx.resolve_path("a/b").unwrap(), dir.path().join("a/b"));
        assert_eq!(
            ctx.resolve_path("/etc/hosts").unwrap(),
            Path::new("/etc/hosts")
        );
        std::os::unix::fs::symlink("/etc", dir.path().join("out")).unwrap();
        assert!(matches!(
            ctx.resolve_path("out/hosts"),
            Err(ConnectorError::Refused(_))
        ));
    }

    #[test]
    fn text_is_capped_on_a_char_boundary() {
        let t = cap_text("é".repeat(100), 40);
        assert!(t.len() <= 40 && t.ends_with("[output truncated]"));
        assert_eq!(cap_text("short".to_owned(), 40), "short");
    }
}
