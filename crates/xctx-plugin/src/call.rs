//! Planning a tool call: checking its arguments and the space's `MISE_ENV` against the manifest, and filling the
//! argv template.
//!
//! Placeholders fill whole argv elements only, and no shell is ever involved, so an argument can never become
//! more than the one element it was put in: `"; rm -rf /"`, spaces, newlines and `{other}` stay literal text. A
//! `strings` parameter, whose placeholder is a whole element, becomes one element per item. An element naming an
//! optional parameter the call left out is dropped.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value};
use xctx_core::Redactor;

use crate::error::{PluginError, Result};
use crate::manifest::{
    Effect, Manifest, PaneSpec, ParamKind, SCOPE_PRODUCTION, SCOPE_SPACE, ToolAction, ToolSpec,
};
use crate::template::{self, CtxVar, Piece};

/// The env var a tool is told its space's slug in.
pub const SPACE_ENV: &str = "XCTX_SPACE";

/// The space a call runs for.
#[derive(Clone, Debug, Default)]
pub struct CallContext {
    /// The space's worktree: the tool's working directory and `{WORKTREE}`.
    pub worktree: PathBuf,
    /// The space's slug: `{SPACE}` and `XCTX_SPACE`.
    pub space: String,
    /// The space's port, if it has one: `{PORT}` and `PORT`.
    pub port: Option<u16>,
    /// The space's effective `MISE_ENV`: its slug, or `production,<slug>` once widened to production.
    pub mise_env: String,
    /// True once the user approved widening the space to production.
    pub space_production: bool,
    /// The secrets the app resolved from the space's mise environment, by env name. Only those the manifest
    /// names are passed to the tool.
    pub secrets: BTreeMap<String, String>,
    /// Masks the space's secrets in the tool's output.
    pub redactor: Redactor,
}

/// What a planned call does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invocation {
    /// Run this argv as a process, without a shell (see [`crate::run_plan`]).
    Run {
        /// The filled argv; `argv[0]` is the program.
        argv: Vec<String>,
    },
    /// Hand the call to the app's built-in implementation named `name`, with the checked arguments.
    Builtin {
        /// The built-in, such as `http.send`.
        name: String,
        /// The call's arguments, checked against the tool's parameters.
        args: Map<String, Value>,
    },
}

/// A checked tool call, ready to run (or to put in an Inbox gate first, for a write tool).
#[derive(Clone, Debug)]
pub struct CallPlan {
    /// The plugin.
    pub plugin: String,
    /// The tool.
    pub tool: String,
    /// Whether it reads or writes; the app gates writes in the Inbox.
    pub effect: Effect,
    /// What it does.
    pub invocation: Invocation,
    /// Env vars set for it on top of xctx's own: `MISE_ENV`, `PORT` (when known), `XCTX_SPACE` and the
    /// manifest's secrets.
    pub env: BTreeMap<String, String>,
    /// Its working directory: the space's worktree.
    pub cwd: PathBuf,
    /// Masks the space's secrets in its output.
    pub redactor: Redactor,
}

impl CallPlan {
    /// The argv when the plan runs a process.
    pub fn argv(&self) -> Option<&[String]> {
        match &self.invocation {
            Invocation::Run { argv } => Some(argv),
            Invocation::Builtin { .. } => None,
        }
    }

    /// The built-in's name when the plan is a built-in.
    pub fn builtin(&self) -> Option<&str> {
        match &self.invocation {
            Invocation::Builtin { name, .. } => Some(name),
            Invocation::Run { .. } => None,
        }
    }
}

/// Check a call of `manifest`'s tool `tool` with `args`, and plan it for the space in `ctx`.
///
/// Refuses unknown tools, unknown or missing arguments, arguments of the wrong type, a `MISE_ENV` outside the
/// manifest's scope (see [`check_scope`]), unknown `{PORT}` and secrets the manifest needs but `ctx` lacks.
pub fn plan_call(
    manifest: &Manifest,
    tool: &str,
    args: &Map<String, Value>,
    ctx: &CallContext,
) -> Result<CallPlan> {
    let spec = manifest
        .tool(tool)
        .ok_or_else(|| PluginError::UnknownTool {
            plugin: manifest.name.clone(),
            tool: tool.to_owned(),
        })?;
    let values = check_args(spec, args)?;
    check_scope(manifest, ctx)?;

    let mut env = BTreeMap::new();
    env.insert("MISE_ENV".to_owned(), ctx.mise_env.clone());
    env.insert(SPACE_ENV.to_owned(), ctx.space.clone());
    if let Some(port) = ctx.port {
        env.insert("PORT".to_owned(), port.to_string());
    }
    for name in &manifest.secrets {
        let value = ctx
            .secrets
            .get(name)
            .ok_or_else(|| PluginError::MissingSecret {
                plugin: manifest.name.clone(),
                name: name.clone(),
            })?;
        env.insert(name.clone(), value.clone());
    }

    let invocation = match &spec.action {
        ToolAction::Builtin(name) => {
            let args = args
                .iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            Invocation::Builtin {
                name: name.clone(),
                args,
            }
        }
        ToolAction::Run(template) => Invocation::Run {
            argv: fill_argv(spec, template, &values, ctx)?,
        },
    };
    Ok(CallPlan {
        plugin: manifest.name.clone(),
        tool: spec.name.clone(),
        effect: spec.effect,
        invocation,
        env,
        cwd: ctx.worktree.clone(),
        redactor: ctx.redactor.clone(),
    })
}

/// Check that `manifest` may run under the space's `MISE_ENV`.
///
/// Every environment in the comma-separated `MISE_ENV` must be allowed: the space's own slug by the `space`
/// pattern (or its literal name), `production` only when the space was widened to production *and* the scope
/// lists `production`, and any other name only when listed literally. An empty `MISE_ENV` is refused.
pub fn check_scope(manifest: &Manifest, ctx: &CallContext) -> Result<()> {
    let refuse = |reason: String| PluginError::ScopeRefused {
        plugin: manifest.name.clone(),
        mise_env: ctx.mise_env.clone(),
        reason,
    };
    let envs: Vec<&str> = ctx
        .mise_env
        .split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .collect();
    if envs.is_empty() {
        return Err(refuse("the space has no MISE_ENV".to_owned()));
    }
    for env in envs {
        if env == SCOPE_PRODUCTION {
            if !ctx.space_production {
                return Err(refuse("the space was not widened to production".to_owned()));
            }
            if !manifest.scope.allows(SCOPE_PRODUCTION) {
                return Err(refuse(
                    "the plugin's scope does not include production".to_owned(),
                ));
            }
        } else if !(manifest.scope.allows(env)
            || (env == ctx.space && manifest.scope.allows(SCOPE_SPACE)))
        {
            return Err(refuse(format!(
                "the plugin's scope does not include {env:?}"
            )));
        }
    }
    Ok(())
}

/// Fill the space's placeholders (`{PORT}`, `{WORKTREE}`, `{SPACE}`) in a pane setting, such as a terminal
/// pane's argv element or a base URL. Fails on a parameter placeholder or an unknown `{PORT}`.
pub fn fill_context(text: &str, ctx: &CallContext) -> Result<String> {
    let pieces = template::parse(text).map_err(|message| PluginError::Invalid {
        origin: "pane".to_owned(),
        field: text.to_owned(),
        message,
    })?;
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Lit(s) => out.push_str(&s),
            Piece::Ctx(var) => out.push_str(&context_value(var, ctx)?),
            Piece::Param(name) => {
                return Err(PluginError::Invalid {
                    origin: "pane".to_owned(),
                    field: text.to_owned(),
                    message: format!(
                        "`{{{name}}}` is a parameter, which a pane setting cannot use"
                    ),
                });
            }
        }
    }
    Ok(out)
}

/// The argv of a pane's command with the space's placeholders filled: a terminal pane's `run`, or a logs pane's
/// `run`. None for other panes, and for a terminal pane whose command is chosen when mounting.
pub fn pane_argv(pane: &PaneSpec, ctx: &CallContext) -> Result<Option<Vec<String>>> {
    let run = match pane {
        PaneSpec::Terminal { run } if !run.is_empty() => run,
        PaneSpec::Logs { run: Some(run), .. } => run,
        _ => return Ok(None),
    };
    run.iter()
        .map(|e| fill_context(e, ctx))
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

/// A checked argument.
enum ArgValue {
    One(String),
    Many(Vec<String>),
}

fn check_args(spec: &ToolSpec, args: &Map<String, Value>) -> Result<BTreeMap<String, ArgValue>> {
    if let Some(unknown) = args.keys().find(|k| spec.param(k).is_none()) {
        return Err(PluginError::UnknownParam {
            tool: spec.name.clone(),
            param: unknown.clone(),
        });
    }
    let mut values = BTreeMap::new();
    for param in &spec.params {
        let bad = |message: &str| PluginError::BadParam {
            tool: spec.name.clone(),
            param: param.name.clone(),
            message: message.to_owned(),
        };
        let value = match args.get(&param.name) {
            None | Some(Value::Null) => {
                if param.required {
                    return Err(PluginError::MissingParam {
                        tool: spec.name.clone(),
                        param: param.name.clone(),
                    });
                }
                continue;
            }
            Some(value) => value,
        };
        let checked = match (param.kind, value) {
            (ParamKind::String, Value::String(s)) => ArgValue::One(s.clone()),
            (ParamKind::Path, Value::String(s)) => {
                if s.is_empty() {
                    return Err(bad("a path must not be empty"));
                }
                ArgValue::One(s.clone())
            }
            (ParamKind::Integer, Value::Number(n)) => match n.as_i64() {
                Some(i) => ArgValue::One(i.to_string()),
                None => return Err(bad("expected a whole number")),
            },
            (ParamKind::Boolean, Value::Bool(b)) => ArgValue::One(b.to_string()),
            (ParamKind::Strings, Value::Array(items)) => {
                let mut list = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        Value::String(s) => list.push(s.clone()),
                        _ => return Err(bad("expected a list of strings")),
                    }
                }
                ArgValue::Many(list)
            }
            (kind, _) => {
                let expected = match kind {
                    ParamKind::String => "expected a string",
                    ParamKind::Path => "expected a path as a string",
                    ParamKind::Integer => "expected a whole number",
                    ParamKind::Boolean => "expected true or false",
                    ParamKind::Strings => "expected a list of strings",
                };
                return Err(bad(expected));
            }
        };
        let has_nul = match &checked {
            ArgValue::One(s) => s.contains('\0'),
            ArgValue::Many(list) => list.iter().any(|s| s.contains('\0')),
        };
        if has_nul {
            return Err(bad("must not contain a NUL character"));
        }
        values.insert(param.name.clone(), checked);
    }
    Ok(values)
}

fn context_value(var: CtxVar, ctx: &CallContext) -> Result<String> {
    match var {
        CtxVar::Port => {
            ctx.port
                .map(|p| p.to_string())
                .ok_or_else(|| PluginError::MissingContext {
                    what: "PORT".to_owned(),
                })
        }
        CtxVar::Worktree => Ok(ctx.worktree.display().to_string()),
        CtxVar::Space => Ok(ctx.space.clone()),
    }
}

fn fill_argv(
    spec: &ToolSpec,
    template: &[String],
    values: &BTreeMap<String, ArgValue>,
    ctx: &CallContext,
) -> Result<Vec<String>> {
    let mut argv = Vec::with_capacity(template.len());
    'elements: for element in template {
        let pieces = template::parse(element).map_err(|message| PluginError::Invalid {
            origin: spec.name.clone(),
            field: element.clone(),
            message,
        })?;
        if let [Piece::Param(name)] = pieces.as_slice() {
            match values.get(name) {
                Some(ArgValue::Many(list)) => argv.extend(list.iter().cloned()),
                Some(ArgValue::One(s)) => argv.push(s.clone()),
                None => {}
            }
            continue;
        }
        let mut out = String::new();
        for piece in pieces {
            match piece {
                Piece::Lit(s) => out.push_str(&s),
                Piece::Ctx(var) => out.push_str(&context_value(var, ctx)?),
                Piece::Param(name) => match values.get(&name) {
                    Some(ArgValue::One(s)) => out.push_str(s),
                    Some(ArgValue::Many(_)) => {
                        return Err(PluginError::BadParam {
                            tool: spec.name.clone(),
                            param: name,
                            message: "a list cannot be part of a larger argument".to_owned(),
                        });
                    }
                    None => continue 'elements,
                },
            }
        }
        argv.push(out);
    }
    match argv.first() {
        Some(program) if !program.is_empty() => Ok(argv),
        _ => Err(PluginError::BadParam {
            tool: spec.name.clone(),
            param: "argv[0]".to_owned(),
            message: "the program is empty".to_owned(),
        }),
    }
}
