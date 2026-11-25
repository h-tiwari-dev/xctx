//! Where a space's repo lives on the web: the page of its git remote (GitHub, GitLab or any host reached over
//! https or ssh), shown in agent pane headers so a pane says which repo it works on and opens it in one click.

use std::path::Path;

/// The web page of git remote `remote`: `git@github.com:me/app.git`, `ssh://git@github.com/me/app.git` and
/// `https://github.com/me/app.git` all give `https://github.com/me/app`. Credentials in an https remote are
/// dropped. `None` for a local path or anything that is not a host and a path.
pub fn web_url(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let (host, path) = if let Some(rest) = remote
        .strip_prefix("https://")
        .or_else(|| remote.strip_prefix("http://"))
    {
        let (host, path) = rest.split_once('/')?;
        (host.rsplit('@').next()?, path)
    } else if let Some(rest) = remote
        .strip_prefix("ssh://")
        .or_else(|| remote.strip_prefix("git+ssh://"))
    {
        let (host, path) = rest.split_once('/')?;
        let host = host.rsplit('@').next()?;
        // `ssh://host:2222/path`: the port is not part of the web address.
        (host.split(':').next()?, path)
    } else {
        // scp-like `user@host:path`; a plain local path has no `:` before its first `/`.
        let (host, path) = remote.split_once(':')?;
        if host.contains('/') || path.starts_with("//") {
            return None;
        }
        (host.rsplit('@').next()?, path)
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("https://{host}/{path}"))
}

/// The web page of the repo at `repo` (its main checkout or a worktree): its first remote's (see
/// [`xctx_core::git::remote_name`]) [`web_url`]. `None` when it has no remote or git cannot be run.
pub fn repo_web_url(repo: &Path) -> Option<String> {
    let remote = xctx_core::git::remote_name(repo).ok()??;
    let url = xctx_core::git::remote_url(repo, &remote).ok()?;
    web_url(&url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes_become_their_web_page() {
        for remote in [
            "git@github.com:me/app.git",
            "ssh://git@github.com/me/app.git",
            "ssh://git@github.com:22/me/app",
            "https://github.com/me/app.git",
            "https://token:x-oauth@github.com/me/app",
            "https://github.com/me/app/",
        ] {
            assert_eq!(
                web_url(remote).as_deref(),
                Some("https://github.com/me/app"),
                "{remote}"
            );
        }
        assert_eq!(
            web_url("git@gitlab.com:group/sub/app.git").as_deref(),
            Some("https://gitlab.com/group/sub/app")
        );
    }

    #[test]
    fn local_and_odd_remotes_have_no_page() {
        for remote in [
            "/srv/git/app.git",
            "../app",
            "file:///srv/app.git",
            "",
            "https://github.com",
        ] {
            assert_eq!(web_url(remote), None, "{remote}");
        }
    }
}
