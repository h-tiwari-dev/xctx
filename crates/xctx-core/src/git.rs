//! Git and worktrees: what a repository looks like, and the worktree, push and pull request operations a space
//! needs. Every call runs the `git` (or `gh`) program found on `PATH`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{Error, Result};

/// One worktree of a repository, as `git worktree list` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    /// The worktree's directory.
    pub path: PathBuf,
    /// The checked-out branch, or `None` when HEAD is detached.
    pub branch: Option<String>,
}

/// Where a repository's main checkout is, which decides where new worktrees go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MainWorktree {
    /// The main checkout, or the bare directory for a bare layout. Git commands for the repository run here; a
    /// bare layout never names one of its linked worktrees, which may belong to a space and vanish with it.
    pub path: PathBuf,
    /// The bare git directory, for a bare repository layout.
    pub bare: Option<PathBuf>,
}

/// What [`create_worktree`] made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedWorktree {
    /// The new worktree's directory.
    pub path: PathBuf,
    /// The branch checked out in it.
    pub branch: String,
    /// True when the branch was created for this worktree; false when an existing branch was checked out.
    pub branch_created: bool,
}

/// What [`open_pull_request`] needs to open a pull request with `gh`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestRequest {
    /// The branch with the changes.
    pub head: String,
    /// The branch to merge into.
    pub base: String,
    /// The pull request title.
    pub title: String,
    /// The pull request body.
    pub body: String,
}

/// Run `program args` in `dir` and return its stdout. A non-zero exit becomes [`Error::Command`] with the trimmed
/// stderr (or stdout, when stderr is empty).
fn run_program<I, S>(program: &OsStr, args: I, dir: &Path) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_program_env(program, args, dir, &[])
}

/// [`run_program`] with extra environment variables.
fn run_program_env<I, S>(
    program: &OsStr,
    args: I,
    dir: &Path,
    env: &[(&str, &OsStr)],
) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<S> = args.into_iter().collect();
    let mut command = Command::new(program);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command
        .args(&args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .output()
        .map_err(|source| Error::Spawn {
            program: program.to_string_lossy().into_owned(),
            source,
        })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let message = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exited with {}", output.status)
    };
    Err(Error::Command {
        program: program.to_string_lossy().into_owned(),
        args: args
            .iter()
            .map(|a| a.as_ref().to_string_lossy())
            .collect::<Vec<_>>()
            .join(" "),
        dir: dir.to_path_buf(),
        message,
    })
}

/// Run `git args` in `dir` and return its stdout. Credentials are never prompted for; a push that needs them fails.
pub fn run_git<I, S>(dir: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_program(OsStr::new("git"), args, dir)
}

/// Run `git args` in `dir` with extra environment variables, such as `GIT_INDEX_FILE`.
pub fn run_git_env<I, S>(dir: &Path, args: I, env: &[(&str, &OsStr)]) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_program_env(OsStr::new("git"), args, dir, env)
}

/// Run `git args` in `dir` and report whether it exited successfully, for commands whose exit status is the answer.
fn git_succeeds(dir: &Path, args: &[&str]) -> Result<bool> {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|source| Error::Spawn {
            program: "git".into(),
            source,
        })?;
    Ok(status.success())
}

fn trimmed(text: String) -> String {
    text.trim().to_owned()
}

/// The top directory of the worktree containing `dir`. Fails with [`Error::NotARepository`] when `dir` is not in a
/// git repository (or does not exist).
pub fn repo_root(dir: &Path) -> Result<PathBuf> {
    if !dir.is_dir() {
        return Err(Error::NotARepository(dir.to_path_buf()));
    }
    match run_git(dir, ["rev-parse", "--show-toplevel"]) {
        Ok(out) => Ok(PathBuf::from(trimmed(out))),
        Err(Error::Command { .. }) => Err(Error::NotARepository(dir.to_path_buf())),
        Err(e) => Err(e),
    }
}

/// Parse `git worktree list --porcelain`: the worktrees with a checkout, and the bare directory if there is one.
pub fn parse_worktrees(text: &str) -> (Vec<Worktree>, Option<PathBuf>) {
    let mut worktrees = Vec::new();
    let mut bare = None;
    for block in text.split("\n\n") {
        let mut path = None;
        let mut branch = None;
        let mut is_bare = false;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
                branch = Some(b.to_owned());
            } else if line == "bare" {
                is_bare = true;
            }
        }
        let Some(path) = path else { continue };
        if is_bare {
            bare = Some(path);
        } else {
            worktrees.push(Worktree { path, branch });
        }
    }
    (worktrees, bare)
}

/// Every worktree of the repository containing `dir`, main checkout first.
pub fn list_worktrees(dir: &Path) -> Result<Vec<Worktree>> {
    Ok(parse_worktrees(&run_git(dir, ["worktree", "list", "--porcelain"])?).0)
}

/// The main checkout of the repository containing `dir`, even when `dir` is one of its linked worktrees. For a bare
/// layout it is the bare directory: git lists linked worktrees by path, so the first one may be another space's.
pub fn main_worktree(dir: &Path) -> Result<MainWorktree> {
    if !dir.is_dir() {
        return Err(Error::NotARepository(dir.to_path_buf()));
    }
    let text = match run_git(dir, ["worktree", "list", "--porcelain"]) {
        Ok(text) => text,
        Err(Error::Command { .. }) => return Err(Error::NotARepository(dir.to_path_buf())),
        Err(e) => return Err(e),
    };
    let (worktrees, bare) = parse_worktrees(&text);
    let path = bare
        .clone()
        .or_else(|| worktrees.into_iter().next().map(|w| w.path))
        .ok_or_else(|| Error::NotARepository(dir.to_path_buf()))?;
    Ok(MainWorktree { path, bare })
}

/// The branch checked out in `dir`, or `None` when HEAD is detached.
pub fn current_branch(dir: &Path) -> Result<Option<String>> {
    let branch = trimmed(run_git(dir, ["branch", "--show-current"])?);
    Ok((!branch.is_empty()).then_some(branch))
}

/// The commit HEAD points at in `dir`.
pub fn head_commit(dir: &Path) -> Result<String> {
    Ok(trimmed(run_git(dir, ["rev-parse", "HEAD"])?))
}

/// The commit `rev` names, which may be any revision git understands.
pub fn resolve_commit(dir: &Path, rev: &str) -> Result<String> {
    Ok(trimmed(run_git(
        dir,
        [
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )?))
}

/// Whether `refs/heads/<branch>` exists in the repository containing `dir`.
pub fn branch_exists(dir: &Path, branch: &str) -> Result<bool> {
    git_succeeds(
        dir,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
}

/// The repository's default branch: what the remote's HEAD points at, else a local `main` or `master`, else the
/// branch checked out in `dir`, else `main`.
pub fn default_branch(dir: &Path) -> Result<String> {
    if let Some(remote) = remote_name(dir)? {
        let head = format!("refs/remotes/{remote}/HEAD");
        if git_succeeds(dir, &["symbolic-ref", "--quiet", &head])? {
            let target = trimmed(run_git(dir, ["symbolic-ref", "--short", &head])?);
            if let Some(branch) = target.strip_prefix(&format!("{remote}/")) {
                return Ok(branch.to_owned());
            }
        }
    }
    for candidate in ["main", "master"] {
        if branch_exists(dir, candidate)? {
            return Ok(candidate.to_owned());
        }
    }
    Ok(current_branch(dir)?.unwrap_or_else(|| "main".to_owned()))
}

/// Whether `dir` has uncommitted changes: modified, staged or untracked files that are not ignored.
pub fn is_dirty(dir: &Path) -> Result<bool> {
    Ok(!run_git(dir, ["status", "--porcelain"])?.trim().is_empty())
}

/// Whether git ignores `path` (relative to `dir`, or absolute) in the repository containing `dir`.
pub fn is_ignored(dir: &Path, path: &Path) -> Result<bool> {
    let path = path.to_string_lossy();
    git_succeeds(dir, &["check-ignore", "--quiet", &path])
}

/// The remote branches are pushed to: `origin` when it exists, else the first remote, else `None`.
pub fn remote_name(dir: &Path) -> Result<Option<String>> {
    let out = run_git(dir, ["remote"])?;
    let remotes: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    Ok(remotes
        .iter()
        .find(|r| **r == "origin")
        .or_else(|| remotes.first())
        .map(|r| (*r).to_owned()))
}

/// The URL of `remote`.
pub fn remote_url(dir: &Path, remote: &str) -> Result<String> {
    Ok(trimmed(run_git(dir, ["remote", "get-url", remote])?))
}

/// How many commits `head` has that `base` does not. Both are any revision git understands.
pub fn commits_ahead(dir: &Path, base: &str, head: &str) -> Result<u32> {
    let out = trimmed(run_git(
        dir,
        ["rev-list", "--count", &format!("{base}..{head}")],
    )?);
    out.parse().map_err(|_| Error::Command {
        program: "git".into(),
        args: format!("rev-list --count {base}..{head}"),
        dir: dir.to_path_buf(),
        message: format!("unexpected output {out:?}"),
    })
}

/// The best common ancestor of `a` and `b`.
pub fn merge_base(dir: &Path, a: &str, b: &str) -> Result<String> {
    Ok(trimmed(run_git(dir, ["merge-base", a, b])?))
}

/// Where a space's worktree for this repository goes: `<main checkout>-worktrees/<slug>` next to the main checkout,
/// or next to the bare directory for a bare layout. Keeping it out of `Application Support` keeps spaces out of the
/// path, which some build tools cannot handle.
pub fn worktree_path(main: &MainWorktree, slug: &str) -> PathBuf {
    let flat = slug.replace('/', "-");
    if let Some(parent) = main.bare.as_deref().and_then(Path::parent) {
        return parent.join(flat);
    }
    let mut dir = main.path.clone().into_os_string();
    dir.push("-worktrees");
    PathBuf::from(dir).join(flat)
}

/// Make a worktree at `path` with `branch` checked out. An existing branch is checked out as it is; otherwise the
/// branch is created from `start_point`. When the worktree cannot be made, a branch git made for it on the way is
/// deleted again (only while it still points at `start_point`) and git's error is returned. Git refuses when the
/// branch is already checked out somewhere else or `path` is a non-empty directory.
pub fn create_worktree(
    repo: &Path,
    path: &Path,
    branch: &str,
    start_point: &str,
) -> Result<CreatedWorktree> {
    let exists = branch_exists(repo, branch)?;
    let path_arg = path.as_os_str();
    if exists {
        run_git(
            repo,
            [
                OsStr::new("worktree"),
                OsStr::new("add"),
                path_arg,
                OsStr::new(branch),
            ],
        )?;
    } else {
        let start_commit = resolve_commit(repo, start_point).ok();
        let added = run_git(
            repo,
            [
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("-b"),
                OsStr::new(branch),
                path_arg,
                OsStr::new(start_point),
            ],
        );
        if let Err(e) = added {
            // git makes the branch before the worktree, and keeps it when the worktree cannot be made. Cleanup
            // errors are dropped so the caller sees why the worktree failed.
            let tip = resolve_commit(repo, &format!("refs/heads/{branch}")).ok();
            if tip.is_some() && tip == start_commit {
                let _ = delete_branch(repo, branch);
            }
            return Err(e);
        }
    }
    Ok(CreatedWorktree {
        path: path.to_path_buf(),
        branch: branch.to_owned(),
        branch_created: !exists,
    })
}

/// Remove the worktree at `path`; its branch is kept. Without `force`, git refuses when the worktree has changes.
/// A worktree directory that is already gone is pruned from git's records instead.
pub fn remove_worktree(repo: &Path, path: &Path, force: bool) -> Result<()> {
    if !path.exists() {
        run_git(repo, ["worktree", "prune"])?;
        return Ok(());
    }
    let mut args = vec![OsStr::new("worktree"), OsStr::new("remove")];
    if force {
        args.push(OsStr::new("--force"));
    }
    args.push(path.as_os_str());
    run_git(repo, args)?;
    Ok(())
}

/// Delete a local branch even if it is not merged. Used to undo a branch made by a launch that failed.
pub fn delete_branch(repo: &Path, branch: &str) -> Result<()> {
    run_git(repo, ["branch", "-D", branch])?;
    Ok(())
}

/// Push `branch` to `remote` and set it as the branch's upstream.
pub fn push_branch(dir: &Path, remote: &str, branch: &str) -> Result<()> {
    run_git(dir, ["push", "-u", remote, branch])?;
    Ok(())
}

/// Open a pull request with the `gh` program at `gh`, run in `dir`. Returns the URL `gh` prints.
pub fn open_pull_request(gh: &Path, dir: &Path, request: &PullRequestRequest) -> Result<String> {
    let out = run_program(
        gh.as_os_str(),
        [
            "pr",
            "create",
            "--head",
            &request.head,
            "--base",
            &request.base,
            "--title",
            &request.title,
            "--body",
            &request.body,
        ],
        dir,
    )?;
    let url = out
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or_default()
        .to_owned();
    Ok(url)
}

/// Find `gh`: on `PATH`, then where Homebrew installs it (a GUI app's `PATH` usually lacks Homebrew).
pub fn find_gh() -> Option<PathBuf> {
    let path_dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    path_dirs
        .into_iter()
        .chain([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ])
        .map(|dir| dir.join("gh"))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_parsing_skips_bare_and_keeps_detached() {
        let text = "worktree /r/.bare\nbare\n\nworktree /r/main\nHEAD abc\nbranch refs/heads/main\n\n\
                    worktree /r/x\nHEAD def\ndetached\n";
        let (worktrees, bare) = parse_worktrees(text);
        assert_eq!(bare, Some(PathBuf::from("/r/.bare")));
        assert_eq!(
            worktrees,
            vec![
                Worktree {
                    path: PathBuf::from("/r/main"),
                    branch: Some("main".into())
                },
                Worktree {
                    path: PathBuf::from("/r/x"),
                    branch: None
                },
            ]
        );
    }

    #[test]
    fn worktree_path_sits_next_to_the_main_checkout_or_the_bare_dir() {
        let main = MainWorktree {
            path: PathBuf::from("/code/payments"),
            bare: None,
        };
        assert_eq!(
            worktree_path(&main, "rate-limit"),
            PathBuf::from("/code/payments-worktrees/rate-limit")
        );
        let bare = MainWorktree {
            path: PathBuf::from("/code/r/main"),
            bare: Some(PathBuf::from("/code/r/.bare")),
        };
        assert_eq!(worktree_path(&bare, "a/b"), PathBuf::from("/code/r/a-b"));
    }
}

/// A commit holding everything in the worktree at `dir` right now: committed work, uncommitted changes and
/// untracked files that git does not ignore, on top of `HEAD`. Neither the worktree nor its index changes: the
/// files are staged into a temporary index. Returns `HEAD` itself when there is nothing uncommitted.
pub fn snapshot_commit(dir: &Path, message: &str) -> Result<String> {
    let head = head_commit(dir)?;
    let index_dir = tempfile::tempdir().map_err(|source| Error::Io {
        path: std::env::temp_dir(),
        source,
    })?;
    let index = index_dir.path().join("index");
    let env = [("GIT_INDEX_FILE", index.as_os_str())];
    run_git_env(dir, ["read-tree", "HEAD"], &env)?;
    run_git_env(dir, ["add", "-A", "--", "."], &env)?;
    let tree = trimmed(run_git_env(dir, ["write-tree"], &env)?);
    let head_tree = trimmed(run_git(dir, ["rev-parse", "HEAD^{tree}"])?);
    if tree == head_tree {
        return Ok(head);
    }
    let commit = run_git_env(
        dir,
        [
            "-c",
            "user.name=xctx",
            "-c",
            "user.email=xctx@localhost",
            "commit-tree",
            &tree,
            "-p",
            &head,
            "-m",
            message,
        ],
        &[],
    )?;
    Ok(trimmed(commit))
}

/// The tree a commit points at.
pub fn tree_of(dir: &Path, commit: &str) -> Result<String> {
    Ok(trimmed(run_git(
        dir,
        ["rev-parse", &format!("{commit}^{{tree}}")],
    )?))
}

/// One file changed between two commits, from `git diff --numstat`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileChange {
    /// The file's path in the repository (the new path of a rename).
    pub path: String,
    /// Lines added; `None` for a binary file.
    pub added: Option<u32>,
    /// Lines removed; `None` for a binary file.
    pub removed: Option<u32>,
}

/// Parse `git diff --numstat -z` output.
pub fn parse_numstat_z(text: &str) -> Vec<FileChange> {
    let mut changes = Vec::new();
    let mut fields = text.split('\0');
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        // A rename has an empty path here, then the old and new paths as the next two fields.
        let path = if path.is_empty() {
            let _old = fields.next();
            fields.next().unwrap_or_default().to_owned()
        } else {
            path.to_owned()
        };
        changes.push(FileChange {
            path,
            added: added.parse().ok(),
            removed: removed.parse().ok(),
        });
    }
    changes
}

/// The files changed from `base` to `head`, with line counts; renames are followed.
pub fn diff_stat(dir: &Path, base: &str, head: &str) -> Result<Vec<FileChange>> {
    let out = run_git(dir, ["diff", "--numstat", "-z", "-M", base, head])?;
    Ok(parse_numstat_z(&out))
}

/// The unified diff of one file from `base` to `head`.
pub fn diff_file(dir: &Path, base: &str, head: &str, path: &str) -> Result<String> {
    run_git(dir, ["diff", "-M", base, head, "--", path])
}

/// The files that would conflict when merging `theirs` into `ours`, found with `git merge-tree --write-tree`
/// without touching any worktree. Empty when the merge is clean.
pub fn merge_conflicts(dir: &Path, ours: &str, theirs: &str) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "-z",
            ours,
            theirs,
        ])
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|source| Error::Spawn {
            program: "git".into(),
            source,
        })?;
    match output.status.code() {
        Some(0) => Ok(Vec::new()),
        // Exit 1 is a conflicted merge: the tree id, then the conflicted paths.
        Some(1) => {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut names: Vec<String> = text
                .split('\0')
                .skip(1)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
            names.dedup();
            Ok(names)
        }
        _ => Err(Error::Command {
            program: "git".into(),
            args: format!("merge-tree --write-tree {ours} {theirs}"),
            dir: dir.to_path_buf(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        }),
    }
}

#[cfg(test)]
mod numstat_tests {
    use super::*;

    #[test]
    fn numstat_reads_counts_binaries_and_renames() {
        let text = "3\t1\tsrc/a.rs\x00-\t-\timg.png\x000\t0\t\x00old.rs\x00new.rs\x00";
        assert_eq!(
            parse_numstat_z(text),
            [
                FileChange {
                    path: "src/a.rs".into(),
                    added: Some(3),
                    removed: Some(1)
                },
                FileChange {
                    path: "img.png".into(),
                    added: None,
                    removed: None
                },
                FileChange {
                    path: "new.rs".into(),
                    added: Some(0),
                    removed: Some(0)
                },
            ]
        );
    }
}
