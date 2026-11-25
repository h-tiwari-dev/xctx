//! The plugin manifest (`plugin.toml`): its model, parsing and validation.
//!
//! ```toml
//! name = "my-api"
//! version = "0.1.0"
//! description = "Probe the space's API"
//! secrets = ["API_TOKEN"]
//!
//! [pane]
//! kind = "http"                         # terminal | http | logs | sql | web
//! base_url = "http://localhost:{PORT}"
//!
//! [scope]
//! mise_envs = ["space"]                 # "space", "production", or a literal MISE_ENV name
//!
//! [[tools]]
//! name = "api.health"
//! description = "GET /health"
//! effect = "read"                       # read | write
//! run = ["curl", "-s", "http://localhost:{PORT}/{path}"]   # or: builtin = "http.send"
//! params = [{ name = "path", type = "string", required = true }]
//!
//! [[events]]
//! name = "api.down"
//! match = { regex = "connection refused" }   # or: { field = "status", equals = "500" }
//! action = "fail"                        # fail | pass | notice
//! ```
//!
//! In `run`, `{name}` is a parameter, `{PORT}`, `{WORKTREE}` and `{SPACE}` come from the space, and `{{`/`}}` are
//! literal braces. Placeholders never add or split argv elements (see [`crate::plan_call`]).

use std::path::Path;

use regex::Regex;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};

use crate::error::{PluginError, Result};
use crate::template::{self, Piece};

/// The file name of a manifest inside a plugin's folder.
pub const MANIFEST_FILE: &str = "plugin.toml";

/// The scope pattern for the space's own `MISE_ENV` (its slug).
pub const SCOPE_SPACE: &str = "space";

/// The scope pattern (and `MISE_ENV`) for production.
pub const SCOPE_PRODUCTION: &str = xctx_core::space::PRODUCTION_MISE_ENV;

/// A validated plugin manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Manifest {
    /// The plugin's name: lowercase letters, digits and `-`. Unique among loaded plugins.
    pub name: String,
    /// Its version, free text such as `1.0.0`.
    pub version: String,
    /// One line on what it is for, shown in the Plugins menu and to agents.
    pub description: String,
    /// The pane it shows when mounted.
    pub pane: PaneSpec,
    /// The tools agents can call, in manifest order. Names are unique.
    pub tools: Vec<ToolSpec>,
    /// The events its tools or pane output can fire, in manifest order. Names are unique.
    pub events: Vec<EventSpec>,
    /// Which `MISE_ENV`s it may run under.
    pub scope: Scope,
    /// The env names of the secrets its tools need (from the space's mise environment), such as `JIRA_API_TOKEN`.
    pub secrets: Vec<String>,
}

/// The pane a plugin shows. Strings may use `{PORT}`, `{WORKTREE}` and `{SPACE}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PaneSpec {
    /// A terminal running a command. An empty `run` means the command is chosen when mounting (the user's shell
    /// by default).
    Terminal {
        /// The command's argv.
        run: Vec<String>,
    },
    /// The API client renderer.
    Http {
        /// The URL requests are relative to, such as `http://localhost:{PORT}`.
        base_url: Option<String>,
    },
    /// The logs renderer, tailing a command or a file (at most one; neither means chosen when mounting).
    Logs {
        /// A command whose output is tailed.
        run: Option<Vec<String>>,
        /// A file that is tailed, relative to the worktree unless absolute.
        file: Option<String>,
    },
    /// The database renderer.
    Sql {
        /// The engine, which picks the command-line tool.
        engine: SqlEngine,
        /// The database: a sqlite file, or a postgres connection string.
        database: String,
    },
    /// The browser renderer.
    Web {
        /// The page opened when mounting, such as `http://localhost:{PORT}`.
        url: Option<String>,
    },
}

impl PaneSpec {
    /// The pane kind's name as written in a manifest: `terminal`, `http`, `logs`, `sql` or `web`.
    pub fn kind(&self) -> &'static str {
        match self {
            PaneSpec::Terminal { .. } => "terminal",
            PaneSpec::Http { .. } => "http",
            PaneSpec::Logs { .. } => "logs",
            PaneSpec::Sql { .. } => "sql",
            PaneSpec::Web { .. } => "web",
        }
    }
}

/// A database engine of a `sql` pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SqlEngine {
    /// Through `sqlite3 -readonly`.
    Sqlite,
    /// Through `psql` with `default_transaction_read_only=on`.
    Postgres,
}

/// What a tool does to the world. Write tools always go through an Inbox gate the user approves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    /// Only reads; an agent may call it through `plugin.call`.
    Read,
    /// Changes something; the call becomes an Inbox gate.
    Write,
}

/// A tool agents can call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ToolSpec {
    /// `<namespace>.<verb>`, such as `http.send`.
    pub name: String,
    /// What it does, for agents.
    pub description: String,
    /// Whether it reads or writes.
    pub effect: Effect,
    /// Its parameters, in manifest order. Names are unique.
    pub params: Vec<ParamSpec>,
    /// How it runs.
    pub action: ToolAction,
}

impl ToolSpec {
    /// The parameter named `name`.
    pub fn param(&self, name: &str) -> Option<&ParamSpec> {
        self.params.iter().find(|p| p.name == name)
    }

    /// A JSON Schema object describing the tool's arguments, for agents.
    pub fn input_schema(&self) -> Value {
        let mut properties = serde_json::Map::new();
        for p in &self.params {
            let mut schema = match p.kind {
                ParamKind::String | ParamKind::Path => json!({ "type": "string" }),
                ParamKind::Integer => json!({ "type": "integer" }),
                ParamKind::Boolean => json!({ "type": "boolean" }),
                ParamKind::Strings => json!({ "type": "array", "items": { "type": "string" } }),
            };
            if !p.description.is_empty() {
                schema["description"] = Value::String(p.description.clone());
            }
            properties.insert(p.name.clone(), schema);
        }
        let required: Vec<&str> = self
            .params
            .iter()
            .filter(|p| p.required)
            .map(|p| p.name.as_str())
            .collect();
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    }
}

/// A tool parameter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ParamSpec {
    /// Lowercase letters, digits and `_`, starting with a letter; used as `{name}` in `run`.
    pub name: String,
    /// Its type (`type` in the manifest).
    #[serde(rename = "type")]
    pub kind: ParamKind,
    /// Whether a call must pass it.
    pub required: bool,
    /// What it is, for agents.
    pub description: String,
}

/// The type of a tool parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamKind {
    /// Any text.
    String,
    /// A whole number.
    Integer,
    /// `true` or `false`; filled in as that text.
    Boolean,
    /// A non-empty file path or program name.
    Path,
    /// A list of strings. Its placeholder must be a whole argv element, which becomes one element per item.
    Strings,
}

/// How a tool runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolAction {
    /// A separate process, with this argv template; never through a shell.
    Run(Vec<String>),
    /// A first-party implementation in the app (such as `http.send`), named here.
    Builtin(String),
}

/// An event a plugin can fire, and the loop action it maps to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EventSpec {
    /// `<namespace>.<name>`, such as `response.status`.
    pub name: String,
    /// Which output lines fire it (`match` in the manifest).
    #[serde(rename = "match")]
    pub matcher: EventMatcher,
    /// What the loop does when it fires.
    pub action: LoopAction,
}

/// Which output lines fire an event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventMatcher {
    /// A line matching this regular expression anywhere.
    LineRegex(Pattern),
    /// A line that is a JSON object with this field (a dotted path such as `response.status`), equal to `equals`
    /// when given (a string, or a number or boolean written as text), or present and not null otherwise.
    JsonField {
        /// The field's dotted path.
        field: String,
        /// The value it must have.
        equals: Option<String>,
    },
}

/// A compiled regular expression that compares and serializes as its source.
#[derive(Clone, Debug)]
pub struct Pattern {
    regex: Regex,
}

impl Pattern {
    /// Compile `source`, or return the regex crate's message.
    pub fn new(source: &str) -> std::result::Result<Pattern, String> {
        Regex::new(source)
            .map(|regex| Pattern { regex })
            .map_err(|e| e.to_string())
    }

    /// The source text.
    pub fn as_str(&self) -> &str {
        self.regex.as_str()
    }

    /// The compiled expression.
    pub fn regex(&self) -> &Regex {
        &self.regex
    }
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for Pattern {}

impl Serialize for Pattern {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// What a space's loop does when a plugin event fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoopAction {
    /// Stop the loop at its stage.
    Fail,
    /// The stage's check passed, so an *until checks pass* loop moves on.
    Pass,
    /// Post a notice to the Inbox; the loop carries on.
    Notice,
}

/// Which `MISE_ENV`s a plugin may run under.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Scope {
    /// Patterns: [`SCOPE_SPACE`] for the space's own environment, [`SCOPE_PRODUCTION`] for production (honoured
    /// only once the space was widened to production), or a literal `MISE_ENV` name. Never empty.
    pub mise_envs: Vec<String>,
}

impl Default for Scope {
    /// Only the space's own environment: never production.
    fn default() -> Self {
        Scope {
            mise_envs: vec![SCOPE_SPACE.to_owned()],
        }
    }
}

impl Scope {
    /// True when the pattern list includes `pattern` literally.
    pub fn allows(&self, pattern: &str) -> bool {
        self.mise_envs.iter().any(|p| p == pattern)
    }
}

impl Manifest {
    /// Parse and validate a manifest. `origin` names it in errors (a path, or `built-in <name>`).
    pub fn parse(text: &str, origin: &str) -> Result<Manifest> {
        let raw: RawManifest = toml::from_str(text).map_err(|e| PluginError::Syntax {
            origin: origin.to_owned(),
            message: e.to_string().trim_end().to_owned(),
        })?;
        raw.validate(origin)
    }

    /// Read, parse and validate the manifest at `path`.
    pub fn load(path: &Path) -> Result<Manifest> {
        let text = std::fs::read_to_string(path).map_err(|source| PluginError::Io {
            path: path.to_owned(),
            source,
        })?;
        Manifest::parse(&text, &path.display().to_string())
    }

    /// The tool named `name`.
    pub fn tool(&self, name: &str) -> Option<&ToolSpec> {
        self.tools.iter().find(|t| t.name == name)
    }

    /// The event named `name`.
    pub fn event(&self, name: &str) -> Option<&EventSpec> {
        self.events.iter().find(|e| e.name == name)
    }

    /// The loop action the event named `name` maps to, for an event a tool emits by name (`plugin.emit`).
    pub fn action_for(&self, event: &str) -> Option<LoopAction> {
        self.event(event).map(|e| e.action)
    }

    /// True when any tool writes, so mounting it (for a connector) is gated.
    pub fn has_write_tools(&self) -> bool {
        self.tools.iter().any(|t| t.effect == Effect::Write)
    }
}

/// True for a plugin name: non-empty lowercase letters, digits and `-`.
pub fn is_plugin_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// True for a tool or event name: `<namespace>.<verb>`, each part starting with a lowercase letter, then
/// lowercase letters, digits, `-` and `_`.
pub fn is_dotted_name(name: &str) -> bool {
    let part = |s: &str| {
        let mut chars = s.chars();
        matches!(chars.next(), Some('a'..='z'))
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    };
    match name.split_once('.') {
        Some((ns, verb)) => part(ns) && part(verb),
        None => false,
    }
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('A'..='Z' | '_'))
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn is_mise_env_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    name: String,
    version: String,
    #[serde(default)]
    description: String,
    pane: RawPane,
    #[serde(default)]
    tools: Vec<RawTool>,
    #[serde(default)]
    events: Vec<RawEvent>,
    scope: Option<RawScope>,
    #[serde(default)]
    secrets: Vec<String>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum RawPane {
    Terminal {
        #[serde(default)]
        run: Vec<String>,
    },
    Http {
        base_url: Option<String>,
    },
    Logs {
        run: Option<Vec<String>>,
        file: Option<String>,
    },
    Sql {
        engine: SqlEngine,
        database: String,
    },
    Web {
        url: Option<String>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTool {
    name: String,
    #[serde(default)]
    description: String,
    effect: Effect,
    #[serde(default)]
    params: Vec<RawParam>,
    run: Option<Vec<String>>,
    builtin: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawParam {
    name: String,
    #[serde(rename = "type")]
    kind: ParamKind,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvent {
    name: String,
    #[serde(rename = "match")]
    matcher: RawMatch,
    action: LoopAction,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMatch {
    regex: Option<String>,
    field: Option<String>,
    equals: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScope {
    mise_envs: Vec<String>,
}

struct Check<'a> {
    origin: &'a str,
}

impl Check<'_> {
    fn err(&self, field: impl Into<String>, message: impl Into<String>) -> PluginError {
        PluginError::Invalid {
            origin: self.origin.to_owned(),
            field: field.into(),
            message: message.into(),
        }
    }

    /// A template that may only use the space's placeholders.
    fn context_template(&self, field: &str, value: &str) -> Result<()> {
        let pieces = template::parse(value).map_err(|m| self.err(field, m))?;
        match pieces.iter().find_map(|p| match p {
            Piece::Param(name) => Some(name),
            _ => None,
        }) {
            Some(name) => Err(self.err(field, format!("`{{{name}}}` is a parameter; only {{PORT}}, {{WORKTREE}} and {{SPACE}} can be used here"))),
            None => Ok(()),
        }
    }

    fn command(&self, field: &str, argv: &[String]) -> Result<()> {
        if argv.first().is_none_or(|p| p.is_empty()) {
            return Err(self.err(field, "needs a program as its first element"));
        }
        for (i, element) in argv.iter().enumerate() {
            self.context_template(&format!("{field}[{i}]"), element)?;
        }
        Ok(())
    }
}

impl RawManifest {
    fn validate(self, origin: &str) -> Result<Manifest> {
        let check = Check { origin };
        if !is_plugin_name(&self.name) {
            return Err(check.err(
                "name",
                format!("{:?} must be lowercase letters, digits and `-`", self.name),
            ));
        }
        if self.version.trim().is_empty() {
            return Err(check.err("version", "must not be empty"));
        }
        let pane = match self.pane {
            RawPane::Terminal { run } => {
                if !run.is_empty() {
                    check.command("pane.run", &run)?;
                }
                PaneSpec::Terminal { run }
            }
            RawPane::Http { base_url } => {
                if let Some(url) = &base_url {
                    check.context_template("pane.base_url", url)?;
                }
                PaneSpec::Http { base_url }
            }
            RawPane::Logs { run, file } => {
                if run.is_some() && file.is_some() {
                    return Err(
                        check.err("pane", "a logs pane tails either `run` or `file`, not both")
                    );
                }
                if let Some(run) = &run {
                    check.command("pane.run", run)?;
                }
                if let Some(file) = &file {
                    if file.is_empty() {
                        return Err(check.err("pane.file", "must not be empty"));
                    }
                    check.context_template("pane.file", file)?;
                }
                PaneSpec::Logs { run, file }
            }
            RawPane::Sql { engine, database } => {
                if database.trim().is_empty() {
                    return Err(check.err("pane.database", "must not be empty"));
                }
                check.context_template("pane.database", &database)?;
                PaneSpec::Sql { engine, database }
            }
            RawPane::Web { url } => {
                if let Some(url) = &url {
                    check.context_template("pane.url", url)?;
                }
                PaneSpec::Web { url }
            }
        };

        let mut tools: Vec<ToolSpec> = Vec::with_capacity(self.tools.len());
        for (i, raw) in self.tools.into_iter().enumerate() {
            let field = format!("tools[{i}]");
            if !is_dotted_name(&raw.name) {
                return Err(check.err(
                    format!("{field}.name"),
                    format!(
                        "{:?} must be `<namespace>.<verb>` in lowercase, such as `http.send`",
                        raw.name
                    ),
                ));
            }
            if tools.iter().any(|t| t.name == raw.name) {
                return Err(check.err(
                    format!("{field}.name"),
                    format!("a second tool named {:?}", raw.name),
                ));
            }
            let mut params: Vec<ParamSpec> = Vec::with_capacity(raw.params.len());
            for (j, p) in raw.params.into_iter().enumerate() {
                let pfield = format!("{field}.params[{j}].name");
                if !template::is_param_name(&p.name) {
                    return Err(check.err(
                        pfield,
                        format!("{:?} must start with a lowercase letter, then lowercase letters, digits and `_`", p.name),
                    ));
                }
                if params.iter().any(|q| q.name == p.name) {
                    return Err(check.err(pfield, format!("a second parameter named {:?}", p.name)));
                }
                params.push(ParamSpec {
                    name: p.name,
                    kind: p.kind,
                    required: p.required,
                    description: p.description,
                });
            }
            let action = match (raw.run, raw.builtin) {
                (Some(_), Some(_)) => {
                    return Err(check.err(field, "has both `run` and `builtin`; give one"));
                }
                (None, None) => {
                    return Err(
                        check.err(field, "needs `run = [argv...]` or `builtin = \"<name>\"`")
                    );
                }
                (None, Some(builtin)) => {
                    if builtin.trim().is_empty() {
                        return Err(check.err(format!("{field}.builtin"), "must not be empty"));
                    }
                    ToolAction::Builtin(builtin)
                }
                (Some(run), None) => {
                    validate_run(&check, &field, &run, &params)?;
                    ToolAction::Run(run)
                }
            };
            tools.push(ToolSpec {
                name: raw.name,
                description: raw.description,
                effect: raw.effect,
                params,
                action,
            });
        }

        let mut events: Vec<EventSpec> = Vec::with_capacity(self.events.len());
        for (i, raw) in self.events.into_iter().enumerate() {
            let field = format!("events[{i}]");
            if !is_dotted_name(&raw.name) {
                return Err(check.err(
                    format!("{field}.name"),
                    format!(
                        "{:?} must be `<namespace>.<name>` in lowercase, such as `logs.error`",
                        raw.name
                    ),
                ));
            }
            if events.iter().any(|e| e.name == raw.name) {
                return Err(check.err(
                    format!("{field}.name"),
                    format!("a second event named {:?}", raw.name),
                ));
            }
            let matcher = match (raw.matcher.regex, raw.matcher.field, raw.matcher.equals) {
                (Some(regex), None, None) => EventMatcher::LineRegex(
                    Pattern::new(&regex)
                        .map_err(|m| check.err(format!("{field}.match.regex"), m))?,
                ),
                (None, Some(json_field), equals) => {
                    if json_field.is_empty() || json_field.split('.').any(str::is_empty) {
                        return Err(check.err(
                            format!("{field}.match.field"),
                            "must be a field name or a dotted path such as `response.status`",
                        ));
                    }
                    EventMatcher::JsonField {
                        field: json_field,
                        equals,
                    }
                }
                (None, None, Some(_)) => {
                    return Err(check.err(format!("{field}.match"), "`equals` needs a `field`"));
                }
                (None, None, None) => {
                    return Err(check.err(format!("{field}.match"), "needs `regex` or `field`"));
                }
                (Some(_), _, _) => {
                    return Err(check.err(
                        format!("{field}.match"),
                        "has both `regex` and `field`; give one",
                    ));
                }
            };
            events.push(EventSpec {
                name: raw.name,
                matcher,
                action: raw.action,
            });
        }

        let scope = match self.scope {
            None => Scope::default(),
            Some(raw) => {
                if raw.mise_envs.is_empty() {
                    return Err(check.err(
                        "scope.mise_envs",
                        "must name at least one environment, such as \"space\"",
                    ));
                }
                for (i, env) in raw.mise_envs.iter().enumerate() {
                    if !is_mise_env_name(env) {
                        return Err(check.err(
                            format!("scope.mise_envs[{i}]"),
                            format!("{env:?} must be \"space\", \"production\" or a MISE_ENV name"),
                        ));
                    }
                }
                let mut mise_envs = raw.mise_envs;
                dedup(&mut mise_envs);
                Scope { mise_envs }
            }
        };

        for (i, secret) in self.secrets.iter().enumerate() {
            if !is_env_name(secret) {
                return Err(check.err(
                    format!("secrets[{i}]"),
                    format!("{secret:?} must be an env name in capitals, such as JIRA_API_TOKEN"),
                ));
            }
        }
        let mut secrets = self.secrets;
        dedup(&mut secrets);

        Ok(Manifest {
            name: self.name,
            version: self.version,
            description: self.description,
            pane,
            tools,
            events,
            scope,
            secrets,
        })
    }
}

fn dedup(items: &mut Vec<String>) {
    let mut seen = Vec::with_capacity(items.len());
    items.retain(|item| {
        if seen.contains(item) {
            false
        } else {
            seen.push(item.clone());
            true
        }
    });
}

fn validate_run(
    check: &Check<'_>,
    field: &str,
    run: &[String],
    params: &[ParamSpec],
) -> Result<()> {
    if run.is_empty() {
        return Err(check.err(
            format!("{field}.run"),
            "needs a program as its first element",
        ));
    }
    for (i, element) in run.iter().enumerate() {
        let efield = format!("{field}.run[{i}]");
        let pieces = template::parse(element).map_err(|m| check.err(&efield, m))?;
        for piece in &pieces {
            let Piece::Param(name) = piece else { continue };
            let Some(param) = params.iter().find(|p| &p.name == name) else {
                return Err(check.err(
                    &efield,
                    format!("`{{{name}}}` is not one of the tool's parameters"),
                ));
            };
            if param.kind == ParamKind::Strings && pieces.len() != 1 {
                return Err(check.err(
                    &efield,
                    format!(
                        "`{{{name}}}` is a list, so it must be a whole argv element on its own"
                    ),
                ));
            }
        }
        if i == 0 && pieces == [Piece::Lit(String::new())] {
            return Err(check.err(&efield, "the program must not be empty"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
name = "demo"
version = "1"
[pane]
kind = "terminal"
"#;

    fn invalid_field(text: &str) -> String {
        match Manifest::parse(text, "test") {
            Err(PluginError::Invalid { field, .. }) => field,
            other => panic!("expected an invalid field, got {other:?}"),
        }
    }

    #[test]
    fn minimal_manifest_defaults_to_the_space_scope() {
        let m = Manifest::parse(MINIMAL, "test").expect("parses");
        assert_eq!(m.scope, Scope::default());
        assert_eq!(m.pane, PaneSpec::Terminal { run: vec![] });
        assert!(m.tools.is_empty() && m.events.is_empty() && m.secrets.is_empty());
    }

    #[test]
    fn syntax_errors_name_the_origin() {
        let err = Manifest::parse("name = ", "/x/plugin.toml").expect_err("bad toml");
        assert!(
            matches!(&err, PluginError::Syntax { origin, .. } if origin == "/x/plugin.toml"),
            "{err}"
        );
        let unknown = format!("{MINIMAL}\ncolour = \"red\"\n");
        assert!(matches!(
            Manifest::parse(&unknown, "t"),
            Err(PluginError::Syntax { .. })
        ));
        let bad_kind = "name = \"a\"\nversion = \"1\"\n[pane]\nkind = \"video\"\n";
        assert!(matches!(
            Manifest::parse(bad_kind, "t"),
            Err(PluginError::Syntax { .. })
        ));
        let unknown_pane_field =
            "name = \"a\"\nversion = \"1\"\n[pane]\nkind = \"web\"\nurls = \"x\"\n";
        assert!(matches!(
            Manifest::parse(unknown_pane_field, "t"),
            Err(PluginError::Syntax { .. })
        ));
    }

    #[test]
    fn invalid_fields_are_named() {
        assert_eq!(invalid_field(&MINIMAL.replace("demo", "Demo_1")), "name");
        assert_eq!(
            invalid_field(&MINIMAL.replace("version = \"1\"", "version = \" \"")),
            "version"
        );

        let tool = |body: &str| format!("{MINIMAL}\n[[tools]]\n{body}\n");
        assert_eq!(
            invalid_field(&tool("name = \"run\"\neffect = \"read\"\nrun = [\"ls\"]")),
            "tools[0].name"
        );
        assert_eq!(
            invalid_field(&tool("name = \"a.b\"\neffect = \"read\"")),
            "tools[0]"
        );
        assert_eq!(
            invalid_field(&tool(
                "name = \"a.b\"\neffect = \"read\"\nrun = [\"ls\"]\nbuiltin = \"x\""
            )),
            "tools[0]"
        );
        assert_eq!(
            invalid_field(&tool(
                "name = \"a.b\"\neffect = \"read\"\nrun = [\"ls\", \"{x}\"]"
            )),
            "tools[0].run[1]"
        );
        assert_eq!(
            invalid_field(&tool("name = \"a.b\"\neffect = \"read\"\nrun = [\"\"]")),
            "tools[0].run[0]"
        );
        assert_eq!(
            invalid_field(&tool("name = \"a.b\"\neffect = \"read\"\nrun = []")),
            "tools[0].run"
        );
        assert_eq!(
            invalid_field(&tool(
                "name = \"a.b\"\neffect = \"read\"\nrun = [\"ls\", \"-{xs}\"]\nparams = [{ name = \"xs\", type = \"strings\" }]"
            )),
            "tools[0].run[1]"
        );
        assert_eq!(
            invalid_field(&tool(
                "name = \"a.b\"\neffect = \"read\"\nrun = [\"ls\"]\nparams = [{ name = \"x\", type = \"string\" }, { name = \"x\", type = \"path\" }]"
            )),
            "tools[0].params[1].name"
        );
        let two = format!(
            "{MINIMAL}\n[[tools]]\nname = \"a.b\"\neffect = \"read\"\nbuiltin = \"x\"\n[[tools]]\nname = \"a.b\"\neffect = \"write\"\nbuiltin = \"y\"\n"
        );
        assert_eq!(invalid_field(&two), "tools[1].name");

        let event = |body: &str| format!("{MINIMAL}\n[[events]]\n{body}\n");
        assert_eq!(
            invalid_field(&event(
                "name = \"a.b\"\naction = \"fail\"\nmatch = { regex = \"(unclosed\" }"
            )),
            "events[0].match.regex"
        );
        assert_eq!(
            invalid_field(&event("name = \"a.b\"\naction = \"fail\"\nmatch = {}")),
            "events[0].match"
        );
        assert_eq!(
            invalid_field(&event(
                "name = \"a.b\"\naction = \"fail\"\nmatch = { regex = \"x\", field = \"y\" }"
            )),
            "events[0].match"
        );

        assert_eq!(
            invalid_field(&format!("{MINIMAL}\n[scope]\nmise_envs = []\n")),
            "scope.mise_envs"
        );
        assert_eq!(
            invalid_field(&format!("{MINIMAL}\n[scope]\nmise_envs = [\"a b\"]\n")),
            "scope.mise_envs[0]"
        );
        assert_eq!(
            invalid_field(&MINIMAL.replace("version", "secrets = [\"lower\"]\nversion")),
            "secrets[0]"
        );

        let logs_both =
            "name = \"a\"\nversion = \"1\"\n[pane]\nkind = \"logs\"\nrun = [\"x\"]\nfile = \"y\"\n";
        assert_eq!(invalid_field(logs_both), "pane");
        let web_param =
            "name = \"a\"\nversion = \"1\"\n[pane]\nkind = \"web\"\nurl = \"http://x/{path}\"\n";
        assert_eq!(invalid_field(web_param), "pane.url");
    }

    #[test]
    fn tools_and_events_round_out() {
        let text = format!(
            r#"{MINIMAL}
[[tools]]
name = "fs.list"
effect = "read"
run = ["ls", "{{dir}}", "{{flags}}"]
params = [
  {{ name = "dir", type = "path", required = true, description = "a folder" }},
  {{ name = "flags", type = "strings" }},
]
[[events]]
name = "build.failed"
action = "fail"
match = {{ field = "result.status", equals = "failed" }}
"#
        );
        let m = Manifest::parse(&text, "t").expect("parses");
        let tool = m.tool("fs.list").expect("tool");
        assert_eq!(tool.param("dir").map(|p| p.kind), Some(ParamKind::Path));
        let schema = tool.input_schema();
        assert_eq!(schema["required"], json!(["dir"]));
        assert_eq!(schema["properties"]["flags"]["type"], "array");
        assert_eq!(m.action_for("build.failed"), Some(LoopAction::Fail));
        assert_eq!(m.action_for("nope"), None);
        assert!(!m.has_write_tools());
        let json = serde_json::to_value(&m).expect("serializes");
        assert_eq!(json["pane"]["kind"], "terminal");
        assert_eq!(json["tools"][0]["action"]["run"][0], "ls");
    }
}
