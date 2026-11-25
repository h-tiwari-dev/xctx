//! Plugins in the app, without a window: which plugins a space can use (built-in, the user's, and its repos'
//! once trusted), where they are mounted, what a tool call runs and with what, and what a plugin event does to
//! the space's loop, shared space and Inbox.
//!
//! Everything here is GPUI-free so it is tested directly (see `plugin_tests.rs`); `plugin_jobs.rs` applies it to
//! the live workspace, running [`CallJob`]s on the background executor and the results on the UI thread.
//!
//! On disk, under the data directory:
//! - `plugin-trust.json`: the repo plugins the user trusted, each by repo, plugin name and the SHA-256 of its
//!   `plugin.toml`, so an edited manifest asks again;
//! - `spaces/<slug>/plugins.json`: the plugins mounted in a space, one entry per plugin pane, with the settings
//!   chosen when mounting (a logs command, a database file, a base URL);
//! - `spaces/<slug>/http.json`: the API client's history (see `xctx_http`);
//! - `browser-profiles/<slug>/`: the Chrome profile of a space's browser pane.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use xctx_browser::{ConsoleEntry, NetworkError, ScreenshotOptions, UrlPolicy};
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
    CallContext, CallPlan, Effect, Invocation, LoopAction, MANIFEST_FILE, Manifest, PaneSpec,
    ParamKind, Plugin, REPO_PLUGINS_DIR, Registry, SqlEngine, fill_context, match_events,
    pane_argv, plan_call, run_plan,
};

use crate::plugin_backend::{
    BrowserHandle, BrowserSnapshot, LogTail, TailEvent, TailStatus, TailTarget,
};
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

/// The folder under the data directory that holds each space's Chrome profile.
pub const BROWSER_PROFILES_DIR: &str = "browser-profiles";

/// How long a tool run (`run = [...]`) may take before it is killed.
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// The most bytes of a tool's stdout (and of its stderr) kept.
pub const TOOL_MAX_OUTPUT: usize = 256 * 1024;

/// The same event, with the same detail, from the same plugin in the same space, fires once in this window.
pub const EVENT_DEBOUNCE: Duration = Duration::from_secs(5);

/// The most characters of an event's detail kept in a finding or a notice.
pub const MAX_EVENT_DETAIL_CHARS: usize = 1000;

/// The most characters of a write call's arguments shown in its Inbox gate.
pub const MAX_GATE_ARGS_CHARS: usize = 4000;

/// How many lines of a logs pane a space keeps.
pub const LOG_CAPACITY: usize = xctx_connectors::DEFAULT_LOG_CAPACITY;

/// Who findings from plugin events are by: `plugin:<name>`.
pub fn event_author(plugin: &str) -> String {
    format!("plugin:{plugin}")
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
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

/// The SHA-256 of a manifest's bytes, in hex: what trusting a repo plugin records.
pub fn manifest_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The manifest file of `repo`'s plugin `plugin` in the folder the space works in (its worktree).
pub fn repo_manifest_path(worktree: &Path, plugin: &str) -> PathBuf {
    worktree
        .join(REPO_PLUGINS_DIR)
        .join(plugin)
        .join(MANIFEST_FILE)
}

/// One trusted repo plugin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustEntry {
    /// The repo's checkout (not a space's worktree), so trust holds in every space of the repo.
    pub repo: PathBuf,
    /// The plugin's name.
    pub plugin: String,
    /// [`manifest_hash`] of the manifest the user trusted.
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
        registry.add_repo(&repo.worktree, |manifest| {
            std::fs::read(repo_manifest_path(&repo.worktree, &manifest.name)).is_ok_and(|bytes| {
                trust.is_trusted(&repo.repo, &manifest.name, &manifest_hash(&bytes))
            })
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

/// A write call's arguments as its Inbox gate shows them: compact JSON, masked, cut at
/// [`MAX_GATE_ARGS_CHARS`].
pub fn masked_args_summary(args: &Map<String, Value>, redactor: &Redactor) -> String {
    let mut value = Value::Object(args.clone());
    redactor.redact_json(&mut value);
    let text = value.to_string();
    cut_chars(&text, MAX_GATE_ARGS_CHARS)
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
    }
    Ok(out)
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
    /// The space's browser.
    pub browser: Option<BrowserHandle>,
    /// The web pane's page.
    pub web_url: Option<String>,
    /// The secrets the manifest declares.
    pub secrets: BTreeMap<String, String>,
    /// The sql pane the call's result shows in.
    pub pane: Option<PaneId>,
}

impl CallEnv {
    fn base_url(&self) -> String {
        self.http_base
            .clone()
            .unwrap_or_else(|| format!("http://localhost:{}", self.port))
    }

    fn web_base(&self) -> String {
        self.web_url
            .clone()
            .unwrap_or_else(|| format!("http://localhost:{}", self.port))
    }

    fn send_options(&self) -> SendOptions {
        let mut options = SendOptions::default();
        if let Some(base) = &self.http_base
            && let Ok(host) = xctx_http::host_of(base)
        {
            options.allow_host(&host);
        }
        options
    }

    fn url_policy(&self) -> UrlPolicy {
        let mut policy = UrlPolicy::default();
        if let Some(url) = &self.web_url {
            let _ = policy.allow_url_host(url);
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
                let output = run_plan(&self.plan, TOOL_TIMEOUT, TOOL_MAX_OUTPUT)
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
                    run_page(name, args, &self.env, redactor).map_err(mask)
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
                    redactor.redact(&exchange.request.url),
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

fn run_page(
    name: &str,
    args: &Map<String, Value>,
    env: &CallEnv,
    redactor: &Redactor,
) -> Result<CallOutput, String> {
    let browser = env
        .browser
        .as_ref()
        .ok_or("the browser plugin is not mounted in this space")?;
    let policy = env.url_policy();
    match name {
        "page.open" => {
            let path = str_arg(args, "path").ok_or("page.open: path is required")?;
            let url = join_url(&env.web_base(), path);
            browser.with_page(policy, redactor, |page| page.open(&url))?;
            let snapshot = browser.snapshot();
            Ok(page_output(&snapshot, false, "opened"))
        }
        "page.screenshot" => {
            let full_page = args
                .get("full_page")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let png = browser.with_page(policy, redactor, |page| {
                page.screenshot_with(ScreenshotOptions {
                    clip: None,
                    full_page,
                })
            })?;
            let saved = match &env.scratch {
                Some(scratch) => {
                    let rel = format!("screenshots/{}.png", now_millis());
                    scratch
                        .write_artifact(&rel, &png)
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
        "page.reload" => {
            browser.with_page(policy, redactor, |page| page.reload())?;
            Ok(page_output(&browser.snapshot(), false, "reloaded"))
        }
        "page.console" => {
            let errors_only = args
                .get("errors_only")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            browser.with_page(policy, redactor, |page| {
                page.pump(Duration::from_millis(200))
            })?;
            Ok(page_output(&browser.snapshot(), errors_only, "console"))
        }
        other => Err(format!("{other} is not a browser built-in")),
    }
}

fn page_output(snapshot: &BrowserSnapshot, errors_only: bool, what: &str) -> CallOutput {
    let console: Vec<&ConsoleEntry> = snapshot
        .console
        .iter()
        .filter(|c| !errors_only || c.level == "error" || c.is_exception())
        .collect();
    let network: &[NetworkError] = &snapshot.network_errors;
    let mut text = format!("{what}: {}", snapshot.url.as_deref().unwrap_or(""));
    for c in &console {
        text.push_str(&format!("\n[{}] {}", c.level, c.text));
    }
    for n in network {
        text.push_str(&format!(
            "\n[network] {} {} {}",
            n.url,
            n.status.map(|s| s.to_string()).unwrap_or_default(),
            n.reason
        ));
    }
    CallOutput {
        json: json!({
            "url": snapshot.url,
            "console": console,
            "network_errors": network,
        }),
        text: cut_chars(&text, TOOL_MAX_OUTPUT),
        events: Vec::new(),
        effect: PaneEffect::None,
    }
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

/// Drops repeats of an event: the same space, plugin, event and detail within [`EVENT_DEBOUNCE`].
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
    /// The finding recorded in the shared space.
    pub finding: Option<u64>,
    /// The Inbox notice posted (a notice event, a failure, or a loop now waiting for approval).
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
    /// Its pane kind: `terminal`, `http`, `logs`, `sql` or `web` (empty when the plugin is no longer known).
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
    /// The space's browser, once a web pane or a page tool needed it.
    pub browser: Option<BrowserHandle>,
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
        PluginHost {
            data_dir,
            user_dir,
            trust,
            trust_error,
            mounts: HashMap::new(),
            runtimes: HashMap::new(),
            debounce: EventDebounce::default(),
            tail_debounce: TailDebounce::default(),
            pending_writes: HashMap::new(),
            pending_mounts: HashMap::new(),
            secrets: HashMap::new(),
            shown_problems: HashSet::new(),
            events_tx,
            events_rx,
            dirty: Arc::new(AtomicBool::new(false)),
        }
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
            return Ok((MountCheck::NeedsTrust { repo }, found.manifest.clone()));
        }
        if from_agent && let Some(reason) = mount_gate_reason(&found.manifest) {
            return Ok((MountCheck::NeedsApproval(reason), found.manifest.clone()));
        }
        Ok((MountCheck::Ready, found.manifest.clone()))
    }

    /// Trust `space`'s repo plugin `plugin` as its manifest is now.
    pub fn trust_repo_plugin(&mut self, space: &Space, plugin: &str) -> Result<(), String> {
        let registry = self.registry(space);
        let found = registry
            .get(plugin)
            .ok_or_else(|| format!("{} has no plugin named {plugin} any more", space.id))?;
        let repo = plugin_repo(space, found)
            .ok_or_else(|| format!("{plugin} is not one of {}'s repo plugins", space.id))?;
        let bytes = std::fs::read(repo_manifest_path(&repo.worktree, plugin))
            .map_err(|e| format!("could not read {plugin}'s manifest: {e}"))?;
        self.trust.trust(&repo.repo, plugin, &manifest_hash(&bytes))
    }

    /// The runtime of `space`, made on first use (its http history loaded from disk; a damaged history starts
    /// empty, leaving the file in place until the next send replaces it).
    pub fn runtime(&mut self, space: &SpaceId) -> &mut SpaceRuntime {
        let data_dir = self.data_dir.clone();
        self.runtimes.entry(space.clone()).or_insert_with(|| {
            let history = data_dir
                .as_deref()
                .and_then(|d| xctx_http::load(d, space).ok())
                .unwrap_or_else(|| HttpHistory::new(space.clone()));
            SpaceRuntime {
                http: Arc::new(Mutex::new(history)),
                http_state: HttpState::default(),
                logs: HashMap::new(),
                sql: HashMap::new(),
                browser: None,
            }
        })
    }

    /// The browser of `space`, made (not started) on first use with its profile under the data directory.
    pub fn browser(&mut self, space: &SpaceId) -> BrowserHandle {
        let profile = match &self.data_dir {
            Some(dir) => dir.join(BROWSER_PROFILES_DIR).join(space.as_str()),
            None => std::env::temp_dir()
                .join("xctx-browser-profiles")
                .join(space.as_str()),
        };
        self.runtime(space)
            .browser
            .get_or_insert_with(|| BrowserHandle::new(profile))
            .clone()
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
                    let mut env: BTreeMap<String, String> = space.env().into_iter().collect();
                    env.insert(
                        xctx_plugin::SPACE_ENV.to_owned(),
                        space.id.as_str().to_owned(),
                    );
                    if let Some(path) = path_env {
                        env.insert("PATH".to_owned(), path);
                    }
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

    /// Stop what `pane` of `space` keeps running. When no other web pane of the space is left, its browser is
    /// returned for the caller to close in the background (closing blocks while a page action runs).
    pub fn stop_backend(&mut self, space: &Space, pane: PaneId) -> Option<BrowserHandle> {
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
        let runtime = self.runtimes.get_mut(&space.id)?;
        runtime.logs.remove(&pane);
        runtime.sql.remove(&pane);
        if uses_browser {
            None
        } else {
            runtime.browser.take()
        }
    }

    /// Stop everything `space`'s plugins keep running (it closes, ships or is discarded). Returns its browser
    /// to close in the background.
    pub fn stop_space(&mut self, space: &SpaceId) -> Option<BrowserHandle> {
        self.runtimes
            .remove(space)
            .and_then(|mut runtime| runtime.browser.take())
    }

    /// Stop everything every space's plugins keep running (xctx quits). Returns the browsers to close.
    pub fn stop_all(&mut self) -> Vec<BrowserHandle> {
        self.runtimes
            .drain()
            .filter_map(|(_, mut runtime)| runtime.browser.take())
            .collect()
    }

    /// The events tails matched since the last call, without those of the same event that fired from a tail
    /// moments ago (see [`TailDebounce`]).
    pub fn drain_tail_events(&mut self, now: Instant) -> Vec<TailEvent> {
        let events: Vec<TailEvent> = self.events_rx.try_iter().collect();
        events
            .into_iter()
            .filter(|e| {
                self.tail_debounce
                    .admit(e.space.as_str(), &e.plugin, &e.event, now)
            })
            .collect()
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
            let gate = PluginGate {
                space: space.id.clone(),
                plugin: plugin.to_owned(),
                kind: PluginGateKind::WriteCall {
                    tool: tool.to_owned(),
                    args: masked_args_summary(args, redactor),
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
        let uses_browser = matches!(manifest.pane, PaneSpec::Web { .. })
            || manifest.tools.iter().any(|t| {
                matches!(&t.action, xctx_plugin::ToolAction::Builtin(b) if b.starts_with("page."))
            });
        let browser = uses_browser.then(|| self.browser(&space.id));
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
            scratch: crate::space_graph::scratch_dir_for(space.id.as_str()).ok(),
            browser,
            web_url: settings.url.clone(),
            secrets: ctx.secrets.clone(),
            pane: Some(pane),
        })
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

    /// Fire `plugin`'s `event` in `space` with `detail`, as its manifest maps it: record a finding in the shared
    /// space; `fail` stops the loop at its stage and `pass` moves it on (see [`apply_loop_action`]); a notice, a
    /// failure, or a loop now waiting for approval is posted to the Inbox. A repeat within [`EVENT_DEBOUNCE`]
    /// does nothing. `state` is the space's graph state; without it (it could not be loaded) only the Inbox is
    /// told. Refused for an unknown plugin.
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
        let registry = self.registry(space);
        let found = registry
            .usable(plugin)
            .ok_or_else(|| RpcError::not_found(format!("no usable plugin named {plugin}")))?;
        let action = found.manifest.action_for(event);
        let detail = cut_chars(redactor.redact(detail).trim(), MAX_EVENT_DETAIL_CHARS);
        let id = &space.id;
        let summary = if detail.is_empty() {
            format!("{plugin}: {event}")
        } else {
            format!("{plugin}: {event}: {detail}")
        };
        let mut report = EventReport {
            action,
            debounced: false,
            loop_effect: LoopEffect::Unchanged,
            finding: None,
            notice: None,
            summary: summary.clone(),
            errors: Vec::new(),
        };
        if !self
            .debounce
            .admit(id.as_str(), plugin, event, &detail, now)
        {
            report.debounced = true;
            return Ok(report);
        }
        if let Some(state) = state {
            match state.shared_mut().add_finding(
                &event_author(plugin),
                event_severity(action),
                None,
                None,
                &summary,
            ) {
                Ok(finding) => report.finding = Some(finding),
                Err(e) => report
                    .errors
                    .push(format!("could not record the finding: {e}")),
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
        let notice = match (action, report.loop_effect) {
            (_, LoopEffect::Advanced(LoopDecision::NeedsApproval(stage))) => {
                Some(loop_approval_notice(id, stage))
            }
            (Some(LoopAction::Notice), _) => Some(Notice {
                title: format!("{plugin}: {event} in {id}"),
                body: (!detail.is_empty()).then(|| detail.clone()),
                level: Level::Warning,
            }),
            (Some(LoopAction::Fail), effect) => Some(Notice {
                title: if effect == LoopEffect::Blocked {
                    format!("{plugin}: {event} stopped the loop of {id}")
                } else {
                    format!("{plugin}: {event} in {id}")
                },
                body: (!detail.is_empty()).then(|| detail.clone()),
                level: Level::Error,
            }),
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
            PaneSpec::Http { .. } => s.base_url.clone(),
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

    /// The pane kind (`terminal`, `http`, `logs`, `sql` or `web`) of plugin pane `pane` of `space` and its
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
                .map(|d| redactor.redact(&d.location).into_owned())
                .unwrap_or_default(),
            result: state.result.as_ref().map(SqlTable::from_json),
            error: state.error,
            busy: state.busy > 0,
            write_pending: state.write_pending.is_some(),
        }
    }

    /// What `space`'s browser pane shows: an idle snapshot until the browser is used.
    pub fn browser_data(&self, space: &SpaceId) -> BrowserSnapshot {
        self.runtimes
            .get(space)
            .and_then(|r| r.browser.as_ref())
            .map(BrowserHandle::snapshot)
            .unwrap_or_default()
    }

    /// The plugins mounted in `space`, in mount order.
    pub fn mounted(&mut self, space: &Space) -> Vec<MountedPlugin> {
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
                settings: m.settings,
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
        let summary = masked_args_summary(&args, &redactor);
        assert!(summary.contains("[REDACTED:DB_PASSWORD]"), "{summary}");
        assert!(!summary.contains("s3cret-pw"));
        let mut long = Map::new();
        long.insert("x".into(), json!("a".repeat(10_000)));
        assert_eq!(
            masked_args_summary(&long, &redactor).chars().count(),
            MAX_GATE_ARGS_CHARS
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
        Ok(())
    }
}
