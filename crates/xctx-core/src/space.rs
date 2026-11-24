//! The space model and its lifecycle. A [`SpaceDraft`] is a proposal that touches nothing on disk; [`launch`] turns
//! it into a [`Space`] with a worktree per repository on branch `space/<slug>`, each with its own git-ignored
//! `mise.<slug>.toml`. [`widen`] adds repositories to an active space, or switches it to `MISE_ENV=production`.
//! [`ship`] pushes and opens pull requests, then removes the worktrees; [`discard`] only removes the worktrees. Both
//! keep the branches.

use std::fmt;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::git::{self, PullRequestRequest};
use crate::index::task_words;
use crate::mise;
use crate::store::Store;

/// The ports spaces are given, one per active space.
pub const PORT_RANGE: RangeInclusive<u16> = 4001..=4999;

const SLUG_WORDS: usize = 4;
const SLUG_MAX_LEN: usize = 40;
const TITLE_WORDS: usize = 4;

/// A space's id: its slug, which is also its `MISE_ENV` name and the tail of its `space/<slug>` branch.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SpaceId(String);

impl SpaceId {
    /// Wrap a slug. The store only ever makes ids from [`slugify`], which keeps them to `a-z`, `0-9` and `-`.
    pub fn new(slug: impl Into<String>) -> SpaceId {
        SpaceId(slug.into())
    }

    /// The slug.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SpaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a space is laid out for. Reshaping changes the panes, never the worktrees or agents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpaceShape {
    /// Writing code: agents and shells.
    #[default]
    Dev,
    /// Chasing a failure.
    Debug,
    /// Reading the diff before shipping.
    Review,
}

/// Where a space is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpaceStatus {
    /// Launched, with worktrees on disk.
    Active,
    /// Pushed and pull requests opened where there was work; worktrees removed, branches kept.
    Shipped,
    /// Worktrees removed, branches kept.
    Discarded,
}

/// A repository proposed for a draft, with why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftRepo {
    /// The repository: its main checkout, or any directory inside it.
    pub path: PathBuf,
    /// The name shown to the user: the folder name.
    pub name: String,
    /// Why it is in the draft, for the user, e.g. `name matches "payments"; files src/limit.ts`.
    pub reason: String,
}

impl DraftRepo {
    /// A draft repository named after the last component of `path`.
    pub fn new(path: impl Into<PathBuf>, reason: impl Into<String>) -> DraftRepo {
        let path = path.into();
        DraftRepo {
            name: folder_name(&path),
            path,
            reason: reason.into(),
        }
    }
}

/// A proposed space: a task and the repositories it touches. Nothing exists on disk until [`launch`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceDraft {
    /// The task as the user typed it; the agent is given this.
    pub task: String,
    /// A short name for tabs: the first words of the task.
    pub title: String,
    /// The shape to open in.
    pub shape: SpaceShape,
    /// The repositories, best match first.
    pub repos: Vec<DraftRepo>,
}

impl SpaceDraft {
    /// A draft for `task` with no repositories yet, in the dev shape.
    pub fn new(task: impl Into<String>) -> SpaceDraft {
        let task = task.into();
        let title = task
            .split_whitespace()
            .take(TITLE_WORDS)
            .collect::<Vec<_>>()
            .join(" ");
        SpaceDraft {
            title: if title.is_empty() {
                "space".into()
            } else {
                title
            },
            task,
            shape: SpaceShape::Dev,
            repos: Vec::new(),
        }
    }

    /// Add a repository. Returns false, and changes nothing, when one with the same path is already there.
    pub fn add_repo(&mut self, repo: DraftRepo) -> bool {
        if self.repos.iter().any(|r| r.path == repo.path) {
            return false;
        }
        self.repos.push(repo);
        true
    }

    /// Drop the repository at `path`. Returns false when it was not in the draft.
    pub fn drop_repo(&mut self, path: &Path) -> bool {
        let before = self.repos.len();
        self.repos.retain(|r| r.path != path);
        self.repos.len() != before
    }
}

/// One repository of a launched space.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceRepo {
    /// The repository's folder name, with `-2`, `-3`, ... appended when another repository of the space has the
    /// same one. Unique within the space, so panes can name their repository by it.
    pub name: String,
    /// The repository's main checkout, or its bare directory for a bare layout.
    pub repo: PathBuf,
    /// This space's worktree of it, where the agent and shell run.
    pub worktree: PathBuf,
    /// The branch checked out in the worktree: `space/<slug>`.
    pub branch: String,
    /// The branch the main checkout was on at launch (the default branch for a bare layout); pull requests target
    /// it.
    pub base_branch: String,
    /// The commit the space's branch started from; commits after it are the space's work.
    pub base_commit: String,
    /// True when launch created the branch; false when it checked out a `space/<slug>` branch that already existed.
    pub branch_created: bool,
    /// Why the repository is in the space, carried over from the draft.
    pub reason: String,
}

/// A launched task: one worktree per repository on `space/<slug>`, its own `PORT`, and `MISE_ENV=<slug>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Space {
    /// The slug.
    pub id: SpaceId,
    /// A short name for tabs.
    pub title: String,
    /// The task as the user typed it.
    pub task: String,
    /// What it is laid out for now.
    pub shape: SpaceShape,
    /// Where it is in its life.
    pub status: SpaceStatus,
    /// Its `PORT`, unique among active spaces.
    pub port: u16,
    /// When it was launched, in seconds since the Unix epoch.
    pub created_at: u64,
    /// Its repositories.
    pub repos: Vec<SpaceRepo>,
    /// True once a [`widen`] the user approved switched the space's terminals to `MISE_ENV=production`. Absent
    /// in stores written before Milestone 3, which read as false.
    #[serde(default)]
    pub production: bool,
}

impl Space {
    /// The branch every worktree of the space is on: `space/<slug>`.
    pub fn branch(&self) -> String {
        space_branch(self.id.as_str())
    }

    /// The space's own `MISE_ENV` name: its slug, which names its `mise.<slug>.toml`. See
    /// [`Space::effective_mise_env`] for the value its terminals actually run with.
    pub fn mise_env(&self) -> &str {
        self.id.as_str()
    }

    /// The `MISE_ENV` the space's terminals run with: `production` once [`Space::production`] is set, else its slug
    /// ([`Space::mise_env`]). Under `production` mise does not load `mise.<slug>.toml`; the space's `PORT` and
    /// `XCTX_SPACE` still reach its terminals through [`Space::env`].
    pub fn effective_mise_env(&self) -> &str {
        if self.production {
            PRODUCTION_MISE_ENV
        } else {
            self.mise_env()
        }
    }

    /// The name of the space's mise file in each worktree.
    pub fn mise_file_name(&self) -> String {
        mise::mise_file_name(self.id.as_str())
    }

    /// The environment the space's terminals run with: `PORT`, `XCTX_SPACE` and `MISE_ENV` (its
    /// [`Space::effective_mise_env`]).
    pub fn env(&self) -> Vec<(String, String)> {
        let mut env = mise::space_env_vars(self.id.as_str(), self.port);
        env.push(("MISE_ENV".to_owned(), self.effective_mise_env().to_owned()));
        env
    }
}

/// The `MISE_ENV` a space switched to production runs with.
pub const PRODUCTION_MISE_ENV: &str = "production";

/// The branch for the space `slug`: `space/<slug>`.
pub fn space_branch(slug: &str) -> String {
    format!("space/{slug}")
}

/// A short readable slug for a task: its first few meaningful ASCII words, lowercased and joined with `-`, or
/// `space` when it has none. Slugs name branches, files and `MISE_ENV`, so they stay within `a-z`, `0-9` and `-`.
pub fn slugify(task: &str) -> String {
    let mut slug = String::new();
    let words = task_words(task)
        .into_iter()
        .filter(|w| w.chars().all(|c| c.is_ascii_alphanumeric()))
        .take(SLUG_WORDS);
    for word in words {
        let extra = usize::from(!slug.is_empty()) + word.len();
        if !slug.is_empty() && slug.len() + extra > SLUG_MAX_LEN {
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word);
    }
    slug.truncate(SLUG_MAX_LEN);
    if slug.is_empty() {
        "space".into()
    } else {
        slug
    }
}

/// [`slugify`], with `-2`, `-3`, ... appended until `taken` says the slug is free.
pub fn unique_slug(task: &str, mut taken: impl FnMut(&str) -> bool) -> String {
    let root = slugify(task);
    let mut slug = root.clone();
    let mut n = 2;
    while taken(&slug) {
        slug = format!("{root}-{n}");
        n += 1;
    }
    slug
}

/// The lowest port in [`PORT_RANGE`] not in `used`.
pub fn allocate_port(used: impl IntoIterator<Item = u16>) -> Result<u16> {
    let used: Vec<u16> = used.into_iter().collect();
    PORT_RANGE
        .clone()
        .find(|p| !used.contains(p))
        .ok_or(Error::NoFreePort {
            first: *PORT_RANGE.start(),
            last: *PORT_RANGE.end(),
        })
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `name`, with `-2`, `-3`, ... appended until no name in `taken` is the same.
fn unique_repo_name(name: &str, taken: &[String]) -> String {
    let mut unique = name.to_owned();
    let mut n = 2;
    while taken.contains(&unique) {
        unique = format!("{name}-{n}");
        n += 1;
    }
    unique
}

/// Whether `slug` is in use in any of `targets`: its `space/<slug>` branch or its worktree directory already exists,
/// e.g. left by a space whose record was lost.
fn slug_in_use(targets: &[(DraftRepo, git::MainWorktree)], slug: &str) -> Result<bool> {
    let branch = space_branch(slug);
    for (_, main) in targets {
        if git::branch_exists(&main.path, &branch)? || git::worktree_path(main, slug).exists() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Undo what a failed launch made, newest first. Errors here are dropped: the launch error is what the user sees.
fn roll_back(slug: &str, made: &[SpaceRepo]) {
    for repo in made.iter().rev() {
        let _ = git::remove_worktree(&repo.repo, &repo.worktree, true);
        let _ = mise::remove_space_mise_file(&repo.worktree, &repo.repo, slug);
        if repo.branch_created {
            let _ = git::delete_branch(&repo.repo, &repo.branch);
        }
    }
}

fn launch_repo(
    draft_repo: &DraftRepo,
    main: &git::MainWorktree,
    slug: &str,
    title: &str,
    port: u16,
) -> Result<SpaceRepo> {
    let branch = space_branch(slug);
    let (base_branch, head) = if main.bare.is_some() {
        let base_branch = git::default_branch(&main.path)?;
        let head = git::resolve_commit(&main.path, &base_branch)?;
        (base_branch, head)
    } else {
        let base_branch = match git::current_branch(&main.path)? {
            Some(branch) => branch,
            None => git::default_branch(&main.path)?,
        };
        (base_branch, git::head_commit(&main.path)?)
    };
    let path = git::worktree_path(main, slug);
    let created = git::create_worktree(&main.path, &path, &branch, &head)?;
    let mut repo = SpaceRepo {
        name: draft_repo.name.clone(),
        repo: main.path.clone(),
        worktree: created.path,
        branch,
        base_branch,
        base_commit: head.clone(),
        branch_created: created.branch_created,
        reason: draft_repo.reason.clone(),
    };
    let finish = |repo: &mut SpaceRepo| -> Result<()> {
        if !repo.branch_created {
            repo.base_commit = git::merge_base(&repo.repo, &head, &repo.branch)?;
        }
        mise::write_space_mise_file(&repo.worktree, slug, title, port)?;
        Ok(())
    };
    if let Err(e) = finish(&mut repo) {
        roll_back(slug, std::slice::from_ref(&repo));
        return Err(e);
    }
    Ok(repo)
}

/// Make the draft real: for each repository, a worktree on a new branch `space/<slug>` started from the main
/// checkout's HEAD (from the default branch for a bare layout), and a git-ignored `mise.<slug>.toml` with the
/// space's `PORT`. The slug is one no saved space has and no repository of the draft has a `space/<slug>` branch or
/// worktree directory for, so a launch never takes over work it does not know about. Repositories that share a
/// folder name get unique [`SpaceRepo::name`]s. The space is saved to `store` as active. All or nothing: if any step
/// fails, what was made is removed again and the store is unchanged.
pub fn launch(store: &mut Store, draft: SpaceDraft) -> Result<Space> {
    if draft.repos.is_empty() {
        return Err(Error::EmptyDraft);
    }
    let mut targets: Vec<(DraftRepo, git::MainWorktree)> = Vec::new();
    for draft_repo in &draft.repos {
        let main = git::main_worktree(&draft_repo.path)?;
        if targets.iter().all(|(_, m)| m.path != main.path) {
            targets.push((draft_repo.clone(), main));
        }
    }
    let mut names: Vec<String> = Vec::new();
    for (draft_repo, _) in &mut targets {
        draft_repo.name = unique_repo_name(&draft_repo.name, &names);
        names.push(draft_repo.name.clone());
    }
    let mut lookup_error = None;
    let slug = unique_slug(&draft.task, |slug| {
        if store.spaces().iter().any(|s| s.id.as_str() == slug) {
            return true;
        }
        match slug_in_use(&targets, slug) {
            Ok(in_use) => in_use,
            Err(e) => {
                lookup_error = Some(e);
                false
            }
        }
    });
    if let Some(e) = lookup_error {
        return Err(e);
    }
    let port = store.allocate_port()?;
    let mut made: Vec<SpaceRepo> = Vec::new();
    for (draft_repo, main) in &targets {
        match launch_repo(draft_repo, main, &slug, &draft.title, port) {
            Ok(repo) => made.push(repo),
            Err(e) => {
                roll_back(&slug, &made);
                return Err(e);
            }
        }
    }
    let space = Space {
        id: SpaceId::new(slug.clone()),
        title: draft.title,
        task: draft.task,
        shape: draft.shape,
        status: SpaceStatus::Active,
        port,
        created_at: now_secs(),
        repos: made,
        production: false,
    };
    if let Err(e) = store.upsert_space(space.clone()) {
        roll_back(&slug, &space.repos);
        return Err(e);
    }
    Ok(space)
}

fn active_space(store: &Store, id: &SpaceId) -> Result<Space> {
    let space = store
        .space(id)
        .cloned()
        .ok_or_else(|| Error::SpaceNotFound(id.clone()))?;
    if space.status != SpaceStatus::Active {
        return Err(Error::SpaceNotActive {
            id: id.clone(),
            status: space.status,
        });
    }
    Ok(space)
}

/// What a user approved adding to an active space: repositories, a switch to `MISE_ENV=production`, or both.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidenRequest {
    /// Repositories to add a worktree of, each by its main checkout or any directory inside it. Ones the space
    /// already has are skipped.
    #[serde(default)]
    pub repos: Vec<DraftRepo>,
    /// Switch the space's terminals to `MISE_ENV=production`.
    #[serde(default)]
    pub production: bool,
}

/// What [`widen`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WidenReport {
    /// The repositories added, in request order, as they are now saved in the space.
    pub added_repos: Vec<SpaceRepo>,
    /// Whether the space now runs with `MISE_ENV=production` (it may already have before this widening).
    pub production_enabled: bool,
}

/// Whether two repository paths are the same directory, comparing canonical paths when both can be resolved.
fn same_repo(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Widen an active space with what the user approved. Each repository the space does not already have (compared by
/// main checkout, so two paths into one repository count once) gets a worktree on `space/<slug>` started from its
/// main checkout's HEAD (its default branch for a bare layout) and a git-ignored `mise.<slug>.toml` with the space's
/// `PORT` and `XCTX_SPACE`, under a [`SpaceRepo::name`] unique within the space (`-2`, `-3`, ... as in [`launch`]).
/// With `request.production`, [`Space::production`] is set so [`Space::effective_mise_env`] is `production`.
///
/// Fails with [`Error::SpaceNotFound`] or [`Error::SpaceNotActive`] for a space that cannot be widened,
/// [`Error::EmptyWiden`] when nothing would change, and [`Error::SpaceBranchTaken`] when a new repository already
/// has a `space/<slug>` branch or worktree folder. All or nothing: if any repository or the store update fails,
/// every worktree, mise file and branch this call made is removed and the saved space is unchanged.
pub fn widen(store: &mut Store, id: &SpaceId, request: WidenRequest) -> Result<WidenReport> {
    let mut space = active_space(store, id)?;
    if request.repos.is_empty() && !request.production {
        return Err(Error::EmptyWiden(id.clone()));
    }
    let slug = space.id.as_str().to_owned();
    let branch = space.branch();
    let mut targets: Vec<(DraftRepo, git::MainWorktree)> = Vec::new();
    for draft_repo in &request.repos {
        let main = git::main_worktree(&draft_repo.path)?;
        let known = space.repos.iter().any(|r| same_repo(&r.repo, &main.path))
            || targets.iter().any(|(_, m)| same_repo(&m.path, &main.path));
        if !known {
            targets.push((draft_repo.clone(), main));
        }
    }
    if targets.is_empty() && (!request.production || space.production) {
        return Err(Error::EmptyWiden(id.clone()));
    }
    for (_, main) in &targets {
        if git::branch_exists(&main.path, &branch)? || git::worktree_path(main, &slug).exists() {
            return Err(Error::SpaceBranchTaken {
                repo: main.path.clone(),
                branch,
            });
        }
    }
    let mut names: Vec<String> = space.repos.iter().map(|r| r.name.clone()).collect();
    for (draft_repo, _) in &mut targets {
        draft_repo.name = unique_repo_name(&draft_repo.name, &names);
        names.push(draft_repo.name.clone());
    }
    let mut made: Vec<SpaceRepo> = Vec::new();
    for (draft_repo, main) in &targets {
        match launch_repo(draft_repo, main, &slug, &space.title, space.port) {
            Ok(repo) => made.push(repo),
            Err(e) => {
                roll_back(&slug, &made);
                return Err(e);
            }
        }
    }
    space.repos.extend(made.iter().cloned());
    space.production |= request.production;
    let production_enabled = space.production;
    if let Err(e) = store.upsert_space(space) {
        roll_back(&slug, &made);
        return Err(e);
    }
    Ok(WidenReport {
        added_repos: made,
        production_enabled,
    })
}

/// Fail with [`Error::DirtyWorktrees`] when any worktree of the space that still exists has uncommitted changes.
fn ensure_clean(space: &Space) -> Result<()> {
    let mut dirty = Vec::new();
    for repo in &space.repos {
        if repo.worktree.is_dir() && git::is_dirty(&repo.worktree)? {
            dirty.push(repo.name.clone());
        }
    }
    if dirty.is_empty() {
        Ok(())
    } else {
        Err(Error::DirtyWorktrees { repos: dirty })
    }
}

/// Remove each worktree of the space, then its mise file, so a worktree git refuses to remove keeps its mise file;
/// branches stay. A repository whose worktree and repository folders are both gone has nothing left to clean up.
fn remove_worktrees(space: &Space, force: bool) -> Result<Vec<RemovedWorktree>> {
    let mut removed = Vec::new();
    for repo in &space.repos {
        let existed = repo.worktree.is_dir();
        if existed || repo.repo.exists() {
            git::remove_worktree(&repo.repo, &repo.worktree, force)?;
            mise::remove_space_mise_file(&repo.worktree, &repo.repo, space.id.as_str())?;
        }
        removed.push(RemovedWorktree {
            name: repo.name.clone(),
            worktree: repo.worktree.clone(),
            branch: repo.branch.clone(),
            already_gone: !existed,
        });
    }
    Ok(removed)
}

/// The commit after which the space's branch has its own work: where it meets its base branch, so upstream commits
/// merged into the space are not counted, or the launch's base commit when the base branch is gone or unrelated.
fn ship_base(repo: &SpaceRepo) -> Result<String> {
    if !git::branch_exists(&repo.repo, &repo.base_branch)? {
        return Ok(repo.base_commit.clone());
    }
    match git::merge_base(&repo.repo, &repo.base_branch, &repo.branch) {
        Ok(base) => Ok(base),
        Err(Error::Command { .. }) => Ok(repo.base_commit.clone()),
        Err(e) => Err(e),
    }
}

fn set_status(store: &mut Store, mut space: Space, status: SpaceStatus) -> Result<()> {
    space.status = status;
    store.upsert_space(space)
}

/// How [`ship`] opens pull requests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShipOptions {
    /// The `gh` program. With `None`, branches are pushed and no pull request is opened.
    pub gh: Option<PathBuf>,
}

impl ShipOptions {
    /// Options that use the `gh` found by [`git::find_gh`], if any.
    pub fn detect() -> ShipOptions {
        ShipOptions { gh: git::find_gh() }
    }
}

/// Why [`ship`] did not push a repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShipSkipReason {
    /// The space's branch has no commits after its base.
    NoNewCommits,
    /// The repository has no remote; the commits stay on the local branch.
    NoRemote,
}

/// What happened to a pushed branch's pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PullRequestOutcome {
    /// `gh` opened it.
    Opened {
        /// The pull request's URL.
        url: String,
    },
    /// No `gh` program was configured, so none was opened.
    GhNotFound,
    /// `gh` failed; the branch is pushed, so the user can open one by hand.
    Failed {
        /// What `gh` reported.
        message: String,
    },
}

/// What [`ship`] did with one repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShipOutcome {
    /// Nothing was pushed.
    Skipped(ShipSkipReason),
    /// The branch was pushed.
    Pushed {
        /// The remote it went to.
        remote: String,
        /// What happened to the pull request.
        pull_request: PullRequestOutcome,
    },
}

/// One repository's line of a [`ShipReport`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoShipReport {
    /// The repository's name.
    pub name: String,
    /// The space's branch.
    pub branch: String,
    /// Commits on the branch that its base branch does not have.
    pub commits_ahead: u32,
    /// What happened.
    pub outcome: ShipOutcome,
}

impl fmt::Display for RepoShipReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let commits = if self.commits_ahead == 1 {
            "1 commit".to_owned()
        } else {
            format!("{} commits", self.commits_ahead)
        };
        match &self.outcome {
            ShipOutcome::Skipped(ShipSkipReason::NoNewCommits) => {
                write!(f, "{}: no new commits", self.name)
            }
            ShipOutcome::Skipped(ShipSkipReason::NoRemote) => {
                write!(
                    f,
                    "{}: {commits} kept on {} (no remote)",
                    self.name, self.branch
                )
            }
            ShipOutcome::Pushed {
                remote,
                pull_request,
            } => {
                write!(
                    f,
                    "{}: pushed {commits} to {remote}/{}",
                    self.name, self.branch
                )?;
                match pull_request {
                    PullRequestOutcome::Opened { url } => write!(f, ", opened {url}"),
                    PullRequestOutcome::GhNotFound => write!(f, ", no pull request (gh not found)"),
                    PullRequestOutcome::Failed { message } => {
                        write!(f, ", pull request failed: {message}")
                    }
                }
            }
        }
    }
}

/// A worktree [`ship`] or [`discard`] removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemovedWorktree {
    /// The repository's name.
    pub name: String,
    /// Where the worktree was.
    pub worktree: PathBuf,
    /// The branch it had checked out, which is kept.
    pub branch: String,
    /// True when the directory was already gone: git's record of it was pruned, or, when the repository is gone
    /// too, nothing was left to clean up.
    pub already_gone: bool,
}

/// What [`ship`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShipReport {
    /// The space.
    pub space: SpaceId,
    /// One line per repository, in the space's order.
    pub repos: Vec<RepoShipReport>,
    /// The worktrees removed.
    pub removed: Vec<RemovedWorktree>,
}

/// What [`discard`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscardReport {
    /// The space.
    pub space: SpaceId,
    /// The worktrees removed; their branches are kept.
    pub removed: Vec<RemovedWorktree>,
}

/// Ship an active space. Refuses, before changing anything, while any worktree has uncommitted changes. For each
/// repository with commits after its base and a remote, pushes `space/<slug>` and opens a pull request against the
/// base branch with `options.gh`; then removes every worktree and its mise file, keeps the branches, and marks the
/// space shipped. A failed push stops the ship with the space still active and its worktrees in place, so it can be
/// retried; a failed pull request is reported and does not stop it.
pub fn ship(store: &mut Store, id: &SpaceId, options: &ShipOptions) -> Result<ShipReport> {
    let space = active_space(store, id)?;
    ensure_clean(&space)?;
    let mut repos = Vec::new();
    for repo in &space.repos {
        let base = ship_base(repo)?;
        let commits_ahead = git::commits_ahead(&repo.repo, &base, &repo.branch)?;
        let outcome = if commits_ahead == 0 {
            ShipOutcome::Skipped(ShipSkipReason::NoNewCommits)
        } else if let Some(remote) = git::remote_name(&repo.repo)? {
            let dir = if repo.worktree.is_dir() {
                &repo.worktree
            } else {
                &repo.repo
            };
            git::push_branch(dir, &remote, &repo.branch)?;
            let pull_request = match &options.gh {
                None => PullRequestOutcome::GhNotFound,
                Some(gh) => {
                    let request = PullRequestRequest {
                        head: repo.branch.clone(),
                        base: repo.base_branch.clone(),
                        title: space.title.clone(),
                        body: space.task.clone(),
                    };
                    match git::open_pull_request(gh, dir, &request) {
                        Ok(url) => PullRequestOutcome::Opened { url },
                        Err(e) => PullRequestOutcome::Failed {
                            message: match e {
                                Error::Command { message, .. } => message,
                                other => other.to_string(),
                            },
                        },
                    }
                }
            };
            ShipOutcome::Pushed {
                remote,
                pull_request,
            }
        } else {
            ShipOutcome::Skipped(ShipSkipReason::NoRemote)
        };
        repos.push(RepoShipReport {
            name: repo.name.clone(),
            branch: repo.branch.clone(),
            commits_ahead,
            outcome,
        });
    }
    let removed = remove_worktrees(&space, false)?;
    set_status(store, space, SpaceStatus::Shipped)?;
    Ok(ShipReport {
        space: id.clone(),
        repos,
        removed,
    })
}

/// Discard an active space: remove every worktree and its mise file, keep the `space/<slug>` branches, and mark
/// the space discarded. Without `force`, refuses before changing anything while any worktree has uncommitted
/// changes; with `force`, those changes are lost (committed work survives on the branch).
pub fn discard(store: &mut Store, id: &SpaceId, force: bool) -> Result<DiscardReport> {
    let space = active_space(store, id)?;
    if !force {
        ensure_clean(&space)?;
    }
    let removed = remove_worktrees(&space, force)?;
    set_status(store, space, SpaceStatus::Discarded)?;
    Ok(DiscardReport {
        space: id.clone(),
        removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_take_the_first_meaningful_words() {
        assert_eq!(
            slugify("Add rate limiting to checkout"),
            "rate-limiting-checkout"
        );
        assert_eq!(slugify("the and for"), "space");
        assert_eq!(slugify("Ünïcode café & spaces!!"), "spaces");
        let long =
            slugify("internationalization localization accessibility performance observability");
        assert!(long.len() <= SLUG_MAX_LEN, "{long}");
        assert_eq!(long, "internationalization-localization");
    }

    #[test]
    fn unique_slugs_count_up() {
        let taken = ["login-page".to_owned(), "login-page-2".to_owned()];
        assert_eq!(
            unique_slug("login page", |s| taken.iter().any(|t| t == s)),
            "login-page-3"
        );
    }

    #[test]
    fn ports_fill_the_lowest_gap() {
        assert_eq!(allocate_port([4001, 4003]).ok(), Some(4002));
        assert!(matches!(
            allocate_port(PORT_RANGE),
            Err(Error::NoFreePort { .. })
        ));
    }

    #[test]
    fn drafts_add_and_drop_repos_by_path() {
        let mut draft = SpaceDraft::new("make checkout retry safe when we rate limit");
        assert_eq!(draft.title, "make checkout retry safe");
        assert!(draft.add_repo(DraftRepo::new("/code/payments", "name matches")));
        assert!(!draft.add_repo(DraftRepo::new("/code/payments", "again")));
        assert_eq!(draft.repos[0].name, "payments");
        assert!(draft.drop_repo(Path::new("/code/payments")));
        assert!(!draft.drop_repo(Path::new("/code/payments")));
    }
}
