//! The dialogs omp waits on: one registry of pending requests, one answer path, and the tool-approval classifier.
//!
//! omp blocks a tool call on an `extension_ui_request`: an `ask` form, a `select`, a `confirm`, an `input` (maybe
//! secret), an `editor`, or a tool approval (a `select` titled `Allow tool: ...`). [`Dialogs`] keeps every such
//! request until it is settled, and settles it exactly once:
//!
//! - **Registry.** [`Dialogs::apply`] takes each frame omp sends. A blocking request becomes a [`Pending`], keyed by
//!   its request id; a `cancel` frame removes the request named by its `targetId`. Both the pane and the Inbox read
//!   [`Dialogs::pending`], so they always show the same set.
//! - **Timeout.** omp's `timeout` becomes a deadline, [`Pending::deadline`]. Nothing here owns a clock or a timer: the
//!   host passes `now` in, sleeps until [`Dialogs::next_deadline`] and then calls [`Dialogs::expire`]. omp applies the
//!   timeout on its side and tells us with `cancel`, so expiry only closes the dialog; it sends nothing. A timeout of
//!   zero means no deadline. An answer that arrives after the deadline is an error, never a frame.
//! - **One answer path.** [`Dialogs::answer`] is the only way to a frame for omp. It checks the answer against the
//!   request (a form goes through [`crate::form::Form::answer`], so required, min/max, minlength/maxlength, pattern,
//!   radio, checkbox and "Other" rules all apply; a choice must be one of the offered options) and only then builds the
//!   `extension_ui_response`. An invalid answer is a [`DialogError`] and the request stays pending so the user can
//!   fix it. Once answered, cancelled or timed out, a second answer for the same id is an error, so the pane and the
//!   Inbox can both offer an answer and only the first one becomes a frame.
//! - **Nothing is auto-approved.** No code path builds an approval the user did not give. Dismissing a tool
//!   approval ([`Answer::Cancel`]) is a deny, as recorded in `permission.jsonl`.
//!
//! # Tool approvals and danger
//!
//! An approval is parsed from the title (`Allow tool: bash\nCommand: echo hi`) into a [`Permission`]. It is flagged
//! with a [`Danger`] when one of these holds, so the Inbox can refuse one-click approval (an [`Answer`] from
//! [`Origin::Inbox`] that approves a dangerous request is [`DialogError::NeedsReview`]; denying is always allowed, and
//! the pane can still approve after the user has read the exact call):
//!
//! - `bash`: the command line cannot be read by the rules below (unbalanced quotes, `$(...)` or backticks inside
//!   double quotes, a program name built from a variable or glob, no `Command:` line); or any segment (the command
//!   split on `;`, `|`, `&`, parentheses, backticks, `$(` and newlines, after leading `VAR=x`, `env`, `command`,
//!   `nohup`, `nice`, `exec`, `time` and shell keywords) is
//!   - destructive: `rm rmdir unlink shred dd mkfs* truncate mv chmod chown chgrp kill killall pkill sudo doas su
//!     launchctl diskutil tee`, `sed -i`, `find -delete/-exec/-execdir/-ok`, `git` `reset clean rebase restore checkout
//!     filter-branch gc prune reflog update-ref`, `git branch -d/-D/--delete/-f`, `git tag -d/-f`, `git stash
//!     drop|clear`, `git worktree remove|prune|move`; or any output redirection other than into `/dev/null` or
//!     `>&N`;
//!   - network: `curl wget nc ncat netcat socat ssh scp sftp rsync ftp telnet mosh aria2c gh npx bunx uvx`, `git push
//!     clone fetch pull ls-remote submodule`, package-manager installs and publishes (`npm pnpm yarn bun pip pip3
//!     pipx uv brew gem go cargo mise apt apt-get` with `install add update publish ...`), or any word that starts
//!     with `http://`, `https://`, `ftp://` or `ssh://`;
//!   - opaque: `eval`, `xargs`, a shell with `-c`, or `python python3 node perl ruby php deno bun osascript` with
//!     `-c`, `-e`, `-p`, `--eval` or `--print`, because the code they run is not on the command line.
//! - `write`, `edit` and `ast_edit`: any path in the detail (`Path:`, `File:`, `file_path:` ...) resolves outside
//!   the agent's worktree, contains `..`, starts with `~`, goes through a symlink that leaves the worktree, or no
//!   path can be read. Symlinks are resolved on disk; any I/O error counts as outside.
//!
//! These rules are a conservative filter, not a sandbox: they err towards asking the user to look. A tool that is
//! not listed (`read`, `find`, `eval`, ...) is never flagged, and an unflagged call still shows its exact tool and
//! arguments. The `write`/`edit` detail format has not been recorded yet; an unrecognised one is flagged.
//!
//! # Plan review
//!
//! Not built. omp's plan review is not an `extension_ui_request` (`docs/omp-native.md`, Findings row 1) and plan
//! mode has not been recorded, so no UI or registry entry exists for it. It stays blocked on a recorded fixture and
//! the bridging extension described in `docs/omp-native.md`, Wave 3.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::command::Command;
use crate::form::{Answers, FieldError, Form};
use crate::frame::{Frame, UiMethod, UiRequest};

/// How many settled request ids are remembered, so a late second answer is told apart from an unknown id.
const SETTLED_KEPT: usize = 256;

/// A tool approval that cannot be approved with one click, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Danger {
    /// Which rule matched.
    pub class: DangerClass,
    /// What matched, in words for the user.
    pub reason: String,
}

/// The classes of danger (see the module docs for the exact rules).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DangerClass {
    /// A command that deletes, overwrites, rewrites history or kills.
    Destructive,
    /// A command that talks to the network.
    Network,
    /// A command whose effect cannot be read from its text.
    Opaque,
    /// A `write` or `edit` that leaves the agent's worktree, or whose path cannot be checked.
    OutsideWorktree,
}

/// The tool approval `request` asks, classified for danger against `worktree` the way [`Dialogs`] classifies it.
/// `None` when the request is not an `Allow tool:` select.
pub fn permission_of(request: &UiRequest, worktree: &Path) -> Option<Permission> {
    let a = request.approval()?;
    let command = command_of(&a.detail);
    let danger = classify(&a.tool, command.as_deref(), &a.detail, worktree);
    Some(Permission {
        tool: a.tool,
        command,
        detail: a.detail,
        options: a.options,
        danger,
    })
}

/// A tool approval parsed from an `Allow tool: ...` select.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Permission {
    /// The tool asking, such as `bash`.
    pub tool: String,
    /// The shell command of a `Command:` line (everything after it, so a multi-line command is whole).
    pub command: Option<String>,
    /// The text after the title's first line, such as `Command: echo hi`.
    pub detail: String,
    /// The answers omp offers: approve once, approve for the session, deny.
    pub options: Vec<String>,
    /// Set when the call must not be approved with one click.
    pub danger: Option<Danger>,
}

impl Permission {
    /// Whether the Inbox may offer a one-click approval for this call.
    pub fn one_click_allowed(&self) -> bool {
        self.danger.is_none()
    }

    /// Whether `option` is a refusal. Anything not recognisably a refusal counts as an approval.
    pub fn is_deny(option: &str) -> bool {
        let lower = option.trim().to_ascii_lowercase();
        lower.starts_with("deny") || lower.starts_with("reject")
    }

    /// The option that approves this one call only: the first that is neither a deny nor a session-wide approval.
    pub fn approve_once(&self) -> Option<&str> {
        self.options
            .iter()
            .map(String::as_str)
            .find(|o| !Self::is_deny(o) && !o.to_ascii_lowercase().contains("session"))
    }

    /// The option that refuses the call.
    pub fn deny(&self) -> Option<&str> {
        self.options
            .iter()
            .map(String::as_str)
            .find(|o| Self::is_deny(o))
    }
}

/// What a pending request asks.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// An `ask` form, parsed.
    Form(Form),
    /// Pick one of `options`.
    Select {
        /// The question.
        title: String,
        /// The labels to choose from.
        options: Vec<String>,
    },
    /// Yes or no.
    Confirm {
        /// The question.
        title: String,
        /// More detail.
        message: String,
    },
    /// One line of text.
    Input {
        /// The question.
        title: String,
        /// Greyed hint text.
        placeholder: String,
        /// Mask what is typed, and never echo it anywhere.
        secret: bool,
    },
    /// Several lines of text.
    Editor {
        /// The question.
        title: String,
        /// The text to start from.
        prefill: String,
    },
    /// A tool approval.
    Permission(Permission),
    /// An extension's multi-question dialog. Its answer shape is unrecorded, so it can only be dismissed.
    Ask {
        /// The questions, as omp sent them.
        questions: Value,
    },
}

/// A request omp waits on.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    /// The request id every answer carries.
    pub id: String,
    /// What it asks.
    pub kind: Kind,
    /// When omp gives up on it, if it has a timeout.
    pub deadline: Option<Instant>,
}

impl Pending {
    /// Time left at `now` (zero once the deadline has passed), for a countdown; `None` without a timeout.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        self.deadline.map(|d| d.saturating_duration_since(now))
    }
}

/// Who is answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The pane, where the user sees the whole request.
    Pane,
    /// The Inbox, which offers quick answers; it cannot approve a [`Danger`]-flagged tool call.
    Inbox,
}

/// What the user answered.
#[derive(Clone, PartialEq)]
pub enum Answer {
    /// The values of a form.
    Form(Answers),
    /// The label of one offered option (a `select`, or a tool approval's approve/deny).
    Choice(String),
    /// Yes or no for a `confirm`.
    Confirm(bool),
    /// The text of an `input` or `editor`.
    Text(String),
    /// Dismiss the request. For a tool approval this is a deny.
    Cancel,
}

impl fmt::Debug for Answer {
    /// Prints the variant only: a text answer may be a secret and a form may carry one.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Answer::Form(_) => "Answer::Form(..)",
            Answer::Choice(_) => "Answer::Choice(..)",
            Answer::Confirm(_) => "Answer::Confirm(..)",
            Answer::Text(_) => "Answer::Text(..)",
            Answer::Cancel => "Answer::Cancel",
        })
    }
}

/// Why an answer did not become a frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DialogError {
    /// No request has this id (it never arrived, or the session was replaced).
    #[error("no pending request {0}")]
    Unknown(String),
    /// The request was already answered.
    #[error("request {0} was already answered")]
    AlreadyAnswered(String),
    /// The request timed out before it was answered.
    #[error("request {0} timed out")]
    TimedOut(String),
    /// omp cancelled the request.
    #[error("request {0} was cancelled")]
    Cancelled(String),
    /// A form answer broke one or more field rules.
    #[error("the form answer is not valid: {}", fields(.0))]
    Invalid(Vec<FieldError>),
    /// The chosen label is not one of the options.
    #[error("\"{0}\" is not one of the options")]
    InvalidChoice(String),
    /// This kind of request cannot be answered like that.
    #[error("this request takes {expected}")]
    WrongAnswer {
        /// What it takes.
        expected: &'static str,
    },
    /// The Inbox tried to approve a dangerous tool call; it has to be approved in the pane.
    #[error("{tool} needs review in the pane: {reason}")]
    NeedsReview {
        /// The tool.
        tool: String,
        /// Why one click is not enough.
        reason: String,
    },
}

fn fields(errors: &[FieldError]) -> String {
    errors
        .iter()
        .map(|e| format!("{}: {}", e.name, e.message))
        .collect::<Vec<_>>()
        .join("; ")
}

/// A change [`Dialogs::apply`] made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A request now waits for the user.
    Raised(String),
    /// omp cancelled a request.
    Cancelled(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settled {
    Answered,
    TimedOut,
    Cancelled,
}

/// The pending requests of one omp session.
#[derive(Debug, Clone)]
pub struct Dialogs {
    worktree: PathBuf,
    pending: Vec<Pending>,
    settled: VecDeque<(String, Settled)>,
}

impl Dialogs {
    /// A registry for the agent working in `worktree` (what `write` and `edit` approvals are measured against).
    pub fn new(worktree: &Path) -> Dialogs {
        Dialogs {
            worktree: std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf()),
            pending: Vec::new(),
            settled: VecDeque::new(),
        }
    }

    /// Take one frame from omp. Only `extension_ui_request` frames matter: a blocking one is registered (at `now`,
    /// which starts its timeout), a `cancel` removes its target, anything else is ignored.
    pub fn apply(&mut self, frame: &Frame, now: Instant) -> Option<Change> {
        let Frame::Ui(request) = frame else {
            return None;
        };
        if let UiMethod::Cancel { target_id } = &request.method {
            let at = self.pending.iter().position(|p| &p.id == target_id)?;
            let gone = self.pending.remove(at);
            self.settle(gone.id.clone(), Settled::Cancelled);
            return Some(Change::Cancelled(gone.id));
        }
        if !request.blocks() || request.id.is_empty() || self.knows(&request.id) {
            return None;
        }
        let kind = self.kind_of(request)?;
        let deadline = request
            .timeout_ms
            .filter(|ms| *ms > 0)
            .and_then(|ms| now.checked_add(Duration::from_millis(ms)));
        self.pending.push(Pending {
            id: request.id.clone(),
            kind,
            deadline,
        });
        Some(Change::Raised(request.id.clone()))
    }

    /// Everything waiting on the user, oldest first.
    pub fn pending(&self) -> &[Pending] {
        &self.pending
    }

    /// The pending request `id`.
    pub fn get(&self, id: &str) -> Option<&Pending> {
        self.pending.iter().find(|p| p.id == id)
    }

    /// The earliest deadline, for the host to sleep until.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.iter().filter_map(|p| p.deadline).min()
    }

    /// Close every request whose deadline is at or before `now`; returns their ids. Sends nothing: omp owns the
    /// timeout and reports it with `cancel`.
    pub fn expire(&mut self, now: Instant) -> Vec<String> {
        let (expired, alive): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|p| p.deadline.is_some_and(|d| d <= now));
        self.pending = alive;
        expired
            .into_iter()
            .map(|p| {
                self.settle(p.id.clone(), Settled::TimedOut);
                p.id
            })
            .collect()
    }

    /// Forget every request, because the session was replaced (`new_session`, `open_session`, `fork`). Answers for
    /// the old ids are then [`DialogError::Unknown`].
    pub fn clear(&mut self) {
        self.pending.clear();
        self.settled.clear();
    }

    /// Answer request `id`. Returns the one frame to send to omp, and settles the request; or an error and sends
    /// nothing. An answer that fails validation leaves the request pending.
    pub fn answer(
        &mut self,
        id: &str,
        answer: Answer,
        origin: Origin,
        now: Instant,
    ) -> Result<Command, DialogError> {
        let Some(at) = self.pending.iter().position(|p| p.id == id) else {
            return Err(self.missing(id));
        };
        if self.pending[at].deadline.is_some_and(|d| d <= now) {
            let gone = self.pending.remove(at);
            self.settle(gone.id, Settled::TimedOut);
            return Err(DialogError::TimedOut(id.to_owned()));
        }
        let command = build(id, &self.pending[at].kind, answer, origin)?;
        let gone = self.pending.remove(at);
        self.settle(gone.id, Settled::Answered);
        Ok(command)
    }

    fn knows(&self, id: &str) -> bool {
        self.pending.iter().any(|p| p.id == id) || self.settled.iter().any(|(s, _)| s == id)
    }

    fn missing(&self, id: &str) -> DialogError {
        match self.settled.iter().find(|(s, _)| s == id) {
            Some((_, Settled::Answered)) => DialogError::AlreadyAnswered(id.to_owned()),
            Some((_, Settled::TimedOut)) => DialogError::TimedOut(id.to_owned()),
            Some((_, Settled::Cancelled)) => DialogError::Cancelled(id.to_owned()),
            None => DialogError::Unknown(id.to_owned()),
        }
    }

    fn settle(&mut self, id: String, how: Settled) {
        if self.settled.len() >= SETTLED_KEPT {
            self.settled.pop_front();
        }
        self.settled.push_back((id, how));
    }

    fn kind_of(&self, request: &UiRequest) -> Option<Kind> {
        Some(match &request.method {
            UiMethod::Select { title, options } => match permission_of(request, &self.worktree) {
                Some(permission) => Kind::Permission(permission),
                None => Kind::Select {
                    title: title.clone(),
                    options: options.clone(),
                },
            },
            UiMethod::Confirm { title, message } => Kind::Confirm {
                title: title.clone(),
                message: message.clone(),
            },
            UiMethod::Input {
                title,
                placeholder,
                secret,
            } => Kind::Input {
                title: title.clone(),
                placeholder: placeholder.clone(),
                secret: *secret,
            },
            UiMethod::Editor { title, prefill } => Kind::Editor {
                title: title.clone(),
                prefill: prefill.clone(),
            },
            UiMethod::Form { html } => Kind::Form(Form::parse(html)),
            UiMethod::Ask { questions } => Kind::Ask {
                questions: questions.clone(),
            },
            _ => return None,
        })
    }
}

/// Check `answer` against `kind` and build the frame; nothing is built from an answer that fails.
fn build(id: &str, kind: &Kind, answer: Answer, origin: Origin) -> Result<Command, DialogError> {
    if matches!(answer, Answer::Cancel) {
        return Ok(Command::ui_cancelled(id));
    }
    match (kind, answer) {
        (Kind::Form(form), Answer::Form(answers)) => form
            .answer(&answers)
            .map(|data| Command::ui_form(id, data))
            .map_err(DialogError::Invalid),
        (Kind::Form(_), _) => Err(wrong("the form's values")),
        (Kind::Select { options, .. }, Answer::Choice(choice)) => {
            if options.contains(&choice) {
                Ok(Command::ui_value(id, &choice))
            } else {
                Err(DialogError::InvalidChoice(choice))
            }
        }
        (Kind::Select { .. }, _) => Err(wrong("one of the options")),
        (Kind::Permission(permission), Answer::Choice(choice)) => {
            if !permission.options.contains(&choice) {
                return Err(DialogError::InvalidChoice(choice));
            }
            if let (Origin::Inbox, Some(danger)) = (origin, &permission.danger)
                && !Permission::is_deny(&choice)
            {
                return Err(DialogError::NeedsReview {
                    tool: permission.tool.clone(),
                    reason: danger.reason.clone(),
                });
            }
            Ok(Command::ui_value(id, &choice))
        }
        (Kind::Permission(_), _) => Err(wrong("one of the options")),
        (Kind::Confirm { .. }, Answer::Confirm(yes)) => Ok(Command::ui_confirmed(id, yes)),
        (Kind::Confirm { .. }, _) => Err(wrong("yes or no")),
        (Kind::Input { .. } | Kind::Editor { .. }, Answer::Text(text)) => {
            Ok(Command::ui_value(id, &text))
        }
        (Kind::Input { .. } | Kind::Editor { .. }, _) => Err(wrong("text")),
        (Kind::Ask { .. }, _) => Err(wrong("only dismissal")),
    }
}

fn wrong(expected: &'static str) -> DialogError {
    DialogError::WrongAnswer { expected }
}

// ---- classifier ---------------------------------------------------------------------------------------------

/// The shell command of a `Command:` line and everything after it.
fn command_of(detail: &str) -> Option<String> {
    let mut offset = 0;
    for line in detail.split_inclusive('\n') {
        if let Some(rest) = line.trim_start().strip_prefix("Command:") {
            let tail = &detail[offset + line.len()..];
            let command = format!("{}{tail}", rest.trim_start());
            return Some(command.trim_end().to_owned()).filter(|c| !c.is_empty());
        }
        offset += line.len();
    }
    None
}

fn danger(class: DangerClass, reason: impl Into<String>) -> Option<Danger> {
    Some(Danger {
        class,
        reason: reason.into(),
    })
}

fn classify(tool: &str, command: Option<&str>, detail: &str, worktree: &Path) -> Option<Danger> {
    match tool.to_ascii_lowercase().as_str() {
        "bash" => match command {
            Some(command) => classify_command(command),
            None => danger(DangerClass::Opaque, "there is no command to inspect"),
        },
        "write" | "edit" | "ast_edit" => classify_paths(detail, worktree),
        _ => None,
    }
}

const PATH_KEYS: [&str; 5] = ["path", "file", "file_path", "filepath", "target"];

fn classify_paths(detail: &str, worktree: &Path) -> Option<Danger> {
    let mut seen = false;
    for line in detail.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if !PATH_KEYS.contains(&key.trim().to_ascii_lowercase().as_str()) {
            continue;
        }
        seen = true;
        let value = value.trim();
        if !inside(worktree, value) {
            return danger(
                DangerClass::OutsideWorktree,
                format!("{value} is not provably inside the worktree"),
            );
        }
    }
    if seen {
        None
    } else {
        danger(
            DangerClass::OutsideWorktree,
            "no path to check against the worktree",
        )
    }
}

/// Whether `raw` names a place inside `root` (canonical). Symlinks are followed on disk; `..`, `~` and every I/O
/// error count as outside.
fn inside(root: &Path, raw: &str) -> bool {
    let raw = raw.trim().trim_matches(['"', '\'', '`']);
    if raw.is_empty() || raw.starts_with('~') {
        return false;
    }
    let path = Path::new(raw);
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    if full.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    let mut base = full.as_path();
    let mut rest: Vec<&OsStr> = Vec::new();
    loop {
        match std::fs::canonicalize(base) {
            Ok(mut real) => {
                real.extend(rest.iter().rev());
                return real.starts_with(root);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // A dangling symlink exists but does not resolve: writing through it lands wherever it points.
                if std::fs::symlink_metadata(base).is_ok() {
                    return false;
                }
                match (base.parent(), base.file_name()) {
                    (Some(parent), Some(name)) => {
                        rest.push(name);
                        base = parent;
                    }
                    _ => return false,
                }
            }
            Err(_) => return false,
        }
    }
}

const DESTRUCTIVE: [&str; 22] = [
    "rm",
    "rmdir",
    "unlink",
    "shred",
    "dd",
    "truncate",
    "mv",
    "chmod",
    "chown",
    "chgrp",
    "kill",
    "killall",
    "pkill",
    "sudo",
    "doas",
    "su",
    "launchctl",
    "diskutil",
    "tee",
    "srm",
    "mkfs",
    "fdisk",
];

const NETWORK: [&str; 15] = [
    "curl", "wget", "nc", "ncat", "netcat", "socat", "ssh", "scp", "sftp", "rsync", "ftp",
    "telnet", "mosh", "aria2c", "gh",
];

/// Download-and-run launchers.
const LAUNCHERS: [&str; 3] = ["npx", "bunx", "uvx"];

const SHELLS: [&str; 6] = ["sh", "bash", "zsh", "dash", "ksh", "fish"];

const INTERPRETERS: [&str; 9] = [
    "python",
    "python3",
    "node",
    "perl",
    "ruby",
    "php",
    "deno",
    "bun",
    "osascript",
];

/// Programs that run another program given on their command line.
const WRAPPERS: [&str; 6] = ["env", "command", "builtin", "nohup", "nice", "exec"];

/// Shell keywords that precede a command.
const KEYWORDS: [&str; 14] = [
    "!", "{", "}", "if", "then", "else", "elif", "do", "while", "until", "time", "for", "in",
    "function",
];

const PACKAGE: [(&str, &[&str]); 14] = [
    (
        "npm",
        &[
            "install",
            "i",
            "add",
            "ci",
            "update",
            "publish",
            "uninstall",
            "exec",
        ],
    ),
    (
        "pnpm",
        &[
            "install", "i", "add", "update", "up", "publish", "dlx", "remove",
        ],
    ),
    (
        "yarn",
        &[
            "install", "add", "up", "upgrade", "publish", "dlx", "remove",
        ],
    ),
    ("bun", &["install", "i", "add", "update", "publish", "x"]),
    ("pip", &["install", "download", "uninstall"]),
    ("pip3", &["install", "download", "uninstall"]),
    ("pipx", &["install", "run", "upgrade"]),
    ("uv", &["add", "pip", "sync", "tool", "run"]),
    (
        "brew",
        &[
            "install",
            "upgrade",
            "update",
            "uninstall",
            "reinstall",
            "tap",
        ],
    ),
    ("gem", &["install", "update", "push"]),
    ("go", &["get", "install"]),
    (
        "cargo",
        &[
            "install",
            "publish",
            "add",
            "update",
            "fetch",
            "yank",
            "uninstall",
        ],
    ),
    ("mise", &["install", "use", "upgrade"]),
    ("apt", &["install", "update", "upgrade", "remove"]),
];

/// `apt-get` has the same verbs as `apt`.
const APT_GET: [&str; 4] = ["install", "update", "upgrade", "remove"];

const URL_SCHEMES: [&str; 4] = ["http://", "https://", "ftp://", "ssh://"];

/// A command line split into the words of each simple command.
struct Parsed {
    segments: Vec<Vec<String>>,
    /// Some output goes to a file (a `>` that is not into `/dev/null` or a `>&N` duplication).
    redirects: bool,
}

struct Scanner {
    segments: Vec<Vec<String>>,
    segment: Vec<String>,
    word: String,
    in_word: bool,
    redirects: bool,
}

impl Scanner {
    fn end_word(&mut self) {
        if self.in_word {
            self.segment.push(std::mem::take(&mut self.word));
            self.in_word = false;
        }
    }

    fn end_segment(&mut self) {
        self.end_word();
        if !self.segment.is_empty() {
            self.segments.push(std::mem::take(&mut self.segment));
        }
    }

    fn push(&mut self, c: char) {
        self.word.push(c);
        self.in_word = true;
    }
}

/// Split `cmd` the way a POSIX shell would for the purpose of finding programs; `None` when it cannot be followed
/// (an unterminated quote, or a command substitution inside double quotes).
fn parse_shell(cmd: &str) -> Option<Parsed> {
    let chars: Vec<char> = cmd.chars().collect();
    let mut s = Scanner {
        segments: Vec::new(),
        segment: Vec::new(),
        word: String::new(),
        in_word: false,
        redirects: false,
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' => {
                s.in_word = true;
                i += 1;
                loop {
                    match chars.get(i)? {
                        '\'' => break,
                        &ch => s.word.push(ch),
                    }
                    i += 1;
                }
                i += 1;
            }
            '"' => {
                s.in_word = true;
                i += 1;
                loop {
                    match chars.get(i)? {
                        '"' => break,
                        '`' => return None,
                        '$' if chars.get(i + 1) == Some(&'(') => return None,
                        '\\' => {
                            i += 1;
                            s.word.push(*chars.get(i)?);
                        }
                        &ch => s.word.push(ch),
                    }
                    i += 1;
                }
                i += 1;
            }
            '\\' => {
                match chars.get(i + 1) {
                    Some('\n') => {}
                    Some(&ch) => s.push(ch),
                    None => {}
                }
                i += 2;
            }
            '\n' | ';' | '|' | '&' | '(' | ')' | '`' => {
                s.end_segment();
                i += 1;
            }
            '$' if chars.get(i + 1) == Some(&'(') => {
                s.end_segment();
                i += 2;
            }
            '<' => {
                s.end_word();
                i += 1;
            }
            '>' => {
                s.end_word();
                let mut j = i + 1;
                if chars.get(j) == Some(&'>') {
                    j += 1;
                }
                if chars.get(j) == Some(&'&') {
                    j += 1;
                    if !chars
                        .get(j)
                        .is_some_and(|c| c.is_ascii_digit() || *c == '-')
                    {
                        s.redirects = true;
                    }
                } else {
                    while chars.get(j).is_some_and(|c| *c == ' ' || *c == '\t') {
                        j += 1;
                    }
                    let target: String = chars[j..].iter().take(9).collect();
                    let after = chars.get(j + 9);
                    let to_null = target == "/dev/null"
                        && after.is_none_or(|c| c.is_whitespace() || ";|&()".contains(*c));
                    if !to_null {
                        s.redirects = true;
                    }
                }
                i = j;
            }
            c if c.is_whitespace() => {
                s.end_word();
                i += 1;
            }
            c => {
                s.push(c);
                i += 1;
            }
        }
    }
    s.end_segment();
    Some(Parsed {
        segments: s.segments,
        redirects: s.redirects,
    })
}

fn classify_command(cmd: &str) -> Option<Danger> {
    let Some(parsed) = parse_shell(cmd) else {
        return danger(
            DangerClass::Opaque,
            "the command uses quoting or substitution that cannot be followed",
        );
    };
    if parsed.redirects {
        return danger(
            DangerClass::Destructive,
            "the command writes output into a file",
        );
    }
    parsed.segments.iter().find_map(|seg| classify_segment(seg))
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !name.starts_with(|c: char| c.is_ascii_digit())
    })
}

fn classify_segment(words: &[String]) -> Option<Danger> {
    if let Some(url) = words
        .iter()
        .find(|w| URL_SCHEMES.iter().any(|s| w.starts_with(s)))
    {
        return danger(DangerClass::Network, format!("the command reaches {url}"));
    }
    let mut i = 0;
    while i < words.len() && (is_assignment(&words[i]) || KEYWORDS.contains(&words[i].as_str())) {
        i += 1;
    }
    let head = words.get(i)?;
    let name = head.rsplit('/').next().unwrap_or(head).to_ascii_lowercase();
    if head.contains(['$', '*', '?', '[', '~']) {
        return danger(
            DangerClass::Opaque,
            format!("the program `{head}` is built from a variable or pattern"),
        );
    }
    let args = &words[i + 1..];
    if WRAPPERS.contains(&name.as_str()) {
        // A wrapper's own options may take a value (`env -u NAME prog`), so every later word is a candidate.
        return (i + 1..words.len()).find_map(|k| classify_segment(&words[k..]));
    }
    classify_program(&name, args)
}

fn classify_program(name: &str, args: &[String]) -> Option<Danger> {
    if DESTRUCTIVE.contains(&name) || name.starts_with("mkfs") {
        return danger(
            DangerClass::Destructive,
            format!("`{name}` can delete, overwrite or stop things"),
        );
    }
    if NETWORK.contains(&name) || LAUNCHERS.contains(&name) {
        return danger(
            DangerClass::Network,
            format!("`{name}` reaches the network"),
        );
    }
    let has = |flags: &[&str]| args.iter().any(|a| flags.contains(&a.as_str()));
    if name == "eval" || name == "xargs" {
        return danger(
            DangerClass::Opaque,
            format!("`{name}` runs a command that is not on the command line"),
        );
    }
    if SHELLS.contains(&name)
        && args
            .iter()
            .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('c'))
    {
        return danger(
            DangerClass::Opaque,
            format!("`{name} -c` runs a script that is not on the command line"),
        );
    }
    if INTERPRETERS.contains(&name) && has(&["-c", "-e", "-p", "--eval", "--print"]) {
        return danger(
            DangerClass::Opaque,
            format!("`{name}` runs code given inline"),
        );
    }
    if name == "sed"
        && args
            .iter()
            .any(|a| a.starts_with("-i") || a == "--in-place")
    {
        return danger(DangerClass::Destructive, "`sed -i` edits files in place");
    }
    if name == "find" && has(&["-delete", "-exec", "-execdir", "-ok"]) {
        return danger(
            DangerClass::Destructive,
            "`find` deletes or runs commands on what it finds",
        );
    }
    if name == "git" {
        return classify_git(args);
    }
    if name == "apt-get" {
        return package(name, args, &APT_GET);
    }
    if let Some((_, verbs)) = PACKAGE.iter().find(|(p, _)| *p == name) {
        if name == "yarn" && args.iter().all(|a| a.starts_with('-')) {
            return danger(DangerClass::Network, "`yarn` alone installs dependencies");
        }
        return package(name, args, verbs);
    }
    None
}

fn package(name: &str, args: &[String], verbs: &[&str]) -> Option<Danger> {
    let verb = args.iter().find(|a| !a.starts_with('-'))?;
    verbs.contains(&verb.as_str()).then(|| Danger {
        class: DangerClass::Network,
        reason: format!("`{name} {verb}` downloads or publishes packages"),
    })
}

fn classify_git(args: &[String]) -> Option<Danger> {
    let mut it = args.iter();
    let mut sub = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" => {
                it.next();
            }
            _ if a.starts_with('-') => {}
            _ => {
                sub = Some(a.as_str());
                break;
            }
        }
    }
    let rest: Vec<&str> = it.map(String::as_str).collect();
    let any = |flags: &[&str]| rest.iter().any(|a| flags.contains(a));
    let sub = sub?;
    match sub {
        "push" | "clone" | "fetch" | "pull" | "ls-remote" | "submodule" => danger(
            DangerClass::Network,
            format!("`git {sub}` reaches the network"),
        ),
        "reset" | "clean" | "rebase" | "restore" | "checkout" | "filter-branch" | "gc"
        | "prune" | "reflog" | "update-ref" => danger(
            DangerClass::Destructive,
            format!("`git {sub}` can discard work or history"),
        ),
        "branch" if any(&["-d", "-D", "--delete", "-f", "--force"]) => danger(
            DangerClass::Destructive,
            "`git branch` deletes or moves a branch",
        ),
        "tag" if any(&["-d", "--delete", "-f", "--force"]) => {
            danger(DangerClass::Destructive, "`git tag` deletes or moves a tag")
        }
        "stash" if rest.first().is_some_and(|a| ["drop", "clear"].contains(a)) => {
            danger(DangerClass::Destructive, "`git stash` drops saved work")
        }
        "worktree"
            if rest
                .first()
                .is_some_and(|a| ["remove", "prune", "move"].contains(a)) =>
        {
            danger(
                DangerClass::Destructive,
                "`git worktree` removes a worktree",
            )
        }
        _ => None,
    }
}
