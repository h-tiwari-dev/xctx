//! Agent runners: the program an agent pane runs, and how xctx gives it a task, resumes, opens and forks its
//! sessions, described by a runner manifest (`runner.toml`).
//!
//! ```toml
//! name = "pi"                                    # lowercase letters, digits and `-`
//! description = "pi coding agent"
//! program = "pi"                                 # a name looked up on PATH, or an absolute path
//! task = { style = "after-double-dash" }         # or { style = "flag", flag = "--prompt" }, "stdin", "none"
//! resume = ["--continue"]                        # optional: resume the worktree's last session
//! open_session = ["--session-id", "{session}"]   # optional: open or create the named session
//! fork_session = ["--fork", "{source}", "--session-id", "{session}"]   # optional: copy a session
//! extra_args = []                                # optional: given first, every time, as written
//! [env]                                          # optional: set in the agent's environment
//! ```
//!
//! In the session templates, `{session}` is the session xctx names and `{source}` the session (an id or a session
//! file) being forked; `{{`/`}}` are literal braces. A placeholder fills its own element and never adds or splits
//! elements, and no shell is involved, so a session id or path can never become extra arguments.
//!
//! A runner can fork when it has `fork_session`; attempts (Milestone 4) are only offered for such runners. The
//! built-in runners ([`BUILTIN_RUNNERS`]) are `pi`, the default, whose arguments are exactly the ones xctx built
//! for pi before runners existed, and `claude`, which cannot name or fork sessions. User runners are loaded from
//! `<dir>/<name>/runner.toml` by [`RunnerRegistry::add_user_dir`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::PluginError;
use crate::manifest::is_plugin_name;
use crate::registry::Source;
use crate::template::{self, Piece};

/// The file name of a runner manifest inside a runner's folder.
pub const RUNNER_FILE: &str = "runner.toml";

/// The runner agent panes use unless a space or lane picks another.
pub const DEFAULT_RUNNER: &str = "pi";

/// The built-in runner manifests as `(name, TOML text)`, the default first.
pub const BUILTIN_RUNNERS: &[(&str, &str)] = &[
    ("pi", include_str!("builtin/runners/pi.toml")),
    ("claude", include_str!("builtin/runners/claude.toml")),
];

/// The placeholder for the session xctx names, in `open_session` and `fork_session`.
pub const SESSION_PLACEHOLDER: &str = "session";

/// The placeholder for the session being forked, in `fork_session`.
pub const SOURCE_PLACEHOLDER: &str = "source";

/// What a runner can be asked to do, named in [`RunnerError::Unsupported`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    /// Take a task as its first message (`task` is not `none`).
    Task,
    /// Resume the worktree's last session (`resume`).
    Resume,
    /// Open or create a session xctx names (`open_session`).
    OpenSession,
    /// Copy a session into a new named one (`fork_session`), which attempts need.
    ForkSession,
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Capability::Task => "cannot be given a task",
            Capability::Resume => "cannot resume a session",
            Capability::OpenSession => "cannot open named sessions",
            Capability::ForkSession => "cannot fork sessions, so attempts are not available",
        })
    }
}

/// A runner manifest that could not be loaded, a runner name clash, or arguments a runner cannot build.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    /// The manifest could not be read, is not valid TOML, or has an invalid field ([`PluginError::Syntax`],
    /// [`PluginError::Invalid`] or [`PluginError::Io`], naming the origin and field).
    #[error(transparent)]
    Manifest(#[from] PluginError),

    /// Two runners have the same name; the later one was not loaded.
    #[error(
        "runner {name:?} from {new} clashes with the one already loaded from {existing}; rename one of them"
    )]
    Clash {
        /// The runner name.
        name: String,
        /// Where the loaded runner came from.
        existing: String,
        /// Where the refused runner came from.
        new: String,
    },

    /// The runner was asked for something its manifest does not describe, such as a fork.
    #[error("runner {runner} {capability}")]
    Unsupported {
        /// The runner's name.
        runner: String,
        /// What it cannot do.
        capability: Capability,
    },
}

/// How a runner is given its first message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "style", rename_all = "kebab-case")]
pub enum TaskStyle {
    /// As the last argument, after `--`: `pi -- <task>`.
    AfterDoubleDash,
    /// As the value of a flag, last: `<flag> <task>`, such as `--prompt <task>`.
    Flag {
        /// The flag, such as `--prompt`.
        flag: String,
    },
    /// Written to the program's standard input (see [`stdin_task`]); nothing is added to the arguments.
    Stdin,
    /// The runner takes no first message; starting it with a task is refused.
    None,
}

/// A list of arguments whose elements may use the placeholders their field allows (validated when parsing).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ArgsTemplate {
    elements: Vec<String>,
}

impl ArgsTemplate {
    /// The elements as written in the manifest.
    pub fn elements(&self) -> &[String] {
        &self.elements
    }

    /// The elements with each `{name}` replaced by its value in `values`. A value is used as is (never scanned
    /// for placeholders again) and never splits an element. Placeholders missing from `values` are an error,
    /// which validation rules out for the values [`build_args`] supplies.
    pub fn fill(&self, values: &[(&str, &str)]) -> Result<Vec<String>, String> {
        self.elements
            .iter()
            .map(|element| {
                let mut out = String::new();
                for piece in template::parse(element)? {
                    match piece {
                        Piece::Lit(s) => out.push_str(&s),
                        Piece::Param(name) => match values.iter().find(|(n, _)| *n == name) {
                            Some((_, v)) => out.push_str(v),
                            None => return Err(format!("no value for `{{{name}}}`")),
                        },
                        Piece::Ctx(_) => {
                            return Err(format!(
                                "{element:?} uses a placeholder runners do not have"
                            ));
                        }
                    }
                }
                Ok(out)
            })
            .collect()
    }
}

/// A validated runner manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunnerManifest {
    /// The runner's name: lowercase letters, digits and `-`. Unique among loaded runners.
    pub name: String,
    /// One line on what it is, shown when picking a runner.
    pub description: String,
    /// The program: a name to look up on `PATH` (no `/`), or an absolute path.
    pub program: String,
    /// How it is given its first message.
    pub task: TaskStyle,
    /// The arguments that resume the worktree's last session; no placeholders.
    pub resume: Option<ArgsTemplate>,
    /// The arguments that open or create session `{session}`.
    pub open_session: Option<ArgsTemplate>,
    /// The arguments that copy session `{source}` into a new session `{session}`.
    pub fork_session: Option<ArgsTemplate>,
    /// Arguments given first, every time, as written (no placeholders).
    pub extra_args: Vec<String>,
    /// Environment variables set for the program, by name.
    pub env: BTreeMap<String, String>,
}

impl RunnerManifest {
    /// Parse and validate a runner manifest. `origin` names it in errors (a path, or `built-in <name>`).
    pub fn parse(text: &str, origin: &str) -> Result<RunnerManifest, RunnerError> {
        let raw: RawRunner = toml::from_str(text).map_err(|e| PluginError::Syntax {
            origin: origin.to_owned(),
            message: e.to_string().trim_end().to_owned(),
        })?;
        Ok(raw.validate(origin)?)
    }

    /// Read, parse and validate the runner manifest at `path`.
    pub fn load(path: &Path) -> Result<RunnerManifest, RunnerError> {
        let text = std::fs::read_to_string(path).map_err(|source| PluginError::Io {
            path: path.to_owned(),
            source,
        })?;
        RunnerManifest::parse(&text, &path.display().to_string())
    }

    /// Whether it can fork a session into a new one, which attempts need: it has `fork_session`.
    pub fn can_fork(&self) -> bool {
        self.fork_session.is_some()
    }

    /// Whether it can open a session xctx names: it has `open_session`.
    pub fn can_open(&self) -> bool {
        self.open_session.is_some()
    }

    /// Whether it can resume the worktree's last session: it has `resume`.
    pub fn can_resume(&self) -> bool {
        self.resume.is_some()
    }

    /// Whether it can be given a task as its first message.
    pub fn takes_task(&self) -> bool {
        self.task != TaskStyle::None
    }

    fn unsupported(&self, capability: Capability) -> RunnerError {
        RunnerError::Unsupported {
            runner: self.name.clone(),
            capability,
        }
    }
}

/// How an agent starts, as far as its first message goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunnerStart {
    /// A new conversation given a task as its first message.
    Task(String),
    /// Resume: with no named session, the worktree's last one (`resume`); with one, just that session.
    Continue,
    /// A new conversation with no first message.
    Fresh,
}

/// Which session an agent runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunnerSession {
    /// No session named by xctx: only what [`RunnerStart`] says.
    Unnamed,
    /// Open session `id`, or create it (`open_session`).
    Open(String),
    /// Copy session `source` (an id or a session file) into session `id` (`fork_session`).
    Fork {
        /// The session copied.
        source: String,
        /// The new session's id.
        id: String,
    },
}

/// The arguments (after the program) that start `runner` as `start` in `session`: `extra_args`, then the
/// session's arguments (`resume` for an unnamed [`RunnerStart::Continue`], `open_session` or `fork_session`),
/// then the task as its [`TaskStyle`] says. Refused, naming the runner, when its manifest lacks what is asked:
/// for example a fork from a runner without `fork_session`.
pub fn build_args(
    runner: &RunnerManifest,
    start: &RunnerStart,
    session: &RunnerSession,
) -> Result<Vec<String>, RunnerError> {
    let invalid = |field: &str, message: String| {
        RunnerError::Manifest(PluginError::Invalid {
            origin: format!("runner {}", runner.name),
            field: field.to_owned(),
            message,
        })
    };
    let mut args = runner.extra_args.clone();
    match session {
        RunnerSession::Unnamed => {
            if *start == RunnerStart::Continue {
                let resume = runner
                    .resume
                    .as_ref()
                    .ok_or_else(|| runner.unsupported(Capability::Resume))?;
                args.extend(resume.fill(&[]).map_err(|m| invalid("resume", m))?);
            }
        }
        RunnerSession::Open(id) => {
            let open = runner
                .open_session
                .as_ref()
                .ok_or_else(|| runner.unsupported(Capability::OpenSession))?;
            args.extend(
                open.fill(&[(SESSION_PLACEHOLDER, id)])
                    .map_err(|m| invalid("open_session", m))?,
            );
        }
        RunnerSession::Fork { source, id } => {
            let fork = runner
                .fork_session
                .as_ref()
                .ok_or_else(|| runner.unsupported(Capability::ForkSession))?;
            args.extend(
                fork.fill(&[(SOURCE_PLACEHOLDER, source), (SESSION_PLACEHOLDER, id)])
                    .map_err(|m| invalid("fork_session", m))?,
            );
        }
    }
    if let RunnerStart::Task(task) = start {
        match &runner.task {
            TaskStyle::AfterDoubleDash => {
                args.push("--".to_owned());
                args.push(task.clone());
            }
            TaskStyle::Flag { flag } => {
                args.push(flag.clone());
                args.push(task.clone());
            }
            TaskStyle::Stdin => {}
            TaskStyle::None => return Err(runner.unsupported(Capability::Task)),
        }
    }
    Ok(args)
}

/// The text to write to the program's standard input: the task, for a runner whose task style is
/// [`TaskStyle::Stdin`] started with [`RunnerStart::Task`]; else `None`.
pub fn stdin_task<'a>(runner: &RunnerManifest, start: &'a RunnerStart) -> Option<&'a str> {
    match (start, &runner.task) {
        (RunnerStart::Task(task), TaskStyle::Stdin) => Some(task),
        _ => None,
    }
}

/// The origin a built-in runner manifest is named by in errors.
pub fn builtin_runner_origin(name: &str) -> String {
    format!("built-in runner {name}")
}

/// A loaded runner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Runner {
    /// Its manifest.
    pub manifest: RunnerManifest,
    /// Where it came from: built-in, or the user's runners folder.
    pub source: Source,
}

impl Runner {
    /// The runner's name.
    pub fn name(&self) -> &str {
        &self.manifest.name
    }
}

/// A runner manifest that was not loaded, and why.
#[derive(Debug)]
pub struct RunnerProblem {
    /// The manifest file or folder, absent for a built-in.
    pub path: Option<PathBuf>,
    /// What went wrong.
    pub error: RunnerError,
}

/// The loaded runners, built-in first, and the problems met loading them. Names are unique: a user runner with
/// the name of one already loaded (a built-in included) is refused and reported, never shadowing it.
#[derive(Debug, Default)]
pub struct RunnerRegistry {
    runners: Vec<Runner>,
    problems: Vec<RunnerProblem>,
}

impl RunnerRegistry {
    /// A registry with no runners at all.
    pub fn empty() -> RunnerRegistry {
        RunnerRegistry::default()
    }

    /// A registry with the built-in runners.
    pub fn with_builtins() -> RunnerRegistry {
        let mut registry = RunnerRegistry::default();
        for (name, text) in BUILTIN_RUNNERS {
            match RunnerManifest::parse(text, &builtin_runner_origin(name)) {
                Ok(manifest) => registry.insert(manifest, Source::Builtin),
                Err(error) => registry.problems.push(RunnerProblem { path: None, error }),
            }
        }
        registry
    }

    /// Load the user's runners from `dir` (each `<dir>/<name>/runner.toml`, `name` matching the folder), such as
    /// `~/.config/xctx/runners`. A missing `dir` loads nothing; bad manifests and clashes are reported in
    /// [`RunnerRegistry::problems`] and the others still load.
    pub fn add_user_dir(&mut self, dir: &Path) {
        for path in self.manifest_paths(dir) {
            match load_named(&path) {
                Ok(manifest) => self.insert(manifest, Source::User { path }),
                Err(error) => self.problems.push(RunnerProblem {
                    path: Some(path),
                    error,
                }),
            }
        }
    }

    /// Every loaded runner, built-in first, then in loading order.
    pub fn runners(&self) -> &[Runner] {
        &self.runners
    }

    /// The runner named `name`.
    pub fn get(&self, name: &str) -> Option<&Runner> {
        self.runners.iter().find(|r| r.name() == name)
    }

    /// The default runner ([`DEFAULT_RUNNER`]), when it loaded.
    pub fn default_runner(&self) -> Option<&Runner> {
        self.get(DEFAULT_RUNNER)
    }

    /// The runner manifests that could not be loaded.
    pub fn problems(&self) -> &[RunnerProblem] {
        &self.problems
    }

    fn insert(&mut self, manifest: RunnerManifest, source: Source) {
        if let Some(existing) = self.get(&manifest.name) {
            let error = RunnerError::Clash {
                name: manifest.name.clone(),
                existing: existing.source.to_string(),
                new: source.to_string(),
            };
            let path = match source {
                Source::Builtin => None,
                Source::User { path } | Source::Repo { path, .. } => Some(path),
            };
            self.problems.push(RunnerProblem { path, error });
            return;
        }
        self.runners.push(Runner { manifest, source });
    }

    /// The `runner.toml` of every runner folder in `dir`, sorted by folder name. A folder without one is reported.
    fn manifest_paths(&mut self, dir: &Path) -> Vec<PathBuf> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(source) => {
                self.problems.push(RunnerProblem {
                    path: Some(dir.to_owned()),
                    error: PluginError::Io {
                        path: dir.to_owned(),
                        source,
                    }
                    .into(),
                });
                return Vec::new();
            }
        };
        let mut folders: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        folders.sort();
        let mut paths = Vec::with_capacity(folders.len());
        for folder in folders {
            let path = folder.join(RUNNER_FILE);
            if path.is_file() {
                paths.push(path);
            } else {
                self.problems.push(RunnerProblem {
                    error: PluginError::Io {
                        path: path.clone(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "the runner folder has no runner.toml",
                        ),
                    }
                    .into(),
                    path: Some(folder),
                });
            }
        }
        paths
    }
}

/// Load a runner manifest whose `name` must match its folder's.
fn load_named(path: &Path) -> Result<RunnerManifest, RunnerError> {
    let manifest = RunnerManifest::load(path)?;
    let folder = path
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned());
    if folder.as_deref() != Some(manifest.name.as_str()) {
        return Err(PluginError::Invalid {
            origin: path.display().to_string(),
            field: "name".to_owned(),
            message: format!(
                "{:?} must match its folder's name {:?}",
                manifest.name,
                folder.unwrap_or_default()
            ),
        }
        .into());
    }
    Ok(manifest)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRunner {
    name: String,
    #[serde(default)]
    description: String,
    program: String,
    task: RawTask,
    resume: Option<Vec<String>>,
    open_session: Option<Vec<String>>,
    fork_session: Option<Vec<String>>,
    #[serde(default)]
    extra_args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTask {
    style: RawTaskStyle,
    flag: Option<String>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum RawTaskStyle {
    AfterDoubleDash,
    Flag,
    Stdin,
    None,
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('A'..='Z' | 'a'..='z' | '_'))
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl RawRunner {
    fn validate(self, origin: &str) -> Result<RunnerManifest, PluginError> {
        let err = |field: &str, message: String| PluginError::Invalid {
            origin: origin.to_owned(),
            field: field.to_owned(),
            message,
        };
        if !is_plugin_name(&self.name) {
            return Err(err(
                "name",
                format!("{:?} must be lowercase letters, digits and `-`", self.name),
            ));
        }
        let program = self.program.trim();
        if program.is_empty() {
            return Err(err("program", "must name a program".to_owned()));
        }
        if program != self.program {
            return Err(err(
                "program",
                format!("{:?} has leading or trailing spaces", self.program),
            ));
        }
        if program.contains('/') && !Path::new(program).is_absolute() {
            return Err(err(
                "program",
                format!("{program:?} must be a name looked up on PATH or an absolute path"),
            ));
        }
        let task = match (self.task.style, self.task.flag) {
            (RawTaskStyle::Flag, Some(flag)) => {
                if !flag.starts_with('-') || flag.trim() != flag || flag.chars().all(|c| c == '-') {
                    return Err(err(
                        "task.flag",
                        format!("{flag:?} must be a flag such as `--prompt`"),
                    ));
                }
                TaskStyle::Flag { flag }
            }
            (RawTaskStyle::Flag, None) => {
                return Err(err(
                    "task.flag",
                    "the `flag` style needs the flag, such as `flag = \"--prompt\"`".to_owned(),
                ));
            }
            (_, Some(_)) => {
                return Err(err(
                    "task.flag",
                    "only the `flag` style takes a flag".to_owned(),
                ));
            }
            (RawTaskStyle::AfterDoubleDash, None) => TaskStyle::AfterDoubleDash,
            (RawTaskStyle::Stdin, None) => TaskStyle::Stdin,
            (RawTaskStyle::None, None) => TaskStyle::None,
        };
        let resume = template_field(&err, "resume", self.resume, &[], &[])?;
        let open_session = template_field(
            &err,
            "open_session",
            self.open_session,
            &[SESSION_PLACEHOLDER],
            &[SESSION_PLACEHOLDER],
        )?;
        let fork_session = template_field(
            &err,
            "fork_session",
            self.fork_session,
            &[SOURCE_PLACEHOLDER, SESSION_PLACEHOLDER],
            &[SOURCE_PLACEHOLDER, SESSION_PLACEHOLDER],
        )?;
        for name in self.env.keys() {
            if !is_env_name(name) {
                return Err(err(
                    &format!("env.{name}"),
                    format!("{name:?} is not an environment variable name"),
                ));
            }
        }
        Ok(RunnerManifest {
            name: self.name,
            description: self.description.trim().to_owned(),
            program: self.program,
            task,
            resume,
            open_session,
            fork_session,
            extra_args: self.extra_args,
            env: self.env,
        })
    }
}

/// Validate an optional template field: non-empty, only `allowed` placeholders, each of `required` used.
fn template_field(
    err: &impl Fn(&str, String) -> PluginError,
    field: &str,
    value: Option<Vec<String>>,
    allowed: &[&str],
    required: &[&str],
) -> Result<Option<ArgsTemplate>, PluginError> {
    let Some(elements) = value else {
        return Ok(None);
    };
    if elements.is_empty() {
        return Err(err(
            field,
            "must have at least one argument; leave the field out when the runner has none"
                .to_owned(),
        ));
    }
    let allowed_text = || {
        if allowed.is_empty() {
            "no placeholders can be used here".to_owned()
        } else {
            let names: Vec<String> = allowed.iter().map(|n| format!("{{{n}}}")).collect();
            format!("only {} can be used here", names.join(" and "))
        }
    };
    let mut used = Vec::new();
    for (i, element) in elements.iter().enumerate() {
        let at = format!("{field}[{i}]");
        for piece in template::parse(element).map_err(|m| err(&at, m))? {
            match piece {
                Piece::Lit(_) => {}
                Piece::Param(name) if allowed.contains(&name.as_str()) => used.push(name),
                Piece::Param(name) => {
                    return Err(err(&at, format!("`{{{name}}}`: {}", allowed_text())));
                }
                Piece::Ctx(_) => {
                    return Err(err(
                        &at,
                        format!("{element:?} uses a space placeholder; {}", allowed_text()),
                    ));
                }
            }
        }
    }
    if let Some(missing) = required.iter().find(|r| !used.iter().any(|u| u == *r)) {
        return Err(err(field, format!("must use `{{{missing}}}`")));
    }
    Ok(Some(ArgsTemplate { elements }))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn builtin(name: &str) -> Result<RunnerManifest, RunnerError> {
        let text = BUILTIN_RUNNERS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, t)| *t)
            .unwrap_or_default();
        RunnerManifest::parse(text, &builtin_runner_origin(name))
    }

    const MINIMAL: &str = "name = \"agent\"\nprogram = \"agent\"\ntask = { style = \"none\" }\n";

    fn invalid_field(text: &str) -> String {
        match RunnerManifest::parse(text, "test") {
            Err(RunnerError::Manifest(PluginError::Invalid { origin, field, .. })) => {
                assert_eq!(origin, "test");
                field
            }
            other => panic!("expected an invalid field, got {other:?}"),
        }
    }

    fn args(r: &RunnerManifest, start: &RunnerStart, session: &RunnerSession) -> Vec<String> {
        match build_args(r, start, session) {
            Ok(args) => args,
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn every_builtin_runner_parses_under_its_own_name() -> TestResult {
        for (name, _) in BUILTIN_RUNNERS {
            assert_eq!(&builtin(name)?.name, name);
        }
        assert_eq!(BUILTIN_RUNNERS[0].0, DEFAULT_RUNNER);
        Ok(())
    }

    /// The arguments `crates/xctx/src/agent_session.rs::agent_args` builds today for every `AgentStart` and
    /// `AgentSession` the app uses, written out (that crate cannot be a dependency here): the built-in `pi`
    /// runner must build exactly these, so the app can swap it in without changing what pi is given.
    #[test]
    fn pi_builds_exactly_the_arguments_the_app_gives_pi_today() -> TestResult {
        let pi = builtin("pi")?;
        assert_eq!(pi.program, "pi");
        assert!(pi.can_fork() && pi.can_open() && pi.can_resume() && pi.takes_task());
        assert!(pi.extra_args.is_empty() && pi.env.is_empty());

        let task = RunnerStart::Task("fix it".into());
        let open = RunnerSession::Open("xctx-s-writer-api".into());
        let fork = RunnerSession::Fork {
            source: "xctx-s-writer-api".into(),
            id: "xctx-s-writer-api-attempt-2".into(),
        };
        let file_fork = RunnerSession::Fork {
            source: "/Users/me/.pi/agent/sessions/--p--/2026_abc.jsonl".into(),
            id: "xctx-s-writer-api".into(),
        };
        let expected: Vec<(RunnerStart, RunnerSession, Vec<&str>)> = vec![
            (
                RunnerStart::Continue,
                RunnerSession::Unnamed,
                vec!["--continue"],
            ),
            (RunnerStart::Fresh, RunnerSession::Unnamed, vec![]),
            (task.clone(), RunnerSession::Unnamed, vec!["--", "fix it"]),
            (
                task.clone(),
                open.clone(),
                vec!["--session-id", "xctx-s-writer-api", "--", "fix it"],
            ),
            (
                RunnerStart::Continue,
                open.clone(),
                vec!["--session-id", "xctx-s-writer-api"],
            ),
            (
                RunnerStart::Fresh,
                open.clone(),
                vec!["--session-id", "xctx-s-writer-api"],
            ),
            (
                RunnerStart::Task("use a token bucket".into()),
                fork.clone(),
                vec![
                    "--fork",
                    "xctx-s-writer-api",
                    "--session-id",
                    "xctx-s-writer-api-attempt-2",
                    "--",
                    "use a token bucket",
                ],
            ),
            (
                RunnerStart::Fresh,
                fork.clone(),
                vec![
                    "--fork",
                    "xctx-s-writer-api",
                    "--session-id",
                    "xctx-s-writer-api-attempt-2",
                ],
            ),
            (
                RunnerStart::Continue,
                fork,
                vec![
                    "--fork",
                    "xctx-s-writer-api",
                    "--session-id",
                    "xctx-s-writer-api-attempt-2",
                ],
            ),
            // An agent from before Milestone 4 carried over from its newest session file.
            (
                RunnerStart::Continue,
                file_fork,
                vec![
                    "--fork",
                    "/Users/me/.pi/agent/sessions/--p--/2026_abc.jsonl",
                    "--session-id",
                    "xctx-s-writer-api",
                ],
            ),
        ];
        for (start, session, want) in expected {
            assert_eq!(
                args(&pi, &start, &session),
                want,
                "{start:?} in {session:?}"
            );
        }
        // Same as xctx_core::pi_session's helpers, which agent_args uses.
        assert_eq!(
            args(&pi, &RunnerStart::Fresh, &RunnerSession::Open("x".into())),
            xctx_core::pi_session::open_args("x")
        );
        assert_eq!(
            args(
                &pi,
                &RunnerStart::Fresh,
                &RunnerSession::Fork {
                    source: "a".into(),
                    id: "b".into()
                }
            ),
            xctx_core::pi_session::fork_args("a", "b")
        );
        // Values are whole elements, never rescanned or split.
        assert_eq!(
            args(
                &pi,
                &RunnerStart::Task("{session} -- --fork x".into()),
                &RunnerSession::Open("a b {source}".into())
            ),
            [
                "--session-id",
                "a b {source}",
                "--",
                "{session} -- --fork x"
            ]
        );
        assert_eq!(stdin_task(&pi, &task), None);
        Ok(())
    }

    #[test]
    fn a_runner_without_sessions_cannot_fork_or_open() -> TestResult {
        let claude = builtin("claude")?;
        assert!(!claude.can_fork() && !claude.can_open());
        assert!(claude.can_resume() && claude.takes_task());
        let task = RunnerStart::Task("fix it".into());
        assert_eq!(
            args(&claude, &task, &RunnerSession::Unnamed),
            ["--", "fix it"]
        );
        assert_eq!(
            args(&claude, &RunnerStart::Continue, &RunnerSession::Unnamed),
            ["--continue"]
        );
        let fork = build_args(
            &claude,
            &RunnerStart::Fresh,
            &RunnerSession::Fork {
                source: "a".into(),
                id: "b".into(),
            },
        );
        match fork {
            Err(e) => assert_eq!(
                e.to_string(),
                "runner claude cannot fork sessions, so attempts are not available"
            ),
            Ok(a) => panic!("forked: {a:?}"),
        }
        let open = build_args(&claude, &task, &RunnerSession::Open("a".into()));
        assert!(matches!(
            open,
            Err(RunnerError::Unsupported {
                capability: Capability::OpenSession,
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn task_styles_extra_args_and_missing_capabilities() -> TestResult {
        let text = r#"
name = "shell-agent"
description = "  A generic agent  "
program = "/usr/local/bin/agent"
task = { style = "flag", flag = "--prompt" }
open_session = ["--session={session}"]
extra_args = ["--quiet", "{literal}"]
[env]
AGENT_MODE = "xctx"
"#;
        let r = RunnerManifest::parse(text, "test")?;
        assert_eq!(r.description, "A generic agent");
        assert!(!r.can_fork() && r.can_open() && !r.can_resume());
        assert_eq!(r.env.get("AGENT_MODE").map(String::as_str), Some("xctx"));
        assert_eq!(
            args(
                &r,
                &RunnerStart::Task("go".into()),
                &RunnerSession::Open("s1".into())
            ),
            ["--quiet", "{literal}", "--session=s1", "--prompt", "go"]
        );
        let resume = build_args(&r, &RunnerStart::Continue, &RunnerSession::Unnamed);
        assert!(
            resume.is_err_and(|e| e.to_string() == "runner shell-agent cannot resume a session")
        );

        let stdin = RunnerManifest::parse(&MINIMAL.replace("\"none\"", "\"stdin\""), "test")?;
        let task = RunnerStart::Task("hello".into());
        assert!(args(&stdin, &task, &RunnerSession::Unnamed).is_empty());
        assert_eq!(stdin_task(&stdin, &task), Some("hello"));
        assert_eq!(stdin_task(&stdin, &RunnerStart::Fresh), None);

        let none = RunnerManifest::parse(MINIMAL, "test")?;
        assert!(!none.takes_task());
        assert!(args(&none, &RunnerStart::Fresh, &RunnerSession::Unnamed).is_empty());
        let refused = build_args(&none, &task, &RunnerSession::Unnamed);
        assert!(refused.is_err_and(|e| e.to_string() == "runner agent cannot be given a task"));
        Ok(())
    }

    #[test]
    fn syntax_errors_name_the_origin() {
        for text in [
            "name = ".to_owned(),
            format!("{MINIMAL}colour = \"red\"\n"),
            MINIMAL.replace("\"none\"", "\"telepathy\""),
            MINIMAL.replace(
                "{ style = \"none\" }",
                "{ style = \"none\", flags = \"-p\" }",
            ),
            "name = \"a\"\nprogram = \"a\"\n".to_owned(),
        ] {
            match RunnerManifest::parse(&text, "/x/runner.toml") {
                Err(RunnerError::Manifest(PluginError::Syntax { origin, .. })) => {
                    assert_eq!(origin, "/x/runner.toml");
                }
                other => panic!("{text}: expected a syntax error, got {other:?}"),
            }
        }
    }

    #[test]
    fn invalid_fields_are_named() {
        assert_eq!(
            invalid_field(&MINIMAL.replace("\"agent\"\nprogram", "\"Agent_1\"\nprogram")),
            "name"
        );
        let program = |p: &str| MINIMAL.replace("program = \"agent\"", &format!("program = {p:?}"));
        assert_eq!(invalid_field(&program("")), "program");
        assert_eq!(invalid_field(&program(" agent")), "program");
        assert_eq!(invalid_field(&program("bin/agent")), "program");
        assert!(RunnerManifest::parse(&program("/opt/agent"), "t").is_ok());
        let flag = |f: &str| {
            MINIMAL.replace(
                "{ style = \"none\" }",
                &format!("{{ style = \"flag\", flag = {f:?} }}"),
            )
        };
        assert_eq!(invalid_field(&flag("prompt")), "task.flag");
        assert_eq!(invalid_field(&flag("--")), "task.flag");
        assert!(RunnerManifest::parse(&flag("-p"), "t").is_ok());
        assert_eq!(
            invalid_field(&MINIMAL.replace("{ style = \"none\" }", "{ style = \"flag\" }")),
            "task.flag"
        );
        assert_eq!(
            invalid_field(&MINIMAL.replace(
                "{ style = \"none\" }",
                "{ style = \"stdin\", flag = \"-p\" }"
            )),
            "task.flag"
        );

        let with = |line: &str| format!("{MINIMAL}{line}\n");
        assert_eq!(invalid_field(&with("resume = []")), "resume");
        assert_eq!(
            invalid_field(&with("resume = [\"--resume\", \"{session}\"]")),
            "resume[1]"
        );
        assert_eq!(
            invalid_field(&with("open_session = [\"--id\"]")),
            "open_session"
        );
        assert_eq!(
            invalid_field(&with("open_session = [\"--id\", \"{source}\"]")),
            "open_session[1]"
        );
        assert_eq!(
            invalid_field(&with("open_session = [\"--id\", \"{PORT}\"]")),
            "open_session[1]"
        );
        assert_eq!(
            invalid_field(&with("open_session = [\"--id\", \"{session\"]")),
            "open_session[1]"
        );
        assert_eq!(
            invalid_field(&with("fork_session = [\"--fork\", \"{source}\"]")),
            "fork_session"
        );
        assert_eq!(
            invalid_field(&with("fork_session = [\"--fork\", \"{source}\", \"{id}\"]")),
            "fork_session[2]"
        );
        assert_eq!(
            invalid_field(&with("[env]\n\"BAD-NAME\" = \"x\"")),
            "env.BAD-NAME"
        );
        let braces = with("open_session = [\"--id={{{session}}}\"]");
        let parsed = RunnerManifest::parse(&braces, "t");
        assert!(parsed.is_ok_and(|r| {
            args(&r, &RunnerStart::Fresh, &RunnerSession::Open("s".into())) == ["--id={s}"]
        }));
    }

    fn write_runner(dir: &Path, folder: &str, text: &str) -> std::io::Result<PathBuf> {
        let path = dir.join(folder).join(RUNNER_FILE);
        std::fs::create_dir_all(dir.join(folder))?;
        std::fs::write(&path, text)?;
        Ok(path)
    }

    #[test]
    fn the_registry_loads_builtins_then_user_runners_and_reports_problems() -> TestResult {
        let dir = tempfile::tempdir()?;
        let runners = dir.path();
        let codex = write_runner(
            runners,
            "codex",
            "name = \"codex\"\nprogram = \"codex\"\ntask = { style = \"after-double-dash\" }\n",
        )?;
        let clash = write_runner(
            runners,
            "pi",
            "name = \"pi\"\nprogram = \"my-pi\"\ntask = { style = \"none\" }\n",
        )?;
        let misnamed = write_runner(
            runners,
            "other",
            "name = \"mine\"\nprogram = \"x\"\ntask = { style = \"none\" }\n",
        )?;
        let bad = write_runner(
            runners,
            "bad",
            "name = \"bad\"\nprogram = \"\"\ntask = { style = \"none\" }\n",
        )?;
        std::fs::create_dir_all(runners.join("empty"))?;
        std::fs::create_dir_all(runners.join(".hidden"))?;

        let mut registry = RunnerRegistry::with_builtins();
        assert!(registry.problems().is_empty());
        assert_eq!(registry.default_runner().map(Runner::name), Some("pi"));
        registry.add_user_dir(runners);
        registry.add_user_dir(&runners.join("missing"));

        let names: Vec<&str> = registry.runners().iter().map(Runner::name).collect();
        assert_eq!(names, ["pi", "claude", "codex"]);
        assert_eq!(
            registry.get("codex").map(|r| &r.source),
            Some(&Source::User { path: codex })
        );
        // The built-in pi is kept: a user runner never shadows it.
        assert_eq!(
            registry.get("pi").map(|r| &r.source),
            Some(&Source::Builtin)
        );
        assert_eq!(
            registry.get("pi").map(|r| r.manifest.program.as_str()),
            Some("pi")
        );

        let problems = registry.problems();
        assert_eq!(problems.len(), 4, "{problems:?}");
        let problem = |path: &Path| problems.iter().find(|p| p.path.as_deref() == Some(path));
        assert!(problem(&clash).is_some_and(|p| matches!(
            &p.error,
            RunnerError::Clash { name, existing, .. } if name == "pi" && existing == "built-in"
        )));
        assert!(problem(&misnamed).is_some_and(|p| matches!(
            &p.error,
            RunnerError::Manifest(PluginError::Invalid { field, .. }) if field == "name"
        )));
        assert!(problem(&bad).is_some_and(|p| matches!(
            &p.error,
            RunnerError::Manifest(PluginError::Invalid { field, .. }) if field == "program"
        )));
        assert!(
            problem(&runners.join("empty"))
                .is_some_and(|p| matches!(&p.error, RunnerError::Manifest(PluginError::Io { .. })))
        );

        // A second folder loads after the first; a clash between user runners is reported too.
        let more = tempfile::tempdir()?;
        write_runner(
            more.path(),
            "codex",
            "name = \"codex\"\nprogram = \"codex2\"\ntask = { style = \"none\" }\n",
        )?;
        registry.add_user_dir(more.path());
        assert_eq!(registry.runners().len(), 3);
        assert!(registry.problems().iter().any(|p| matches!(
            &p.error,
            RunnerError::Clash { name, .. } if name == "codex"
        )));
        assert!(RunnerRegistry::empty().runners().is_empty());
        Ok(())
    }
}
