//! What runs in each pane of a space: finding `pi`, `lazygit` and the user's shell, and building the
//! [`TerminalConfig`] that starts them in the right worktree with the space's environment.
//!
//! Everything here is decided from a [`ProgramSearch`] rather than the process environment directly, so it can be
//! tested without touching the user's `PATH` or home directory. The one exception is the scratch directory, which
//! follows `XCTX_DATA_DIR` (see [`scratch_dir_for`]).

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use xctx_core::Space;
use xctx_layout::{PaneId, PaneKind, PaneRole, RolePane};
use xctx_term::TerminalConfig;

use crate::agent_session::{AgentSession, agent_args};
use crate::space_graph::scratch_dir_for;

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
    /// The window's control socket and where `xctx-ctl` is, given to every pane; `None` when the socket is not
    /// serving.
    pub control: Option<ControlEnv>,
}

/// What panes need to drive the window: `XCTX_CONTROL_SOCKET`, and `xctx-ctl` on their `PATH`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlEnv {
    /// The control socket, exported as [`xctx_control::CONTROL_SOCKET_ENV`].
    pub socket: PathBuf,
    /// The directory holding `xctx-ctl`, put first on panes' `PATH`; `None` when it was not found.
    pub ctl_dir: Option<PathBuf>,
}

/// The name of the control CLI, installed next to the `xctx` executable.
pub const CTL_PROGRAM: &str = "xctx-ctl";

/// The directory of `xctx-ctl` when it sits next to `exe` (the running `xctx`): `target/<profile>/` in
/// development, `xctx.app/Contents/MacOS/` in the bundle.
pub fn ctl_dir_next_to(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    is_executable(&dir.join(CTL_PROGRAM)).then(|| dir.to_path_buf())
}

/// Where Homebrew and other package managers install programs.
pub const SYSTEM_PROGRAM_DIRS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

/// How long [`login_shell_path`] waits for the login shell before giving up.
pub const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(2);

/// The `PATH` the user's login shell `shell` sets up, read by running `shell -l -c 'printf %s "$PATH"'`. An app
/// started from Finder or the Dock inherits launchd's bare `PATH`; this is where Homebrew and version managers add
/// theirs. `None` when the shell cannot be run, fails, prints no usable `PATH`, or takes longer than `timeout` (it
/// is then killed). Blocks for up to `timeout`.
pub fn login_shell_path(shell: &str, timeout: Duration) -> Option<OsString> {
    let mut child = Command::new(shell)
        .args(["-l", "-c", "printf %s \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    let output = match child.stdout.take() {
        Some(mut stdout) => {
            // Read on a thread so a shell that never closes its output cannot block past the deadline.
            let (sender, receiver) = mpsc::channel();
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = sender.send(stdout.read_to_end(&mut bytes).map(|_| bytes));
            });
            receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .ok()
                .and_then(Result::ok)
        }
        None => None,
    };
    let succeeded = output.is_some() && wait_until(&mut child, deadline);
    if !succeeded {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    output.and_then(|bytes| parse_login_path(&bytes))
}

/// Whether `child` exits successfully before `deadline`.
fn wait_until(child: &mut std::process::Child, deadline: Instant) -> bool {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => return false,
        }
    }
}

/// The `PATH` in what a login shell printed: its last line (start-up files may print before it), when that line
/// names at least one absolute directory.
pub fn parse_login_path(output: &[u8]) -> Option<OsString> {
    let last = output
        .rsplit(|&byte| byte == b'\n')
        .find(|line| !line.is_empty())?;
    let path = OsString::from_vec(last.to_vec());
    std::env::split_paths(&path)
        .any(|dir| dir.is_absolute())
        .then_some(path)
}

/// `login`'s directories, then those of `inherited` that `login` lacks: the login shell's `PATH` wins, and what the
/// process was started with (say, a terminal's activated tool directories) is kept after it.
pub fn merge_paths(login: Option<OsString>, inherited: Option<OsString>) -> Option<OsString> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for path in [login, inherited].into_iter().flatten() {
        for dir in std::env::split_paths(&path) {
            if !dir.as_os_str().is_empty() && !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    if dirs.is_empty() {
        return None;
    }
    std::env::join_paths(dirs).ok()
}

impl ProgramSearch {
    /// A search over this process's `HOME` and `SHELL`, and a `PATH` of the login shell's `PATH` (see
    /// [`login_shell_path`]) followed by this process's own. Runs the login shell, so it blocks for up to
    /// [`LOGIN_SHELL_TIMEOUT`]: call it once, at startup.
    pub fn from_login_shell() -> ProgramSearch {
        let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty());
        let login = login_shell_path(
            shell.as_deref().unwrap_or(DEFAULT_SHELL),
            LOGIN_SHELL_TIMEOUT,
        );
        ProgramSearch::from_values(
            merge_paths(login, std::env::var_os("PATH")),
            std::env::var_os("HOME"),
            shell,
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
            control: None,
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

    /// The `PATH` panes run with: the `xctx-ctl` directory when the control socket is serving, this search's `PATH`,
    /// then the directory `program` was found in and the
    /// [`ProgramSearch::extra_dirs`], without repeats. This lets a program found outside `PATH` (say `pi` in
    /// `~/.pi/agent/bin`, a node script) find its own interpreter and siblings.
    pub fn pane_path(&self, program: Option<&Path>) -> String {
        let mut dirs: Vec<PathBuf> = Vec::new();
        let program_dir = program.and_then(Path::parent).map(Path::to_path_buf);
        let ctl_dir = self.control.as_ref().and_then(|c| c.ctl_dir.clone());
        for dir in ctl_dir
            .into_iter()
            .chain(self.path.iter().cloned())
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
    /// A new session with no task: plain `pi`. Used for an agent pane split into a worktree another agent pane
    /// already works in, so two `pi` processes never write to one session.
    Fresh,
}

/// How an agent pane of `new` role added by a split starts in `space`: [`AgentStart::Fresh`] when it runs a
/// new lane of the space's graph (`new_lane`, such as a helper added from the graph panel), which has no session
/// of its own, or when one of `others` (the space's other panes) is an agent in the same worktree, whose session
/// `pi --continue` would resume; else [`AgentStart::Continue`].
pub fn split_agent_start<'a>(
    space: &Space,
    new: &PaneRole,
    others: impl IntoIterator<Item = &'a PaneRole>,
    new_lane: bool,
) -> AgentStart {
    if new_lane {
        return AgentStart::Fresh;
    }
    let worktree = pane_worktree(space, new);
    let shared = others
        .into_iter()
        .any(|role| role.kind == PaneKind::Agent && pane_worktree(space, role) == worktree);
    if shared {
        AgentStart::Fresh
    } else {
        AgentStart::Continue
    }
}

/// How each of `panes` starts when space `space` is reopened, in order. In each worktree one agent pane resumes
/// the worktree's last session (`pi --continue`): the first of `preferred` (the panes bound to the repos'
/// writers) working there, else the first agent pane there. Every other agent pane in that worktree starts
/// [`AgentStart::Fresh`], so two `pi` processes never resume one session. Panes that are not agents get
/// [`AgentStart::Continue`], which they ignore.
pub fn restore_agent_starts(
    space: &Space,
    panes: &[RolePane],
    preferred: &[PaneId],
) -> Vec<AgentStart> {
    let agents = || panes.iter().filter(|p| p.role.kind == PaneKind::Agent);
    let mut resuming: HashMap<Option<&Path>, PaneId> = HashMap::new();
    for pane in agents().filter(|p| preferred.contains(&p.id)) {
        resuming
            .entry(pane_worktree(space, &pane.role))
            .or_insert(pane.id);
    }
    for pane in agents() {
        resuming
            .entry(pane_worktree(space, &pane.role))
            .or_insert(pane.id);
    }
    panes
        .iter()
        .map(|pane| {
            let resumes = resuming.get(&pane_worktree(space, &pane.role)) == Some(&pane.id);
            if pane.role.kind != PaneKind::Agent || resumes {
                AgentStart::Continue
            } else {
                AgentStart::Fresh
            }
        })
        .collect()
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

/// The environment variable naming the agent a pane runs as, by its id in the space's graph.
pub const AGENT_ID_ENV: &str = "XCTX_AGENT_ID";

/// The environment variable naming the role of the agent a pane runs as: `orchestrator`, `writer` or
/// `helper:<kind>`.
pub const AGENT_ROLE_ENV: &str = "XCTX_AGENT_ROLE";

/// The environment variable naming the space's scratch directory (see [`scratch_dir_for`]), which is removed on
/// Ship or Discard.
pub const SCRATCH_DIR_ENV: &str = "XCTX_SCRATCH_DIR";

/// The environment variable naming the attempt an attempt's pane works on, by its number in the space; `xctx-ctl`
/// takes `attempt.report`'s `attempt=` from it. Empty in every other pane.
pub const ATTEMPT_ID_ENV: &str = "XCTX_ATTEMPT_ID";

/// The environment variable holding an attempt's report token (see `xctx_attempt::Attempt::token`), which
/// `xctx-ctl` sends with `attempt.report`. Set only in that attempt's panes; empty in every other pane.
pub const ATTEMPT_TOKEN_ENV: &str = xctx_control::cli::ATTEMPT_TOKEN_ENV;

/// Whether an agent pane that was started from `launch` runs `pi` itself, so the session it was given may be
/// recorded on its agent: the terminal `spawned`, and `pi` was found (no fallback shell, which has a notice).
pub fn runs_pi(kind: PaneKind, launch: &PaneLaunch, spawned: bool) -> bool {
    kind == PaneKind::Agent && spawned && launch.notice.is_none()
}

/// The agent of a space's graph a pane runs as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneAgent {
    /// Its id in the graph, given to the pane as [`AGENT_ID_ENV`].
    pub id: String,
    /// Its role, given to the pane as [`AGENT_ROLE_ENV`].
    pub role: String,
}

impl PaneLaunch {
    /// The same launch, run as `agent` when there is one: [`AGENT_ID_ENV`] and [`AGENT_ROLE_ENV`] are set. Without
    /// one they stay empty, as [`pane_launch`] sets them.
    pub fn with_agent(mut self, agent: Option<&PaneAgent>) -> PaneLaunch {
        if let Some(agent) = agent {
            self.config = self
                .config
                .env(AGENT_ID_ENV, agent.id.clone())
                .env(AGENT_ROLE_ENV, agent.role.clone());
        }
        self
    }
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
/// `MISE_ENV`, the space's scratch directory as [`SCRATCH_DIR_ENV`], and `XCTX_CONTROL_SOCKET` when the control
/// socket is serving. A missing `pi` or `lazygit` falls back to the login shell with a notice. [`AGENT_ID_ENV`],
/// [`AGENT_ROLE_ENV`] and [`ATTEMPT_ID_ENV`] are set empty, so the agent identity of an xctx this one runs inside
/// never leaks into its panes; the agent a pane runs as is added with [`PaneLaunch::with_agent`]. The agent runs
/// no session of xctx's naming; see [`pane_launch_with`].
#[cfg_attr(not(test), allow(dead_code))]
pub fn pane_launch(
    space: &Space,
    role: &PaneRole,
    agent: &AgentStart,
    search: &ProgramSearch,
) -> PaneLaunch {
    pane_launch_with(space, role, agent, &AgentSession::Unnamed, search)
}

/// [`pane_launch`], with an agent pane's `pi` running `session` (see [`agent_args`]).
pub fn pane_launch_with(
    space: &Space,
    role: &PaneRole,
    agent: &AgentStart,
    session: &AgentSession,
    search: &ProgramSearch,
) -> PaneLaunch {
    let (program, args, notice) = match role.kind {
        PaneKind::Shell => (None, Vec::new(), None),
        PaneKind::Agent => match search.find(AGENT_PROGRAM) {
            Some(pi) => (Some(pi), agent_args(agent, session), None),
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
    config = config
        .env(AGENT_ID_ENV, "")
        .env(AGENT_ROLE_ENV, "")
        .env(ATTEMPT_ID_ENV, "")
        .env(ATTEMPT_TOKEN_ENV, "");
    if let Ok(scratch) = scratch_dir_for(space.id.as_str()) {
        config = config.env(SCRATCH_DIR_ENV, scratch.root().to_string_lossy());
    }
    if let Some(control) = &search.control {
        config = config.env(
            xctx_control::CONTROL_SOCKET_ENV,
            control.socket.to_string_lossy(),
        );
    }
    config = config.env("PATH", search.pane_path(program.as_deref()));
    PaneLaunch { config, notice }
}

/// What an attempt's agent pane runs: which attempt, where, as which agent and in which session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptPane {
    /// The attempt's number, exported as [`ATTEMPT_ID_ENV`].
    pub number: u32,
    /// The space repo it is a copy of.
    pub repo: String,
    /// Its worktree, where `pi` runs.
    pub worktree: PathBuf,
    /// Its own `PORT`.
    pub port: u16,
    /// The agent it runs as, exported as [`AGENT_ID_ENV`] and [`AGENT_ROLE_ENV`]: its own id
    /// (`<writer>#attempt-<n>`, see [`crate::attempts::attempt_agent_id`]) with its writer's role.
    pub agent: PaneAgent,
    /// Its report token, exported as [`ATTEMPT_TOKEN_ENV`].
    pub token: String,
    /// Its `pi` session: a fork of the writer's the first time, its own afterwards.
    pub session: AgentSession,
    /// What it was told to try, sent as the first message when its session is forked.
    pub instruction: Option<String>,
    /// Its scratch directory, exported as [`SCRATCH_DIR_ENV`]; the space's when `None`.
    pub scratch: Option<PathBuf>,
}

/// The config that starts an attempt's agent: as [`pane_launch_with`] for an agent in the attempt's repo, but run
/// in the attempt's worktree with its own `PORT`, scratch directory, [`ATTEMPT_ID_ENV`] and [`ATTEMPT_TOKEN_ENV`],
/// as its own agent. Its instruction is the first message only when the session is being forked.
pub fn attempt_launch(space: &Space, attempt: &AttemptPane, search: &ProgramSearch) -> PaneLaunch {
    let role = PaneRole {
        kind: PaneKind::Agent,
        repo: attempt.repo.clone(),
    };
    let start = match (&attempt.session, &attempt.instruction) {
        (AgentSession::Fork { .. }, Some(instruction)) => AgentStart::Task(instruction.clone()),
        _ => AgentStart::Fresh,
    };
    let mut launch = pane_launch_with(space, &role, &start, &attempt.session, search)
        .with_agent(Some(&attempt.agent));
    launch.config = launch
        .config
        .cwd(&attempt.worktree)
        .env("PORT", attempt.port.to_string())
        .env(ATTEMPT_ID_ENV, attempt.number.to_string())
        .env(ATTEMPT_TOKEN_ENV, attempt.token.clone());
    if let Some(scratch) = &attempt.scratch {
        launch.config = launch
            .config
            .env(SCRATCH_DIR_ENV, scratch.to_string_lossy());
    }
    launch
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
            production: false,
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
            control: None,
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
    fn a_fresh_agent_runs_plain_pi_without_a_session_flag() -> TestResult {
        let root = tempfile::tempdir()?;
        let pi = root.path().join("bin/pi");
        executable(&pi)?;
        let space = space(root.path());
        let launch = pane_launch(
            &space,
            &role(PaneKind::Agent, "web"),
            &AgentStart::Fresh,
            &search(root.path()),
        );
        assert_eq!(launch.notice, None);
        assert_eq!(launch.config.program, pi.to_string_lossy());
        assert!(launch.config.args.is_empty(), "{:?}", launch.config.args);
        assert_space_env(&launch, &space.repos[1].worktree);
        Ok(())
    }

    #[test]
    fn a_split_agent_starts_fresh_beside_another_agent_in_its_worktree() {
        let space = space(Path::new("/code"));
        let new = role(PaneKind::Agent, "api");
        let others = [role(PaneKind::Agent, "api"), role(PaneKind::Shell, "web")];
        assert_eq!(
            split_agent_start(&space, &new, &others, false),
            AgentStart::Fresh
        );
        let elsewhere = [role(PaneKind::Agent, "web"), role(PaneKind::Shell, "api")];
        assert_eq!(
            split_agent_start(&space, &new, &elsewhere, false),
            AgentStart::Continue
        );
        assert_eq!(
            split_agent_start(&space, &new, &[], false),
            AgentStart::Continue
        );
    }

    #[test]
    fn a_helper_pane_always_starts_fresh() {
        let space = space(Path::new("/code"));
        let new = role(PaneKind::Agent, "api");
        // Even alone in its worktree, a new lane never resumes another agent's session.
        assert_eq!(
            split_agent_start(&space, &new, &[], true),
            AgentStart::Fresh
        );
        let elsewhere = [role(PaneKind::Agent, "web")];
        assert_eq!(
            split_agent_start(&space, &new, &elsewhere, true),
            AgentStart::Fresh
        );
    }

    fn role_pane(id: u64, kind: PaneKind, repo: &str) -> RolePane {
        RolePane {
            id: PaneId(id),
            role: role(kind, repo),
        }
    }

    #[test]
    fn a_reopened_space_resumes_one_agent_per_worktree() {
        let space = space(Path::new("/code"));
        let panes = [
            role_pane(1, PaneKind::Agent, "api"),
            role_pane(2, PaneKind::Shell, "api"),
            role_pane(3, PaneKind::Agent, "api"),
            role_pane(4, PaneKind::Agent, "web"),
            role_pane(5, PaneKind::Agent, "web"),
            role_pane(6, PaneKind::Git, "web"),
        ];
        use AgentStart::{Continue, Fresh};
        assert_eq!(
            restore_agent_starts(&space, &panes, &[]),
            [Continue, Continue, Fresh, Continue, Fresh, Continue]
        );
        // The pane bound to a repo's writer resumes, not the first agent pane in its worktree.
        assert_eq!(
            restore_agent_starts(&space, &panes, &[PaneId(3), PaneId(4)]),
            [Fresh, Continue, Continue, Continue, Fresh, Continue]
        );
        // A pane in an unknown repo works in the first worktree, so it shares api's session.
        let unknown = [
            role_pane(1, PaneKind::Agent, "api"),
            role_pane(2, PaneKind::Agent, "gone"),
        ];
        assert_eq!(
            restore_agent_starts(&space, &unknown, &[]),
            [Continue, Fresh]
        );
    }

    #[test]
    fn panes_that_run_no_agent_get_an_empty_agent_identity() -> TestResult {
        let root = tempfile::tempdir()?;
        executable(&root.path().join("bin/pi"))?;
        executable(&root.path().join("bin/lazygit"))?;
        let space = space(root.path());
        let search = search(root.path());
        for kind in [PaneKind::Shell, PaneKind::Git, PaneKind::Agent] {
            let launch = pane_launch(&space, &role(kind, "api"), &AgentStart::Fresh, &search)
                .with_agent(None);
            // Set, not missing, so an outer xctx's XCTX_AGENT_ID never reaches the pane.
            assert_eq!(env(&launch, AGENT_ID_ENV), Some(""), "{kind:?}");
            assert_eq!(env(&launch, AGENT_ROLE_ENV), Some(""), "{kind:?}");
            assert_eq!(env(&launch, ATTEMPT_TOKEN_ENV), Some(""), "{kind:?}");
        }
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
        // The shell it fell back to runs no session, so none is recorded for the agent.
        assert!(!runs_pi(PaneKind::Agent, &launch, true));
        assert_space_env(&launch, &space.repos[0].worktree);
        executable(&root.path().join("bin/pi"))?;
        let found = pane_launch(
            &space,
            &role(PaneKind::Agent, "api"),
            &AgentStart::Task("x".into()),
            &search(root.path()),
        );
        assert!(runs_pi(PaneKind::Agent, &found, true));
        assert!(!runs_pi(PaneKind::Agent, &found, false));
        assert!(!runs_pi(PaneKind::Shell, &found, true));
        Ok(())
    }

    #[test]
    fn panes_get_the_control_socket_and_xctx_ctl_first_on_path() -> TestResult {
        let root = tempfile::tempdir()?;
        let space = space(root.path());
        let ctl_dir = root.path().join("app");
        fs::create_dir_all(&ctl_dir)?;
        executable(&ctl_dir.join(CTL_PROGRAM))?;
        assert_eq!(
            ctl_dir_next_to(&ctl_dir.join("xctx")),
            Some(ctl_dir.clone())
        );
        assert_eq!(ctl_dir_next_to(&root.path().join("xctx")), None);

        let mut search = search(root.path());
        let plain = pane_launch(
            &space,
            &role(PaneKind::Shell, "web"),
            &AgentStart::Continue,
            &search,
        );
        assert_eq!(env(&plain, xctx_control::CONTROL_SOCKET_ENV), None);

        search.control = Some(ControlEnv {
            socket: root.path().join("control.sock"),
            ctl_dir: Some(ctl_dir.clone()),
        });
        let launch = pane_launch(
            &space,
            &role(PaneKind::Shell, "web"),
            &AgentStart::Continue,
            &search,
        );
        let socket = root.path().join("control.sock");
        assert_eq!(
            env(&launch, xctx_control::CONTROL_SOCKET_ENV),
            socket.to_str()
        );
        let path = env(&launch, "PATH").unwrap_or_default();
        assert!(
            path.starts_with(&format!("{}:", ctl_dir.display())),
            "{path}"
        );
        assert_space_env(&launch, &space.repos[1].worktree);
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
    fn the_login_path_is_the_last_line_the_shell_printed() {
        assert_eq!(
            parse_login_path(b"/opt/homebrew/bin:/usr/bin"),
            Some(OsString::from("/opt/homebrew/bin:/usr/bin"))
        );
        assert_eq!(
            parse_login_path(b"welcome back\n/usr/local/bin:/bin\n"),
            Some(OsString::from("/usr/local/bin:/bin"))
        );
        assert_eq!(parse_login_path(b""), None);
        assert_eq!(parse_login_path(b"\n\n"), None);
        assert_eq!(parse_login_path(b"not a path"), None);
    }

    #[test]
    fn the_login_path_comes_first_and_the_inherited_one_fills_in() {
        assert_eq!(
            merge_paths(
                Some("/opt/homebrew/bin:/usr/bin".into()),
                Some("/usr/bin:/Users/me/.cargo/bin:".into())
            ),
            Some(OsString::from(
                "/opt/homebrew/bin:/usr/bin:/Users/me/.cargo/bin"
            ))
        );
        assert_eq!(
            merge_paths(None, Some("/usr/bin:/bin".into())),
            Some(OsString::from("/usr/bin:/bin"))
        );
        assert_eq!(
            merge_paths(Some("/a".into()), None),
            Some(OsString::from("/a"))
        );
        assert_eq!(merge_paths(None, None), None);
        assert_eq!(merge_paths(Some("".into()), Some("".into())), None);
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
            control: None,
        };
        assert_eq!(
            search.pane_path(Some(Path::new("/Users/me/.pi/agent/bin/pi"))),
            "/usr/bin:/opt/homebrew/bin:/Users/me/.pi/agent/bin:/Users/me/.local/bin:/usr/local/bin"
        );
    }

    #[test]
    fn agents_open_their_named_session_and_attempts_fork_in_their_worktree() -> TestResult {
        let root = tempfile::tempdir()?;
        executable(&root.path().join("bin/pi"))?;
        let space = space(root.path());
        let mut search = search(root.path());
        search.control = Some(ControlEnv {
            socket: root.path().join("control.sock"),
            ctl_dir: None,
        });
        let named = pane_launch_with(
            &space,
            &role(PaneKind::Agent, "api"),
            &AgentStart::Task("fix it".into()),
            &AgentSession::Open("xctx-fix-retry-writer-api".into()),
            &search,
        );
        assert_eq!(
            named.config.args,
            ["--session-id", "xctx-fix-retry-writer-api", "--", "fix it"]
        );
        assert_eq!(env(&named, ATTEMPT_ID_ENV), Some(""));

        let worktree = root.path().join("api-worktrees/fix-retry--attempt-2");
        let mut attempt = AttemptPane {
            number: 2,
            repo: "api".into(),
            worktree: worktree.clone(),
            port: 4012,
            agent: PaneAgent {
                id: "writer-api#attempt-2".into(),
                role: "writer".into(),
            },
            token: "0a0b".into(),
            session: AgentSession::Fork {
                source: "xctx-fix-retry-writer-api".into(),
                id: "xctx-fix-retry-writer-api-attempt-2".into(),
            },
            instruction: Some("use a token bucket".into()),
            scratch: Some(root.path().join("scratch/attempts/2")),
        };
        let launch = attempt_launch(&space, &attempt, &search);
        assert_eq!(
            launch.config.args,
            [
                "--fork",
                "xctx-fix-retry-writer-api",
                "--session-id",
                "xctx-fix-retry-writer-api-attempt-2",
                "--",
                "use a token bucket"
            ]
        );
        assert_eq!(launch.config.cwd.as_deref(), Some(worktree.as_path()));
        assert_eq!(env(&launch, "PORT"), Some("4012"));
        assert_eq!(env(&launch, ATTEMPT_ID_ENV), Some("2"));
        assert_eq!(env(&launch, AGENT_ID_ENV), Some("writer-api#attempt-2"));
        assert_eq!(env(&launch, ATTEMPT_TOKEN_ENV), Some("0a0b"));
        assert_eq!(env(&launch, "XCTX_SPACE"), Some("fix-retry"));
        assert_eq!(env(&launch, "MISE_ENV"), Some("fix-retry"));
        assert!(env(&launch, xctx_control::CONTROL_SOCKET_ENV).is_some());
        let scratch = root.path().join("scratch/attempts/2");
        assert_eq!(env(&launch, SCRATCH_DIR_ENV), scratch.to_str());

        // Reopened, it opens its own session and is not told its instruction again.
        attempt.session = AgentSession::Open("xctx-fix-retry-writer-api-attempt-2".into());
        let reopened = attempt_launch(&space, &attempt, &search);
        assert_eq!(
            reopened.config.args,
            ["--session-id", "xctx-fix-retry-writer-api-attempt-2"]
        );
        Ok(())
    }

    #[test]
    fn panes_get_their_agent_and_the_spaces_scratch_dir() -> TestResult {
        let root = tempfile::tempdir()?;
        executable(&root.path().join("bin/pi"))?;
        let space = space(root.path());
        let search = search(root.path());
        let agent = PaneAgent {
            id: "writer-api".to_owned(),
            role: "writer".to_owned(),
        };
        let launch = pane_launch(
            &space,
            &role(PaneKind::Agent, "api"),
            &AgentStart::Continue,
            &search,
        )
        .with_agent(Some(&agent));
        let scratch = scratch_dir_for("fix-retry")?;
        let scratch = scratch.root().to_str();
        assert_eq!(env(&launch, AGENT_ID_ENV), Some("writer-api"));
        assert_eq!(env(&launch, AGENT_ROLE_ENV), Some("writer"));
        assert_eq!(env(&launch, SCRATCH_DIR_ENV), scratch);

        let shell = pane_launch(
            &space,
            &role(PaneKind::Shell, "web"),
            &AgentStart::Continue,
            &search,
        )
        .with_agent(None);
        assert_eq!(env(&shell, AGENT_ID_ENV), Some(""));
        assert_eq!(env(&shell, AGENT_ROLE_ENV), Some(""));
        assert_eq!(env(&shell, SCRATCH_DIR_ENV), scratch);
        Ok(())
    }
}
