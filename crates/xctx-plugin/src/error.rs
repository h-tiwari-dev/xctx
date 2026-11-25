//! The error type of plugins: bad manifests, refused tool calls and failed tool runs.

use std::path::PathBuf;

/// A manifest that could not be read or is invalid, a refused tool call, or a tool that could not run.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// The manifest is not valid TOML, or has a field of the wrong type or an unknown field.
    #[error("{origin}: {message}")]
    Syntax {
        /// Where the manifest came from: its path, or `built-in <name>`.
        origin: String,
        /// What the TOML parser said, including the line and column.
        message: String,
    },

    /// The manifest parsed but one of its fields is invalid.
    #[error("{origin}: `{field}`: {message}")]
    Invalid {
        /// Where the manifest came from: its path, or `built-in <name>`.
        origin: String,
        /// The field, as a path such as `tools[1].params[0].name`.
        field: String,
        /// What is wrong with it.
        message: String,
    },

    /// Reading a manifest or a plugins folder failed.
    #[error("{path}: {source}")]
    Io {
        /// The file or folder.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },

    /// Two plugins have the same name; the later one was not loaded.
    #[error(
        "plugin {name:?} from {new} clashes with the one already loaded from {existing}; rename one of them"
    )]
    Clash {
        /// The plugin name.
        name: String,
        /// Where the loaded plugin came from.
        existing: String,
        /// Where the refused plugin came from.
        new: String,
    },

    /// The plugin has no tool with this name.
    #[error("plugin {plugin:?} has no tool {tool:?}")]
    UnknownTool {
        /// The plugin.
        plugin: String,
        /// The tool asked for.
        tool: String,
    },

    /// A call passed an argument the tool does not declare.
    #[error("tool {tool:?} has no parameter {param:?}")]
    UnknownParam {
        /// The tool.
        tool: String,
        /// The argument name.
        param: String,
    },

    /// A call left out a required argument.
    #[error("tool {tool:?} needs the parameter {param:?}")]
    MissingParam {
        /// The tool.
        tool: String,
        /// The parameter.
        param: String,
    },

    /// An argument has the wrong type or an unusable value.
    #[error("tool {tool:?} parameter {param:?}: {message}")]
    BadParam {
        /// The tool.
        tool: String,
        /// The parameter.
        param: String,
        /// What is wrong with the value.
        message: String,
    },

    /// The plugin may not run under the space's `MISE_ENV`.
    #[error("plugin {plugin:?} may not run under MISE_ENV={mise_env:?}: {reason}")]
    ScopeRefused {
        /// The plugin.
        plugin: String,
        /// The space's `MISE_ENV`.
        mise_env: String,
        /// Why it was refused.
        reason: String,
    },

    /// A value the call needs from the space is not known, such as `{PORT}` for a space without a port.
    #[error("{what} is not known for this space")]
    MissingContext {
        /// What is missing, such as `PORT`.
        what: String,
    },

    /// The manifest names a secret the app did not supply.
    #[error(
        "plugin {plugin:?} needs the secret {name}, which is not set in the space's mise environment"
    )]
    MissingSecret {
        /// The plugin.
        plugin: String,
        /// The secret's env name.
        name: String,
    },

    /// The plan is a built-in tool, which the app routes to its own implementation instead of running a process.
    #[error("tool {tool:?} is the built-in {builtin:?}; it is not run as a process")]
    Builtin {
        /// The tool.
        tool: String,
        /// The built-in it names.
        builtin: String,
    },

    /// The tool's program could not be started.
    #[error("could not start {program:?}: {source}")]
    Spawn {
        /// The program (`argv[0]`).
        program: String,
        /// The underlying error.
        source: std::io::Error,
    },

    /// Waiting for the tool or reading its output failed.
    #[error("running {program:?} failed: {message}")]
    Run {
        /// The program (`argv[0]`).
        program: String,
        /// What failed.
        message: String,
    },
}

/// The result type of the plugin crate's fallible operations.
pub type Result<T, E = PluginError> = std::result::Result<T, E>;
