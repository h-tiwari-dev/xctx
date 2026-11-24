//! What `xctx-ctl` does with its arguments, kept here so it can be tested without a process.
//!
//! ```text
//! xctx-ctl [--socket <path>] [--compact] <method> [key=value | key:=<json>]...
//! ```
//!
//! `key=value` passes a string, except that `pane`, `since_id`, `limit`, `max_iterations`, `max_turns` and
//! `version` are numbers, `all`, `checks_passed` and `production` booleans, and `repos` a comma-separated list.
//! `key:=<json>` passes any JSON value. Methods about a space take `space` from `$XCTX_SPACE` when it is not
//! given, so an agent in a space's pane can say `xctx-ctl pane.split axis=vertical`. Likewise `channel.post`
//! takes `from`, `contract.set` `author` and `contract.agree` `agent` from `$XCTX_AGENT_ID`, and `attempt.report`
//! takes `attempt` from `$XCTX_ATTEMPT_ID` and `token` from `$XCTX_ATTEMPT_TOKEN`.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::path::CONTROL_SOCKET_ENV;
use crate::protocol::{Call, Method, RpcError};

/// The environment variable a space's panes have, naming the space. `xctx-ctl` uses it as the default `space`.
pub const SPACE_ENV: &str = "XCTX_SPACE";

/// The environment variable an agent's pane has, naming the agent in its space's graph. `xctx-ctl` uses it as the
/// default for the parameter that names the calling agent (see [`agent_key`]).
pub const AGENT_ENV: &str = "XCTX_AGENT_ID";

/// The environment variable an attempt's panes have, naming the attempt. `xctx-ctl` uses it as `attempt=` for
/// `attempt.report`.
pub const ATTEMPT_ENV: &str = "XCTX_ATTEMPT_ID";

/// The environment variable an attempt's panes have, holding the attempt's report token. `xctx-ctl` sends it as
/// `token=` with `attempt.report`; the window refuses reports without the right one.
pub const ATTEMPT_TOKEN_ENV: &str = "XCTX_ATTEMPT_TOKEN";

/// Parameters that are numbers when given as `key=value`.
const INTEGER_KEYS: &[&str] = &[
    "pane",
    "since_id",
    "limit",
    "max_iterations",
    "max_turns",
    "version",
    "attempt",
    "round",
    "count",
    "tests_passed",
    "tests_failed",
];
/// Parameters that are booleans when given as `key=value`.
const BOOL_KEYS: &[&str] = &["all", "checks_passed", "production", "gave_up"];
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

/// The parameter of `method` that names the calling agent, which `xctx-ctl` takes from `$XCTX_AGENT_ID` when it
/// is not given: `from` for `channel.post`, `author` for `contract.set`, `agent` for `contract.agree`. `None` for
/// methods that name no caller (`graph.pause` and the like name the agent acted on, so they get no default).
pub fn agent_key(method: Method) -> Option<&'static str> {
    match method {
        Method::ChannelPost => Some("from"),
        Method::ContractSet => Some("author"),
        Method::ContractAgree => Some("agent"),
        _ => None,
    }
}

/// What `xctx-ctl` takes from its environment when a call leaves it out. Empty values count as unset.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Defaults {
    /// `$XCTX_SPACE`: the `space=` of methods about a space.
    pub space: Option<String>,
    /// `$XCTX_AGENT_ID`: the caller's agent, for the parameter [`agent_key`] names.
    pub agent: Option<String>,
    /// `$XCTX_ATTEMPT_ID`: the `attempt=` of `attempt.report`.
    pub attempt: Option<String>,
    /// `$XCTX_ATTEMPT_TOKEN`: the `token=` of `attempt.report`.
    pub attempt_token: Option<String>,
}

/// Parse the arguments after the program name. `default_space` is `$XCTX_SPACE` and `default_agent`
/// `$XCTX_AGENT_ID`; an empty one counts as unset. See [`parse_args_with`] for the attempt default too.
pub fn parse_args(
    args: impl IntoIterator<Item = String>,
    default_space: Option<&str>,
    default_agent: Option<&str>,
) -> Result<Command, UsageError> {
    parse_args_with(
        args,
        &Defaults {
            space: default_space.map(str::to_owned),
            agent: default_agent.map(str::to_owned),
            attempt: None,
            attempt_token: None,
        },
    )
}

/// Parse the arguments after the program name, filling what the call leaves out from `defaults`.
pub fn parse_args_with(
    args: impl IntoIterator<Item = String>,
    defaults: &Defaults,
) -> Result<Command, UsageError> {
    let default_space = defaults.space.as_deref();
    let default_agent = defaults.agent.as_deref();
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
    if let Some(key) = agent_key(m)
        && !params.contains_key(key)
        && let Some(agent) = default_agent.filter(|s| !s.is_empty())
    {
        params.insert(key.into(), Value::String(agent.to_owned()));
    }
    if m == Method::AttemptReport
        && !params.contains_key("attempt")
        && let Some(attempt) = defaults.attempt.as_deref().filter(|s| !s.is_empty())
    {
        let number: u64 = attempt.trim().parse().map_err(|_| {
            UsageError(format!(
                "{ATTEMPT_ENV} must be an attempt number, got {attempt:?}"
            ))
        })?;
        params.insert("attempt".into(), Value::from(number));
    }
    if m == Method::AttemptReport
        && !params.contains_key("token")
        && let Some(token) = defaults.attempt_token.as_deref().filter(|s| !s.is_empty())
    {
        params.insert("token".into(), Value::String(token.to_owned()));
    }
    let call = Call::parse(m.name(), Some(Value::Object(params)))?;
    Ok(Command::Call {
        socket,
        compact,
        call,
    })
}

fn parse_param(arg: &str) -> Result<(String, Value), UsageError> {
    // Split on the first `=` so a string value may itself contain `=` or `:=`.
    let Some((lhs, raw)) = arg.split_once('=') else {
        return Err(UsageError(format!(
            "expected key=value or key:=<json>, got {arg:?}"
        )));
    };
    if let Some(key) = lhs.strip_suffix(':') {
        check_key(key, arg)?;
        let value = serde_json::from_str(raw)
            .map_err(|e| UsageError(format!("{key}:= is not JSON: {e}")))?;
        return Ok((key.to_owned(), value));
    }
    let key = lhs;
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

/// The line `xctx-ctl` prints when the window refuses a call of `method`: the window's `message`, led by the
/// method's name unless the message already names it (most of the window's refusals do).
pub fn refusal_line(method: Method, message: &str) -> String {
    let name = method.name();
    let named = message
        .strip_prefix(name)
        .is_some_and(|rest| rest.starts_with(':'));
    if named {
        format!("xctx-ctl: {message}")
    } else {
        format!("xctx-ctl: {name}: {message}")
    }
}

/// The help text: usage, every method with its parameters, and the environment it reads.
pub fn help() -> String {
    let mut out = String::from(
        "xctx-ctl: drive a running xctx window over its control socket\n\n\
         usage: xctx-ctl [--socket <path>] [--compact] <method> [key=value | key:=<json>]...\n\n\
         methods:\n",
    );
    for m in Method::ALL {
        let _ = writeln!(out, "  {:<20} {}", m.name(), m.usage());
        let _ = writeln!(out, "  {:<20}   {}", "", m.summary());
    }
    let _ = write!(
        out,
        "\nenvironment:\n  \
         {CONTROL_SOCKET_ENV:<20} the socket (else control.sock in the xctx data directory)\n  \
         {SPACE_ENV:<20} the default space=, set in every pane of a space\n  \
         {AGENT_ENV:<20} the default from= (channel.post), author= (contract.set) and agent= \
         (contract.agree), set in every agent's pane\n  \
         {ATTEMPT_ENV:<20} the default attempt= of attempt.report, set in every attempt's panes\n  \
         {ATTEMPT_TOKEN_ENV:<20} the default token= of attempt.report, set in every attempt's panes\n\n\
         The result is printed as JSON. Exit status: 0 ok, 1 the window refused or the call failed, \
         2 bad arguments, 3 no window.\n\
         Nothing here approves, launches, widens, forks or adopts: space.draft posts a Plan Gate, \
         space.widen_propose a space widening and attempt.propose an attempt proposal, which the user \
         approves in the window.\n"
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_names_its_method_once() {
        assert_eq!(
            refusal_line(
                Method::AttemptReport,
                "attempt.report: the token does not belong to attempt 2 of s"
            ),
            "xctx-ctl: attempt.report: the token does not belong to attempt 2 of s"
        );
        assert_eq!(
            refusal_line(Method::AttemptReport, "space s is not active"),
            "xctx-ctl: attempt.report: space s is not active"
        );
        // Only the method's own name counts.
        assert_eq!(
            refusal_line(Method::AttemptReport, "attempt.reports: x"),
            "xctx-ctl: attempt.report: attempt.reports: x"
        );
    }
    use crate::StatusLevel;
    use crate::protocol::ReproStatus;
    use crate::protocol::{
        AgentParams, ChannelListParams, ChannelPostParams, ContractAgreeParams, ContractSetParams,
        GraphAddLaneParams, HelperKind, InboxListParams, LaneRole, LoopAdvanceParams, LoopAutonomy,
        LoopSetAutonomyParams, PaneParams, PaneSplitParams, SpaceDraftParams, SpaceParams,
        SpaceWidenProposeParams, StatusSetParams,
    };
    use xctx_layout::{PaneId, SplitAxis};

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    fn call(s: &str, space: Option<&str>) -> Result<Call, UsageError> {
        match parse_args(args(s), space, None)? {
            Command::Call { call, .. } => Ok(call),
            other => Err(UsageError(format!("not a call: {other:?}"))),
        }
    }

    #[test]
    fn no_args_and_help_print_help() -> Result<(), UsageError> {
        assert_eq!(parse_args(args(""), None, None)?, Command::Help);
        assert_eq!(parse_args(args("--help"), None, None)?, Command::Help);
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
    fn values_split_on_the_first_equals_sign() -> Result<(), UsageError> {
        assert_eq!(
            call("status.set message=ratio:=1", None)?,
            Call::StatusSet(StatusSetParams {
                message: "ratio:=1".into(),
                level: StatusLevel::default(),
                space: None,
            })
        );
        let with_space = vec!["space.draft".to_owned(), "task=set x:=y".to_owned()];
        let Command::Call { call: draft, .. } = parse_args(with_space, None, None)? else {
            return Err(UsageError("space.draft is not a call".into()));
        };
        assert_eq!(
            draft,
            Call::SpaceDraft(SpaceDraftParams {
                task: "set x:=y".into(),
                repos: None,
                rationale: None,
            })
        );
        assert_eq!(
            call(r#"space.draft task=t repos:=["a","b"]"#, None)?,
            Call::SpaceDraft(SpaceDraftParams {
                task: "t".into(),
                repos: Some(vec!["a".into(), "b".into()]),
                rationale: None,
            })
        );
        assert!(call("space.draft task=t repos:=[", None).is_err());
        assert!(call("space.draft :=1 task=t", None).is_err());
        Ok(())
    }

    #[test]
    fn options_are_read_before_the_method() -> Result<(), UsageError> {
        let parsed = parse_args(args("--socket /tmp/s.sock --compact ping"), None, None)?;
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
    fn m3_methods_parse_with_typed_keys_and_the_default_space() -> Result<(), UsageError> {
        let here = Some("here");
        let space = || SpaceParams {
            space: "here".into(),
        };
        let agent = || AgentParams {
            space: "here".into(),
            agent: "writer-api".into(),
        };
        assert_eq!(call("graph.get", here)?, Call::GraphGet(space()));
        assert_eq!(
            call("graph.pause agent=writer-api", here)?,
            Call::GraphPause(agent())
        );
        assert_eq!(
            call("graph.resume agent=writer-api", here)?,
            Call::GraphResume(agent())
        );
        assert_eq!(
            call(
                "graph.add_lane role=helper kind=tests repo=api label=unit",
                here
            )?,
            Call::GraphAddLane(GraphAddLaneParams {
                space: "here".into(),
                role: LaneRole::Helper,
                repo: Some("api".into()),
                kind: Some(HelperKind::Tests),
                label: Some("unit".into()),
            })
        );
        assert_eq!(
            call("graph.remove_lane agent=writer-api space=there", here)?,
            Call::GraphRemoveLane(AgentParams {
                space: "there".into(),
                agent: "writer-api".into(),
            })
        );
        assert_eq!(
            parse_args(
                [
                    "channel.post",
                    "from=writer-api",
                    "to=writer-web",
                    "kind=question",
                    "body=is amount in cents?",
                ]
                .map(str::to_owned),
                here,
                None,
            )?,
            Command::Call {
                socket: None,
                compact: false,
                call: Call::ChannelPost(ChannelPostParams {
                    space: "here".into(),
                    from: "writer-api".into(),
                    to: Some("writer-web".into()),
                    kind: "question".into(),
                    body: "is amount in cents?".into(),
                }),
            }
        );
        assert_eq!(
            call("channel.list kind=finding since_id=41 limit=20", here)?,
            Call::ChannelList(ChannelListParams {
                space: "here".into(),
                to: None,
                kind: Some("finding".into()),
                since_id: Some(41),
                limit: Some(20),
            })
        );
        assert_eq!(call("contract.get", here)?, Call::ContractGet(space()));
        assert_eq!(
            call(
                "contract.set author=orchestrator title=charge summary=payload spec=amount_cents:u64",
                here
            )?,
            Call::ContractSet(ContractSetParams {
                space: "here".into(),
                author: "orchestrator".into(),
                title: "charge".into(),
                summary: "payload".into(),
                spec: "amount_cents:u64".into(),
            })
        );
        assert_eq!(
            call("contract.agree agent=writer-api", here)?,
            Call::ContractAgree(ContractAgreeParams {
                space: "here".into(),
                agent: "writer-api".into(),
                version: None,
            })
        );
        assert_eq!(
            call("contract.agree agent=writer-api version=3", here)?,
            Call::ContractAgree(ContractAgreeParams {
                space: "here".into(),
                agent: "writer-api".into(),
                version: Some(3),
            })
        );
        assert_eq!(call("loop.status", here)?, Call::LoopStatus(space()));
        assert_eq!(
            call("loop.advance checks_passed=yes", here)?,
            Call::LoopAdvance(LoopAdvanceParams {
                space: "here".into(),
                checks_passed: Some(true),
            })
        );
        assert_eq!(
            call("loop.advance", here)?,
            Call::LoopAdvance(LoopAdvanceParams {
                space: "here".into(),
                checks_passed: None,
            })
        );
        assert_eq!(
            call(
                "loop.set_autonomy autonomy=within_budget max_iterations=3 max_turns=40",
                here
            )?,
            Call::LoopSetAutonomy(LoopSetAutonomyParams {
                space: "here".into(),
                autonomy: LoopAutonomy::WithinBudget,
                max_iterations: Some(3),
                max_turns: Some(40),
            })
        );
        assert_eq!(
            call(
                "space.widen_propose repos=web,api production=true rationale=consumer",
                here
            )?,
            Call::SpaceWidenPropose(SpaceWidenProposeParams {
                space: "here".into(),
                repos: Some(vec!["web".into(), "api".into()]),
                production: true,
                rationale: Some("consumer".into()),
            })
        );
        Ok(())
    }

    fn call_by(s: &str, agent: Option<&str>) -> Result<Call, UsageError> {
        match parse_args(args(s), Some("here"), agent)? {
            Command::Call { call, .. } => Ok(call),
            other => Err(UsageError(format!("not a call: {other:?}"))),
        }
    }

    #[test]
    fn attempt_report_takes_its_attempt_from_xctx_attempt_id() -> Result<(), UsageError> {
        let defaults = Defaults {
            space: Some("s".into()),
            agent: Some("writer-api".into()),
            attempt: Some("2".into()),
            attempt_token: Some("feed".into()),
        };
        let parsed = parse_args_with(
            args(
                "attempt.report summary=done tests_passed=4 tests_failed=0 repro=fixed gave_up=no",
            ),
            &defaults,
        )?;
        let Command::Call {
            call: Call::AttemptReport(p),
            ..
        } = parsed
        else {
            return Err(UsageError(format!("not a report: {parsed:?}")));
        };
        assert_eq!((p.space.as_str(), p.attempt), ("s", 2));
        assert_eq!(p.token.as_deref(), Some("feed"));
        assert_eq!(
            (p.tests_passed, p.repro, p.gave_up),
            (Some(4), ReproStatus::Fixed, false)
        );

        let given = parse_args_with(args("attempt.report attempt=3 summary=x"), &defaults)?;
        assert!(
            matches!(given, Command::Call { call: Call::AttemptReport(ref p), .. } if p.attempt == 3)
        );
        let own = parse_args_with(args("attempt.report summary=x token=mine"), &defaults)?;
        assert!(
            matches!(own, Command::Call { call: Call::AttemptReport(ref p), .. } if p.token.as_deref() == Some("mine"))
        );
        // Only attempt.report takes the default.
        assert!(parse_args_with(args("attempt.diff"), &defaults).is_err());
        let bad = Defaults {
            attempt: Some("two".into()),
            ..defaults
        };
        assert!(parse_args_with(args("attempt.report summary=x"), &bad).is_err());
        Ok(())
    }

    #[test]
    fn attempt_proposals_take_instructions_as_json() -> Result<(), UsageError> {
        let parsed = call(
            r#"attempt.propose agent=writer-api count=2 instructions:=["a,b","c"]"#,
            Some("s"),
        )?;
        let Call::AttemptPropose(p) = parsed else {
            return Err(UsageError(format!("not a proposal: {parsed:?}")));
        };
        assert_eq!(p.count, 2);
        assert_eq!(p.instructions, Some(vec!["a,b".to_owned(), "c".to_owned()]));
        Ok(())
    }

    #[test]
    fn the_calling_agent_comes_from_xctx_agent_id_unless_given() -> Result<(), UsageError> {
        let me = Some("writer-api");
        assert_eq!(
            call_by("channel.post kind=finding body=done", me)?,
            Call::ChannelPost(ChannelPostParams {
                space: "here".into(),
                from: "writer-api".into(),
                to: None,
                kind: "finding".into(),
                body: "done".into(),
            })
        );
        assert_eq!(
            call_by("contract.set title=t summary=s spec=x", me)?,
            Call::ContractSet(ContractSetParams {
                space: "here".into(),
                author: "writer-api".into(),
                title: "t".into(),
                summary: "s".into(),
                spec: "x".into(),
            })
        );
        assert_eq!(
            call_by("contract.agree version=2", me)?,
            Call::ContractAgree(ContractAgreeParams {
                space: "here".into(),
                agent: "writer-api".into(),
                version: Some(2),
            })
        );
        assert_eq!(
            call_by("contract.agree agent=writer-web", me)?,
            Call::ContractAgree(ContractAgreeParams {
                space: "here".into(),
                agent: "writer-web".into(),
                version: None,
            }),
            "an explicit agent wins"
        );
        assert!(
            call_by("graph.pause", me).is_err(),
            "the agent acted on is never defaulted"
        );
        assert!(call_by("contract.agree", None).is_err());
        assert!(
            call_by("contract.agree", Some("")).is_err(),
            "an empty XCTX_AGENT_ID counts as unset"
        );
        assert!(call_by("channel.post kind=finding body=done", Some("")).is_err());
        Ok(())
    }

    #[test]
    fn help_lists_every_m3_method() {
        let text = help();
        for m in Method::ALL {
            assert!(text.contains(m.name()), "{m}");
        }
        assert!(text.contains("space.widen_propose"));
        assert!(text.contains(AGENT_ENV));
    }

    #[test]
    fn m3_mistakes_are_usage_errors() {
        for bad in [
            "graph.pause space=s",
            "graph.add_lane space=s role=boss",
            "graph.add_lane space=s role=helper kind=lint",
            "channel.post space=s from=a body=hi",
            "channel.list space=s limit=many",
            "loop.advance space=s checks_passed=maybe",
            "loop.set_autonomy space=s autonomy=yolo",
            "loop.set_autonomy space=s autonomy=within_budget max_turns=-1",
            "space.widen_propose space=s production=sure",
            "contract.set space=s author=a title=t summary=s",
            "contract.agree space=s agent=a version=latest",
            "contract.agree space=s agent=a version=-1",
        ] {
            assert!(parse_args(args(bad), None, None).is_err(), "{bad}");
        }
        assert!(
            parse_args(args("graph.get"), None, None).is_err(),
            "without $XCTX_SPACE a space must be given"
        );
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
            assert!(parse_args(args(bad), None, None).is_err(), "{bad}");
        }
    }
}
