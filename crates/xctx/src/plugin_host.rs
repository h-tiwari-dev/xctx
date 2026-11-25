//! Plugins in the app, without a window: which plugins a space can use (built-in, the user's, and its repos'
//! once trusted), where they are mounted, what a tool call runs and with what, and what a plugin event does to
//! the space's loop, shared space and Inbox.
//!
//! Everything here is GPUI-free so it is tested directly (see `plugin_tests.rs`); `plugin_jobs.rs` applies it to
//! the live workspace, running [`CallJob`]s on the background executor and the results on the UI thread.
//!
//! On disk, under the data directory:
//! - `plugin-trust.json`: the repo plugins the user trusted, each by repo, plugin name and the SHA-256 of the
//!   `plugin.toml` bytes the registry parsed, so an edited manifest asks again (trust covers the manifest, not the
//!   scripts it runs);
//! - `spaces/<slug>/plugins.json`: the plugins mounted in a space, one entry per plugin pane, with the settings
//!   chosen when mounting (a logs command, a database file, a base URL);
//! - `spaces/<slug>/http.json`: the API client's history (see `xctx_http`).
//!
//! The browser pane's page keeps nothing on disk: each space's live browser has its own non-persistent website
//! data store (see `xctx_web_view`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use xctx_browser::{ConsoleEntry, NetworkError, UrlPolicy};
use xctx_channel::{FindingSeverity, ScratchDir};
use xctx_connectors::{BuiltinContext, Database, LogBuffer, LogLine, LogSource};
use xctx_control::RpcError;
use xctx_control::protocol::{
    PluginEventAction, PluginSource, PluginSummary, PluginToolParam, PluginToolSpec,
    PluginToolSummary, PluginToolsResult, ToolEffect,
};
use xctx_core::{Redactor, Space, SpaceId};
use xctx_graph::{LoopDecision, LoopError, LoopRun, LoopStatus};
use xctx_http::{EventRules, Exchange, ExchangeDiff, HttpHistory, Request, SendOptions};
use xctx_inbox::{Inbox, ItemBody, ItemId, Level, Notice, PluginGate, PluginGateKind, Source};
use xctx_layout::{PaneId, PaneKind, RolePane};
use xctx_plugin::{
    CallContext, CallPlan, Effect, FiredEvent, Invocation, LoopAction, Manifest, PaneSpec,
    ParamKind, Plugin, Registry, SqlEngine, fill_context, loop_outcome, match_events, pane_argv,
    plan_call, run_plan_isolated,
};

use crate::plugin_backend::{LogTail, TailEvent, TailStatus, TailTarget};
use crate::space_graph::{SpaceGraphState, loop_approval_notice};

/// The env var that moves the user's plugins folder (tests and throwaway runs); `~/.config/xctx/plugins`
/// otherwise.
pub const PLUGIN_DIR_ENV: &str = "XCTX_PLUGIN_DIR";

/// The env var that moves the user's runners folder; `~/.config/xctx/runners` otherwise.
pub const RUNNER_DIR_ENV: &str = "XCTX_RUNNER_DIR";

/// The file under the data directory that records trusted repo plugins.
pub const TRUST_FILE: &str = "plugin-trust.json";

/// The file under a space's data directory that records its mounted plugins.
pub const MOUNTS_FILE: &str = "plugins.json";

/// The version of [`TRUST_FILE`] and [`MOUNTS_FILE`] this build writes.
pub const PLUGIN_FILES_VERSION: u32 = 1;

/// How long a tool run (`run = [...]`) may take before it is killed.
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// The most bytes of a tool's stdout (and of its stderr) kept.
pub const TOOL_MAX_OUTPUT: usize = 256 * 1024;

/// The same event, with the same detail, from the same plugin in the same space, fires once in this window.
pub const EVENT_DEBOUNCE: Duration = Duration::from_secs(5);

/// The most characters of an event's detail kept in a finding or a notice.
pub const MAX_EVENT_DETAIL_CHARS: usize = 1000;

/// The most characters of a write call's masked arguments an Inbox gate shows. A call whose arguments are longer is
/// refused rather than gated, so the user always sees everything that would run.
pub const MAX_GATE_ARGS_CHARS: usize = 4000;

/// The most distinct events one tool call records as findings; the rest are counted in the summary only.
pub const MAX_EVENTS_PER_CALL: usize = 20;

/// The same plugin's same event posts at most one Inbox notice per space in this window, unless it newly stops
/// the loop or the loop waits for approval, which always post.
pub const NOTICE_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// The variables of xctx's own environment a tool or a tailed command is given; everything else (tokens, the
/// control socket, the user's other secrets) is left out. `LC_*` is allowed as a prefix.
pub const TOOL_ENV_ALLOWLIST: &[&str] =
    &["HOME", "USER", "LOGNAME", "SHELL", "TMPDIR", "LANG", "TERM"];

/// The `PATH` a tool gets when the app knows no better one.
pub const DEFAULT_TOOL_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// How many lines of a logs pane a space keeps.
pub const LOG_CAPACITY: usize = xctx_connectors::DEFAULT_LOG_CAPACITY;

/// Who findings from plugin events are by: `plugin:<name>`.
pub fn event_author(plugin: &str) -> String {
    format!("plugin:{plugin}")
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The whole environment a tool or a tailed command starts with: the allowed variables of `base` (xctx's own
/// environment, see [`TOOL_ENV_ALLOWLIST`]), then `path` as `PATH` (the panes' `PATH`; [`DEFAULT_TOOL_PATH`]
/// without one), then `extra` (the plan's `MISE_ENV`, `PORT`, `XCTX_SPACE` and declared secrets), which wins.
pub fn isolated_env(
    base: impl IntoIterator<Item = (String, String)>,
    path: Option<String>,
    extra: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = base
        .into_iter()
        .filter(|(k, _)| TOOL_ENV_ALLOWLIST.contains(&k.as_str()) || k.starts_with("LC_"))
        .collect();
    env.insert(
        "PATH".to_owned(),
        path.filter(|p| !p.is_empty())
            .unwrap_or_else(|| DEFAULT_TOOL_PATH.to_owned()),
    );
    env.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
    env
}

/// xctx's own environment, as text (variables that are not valid UTF-8 are left out).
fn process_env() -> impl Iterator<Item = (String, String)> {
    std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
}

// -------------------------------------------------------------------------------------------------------------
// Folders

/// The folder `<home>/.config/xctx/<leaf>`, or `override_dir` when it is set and not empty. `None` without
/// either.
pub fn config_dir_in(
    override_dir: Option<OsString>,
    home: Option<OsString>,
    leaf: &str,
) -> Option<PathBuf> {
    if let Some(dir) = override_dir.filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    home.filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".config").join("xctx").join(leaf))
}

/// The user's plugins folder: `$XCTX_PLUGIN_DIR`, else `~/.config/xctx/plugins`.
pub fn user_plugin_dir() -> Option<PathBuf> {
    config_dir_in(
        std::env::var_os(PLUGIN_DIR_ENV),
        std::env::var_os("HOME"),
        "plugins",
    )
}

/// The user's runners folder: `$XCTX_RUNNER_DIR`, else `~/.config/xctx/runners`.
pub fn user_runner_dir() -> Option<PathBuf> {
    config_dir_in(
        std::env::var_os(RUNNER_DIR_ENV),
        std::env::var_os("HOME"),
        "runners",
    )
}

/// The runner registry: the built-in runners and the user's (see [`user_runner_dir`]).
pub fn load_runners(dir: Option<&Path>) -> xctx_plugin::RunnerRegistry {
    let mut registry = xctx_plugin::RunnerRegistry::with_builtins();
    if let Some(dir) = dir {
        registry.add_user_dir(dir);
    }
    registry
}

fn check_slug(slug: &str) -> Result<(), String> {
    let valid = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(format!("{slug:?} is not a valid space slug"))
    }
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| format!("could not encode {}: {e}", path.display()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("could not write {}: {e}", path.display()))
}

// -------------------------------------------------------------------------------------------------------------
// Trust

/// The SHA-256 of a manifest's bytes, in hex: what trusting a repo plugin records. The registry hashes the bytes
/// it parsed ([`Plugin::manifest_hash`]); use that, not a second read of the file.
#[cfg(test)]
pub fn manifest_hash(bytes: &[u8]) -> String {
    xctx_plugin::manifest_hash(bytes)
}

/// One trusted repo plugin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustEntry {
    /// The repo's checkout (not a space's worktree), so trust holds in every space of the repo.
    pub repo: PathBuf,
    /// The plugin's name.
    pub plugin: String,
    /// The SHA-256 of the manifest bytes the user trusted ([`Plugin::manifest_hash`]).
    pub hash: String,
}

#[derive(Serialize, Deserialize)]
struct TrustDoc {
    version: u32,
    #[serde(default)]
    trusted: Vec<TrustEntry>,
}

/// The repo plugins the user trusted, kept in [`TRUST_FILE`].
#[derive(Clone, Debug, Default)]
pub struct TrustStore {
    path: Option<PathBuf>,
    entries: Vec<TrustEntry>,
}

impl TrustStore {
    /// The trust recorded in `data_dir`, with why it could not be read (it then starts empty, and is not
    /// overwritten until something is trusted). Without a data directory nothing can be trusted.
    pub fn load(data_dir: Option<&Path>) -> (TrustStore, Option<String>) {
        let Some(path) = data_dir.map(|d| d.join(TRUST_FILE)) else {
            return (TrustStore::default(), None);
        };
        let entries = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<TrustDoc>(&bytes) {
                Ok(doc) if doc.version <= PLUGIN_FILES_VERSION => Ok(doc.trusted),
                Ok(doc) => Err(format!(
                    "{} is from a newer xctx (version {})",
                    path.display(),
                    doc.version
                )),
                Err(e) => Err(format!("{} is damaged: {e}", path.display())),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("could not read {}: {e}", path.display())),
        };
        match entries {
            Ok(entries) => (
                TrustStore {
                    path: Some(path),
                    entries,
                },
                None,
            ),
            Err(e) => (
                TrustStore {
                    path: Some(path),
                    entries: Vec::new(),
                },
                Some(e),
            ),
        }
    }

    /// Whether `plugin` of the repo checked out at `repo`, whose manifest hashes to `hash`, is trusted.
    pub fn is_trusted(&self, repo: &Path, plugin: &str, hash: &str) -> bool {
        self.entries
            .iter()
            .any(|e| e.repo == repo && e.plugin == plugin && e.hash == hash)
    }

    /// Trust `plugin` of `repo` as its manifest hashes now, replacing what was trusted for it before, and save.
    pub fn trust(&mut self, repo: &Path, plugin: &str, hash: &str) -> Result<(), String> {
        let Some(path) = self.path.clone() else {
            return Err("there is no data directory to record trust in".to_owned());
        };
        let mut entries = self.entries.clone();
        entries.retain(|e| !(e.repo == repo && e.plugin == plugin));
        entries.push(TrustEntry {
            repo: repo.to_path_buf(),
            plugin: plugin.to_owned(),
            hash: hash.to_owned(),
        });
        write_json_atomic(
            &path,
            &TrustDoc {
                version: PLUGIN_FILES_VERSION,
                trusted: entries.clone(),
            },
        )?;
        self.entries = entries;
        Ok(())
    }
}

// -------------------------------------------------------------------------------------------------------------
// Mounts

/// Settings chosen when mounting, overriding the manifest's pane settings. All optional.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountSettings {
    /// A terminal pane's command, or a logs pane's command to tail (argv, no shell; may use `{PORT}`,
    /// `{WORKTREE}`, `{SPACE}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// A logs pane's file to tail, relative to the worktree unless absolute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_file: Option<String>,
    /// A sql pane's engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<SqlEngine>,
    /// A sql pane's database: a sqlite file or a postgres URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// An http pane's base URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// A web pane's page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// A plugin mounted in a space: one pane of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    /// The plugin's pane.
    pub pane: PaneId,
    /// The plugin's name.
    pub plugin: String,
    /// The space repo its pane and tools work in.
    pub repo: String,
    /// What was chosen when mounting.
    #[serde(default)]
    pub settings: MountSettings,
    /// True when the user made the mount from the Plugins menu, so its paths (an absolute log file or database)
    /// are the user's choice. False for an agent's `plugin.mount`, for panes found in a layout, and for mounts
    /// saved before this was recorded: their paths must stay in the worktree.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub user_paths: bool,
}

impl MountSettings {
    /// The settings as they may be shown: URLs and the database with credentials masked
    /// ([`xctx_http::mask_url`]), everything masked with `redactor`.
    pub fn masked(&self, redactor: &Redactor) -> MountSettings {
        let url = |u: &Option<String>| u.as_deref().map(|u| xctx_http::mask_url(u, redactor));
        let text = |t: &str| redactor.redact(t).into_owned();
        MountSettings {
            command: self
                .command
                .as_ref()
                .map(|argv| argv.iter().map(|a| text(a)).collect()),
            log_file: self.log_file.as_deref().map(text),
            engine: self.engine,
            database: url(&self.database),
            base_url: url(&self.base_url),
            url: url(&self.url),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct MountsDoc {
    version: u32,
    #[serde(default)]
    mounts: Vec<Mount>,
}

/// Where space `slug` records its mounted plugins: `<data dir>/spaces/<slug>/plugins.json`.
pub fn mounts_path(data_dir: &Path, slug: &str) -> Result<PathBuf, String> {
    check_slug(slug)?;
    Ok(data_dir.join("spaces").join(slug).join(MOUNTS_FILE))
}

/// The plugins mounted in space `slug`; none when it never mounted one. A damaged or newer file is an error.
pub fn load_mounts(data_dir: &Path, slug: &str) -> Result<Vec<Mount>, String> {
    let path = mounts_path(data_dir, slug)?;
    match std::fs::read(&path) {
        Ok(bytes) => {
            let doc: MountsDoc = serde_json::from_slice(&bytes)
                .map_err(|e| format!("{} is damaged: {e}", path.display()))?;
            if doc.version > PLUGIN_FILES_VERSION {
                return Err(format!(
                    "{} is from a newer xctx (version {})",
                    path.display(),
                    doc.version
                ));
            }
            Ok(doc.mounts)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("could not read {}: {e}", path.display())),
    }
}

/// Record the plugins mounted in space `slug`.
pub fn save_mounts(data_dir: &Path, slug: &str, mounts: &[Mount]) -> Result<(), String> {
    let path = mounts_path(data_dir, slug)?;
    write_json_atomic(
        &path,
        &MountsDoc {
            version: PLUGIN_FILES_VERSION,
            mounts: mounts.to_vec(),
        },
    )
}

/// What a space's plugins keep in `data_dir` that goes when the space is shipped or discarded: its
/// `plugins.json`. Empty for a slug that is not valid.
pub fn space_plugin_files(data_dir: &Path, slug: &str) -> Vec<PathBuf> {
    if check_slug(slug).is_err() {
        return Vec::new();
    }
    vec![data_dir.join("spaces").join(slug).join(MOUNTS_FILE)]
}

/// Remove `paths` (files or folders) that exist, saying which could not be removed.
pub fn remove_paths(paths: &[PathBuf]) -> Result<(), String> {
    let mut failed = Vec::new();
    for path in paths {
        let result = match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
            Ok(_) => std::fs::remove_file(path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            failed.push(format!("{}: {e}", path.display()));
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("could not remove {}", failed.join("; ")))
    }
}

/// `mounts` in step with a layout's `panes`: entries whose pane is gone (or is no longer that plugin's) are
/// dropped, and each plugin pane without an entry gets one with no settings. Returns whether anything changed.
pub fn reconcile_mounts(mounts: &mut Vec<Mount>, panes: &[RolePane]) -> bool {
    let before = mounts.clone();
    mounts.retain(|m| {
        panes.iter().any(|p| {
            p.id == m.pane
                && p.role.kind == PaneKind::Plugin
                && p.role.plugin.as_deref() == Some(m.plugin.as_str())
        })
    });
    for pane in panes.iter().filter(|p| p.role.kind == PaneKind::Plugin) {
        let Some(plugin) = pane.role.plugin.clone() else {
            continue;
        };
        if !mounts.iter().any(|m| m.pane == pane.id) {
            mounts.push(Mount {
                pane: pane.id,
                plugin,
                repo: pane.role.repo.clone(),
                settings: MountSettings::default(),
                user_paths: false,
            });
        }
    }
    *mounts != before
}

// -------------------------------------------------------------------------------------------------------------
// Registry

/// The plugins space `space` can use: the built-in ones, the user's (from `user_dir`), and those of each of its
/// repos (read from the repo's worktree), a repo plugin trusted only when `trust` holds its manifest's hash.
pub fn space_registry(user_dir: Option<&Path>, space: &Space, trust: &TrustStore) -> Registry {
    let mut registry = Registry::with_builtins();
    if let Some(dir) = user_dir {
        registry.add_user_dir(dir);
    }
    for repo in &space.repos {
        registry.add_repo(&repo.worktree, |manifest, hash| {
            trust.is_trusted(&repo.repo, &manifest.name, hash)
        });
    }
    registry
}

/// The space repo whose worktree a repo plugin came from, if it is one.
pub fn plugin_repo<'a>(space: &'a Space, plugin: &Plugin) -> Option<&'a xctx_core::SpaceRepo> {
    match &plugin.source {
        xctx_plugin::Source::Repo { repo, .. } => space.repos.iter().find(|r| &r.worktree == repo),
        _ => None,
    }
}

/// What every registry load problem of `registry` says, for the status bar.
pub fn problem_messages(registry: &Registry) -> Vec<String> {
    registry
        .problems()
        .iter()
        .map(|p| match &p.path {
            Some(path) => format!("plugin {}: {}", path.display(), p.error),
            None => format!("plugin: {}", p.error),
        })
        .collect()
}

/// Whether mounting `manifest` needs the user's approval in the Inbox: a connector (a plugin reaching an
/// outside service with secrets) that has write tools. Returns why.
pub fn mount_gate_reason(manifest: &Manifest) -> Option<String> {
    if manifest.secrets.is_empty() || !manifest.has_write_tools() {
        return None;
    }
    let writes: Vec<&str> = manifest
        .tools
        .iter()
        .filter(|t| t.effect == Effect::Write)
        .map(|t| t.name.as_str())
        .collect();
    Some(format!(
        "{} reaches an outside service with {} and can change it ({}); each write still asks in the Inbox",
        manifest.name,
        manifest.secrets.join(", "),
        writes.join(", ")
    ))
}

// -------------------------------------------------------------------------------------------------------------
// Calls

/// The secrets of `all` that `manifest` declares, by name.
pub fn declared_secrets(
    manifest: &Manifest,
    all: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    manifest
        .secrets
        .iter()
        .filter_map(|name| all.get(name).map(|v| (name.clone(), v.clone())))
        .collect()
}

/// The worktree of space repo `repo`, else of the space's first repo.
pub fn repo_worktree(space: &Space, repo: &str) -> PathBuf {
    space
        .repos
        .iter()
        .find(|r| r.name == repo)
        .or_else(|| space.repos.first())
        .map(|r| r.worktree.clone())
        .unwrap_or_default()
}

/// The values of the settings `manifest` lists under `secrets` that are not secret (see
/// [`xctx_connectors::PLAIN_SETTINGS`]: where a service is, such as `JIRA_BASE_URL`, not how to get in) and that
/// mise did not report among `known` (mise only reports the values it redacts): each looked up with `lookup`
/// (the app's process environment), when set and not empty. Secrets are never looked up this way: they come
/// from mise only, and stay masked.
pub fn plain_settings(
    manifest: &Manifest,
    known: &BTreeMap<String, String>,
    lookup: impl Fn(&str) -> Option<String>,
) -> BTreeMap<String, String> {
    manifest
        .secrets
        .iter()
        .filter(|name| xctx_connectors::PLAIN_SETTINGS.contains(&name.as_str()))
        .filter(|name| !known.contains_key(name.as_str()))
        .filter_map(|name| {
            lookup(name)
                .filter(|v| !v.is_empty())
                .map(|v| (name.clone(), v))
        })
        .collect()
}

/// The context a call of `manifest`'s tools runs in for `space`, working in `repo`: its worktree, `PORT`,
/// effective `MISE_ENV` and production flag, the space's secrets the manifest declares, its plain settings
/// (see [`plain_settings`], from the process environment when mise did not report them), and `redactor`.
pub fn call_context(
    space: &Space,
    repo: &str,
    secrets: &BTreeMap<String, String>,
    redactor: &Redactor,
    manifest: &Manifest,
) -> CallContext {
    CallContext {
        worktree: repo_worktree(space, repo),
        space: space.id.as_str().to_owned(),
        port: Some(space.port),
        mise_env: space.effective_mise_env(),
        space_production: space.production,
        secrets: {
            let mut declared = declared_secrets(manifest, secrets);
            declared.extend(plain_settings(manifest, secrets, |name| {
                std::env::var(name).ok()
            }));
            declared
        },
        redactor: redactor.clone(),
    }
}

/// A write call's arguments as its Inbox gate shows them: compact JSON, masked. Refused when longer than
/// [`MAX_GATE_ARGS_CHARS`]: the gate must show everything that would run, so the agent is told to send a smaller
/// call.
pub fn masked_args_summary(
    args: &Map<String, Value>,
    redactor: &Redactor,
) -> Result<String, String> {
    let mut value = Value::Object(args.clone());
    redactor.redact_json(&mut value);
    let text = value.to_string();
    let len = text.chars().count();
    if len > MAX_GATE_ARGS_CHARS {
        return Err(format!(
            "the arguments of this write are {len} characters, more than the {MAX_GATE_ARGS_CHARS} its Inbox request can show; send a smaller call (split the change into several writes)"
        ));
    }
    Ok(text)
}

/// `text` cut to `max` characters, with an ellipsis when cut.
pub fn cut_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// `path` on `base`: a full `http(s)://` URL is taken as given; anything else is joined to `base` with one `/`.
pub fn join_url(base: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_owned();
    }
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// What a pane's settings resolve to for one mount: the manifest's, overridden by the mount's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneSettings {
    /// An http pane's base URL, filled.
    pub base_url: Option<String>,
    /// A web pane's page, filled.
    pub url: Option<String>,
    /// A sql pane's database.
    pub database: Option<Database>,
    /// A logs pane's command, filled.
    pub logs_command: Option<Vec<String>>,
    /// A logs pane's file.
    pub logs_file: Option<String>,
    /// A terminal pane's command, filled; `None` runs the user's shell.
    pub terminal: Option<Vec<String>>,
    /// A files pane's worktree: the mounted repo's, never the user's main checkout.
    pub files_root: Option<PathBuf>,
}

/// Resolve `mount`'s pane settings for `manifest` in `ctx`. A setting that cannot be filled (an unknown
/// placeholder) is an error.
pub fn pane_settings(
    manifest: &Manifest,
    mount: &Mount,
    ctx: &CallContext,
) -> Result<PaneSettings, String> {
    let fill = |text: &str| fill_context(text, ctx).map_err(|e| e.to_string());
    let fill_argv = |argv: &[String]| argv.iter().map(|a| fill(a)).collect::<Result<Vec<_>, _>>();
    let s = &mount.settings;
    let mut out = PaneSettings::default();
    match &manifest.pane {
        PaneSpec::Http { base_url } => {
            out.base_url = match s.base_url.as_deref().or(base_url.as_deref()) {
                Some(url) => Some(fill(url)?),
                None => None,
            };
        }
        PaneSpec::Web { url } => {
            out.url = match s.url.as_deref().or(url.as_deref()) {
                Some(url) => Some(fill(url)?),
                None => None,
            };
        }
        PaneSpec::Sql { engine, database } => {
            let engine = s.engine.unwrap_or(*engine);
            let location = fill(s.database.as_deref().unwrap_or(database))?;
            out.database = Some(Database { engine, location });
        }
        PaneSpec::Logs { file, .. } => {
            if let Some(command) = s.command.as_deref() {
                out.logs_command = Some(fill_argv(command)?);
            } else if let Some(file) = s.log_file.clone() {
                out.logs_file = Some(file);
            } else if let Some(argv) = pane_argv(&manifest.pane, ctx).map_err(|e| e.to_string())? {
                out.logs_command = Some(argv);
            } else {
                out.logs_file = file.clone();
            }
        }
        PaneSpec::Terminal { .. } => {
            out.terminal = match s.command.as_deref() {
                Some(command) if !command.is_empty() => Some(fill_argv(command)?),
                _ => pane_argv(&manifest.pane, ctx).map_err(|e| e.to_string())?,
            };
        }
        PaneSpec::Files => out.files_root = Some(ctx.worktree.clone()),
    }
    Ok(out)
}

/// What a write through a pane with `settings` reaches, masked, for its Inbox gate: the database, else the base
/// URL, else the page. `None` when the pane has none of these.
pub fn write_target(settings: &PaneSettings, redactor: &Redactor) -> Option<String> {
    settings
        .database
        .as_ref()
        .map(|d| d.location.clone())
        .or_else(|| settings.base_url.clone())
        .or_else(|| settings.url.clone())
        .map(|t| xctx_http::mask_url(&t, redactor))
        .filter(|t| !t.trim().is_empty())
}

/// What the API client does: send a request, replay one from the history, or diff two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HttpAction {
    /// Send this request (relative URLs are joined to the pane's base URL).
    Send(Request),
    /// Send exchange `id`'s request `times` times.
    Replay {
        /// The history entry.
        id: u64,
        /// How many times, 1 to [`xctx_http::MAX_REPLAYS`].
        times: u32,
    },
    /// Compare exchange `b` against `a`.
    Diff {
        /// The first entry.
        a: u64,
        /// The second entry.
        b: u64,
    },
}

/// What a finished call changes in its pane, applied on the UI thread.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum PaneEffect {
    /// Nothing.
    #[default]
    None,
    /// The API client selects `selected`, or shows `diff`.
    Http {
        /// The exchange to select.
        selected: Option<u64>,
        /// The diff to show.
        diff: Option<ExchangeDiff>,
    },
    /// A sql pane shows this result (`sql.read`'s or `sql.write`'s JSON).
    Sql(Value),
}

/// What a tool call returned, masked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CallOutput {
    /// The structured result: the JSON a tool printed, else its text as a string; a built-in's JSON.
    pub json: Value,
    /// A plain-text rendering.
    pub text: String,
    /// The events the call raised, by name, with their masked detail.
    pub events: Vec<(String, String)>,
    /// What it changes in its pane.
    pub effect: PaneEffect,
}

/// Everything a call needs besides its plan, gathered on the UI thread and sent with it to the background.
#[derive(Clone, Debug)]
pub struct CallEnv {
    /// The data directory, where the http history is saved.
    pub data_dir: Option<PathBuf>,
    /// The space.
    pub space: SpaceId,
    /// The space's port.
    pub port: u16,
    /// True only when the user approved this call in the Inbox.
    pub approved: bool,
    /// The space's http history.
    pub http: Arc<Mutex<HttpHistory>>,
    /// The http pane's base URL.
    pub http_base: Option<String>,
    /// The logs pane's source.
    pub logs: Option<LogSource>,
    /// The sql pane's database.
    pub database: Option<Database>,
    /// The space's scratch directory.
    pub scratch: Option<ScratchDir>,
    /// The web pane's page.
    pub web_url: Option<String>,
    /// The secrets the manifest declares.
    pub secrets: BTreeMap<String, String>,
    /// The sql pane the call's result shows in.
    pub pane: Option<PaneId>,
    /// True when the pane's mount was made by the user, so its log file or database may be outside the worktree
    /// (see [`Mount::user_paths`]).
    pub user_paths: bool,
}

impl CallEnv {
    fn base_url(&self) -> String {
        self.http_base
            .clone()
            .unwrap_or_else(|| format!("http://localhost:{}", self.port))
    }

    /// The web pane's page, `http://localhost:{PORT}` by default: what `page.open` paths are relative to.
    pub fn web_base(&self) -> String {
        self.web_url
            .clone()
            .unwrap_or_else(|| format!("http://localhost:{}", self.port))
    }

    /// The space's port on the machine itself and the origin of the pane's base URL, nothing else.
    fn send_options(&self) -> SendOptions {
        let mut options = SendOptions::default();
        options.allow_local_port(self.port);
        if let Some(base) = &self.http_base {
            let _ = options.allow_url_origin(base);
        }
        options
    }

    /// What the agents' `page.*` tools may open and read: the space's port on the machine itself and the
    /// origin of the pane's URL, nothing else.
    pub fn url_policy(&self) -> UrlPolicy {
        let mut policy = UrlPolicy::default();
        policy.allow_local_port(self.port);
        if let Some(url) = &self.web_url {
            let _ = policy.allow_url_origin(url);
        }
        policy
    }
}

/// A checked call ready to run in the background.
#[derive(Clone, Debug)]
pub struct CallJob {
    /// The plan.
    pub plan: CallPlan,
    /// The plugin's manifest, for its events.
    pub manifest: Manifest,
    /// What it runs with.
    pub env: CallEnv,
}

impl CallJob {
    /// Run the call. Blocks: call it from the background. The error text is masked.
    pub fn run(&self) -> Result<CallOutput, String> {
        let redactor = &self.plan.redactor;
        let mask = |e: String| redactor.redact(&e).into_owned();
        match &self.plan.invocation {
            Invocation::Run { .. } => {
                // The tool starts from a cleared environment: a few of xctx's variables, the panes' PATH, and
                // the plan's own (see `isolated_env`), never xctx's tokens and secrets.
                let mut plan = self.plan.clone();
                plan.env = isolated_env(process_env(), plan.env.get("PATH").cloned(), &plan.env);
                let output = run_plan_isolated(&plan, TOOL_TIMEOUT, TOOL_MAX_OUTPUT)
                    .map_err(|e| mask(e.to_string()))?;
                let events = match_events(&self.manifest, output.lines())
                    .into_iter()
                    .map(|f| (f.name, f.line))
                    .collect();
                let json = serde_json::from_str::<Value>(output.stdout.trim())
                    .ok()
                    .filter(|v| v.is_object() || v.is_array())
                    .map(|mut v| {
                        redactor.redact_json(&mut v);
                        v
                    });
                let mut text = output.stdout.clone();
                if !output.stderr.is_empty() {
                    text.push_str(&format!("\n[stderr]\n{}", output.stderr));
                }
                if output.timed_out {
                    text.push_str("\n[timed out]");
                } else if !output.success() {
                    match output.status {
                        Some(code) => text.push_str(&format!("\n[exit {code}]")),
                        None => text.push_str("\n[killed]"),
                    }
                }
                if output.truncated {
                    text.push_str("\n[output truncated]");
                }
                Ok(CallOutput {
                    json: json.unwrap_or_else(|| Value::String(text.clone())),
                    text,
                    events,
                    effect: PaneEffect::None,
                })
            }
            Invocation::Builtin { name, args } => {
                if name.starts_with("http.") {
                    let action = http_action(name, args)?;
                    run_http(&action, &self.env, &self.manifest, redactor).map_err(mask)
                } else if name.starts_with("page.") {
                    Err(format!(
                        "{name} drives the browser pane's live page and runs in the window, not here"
                    ))
                } else {
                    run_connector(name, args, &self.plan, &self.env).map_err(mask)
                }
            }
        }
    }
}

fn str_arg<'a>(args: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    args.get(name).and_then(Value::as_str)
}

fn id_arg(args: &Map<String, Value>, name: &str) -> Result<u64, String> {
    match args.get(name) {
        Some(Value::String(s)) => s
            .trim()
            .trim_start_matches('#')
            .parse()
            .map_err(|_| format!("{name}: {s:?} is not a history entry number")),
        Some(Value::Number(n)) => n
            .as_u64()
            .ok_or_else(|| format!("{name}: not a history entry number")),
        _ => Err(format!("{name} is required")),
    }
}

/// The API client action a built-in `http.*` call asks for.
pub fn http_action(name: &str, args: &Map<String, Value>) -> Result<HttpAction, String> {
    match name {
        "http.send" => {
            let method = str_arg(args, "method")
                .unwrap_or("GET")
                .trim()
                .to_uppercase();
            let path = str_arg(args, "path").ok_or("http.send: path is required")?;
            let mut request = Request::new(method, path);
            if let Some(Value::Array(headers)) = args.get("headers") {
                for header in headers.iter().filter_map(Value::as_str) {
                    let (name, value) = header.split_once(':').ok_or_else(|| {
                        format!("http.send: header {header:?} is not `Name: value`")
                    })?;
                    request = request.header(name.trim(), value.trim());
                }
            }
            if let Some(body) = str_arg(args, "body") {
                request = request.body(body);
            }
            Ok(HttpAction::Send(request))
        }
        "http.replay" => Ok(HttpAction::Replay {
            id: id_arg(args, "id")?,
            times: args
                .get("times")
                .and_then(Value::as_u64)
                .and_then(|t| u32::try_from(t).ok())
                .unwrap_or(1),
        }),
        "http.diff" => Ok(HttpAction::Diff {
            a: id_arg(args, "from")?,
            b: id_arg(args, "to")?,
        }),
        other => Err(format!("{other} is not an API client built-in")),
    }
}

/// Run an API client action for the space in `env`: send (recording each exchange in the space's history and
/// saving it), replay or diff. Exchanges are masked with `redactor` before they are returned; their events are
/// `response.status` and `response.slow` (see `xctx_http::events`), whose manifest action fires them.
pub fn run_http(
    action: &HttpAction,
    env: &CallEnv,
    manifest: &Manifest,
    redactor: &Redactor,
) -> Result<CallOutput, String> {
    let options = env.send_options();
    let requests: Vec<Request> = match action {
        HttpAction::Send(request) => {
            let mut request = request.clone();
            request.url = join_url(&env.base_url(), &request.url);
            vec![request]
        }
        HttpAction::Replay { id, times } => {
            if *times == 0 || *times > xctx_http::MAX_REPLAYS {
                return Err(format!(
                    "http.replay: times must be 1 to {}",
                    xctx_http::MAX_REPLAYS
                ));
            }
            let request = lock(&env.http)
                .get(*id)
                .map(|e| e.request.clone())
                .ok_or_else(|| format!("the history has no entry {id}"))?;
            vec![request; *times as usize]
        }
        HttpAction::Diff { a, b } => {
            let history = lock(&env.http);
            let first = history
                .get(*a)
                .ok_or_else(|| format!("the history has no entry {a}"))?
                .masked(redactor);
            let second = history
                .get(*b)
                .ok_or_else(|| format!("the history has no entry {b}"))?
                .masked(redactor);
            drop(history);
            let diff = xctx_http::diff(&first, &second);
            let mut json = serde_json::to_value(&diff).map_err(|e| e.to_string())?;
            redactor.redact_json(&mut json);
            let text = if diff.is_same() {
                format!("#{a} and #{b} are the same")
            } else {
                let mut lines = vec![format!(
                    "#{a} {} -> #{b} {}",
                    status_text(diff.a_status),
                    status_text(diff.b_status)
                )];
                lines.extend(diff.body.iter().cloned());
                redactor.redact(&lines.join("\n")).into_owned()
            };
            return Ok(CallOutput {
                json,
                text,
                events: Vec::new(),
                effect: PaneEffect::Http {
                    selected: Some(*b),
                    diff: Some(diff),
                },
            });
        }
    };
    let mut recorded: Vec<Exchange> = Vec::new();
    for request in &requests {
        let exchange = xctx_http::send(request, &options).map_err(|e| e.to_string())?;
        let id = lock(&env.http).record(exchange.clone());
        let mut exchange = exchange;
        exchange.id = id;
        recorded.push(exchange);
    }
    if let Some(data_dir) = &env.data_dir {
        let history = lock(&env.http).clone();
        xctx_http::save(data_dir, &history)
            .map_err(|e| format!("could not save the history: {e}"))?;
    }
    let mut events = Vec::new();
    for exchange in &recorded {
        for event in xctx_http::events(exchange, &EventRules::default()) {
            if manifest.event(event.name()).is_some() {
                let detail = format!(
                    "{} {} -> {} in {} ms",
                    exchange.request.method,
                    xctx_http::mask_url(&exchange.request.url, redactor),
                    status_text(exchange.status()),
                    exchange.elapsed_ms()
                );
                events.push((event.name().to_owned(), detail));
            }
        }
    }
    let masked: Vec<Exchange> = recorded.iter().map(|e| e.masked(redactor)).collect();
    let text = masked
        .iter()
        .map(|e| {
            let mut line = format!(
                "#{} {} {} -> {} in {} ms",
                e.id,
                e.request.method,
                e.request.url,
                status_text(e.status()),
                e.elapsed_ms()
            );
            if let Some(r) = e.response() {
                line.push('\n');
                line.push_str(&r.body);
            } else if let Some(f) = e.failure() {
                line.push_str(&format!(": {}", f.message));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let json = if masked.len() == 1 {
        serde_json::to_value(&masked[0])
    } else {
        serde_json::to_value(&masked)
    }
    .map_err(|e| e.to_string())?;
    Ok(CallOutput {
        json,
        text: cut_chars(&text, TOOL_MAX_OUTPUT),
        events,
        effect: PaneEffect::Http {
            selected: masked.last().map(|e| e.id),
            diff: None,
        },
    })
}

fn status_text(status: Option<u16>) -> String {
    status.map_or_else(|| "no response".to_owned(), |s| s.to_string())
}

/// What an agent's `page.*` call asks of the space's live browser, checked: see [`page_action`]. The window
/// runs it on the browser pane's view (`plugin_browser.rs`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageAction {
    /// Open this URL (allowed by the call's [`CallEnv::url_policy`]) and wait for it to load.
    Open {
        /// The URL.
        url: String,
    },
    /// A PNG of the page, saved in the space's artifacts.
    Screenshot {
        /// The whole page rather than the viewport.
        full_page: bool,
    },
    /// The page's console and failed requests.
    Console {
        /// Only errors (and exceptions) of the console.
        errors_only: bool,
    },
    /// Reload the page and wait for it.
    Reload,
    /// Click the first element matching the selector.
    Click {
        /// The CSS selector.
        selector: String,
    },
    /// Type into the first element matching the selector.
    Type {
        /// The CSS selector.
        selector: String,
        /// The text.
        text: String,
        /// Press Enter (and submit its form) afterwards.
        submit: bool,
    },
    /// The visible text of the page, or of the first element matching the selector.
    Text {
        /// The CSS selector; the whole page when `None`.
        selector: Option<String>,
    },
}

/// The [`PageAction`] built-in `name` asks for with `args` in `env`. `page.open` joins its `path` to the pane's
/// page and refuses a URL outside [`CallEnv::url_policy`] (the space's own app), before anything is loaded.
pub fn page_action(
    name: &str,
    args: &Map<String, Value>,
    env: &CallEnv,
) -> Result<PageAction, String> {
    let flag = |key: &str| args.get(key).and_then(Value::as_bool).unwrap_or(false);
    let required = |key: &str| {
        str_arg(args, key)
            .map(str::to_owned)
            .ok_or_else(|| format!("{name}: {key} is required"))
    };
    match name {
        "page.open" => {
            let url = join_url(&env.web_base(), &required("path")?);
            env.url_policy()
                .check(&url)
                .map_err(|e| format!("page.open: {e}"))?;
            Ok(PageAction::Open { url })
        }
        "page.screenshot" => Ok(PageAction::Screenshot {
            full_page: flag("full_page"),
        }),
        "page.console" => Ok(PageAction::Console {
            errors_only: flag("errors_only"),
        }),
        "page.reload" => Ok(PageAction::Reload),
        "page.click" => Ok(PageAction::Click {
            selector: required("selector")?,
        }),
        "page.type" => Ok(PageAction::Type {
            selector: required("selector")?,
            text: required("text")?,
            submit: flag("submit"),
        }),
        "page.text" => Ok(PageAction::Text {
            selector: str_arg(args, "selector")
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned),
        }),
        other => Err(format!("{other} is not a browser built-in")),
    }
}

/// What a page shows, as `page.open`, `page.reload` and `page.console` report it (unmasked; [`page_output`]
/// masks it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageReading {
    /// The page's URL.
    pub url: String,
    /// Its console, oldest first.
    pub console: Vec<ConsoleEntry>,
    /// Its failed requests, oldest first.
    pub network_errors: Vec<NetworkError>,
}

/// The output of a page action: its JSON and text, both redacted (they hold what the page shows).
pub fn action_output(mut json: Value, text: &str, redactor: &Redactor) -> CallOutput {
    redactor.redact_json(&mut json);
    CallOutput {
        json,
        text: cut_chars(&redactor.redact(text), TOOL_MAX_OUTPUT),
        events: Vec::new(),
        effect: PaneEffect::None,
    }
}

/// The output of `page.open`, `page.reload` (`what` is `opened` or `reloaded`) and `page.console`
/// (`console`, with only errors when `errors_only`): `{url, console, network_errors}`, masked.
pub fn page_output(
    reading: &PageReading,
    errors_only: bool,
    what: &str,
    redactor: &Redactor,
) -> CallOutput {
    let url = xctx_http::mask_url(&reading.url, redactor);
    let console: Vec<ConsoleEntry> = reading
        .console
        .iter()
        .filter(|c| !errors_only || c.is_error())
        .map(|c| mask_console(c.clone(), redactor))
        .collect();
    let network: Vec<NetworkError> = reading
        .network_errors
        .iter()
        .map(|n| mask_network(n.clone(), redactor))
        .collect();
    let mut text = format!("{what}: {url}");
    for c in &console {
        text.push_str(&format!("\n[{}] {}", c.level, c.text));
    }
    for n in &network {
        text.push_str(&format!(
            "\n[network] {} {} {}",
            n.url,
            n.status.map(|s| s.to_string()).unwrap_or_default(),
            n.reason
        ));
    }
    CallOutput {
        json: json!({
            "url": url,
            "console": console,
            "network_errors": network,
        }),
        text: cut_chars(&text, TOOL_MAX_OUTPUT),
        events: Vec::new(),
        effect: PaneEffect::None,
    }
}

/// Save `png` in the space's scratch artifacts (`screenshots/<millis>.png`) and say where: `{path, bytes}`.
/// The path is empty when the space has no scratch directory.
pub fn screenshot_output(env: &CallEnv, png: &[u8]) -> Result<CallOutput, String> {
    let saved = match &env.scratch {
        Some(scratch) => {
            let rel = format!("screenshots/{}.png", now_millis());
            scratch
                .write_artifact(&rel, png)
                .map(|p| p.display().to_string())
                .map_err(|e| format!("could not save the screenshot: {e}"))?
        }
        None => String::new(),
    };
    Ok(CallOutput {
        json: json!({ "path": saved, "bytes": png.len() }),
        text: format!("screenshot saved to {saved}"),
        events: Vec::new(),
        effect: PaneEffect::None,
    })
}

/// The output of `page.click`: `{tag, text, url}`, redacted.
pub fn clicked_output(clicked: &xctx_browser::Clicked, redactor: &Redactor) -> CallOutput {
    let text = format!(
        "clicked <{}> {:?}, now at {}",
        clicked.tag, clicked.text, clicked.url
    );
    action_output(json!(clicked), &text, redactor)
}

/// The output of `page.type` into `selector`: `{value|null, url}`, redacted.
pub fn typed_output(
    selector: &str,
    typed: &xctx_browser::Typed,
    redactor: &Redactor,
) -> CallOutput {
    let text = match &typed.value {
        Some(value) => format!("typed into {selector}: {value:?}, now at {}", typed.url),
        None => format!(
            "typed into {selector}; the field is gone, now at {}",
            typed.url
        ),
    };
    action_output(json!(typed), &text, redactor)
}

/// The output of `page.text`: `{text, truncated, chars, url}`, redacted.
pub fn text_output(read: &xctx_browser::PageText, redactor: &Redactor) -> CallOutput {
    let mut text = read.text.clone();
    if read.truncated {
        text.push_str(&format!(
            "\n[cut at {} of {} characters]",
            read.text.chars().count(),
            read.chars
        ));
    }
    action_output(json!(read), &text, redactor)
}

/// A console entry with its text and location masked.
pub fn mask_console(mut entry: ConsoleEntry, redactor: &Redactor) -> ConsoleEntry {
    entry.text = redactor.redact(&entry.text).into_owned();
    entry.location = entry.location.map(|l| xctx_http::mask_url(&l, redactor));
    entry
}

/// A failed request with its URL and reason masked.
pub fn mask_network(mut error: NetworkError, redactor: &Redactor) -> NetworkError {
    error.url = xctx_http::mask_url(&error.url, redactor);
    error.reason = redactor.redact(&error.reason).into_owned();
    error
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn run_connector(
    name: &str,
    args: &Map<String, Value>,
    plan: &CallPlan,
    env: &CallEnv,
) -> Result<CallOutput, String> {
    let mut ctx = BuiltinContext::new(plan.cwd.clone());
    ctx.space = env.space.as_str().to_owned();
    ctx.port = Some(env.port);
    ctx.env = plan.env.clone();
    ctx.secrets = env.secrets.clone();
    ctx.redactor = plan.redactor.clone();
    ctx.approved = env.approved;
    ctx.logs = env.logs.clone();
    ctx.database = env.database.clone();
    ctx.scratch_dir = env.scratch.as_ref().map(|s| s.root().to_path_buf());
    ctx.user_paths = env.user_paths;
    let out = xctx_connectors::run_builtin(name, args, &ctx).map_err(|e| e.to_string())?;
    let effect = if name.starts_with("sql.") {
        PaneEffect::Sql(out.json.clone())
    } else {
        PaneEffect::None
    };
    Ok(CallOutput {
        json: out.json,
        text: out.text,
        events: out.events.into_iter().map(|e| (e.name, e.detail)).collect(),
        effect,
    })
}

// -------------------------------------------------------------------------------------------------------------
// Events and the loop

/// What a plugin event did to the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopEffect {
    /// Nothing: the event is a notice, or the loop was not at a point the event applies to.
    Unchanged,
    /// The loop was stopped at its stage.
    Blocked,
    /// The stage's check passed and the loop moved on (or now waits for approval to).
    Advanced(LoopDecision),
}

/// Apply `action` to `run`: `fail` blocks a loop that is running, waiting for approval or out of budget (an idle,
/// passed or already blocked loop is left alone); `pass` advances a running loop as `loop.advance` with passing
/// checks does (so an *ask each step* loop waits for approval); `notice` changes nothing.
pub fn apply_loop_action(
    run: &mut LoopRun,
    action: LoopAction,
    reason: &str,
) -> Result<LoopEffect, LoopError> {
    match action {
        LoopAction::Fail => match run.status() {
            LoopStatus::Running
            | LoopStatus::AwaitingApproval { .. }
            | LoopStatus::BudgetExhausted => {
                run.block(reason);
                Ok(LoopEffect::Blocked)
            }
            _ => Ok(LoopEffect::Unchanged),
        },
        LoopAction::Pass => {
            if *run.status() == LoopStatus::Running {
                run.advance(true).map(LoopEffect::Advanced)
            } else {
                Ok(LoopEffect::Unchanged)
            }
        }
        LoopAction::Notice => Ok(LoopEffect::Unchanged),
    }
}

/// How serious the finding of an event mapped to `action` is.
pub fn event_severity(action: Option<LoopAction>) -> FindingSeverity {
    match action {
        Some(LoopAction::Fail) => FindingSeverity::Error,
        Some(LoopAction::Notice) => FindingSeverity::Warning,
        Some(LoopAction::Pass) | None => FindingSeverity::Info,
    }
}

/// The protocol's name for a loop action.
pub fn event_action(action: LoopAction) -> PluginEventAction {
    match action {
        LoopAction::Fail => PluginEventAction::Fail,
        LoopAction::Pass => PluginEventAction::Pass,
        LoopAction::Notice => PluginEventAction::Notice,
    }
}

/// `detail` without the parts that change from one firing of the same thing to the next (an elapsed time such as
/// `in 31 ms` or `1.2s`, a timestamp such as `2024-05-01T10:00:00Z` or `10:00:00.123`), so repeats debounce.
pub fn stable_detail(detail: &str) -> String {
    let is_number = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == '.');
    let is_duration = |t: &str| {
        ["ms", "s", "µs", "us"]
            .iter()
            .any(|unit| t.strip_suffix(unit).is_some_and(is_number))
    };
    let is_timestamp = |t: &str| {
        let digits = t.chars().filter(char::is_ascii_digit).count();
        digits >= 4
            && t.chars().any(|c| matches!(c, ':' | '-' | 'T'))
            && t.chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '-' | ':' | '.' | 'T' | 'Z' | '+'))
    };
    let words: Vec<&str> = detail.split_whitespace().collect();
    let mut kept: Vec<&str> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let word = words[i].trim_matches(|c: char| matches!(c, '[' | ']' | '(' | ')' | ','));
        let unit_next = words
            .get(i + 1)
            .is_some_and(|n| matches!(*n, "ms" | "s" | "µs" | "us"));
        if is_number(word) && unit_next {
            i += 2;
            continue;
        }
        if is_duration(word) || is_timestamp(word) {
            i += 1;
            continue;
        }
        kept.push(words[i]);
        i += 1;
    }
    kept.join(" ")
}

/// Rate-limits the Inbox notices of plugin events: one per space, plugin and event per [`NOTICE_INTERVAL`].
#[derive(Debug, Default)]
pub struct NoticeLimiter {
    posted: HashMap<(String, String, String), Instant>,
}

impl NoticeLimiter {
    /// Whether a notice of `event` may be posted at `now`; when it may, it counts as posted.
    pub fn admit(&mut self, space: &str, plugin: &str, event: &str, now: Instant) -> bool {
        self.posted
            .retain(|_, at| now.saturating_duration_since(*at) < NOTICE_INTERVAL);
        let key = (space.to_owned(), plugin.to_owned(), event.to_owned());
        if self.posted.contains_key(&key) {
            return false;
        }
        self.posted.insert(key, now);
        true
    }
}

/// One event of a call, after repeats of the same name were collapsed into it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollapsedEvent {
    /// The event's name.
    pub name: String,
    /// The first detail it fired with.
    pub detail: String,
    /// How many times it fired in the call.
    pub count: usize,
}

/// `events` (name, detail) collapsed by name in the order each name first fired: the first detail and a count.
pub fn collapse_events(events: &[(String, String)]) -> Vec<CollapsedEvent> {
    let mut out: Vec<CollapsedEvent> = Vec::new();
    for (name, detail) in events {
        match out.iter_mut().find(|e| &e.name == name) {
            Some(e) => e.count += 1,
            None => out.push(CollapsedEvent {
                name: name.clone(),
                detail: detail.clone(),
                count: 1,
            }),
        }
    }
    out
}

/// Drops repeats of an event: the same space, plugin, event and detail within [`EVENT_DEBOUNCE`]. Callers pass
/// the [`stable_detail`], so an elapsed time does not make a repeat look new.
#[derive(Debug, Default)]
pub struct EventDebounce {
    seen: HashMap<(String, String, String, String), Instant>,
}

/// Drops a tailed line's event when the same event of the same plugin fired from a tail of the space within
/// [`EVENT_DEBOUNCE`], whatever the line: a noisy log must not flood the shared space and the Inbox.
#[derive(Debug, Default)]
pub struct TailDebounce {
    inner: EventDebounce,
}

impl TailDebounce {
    /// Whether the tailed event may fire at `now`.
    pub fn admit(&mut self, space: &str, plugin: &str, event: &str, now: Instant) -> bool {
        self.inner.admit(space, plugin, event, "", now)
    }
}

impl EventDebounce {
    /// Whether the event may fire at `now`; a repeat within the window does not, and does not extend it.
    pub fn admit(
        &mut self,
        space: &str,
        plugin: &str,
        event: &str,
        detail: &str,
        now: Instant,
    ) -> bool {
        self.seen
            .retain(|_, at| now.saturating_duration_since(*at) < EVENT_DEBOUNCE);
        let key = (
            space.to_owned(),
            plugin.to_owned(),
            event.to_owned(),
            detail.to_owned(),
        );
        if self.seen.contains_key(&key) {
            return false;
        }
        self.seen.insert(key, now);
        true
    }
}

/// What firing a plugin event did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventReport {
    /// The loop action the manifest maps the event to.
    pub action: Option<LoopAction>,
    /// True when it repeated an event fired moments ago, so nothing was done.
    pub debounced: bool,
    /// What it did to the loop.
    pub loop_effect: LoopEffect,
    /// The findings recorded in the shared space, one per distinct event (at most [`MAX_EVENTS_PER_CALL`]).
    pub findings: Vec<u64>,
    /// The Inbox notice posted, at most one per call: the loop newly stopped, the loop waits for approval, or a
    /// notice or failure event not posted in the last [`NOTICE_INTERVAL`].
    pub notice: Option<ItemId>,
    /// One line on what happened, masked, for the status bar.
    pub summary: String,
    /// What could not be done (saving the finding, the loop or the notice).
    pub errors: Vec<String>,
}

// -------------------------------------------------------------------------------------------------------------
// Control summaries

/// A plugin as `plugin.list` shows it.
pub fn plugin_summary(
    plugin: &Plugin,
    space: Option<&Space>,
    mounted_in: Vec<String>,
) -> PluginSummary {
    let (source, repo, trusted) = match &plugin.source {
        xctx_plugin::Source::Builtin => (PluginSource::Builtin, None, None),
        xctx_plugin::Source::User { .. } => (PluginSource::User, None, None),
        xctx_plugin::Source::Repo { repo, .. } => {
            let name = space
                .and_then(|s| s.repos.iter().find(|r| &r.worktree == repo))
                .map(|r| r.name.clone())
                .unwrap_or_else(|| repo.display().to_string());
            (PluginSource::Repo, Some(name), Some(plugin.trusted))
        }
    };
    PluginSummary {
        name: plugin.name().to_owned(),
        description: plugin.manifest.description.clone(),
        source,
        repo,
        trusted,
        mounted_in,
        tools: plugin
            .manifest
            .tools
            .iter()
            .map(|t| PluginToolSummary {
                name: t.name.clone(),
                effect: tool_effect(t.effect),
            })
            .collect(),
    }
}

fn tool_effect(effect: Effect) -> ToolEffect {
    match effect {
        Effect::Read => ToolEffect::Read,
        Effect::Write => ToolEffect::Write,
    }
}

fn param_kind(kind: ParamKind) -> &'static str {
    match kind {
        ParamKind::String => "string",
        ParamKind::Integer => "integer",
        ParamKind::Boolean => "boolean",
        ParamKind::Path => "path",
        ParamKind::Strings => "strings",
    }
}

/// A plugin's tools as `plugin.tools` shows them.
pub fn tool_specs(manifest: &Manifest) -> PluginToolsResult {
    PluginToolsResult {
        plugin: manifest.name.clone(),
        tools: manifest
            .tools
            .iter()
            .map(|t| PluginToolSpec {
                name: t.name.clone(),
                description: t.description.clone(),
                effect: tool_effect(t.effect),
                params: t
                    .params
                    .iter()
                    .map(|p| PluginToolParam {
                        name: p.name.clone(),
                        kind: param_kind(p.kind).to_owned(),
                        required: p.required,
                        description: (!p.description.is_empty()).then(|| p.description.clone()),
                    })
                    .collect(),
            })
            .collect(),
    }
}

// -------------------------------------------------------------------------------------------------------------
// Pane data for the plugin panes

/// What the API client pane of a space shows. Exchanges are masked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HttpPaneData {
    /// The pane's base URL, when an http plugin is mounted.
    pub base_url: Option<String>,
    /// The space's history, oldest first, masked.
    pub history: Vec<Exchange>,
    /// The exchange selected (the latest sent, replayed or diffed).
    pub selected: Option<u64>,
    /// The diff last asked for.
    pub diff: Option<ExchangeDiff>,
    /// True while a send, replay or diff runs.
    pub busy: bool,
    /// Why the last action failed, masked.
    pub error: Option<String>,
}

/// What a logs pane shows. Lines are masked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogsPaneData {
    /// What it tails.
    pub source: String,
    /// Its lines, oldest first.
    pub lines: Vec<LogLine>,
    /// Where the tail is.
    pub status: TailStatus,
}

/// A table a sql pane shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SqlTable {
    /// The column names.
    pub columns: Vec<String>,
    /// The rows, each cell as text (NULL as empty).
    pub rows: Vec<Vec<String>>,
    /// How many rows came back (before the pane's own cap).
    pub row_count: usize,
    /// True when the tool cut the result.
    pub truncated: bool,
}

impl SqlTable {
    /// The table in a `sql.read` or `sql.write` JSON result.
    pub fn from_json(value: &Value) -> SqlTable {
        let columns = value
            .get("columns")
            .and_then(Value::as_array)
            .map(|c| c.iter().map(cell_text).collect())
            .unwrap_or_default();
        let rows: Vec<Vec<String>> = value
            .get("rows")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|row| match row {
                        Value::Array(cells) => cells.iter().map(cell_text).collect(),
                        Value::Object(map) => map.values().map(cell_text).collect(),
                        other => vec![cell_text(other)],
                    })
                    .collect()
            })
            .unwrap_or_default();
        SqlTable {
            columns,
            row_count: value
                .get("row_count")
                .and_then(Value::as_u64)
                .map_or(rows.len(), |n| n as usize),
            rows,
            truncated: value
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }
}

fn cell_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// What a sql pane shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SqlPaneData {
    /// Its engine: `sqlite` or `postgres`.
    pub engine: String,
    /// Its database, masked.
    pub database: String,
    /// The last result.
    pub result: Option<SqlTable>,
    /// Why the last query failed, masked.
    pub error: Option<String>,
    /// True while a query runs.
    pub busy: bool,
    /// True while a write it asked for waits in the Inbox.
    pub write_pending: bool,
}

/// A plugin mounted in a space, for the Plugins menu and the pane headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountedPlugin {
    /// Its pane.
    pub pane: PaneId,
    /// The plugin's name.
    pub plugin: String,
    /// The repo it works in.
    pub repo: String,
    /// Its pane kind: `terminal`, `http`, `logs`, `sql`, `web` or `files` (empty when the plugin is no longer
    /// known).
    pub kind: &'static str,
    /// What was chosen when mounting.
    pub settings: MountSettings,
}

/// A sql pane's state.
#[derive(Clone, Debug, Default)]
pub struct SqlState {
    /// The last result's JSON.
    pub result: Option<Value>,
    /// Why the last query failed.
    pub error: Option<String>,
    /// Queries running.
    pub busy: u32,
    /// The write it asked for, waiting in the Inbox.
    pub write_pending: Option<ItemId>,
}

/// The API client's state in a space.
#[derive(Clone, Debug, Default)]
pub struct HttpState {
    /// The exchange selected.
    pub selected: Option<u64>,
    /// The diff shown.
    pub diff: Option<ExchangeDiff>,
    /// Actions running.
    pub busy: u32,
    /// Why the last one failed.
    pub error: Option<String>,
}

/// The browser pane's state in a space, beside its live view (which the window keeps).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WebState {
    /// Agent `page.*` calls running on the page.
    pub busy: u32,
    /// Why the last one failed, masked.
    pub error: Option<String>,
}

/// What a space's mounted plugins keep running.
#[derive(Debug)]
pub struct SpaceRuntime {
    /// The space's http history, loaded on first use.
    pub http: Arc<Mutex<HttpHistory>>,
    /// The API client pane's state.
    pub http_state: HttpState,
    /// Each logs pane's tail.
    pub logs: HashMap<PaneId, LogTail>,
    /// Each sql pane's state.
    pub sql: HashMap<PaneId, SqlState>,
    /// The browser pane's state.
    pub web: WebState,
}

/// A write call waiting in the Inbox.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingWrite {
    /// The space.
    pub space: SpaceId,
    /// The plugin.
    pub plugin: String,
    /// The tool.
    pub tool: String,
    /// The call's arguments, unmasked.
    pub args: Map<String, Value>,
    /// The sql pane that asked for it, if one did.
    pub pane: Option<PaneId>,
}

/// A mount waiting in the Inbox (for trust or approval).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingMount {
    /// The space.
    pub space: SpaceId,
    /// The plugin.
    pub plugin: String,
    /// The repo it works in.
    pub repo: Option<String>,
    /// What was chosen.
    pub settings: MountSettings,
    /// For a repo plugin's trust gate, the registry's hash of the manifest the user is asked about.
    pub hash: Option<String>,
    /// True when an agent asked (`plugin.mount`), so the mount's paths are not the user's.
    pub from_agent: bool,
}

/// What mounting needs first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MountCheck {
    /// Nothing: mount it.
    Ready,
    /// The user must trust the repo plugin first.
    NeedsTrust {
        /// The repo, by name in the space.
        repo: String,
        /// The registry's hash of the exact manifest bytes it parsed ([`Plugin::manifest_hash`]): what the user
        /// is asked to trust.
        hash: String,
    },
    /// The user must approve the mount, for this reason.
    NeedsApproval(String),
}

/// What a call needs before it runs.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Prepared {
    /// A write: post this gate, and keep the call until it is approved.
    Gate(PluginGate, PendingWrite),
    /// Run it now.
    Run(Box<CallJob>),
}

// -------------------------------------------------------------------------------------------------------------
// The files pane
//
// A read-only explorer of a repo's worktree. Paths are relative to the worktree with `/` separators (`""` is the
// worktree itself), and every read is resolved inside it first ([`resolve_inside`]). Folders are read only when
// opened, git runs with `--no-optional-locks` so it never takes the index lock from the agents working there,
// and everything here blocks: the app runs it on the background executor.

/// The most entries a files pane lists of one folder; a "… N more" row counts the rest.
pub const FILES_ENTRY_CAP: usize = 500;

/// The most lines of a file, or of its diff, a files pane shows.
pub const FILES_PREVIEW_LINES: usize = 2000;

/// The most bytes of a file, or of its diff, a files pane reads to show.
pub const FILES_PREVIEW_BYTES: usize = 256 * 1024;

/// The entries a files pane never lists, in any folder.
pub const FILES_HIDDEN: &[&str] = &[".git"];

/// Folders that usually hold dependencies or build output: listed (drawn muted) and, like every folder, read
/// only when opened.
pub const FILES_HEAVY: &[&str] = &["node_modules", "target", "dist", ".next", "build"];

/// How many leading bytes tell a binary file: git's own rule, a NUL among the first 8000.
const BINARY_SNIFF: usize = 8000;

/// A path's change in the worktree, from `git status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GitChange {
    /// Changed (`M`, or a type change).
    Modified,
    /// New in the index (`A`), or copied (`C`).
    Added,
    /// Deleted, in the index or the worktree (`D`).
    Deleted,
    /// Renamed (`R`).
    Renamed,
    /// Not tracked (`??`).
    Untracked,
    /// Unmerged (`U`, `AA`, `DD`).
    Conflicted,
}

impl GitChange {
    /// The change a porcelain v1 `XY` code stands for; `None` for one that is no change (`!!`, ignored).
    pub fn from_code(x: char, y: char) -> Option<GitChange> {
        match (x, y) {
            ('?', '?') => Some(GitChange::Untracked),
            ('!', '!') | (' ', ' ') => None,
            ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D') => Some(GitChange::Conflicted),
            ('R', _) | (_, 'R') => Some(GitChange::Renamed),
            ('D', _) | (_, 'D') => Some(GitChange::Deleted),
            ('A' | 'C', _) | (_, 'A' | 'C') => Some(GitChange::Added),
            _ => Some(GitChange::Modified),
        }
    }
}

/// The changed paths of `git status --porcelain=v1 -z` output, in its order. A rename or copy is reported under
/// its new path (the old path that follows it is skipped); an untracked folder is one entry ending in `/`.
pub fn parse_porcelain_z(text: &str) -> Vec<(String, GitChange)> {
    let mut out = Vec::new();
    let mut fields = text.split('\0');
    while let Some(entry) = fields.next() {
        let mut chars = entry.chars();
        let (Some(x), Some(y), Some(' ')) = (chars.next(), chars.next(), chars.next()) else {
            continue;
        };
        if matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C') {
            fields.next();
        }
        let path = chars.as_str();
        if path.is_empty() {
            continue;
        }
        if let Some(change) = GitChange::from_code(x, y) {
            out.push((path.to_owned(), change));
        }
    }
    out
}

/// The folder `path` is in (`""` for the worktree's own entries).
fn parent_of(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

/// `name` in folder `dir`.
fn join_rel(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// A worktree's changes, by path relative to the worktree. An untracked folder is one entry ending in `/` that
/// covers everything under it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitStatus {
    changes: BTreeMap<String, GitChange>,
}

impl GitStatus {
    /// The status of `entries` ([`parse_porcelain_z`]), whose paths are relative to the repository's top folder,
    /// for a worktree at `prefix` under it (`git rev-parse --show-prefix`: empty, or ending in `/`). Paths outside
    /// the prefix are dropped.
    pub fn new(entries: Vec<(String, GitChange)>, prefix: &str) -> GitStatus {
        let changes = entries
            .into_iter()
            .filter_map(|(path, change)| {
                let rel = path.strip_prefix(prefix)?;
                (!rel.is_empty()).then(|| (rel.to_owned(), change))
            })
            .collect();
        GitStatus { changes }
    }

    /// The change of file `path`: its own, or that of the untracked folder it is in.
    pub fn of(&self, path: &str) -> Option<GitChange> {
        if let Some(change) = self.changes.get(path) {
            return Some(*change);
        }
        let mut end = path.len();
        while let Some(i) = path[..end].rfind('/') {
            if let Some(change) = self.changes.get(&path[..=i]) {
                return Some(*change);
            }
            end = i;
        }
        None
    }

    /// Whether anything at or under folder `dir` changed (anything at all for `""`).
    pub fn changed_under(&self, dir: &str) -> bool {
        if dir.is_empty() {
            return !self.changes.is_empty();
        }
        let prefix = format!("{dir}/");
        let below = self
            .changes
            .range::<str, _>((
                std::ops::Bound::Included(prefix.as_str()),
                std::ops::Bound::Unbounded,
            ))
            .next()
            .is_some_and(|(path, _)| path.starts_with(&prefix));
        below || self.of(&prefix).is_some()
    }

    /// The entries of folder `dir` that only git knows of, as `(name, is_folder)`: deleted files gone from disk,
    /// and the folders they were in.
    pub fn deleted_in(&self, dir: &str) -> Vec<(String, bool)> {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let mut out: Vec<(String, bool)> = Vec::new();
        for (path, change) in &self.changes {
            if *change != GitChange::Deleted {
                continue;
            }
            let Some(rest) = path.strip_prefix(&prefix) else {
                continue;
            };
            let entry = match rest.split_once('/') {
                Some((folder, _)) => (folder.to_owned(), true),
                None => (rest.to_owned(), false),
            };
            if !out.contains(&entry) {
                out.push(entry);
            }
        }
        out
    }
}

/// One entry of a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderEntry {
    /// Its file name.
    pub name: String,
    /// Whether it opens as a folder (a symlink to a folder does).
    pub is_dir: bool,
}

/// A folder's entries as the tree lists them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderListing {
    /// The first [`FILES_ENTRY_CAP`] entries, sorted ([`sort_entries`]).
    pub entries: Vec<FolderEntry>,
    /// How many more there are.
    pub more: usize,
}

/// Sort folder entries as the tree shows them: folders first, then by name ignoring case (ties by the exact name,
/// so the order is stable).
pub fn sort_entries(entries: &mut [FolderEntry]) {
    entries.sort_by_cached_key(|e| (!e.is_dir, e.name.to_lowercase(), e.name.clone()));
}

/// `entries` sorted ([`sort_entries`]) and cut to the first `cap`, counting the rest.
pub fn cap_entries(mut entries: Vec<FolderEntry>, cap: usize) -> FolderListing {
    sort_entries(&mut entries);
    let more = entries.len().saturating_sub(cap);
    entries.truncate(cap);
    FolderListing { entries, more }
}

/// `rel` as a reason shows it: `the worktree` for `""`.
fn shown_rel(rel: &str) -> &str {
    if rel.is_empty() { "the worktree" } else { rel }
}

/// Refuse a relative path that is absolute or steps out with `..`.
fn check_rel(rel: &str) -> Result<(), String> {
    let plain = Path::new(rel)
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)));
    if plain {
        Ok(())
    } else {
        Err(format!("refused: {rel} is not a path inside the worktree"))
    }
}

/// The canonical path of `rel` in worktree `root`, symlinks followed. Refused when `rel` is absolute or has `..`,
/// or when the canonical path is outside the canonical worktree (a symlink pointing out of it).
pub fn resolve_inside(root: &Path, rel: &str) -> Result<PathBuf, String> {
    if root.as_os_str().is_empty() {
        return Err("this pane has no worktree: the space has no repo".to_owned());
    }
    check_rel(rel)?;
    let top = std::fs::canonicalize(root)
        .map_err(|e| format!("cannot open the worktree {}: {e}", root.display()))?;
    let path = std::fs::canonicalize(top.join(rel))
        .map_err(|e| format!("cannot open {}: {e}", shown_rel(rel)))?;
    if !path.starts_with(&top) {
        return Err(format!(
            "refused: {} leads outside the worktree",
            shown_rel(rel)
        ));
    }
    Ok(path)
}

/// The entries of folder `rel` of worktree `root`, without [`FILES_HIDDEN`], sorted and capped at
/// [`FILES_ENTRY_CAP`]. A folder no longer on disk (deleted in the worktree) lists nothing; one leading outside the
/// worktree is refused ([`resolve_inside`]).
pub fn list_folder(root: &Path, rel: &str) -> Result<FolderListing, String> {
    check_rel(rel)?;
    if !rel.is_empty()
        && !root.as_os_str().is_empty()
        && std::fs::symlink_metadata(root.join(rel))
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(FolderListing::default());
    }
    let dir = resolve_inside(root, rel)?;
    let read =
        std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", shown_rel(rel)))?;
    let mut entries = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if FILES_HIDDEN.contains(&name.as_str()) {
            continue;
        }
        let is_dir = match entry.file_type() {
            // A symlink opens as what it points to; one leading outside is refused when opened.
            Ok(kind) if kind.is_symlink() => {
                std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir())
            }
            Ok(kind) => kind.is_dir(),
            Err(_) => false,
        };
        entries.push(FolderEntry { name, is_dir });
    }
    Ok(cap_entries(entries, FILES_ENTRY_CAP))
}

/// Whether `bytes` (a file's first bytes) look binary: a NUL among the first 8000, as git decides.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF)].contains(&0)
}

/// `bytes` as text within `max_lines` lines (invalid UTF-8 replaced), and whether anything was left out.
/// `complete` is false when more bytes followed the ones read: the last, partial line is dropped then.
pub fn cap_text(bytes: &[u8], complete: bool, max_lines: usize) -> (String, bool) {
    let mut end = bytes.len();
    let mut cut = !complete;
    if !complete && let Some(i) = bytes.iter().rposition(|b| *b == b'\n') {
        end = i + 1;
    }
    if let Some(i) = bytes[..end]
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .map(|(i, _)| i)
        .nth(max_lines.saturating_sub(1))
        && i + 1 < end
    {
        end = i + 1;
        cut = true;
    }
    (String::from_utf8_lossy(&bytes[..end]).into_owned(), cut)
}

/// What a files pane shows of a file's text, or of its diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewText {
    /// The text, masked.
    Text {
        /// The text.
        text: String,
        /// Whether it was cut at [`FILES_PREVIEW_LINES`] or [`FILES_PREVIEW_BYTES`].
        cut: bool,
    },
    /// A binary file.
    Binary,
}

/// A file as a files pane shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePreview {
    /// Its size in bytes.
    pub size: u64,
    /// Its text, or that it is binary.
    pub body: PreviewText,
}

/// Text from `bytes` (`complete` when nothing followed them): binary, or capped and masked with `redactor`.
fn preview_text(bytes: &[u8], complete: bool, redactor: &Redactor) -> PreviewText {
    if is_binary(bytes) {
        return PreviewText::Binary;
    }
    let (text, cut) = cap_text(bytes, complete, FILES_PREVIEW_LINES);
    PreviewText::Text {
        text: redactor.redact(&text).into_owned(),
        cut,
    }
}

/// File `rel` of worktree `root`: its size and its first [`FILES_PREVIEW_BYTES`] as masked text, or that it is
/// binary. Refused outside the worktree ([`resolve_inside`]) and for anything but a regular file.
pub fn read_preview(root: &Path, rel: &str, redactor: &Redactor) -> Result<FilePreview, String> {
    use std::io::Read;
    let path = resolve_inside(root, rel)?;
    let meta = std::fs::metadata(&path).map_err(|e| format!("cannot read {rel}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{rel} is not a regular file"));
    }
    let file = std::fs::File::open(&path).map_err(|e| format!("cannot read {rel}: {e}"))?;
    let mut bytes = Vec::new();
    file.take(FILES_PREVIEW_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read {rel}: {e}"))?;
    let complete = bytes.len() <= FILES_PREVIEW_BYTES;
    bytes.truncate(FILES_PREVIEW_BYTES);
    Ok(FilePreview {
        size: meta.len(),
        body: preview_text(&bytes, complete, redactor),
    })
}

/// Run git in `dir` (prompts off, no optional locks, pathspecs literal) and return its stdout; exit status 1
/// counts as success when `one_is_ok` (a `--no-index` diff that found differences).
fn run_git_bytes(dir: &Path, args: &[&str], one_is_ok: bool) -> Result<Vec<u8>, String> {
    let output = std::process::Command::new("git")
        .args(["--no-optional-locks", "--literal-pathspecs"])
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    match output.status.code() {
        Some(0) => Ok(output.stdout),
        Some(1) if one_is_ok => Ok(output.stdout),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(match stderr.trim() {
                "" => format!("git {} exited with {}", args.join(" "), output.status),
                why => format!("git: {why}"),
            })
        }
    }
}

/// The changes of worktree `root`, from `git status --porcelain=v1 -z` (untracked folders as one entry).
pub fn git_status(root: &Path) -> Result<GitStatus, String> {
    let dir = resolve_inside(root, "")?;
    let prefix = run_git_bytes(&dir, &["rev-parse", "--show-prefix"], false)?;
    let prefix = String::from_utf8_lossy(&prefix);
    let text = run_git_bytes(&dir, &["status", "--porcelain=v1", "-z"], false)?;
    Ok(GitStatus::new(
        parse_porcelain_z(&String::from_utf8_lossy(&text)),
        prefix.trim_end_matches('\n'),
    ))
}

/// What file `rel` of worktree `root`, with `change`, changed: `git diff HEAD -- <file>` (staged and unstaged;
/// plain `git diff -- <file>` before the first commit), or the whole file as added when it is untracked. Masked
/// with `redactor` and capped like a preview.
pub fn file_diff(
    root: &Path,
    rel: &str,
    change: GitChange,
    redactor: &Redactor,
) -> Result<PreviewText, String> {
    check_rel(rel)?;
    let dir = resolve_inside(root, "")?;
    let mut out = if change == GitChange::Untracked {
        // git reads the file itself here, so it must be inside first.
        resolve_inside(root, rel)?;
        let args = [
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-index",
            "--",
            "/dev/null",
            rel,
        ];
        run_git_bytes(&dir, &args, true)?
    } else {
        let head = ["diff", "--no-color", "--no-ext-diff", "HEAD", "--", rel];
        match run_git_bytes(&dir, &head, false) {
            Ok(out) => out,
            Err(_) => run_git_bytes(
                &dir,
                &["diff", "--no-color", "--no-ext-diff", "--", rel],
                false,
            )?,
        }
    };
    if out.is_empty() {
        return Ok(PreviewText::Text {
            text: "No differences from the last commit.".to_owned(),
            cut: false,
        });
    }
    let complete = out.len() <= FILES_PREVIEW_BYTES;
    out.truncate(FILES_PREVIEW_BYTES);
    Ok(preview_text(&out, complete, redactor))
}

/// One row of a files pane's tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    /// The entry's path relative to the worktree (the folder's, for `More`, `Loading` and `Refused`).
    pub path: String,
    /// Its name (empty for `More`, `Loading` and `Refused`).
    pub name: String,
    /// How deep it is: 0 for the worktree's own entries.
    pub depth: usize,
    /// What it is.
    pub kind: TreeRowKind,
}

/// What a tree row is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeRowKind {
    /// A folder.
    Folder {
        /// Whether it is open.
        open: bool,
        /// Whether something under it changed.
        changed: bool,
        /// One of [`FILES_HEAVY`].
        heavy: bool,
    },
    /// A file, with its change.
    File(Option<GitChange>),
    /// How many more entries the folder has than are listed.
    More(usize),
    /// The folder is being read.
    Loading,
    /// Why the folder cannot be listed.
    Refused(String),
}

/// What a files pane knows of its worktree: the folders read, which are open, which are being read, and the git
/// status. Only the worktree itself and the folders the user opened are ever read.
#[derive(Clone, Debug)]
pub struct FileTree {
    root: PathBuf,
    folders: HashMap<String, Result<FolderListing, String>>,
    open: HashSet<String>,
    reading: HashSet<String>,
    status: Option<Result<GitStatus, String>>,
}

impl FileTree {
    /// The tree of worktree `root`, with nothing read yet: [`FileTree::refresh`] says what to read first.
    pub fn new(root: PathBuf) -> FileTree {
        FileTree {
            root,
            folders: HashMap::new(),
            open: HashSet::from([String::new()]),
            reading: HashSet::new(),
            status: None,
        }
    }

    /// The worktree.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether a folder is being read.
    pub fn is_reading(&self) -> bool {
        !self.reading.is_empty()
    }

    /// Open or close folder `rel`. True when it was opened and must be read now (it is marked as being read).
    pub fn toggle(&mut self, rel: &str) -> bool {
        if rel.is_empty() || self.open.remove(rel) {
            return false;
        }
        self.open.insert(rel.to_owned());
        if self.folders.contains_key(rel) || self.reading.contains(rel) {
            return false;
        }
        self.reading.insert(rel.to_owned());
        true
    }

    /// The open folders whose parents are open too, to read again; each is marked as being read, and what it
    /// showed stays until its new listing comes. Listings of the other folders are dropped, so they are read
    /// again when opened.
    pub fn refresh(&mut self) -> Vec<String> {
        let shown: HashSet<String> = self
            .open
            .iter()
            .filter(|rel| self.parents_open(rel))
            .cloned()
            .collect();
        self.folders.retain(|rel, _| shown.contains(rel));
        self.reading.extend(shown.iter().cloned());
        let mut shown: Vec<String> = shown.into_iter().collect();
        shown.sort();
        shown
    }

    fn parents_open(&self, rel: &str) -> bool {
        let mut dir = rel;
        while !dir.is_empty() {
            dir = parent_of(dir);
            if !self.open.contains(dir) {
                return false;
            }
        }
        true
    }

    /// Folder `rel` was read.
    pub fn set_listing(&mut self, rel: &str, listing: Result<FolderListing, String>) {
        self.reading.remove(rel);
        self.folders.insert(rel.to_owned(), listing);
    }

    /// Git status ran.
    pub fn set_status(&mut self, status: Result<GitStatus, String>) {
        self.status = Some(status);
    }

    /// The git status, once it ran and if it worked.
    pub fn status(&self) -> Option<&GitStatus> {
        self.status.as_ref().and_then(|s| s.as_ref().ok())
    }

    /// Why git status failed, if it did.
    pub fn status_error(&self) -> Option<&str> {
        self.status
            .as_ref()
            .and_then(|s| s.as_ref().err())
            .map(String::as_str)
    }

    /// The change of file `rel`, if the status knows one.
    pub fn change_of(&self, rel: &str) -> Option<GitChange> {
        self.status().and_then(|s| s.of(rel))
    }

    /// The rows of the tree, depth first: each open folder's entries after it (with the deleted files git knows
    /// of), and only changed paths when `changed_only`.
    pub fn rows(&self, changed_only: bool) -> Vec<TreeRow> {
        let mut out = Vec::new();
        self.push_rows("", 0, changed_only, &mut out);
        out
    }

    fn push_rows(&self, dir: &str, depth: usize, changed_only: bool, out: &mut Vec<TreeRow>) {
        let note = |kind| TreeRow {
            path: dir.to_owned(),
            name: String::new(),
            depth,
            kind,
        };
        let listing = match self.folders.get(dir) {
            None if self.reading.contains(dir) => return out.push(note(TreeRowKind::Loading)),
            None => return,
            Some(Err(why)) => return out.push(note(TreeRowKind::Refused(why.clone()))),
            Some(Ok(listing)) => listing,
        };
        let status = self.status();
        let mut gone: Vec<FolderEntry> = status
            .map(|s| s.deleted_in(dir))
            .unwrap_or_default()
            .into_iter()
            .filter(|(name, _)| !listing.entries.iter().any(|e| &e.name == name))
            .map(|(name, is_dir)| FolderEntry { name, is_dir })
            .collect();
        let merged;
        let entries: &[FolderEntry] = if gone.is_empty() {
            &listing.entries
        } else {
            gone.extend(listing.entries.iter().cloned());
            sort_entries(&mut gone);
            merged = gone;
            &merged
        };
        for entry in entries {
            let path = join_rel(dir, &entry.name);
            if entry.is_dir {
                let changed = status.is_some_and(|s| s.changed_under(&path));
                if changed_only && !changed {
                    continue;
                }
                let open = self.open.contains(&path);
                out.push(TreeRow {
                    kind: TreeRowKind::Folder {
                        open,
                        changed,
                        heavy: FILES_HEAVY.contains(&entry.name.as_str()),
                    },
                    path: path.clone(),
                    name: entry.name.clone(),
                    depth,
                });
                if open {
                    self.push_rows(&path, depth + 1, changed_only, out);
                }
            } else {
                let change = status.and_then(|s| s.of(&path));
                if changed_only && change.is_none() {
                    continue;
                }
                out.push(TreeRow {
                    kind: TreeRowKind::File(change),
                    path,
                    name: entry.name.clone(),
                    depth,
                });
            }
        }
        if listing.more > 0 {
            out.push(note(TreeRowKind::More(listing.more)));
        }
    }
}

// -------------------------------------------------------------------------------------------------------------
// The host

/// The plugins of every space: registries, trust, mounts, what mounted plugins keep running, the calls and
/// mounts waiting in the Inbox, and the events fired lately.
#[derive(Debug)]
pub struct PluginHost {
    data_dir: Option<PathBuf>,
    user_dir: Option<PathBuf>,
    trust: TrustStore,
    /// Why the trust file could not be read, shown once.
    pub trust_error: Option<String>,
    mounts: HashMap<SpaceId, Vec<Mount>>,
    runtimes: HashMap<SpaceId, SpaceRuntime>,
    debounce: EventDebounce,
    tail_debounce: TailDebounce,
    notices: NoticeLimiter,
    /// The value of `XCTX_DATA_DIR` scratch directories are made under (see `scratch_dir_in`).
    scratch_data_dir: Option<OsString>,
    /// Write calls waiting in the Inbox, by item.
    pub pending_writes: HashMap<ItemId, PendingWrite>,
    /// Mounts waiting in the Inbox, by item.
    pub pending_mounts: HashMap<ItemId, PendingMount>,
    secrets: HashMap<SpaceId, BTreeMap<String, String>>,
    shown_problems: HashSet<String>,
    events_tx: Sender<TailEvent>,
    events_rx: Receiver<TailEvent>,
    /// Set by tails whenever a line arrives; the workspace redraws and clears it.
    pub dirty: Arc<AtomicBool>,
}

impl PluginHost {
    /// A host keeping its files under `data_dir`, loading the user's plugins from `user_dir`.
    pub fn new(data_dir: Option<PathBuf>, user_dir: Option<PathBuf>) -> PluginHost {
        let (trust, trust_error) = TrustStore::load(data_dir.as_deref());
        let (events_tx, events_rx) = channel();
        let scratch_data_dir = data_dir.as_ref().map(|d| d.clone().into_os_string());
        PluginHost {
            data_dir,
            user_dir,
            trust,
            trust_error,
            mounts: HashMap::new(),
            runtimes: HashMap::new(),
            debounce: EventDebounce::default(),
            tail_debounce: TailDebounce::default(),
            notices: NoticeLimiter::default(),
            scratch_data_dir,
            pending_writes: HashMap::new(),
            pending_mounts: HashMap::new(),
            secrets: HashMap::new(),
            shown_problems: HashSet::new(),
            events_tx,
            events_rx,
            dirty: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Make scratch directories (where `page.screenshot` saves) as the rest of the app does, from this value of
    /// `XCTX_DATA_DIR` (see `scratch_dir_in`). A new host uses its own data directory, so tests stay in it.
    pub fn set_scratch_data_dir(&mut self, data_dir: Option<OsString>) {
        self.scratch_data_dir = data_dir;
    }

    /// The plugins `space` can use (see [`space_registry`]).
    pub fn registry(&self, space: &Space) -> Registry {
        space_registry(self.user_dir.as_deref(), space, &self.trust)
    }

    /// The built-in plugins and the user's, for requests that name no space.
    pub fn base_registry(&self) -> Registry {
        let mut registry = Registry::with_builtins();
        if let Some(dir) = &self.user_dir {
            registry.add_user_dir(dir);
        }
        registry
    }

    /// The load problems of `space`'s registry not shown before.
    pub fn new_problems(&mut self, space: &Space) -> Vec<String> {
        let messages = problem_messages(&self.registry(space));
        messages
            .into_iter()
            .filter(|m| self.shown_problems.insert(m.clone()))
            .collect()
    }

    /// Learn the secret values mise found for `space` (by env name), which tool calls may be given.
    pub fn learn_secrets(&mut self, space: &SpaceId, secrets: &BTreeMap<String, String>) {
        self.secrets
            .entry(space.clone())
            .or_default()
            .extend(secrets.iter().map(|(k, v)| (k.clone(), v.clone())));
    }

    /// The secrets mise found for `space`.
    pub fn secrets(&self, space: &SpaceId) -> BTreeMap<String, String> {
        self.secrets.get(space).cloned().unwrap_or_default()
    }

    /// The plugins mounted in `space`, loaded from its `plugins.json` on first use.
    pub fn mounts(&mut self, space: &SpaceId) -> Result<&[Mount], String> {
        if !self.mounts.contains_key(space) {
            let loaded = match &self.data_dir {
                Some(dir) => load_mounts(dir, space.as_str())?,
                None => Vec::new(),
            };
            self.mounts.insert(space.clone(), loaded);
        }
        Ok(self
            .mounts
            .get(space)
            .map(Vec::as_slice)
            .unwrap_or_default())
    }

    fn change_mounts(
        &mut self,
        space: &SpaceId,
        change: impl FnOnce(&mut Vec<Mount>),
    ) -> Result<(), String> {
        let mut mounts = self.mounts(space)?.to_vec();
        change(&mut mounts);
        if let Some(dir) = &self.data_dir {
            save_mounts(dir, space.as_str(), &mounts)?;
        }
        self.mounts.insert(space.clone(), mounts);
        Ok(())
    }

    /// Record `mount` in its space.
    pub fn add_mount(&mut self, space: &SpaceId, mount: Mount) -> Result<(), String> {
        self.change_mounts(space, |mounts| {
            mounts.retain(|m| m.pane != mount.pane);
            mounts.push(mount);
        })
    }

    /// Forget the mount of `pane`, returning it.
    pub fn remove_mount(&mut self, space: &SpaceId, pane: PaneId) -> Result<Option<Mount>, String> {
        let found = self.mounts(space)?.iter().find(|m| m.pane == pane).cloned();
        if found.is_some() {
            self.change_mounts(space, |mounts| mounts.retain(|m| m.pane != pane))?;
        }
        Ok(found)
    }

    /// Bring `space`'s mounts in step with its layout's panes (see [`reconcile_mounts`]), saving when they changed.
    pub fn reconcile(&mut self, space: &SpaceId, panes: &[RolePane]) -> Result<(), String> {
        let mut mounts = self.mounts(space)?.to_vec();
        if reconcile_mounts(&mut mounts, panes) {
            self.change_mounts(space, |m| *m = mounts)?;
        }
        Ok(())
    }

    /// The mount of `plugin` in `space` to call it through: `pane` when given (it must be that plugin's), else
    /// its first pane.
    pub fn mount_of(
        &mut self,
        space: &SpaceId,
        plugin: &str,
        pane: Option<PaneId>,
    ) -> Result<Mount, RpcError> {
        let mounts = self.mounts(space).map_err(RpcError::internal)?;
        mounts
            .iter()
            .find(|m| m.plugin == plugin && pane.is_none_or(|p| p == m.pane))
            .cloned()
            .ok_or_else(|| {
                RpcError::conflict(format!(
                    "{plugin} is not mounted in {space}; mount it first (xctx-ctl plugin.mount space={space} plugin={plugin})"
                ))
            })
    }

    /// The spaces of `spaces` that have `plugin` mounted, by slug.
    pub fn mounted_in(&mut self, spaces: &[Space], plugin: &str) -> Vec<String> {
        spaces
            .iter()
            .filter(|s| {
                self.mounts(&s.id)
                    .is_ok_and(|mounts| mounts.iter().any(|m| m.plugin == plugin))
            })
            .map(|s| s.id.as_str().to_owned())
            .collect()
    }

    /// What mounting `plugin` in `space` needs first. `from_agent` is true for `plugin.mount` (an agent asks):
    /// a connector with write tools then asks the user; the user mounting it from the Plugins menu does not. An
    /// untrusted repo plugin always asks to be trusted.
    pub fn mount_check(
        &self,
        space: &Space,
        plugin: &str,
        from_agent: bool,
    ) -> Result<(MountCheck, Manifest), RpcError> {
        let registry = self.registry(space);
        let found = registry.get(plugin).ok_or_else(|| {
            let names: Vec<&str> = registry.plugins().iter().map(Plugin::name).collect();
            RpcError::not_found(format!(
                "no plugin named {plugin}; {} can mount {}",
                space.id,
                names.join(", ")
            ))
        })?;
        if !found.trusted {
            let repo = plugin_repo(space, found)
                .map(|r| r.name.clone())
                .unwrap_or_default();
            let hash = found.manifest_hash.clone().ok_or_else(|| {
                RpcError::internal(format!(
                    "{plugin}: the registry kept no hash of its manifest"
                ))
            })?;
            return Ok((
                MountCheck::NeedsTrust { repo, hash },
                found.manifest.clone(),
            ));
        }
        if from_agent && let Some(reason) = mount_gate_reason(&found.manifest) {
            return Ok((MountCheck::NeedsApproval(reason), found.manifest.clone()));
        }
        Ok((MountCheck::Ready, found.manifest.clone()))
    }

    /// Trust `space`'s repo plugin `plugin`, whose manifest the user was asked about when it hashed to `asked`
    /// (the registry's hash of the bytes it parsed, recorded in the gate). Refused when nothing was recorded, and
    /// when the manifest the registry reads now hashes differently: the user must be asked about the new one.
    pub fn trust_repo_plugin(
        &mut self,
        space: &Space,
        plugin: &str,
        asked: &str,
    ) -> Result<(), String> {
        if asked.trim().is_empty() {
            return Err(format!(
                "this request does not record which manifest of {plugin} it was about; reject it and mount {plugin} again to be asked"
            ));
        }
        let registry = self.registry(space);
        let found = registry
            .get(plugin)
            .ok_or_else(|| format!("{} has no plugin named {plugin} any more", space.id))?;
        let repo = plugin_repo(space, found)
            .ok_or_else(|| format!("{plugin} is not one of {}'s repo plugins", space.id))?;
        match found.manifest_hash.as_deref() {
            Some(hash) if hash == asked => self.trust.trust(&repo.repo, plugin, hash),
            _ => Err(format!(
                "the manifest of {plugin} changed since you were asked; mount it again to be asked about the new one"
            )),
        }
    }

    /// The runtime of `space`, made on first use (its http history loaded from disk; a damaged history, or one
    /// from a newer xctx, is moved aside to `http.json.bad` and the history starts empty).
    pub fn runtime(&mut self, space: &SpaceId) -> &mut SpaceRuntime {
        let data_dir = self.data_dir.clone();
        self.runtimes.entry(space.clone()).or_insert_with(|| {
            let history = data_dir
                .as_deref()
                .and_then(|d| match xctx_http::load(d, space) {
                    Ok(history) => Some(history),
                    Err(_) => {
                        if let Ok(path) = xctx_http::http_path(d, space.as_str()) {
                            let _ = xctx_http::quarantine(&path);
                        }
                        None
                    }
                })
                .unwrap_or_else(|| HttpHistory::new(space.clone()));
            SpaceRuntime {
                http: Arc::new(Mutex::new(history)),
                http_state: HttpState::default(),
                logs: HashMap::new(),
                sql: HashMap::new(),
                web: WebState::default(),
            }
        })
    }

    /// Start what `mount`'s pane keeps running, for a logs pane: its tail. Other kinds start nothing here (a
    /// terminal pane's command runs in its terminal; http, sql and web need nothing until used). Returns why
    /// the tail could not start, which its pane also shows.
    pub fn start_backend(
        &mut self,
        space: &Space,
        mount: &Mount,
        manifest: &Manifest,
        redactor: &Redactor,
        path_env: Option<String>,
    ) -> Option<String> {
        if !matches!(manifest.pane, PaneSpec::Logs { .. }) {
            return None;
        }
        let ctx = call_context(
            space,
            &mount.repo,
            &self.secrets(&space.id),
            redactor,
            manifest,
        );
        let buffer = LogBuffer::new(LOG_CAPACITY);
        let target = TailTarget {
            space: space.id.clone(),
            pane: mount.pane,
            manifest: manifest.clone(),
            redactor: redactor.clone(),
            events: self.events_tx.clone(),
            dirty: Arc::clone(&self.dirty),
        };
        let (tail, error) = match pane_settings(manifest, mount, &ctx) {
            Err(e) => (LogTail::failed(buffer, String::new(), e.clone()), Some(e)),
            Ok(settings) => match (settings.logs_command, settings.logs_file) {
                (Some(argv), _) => {
                    let mut extra: BTreeMap<String, String> = space.env().into_iter().collect();
                    extra.insert(
                        xctx_plugin::SPACE_ENV.to_owned(),
                        space.id.as_str().to_owned(),
                    );
                    // A cleared environment: a few of xctx's variables, the panes' PATH, the space's.
                    let env = isolated_env(process_env(), path_env, &extra);
                    match LogTail::command(&argv, &ctx.worktree, &env, buffer.clone(), target) {
                        Ok(tail) => (tail, None),
                        Err(e) => {
                            let e = redactor.redact(&e).into_owned();
                            (LogTail::failed(buffer, argv.join(" "), e.clone()), Some(e))
                        }
                    }
                }
                (None, Some(file)) => {
                    let path = if Path::new(&file).is_absolute() {
                        PathBuf::from(&file)
                    } else {
                        ctx.worktree.join(&file)
                    };
                    (LogTail::file(path, buffer, target), None)
                }
                (None, None) => {
                    let why = "choose a command or a file for the logs pane to tail".to_owned();
                    (
                        LogTail::failed(buffer, String::new(), why.clone()),
                        Some(why),
                    )
                }
            },
        };
        self.runtime(&space.id).logs.insert(mount.pane, tail);
        error
    }

    /// Stop what `pane` of `space` keeps running. Returns whether the space's browser is no longer needed (no
    /// other web pane of the space is left), for the window to close its live view.
    pub fn stop_backend(&mut self, space: &Space, pane: PaneId) -> bool {
        let registry = self.registry(space);
        let uses_browser = self
            .mounts(&space.id)
            .map(|mounts| {
                mounts.iter().any(|m| {
                    m.pane != pane
                        && registry
                            .get(&m.plugin)
                            .is_some_and(|p| matches!(p.manifest.pane, PaneSpec::Web { .. }))
                })
            })
            .unwrap_or(false);
        if let Some(runtime) = self.runtimes.get_mut(&space.id) {
            runtime.logs.remove(&pane);
            runtime.sql.remove(&pane);
            if !uses_browser {
                runtime.web = WebState::default();
            }
        }
        !uses_browser
    }

    /// Stop everything `space`'s plugins keep running (it closes, ships or is discarded).
    pub fn stop_space(&mut self, space: &SpaceId) {
        self.runtimes.remove(space);
    }

    /// Stop everything every space's plugins keep running (xctx quits).
    pub fn stop_all(&mut self) {
        self.runtimes.clear();
    }

    /// The events tails matched since the last call, without those of the same event that fired from a tail
    /// moments ago (see [`TailDebounce`]).
    /// Events from a pane whose mount is gone (it was unmounted while its lines were in flight) are dropped.
    pub fn drain_tail_events(&mut self, now: Instant) -> Vec<TailEvent> {
        let events: Vec<TailEvent> = self.events_rx.try_iter().collect();
        events
            .into_iter()
            .filter(|e| {
                self.mounts.get(&e.space).is_some_and(|mounts| {
                    mounts
                        .iter()
                        .any(|m| m.pane == e.pane && m.plugin == e.plugin)
                })
            })
            .filter(|e| {
                self.tail_debounce
                    .admit(e.space.as_str(), &e.plugin, &e.event, now)
            })
            .collect()
    }

    /// Forget space `id`, which was shipped or discarded: its mounts, runtime, waiting calls and mounts, and
    /// secrets. Returns the files to remove in the background (see [`space_plugin_files`]).
    pub fn forget_space(&mut self, id: &SpaceId) -> Vec<PathBuf> {
        self.mounts.remove(id);
        self.runtimes.remove(id);
        self.secrets.remove(id);
        self.pending_writes.retain(|_, p| &p.space != id);
        self.pending_mounts.retain(|_, p| &p.space != id);
        self.data_dir
            .as_deref()
            .map(|dir| space_plugin_files(dir, id.as_str()))
            .unwrap_or_default()
    }

    /// Check a call of `plugin`'s `tool` with `args` in `space` (through `pane` when given) and get it ready: a
    /// write not yet `approved` becomes an Inbox gate; anything else a [`CallJob`]. Refused when the plugin is not
    /// known, not trusted or not mounted in the space, and when the plan refuses the call (unknown tool or
    /// parameter, a `MISE_ENV` outside its scope, a secret it needs that the space lacks).
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_call(
        &mut self,
        space: &Space,
        plugin: &str,
        tool: &str,
        args: &Map<String, Value>,
        redactor: &Redactor,
        approved: bool,
        pane: Option<PaneId>,
    ) -> Result<Prepared, RpcError> {
        let registry = self.registry(space);
        let found = registry
            .get(plugin)
            .ok_or_else(|| RpcError::not_found(format!("no plugin named {plugin}")))?;
        if !found.trusted {
            return Err(RpcError::conflict(format!(
                "{plugin} is a repo plugin the user has not trusted yet"
            )));
        }
        let manifest = found.manifest.clone();
        let mount = self.mount_of(&space.id, plugin, pane)?;
        let ctx = call_context(
            space,
            &mount.repo,
            &self.secrets(&space.id),
            redactor,
            &manifest,
        );
        let plan = plan_call(&manifest, tool, args, &ctx)
            .map_err(|e| RpcError::invalid_params(redactor.redact(&e.to_string()).into_owned()))?;
        if plan.effect == Effect::Write && !approved {
            let rationale = str_arg(args, "reason")
                .map(|r| redactor.redact(r).into_owned())
                .filter(|r| !r.trim().is_empty());
            let shown = masked_args_summary(args, redactor).map_err(RpcError::invalid_params)?;
            let target = pane_settings(&manifest, &mount, &ctx)
                .ok()
                .and_then(|s| write_target(&s, redactor));
            let gate = PluginGate {
                space: space.id.clone(),
                plugin: plugin.to_owned(),
                kind: PluginGateKind::WriteCall {
                    tool: tool.to_owned(),
                    args: shown,
                    pane: Some(mount.pane.0),
                    target,
                },
                rationale,
            };
            let pending = PendingWrite {
                space: space.id.clone(),
                plugin: plugin.to_owned(),
                tool: tool.to_owned(),
                args: args.clone(),
                pane: Some(mount.pane),
            };
            return Ok(Prepared::Gate(gate, pending));
        }
        let env = self.call_env(space, plugin, mount.pane, &manifest, &ctx, approved)?;
        Ok(Prepared::Run(Box::new(CallJob {
            plan,
            manifest,
            env,
        })))
    }

    /// What a call through `pane` of `plugin` in `space` runs with: the pane's settings resolved in `ctx`, the
    /// space's http history, the pane's log tail or file, its database, the space's scratch directory, and the
    /// space's browser for a web plugin.
    pub fn call_env(
        &mut self,
        space: &Space,
        plugin: &str,
        pane: PaneId,
        manifest: &Manifest,
        ctx: &CallContext,
        approved: bool,
    ) -> Result<CallEnv, RpcError> {
        let mount = self.mount_of(&space.id, plugin, Some(pane))?;
        let settings = pane_settings(manifest, &mount, ctx)
            .map_err(|e| RpcError::conflict(format!("{plugin}: {e}")))?;
        let logs = self
            .runtime(&space.id)
            .logs
            .get(&pane)
            .map(|tail| LogSource::Buffer(tail.buffer.clone()))
            .or_else(|| settings.logs_file.clone().map(LogSource::File));
        Ok(CallEnv {
            data_dir: self.data_dir.clone(),
            space: space.id.clone(),
            port: space.port,
            approved,
            http: Arc::clone(&self.runtime(&space.id).http),
            http_base: settings.base_url.clone(),
            logs,
            database: settings.database.clone(),
            scratch: crate::space_graph::scratch_dir_in(
                self.scratch_data_dir.clone(),
                space.id.as_str(),
            )
            .ok(),
            web_url: settings.url.clone(),
            secrets: ctx.secrets.clone(),
            pane: Some(pane),
            user_paths: mount.user_paths,
        })
    }

    /// Whether a write restored from its gate after a restart, through `pane` (`None` when the gate recorded
    /// none), can run without guessing its mount: refused when no pane was recorded and `plugin` has several
    /// mounts in `space`, so it cannot run against the wrong database or service.
    pub fn check_restored_pane(
        &mut self,
        space: &SpaceId,
        plugin: &str,
        pane: Option<PaneId>,
    ) -> Result<(), String> {
        if pane.is_some() {
            return Ok(());
        }
        let mounts = self
            .mounts(space)?
            .iter()
            .filter(|m| m.plugin == plugin)
            .count();
        if mounts > 1 {
            return Err(format!(
                "this request does not say which of the {mounts} {plugin} panes of {space} it is for, and xctx restarted since it was asked; reject it and ask again"
            ));
        }
        Ok(())
    }

    /// Whether `message` has not been shown before (and now counts as shown).
    pub fn shown_once(&mut self, message: &str) -> bool {
        self.shown_problems.insert(message.to_owned())
    }

    /// Whether any logs pane of any space is still tailing.
    pub fn any_tails(&self) -> bool {
        self.runtimes
            .values()
            .any(|r| r.logs.values().any(|t| t.status() == TailStatus::Running))
    }

    /// Forget that a sql pane of `space` waits for write `item`.
    pub fn clear_write_pending(&mut self, space: &SpaceId, item: &ItemId) {
        if let Some(runtime) = self.runtimes.get_mut(space) {
            for state in runtime.sql.values_mut() {
                if state.write_pending.as_ref() == Some(item) {
                    state.write_pending = None;
                }
            }
        }
    }

    /// Apply what a finished call changes in its pane.
    pub fn apply_effect(&mut self, space: &SpaceId, pane: Option<PaneId>, effect: &PaneEffect) {
        let runtime = self.runtime(space);
        match effect {
            PaneEffect::None => {}
            PaneEffect::Http { selected, diff } => {
                if selected.is_some() {
                    runtime.http_state.selected = *selected;
                }
                runtime.http_state.diff = diff.clone();
                runtime.http_state.error = None;
            }
            PaneEffect::Sql(json) => {
                if let Some(pane) = pane {
                    let state = runtime.sql.entry(pane).or_default();
                    state.result = Some(json.clone());
                    state.error = None;
                }
            }
        }
    }

    /// Fire `plugin`'s `event` in `space` with `detail`, as its manifest maps it: [`PluginHost::fire_events`] with
    /// one event.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn fire_event(
        &mut self,
        state: Option<&mut SpaceGraphState>,
        inbox: Option<&mut Inbox>,
        space: &Space,
        plugin: &str,
        event: &str,
        detail: &str,
        redactor: &Redactor,
        now: Instant,
    ) -> Result<EventReport, RpcError> {
        self.fire_events(
            state,
            inbox,
            space,
            plugin,
            &[(event.to_owned(), detail.to_owned())],
            redactor,
            now,
        )
    }

    /// Fire the events one call of `plugin` in `space` raised (name, detail), as its manifest maps them.
    ///
    /// Repeats of a name are collapsed (its first detail, with a count), and at most [`MAX_EVENTS_PER_CALL`]
    /// distinct events are recorded as findings in the shared space. An event that repeats one fired within
    /// [`EVENT_DEBOUNCE`] (by its [`stable_detail`]) does nothing. The loop gets at most one action for the whole
    /// call, the strongest of the events' ([`loop_outcome`]: `fail` over `pass` over `notice`; see
    /// [`apply_loop_action`]). At most one Inbox notice is posted: when the loop newly stopped, when it waits for
    /// approval, or else for the strongest `fail` or `notice` event, once per [`NOTICE_INTERVAL`].
    ///
    /// `state` is the space's graph state; without it (it could not be loaded) only the Inbox is told. Refused for
    /// an unknown plugin.
    #[allow(clippy::too_many_arguments)]
    pub fn fire_events(
        &mut self,
        state: Option<&mut SpaceGraphState>,
        inbox: Option<&mut Inbox>,
        space: &Space,
        plugin: &str,
        events: &[(String, String)],
        redactor: &Redactor,
        now: Instant,
    ) -> Result<EventReport, RpcError> {
        let registry = self.registry(space);
        let found = registry
            .usable(plugin)
            .ok_or_else(|| RpcError::not_found(format!("no usable plugin named {plugin}")))?;
        let manifest = found.manifest.clone();
        let id = &space.id;
        let summary_of = |name: &str, detail: &str, count: usize| {
            let times = if count > 1 {
                format!(" (×{count})")
            } else {
                String::new()
            };
            if detail.is_empty() {
                format!("{plugin}: {name}{times}")
            } else {
                format!("{plugin}: {name}{times}: {detail}")
            }
        };
        let mut admitted: Vec<(CollapsedEvent, Option<LoopAction>)> = Vec::new();
        for mut event in collapse_events(events) {
            event.detail = cut_chars(
                redactor.redact(&event.detail).trim(),
                MAX_EVENT_DETAIL_CHARS,
            );
            if self.debounce.admit(
                id.as_str(),
                plugin,
                &event.name,
                &stable_detail(&event.detail),
                now,
            ) {
                let action = manifest.action_for(&event.name);
                admitted.push((event, action));
            }
        }
        let fired: Vec<FiredEvent> = admitted
            .iter()
            .filter_map(|(e, action)| {
                action.map(|action| FiredEvent {
                    name: e.name.clone(),
                    action,
                    line: e.detail.clone(),
                })
            })
            .collect();
        let action = loop_outcome(&fired);
        // The event the call is summed up by: the first with the loop action, else the first admitted.
        let lead = admitted
            .iter()
            .find(|(_, a)| action.is_some() && *a == action)
            .or_else(|| admitted.first())
            .map(|(e, _)| e.clone());
        let others = admitted.len().saturating_sub(1);
        let summary = match &lead {
            Some(e) => {
                let mut summary = summary_of(&e.name, &e.detail, e.count);
                if others > 0 {
                    summary.push_str(&format!(
                        " (and {others} other event{})",
                        if others == 1 { "" } else { "s" }
                    ));
                }
                summary
            }
            None => match events.first() {
                Some((name, _)) => format!("{plugin}: {name}"),
                None => format!("{plugin}: no events"),
            },
        };
        let mut report = EventReport {
            action,
            debounced: lead.is_none(),
            loop_effect: LoopEffect::Unchanged,
            findings: Vec::new(),
            notice: None,
            summary: summary.clone(),
            errors: Vec::new(),
        };
        let Some(lead) = lead else {
            return Ok(report);
        };
        if let Some(state) = state {
            for (event, event_action) in admitted.iter().take(MAX_EVENTS_PER_CALL) {
                match state.shared_mut().add_finding(
                    &event_author(plugin),
                    event_severity(*event_action),
                    None,
                    None,
                    &summary_of(&event.name, &event.detail, event.count),
                ) {
                    Ok(finding) => report.findings.push(finding),
                    Err(e) => {
                        report
                            .errors
                            .push(format!("could not record the finding: {e}"));
                        break;
                    }
                }
            }
            if let Some(action) = action {
                match state.change(|doc| {
                    apply_loop_action(&mut doc.loop_run, action, &summary)
                        .map_err(|e| RpcError::conflict(e.to_string()))
                }) {
                    Ok(effect) => report.loop_effect = effect,
                    Err(e) => report.errors.push(e.message),
                }
            }
        }
        let body = (!lead.detail.is_empty()).then(|| {
            if lead.count > 1 {
                format!("{} (×{})", lead.detail, lead.count)
            } else {
                lead.detail.clone()
            }
        });
        let notice = match (action, report.loop_effect) {
            (_, LoopEffect::Advanced(LoopDecision::NeedsApproval(stage))) => {
                Some(loop_approval_notice(id, stage))
            }
            (Some(LoopAction::Fail), LoopEffect::Blocked) => {
                // Always told; and it counts, so the same failure while stopped does not post again soon.
                self.notices.admit(id.as_str(), plugin, &lead.name, now);
                Some(Notice {
                    title: format!("{plugin}: {} stopped the loop of {id}", lead.name),
                    body,
                    level: Level::Error,
                })
            }
            (Some(LoopAction::Fail), _)
                if self.notices.admit(id.as_str(), plugin, &lead.name, now) =>
            {
                Some(Notice {
                    title: format!("{plugin}: {} in {id}", lead.name),
                    body,
                    level: Level::Error,
                })
            }
            (Some(LoopAction::Notice), _)
                if self.notices.admit(id.as_str(), plugin, &lead.name, now) =>
            {
                Some(Notice {
                    title: format!("{plugin}: {} in {id}", lead.name),
                    body,
                    level: Level::Warning,
                })
            }
            _ => None,
        };
        if let (Some(notice), Some(inbox)) = (notice, inbox) {
            match inbox.post(ItemBody::Notice(notice), Some(id.clone()), Source::App) {
                Ok(item) => report.notice = Some(item),
                Err(e) => report
                    .errors
                    .push(format!("could not post to the Inbox: {e}")),
            }
        }
        Ok(report)
    }

    /// What `space`'s API client pane shows.
    pub fn http_data(&mut self, space: &Space, redactor: &Redactor) -> HttpPaneData {
        let base_url = self.mounted_kind(space, redactor, |m, s| match &m.pane {
            PaneSpec::Http { .. } => s
                .base_url
                .as_deref()
                .map(|u| xctx_http::mask_url(u, redactor)),
            _ => None,
        });
        let runtime = self.runtime(&space.id);
        let history = lock(&runtime.http)
            .exchanges
            .iter()
            .map(|e| e.masked(redactor))
            .collect();
        HttpPaneData {
            base_url,
            history,
            selected: runtime.http_state.selected,
            diff: runtime.http_state.diff.clone(),
            busy: runtime.http_state.busy > 0,
            error: runtime.http_state.error.clone(),
        }
    }

    fn mounted_kind<T>(
        &mut self,
        space: &Space,
        redactor: &Redactor,
        pick: impl Fn(&Manifest, &PaneSettings) -> Option<T>,
    ) -> Option<T> {
        let mounts = self.mounts(&space.id).ok()?.to_vec();
        let registry = self.registry(space);
        let secrets = self.secrets(&space.id);
        mounts.iter().find_map(|mount| {
            let manifest = &registry.usable(&mount.plugin)?.manifest;
            let ctx = call_context(space, &mount.repo, &secrets, redactor, manifest);
            let settings = pane_settings(manifest, mount, &ctx).ok()?;
            pick(manifest, &settings)
        })
    }

    /// The pane kind (`terminal`, `http`, `logs`, `sql`, `web` or `files`) of plugin pane `pane` of `space` and its
    /// resolved settings (a base URL or page with `{PORT}` filled, say); `None` when the pane has no mount or
    /// its plugin is not usable. The settings are `None` when they cannot be filled.
    pub fn mounted_pane(
        &mut self,
        space: &Space,
        pane: PaneId,
        redactor: &Redactor,
    ) -> Option<(&'static str, Option<PaneSettings>)> {
        let mount = self
            .mounts(&space.id)
            .ok()?
            .iter()
            .find(|m| m.pane == pane)
            .cloned()?;
        let registry = self.registry(space);
        let manifest = registry.usable(&mount.plugin)?.manifest.clone();
        let ctx = call_context(
            space,
            &mount.repo,
            &self.secrets(&space.id),
            redactor,
            &manifest,
        );
        Some((
            manifest.pane.kind(),
            pane_settings(&manifest, &mount, &ctx).ok(),
        ))
    }

    /// What logs pane `pane` of `space` shows; `None` when it has no tail.
    pub fn logs_data(
        &self,
        space: &SpaceId,
        pane: PaneId,
        redactor: &Redactor,
    ) -> Option<LogsPaneData> {
        let tail = self.runtimes.get(space)?.logs.get(&pane)?;
        let mut lines = tail.buffer.snapshot();
        for line in &mut lines {
            if let std::borrow::Cow::Owned(masked) = redactor.redact(&line.text) {
                line.text = masked;
            }
        }
        Some(LogsPaneData {
            source: redactor.redact(&tail.source).into_owned(),
            lines,
            status: tail.status(),
        })
    }

    /// Drop the lines of logs pane `pane`.
    pub fn logs_clear(&self, space: &SpaceId, pane: PaneId) -> bool {
        match self.runtimes.get(space).and_then(|r| r.logs.get(&pane)) {
            Some(tail) => {
                tail.buffer.clear();
                true
            }
            None => false,
        }
    }

    /// What sql pane `pane` of `space` shows.
    pub fn sql_data(&mut self, space: &Space, pane: PaneId, redactor: &Redactor) -> SqlPaneData {
        let database = self
            .mounts(&space.id)
            .ok()
            .and_then(|m| m.iter().find(|m| m.pane == pane).cloned())
            .and_then(|mount| {
                let registry = self.registry(space);
                let manifest = registry.usable(&mount.plugin)?.manifest.clone();
                let ctx = call_context(
                    space,
                    &mount.repo,
                    &self.secrets(&space.id),
                    redactor,
                    &manifest,
                );
                pane_settings(&manifest, &mount, &ctx).ok()?.database
            });
        let state = self
            .runtime(&space.id)
            .sql
            .get(&pane)
            .cloned()
            .unwrap_or_default();
        SqlPaneData {
            engine: database
                .as_ref()
                .map(|d| match d.engine {
                    SqlEngine::Sqlite => "sqlite",
                    SqlEngine::Postgres => "postgres",
                })
                .unwrap_or_default()
                .to_owned(),
            database: database
                .map(|d| xctx_http::mask_url(&d.location, redactor))
                .unwrap_or_default(),
            result: state.result.as_ref().map(SqlTable::from_json),
            error: state.error,
            busy: state.busy > 0,
            write_pending: state.write_pending.is_some(),
        }
    }

    /// The browser pane's state in `space` (agent calls running, the last one's error).
    pub fn web_state(&self, space: &SpaceId) -> WebState {
        self.runtimes
            .get(space)
            .map(|r| r.web.clone())
            .unwrap_or_default()
    }

    /// The plugins mounted in `space`, in mount order, their settings masked with `redactor` (see
    /// [`MountSettings::masked`]).
    pub fn mounted(&mut self, space: &Space, redactor: &Redactor) -> Vec<MountedPlugin> {
        let mounts = self
            .mounts(&space.id)
            .map(<[Mount]>::to_vec)
            .unwrap_or_default();
        let registry = self.registry(space);
        mounts
            .into_iter()
            .map(|m| MountedPlugin {
                kind: registry
                    .get(&m.plugin)
                    .map_or("", |p| p.manifest.pane.kind()),
                pane: m.pane,
                plugin: m.plugin,
                repo: m.repo,
                settings: m.settings.masked(redactor),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use xctx_core::{SpaceRepo, SpaceShape, SpaceStatus};
    use xctx_graph::{AutonomyLevel, LoopStage};
    use xctx_layout::PaneRole;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn space(root: &Path) -> Space {
        Space {
            id: SpaceId::new("fix-retry"),
            title: "fix retry".into(),
            task: "fix retry".into(),
            shape: SpaceShape::Dev,
            status: SpaceStatus::Active,
            port: 4011,
            created_at: 0,
            repos: vec![SpaceRepo {
                name: "api".into(),
                repo: root.join("api"),
                worktree: root.join("api-worktrees/fix-retry"),
                branch: "space/fix-retry".into(),
                base_branch: "main".into(),
                base_commit: "abc".into(),
                branch_created: true,
                reason: "named".into(),
            }],
            production: false,
        }
    }

    #[test]
    fn config_dirs_follow_the_override_then_home() {
        assert_eq!(
            config_dir_in(Some("/tmp/p".into()), Some("/Users/me".into()), "plugins"),
            Some(PathBuf::from("/tmp/p"))
        );
        assert_eq!(
            config_dir_in(Some("".into()), Some("/Users/me".into()), "runners"),
            Some(PathBuf::from("/Users/me/.config/xctx/runners"))
        );
        assert_eq!(config_dir_in(None, None, "plugins"), None);
    }

    #[test]
    fn plain_settings_come_from_the_environment_and_secrets_never_do() -> TestResult {
        let jira = xctx_plugin::builtin_manifests()
            .into_iter()
            .filter_map(Result::ok)
            .find(|m| m.name == "jira")
            .ok_or("no jira")?;
        let env = |name: &str| match name {
            "JIRA_BASE_URL" => Some("http://127.0.0.1:9/jira".to_owned()),
            "JIRA_EMAIL" => Some("from-env@x.invalid".to_owned()),
            "JIRA_API_TOKEN" => Some("tok-from-env".to_owned()),
            _ => None,
        };
        // mise reported the email (it redacts it): its value wins; the base URL comes from the environment; the
        // token is a secret and is never read from the environment.
        let known: BTreeMap<String, String> = [("JIRA_EMAIL", "me@x.invalid")]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let plain = plain_settings(&jira, &known, env);
        assert_eq!(
            plain.into_iter().collect::<Vec<_>>(),
            [(
                "JIRA_BASE_URL".to_owned(),
                "http://127.0.0.1:9/jira".to_owned()
            )]
        );
        // Unset or empty values are left out, so the call still says which setting is missing.
        let empty = plain_settings(&jira, &BTreeMap::new(), |name| {
            (name == "JIRA_BASE_URL").then(String::new)
        });
        assert!(empty.is_empty());
        Ok(())
    }

    #[test]
    fn the_context_carries_the_space_and_only_declared_secrets() -> TestResult {
        let root = Path::new("/code");
        let mut space = space(root);
        space.production = true;
        let manifest = Manifest::parse(xctx_plugin::BUILTIN_MANIFESTS[0].1, "t")?;
        let jira = xctx_plugin::builtin_manifests()
            .into_iter()
            .filter_map(Result::ok)
            .find(|m| m.name == "jira")
            .ok_or("no jira")?;
        let all: BTreeMap<String, String> = [
            ("JIRA_API_TOKEN", "tok-123"),
            ("JIRA_EMAIL", "me@x.invalid"),
            ("STRIPE_KEY", "sk_live_1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let redactor = Redactor::new(all.clone());
        let ctx = call_context(&space, "api", &all, &redactor, &jira);
        assert_eq!(ctx.worktree, root.join("api-worktrees/fix-retry"));
        assert_eq!(ctx.port, Some(4011));
        assert_eq!(ctx.mise_env, "production,fix-retry");
        assert!(ctx.space_production);
        assert_eq!(
            ctx.secrets.keys().collect::<Vec<_>>(),
            ["JIRA_API_TOKEN", "JIRA_EMAIL"]
        );
        let none = call_context(&space, "gone", &all, &redactor, &manifest);
        assert!(none.secrets.is_empty());
        assert_eq!(none.worktree, root.join("api-worktrees/fix-retry"));
        Ok(())
    }

    #[test]
    fn trust_is_by_repo_plugin_and_manifest_hash() -> TestResult {
        let dir = tempfile::tempdir()?;
        let (mut trust, error) = TrustStore::load(Some(dir.path()));
        assert!(error.is_none());
        let repo = Path::new("/code/api");
        let hash = manifest_hash(b"name = \"probe\"\n");
        assert_eq!(hash.len(), 64);
        assert_ne!(hash, manifest_hash(b"name = \"probe\" \n"));
        assert!(!trust.is_trusted(repo, "probe", &hash));
        trust.trust(repo, "probe", &hash)?;
        assert!(trust.is_trusted(repo, "probe", &hash));
        assert!(!trust.is_trusted(Path::new("/code/web"), "probe", &hash));
        let (reloaded, _) = TrustStore::load(Some(dir.path()));
        assert!(reloaded.is_trusted(repo, "probe", &hash));
        // Trusting an edited manifest replaces the old hash.
        let edited = manifest_hash(b"edited");
        trust.trust(repo, "probe", &edited)?;
        assert!(!trust.is_trusted(repo, "probe", &hash));
        std::fs::write(dir.path().join(TRUST_FILE), "{ not json")?;
        let (broken, error) = TrustStore::load(Some(dir.path()));
        assert!(error.is_some_and(|e| e.contains("damaged")));
        assert!(!broken.is_trusted(repo, "probe", &edited));
        Ok(())
    }

    #[test]
    fn mounts_round_trip_and_follow_the_layout() -> TestResult {
        let dir = tempfile::tempdir()?;
        assert!(load_mounts(dir.path(), "s")?.is_empty());
        let mounts = vec![Mount {
            pane: PaneId(7),
            plugin: "logs".into(),
            repo: "api".into(),
            settings: MountSettings {
                command: Some(vec!["tail".into(), "-f".into(), "log/dev.log".into()]),
                ..MountSettings::default()
            },
            user_paths: true,
        }];
        save_mounts(dir.path(), "s", &mounts)?;
        assert_eq!(load_mounts(dir.path(), "s")?, mounts);
        assert!(mounts_path(dir.path(), "../x").is_err());

        let mut reconciled = mounts.clone();
        let panes = vec![
            RolePane {
                id: PaneId(7),
                role: PaneRole::for_plugin("logs", "api"),
            },
            RolePane {
                id: PaneId(9),
                role: PaneRole::for_plugin("api-client", "web"),
            },
            RolePane {
                id: PaneId(1),
                role: PaneRole::new(PaneKind::Agent, "api"),
            },
        ];
        assert!(reconcile_mounts(&mut reconciled, &panes));
        assert_eq!(reconciled.len(), 2);
        assert_eq!(reconciled[0], mounts[0]);
        assert_eq!(reconciled[1].plugin, "api-client");
        assert!(!reconcile_mounts(&mut reconciled, &panes));
        assert!(reconcile_mounts(&mut reconciled, &panes[2..]));
        assert!(reconciled.is_empty());
        Ok(())
    }

    #[test]
    fn identical_events_are_debounced_for_a_few_seconds() {
        let mut debounce = EventDebounce::default();
        let t0 = Instant::now();
        assert!(debounce.admit("s", "api-client", "response.status", "500", t0));
        assert!(!debounce.admit(
            "s",
            "api-client",
            "response.status",
            "500",
            t0 + Duration::from_secs(1)
        ));
        assert!(debounce.admit(
            "s",
            "api-client",
            "response.status",
            "503",
            t0 + Duration::from_secs(1)
        ));
        assert!(debounce.admit(
            "t",
            "api-client",
            "response.status",
            "500",
            t0 + Duration::from_secs(1)
        ));
        assert!(debounce.admit(
            "s",
            "api-client",
            "response.status",
            "500",
            t0 + EVENT_DEBOUNCE
        ));
        let mut tails = TailDebounce::default();
        assert!(tails.admit("s", "logs", "app.crash", t0));
        assert!(!tails.admit("s", "logs", "app.crash", t0 + Duration::from_secs(2)));
        assert!(tails.admit("s", "logs", "app.other", t0 + Duration::from_secs(2)));
    }

    #[test]
    fn events_map_to_loop_actions() -> TestResult {
        let mut run = LoopRun::new(AutonomyLevel::UntilChecksPass);
        // An idle loop is not started by an event.
        assert_eq!(
            apply_loop_action(&mut run, LoopAction::Fail, "x")?,
            LoopEffect::Unchanged
        );
        assert_eq!(
            apply_loop_action(&mut run, LoopAction::Pass, "x")?,
            LoopEffect::Unchanged
        );
        run.start()?;
        assert_eq!(
            apply_loop_action(&mut run, LoopAction::Pass, "x")?,
            LoopEffect::Advanced(LoopDecision::Continue(LoopStage::Test))
        );
        assert_eq!(
            apply_loop_action(&mut run, LoopAction::Notice, "x")?,
            LoopEffect::Unchanged
        );
        assert_eq!(run.stage(), LoopStage::Test);
        assert_eq!(
            apply_loop_action(&mut run, LoopAction::Fail, "api 500")?,
            LoopEffect::Blocked
        );
        assert_eq!(
            run.status(),
            &LoopStatus::Blocked {
                reason: "api 500".into()
            }
        );
        assert_eq!(run.stage(), LoopStage::Test);
        // Blocked stays blocked; a pass does not move it.
        assert_eq!(
            apply_loop_action(&mut run, LoopAction::Pass, "x")?,
            LoopEffect::Unchanged
        );

        let mut asking = LoopRun::new(AutonomyLevel::AskEachStep);
        asking.start()?;
        assert_eq!(
            apply_loop_action(&mut asking, LoopAction::Pass, "x")?,
            LoopEffect::Advanced(LoopDecision::NeedsApproval(LoopStage::Test))
        );
        assert_eq!(
            apply_loop_action(&mut asking, LoopAction::Fail, "y")?,
            LoopEffect::Blocked
        );
        Ok(())
    }

    #[test]
    fn write_arguments_are_masked_and_cut_for_the_gate() {
        let redactor = Redactor::new([("DB_PASSWORD".to_owned(), "s3cret-pw".to_owned())]);
        let mut args = Map::new();
        args.insert(
            "statement".into(),
            json!("UPDATE users SET pw = 's3cret-pw'"),
        );
        let summary = masked_args_summary(&args, &redactor).unwrap_or_default();
        assert!(summary.contains("[REDACTED:DB_PASSWORD]"), "{summary}");
        assert!(!summary.contains("s3cret-pw"));
        // Too long to show whole: refused, never cut (what runs must be what the user saw).
        let mut long = Map::new();
        long.insert("x".into(), json!("a".repeat(10_000)));
        let refused = masked_args_summary(&long, &redactor);
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.contains("send a smaller call")),
            "{refused:?}"
        );
    }

    #[test]
    fn connectors_with_writes_need_approval_to_mount() -> TestResult {
        let manifests: Vec<Manifest> = xctx_plugin::builtin_manifests()
            .into_iter()
            .collect::<Result<_, _>>()?;
        let gated: Vec<&str> = manifests
            .iter()
            .filter(|m| mount_gate_reason(m).is_some())
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(gated, ["jira"]);
        Ok(())
    }

    #[test]
    fn tools_get_a_cleared_environment_with_an_allowlist() {
        let base = [
            ("HOME", "/Users/me"),
            ("LC_ALL", "en_US.UTF-8"),
            ("TERM", "xterm"),
            ("GITHUB_TOKEN", "ghp_secret"),
            ("XCTX_CONTROL_SOCKET", "/tmp/xctx.sock"),
            ("PATH", "/usr/bin"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()));
        let extra: BTreeMap<String, String> = [("PORT", "4011"), ("HOME", "/space/home")]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .into();
        let env = isolated_env(
            base.clone(),
            Some("/opt/homebrew/bin:/usr/bin".into()),
            &extra,
        );
        assert_eq!(
            env.keys().map(String::as_str).collect::<Vec<_>>(),
            ["HOME", "LC_ALL", "PATH", "PORT", "TERM"]
        );
        assert_eq!(env["PATH"], "/opt/homebrew/bin:/usr/bin");
        // The plan's own variables win.
        assert_eq!(env["HOME"], "/space/home");
        let bare = isolated_env(base, None, &BTreeMap::new());
        assert_eq!(bare["PATH"], DEFAULT_TOOL_PATH);
        assert!(!bare.contains_key("GITHUB_TOKEN"));
    }

    #[test]
    fn elapsed_times_and_timestamps_stay_out_of_the_debounce_key() {
        assert_eq!(
            stable_detail("GET http://127.0.0.1:4011/x -> 500 in 31 ms"),
            stable_detail("GET http://127.0.0.1:4011/x -> 500 in 7 ms")
        );
        assert_eq!(
            stable_detail("2024-05-01T10:00:00Z ERROR boom (took 1.2s)"),
            "ERROR boom (took"
        );
        assert_eq!(stable_detail("10:00:00.123 panicked"), "panicked");
        // Statuses, ports and paths stay.
        assert_ne!(
            stable_detail("GET http://127.0.0.1:4011/x -> 500 in 31 ms"),
            stable_detail("GET http://127.0.0.1:4011/x -> 503 in 31 ms")
        );
    }

    #[test]
    fn a_calls_events_collapse_by_name() {
        let events: Vec<(String, String)> = [("a", "1"), ("b", "x"), ("a", "2"), ("a", "3")]
            .map(|(n, d)| (n.to_owned(), d.to_owned()))
            .into();
        assert_eq!(
            collapse_events(&events),
            [
                CollapsedEvent {
                    name: "a".into(),
                    detail: "1".into(),
                    count: 3
                },
                CollapsedEvent {
                    name: "b".into(),
                    detail: "x".into(),
                    count: 1
                },
            ]
        );
    }

    #[test]
    fn notices_of_an_event_are_rate_limited() {
        let mut limiter = NoticeLimiter::default();
        let t0 = Instant::now();
        assert!(limiter.admit("s", "logs", "app.crash", t0));
        assert!(!limiter.admit("s", "logs", "app.crash", t0 + Duration::from_secs(60)));
        assert!(limiter.admit("s", "logs", "app.other", t0 + Duration::from_secs(60)));
        assert!(limiter.admit("t", "logs", "app.crash", t0 + Duration::from_secs(60)));
        assert!(limiter.admit("s", "logs", "app.crash", t0 + NOTICE_INTERVAL));
    }

    #[test]
    fn settings_are_masked_for_display() {
        let redactor = Redactor::new([("DB_PW".to_owned(), "hunter22".to_owned())]);
        let settings = MountSettings {
            command: Some(vec!["tail".into(), "--token=hunter22".into()]),
            database: Some("postgres://app:pw-123@db.local/app".into()),
            base_url: Some("http://localhost:4011/?api_key=k-77".into()),
            ..MountSettings::default()
        };
        let shown = format!("{:?}", settings.masked(&redactor));
        for secret in ["hunter22", "pw-123", "k-77"] {
            assert!(!shown.contains(secret), "{shown}");
        }
        assert!(shown.contains("db.local"), "{shown}");
    }

    #[test]
    fn a_shipped_spaces_plugin_files_are_removed() -> TestResult {
        let dir = tempfile::tempdir()?;
        let files = space_plugin_files(dir.path(), "fix-retry");
        save_mounts(dir.path(), "fix-retry", &[])?;
        save_mounts(dir.path(), "other", &[])?;
        assert!(files.iter().all(|f| f.exists()));
        remove_paths(&files)?;
        assert!(files.iter().all(|f| !f.exists()));
        assert!(
            space_plugin_files(dir.path(), "other")
                .iter()
                .all(|f| f.exists())
        );
        // Gone already is fine; a slug that could escape is never removed.
        remove_paths(&files)?;
        assert!(space_plugin_files(dir.path(), "../x").is_empty());
        Ok(())
    }

    #[test]
    fn urls_join_the_base_unless_full() {
        assert_eq!(
            join_url("http://localhost:4011/", "/health"),
            "http://localhost:4011/health"
        );
        assert_eq!(
            join_url("http://localhost:4011", "v1/x"),
            "http://localhost:4011/v1/x"
        );
        assert_eq!(
            join_url("http://localhost:1", "http://127.0.0.1:2/a"),
            "http://127.0.0.1:2/a"
        );
    }

    #[test]
    fn pane_settings_fill_the_space_and_prefer_the_mount() -> TestResult {
        let space = space(Path::new("/code"));
        let manifests: Vec<Manifest> = xctx_plugin::builtin_manifests()
            .into_iter()
            .collect::<Result<_, _>>()?;
        let get = |name: &str| manifests.iter().find(|m| m.name == name).ok_or("missing");
        let redactor = Redactor::default();
        let mount = |plugin: &str, settings| Mount {
            pane: PaneId(1),
            plugin: plugin.into(),
            repo: "api".into(),
            settings,
            user_paths: false,
        };
        let http = get("api-client")?;
        let ctx = call_context(&space, "api", &BTreeMap::new(), &redactor, http);
        let s = pane_settings(http, &mount("api-client", MountSettings::default()), &ctx)?;
        assert_eq!(s.base_url.as_deref(), Some("http://localhost:4011"));
        let db = get("database")?;
        let s = pane_settings(db, &mount("database", MountSettings::default()), &ctx)?;
        assert_eq!(
            s.database.map(|d| d.location),
            Some("/code/api-worktrees/fix-retry/db/development.sqlite3".to_owned())
        );
        let logs = get("logs")?;
        let s = pane_settings(
            logs,
            &mount(
                "logs",
                MountSettings {
                    command: Some(vec!["tail".into(), "-f".into(), "{WORKTREE}/log".into()]),
                    ..MountSettings::default()
                },
            ),
            &ctx,
        )?;
        assert_eq!(
            s.logs_command,
            Some(vec![
                "tail".into(),
                "-f".into(),
                "/code/api-worktrees/fix-retry/log".into()
            ])
        );
        let terminal = get("terminal")?;
        let s = pane_settings(terminal, &mount("terminal", MountSettings::default()), &ctx)?;
        assert_eq!(s.terminal, None);
        let files = get("files")?;
        let s = pane_settings(files, &mount("files", MountSettings::default()), &ctx)?;
        assert_eq!(
            s.files_root.as_deref(),
            Some(Path::new("/code/api-worktrees/fix-retry")),
            "the files pane shows the repo's worktree, not its main checkout"
        );
        Ok(())
    }

    fn web_env(web_url: Option<&str>) -> CallEnv {
        CallEnv {
            data_dir: None,
            space: SpaceId::new("fix-retry"),
            port: 4011,
            approved: false,
            http: Arc::new(Mutex::new(HttpHistory::new(SpaceId::new("fix-retry")))),
            http_base: None,
            logs: None,
            database: None,
            scratch: None,
            web_url: web_url.map(str::to_owned),
            secrets: BTreeMap::new(),
            pane: None,
            user_paths: false,
        }
    }

    fn page_args(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn page_open_stays_on_the_spaces_app() {
        let env = web_env(None);
        let open = |path: &str| page_action("page.open", &page_args(json!({ "path": path })), &env);
        assert_eq!(
            open("/login"),
            Ok(PageAction::Open {
                url: "http://localhost:4011/login".into()
            })
        );
        assert_eq!(
            open("http://127.0.0.1:4011/x"),
            Ok(PageAction::Open {
                url: "http://127.0.0.1:4011/x".into()
            })
        );
        for elsewhere in ["https://google.com/", "http://localhost:4012/"] {
            let refused = open(elsewhere);
            assert!(
                refused.as_ref().is_err_and(|e| e.starts_with("page.open:")),
                "{elsewhere}: {refused:?}"
            );
        }
        // Anything that is not an http(s) URL is a path of the app.
        assert_eq!(
            open("file:///etc/hosts"),
            Ok(PageAction::Open {
                url: "http://localhost:4011/file:///etc/hosts".into()
            })
        );
        // A pane mounted on another page adds that origin, and paths are relative to it.
        let staged = web_env(Some("https://staging.example.com/app"));
        assert_eq!(
            page_action("page.open", &page_args(json!({ "path": "/x" })), &staged),
            Ok(PageAction::Open {
                url: "https://staging.example.com/app/x".into()
            })
        );
        assert!(page_action("page.open", &Map::new(), &env).is_err());
    }

    #[test]
    fn page_tools_read_their_arguments() {
        let env = web_env(None);
        let action = |name: &str, value: Value| page_action(name, &page_args(value), &env);
        assert_eq!(
            action("page.click", json!({ "selector": "#go" })),
            Ok(PageAction::Click {
                selector: "#go".into()
            })
        );
        assert_eq!(
            action(
                "page.type",
                json!({ "selector": "#q", "text": "hi", "submit": true })
            ),
            Ok(PageAction::Type {
                selector: "#q".into(),
                text: "hi".into(),
                submit: true
            })
        );
        assert_eq!(
            action("page.text", json!({ "selector": "  " })),
            Ok(PageAction::Text { selector: None })
        );
        assert_eq!(
            action("page.screenshot", json!({ "full_page": true })),
            Ok(PageAction::Screenshot { full_page: true })
        );
        assert_eq!(
            action("page.console", json!({})),
            Ok(PageAction::Console { errors_only: false })
        );
        assert_eq!(action("page.reload", json!({})), Ok(PageAction::Reload));
        assert!(action("page.click", json!({})).is_err());
        assert!(action("page.type", json!({ "selector": "#q" })).is_err());
        assert!(action("page.fly", json!({})).is_err());
    }

    #[test]
    fn page_outputs_are_masked_and_keep_their_shape() {
        let redactor = Redactor::new([("TOKEN".to_owned(), "hunter22".to_owned())]);
        let reading = PageReading {
            url: "http://localhost:4011/?t=hunter22".into(),
            console: vec![
                ConsoleEntry {
                    level: "log".into(),
                    text: "token hunter22".into(),
                    location: None,
                },
                ConsoleEntry {
                    level: "exception".into(),
                    text: "boom".into(),
                    location: Some("http://localhost:4011/app.js:1:2".into()),
                },
            ],
            network_errors: vec![NetworkError {
                url: "http://localhost:4011/api?key=hunter22".into(),
                status: Some(500),
                reason: "Internal Server Error".into(),
                resource_type: Some("fetch".into()),
            }],
        };
        let out = page_output(&reading, false, "opened", &redactor);
        assert!(!out.text.contains("hunter22") && !out.json.to_string().contains("hunter22"));
        assert!(out.json["url"].is_string());
        assert_eq!(out.json["console"].as_array().map(Vec::len), Some(2));
        assert_eq!(out.json["network_errors"][0]["status"], json!(500));
        assert!(out.text.starts_with("opened: http://localhost:4011/"));
        let errors = page_output(&reading, true, "console", &redactor);
        assert_eq!(errors.json["console"].as_array().map(Vec::len), Some(1));
        assert_eq!(errors.json["console"][0]["level"], json!("exception"));

        let read = xctx_browser::PageText {
            text: "secret hunter22".into(),
            truncated: true,
            chars: 30_000,
            url: "http://localhost:4011/".into(),
        };
        let out = text_output(&read, &redactor);
        assert!(!out.text.contains("hunter22") && !out.json.to_string().contains("hunter22"));
        assert!(
            out.text.contains("[cut at 15 of 30000 characters]"),
            "{}",
            out.text
        );
        assert_eq!(out.json["truncated"], json!(true));
        let typed = xctx_browser::Typed {
            value: None,
            url: "http://localhost:4011/done".into(),
        };
        let out = typed_output("#q", &typed, &redactor);
        assert_eq!(out.json["value"], Value::Null);
        assert!(out.text.contains("the field is gone"));
        let clicked = xctx_browser::Clicked {
            tag: "button".into(),
            text: "Pay hunter22".into(),
            url: "http://localhost:4011/".into(),
        };
        let out = clicked_output(&clicked, &redactor);
        assert_eq!(out.json["tag"], json!("button"));
        assert!(!out.json.to_string().contains("hunter22"));
        let shot = screenshot_output(&web_env(None), &[1, 2, 3]);
        assert_eq!(shot.map(|o| o.json), Ok(json!({ "path": "", "bytes": 3 })));
    }
}
