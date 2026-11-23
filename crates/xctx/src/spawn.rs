//! What runs in each pane of a space: finding `pi`, `lazygit` and the user's shell, and building the
//! [`TerminalConfig`] that starts them in the right worktree with the space's environment.
//!
//! Everything here is decided from a [`ProgramSearch`] rather than the process environment directly, so it can be
//! tested without touching the user's `PATH` or home directory.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use xctx_core::Space;
use xctx_layout::{PaneKind, PaneRole};
use xctx_term::TerminalConfig;

/// The shell used when `$SHELL` is unset or empty.
pub const DEFAULT_SHELL: &str = "/bin/zsh";

/// The agent program.
pub const AGENT_PROGRAM: &str = "pi";

/// The git program for review panes.
pub const GIT_PROGRAM: &str = "lazygit";

/// Where programs are looked up: the `PATH` directories, the home directory (for `~/.pi/agent/bin`) and the
/// login shell. A GUI app's `PATH` usually lacks Homebrew and user tool directories, so [`ProgramSearch::find`]
/// also tries [`ProgramSearch::extra_dirs`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgramSearch {
    /// The `PATH` directories, in order.
    pub path: Vec<PathBuf>,
    /// The user's home directory, if known.
    pub home: Option<PathBuf>,
    /// The value of `$SHELL`, if set.
    pub shell: Option<String>,
    /// Where package managers install programs, tried after the home directory's tool folders:
    /// [`SYSTEM_PROGRAM_DIRS`] outside tests.
    pub system_dirs: Vec<PathBuf>,
}

/// Where Homebrew and other package managers install programs.
pub const SYSTEM_PROGRAM_DIRS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

impl ProgramSearch {
    /// A search over this process's `PATH`, `HOME` and `SHELL`.
    pub fn from_env() -> ProgramSearch {
        ProgramSearch::from_values(
            std::env::var_os("PATH"),
            std::env::var_os("HOME"),
            std::env::var("SHELL").ok(),
        )
    }

    /// A search over explicit values of `PATH`, `HOME` and `SHELL`; empty values count as unset.
    pub fn from_values(
        path: Option<OsString>,
        home: Option<OsString>,
        shell: Option<String>,
    ) -> ProgramSearch {
        ProgramSearch {
            path: path
                .map(|p| {
                    std::env::split_paths(&p)
                        .filter(|d| !d.as_os_str().is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            home: home.filter(|h| !h.is_empty()).map(PathBuf::from),
            shell: shell.filter(|s| !s.is_empty()),
            system_dirs: SYSTEM_PROGRAM_DIRS.iter().map(PathBuf::from).collect(),
        }
    }

    /// The user's login shell: `$SHELL`, else [`DEFAULT_SHELL`].
    pub fn shell(&self) -> String {
        self.shell
            .clone()
            .unwrap_or_else(|| DEFAULT_SHELL.to_owned())
    }

    /// Directories tried after `PATH`, in order: `~/.pi/agent/bin`, `~/.local/bin`, then
    /// [`ProgramSearch::system_dirs`].
    pub fn extra_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(home) = &self.home {
            dirs.push(home.join(".pi").join("agent").join("bin"));
            dirs.push(home.join(".local").join("bin"));
        }
        dirs.extend(self.system_dirs.iter().cloned());
        dirs
    }

    /// The first executable file called `name` in `PATH`, then in [`ProgramSearch::extra_dirs`].
    pub fn find(&self, name: &str) -> Option<PathBuf> {
        self.path
            .iter()
            .cloned()
            .chain(self.extra_dirs())
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable(candidate))
    }

    /// The `PATH` panes run with: this search's `PATH`, then the directory `program` was found in and the
    /// [`ProgramSearch::extra_dirs`], without repeats. This lets a program found outside `PATH` (say `pi` in
    /// `~/.pi/agent/bin`, a node script) find its own interpreter and siblings.
    pub fn pane_path(&self, program: Option<&Path>) -> String {
        let mut dirs: Vec<PathBuf> = Vec::new();
        let program_dir = program.and_then(Path::parent).map(Path::to_path_buf);
        for dir in self
            .path
            .iter()
            .cloned()
            .chain(program_dir)
            .chain(self.extra_dirs())
        {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs.iter()
            .map(|d| d.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(":")
    }
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// How an agent pane starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentStart {
    /// A new session given the space's task: `pi -- <task>`. Used when a space is launched.
    Task(String),
    /// Resume the most recent session in the worktree: `pi --continue`. Used when a space is reopened, and for
    /// agent panes a reshape adds back.
    Continue,
}

/// What to start in a pane, and a notice for the user when it is not what the pane's role asked for (for example
/// a shell because `pi` is not installed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneLaunch {
    /// The program, arguments, working directory and environment.
    pub config: TerminalConfig,
    /// Why the pane runs something other than its role's program, for the status bar.
    pub notice: Option<String>,
}

/// The worktree a pane of `role` works in: its repo's worktree, else the space's first repo's.
pub fn pane_worktree<'a>(space: &'a Space, role: &PaneRole) -> Option<&'a Path> {
    space
        .repos
        .iter()
        .find(|r| r.name == role.repo)
        .or_else(|| space.repos.first())
        .map(|r| r.worktree.as_path())
}

/// The config that starts a pane of `role` in `space`: its program (`pi` for agents, `lazygit` for git panes,
/// the login shell for shells), run in the repo's worktree with the space's `PORT`, `XCTX_SPACE` and
/// `MISE_ENV`. A missing `pi` or `lazygit` falls back to the login shell with a notice.
pub fn pane_launch(
    space: &Space,
    role: &PaneRole,
    agent: &AgentStart,
    search: &ProgramSearch,
) -> PaneLaunch {
    let (program, args, notice) = match role.kind {
        PaneKind::Shell => (None, Vec::new(), None),
        PaneKind::Agent => match search.find(AGENT_PROGRAM) {
            Some(pi) => {
                let args = match agent {
                    AgentStart::Task(task) => vec!["--".to_owned(), task.clone()],
                    AgentStart::Continue => vec!["--continue".to_owned()],
                };
                (Some(pi), args, None)
            }
            None => (
                None,
                Vec::new(),
                Some(format!(
                    "{AGENT_PROGRAM} was not found on PATH, in ~/.pi/agent/bin, /opt/homebrew/bin or \
                     /usr/local/bin; opened a shell for the {} agent instead",
                    role.repo
                )),
            ),
        },
        PaneKind::Git => match search.find(GIT_PROGRAM) {
            Some(lazygit) => (Some(lazygit), Vec::new(), None),
            None => (
                None,
                Vec::new(),
                Some(format!(
                    "{GIT_PROGRAM} was not found; opened a shell for {} instead",
                    role.repo
                )),
            ),
        },
    };
    let mut config = match &program {
        Some(path) => TerminalConfig::new(path.to_string_lossy()).args(args),
        None => TerminalConfig::new(search.shell()).args(["-l"]),
    };
    if let Some(worktree) = pane_worktree(space, role) {
        config = config.cwd(worktree);
    }
    for (key, value) in space.env() {
        config = config.env(key, value);
    }
    config = config.env("PATH", search.pane_path(program.as_deref()));
    PaneLaunch { config, notice }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use xctx_core::{SpaceId, SpaceRepo, SpaceShape, SpaceStatus};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn space(root: &Path) -> Space {
        let repo = |name: &str| SpaceRepo {
            name: name.to_owned(),
            repo: root.join(name),
            worktree: root.join(format!("{name}-worktrees/fix-retry")),
            branch: "space/fix-retry".to_owned(),
            base_branch: "main".to_owned(),
            base_commit: "abc".to_owned(),
            branch_created: true,
            reason: "name matches".to_owned(),
        };
        Space {
            id: SpaceId::new("fix-retry"),
            title: "fix retry".to_owned(),
            task: "fix retry when we rate limit".to_owned(),
            shape: SpaceShape::Dev,
            status: SpaceStatus::Active,
            port: 4007,
            created_at: 0,
            repos: vec![repo("api"), repo("web")],
        }
    }

    fn role(kind: PaneKind, repo: &str) -> PaneRole {
        PaneRole {
            kind,
            repo: repo.to_owned(),
        }
    }

    fn executable(path: &Path) -> TestResult {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, "#!/bin/sh\n")?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    fn env<'a>(launch: &'a PaneLaunch, key: &str) -> Option<&'a str> {
        launch.config.env.get(key).map(String::as_str)
    }

    /// A search whose only directories are under `root`, so the machine's own tools never leak in.
    fn search(root: &Path) -> ProgramSearch {
        ProgramSearch {
            path: vec![root.join("bin")],
            home: Some(root.join("home")),
            shell: Some("/bin/fish".to_owned()),
            system_dirs: Vec::new(),
        }
    }

    fn assert_space_env(launch: &PaneLaunch, cwd: &Path) {
        assert_eq!(env(launch, "MISE_ENV"), Some("fix-retry"));
        assert_eq!(env(launch, "PORT"), Some("4007"));
        assert_eq!(env(launch, "XCTX_SPACE"), Some("fix-retry"));
        assert_eq!(env(launch, "TERM"), Some("xterm-256color"));
        assert_eq!(launch.config.cwd.as_deref(), Some(cwd));
    }

    #[test]
    fn values_ignore_empty_entries() {
        let search = ProgramSearch::from_values(Some("/a::/b".into()), Some("".into()), None);
        assert_eq!(search.path, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(search.home, None);
        assert_eq!(search.shell(), DEFAULT_SHELL);
        assert_eq!(search.extra_dirs().len(), SYSTEM_PROGRAM_DIRS.len());
        let search = ProgramSearch::from_values(None, None, Some(String::new()));
        assert_eq!(search.shell(), DEFAULT_SHELL);
    }

    #[test]
    fn programs_are_found_on_path_before_the_extra_dirs() -> TestResult {
        let root = tempfile::tempdir()?;
        let search = search(root.path());
        assert_eq!(search.find("pi"), None);
        let in_pi_bin = root.path().join("home/.pi/agent/bin/pi");
        executable(&in_pi_bin)?;
        assert_eq!(search.find("pi"), Some(in_pi_bin));
        let on_path = root.path().join("bin/pi");
        executable(&on_path)?;
        assert_eq!(search.find("pi"), Some(on_path));
        Ok(())
    }

    #[test]
    fn files_that_are_not_executable_are_skipped() -> TestResult {
        let root = tempfile::tempdir()?;
        let search = search(root.path());
        fs::create_dir_all(root.path().join("bin"))?;
        fs::write(root.path().join("bin/lazygit"), "")?;
        fs::create_dir_all(root.path().join("bin/pi"))?;
        assert_eq!(search.find("lazygit"), None);
        assert_eq!(search.find("pi"), None);
        Ok(())
    }

    #[test]
    fn a_fresh_agent_runs_pi_with_the_task_in_its_worktree() -> TestResult {
        let root = tempfile::tempdir()?;
        let pi = root.path().join("home/.pi/agent/bin/pi");
        executable(&pi)?;
        let space = space(root.path());
        let task = AgentStart::Task(space.task.clone());
        let launch = pane_launch(
            &space,
            &role(PaneKind::Agent, "web"),
            &task,
            &search(root.path()),
        );
        assert_eq!(launch.notice, None);
        assert_eq!(launch.config.program, pi.to_string_lossy());
        assert_eq!(launch.config.args, ["--", "fix retry when we rate limit"]);
        assert_space_env(&launch, &space.repos[1].worktree);
        let path = env(&launch, "PATH").unwrap_or_default().to_owned();
        let pi_dir = root.path().join("home/.pi/agent/bin");
        assert!(
            path.starts_with(&format!("{}:", root.path().join("bin").display())),
            "{path}"
        );
        assert!(
            path.contains(&pi_dir.to_string_lossy().into_owned()),
            "{path}"
        );
        Ok(())
    }

    #[test]
    fn a_restored_agent_continues_its_session() -> TestResult {
        let root = tempfile::tempdir()?;
        executable(&root.path().join("bin/pi"))?;
        let space = space(root.path());
        let launch = pane_launch(
            &space,
            &role(PaneKind::Agent, "api"),
            &AgentStart::Continue,
            &search(root.path()),
        );
        assert_eq!(launch.config.args, ["--continue"]);
        assert_space_env(&launch, &space.repos[0].worktree);
        Ok(())
    }

    #[test]
    fn a_missing_pi_opens_the_login_shell_with_a_notice() -> TestResult {
        let root = tempfile::tempdir()?;
        let space = space(root.path());
        let launch = pane_launch(
            &space,
            &role(PaneKind::Agent, "api"),
            &AgentStart::Task("x".into()),
            &search(root.path()),
        );
        assert_eq!(launch.config.program, "/bin/fish");
        assert_eq!(launch.config.args, ["-l"]);
        assert!(launch.notice.as_deref().unwrap_or_default().contains("pi"));
        assert_space_env(&launch, &space.repos[0].worktree);
        Ok(())
    }

    #[test]
    fn shells_run_the_login_shell_in_the_worktree() -> TestResult {
        let root = tempfile::tempdir()?;
        let space = space(root.path());
        let launch = pane_launch(
            &space,
            &role(PaneKind::Shell, "web"),
            &AgentStart::Continue,
            &search(root.path()),
        );
        assert_eq!(launch.notice, None);
        assert_eq!(launch.config.program, "/bin/fish");
        assert_eq!(launch.config.args, ["-l"]);
        assert_space_env(&launch, &space.repos[1].worktree);
        let no_shell = ProgramSearch {
            shell: None,
            ..search(root.path())
        };
        let launch = pane_launch(
            &space,
            &role(PaneKind::Shell, "web"),
            &AgentStart::Continue,
            &no_shell,
        );
        assert_eq!(launch.config.program, DEFAULT_SHELL);
        Ok(())
    }

    #[test]
    fn git_panes_run_lazygit_or_fall_back_to_the_shell() -> TestResult {
        let root = tempfile::tempdir()?;
        let space = space(root.path());
        let git = role(PaneKind::Git, "api");
        let launch = pane_launch(&space, &git, &AgentStart::Continue, &search(root.path()));
        assert_eq!(launch.config.program, "/bin/fish");
        assert!(
            launch
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("lazygit")
        );
        let lazygit = root.path().join("bin/lazygit");
        executable(&lazygit)?;
        let launch = pane_launch(&space, &git, &AgentStart::Continue, &search(root.path()));
        assert_eq!(launch.notice, None);
        assert_eq!(launch.config.program, lazygit.to_string_lossy());
        assert!(launch.config.args.is_empty());
        assert_space_env(&launch, &space.repos[0].worktree);
        Ok(())
    }

    #[test]
    fn an_unknown_repo_works_in_the_first_worktree() {
        let space = space(Path::new("/code"));
        assert_eq!(
            pane_worktree(&space, &role(PaneKind::Shell, "gone")),
            Some(space.repos[0].worktree.as_path())
        );
    }

    #[test]
    fn pane_path_keeps_order_and_drops_repeats() {
        let search = ProgramSearch {
            path: vec![
                PathBuf::from("/usr/bin"),
                PathBuf::from("/opt/homebrew/bin"),
            ],
            home: Some(PathBuf::from("/Users/me")),
            shell: None,
            system_dirs: SYSTEM_PROGRAM_DIRS.iter().map(PathBuf::from).collect(),
        };
        assert_eq!(
            search.pane_path(Some(Path::new("/Users/me/.pi/agent/bin/pi"))),
            "/usr/bin:/opt/homebrew/bin:/Users/me/.pi/agent/bin:/Users/me/.local/bin:/usr/local/bin"
        );
    }
}
