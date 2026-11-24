//! What `xctx-ctl` does with its arguments, kept here so it can be tested without a process.
//!
//! ```text
//! xctx-ctl [--socket <path>] [--compact] <method> [key=value | key:=<json>]...
//! ```
//!
//! `key=value` passes a string, except that `pane` is a number, `all` a boolean and `repos` a comma-separated
//! list. `key:=<json>` passes any JSON value. Methods about a space take `space` from `$XCTX_SPACE` when it is not
//! given, so an agent in a space's pane can say `xctx-ctl pane.split axis=vertical`.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::path::CONTROL_SOCKET_ENV;
use crate::protocol::{Call, Method, RpcError};

/// The environment variable a space's panes have, naming the space. `xctx-ctl` uses it as the default `space`.
pub const SPACE_ENV: &str = "XCTX_SPACE";

/// Parameters that are numbers when given as `key=value`.
const INTEGER_KEYS: &[&str] = &["pane"];
/// Parameters that are booleans when given as `key=value`.
const BOOL_KEYS: &[&str] = &["all"];
/// Parameters that are comma-separated lists when given as `key=value`.
const LIST_KEYS: &[&str] = &["repos"];

/// What the command line asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Print [`help`].
    Help,
    /// Print the version.
    Version,
    /// Make a call.
    Call {
        /// `--socket`, overriding the default socket path.
        socket: Option<PathBuf>,
        /// `--compact`: print the result on one line.
        compact: bool,
        /// The call.
        call: Call,
    },
}

/// A command line that cannot be run, with the message to print.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct UsageError(pub String);

impl From<RpcError> for UsageError {
    fn from(e: RpcError) -> Self {
        UsageError(e.message)
    }
}

/// Whether `method` takes its `space` from `$XCTX_SPACE` when none is given.
pub fn defaults_space(method: Method) -> bool {
    !matches!(
        method,
        Method::Ping | Method::SpaceList | Method::SpaceDraft | Method::InboxList
    )
}

/// Parse the arguments after the program name. `default_space` is `$XCTX_SPACE`.
pub fn parse_args(
    args: impl IntoIterator<Item = String>,
    default_space: Option<&str>,
) -> Result<Command, UsageError> {
    let mut socket = None;
    let mut compact = false;
    let mut args = args.into_iter();
    let method = loop {
        let Some(arg) = args.next() else {
            return Ok(Command::Help);
        };
        match arg.as_str() {
            "-h" | "--help" | "help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--compact" => compact = true,
            "--socket" => {
                let path = args
                    .next()
                    .ok_or_else(|| UsageError("--socket needs a path".into()))?;
                socket = Some(PathBuf::from(path));
            }
            s if s.starts_with("--socket=") => {
                socket = Some(PathBuf::from(&s["--socket=".len()..]));
            }
            s if s.starts_with('-') => return Err(UsageError(format!("unknown option {s}"))),
            _ => break arg,
        }
    };
    let Some(m) = Method::from_name(&method) else {
        return Err(UsageError(format!(
            "no method named {method:?}; run xctx-ctl help for the list"
        )));
    };
    let mut params = Map::new();
    for arg in args {
        let (key, value) = parse_param(&arg)?;
        if params.insert(key.clone(), value).is_some() {
            return Err(UsageError(format!("{key} is given twice")));
        }
    }
    if defaults_space(m)
        && !params.contains_key("space")
        && let Some(space) = default_space.filter(|s| !s.is_empty())
    {
        params.insert("space".into(), Value::String(space.to_owned()));
    }
    let call = Call::parse(m.name(), Some(Value::Object(params)))?;
    Ok(Command::Call {
        socket,
        compact,
        call,
    })
}

fn parse_param(arg: &str) -> Result<(String, Value), UsageError> {
    if let Some((key, json)) = arg.split_once(":=") {
        check_key(key, arg)?;
        let value = serde_json::from_str(json)
            .map_err(|e| UsageError(format!("{key}:= is not JSON: {e}")))?;
        return Ok((key.to_owned(), value));
    }
    let Some((key, raw)) = arg.split_once('=') else {
        return Err(UsageError(format!(
            "expected key=value or key:=<json>, got {arg:?}"
        )));
    };
    check_key(key, arg)?;
    let value = if INTEGER_KEYS.contains(&key) {
        let n: u64 = raw
            .parse()
            .map_err(|_| UsageError(format!("{key} must be a number, got {raw:?}")))?;
        Value::from(n)
    } else if BOOL_KEYS.contains(&key) {
        match raw {
            "true" | "yes" | "1" => Value::Bool(true),
            "false" | "no" | "0" => Value::Bool(false),
            _ => return Err(UsageError(format!("{key} must be true or false"))),
        }
    } else if LIST_KEYS.contains(&key) {
        Value::Array(
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.to_owned()))
                .collect(),
        )
    } else {
        Value::String(raw.to_owned())
    };
    Ok((key.to_owned(), value))
}

fn check_key(key: &str, arg: &str) -> Result<(), UsageError> {
    if key.is_empty() {
        return Err(UsageError(format!("missing key in {arg:?}")));
    }
    Ok(())
}

/// The help text: usage, every method with its parameters, and the environment it reads.
pub fn help() -> String {
    let mut out = String::from(
        "xctx-ctl: drive a running xctx window over its control socket\n\n\
         usage: xctx-ctl [--socket <path>] [--compact] <method> [key=value | key:=<json>]...\n\n\
         methods:\n",
    );
    for m in Method::ALL {
        let _ = writeln!(out, "  {:<14} {}", m.name(), m.usage());
        let _ = writeln!(out, "  {:<14}   {}", "", m.summary());
    }
    let _ = write!(
        out,
        "\nenvironment:\n  \
         {CONTROL_SOCKET_ENV:<20} the socket (else control.sock in the xctx data directory)\n  \
         {SPACE_ENV:<20} the default space=, set in every pane of a space\n\n\
         The result is printed as JSON. Exit status: 0 ok, 1 the window refused, 2 bad arguments, 3 no window.\n\
         Nothing here approves or launches: space.draft posts a Plan Gate the user approves in the window.\n"
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{InboxListParams, PaneParams, PaneSplitParams, SpaceDraftParams};
    use xctx_layout::{PaneId, SplitAxis};

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    fn call(s: &str, space: Option<&str>) -> Result<Call, UsageError> {
        match parse_args(args(s), space)? {
            Command::Call { call, .. } => Ok(call),
            other => Err(UsageError(format!("not a call: {other:?}"))),
        }
    }

    #[test]
    fn no_args_and_help_print_help() -> Result<(), UsageError> {
        assert_eq!(parse_args(args(""), None)?, Command::Help);
        assert_eq!(parse_args(args("--help"), None)?, Command::Help);
        assert!(help().contains("pane.split"));
        Ok(())
    }

    #[test]
    fn the_space_comes_from_xctx_space_unless_given() -> Result<(), UsageError> {
        let expected = |space: &str| {
            Call::PaneClose(PaneParams {
                space: space.into(),
                pane: PaneId(4),
            })
        };
        assert_eq!(call("pane.close pane=4", Some("here"))?, expected("here"));
        assert_eq!(
            call("pane.close pane=4 space=there", Some("here"))?,
            expected("there")
        );
        assert_eq!(
            call("inbox.list", Some("here"))?,
            Call::InboxList(InboxListParams::default())
        );
        Ok(())
    }

    #[test]
    fn typed_keys_are_converted_and_json_passes_through() -> Result<(), UsageError> {
        assert_eq!(
            call("space.draft task=fix repos=api,,web", None)?,
            Call::SpaceDraft(SpaceDraftParams {
                task: "fix".into(),
                repos: Some(vec!["api".into(), "web".into()]),
                rationale: None,
            })
        );
        assert_eq!(
            call("pane.split space=s axis=vertical pane:=2", None)?,
            Call::PaneSplit(PaneSplitParams {
                space: "s".into(),
                pane: Some(PaneId(2)),
                axis: SplitAxis::Vertical,
                kind: None,
                repo: None,
            })
        );
        assert_eq!(
            call("inbox.list all=yes", None)?,
            Call::InboxList(InboxListParams {
                space: None,
                all: true
            })
        );
        Ok(())
    }

    #[test]
    fn options_are_read_before_the_method() -> Result<(), UsageError> {
        let parsed = parse_args(args("--socket /tmp/s.sock --compact ping"), None)?;
        assert_eq!(
            parsed,
            Command::Call {
                socket: Some(PathBuf::from("/tmp/s.sock")),
                compact: true,
                call: Call::Ping,
            }
        );
        Ok(())
    }

    #[test]
    fn mistakes_are_usage_errors() {
        for bad in [
            "pane.explode",
            "pane.close pane=x space=s",
            "pane.close space=s",
            "ping extra",
            "ping a=1 a=2",
            "--bogus ping",
            "pane.close =1",
        ] {
            assert!(parse_args(args(bad), None).is_err(), "{bad}");
        }
    }
}
