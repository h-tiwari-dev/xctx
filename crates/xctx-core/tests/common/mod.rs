//! Helpers shared by the integration tests: temporary repositories and a git runner that commits without the
//! user's identity or signing config.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A temporary directory with its canonical path (on macOS `/var` is a link to `/private/var`, and git reports
/// the resolved path).
pub struct TempRoot {
    _dir: tempfile::TempDir,
    pub path: PathBuf,
}

pub fn temp_root() -> TestResult<TempRoot> {
    let dir = tempfile::tempdir()?;
    let path = fs::canonicalize(dir.path())?;
    Ok(TempRoot { _dir: dir, path })
}

/// Run git in `dir` with a throwaway identity and no signing; panics with git's stderr on failure.
pub fn git(dir: &Path, args: &[&str]) -> TestResult<String> {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=xctx test",
            "-c",
            "user.email=test@xctx.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Make a repository at `dir` with `files` committed on `main`.
pub fn make_repo(dir: &Path, files: &[(&str, &str)]) -> TestResult {
    for (name, text) in files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, text)?;
    }
    fs::create_dir_all(dir)?;
    git(dir, &["init", "-q", "-b", "main"])?;
    git(dir, &["add", "."])?;
    git(dir, &["commit", "-q", "-m", "first commit"])?;
    Ok(())
}

/// Commit a new file in `dir`.
pub fn commit_file(dir: &Path, name: &str, text: &str, message: &str) -> TestResult {
    fs::write(dir.join(name), text)?;
    git(dir, &["add", name])?;
    git(dir, &["commit", "-q", "-m", message])?;
    Ok(())
}

/// Under `root/code`: `payments` and `web/checkout-web` repositories, and `notes`, a folder that is not one.
pub struct Repos {
    pub code: PathBuf,
    pub payments: PathBuf,
    pub checkout_web: PathBuf,
    pub notes: PathBuf,
}

pub fn make_repos(root: &Path) -> TestResult<Repos> {
    let code = root.join("code");
    let payments = code.join("payments");
    let checkout_web = code.join("web").join("checkout-web");
    let notes = code.join("notes");
    make_repo(
        &payments,
        &[
            (
                "README.md",
                "# Payments\n\nPayments service: charges carts.\n",
            ),
            ("src/limit.ts", ""),
            ("src/charge.ts", ""),
        ],
    )?;
    make_repo(
        &checkout_web,
        &[
            ("README.md", "Checkout frontend\n"),
            ("src/Checkout.tsx", ""),
        ],
    )?;
    fs::create_dir_all(&notes)?;
    fs::write(notes.join("todo.txt"), "checkout")?;
    Ok(Repos {
        code,
        payments,
        checkout_web,
        notes,
    })
}

/// A bare clone of `repo` at `dir`, added to `repo` as `origin` with its HEAD known.
pub fn add_bare_remote(repo: &Path, dir: &Path) -> TestResult {
    let dir_str = dir.to_string_lossy();
    let repo_str = repo.to_string_lossy();
    let parent = dir.parent().ok_or("remote dir has no parent")?;
    git(parent, &["clone", "-q", "--bare", &repo_str, &dir_str])?;
    git(repo, &["remote", "add", "origin", &dir_str])?;
    git(repo, &["fetch", "-q", "origin"])?;
    git(repo, &["remote", "set-head", "origin", "-a"])?;
    Ok(())
}

/// A stand-in for `gh` that records its arguments in `gh-args` next to it and prints a pull request URL.
pub fn fake_gh(dir: &Path) -> TestResult<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("gh");
    let args = dir.join("gh-args");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\necho 'Creating pull request...' >&2\necho https://github.com/acme/repo/pull/7\n",
            args.display()
        ),
    )?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    Ok(path)
}

/// A stand-in for `gh` that always fails.
pub fn failing_gh(dir: &Path) -> TestResult<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("gh-failing");
    fs::write(
        &path,
        "#!/bin/sh\necho 'no GitHub remote found' >&2\nexit 1\n",
    )?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    Ok(path)
}
