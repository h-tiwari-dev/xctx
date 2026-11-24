//! Which `pi` session an agent pane runs, and the `pi` arguments that start it.
//!
//! Every agent bound to a pane runs a session xctx names itself (`xctx_core::pi_session`), recorded on its graph
//! node (`AgentNode::session`) once its pane has started under it:
//! - an agent with a recorded id opens it: `pi --session-id <id>`;
//! - a new agent (a launch, a new lane) gets `xctx-<slug>-<agent>` and records it;
//! - an agent from before Milestone 4 that is being resumed has no id: the newest session pi keeps for its
//!   worktree is forked into the named one (`pi --fork <file> --session-id <id>`), so it keeps its conversation and
//!   gets an id from then on. When no such session is found, it starts the named session as a new one and records it,
//!   so it can be forked;
//! - an attempt forks its writer's session into its own worktree (`pi --fork <source> --session-id <attempt>`)
//!   the first time it starts, and opens its own session afterwards;
//! - after an adoption, the writer's id becomes `xctx-<slug>-<agent>-adopted-<n>`, forked from the winning
//!   attempt's session the first time it starts in the space's worktree.
//!
//! Whether a session has started is read from pi's session folder: the folder pi keeps for a working directory is
//! `--<path, without its leading slash, with / \ and : as ->--` under `~/.pi/agent/sessions` (or every session
//! sits directly in `$PI_CODING_AGENT_SESSION_DIR` when that is set). Everything here takes that folder as an
//! argument, so tests never read the user's own.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use xctx_attempt::{Attempt, AttemptSet, AttemptState};
use xctx_core::pi_session::{self, read_header, session_id};

use crate::spawn::AgentStart;

/// The most session files of one folder whose header is read when looking for a worktree's newest session.
pub const MAX_HEADERS_READ: usize = 64;

/// Where pi keeps its sessions, as this process sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionStorage {
    /// The session folder: `$PI_CODING_AGENT_SESSION_DIR` or `~/.pi/agent/sessions`.
    pub root: PathBuf,
    /// Whether every session sits directly in `root` (a session folder was set), rather than in one folder per
    /// working directory.
    pub flat: bool,
}

impl SessionStorage {
    /// Storage from explicit values of `PI_CODING_AGENT_SESSION_DIR` and `HOME`; `None` when neither is set.
    pub fn from_values(session_dir: Option<OsString>, home: Option<OsString>) -> Option<Self> {
        let flat = session_dir.as_ref().is_some_and(|d| !d.is_empty());
        pi_session::sessions_root(session_dir, home).map(|root| SessionStorage { root, flat })
    }

    /// Storage as this process's environment says.
    pub fn from_env() -> Option<Self> {
        SessionStorage::from_values(
            std::env::var_os(pi_session::SESSION_DIR_ENV),
            std::env::var_os("HOME"),
        )
    }

    /// The folder pi keeps the sessions of working directory `cwd` in.
    pub fn folder_for(&self, cwd: &Path) -> PathBuf {
        if self.flat {
            self.root.clone()
        } else {
            self.root.join(project_folder_name(&resolved(cwd)))
        }
    }

    /// The session files in `cwd`'s folder, newest first (pi names them `<timestamp>_<id>.jsonl`).
    fn files_for(&self, cwd: &Path) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(self.folder_for(cwd)) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl") && p.is_file())
            .collect();
        files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        files
    }

    /// Whether session `id` of working directory `cwd` has been written yet.
    pub fn has_session(&self, cwd: &Path, id: &str) -> bool {
        let suffix = format!("_{id}.jsonl");
        self.files_for(cwd).iter().any(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix))
        })
    }

    /// The newest unnamed session file whose header says it works in `cwd`, among the newest
    /// [`MAX_HEADERS_READ`] files of its folder: the session an agent from before Milestone 4 left there. Sessions
    /// xctx named (`xctx-*`) are skipped: they belong to other agents of the space (helpers share the writer's
    /// worktree), never to an agent that has no id yet.
    pub fn newest_session_in(&self, cwd: &Path) -> Option<PathBuf> {
        let cwd = resolved(cwd);
        self.files_for(&cwd)
            .into_iter()
            .filter(|file| {
                !file
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.split_once('_'))
                    .is_some_and(|(_, id)| id.starts_with(XCTX_SESSION_PREFIX))
            })
            .take(MAX_HEADERS_READ)
            .find(|file| {
                read_header(file).ok().is_some_and(|h| {
                    !h.id.starts_with(XCTX_SESSION_PREFIX)
                        && h.cwd.is_some_and(|c| c == cwd || resolved(&c) == cwd)
                })
            })
    }

    /// The newest file of session `id`, in any working directory's folder; `None` when pi has not written it.
    pub fn find(&self, id: &str) -> Option<PathBuf> {
        pi_session::find_session(&self.root, id)
    }
}

/// What every session id xctx chooses starts with (see [`pi_session::session_id`]).
pub const XCTX_SESSION_PREFIX: &str = "xctx-";

/// What to give `pi --fork` for session `id`: its file when it is known and UTF-8 (exact, as
/// [`pi_session::fork_args_path`] passes it, where an id could match another session by prefix), else the id.
pub fn fork_source_arg(id: &str, file: Option<&Path>) -> String {
    file.and_then(Path::to_str)
        .map_or_else(|| id.to_owned(), str::to_owned)
}

/// The adopted attempt of writer `agent` in space `slug` whose adoption gave the writer the session `recorded`
/// (see [`adopted_session_id`]), if `recorded` is such a session.
pub fn adopted_winner<'a>(
    set: &'a AttemptSet,
    slug: &str,
    agent: &str,
    recorded: &str,
) -> Option<&'a Attempt> {
    set.attempts.iter().find(|a| {
        a.state == AttemptState::Adopted
            && a.agent == agent
            && adopted_session_id(slug, &a.agent, a.number()) == recorded
    })
}

/// Whether `recorded` is a session a writer `agent` of space `slug` gets after an adoption (see
/// [`adopted_session_id`]), whichever attempt it was.
pub fn is_adopted_session(slug: &str, agent: &str, recorded: &str) -> bool {
    let prefix = format!("{}-", session_id(&[slug, agent, "adopted"]));
    recorded.starts_with(&prefix)
}

/// The session a fork of writer `agent` copies. `recorded` is the session on its graph node, `winner` the session
/// of the attempt it adopted when `recorded` is that adoption's id, and `on_disk` says whether pi has written a
/// session. Refused when the writer has no recorded session (its pane has not started under one: start it first),
/// or when neither its session nor the winner's is on disk yet.
pub fn fork_source(
    agent: &str,
    recorded: Option<&str>,
    winner: Option<&str>,
    on_disk: impl Fn(&str) -> bool,
) -> Result<String, String> {
    let Some(recorded) = recorded else {
        return Err(format!(
            "{agent} has no recorded pi session to fork; start the writer's agent pane first"
        ));
    };
    if on_disk(recorded) {
        return Ok(recorded.to_owned());
    }
    if let Some(winner) = winner.filter(|w| on_disk(w)) {
        return Ok(winner.to_owned());
    }
    Err(format!(
        "{agent}'s pi session {recorded} has not been written yet; start the writer's agent pane and give it a \
         message first, then fork"
    ))
}

/// `path` with symbolic links resolved (pi resolves `/tmp` to `/private/tmp`), or as given when it cannot be.
fn resolved(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The name of the folder pi keeps the sessions of working directory `cwd` in: `--<cwd without its leading slash,
/// with /, \ and : as ->--`.
pub fn project_folder_name(cwd: &Path) -> String {
    let text = cwd.to_string_lossy();
    let trimmed = text
        .strip_prefix('/')
        .or_else(|| text.strip_prefix('\\'))
        .unwrap_or(&text);
    let safe: String = trimmed
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    format!("--{safe}--")
}

/// The session the writer `agent` of space `slug` continues in after attempt `number` was adopted.
pub fn adopted_session_id(slug: &str, agent: &str, number: u32) -> String {
    session_id(&[slug, agent, "adopted", &number.to_string()])
}

/// How an agent's `pi` session starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentSession {
    /// No xctx id: what [`AgentStart`] alone says (`--continue`, a task, or nothing). For panes that run no agent
    /// of the graph.
    Unnamed,
    /// Open session `id`, or create it: `pi --session-id <id>`.
    Open(String),
    /// Copy session `source` (an id or a session file) into session `id` here: `pi --fork <source> --session-id
    /// <id>`.
    Fork {
        /// The session copied.
        source: String,
        /// The new session's id.
        id: String,
    },
}

impl AgentSession {
    /// The id to record on the agent's graph node once its pane started this way.
    pub fn named_id(&self) -> Option<&str> {
        match self {
            AgentSession::Unnamed => None,
            AgentSession::Open(id) | AgentSession::Fork { id, .. } => Some(id),
        }
    }
}

/// The `pi` arguments for an agent pane started as `start` in `session`. A task (or an attempt's instruction)
/// follows `--` as the first message.
pub fn agent_args(start: &AgentStart, session: &AgentSession) -> Vec<String> {
    let mut args = match session {
        AgentSession::Unnamed => match start {
            AgentStart::Continue => vec!["--continue".to_owned()],
            AgentStart::Task(_) | AgentStart::Fresh => Vec::new(),
        },
        AgentSession::Open(id) => pi_session::open_args(id),
        AgentSession::Fork { source, id } => pi_session::fork_args(source, id),
    };
    if let AgentStart::Task(task) = start {
        args.push("--".to_owned());
        args.push(task.clone());
    }
    args
}

/// The session of an agent pane bound to agent `agent` of space `slug`, whose node records `recorded`, started as
/// `start` (see the module documentation). `legacy` finds the newest session pi keeps for the pane's worktree; it
/// is only asked for an agent with no id that is being resumed.
pub fn pane_session(
    slug: &str,
    agent: &str,
    recorded: Option<&str>,
    start: &AgentStart,
    legacy: impl FnOnce() -> Option<PathBuf>,
) -> AgentSession {
    if let Some(id) = recorded {
        return AgentSession::Open(id.to_owned());
    }
    let id = pi_session::agent_session_id(slug, agent);
    match start {
        AgentStart::Task(_) | AgentStart::Fresh => AgentSession::Open(id),
        AgentStart::Continue => match legacy() {
            Some(file) => AgentSession::Fork {
                source: file.to_string_lossy().into_owned(),
                id,
            },
            // Nothing to carry over: the agent has never had a session, so it starts a named one now. Leaving it
            // unnamed would keep it from ever being forked.
            None => AgentSession::Open(id),
        },
    }
}

/// The session of a pane whose recorded id `id` may not have been written yet, because it is to be forked from
/// `fork_from` (an attempt from its writer, a writer from the adopted attempt): a fork the first time (`started` is
/// false), then the session itself.
pub fn forked_session(id: &str, fork_from: &str, started: bool) -> AgentSession {
    if started {
        AgentSession::Open(id.to_owned())
    } else {
        AgentSession::Fork {
            source: fork_from.to_owned(),
            id: id.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn pi_folders_are_named_after_the_working_directory() {
        assert_eq!(
            project_folder_name(Path::new("/private/tmp/code/api-worktrees/rate-limit")),
            "--private-tmp-code-api-worktrees-rate-limit--"
        );
        assert_eq!(project_folder_name(Path::new("/a:b")), "--a-b--");
    }

    #[test]
    fn arguments_open_fork_or_keep_todays_behaviour() {
        let task = AgentStart::Task("fix it".into());
        let open = AgentSession::Open("xctx-s-writer-api".into());
        assert_eq!(
            agent_args(&task, &open),
            ["--session-id", "xctx-s-writer-api", "--", "fix it"]
        );
        assert_eq!(
            agent_args(&AgentStart::Continue, &open),
            ["--session-id", "xctx-s-writer-api"]
        );
        let fork = AgentSession::Fork {
            source: "xctx-s-writer-api".into(),
            id: "xctx-s-writer-api-attempt-2".into(),
        };
        assert_eq!(
            agent_args(&AgentStart::Task("use a token bucket".into()), &fork),
            [
                "--fork",
                "xctx-s-writer-api",
                "--session-id",
                "xctx-s-writer-api-attempt-2",
                "--",
                "use a token bucket"
            ]
        );
        assert_eq!(
            agent_args(&AgentStart::Fresh, &fork),
            [
                "--fork",
                "xctx-s-writer-api",
                "--session-id",
                "xctx-s-writer-api-attempt-2"
            ]
        );
        let unnamed = AgentSession::Unnamed;
        assert_eq!(agent_args(&AgentStart::Continue, &unnamed), ["--continue"]);
        assert!(agent_args(&AgentStart::Fresh, &unnamed).is_empty());
        assert_eq!(agent_args(&task, &unnamed), ["--", "fix it"]);
    }

    #[test]
    fn agents_get_ids_and_old_agents_carry_their_session_over() {
        let never = || -> Option<PathBuf> { panic!("only resumed agents without an id look") };
        assert_eq!(
            pane_session(
                "s",
                "writer-api",
                Some("xctx-s-other"),
                &AgentStart::Continue,
                never
            ),
            AgentSession::Open("xctx-s-other".into())
        );
        assert_eq!(
            pane_session(
                "s",
                "writer-api",
                None,
                &AgentStart::Task("t".into()),
                never
            ),
            AgentSession::Open("xctx-s-writer-api".into())
        );
        assert_eq!(
            pane_session("s", "helper-1", None, &AgentStart::Fresh, never),
            AgentSession::Open("xctx-s-helper-1".into())
        );
        assert_eq!(
            pane_session("s", "writer-api", None, &AgentStart::Continue, || Some(
                PathBuf::from("/p/2026_abc.jsonl")
            )),
            AgentSession::Fork {
                source: "/p/2026_abc.jsonl".into(),
                id: "xctx-s-writer-api".into()
            }
        );
        // An agent with no session anywhere starts a named one, so it can be forked.
        let fresh = pane_session("s", "writer-api", None, &AgentStart::Continue, || None);
        assert_eq!(fresh, AgentSession::Open("xctx-s-writer-api".into()));
        assert_eq!(fresh.named_id(), Some("xctx-s-writer-api"));
        assert_eq!(
            adopted_session_id("s", "writer-api", 3),
            "xctx-s-writer-api-adopted-3"
        );
    }

    #[test]
    fn forks_need_a_recorded_session_on_disk() {
        let on_disk = |id: &str| id == "xctx-s-w" || id == "xctx-s-w-attempt-2";
        let refused = fork_source("w", None, None, on_disk);
        assert!(refused.is_err_and(|e| e.contains("start the writer's agent pane first")));
        assert_eq!(
            fork_source("w", Some("xctx-s-w"), None, on_disk).as_deref(),
            Ok("xctx-s-w")
        );
        // An adopted id not started yet forks from the winner.
        assert_eq!(
            fork_source(
                "w",
                Some("xctx-s-w-adopted-2"),
                Some("xctx-s-w-attempt-2"),
                on_disk
            )
            .as_deref(),
            Ok("xctx-s-w-attempt-2")
        );
        let unwritten = fork_source("w", Some("xctx-s-w-new"), None, on_disk);
        assert!(unwritten.is_err_and(|e| e.contains("has not been written yet")));
        assert!(fork_source("w", Some("xctx-s-x"), Some("xctx-s-y"), on_disk).is_err());
        assert_eq!(
            fork_source_arg("xctx-s-w", Some(Path::new("/p/1_xctx-s-w.jsonl"))),
            "/p/1_xctx-s-w.jsonl"
        );
        assert_eq!(fork_source_arg("xctx-s-w", None), "xctx-s-w");
        assert!(is_adopted_session(
            "s",
            "writer-api",
            "xctx-s-writer-api-adopted-3"
        ));
        assert!(!is_adopted_session("s", "writer-api", "xctx-s-writer-api"));
    }

    #[test]
    fn forked_sessions_fork_once_then_open() {
        assert_eq!(
            forked_session("a", "src", false),
            AgentSession::Fork {
                source: "src".into(),
                id: "a".into()
            }
        );
        assert_eq!(
            forked_session("a", "src", true),
            AgentSession::Open("a".into())
        );
    }

    #[test]
    fn sessions_are_found_in_the_worktree_folder() -> TestResult {
        let dir = tempfile::tempdir()?;
        let root = fs::canonicalize(dir.path())?;
        let worktree = root.join("code/api-worktrees/s");
        fs::create_dir_all(&worktree)?;
        let storage = SessionStorage {
            root: root.join("sessions"),
            flat: false,
        };
        assert_eq!(storage.newest_session_in(&worktree), None);
        let folder = storage.folder_for(&worktree);
        fs::create_dir_all(&folder)?;
        let header = |id: &str, cwd: &Path| {
            format!(
                "{{\"type\":\"session\",\"id\":\"{id}\",\"cwd\":\"{}\"}}\n",
                cwd.display()
            )
        };
        let old = folder.join("2026-01-01T00-00-00-000Z_old.jsonl");
        let new = folder.join("2026-02-01T00-00-00-000Z_xctx-s-writer-api.jsonl");
        let elsewhere = folder.join("2026-03-01T00-00-00-000Z_other.jsonl");
        fs::write(&old, header("old", &worktree))?;
        fs::write(&new, header("xctx-s-writer-api", &worktree))?;
        fs::write(&elsewhere, header("other", &root))?;
        // Named sessions belong to an agent with an id (a helper in the same worktree, say); only the unnamed
        // one from before Milestone 4 is carried over.
        assert_eq!(storage.newest_session_in(&worktree), Some(old.clone()));
        let renamed = folder.join("2026-02-02T00-00-00-000Z_plain.jsonl");
        fs::write(&renamed, header("xctx-s-helper-1", &worktree))?;
        assert_eq!(storage.newest_session_in(&worktree), Some(old));
        assert!(storage.has_session(&worktree, "xctx-s-writer-api"));
        assert_eq!(storage.find("xctx-s-writer-api"), Some(new));
        assert_eq!(storage.find("xctx-s-nobody"), None);
        assert!(!storage.has_session(&worktree, "xctx-s-writer"));

        let flat = SessionStorage {
            root: folder.clone(),
            flat: true,
        };
        assert_eq!(flat.folder_for(&worktree), folder);
        assert!(flat.has_session(&worktree, "old"));
        assert_eq!(
            SessionStorage::from_values(Some("/s".into()), Some("/h".into())),
            Some(SessionStorage {
                root: "/s".into(),
                flat: true
            })
        );
        assert_eq!(
            SessionStorage::from_values(None, Some("/h".into())).map(|s| s.flat),
            Some(false)
        );
        Ok(())
    }
}
