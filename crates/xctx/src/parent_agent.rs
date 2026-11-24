//! The Parent Agent: an always-on `pi` session in a drawer on Home that turns tasks into Plan Gates and keeps
//! the user's spaces arranged, through `xctx-ctl`.
//!
//! Nothing on the control socket can launch or approve, so its drafts always wait at the Plan Gate. Its "writes
//! no code" rule is advisory: `pi` gets only its read and bash tools and a prompt saying not to change anything,
//! but bash is a full shell (and MCP tools and the user's pi extensions still load), so the prompt, not a
//! sandbox, keeps it from editing files. It runs in its own folder beside the data directory, not inside it, so
//! the store and the Inbox file are not in its working directory.

use std::path::{Path, PathBuf};

use gpui::{Context, Entity, Subscription, Window};
use xctx_term::TerminalConfig;
use xctx_term_view::{TerminalView, TerminalViewEvent};
use xctx_ui::AgentStatus;

use crate::spawn::{AGENT_PROGRAM, ProgramSearch};
use crate::workspace::Workspace;

/// What the name of the folder the Parent Agent runs in adds to the data directory's name: it runs in
/// `<data dir>-parent-agent` beside the data directory, and `pi --continue` resumes its session there.
pub const PARENT_AGENT_DIR_SUFFIX: &str = "-parent-agent";

/// The only `pi` tools the Parent Agent gets: read, and bash for `xctx-ctl`. Without `edit` and `write` it has
/// no file-editing tools, but bash can still change files; only the prompt asks it not to.
pub const PARENT_AGENT_TOOLS: &str = "read,bash";

/// What the Parent Agent is told about its role, appended to `pi`'s system prompt.
pub const PARENT_AGENT_PROMPT: &str = "\
You are the xctx Parent Agent. You run on xctx's Home screen and help the user turn tasks into spaces. \
A space is a task with one git worktree per repository, its own PORT and MISE_ENV, and agents and shells in panes.

You never write code and never edit, create or delete files, in any repository or anywhere else. Coding \
happens in the spaces, by their own agents. Use bash only to run xctx-ctl and to look at repositories \
(ls, cat, git log, rg); do not run commands that change anything.

Drive xctx with the xctx-ctl command (run `xctx-ctl help` for every method):
- xctx-ctl space.list and xctx-ctl layout.get space=<slug> show what exists.
- xctx-ctl space.draft task=\"<the task>\" repos=<name>,<name> rationale=\"<why these repos>\" proposes a space. \
It goes to the user's Inbox as a Plan Gate; you cannot launch it, the user approves it there. Leave repos out \
to let xctx pick from its index, or name the repos you found and say why in the rationale.
- xctx-ctl space.reshape space=<slug> shape=dev|debug|review, pane.split, pane.focus and pane.zoom arrange a space.
- xctx-ctl inbox.list shows what waits for the user; xctx-ctl inbox.post title=\"...\" tells them something; \
xctx-ctl status.set message=\"...\" shows a short status line.

When the user describes a task, find the repositories it touches (the user's indexed folders are where their \
repos live), then draft one space for it and tell the user it is waiting in the Inbox. Ask when the task is \
unclear. Secret values are masked as [REDACTED:NAME]; never try to recover them.";

/// The Parent Agent's terminal and state.
pub(crate) struct ParentAgent {
    pub(crate) status: AgentStatus,
    pub(crate) open: bool,
    pub(crate) view: Option<Entity<TerminalView>>,
    _subscription: Option<Subscription>,
}

impl Default for ParentAgent {
    fn default() -> Self {
        ParentAgent {
            status: AgentStatus::Starting,
            open: true,
            view: None,
            _subscription: None,
        }
    }
}

/// The config that starts the Parent Agent: `pi --continue` with only [`PARENT_AGENT_TOOLS`] and
/// [`PARENT_AGENT_PROMPT`], in `dir`, with the control socket and `xctx-ctl` from `search`. `None` when `pi`
/// cannot be found, with why.
pub fn parent_agent_config(search: &ProgramSearch, dir: &Path) -> Result<TerminalConfig, String> {
    let Some(pi) = search.find(AGENT_PROGRAM) else {
        return Err(format!(
            "{AGENT_PROGRAM} was not found on PATH, in ~/.pi/agent/bin, /opt/homebrew/bin or /usr/local/bin, \
             so the Parent Agent cannot run. Install pi and press Restart."
        ));
    };
    let mut config = TerminalConfig::new(pi.to_string_lossy()).args([
        "--continue",
        "--tools",
        PARENT_AGENT_TOOLS,
        "--append-system-prompt",
        PARENT_AGENT_PROMPT,
    ]);
    config = config.cwd(dir);
    if let Some(control) = &search.control {
        config = config.env(
            xctx_control::CONTROL_SOCKET_ENV,
            control.socket.to_string_lossy(),
        );
    }
    config = config.env("PATH", search.pane_path(Some(&pi)));
    Ok(config)
}

/// Where the Parent Agent runs for the data directory `data_dir`: a sibling named after it with
/// [`PARENT_AGENT_DIR_SUFFIX`] (`~/Library/Application Support/xctx-parent-agent` for the default one), outside
/// the data directory so the store and the Inbox file are not in its working directory. Each data directory
/// (such as a throwaway `XCTX_DATA_DIR`) gets its own.
pub fn parent_agent_dir_for(data_dir: &Path) -> Result<PathBuf, String> {
    match (data_dir.parent(), data_dir.file_name()) {
        (Some(parent), Some(name)) => {
            let mut dir_name = name.to_os_string();
            dir_name.push(PARENT_AGENT_DIR_SUFFIX);
            Ok(parent.join(dir_name))
        }
        _ => Err(format!(
            "the Parent Agent has nowhere to run: the data directory {} has no parent folder",
            data_dir.display()
        )),
    }
}

/// Where the Parent Agent runs: [`parent_agent_dir_for`] the data directory.
pub fn parent_agent_dir() -> Result<PathBuf, String> {
    let data_dir = xctx_core::store::data_dir()
        .map_err(|e| format!("the Parent Agent has nowhere to run: {e}"))?;
    parent_agent_dir_for(&data_dir)
}

impl Workspace {
    /// Start (or restart) the Parent Agent.
    pub(crate) fn start_parent_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config = parent_agent_dir().and_then(|dir| {
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
            parent_agent_config(&self.search, &dir)
        });
        let config = match config {
            Ok(config) => config,
            Err(why) => {
                self.parent.status = AgentStatus::Unavailable(why.into());
                self.parent.view = None;
                cx.notify();
                return;
            }
        };
        match TerminalView::spawn(
            config,
            self.terminal_font.clone(),
            self.terminal_theme,
            window,
            cx,
        ) {
            Ok(view) => {
                let subscription = cx.subscribe_in(
                    &view,
                    window,
                    |this, _, event: &TerminalViewEvent, _, cx| {
                        if let TerminalViewEvent::Exited(code) = event {
                            this.parent.status = AgentStatus::Exited(*code);
                            cx.notify();
                        }
                    },
                );
                self.parent.status = AgentStatus::Running;
                self.parent.view = Some(view);
                self.parent._subscription = Some(subscription);
            }
            Err(e) => {
                self.parent.status = AgentStatus::Unavailable(
                    format!("could not start the Parent Agent: {e}").into(),
                );
                self.parent.view = None;
            }
        }
        cx.notify();
    }

    /// Show or hide the Parent Agent's terminal.
    pub(crate) fn toggle_parent_agent(&mut self, cx: &mut Context<Self>) {
        self.parent.open = !self.parent.open;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::spawn::ControlEnv;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn search(root: &Path) -> ProgramSearch {
        ProgramSearch {
            path: vec![root.join("bin")],
            home: Some(root.join("home")),
            shell: Some("/bin/zsh".to_owned()),
            system_dirs: Vec::new(),
            control: None,
        }
    }

    #[test]
    fn without_pi_the_agent_says_how_to_fix_it() -> TestResult {
        let root = tempfile::tempdir()?;
        let err = parent_agent_config(&search(root.path()), root.path()).err();
        assert!(err.is_some_and(|e| e.contains("Install pi")));
        Ok(())
    }

    #[test]
    fn the_agent_runs_pi_without_edit_tools_with_the_socket_and_xctx_ctl() -> TestResult {
        let root = tempfile::tempdir()?;
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin)?;
        let pi = bin.join("pi");
        fs::write(&pi, "#!/bin/sh\n")?;
        fs::set_permissions(&pi, fs::Permissions::from_mode(0o755))?;
        let ctl_dir = root.path().join("app");
        let mut search = search(root.path());
        search.control = Some(ControlEnv {
            socket: root.path().join("control.sock"),
            ctl_dir: Some(ctl_dir.clone()),
        });
        let dir = parent_agent_dir_for(&root.path().join("data"))?;
        let config = parent_agent_config(&search, &dir)?;
        assert_eq!(config.program, pi.to_string_lossy());
        let tools = config.args.iter().position(|a| a == "--tools");
        assert_eq!(
            tools
                .and_then(|i| config.args.get(i + 1))
                .map(String::as_str),
            Some("read,bash")
        );
        assert!(config.args.contains(&"--continue".to_owned()));
        assert!(config.args.iter().any(|a| a.contains("never write code")));
        assert_eq!(config.cwd.as_deref(), Some(dir.as_path()));
        assert_eq!(
            config.env.get("XCTX_CONTROL_SOCKET").map(String::as_str),
            root.path().join("control.sock").to_str()
        );
        let path = config.env.get("PATH").cloned().unwrap_or_default();
        assert!(
            path.starts_with(&ctl_dir.to_string_lossy().into_owned()),
            "{path}"
        );
        Ok(())
    }

    #[test]
    fn the_agent_runs_beside_the_data_directory_not_inside_it() -> TestResult {
        let data = Path::new("/tmp/xctx-try");
        let dir = parent_agent_dir_for(data)?;
        assert_eq!(dir, Path::new("/tmp/xctx-try-parent-agent"));
        assert!(!dir.starts_with(data));
        assert_eq!(
            parent_agent_dir_for(Path::new("/Users/me/Library/Application Support/xctx"))?,
            Path::new("/Users/me/Library/Application Support/xctx-parent-agent")
        );
        assert!(parent_agent_dir_for(Path::new("/")).is_err());
        Ok(())
    }

    #[test]
    fn the_prompt_names_the_gate_and_the_tool() {
        assert!(PARENT_AGENT_PROMPT.contains("xctx-ctl space.draft"));
        assert!(PARENT_AGENT_PROMPT.contains("cannot launch"));
        assert!(!PARENT_AGENT_TOOLS.contains("edit") && !PARENT_AGENT_TOOLS.contains("write"));
    }
}
