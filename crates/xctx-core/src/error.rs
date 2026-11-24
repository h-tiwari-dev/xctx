//! The one error type every fallible `xctx-core` operation returns.

use std::io;
use std::path::PathBuf;

use crate::space::{SpaceId, SpaceStatus};

/// A failure the app can show to the user. Each variant carries enough context to say what went wrong and where.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Reading or writing a file or directory failed.
    #[error("{path}: {source}")]
    Io {
        /// The file or directory involved.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },

    /// A program (git, gh) could not be started at all, usually because it is not installed.
    #[error("could not run {program}: {source}")]
    Spawn {
        /// The program that failed to start.
        program: String,
        /// The underlying error.
        source: io::Error,
    },

    /// A program ran and exited unsuccessfully. `message` is its trimmed stderr, or a generic line when it was empty.
    #[error("{program} {args} failed in {dir}: {message}")]
    Command {
        /// The program that ran, e.g. `git`.
        program: String,
        /// Its arguments, joined with spaces.
        args: String,
        /// The directory it ran in.
        dir: PathBuf,
        /// What it reported.
        message: String,
    },

    /// A JSON file in the store could not be read or written.
    #[error("{path}: {source}")]
    Json {
        /// The file involved.
        path: PathBuf,
        /// The underlying error.
        source: serde_json::Error,
    },

    /// The path is not inside a git repository.
    #[error("{0} is not a git repository")]
    NotARepository(PathBuf),

    /// No space with this id is in the store.
    #[error("no space named {0}")]
    SpaceNotFound(SpaceId),

    /// The operation needs an active space, and this one has already been shipped or discarded.
    #[error("space {id} is {status:?}, not active")]
    SpaceNotActive {
        /// The space.
        id: SpaceId,
        /// What it is instead.
        status: SpaceStatus,
    },

    /// A draft must name at least one repository before it can launch.
    #[error("the draft has no repositories")]
    EmptyDraft,

    /// A widening must add at least one repository the space does not already have, or switch it to production.
    #[error("widening space {0} changes nothing: no new repositories and no switch to production")]
    EmptyWiden(SpaceId),

    /// A repository a widening would add already has the space's `space/<slug>` branch or worktree folder, left by
    /// work the store does not know about; the widening stops rather than take it over.
    #[error("{repo} already has {branch} or its worktree folder; refusing to take it over")]
    SpaceBranchTaken {
        /// The repository's main checkout (its bare directory for a bare layout).
        repo: PathBuf,
        /// The space's branch, `space/<slug>`.
        branch: String,
    },

    /// These repositories have uncommitted changes in the space's worktree, so the operation stopped before
    /// changing anything.
    #[error("uncommitted changes in {}; commit or discard them first", .repos.join(", "))]
    DirtyWorktrees {
        /// Names of the repositories with changes.
        repos: Vec<String>,
    },

    /// Every port in the space port range is held by an active space.
    #[error("no free port between {first} and {last}")]
    NoFreePort {
        /// The first port of the range.
        first: u16,
        /// The last port of the range.
        last: u16,
    },

    /// The space has no repository with this name.
    #[error("space {space} has no repo {repo}")]
    UnknownSpaceRepo {
        /// The space.
        space: SpaceId,
        /// The name asked for.
        repo: String,
    },

    /// An attempt's branch or worktree folder already exists, left by work xctx does not know about; the fork stops
    /// rather than take it over.
    #[error("{repo} already has {branch} or its worktree folder; refusing to take it over")]
    AttemptBranchTaken {
        /// The repository's main checkout.
        repo: PathBuf,
        /// The attempt's branch, `attempt/<slug>/<n>`.
        branch: String,
    },

    /// Adopting the attempt would conflict with the space's branch in these files; nothing was changed.
    #[error("adopting conflicts in {}; resolve it in the space first", .files.join(", "))]
    AdoptConflicts {
        /// The conflicting paths.
        files: Vec<String>,
    },

    /// The space's worktree changed after the attempts were forked from it, so adopting would lose that work;
    /// nothing was changed.
    #[error(
        "{repo} changed after the attempts were forked; commit or discard that work before adopting"
    )]
    ChangedSinceFork {
        /// The repository's name in the space.
        repo: String,
    },

    /// Neither `XCTX_DATA_DIR` nor `HOME` is set, so there is nowhere to keep the store.
    #[error("neither XCTX_DATA_DIR nor HOME is set")]
    NoDataDir,
}

/// The result type of every fallible `xctx-core` operation.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Wrap an I/O error with the path it concerns.
pub(crate) fn io_error(path: impl Into<PathBuf>) -> impl FnOnce(io::Error) -> Error {
    let path = path.into();
    move |source| Error::Io { path, source }
}
