//! xctx plugins: a pane the user sees, tools agents call, and events that steer a space's loop.
//!
//! - [`Manifest`] is a validated `plugin.toml` ([`manifest`] documents the format); [`BUILTIN_MANIFESTS`] are the
//!   first-party plugins, written in the same format.
//! - [`Registry`] loads the built-in, user (`~/.config/xctx/plugins`) and repo (`.xctx/plugins`) plugins,
//!   refuses name clashes, reports bad manifests, and tracks which repo plugins still need the user's trust.
//! - [`plan_call`] checks a tool call against its manifest and the space's `MISE_ENV`, and fills its argv without
//!   a shell; [`run_plan`] runs it with a timeout, capped output and masked secrets. Built-in tools are planned
//!   here but run by the app.
//! - [`match_events`] finds the events a manifest declares in output lines, each with its [`LoopAction`].
//!
//! - [`RunnerManifest`] is a validated `runner.toml` ([`runner`] documents the format): the program an agent pane
//!   runs and how it is given a task, resumes, opens and forks sessions. [`RunnerRegistry`] loads the built-in
//!   runners (`pi`, `claude`) and the user's (`~/.config/xctx/runners`); [`build_args`] builds a runner's arguments.
//!
//! Nothing here runs code inside xctx: plugin tools are separate processes.

#![warn(missing_docs)]

mod builtin;
mod call;
mod error;
mod event;
pub mod manifest;
mod registry;
mod run;
pub mod runner;
mod template;

pub use builtin::{BUILTIN_MANIFESTS, builtin_manifests, builtin_origin};
pub use call::{
    CallContext, CallPlan, Invocation, SPACE_ENV, check_scope, fill_context, pane_argv, plan_call,
};
pub use error::{PluginError, Result};
pub use event::{FiredEvent, loop_outcome, match_events};
pub use manifest::{
    Effect, EventMatcher, EventSpec, LoopAction, MANIFEST_FILE, Manifest, PaneSpec, ParamKind,
    ParamSpec, Pattern, Scope, SqlEngine, ToolAction, ToolSpec,
};
pub use registry::{LoadProblem, Plugin, REPO_PLUGINS_DIR, Registry, Source, manifest_hash};
pub use run::{ToolOutput, run_plan};
pub use runner::{
    ArgsTemplate, BUILTIN_RUNNERS, Capability, DEFAULT_RUNNER, RUNNER_FILE, Runner, RunnerError,
    RunnerManifest, RunnerProblem, RunnerRegistry, RunnerSession, RunnerStart, TaskStyle,
    build_args, builtin_runner_origin, stdin_task,
};
