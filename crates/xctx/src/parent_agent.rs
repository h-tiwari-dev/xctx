//! The Parent Agent: an always-on `omp` session in a drawer on Home that turns tasks into Plan Gates and keeps
//! the user's spaces arranged, through `xctx-ctl`.
//!
//! Nothing on the control socket can launch or approve, so its drafts always wait at the Plan Gate. Its "writes
//! no code" rule is advisory: `omp` gets only its read and bash tools and a prompt saying not to change anything,
//! but bash is a full shell (and MCP tools and the user's omp extensions still load), so the prompt, not a
//! sandbox, keeps it from editing files. It runs in its own folder beside the data directory, not inside it, so
//! the store and the Inbox file are not in its working directory.

use std::path::{Path, PathBuf};

use gpui::{Context, Entity, Subscription, Window};
use xctx_term::TerminalConfig;
use xctx_term_view::{TerminalView, TerminalViewEvent};
use xctx_ui::AgentStatus;

use crate::spawn::ProgramSearch;
use crate::workspace::{View, Workspace};

/// The program the Parent Agent runs: `omp`, looked up like an agent pane's (see [`ProgramSearch::find`]).
pub const PARENT_AGENT_PROGRAM: &str = "omp";

/// What the name of the folder the Parent Agent runs in adds to the data directory's name: it runs in
/// `<data dir>-parent-agent` beside the data directory, and `omp --continue` resumes its session there.
pub const PARENT_AGENT_DIR_SUFFIX: &str = "-parent-agent";

/// The only `omp` tools the Parent Agent gets: read, and bash for `xctx-ctl`. Without `edit` and `write` it has
/// no file-editing tools, but bash can still change files; only the prompt asks it not to.
pub const PARENT_AGENT_TOOLS: &str = "read,bash";

/// What the Parent Agent is told about its role, appended to `omp`'s system prompt.
pub const PARENT_AGENT_PROMPT: &str = "\
You are the xctx Parent Agent. You run on xctx's Home screen and help the user turn tasks into spaces. \
A space is a task with one git worktree per repository, its own PORT and MISE_ENV, and agents and shells in panes.

You never write code and never edit, create or delete files, in any repository or anywhere else. Coding \
happens in the spaces, by their own agents. Use bash only to run xctx-ctl and to look at repositories \
(ls, cat, git log, rg); do not run commands that change anything.

Drive xctx with the xctx-ctl command (run `xctx-ctl help` for every method):
- xctx-ctl space.list and xctx-ctl layout.get space=<slug> show what exists.
- xctx-ctl space.draft task=\"<the task>\" repos=<name>,<name> rationale=\"<why these repos>\" proposes a space. \
It appears on the user's Home screen right under you as a Plan Gate card; you cannot launch it, the user presses \
Launch (or ⌘↵) there, after dropping or adding repos and picking a shape if they want. Leave repos out to let \
xctx pick from its index, or name the repos you found and say why in the rationale.
- xctx-ctl space.reshape space=<slug> shape=dev|debug|review, pane.split, pane.focus and pane.zoom arrange a space.
- xctx-ctl inbox.list shows what waits for the user; xctx-ctl inbox.post title=\"...\" tells them something; \
xctx-ctl status.set message=\"...\" shows a short status line.

Spaces have tools: a pane the user sees beside the agents plus calls agents make. Every tool is available in every \
open space; open what its task calls for with xctx-ctl plugin.mount space=<slug> plugin=<name> (optionally \
repo=<name>; an open tool's pane is returned, never a second one):
- building UI: browser (the app's page, screenshot, console and network errors) and terminal (a dev server or watcher);
- an API change: api-client (send, replay and diff requests to the space's app) and logs;
- a bug: logs (tail the app's log or command) and api-client (to reproduce it);
- a data change: database (read-only queries; any write waits in the Inbox for the user);
- a task that names a ticket: jira (read the issue; comments wait in the Inbox).
xctx-ctl plugin.list space=<slug> shows the tools and which are open; xctx-ctl plugin.tools plugin=<name> shows a \
tool's calls and their parameters; xctx-ctl plugin.call space=<slug> plugin=<name> tool=<tool> \
args:='{\"key\":\"value\"}' runs a read call (opening the tool's pane first when it is closed), and a write call \
only posts a gate to the Inbox; xctx-ctl plugin.unmount space=<slug> plugin=<name> closes its pane, as the user's \
⌘W does. Connectors with writes (jira) and a repo's own plugins ask the user in the Inbox before they open; nothing \
you run approves them.

Spaces are living: once open, their agents run in panes and you manage them through xctx-ctl:
- xctx-ctl agent.list shows every space's agents (orchestrator, writers, helpers), the folder each writer works \
in, their graph state and whether their terminal runs. A space listed with open false runs nothing yet.
- xctx-ctl space.open space=<slug> opens a space's tab and shows it, starting its panes. Open a space only when \
the user asked to work on it.
- xctx-ctl agent.start space=<slug> agent=<id> starts an agent that is not running (process no_pane or exited): \
a new pane with its brief, or its own pane again when it exited. A running agent is left alone.
- xctx-ctl agent.read space=<slug> agent=<id> (optionally lines=<1-200>) shows the end of an agent's screen, to \
check its progress.
- xctx-ctl agent.send space=<slug> agent=<id> text=\"...\" types one message to an agent and presses Enter, to \
brief or redirect it. Start every message with \"Parent Agent:\" so the agent knows who is talking, keep it to a \
line or two, and never ask an agent to do anything the user did not ask for.
When the user asks how a space is going, run agent.list, read its agents with agent.read, and answer from what \
their screens show; say so when an agent has exited or has no pane.

How to shape a space:
- One repo (a portfolio site, say): its writer edits the repo; mount terminal for the dev server and browser for \
the page; for UI work add a Browser helper to check it.
- A frontend and a backend: one writer per repo, so they share a contract (the orchestrator sets it with \
contract.set; both agree before building on it). Mount browser and a Browser helper for the frontend, and \
api-client and logs for the backend; the orchestrator keeps the two in step.

For UI work, add a Browser helper: xctx-ctl graph.add_lane space=<slug> role=helper kind=browser. It tests the app \
in xctx's in-app browser, never an outside browser: call its tools with plugin.call space=<slug> plugin=browser \
tool=<tool> args:='{...}' (the browser opens beside the agents on the first call): page.open {path}, \
page.click {selector}, page.type {selector, text, submit}, page.text {selector}, page.screenshot {full_page} and \
page.console {errors_only}. When you brief agents about the app, tell them to use these tools and not an outside \
browser.

When the user says to start or continue working on something (\"let's work on the portfolio\"), get it running, \
do not just describe it:
1. Run xctx-ctl space.list and agent.list. If an active space matches the task (by its title, slug or repos), use \
it; otherwise draft one (below) and stop there, since only the user can launch it.
2. xctx-ctl space.open space=<slug> if it is not open.
3. xctx-ctl agent.start for the orchestrator and every writer whose process is not running.
4. Brief the orchestrator with agent.send: the user's request in their words, and that it should split the work \
across the writers. For UI work add a Browser helper (above).
5. Tell the user in one line what is now running.

When the user describes a new task, draft its space quickly: find the repositories it touches (the user's indexed \
folders are where their repos live; a quick look is enough), call space.draft once, then tell the user in one \
line that the draft is ready under you and ⌘↵ launches it. Once they launch it, follow the steps above. Ask first \
only when you cannot tell what the task is or which repos it needs. Every space works in its own git worktrees \
(branch space/<slug>), never in the user's main checkout; you do not need to mention that to the user. Secret \
values are masked as [REDACTED:NAME]; never try to recover them.";

/// The Parent Agent's terminal and state.
pub(crate) struct ParentAgent {
    pub(crate) status: AgentStatus,
    pub(crate) view: Option<Entity<TerminalView>>,
    _subscription: Option<Subscription>,
}

impl Default for ParentAgent {
    fn default() -> Self {
        ParentAgent {
            status: AgentStatus::Starting,
            view: None,
            _subscription: None,
        }
    }
}

/// The config that starts the Parent Agent: `omp --continue` with only [`PARENT_AGENT_TOOLS`],
/// [`PARENT_AGENT_PROMPT`] and xctx's agent skills (when `search` has their folder), in `dir`, with the control
/// socket and `xctx-ctl` from `search`. Fails, saying why, when `omp` cannot be found.
pub fn parent_agent_config(search: &ProgramSearch, dir: &Path) -> Result<TerminalConfig, String> {
    let Some(program) = search.find(PARENT_AGENT_PROGRAM) else {
        return Err(format!(
            "{PARENT_AGENT_PROGRAM} was not found on PATH, in ~/.pi/agent/bin, ~/.local/bin, /opt/homebrew/bin or \
             /usr/local/bin, so the Parent Agent cannot run. Install {PARENT_AGENT_PROGRAM} and press Restart."
        ));
    };
    let mut args: Vec<String> = Vec::new();
    if let Some(skills) = &search.skills {
        args.push("--config".to_owned());
        args.push(
            skills
                .join(crate::agent_skills::OMP_CONFIG_FILE)
                .to_string_lossy()
                .into_owned(),
        );
    }
    args.extend(
        [
            "--continue",
            "--tools",
            PARENT_AGENT_TOOLS,
            "--append-system-prompt",
            PARENT_AGENT_PROMPT,
        ]
        .map(str::to_owned),
    );
    let mut config = TerminalConfig::new(program.to_string_lossy()).args(args);
    config = config.cwd(dir);
    if let Some(control) = &search.control {
        config = config.env(
            xctx_control::CONTROL_SOCKET_ENV,
            control.socket.to_string_lossy(),
        );
    }
    config = config.env("PATH", search.pane_path(Some(&program)));
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
                // A restart from Home gives the new terminal the keys, as Home's only input.
                if self.view == View::Home && self.dialog.is_none() {
                    self.focus_home(window, cx);
                }
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
            skills: None,
        }
    }

    #[test]
    fn without_omp_the_agent_says_how_to_fix_it() -> TestResult {
        let root = tempfile::tempdir()?;
        let err = parent_agent_config(&search(root.path()), root.path()).err();
        assert!(err.is_some_and(|e| e.contains("Install omp")));
        Ok(())
    }

    #[test]
    fn the_agent_runs_omp_without_edit_tools_with_the_socket_and_xctx_ctl() -> TestResult {
        let root = tempfile::tempdir()?;
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin)?;
        let omp = bin.join("omp");
        fs::write(&omp, "#!/bin/sh\n")?;
        fs::set_permissions(&omp, fs::Permissions::from_mode(0o755))?;
        let ctl_dir = root.path().join("app");
        let mut search = search(root.path());
        search.control = Some(ControlEnv {
            socket: root.path().join("control.sock"),
            ctl_dir: Some(ctl_dir.clone()),
        });
        let dir = parent_agent_dir_for(&root.path().join("data"))?;
        let config = parent_agent_config(&search, &dir)?;
        assert_eq!(config.program, omp.to_string_lossy());
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
        for plugin in [
            "browser",
            "terminal",
            "api-client",
            "logs",
            "database",
            "jira",
        ] {
            assert!(PARENT_AGENT_PROMPT.contains(plugin), "{plugin}");
        }
        for method in [
            "plugin.mount",
            "plugin.list",
            "plugin.tools",
            "plugin.call",
            "plugin.unmount",
            "xctx-ctl agent.list",
            "xctx-ctl agent.read space=<slug> agent=<id>",
            "xctx-ctl agent.send space=<slug> agent=<id> text=",
            "graph.add_lane space=<slug> role=helper kind=browser",
            "xctx-ctl space.open space=<slug>",
            "xctx-ctl agent.start space=<slug> agent=<id>",
        ] {
            assert!(PARENT_AGENT_PROMPT.contains(method), "{method}");
        }
        for tool in [
            "page.open",
            "page.click",
            "page.type",
            "page.text",
            "page.screenshot",
            "page.console",
        ] {
            assert!(PARENT_AGENT_PROMPT.contains(tool), "{tool}");
        }
        assert!(PARENT_AGENT_PROMPT.contains("Parent Agent:"));
        assert!(PARENT_AGENT_PROMPT.contains("never an outside browser"));
        assert!(PARENT_AGENT_PROMPT.contains("did not ask for"));
        assert!(PARENT_AGENT_PROMPT.contains("never write code"));
        assert!(!PARENT_AGENT_TOOLS.contains("edit") && !PARENT_AGENT_TOOLS.contains("write"));
    }
}
