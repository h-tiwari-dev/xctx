//! The tool dock of a space (`xctx-ui`'s `ToolDock`, on the right edge of every open space): one item per tool the
//! space can use. A click on a closed tool opens its pane with its default setting (see [`setting_for`]) and
//! focuses it; a click on an open tool closes its panes, as `⌘W` on them does. A tool that takes a setting can be
//! opened with another value from a popover (a right-click or the hover chevron), and opens the popover on a plain
//! click when it has no usable default (Logs with no dev server). An untrusted repo tool asks for approval in the
//! Inbox instead. Opening here is the user's own (`from_agent = false`).
//!
//! The dock reads which tools are open from the space's layout every frame, so a tool an agent opens over
//! `xctx-ctl` lights up at once. What the space can use (its manifests, their problems, each repo's web app) is read
//! from disk at most every [`CATALOG_TTL`].

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{AppContext, Context, Entity, Focusable, SharedString, Subscription, Window};
use xctx_control::protocol::PluginSummary;
use xctx_core::{Redactor, Space, SpaceId};
use xctx_layout::PaneKind;
use xctx_plugin::{Manifest, PaneSpec, SqlEngine};
use xctx_ui::{
    DockItem, IconName, PromptEvent, PromptInput, ToolDock, ToolDockView, ToolGroup, ToolProblem,
    ToolSetting,
};

use crate::dev_setup::WebApp;
use crate::plugin_host::{Mount, MountSettings};
use crate::plugin_jobs::{MountOutcome, UnmountTarget};
use crate::workspace::Workspace;

/// How long what a space's dock lists is kept before it is read again.
pub(crate) const CATALOG_TTL: Duration = Duration::from_secs(2);

/// The dock's own state across frames.
#[derive(Default)]
pub(crate) struct DockState {
    /// Per open space, what its dock lists.
    catalogs: HashMap<SpaceId, Catalog>,
    /// The space and tool whose setting popover is open.
    popover: Option<(SpaceId, String)>,
    /// The space whose problems list is open.
    problems_open: Option<SpaceId>,
    /// The popover's input, made the first time a popover opens.
    input: Option<Entity<PromptInput>>,
    /// The workspace's subscription to [`DockState::input`].
    input_subscription: Option<Subscription>,
}

/// What a space's dock lists, as last read from disk.
struct Catalog {
    read_at: Instant,
    tools: Vec<CatalogTool>,
    problems: Vec<ToolProblem>,
    /// Each repo's web app (see [`crate::dev_setup::web_app`]), in the space's order.
    web_apps: Vec<(String, WebApp)>,
}

/// One tool a space can use.
struct CatalogTool {
    summary: PluginSummary,
    manifest: Manifest,
}

/// How a tool with a setting opens: with its default, or with what is typed in its popover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SettingPlan {
    /// The tool's name.
    plugin: String,
    /// Its pane kind, which says what typed text is (see [`mount_settings_for`]).
    kind: &'static str,
    /// The text the popover's input starts with, masked.
    default: String,
    /// The settings it opens with by default (and when [`SettingPlan::default`] is submitted unchanged): the
    /// manifest's own, or the dev server's command. Never masked.
    settings: MountSettings,
    /// There is no usable default (Logs with no dev server): a click opens the popover.
    needs_input: bool,
    /// The repo the pane works in: the one the dev server default came from; `None` for the focused pane's.
    repo: Option<String>,
}

/// The web dev server of one of the space's repos: the default command of a terminal or logs pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DevServer {
    /// The repo it serves from.
    pub(crate) repo: String,
    /// Its command (`/bin/sh -c …`).
    pub(crate) command: Vec<String>,
}

/// What the defaults are filled from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DockSetup {
    /// The space's `PORT`.
    pub(crate) port: u16,
    /// The space's slug.
    pub(crate) space: SpaceId,
    /// The space's dev server, unless none was found or an open pane already runs it.
    pub(crate) dev: Option<DevServer>,
}

/// The name a tool is shown by: its manifest name with dashes as spaces, capitalised, with `api`, `sql`, `http`,
/// `url`, `ui` and `db` in capitals (`api-client` is "API client").
pub(crate) fn display_name(name: &str) -> String {
    let words: Vec<String> = name
        .split('-')
        .filter(|w| !w.is_empty())
        .enumerate()
        .map(|(i, word)| match word {
            "api" | "sql" | "http" | "url" | "ui" | "db" => word.to_uppercase(),
            _ if i == 0 => {
                let mut chars = word.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().chain(chars).collect()
                })
            }
            _ => word.to_owned(),
        })
        .collect();
    if words.is_empty() {
        name.to_owned()
    } else {
        words.join(" ")
    }
}

/// The group and icon of a tool with `manifest`: terminals, logs, files and browsers on a page are panes; the API
/// client and databases are tools; a page-less web pane with secrets is a connector (Jira, Figma); the rest is
/// other.
fn group_and_icon(manifest: &Manifest) -> (ToolGroup, IconName) {
    match &manifest.pane {
        PaneSpec::Terminal { .. } => (ToolGroup::Panes, IconName::Terminal),
        PaneSpec::Logs { .. } => (ToolGroup::Panes, IconName::FileText),
        PaneSpec::Files => (ToolGroup::Panes, IconName::Folder),
        PaneSpec::Web { url: Some(_) } => (ToolGroup::Panes, IconName::Globe),
        PaneSpec::Http { .. } => (ToolGroup::Tools, IconName::Send),
        PaneSpec::Sql { .. } => (ToolGroup::Tools, IconName::Database),
        PaneSpec::Web { url: None } if !manifest.secrets.is_empty() => (
            ToolGroup::Connectors,
            match manifest.name.as_str() {
                "jira" => IconName::Ticket,
                "figma" => IconName::PenTool,
                _ => IconName::Plugin,
            },
        ),
        PaneSpec::Web { url: None } => (ToolGroup::Other, IconName::Plugin),
    }
}

/// A manifest default as the user would type it: `{PORT}` and `{SPACE}` filled, a leading `{WORKTREE}/` dropped
/// (paths are relative to the worktree).
fn fill_default(text: &str, setup: &DockSetup) -> String {
    let text = text.strip_prefix("{WORKTREE}/").unwrap_or(text);
    text.replace("{PORT}", &setup.port.to_string())
        .replace("{SPACE}", setup.space.as_str())
}

/// A command as typed: the script of a `/bin/sh -c` command, else its words joined with spaces.
fn command_text(argv: &[String]) -> String {
    match argv {
        [sh, c, script] if sh == "/bin/sh" && c == "-c" => script.clone(),
        _ => argv.join(" "),
    }
}

/// The setting `manifest`'s tool can be opened with, and how it opens; `None` when it takes none (files,
/// connectors, and a terminal or logs pane whose manifest already names what it runs). A terminal or logs pane
/// defaults to `setup`'s dev server (else a terminal to your shell, and logs has no usable default), a database to
/// the manifest's, the API client to its base URL and the browser to its page (`http://localhost:{PORT}` by
/// default). Defaults are masked with `redactor` for display.
pub(crate) fn setting_for(
    manifest: &Manifest,
    setup: &DockSetup,
    redactor: &Redactor,
) -> Option<(ToolSetting, SettingPlan)> {
    let local = format!("http://localhost:{}", setup.port);
    let dev = setup.dev.as_ref();
    let dev_hint = |blank: &str| match dev {
        Some(dev) => format!(
            "Your web app's dev server in {} on PORT {}. Runs with /bin/sh; {blank}",
            dev.repo, setup.port
        ),
        None => format!("Runs with /bin/sh in the worktree; {blank}"),
    };
    let dev_settings = || MountSettings {
        command: dev.map(|d| d.command.clone()),
        ..MountSettings::default()
    };
    let dev_text = || dev.map(|d| command_text(&d.command)).unwrap_or_default();
    let (label, default, hint, settings, needs_input) = match &manifest.pane {
        PaneSpec::Terminal { run } if run.is_empty() => (
            "Command",
            dev_text(),
            dev_hint("blank for your shell."),
            dev_settings(),
            false,
        ),
        PaneSpec::Logs {
            run: None,
            file: None,
        } => (
            "Command or log file",
            dev_text(),
            dev_hint("or a file: a path ending in .log, .txt or .jsonl, or file:<path>."),
            dev_settings(),
            dev.is_none(),
        ),
        PaneSpec::Sql { database, .. } => (
            "Database",
            fill_default(database, setup),
            "A sqlite file (relative to the worktree) or a postgres:// URL.".to_owned(),
            MountSettings::default(),
            false,
        ),
        PaneSpec::Http { base_url } => (
            "Base URL",
            base_url
                .as_deref()
                .map_or_else(|| local.clone(), |url| fill_default(url, setup)),
            "Requests typed in the pane are relative to it.".to_owned(),
            MountSettings::default(),
            false,
        ),
        PaneSpec::Web { url: Some(url) } => (
            "Page",
            fill_default(url, setup),
            "The page it opens with.".to_owned(),
            MountSettings::default(),
            false,
        ),
        _ => return None,
    };
    let default = xctx_http::mask_url(&default, redactor);
    let repo = match &manifest.pane {
        PaneSpec::Terminal { .. } | PaneSpec::Logs { .. } => dev.map(|d| d.repo.clone()),
        _ => None,
    };
    Some((
        ToolSetting {
            label: label.into(),
            default: default.clone().into(),
            hint: Some(hint.into()),
        },
        SettingPlan {
            plugin: manifest.name.clone(),
            kind: manifest.pane.kind(),
            default,
            settings,
            needs_input,
            repo,
        },
    ))
}

/// What an open pane of `manifest`'s tool shows, from the `settings` it was opened with (else the manifest's), for
/// its hover label, masked with `redactor`: a terminal's command ("Your shell" without one), what a logs pane tails,
/// a database, a base URL, a page (the live browser's, `live_url`, when known), or a files pane's repo. `None` when
/// nothing says.
pub(crate) fn mount_showing(
    manifest: &Manifest,
    settings: &MountSettings,
    repo: &str,
    live_url: Option<&str>,
    setup: &DockSetup,
    redactor: &Redactor,
) -> Option<String> {
    let mask = |text: &str| redactor.redact(text).into_owned();
    let url = |text: &str| xctx_http::mask_url(text, redactor);
    let command = settings.command.as_deref().filter(|c| !c.is_empty());
    match &manifest.pane {
        PaneSpec::Terminal { run } => Some(
            command
                .or((!run.is_empty()).then_some(run.as_slice()))
                .map_or_else(|| "Your shell".to_owned(), |c| mask(&command_text(c))),
        ),
        PaneSpec::Logs { run, file } => command
            .or(run.as_deref())
            .map(|c| mask(&command_text(c)))
            .or_else(|| settings.log_file.as_deref().or(file.as_deref()).map(mask)),
        PaneSpec::Sql { database, .. } => Some(url(&settings
            .database
            .clone()
            .unwrap_or_else(|| fill_default(database, setup)))),
        PaneSpec::Http { base_url } => Some(url(&settings.base_url.clone().unwrap_or_else(|| {
            base_url.as_deref().map_or_else(
                || format!("http://localhost:{}", setup.port),
                |b| fill_default(b, setup),
            )
        }))),
        PaneSpec::Web { url: page } => {
            let page = settings
                .url
                .clone()
                .or_else(|| page.as_deref().map(|p| fill_default(p, setup)));
            page.map(|page| url(live_url.unwrap_or(&page)))
        }
        PaneSpec::Files => Some(format!("{} worktree", mask(repo))),
    }
}

/// The settings a `kind` tool opens with from the `text` typed in its popover (blank for none): for logs a log
/// file when it starts with `file:` or is one word ending in `.log`, `.txt` or `.jsonl`, else a command; for a
/// terminal a command; for a database its location (postgres for a `postgres://` URL, else a sqlite file); for the
/// API client its base URL; for the browser its page. Commands run with `/bin/sh -c`, as typed.
pub(crate) fn mount_settings_for(kind: &str, text: &str) -> MountSettings {
    let text = text.trim();
    let mut settings = MountSettings::default();
    if text.is_empty() {
        return settings;
    }
    let command = || Some(vec!["/bin/sh".to_owned(), "-c".to_owned(), text.to_owned()]);
    match kind {
        "logs" => {
            if let Some(file) = text.strip_prefix("file:") {
                settings.log_file = Some(file.trim().to_owned());
            } else if !text.contains(char::is_whitespace)
                && [".log", ".txt", ".jsonl"]
                    .iter()
                    .any(|ext| text.ends_with(ext))
            {
                settings.log_file = Some(text.to_owned());
            } else {
                settings.command = command();
            }
        }
        "terminal" => settings.command = command(),
        "sql" => {
            let postgres = text.starts_with("postgres://") || text.starts_with("postgresql://");
            settings.engine = Some(if postgres {
                SqlEngine::Postgres
            } else {
                SqlEngine::Sqlite
            });
            settings.database = Some(text.to_owned());
        }
        "http" => settings.base_url = Some(text.to_owned()),
        "web" => settings.url = Some(text.to_owned()),
        _ => {}
    }
    settings
}

/// The dev server a terminal or logs pane defaults to: the first of `web_apps` (repo, app), the `focused` repo
/// first; `None` when there is none or one of `mounts` already runs it.
pub(crate) fn dev_server(
    web_apps: &[(String, WebApp)],
    focused: Option<&str>,
    mounts: &[Mount],
) -> Option<DevServer> {
    let (repo, app) = web_apps
        .iter()
        .find(|(repo, _)| Some(repo.as_str()) == focused)
        .or_else(|| web_apps.first())?;
    let running = mounts
        .iter()
        .any(|m| m.settings.command.as_ref() == Some(&app.command));
    (!running).then(|| DevServer {
        repo: repo.clone(),
        command: app.command.clone(),
    })
}

/// A tool of the dock as drawn, with how it opens.
struct DockTool {
    item: DockItem,
    plan: Option<SettingPlan>,
}

impl Workspace {
    /// Read what `space`'s dock lists again when it is older than [`CATALOG_TTL`] (or was never read), and forget
    /// the catalogs of spaces whose tab closed.
    fn refresh_tool_catalog(&mut self, space: &Space) {
        let fresh = self
            .tool_dock
            .catalogs
            .get(&space.id)
            .is_some_and(|c| c.read_at.elapsed() < CATALOG_TTL);
        if fresh {
            return;
        }
        let summaries = self.plugin_menu_entries(space);
        let registry = self.plugins.registry(space);
        let tools = summaries
            .into_iter()
            .filter_map(|summary| {
                let manifest = registry.get(&summary.name)?.manifest.clone();
                Some(CatalogTool { summary, manifest })
            })
            .collect();
        let mask = |text: &str| self.redactor.redact(text).into_owned();
        let problems = registry
            .problems()
            .iter()
            .map(|p| ToolProblem {
                path: p
                    .path
                    .as_ref()
                    .map_or_else(
                        || "built-in".to_owned(),
                        |path| mask(&path.display().to_string()),
                    )
                    .into(),
                message: mask(&p.error.to_string()).into(),
            })
            .collect();
        let web_apps = space
            .repos
            .iter()
            .filter_map(|r| crate::dev_setup::web_app(&r.worktree).map(|app| (r.name.clone(), app)))
            .collect();
        let tabs = &self.tabs;
        self.tool_dock
            .catalogs
            .retain(|id, _| tabs.contains_key(id));
        self.tool_dock.catalogs.insert(
            space.id.clone(),
            Catalog {
                read_at: Instant::now(),
                tools,
                problems,
                web_apps,
            },
        );
    }

    /// The tools of open space `id` as the dock draws them, with how each opens; empty when it is not open.
    fn dock_tools(&mut self, id: &SpaceId) -> Vec<DockTool> {
        let Some(space) = self.tabs.get(id).map(|tab| tab.space.clone()) else {
            return Vec::new();
        };
        self.refresh_tool_catalog(&space);
        let mounts = self
            .plugins
            .mounts(id)
            .map(<[Mount]>::to_vec)
            .unwrap_or_default();
        let live_url = self.browsers.view(id).and_then(|view| view.url());
        let (Some(tab), Some(catalog)) = (self.tabs.get(id), self.tool_dock.catalogs.get(id))
        else {
            return Vec::new();
        };
        // What is open comes from the layout: a tool is open while it has a pane.
        let panes = tab.layout.panes();
        let open = |name: &str| {
            panes
                .iter()
                .find(|p| p.role.kind == PaneKind::Plugin && p.role.plugin.as_deref() == Some(name))
                .map(|p| p.id)
        };
        let focused = tab
            .layout
            .role(tab.layout.focused())
            .map(|r| r.repo.clone());
        let setup = DockSetup {
            port: space.port,
            space: space.id.clone(),
            dev: dev_server(&catalog.web_apps, focused.as_deref(), &mounts),
        };
        catalog
            .tools
            .iter()
            .map(|tool| {
                let name = tool.summary.name.as_str();
                let (group, icon) = group_and_icon(&tool.manifest);
                let setting = setting_for(&tool.manifest, &setup, &self.redactor);
                let pane = open(name);
                let detail = match pane.and_then(|pane| mounts.iter().find(|m| m.pane == pane)) {
                    Some(mount) => mount_showing(
                        &tool.manifest,
                        &mount.settings,
                        &mount.repo,
                        live_url.as_deref(),
                        &setup,
                        &self.redactor,
                    ),
                    None => Some(self.redactor.redact(&tool.summary.description).into_owned()),
                };
                let (setting, plan) = setting.unzip();
                DockTool {
                    item: DockItem {
                        name: name.to_owned().into(),
                        title: display_name(name).into(),
                        detail: detail.map(Into::into),
                        icon,
                        group,
                        open: pane.is_some(),
                        locked: tool.summary.trusted == Some(false),
                        needs_input: plan.as_ref().is_some_and(|p| p.needs_input),
                        setting,
                    },
                    plan,
                }
            })
            .collect()
    }

    /// What the dock of open space `id` shows now.
    pub(crate) fn tool_dock_view(&mut self, id: &SpaceId) -> ToolDockView {
        let items = self.dock_tools(id).into_iter().map(|t| t.item).collect();
        let problems = self
            .tool_dock
            .catalogs
            .get(id)
            .map(|c| c.problems.clone())
            .unwrap_or_default();
        let state = &self.tool_dock;
        ToolDockView {
            items,
            problems,
            popover: state
                .popover
                .as_ref()
                .filter(|(space, _)| space == id)
                .map(|(_, name)| name.clone().into()),
            problems_open: state.problems_open.as_ref() == Some(id),
            busy: self.dialog.is_some(),
        }
    }

    /// Whether a browser page may be drawn over its pane: not while a dialog, or a popover of the shown space's dock,
    /// is open over it.
    pub(crate) fn pages_may_show(&self) -> bool {
        let shown = self.active_space_id();
        let over_shown = |space: Option<&SpaceId>| space.is_some() && space == shown.as_ref();
        self.dialog.is_none()
            && !over_shown(self.tool_dock.popover.as_ref().map(|(space, _)| space))
            && !over_shown(self.tool_dock.problems_open.as_ref())
    }

    /// A tool pane of `id` opened or closed (from the dock, an agent's `plugin.mount`, `⌘W`): read what its dock
    /// lists again on the next frame, so a trust just given or a dev server just started shows at once. Which tools
    /// are open needs no refresh: the dock reads it from the layout every frame.
    pub(crate) fn plugins_changed(&mut self, id: &SpaceId) {
        self.tool_dock.catalogs.remove(id);
    }

    /// How tool `name` of space `id` opens: its manifest's title and its setting plan (`None` for no setting).
    fn tool_plan(&mut self, id: &SpaceId, name: &str) -> Option<(String, Option<SettingPlan>)> {
        self.dock_tools(id)
            .into_iter()
            .find(|t| t.item.name.as_ref() == name)
            .map(|t| (t.item.title.to_string(), t.plan))
    }

    /// Open tool `name` in the space shown, with its default setting; the popover instead when it has no usable
    /// default.
    fn dock_open(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_space_id() else {
            return;
        };
        let Some((title, plan)) = self.tool_plan(&id, name) else {
            return;
        };
        match plan {
            Some(plan) if plan.needs_input => self.dock_popover(Some(name), window, cx),
            Some(plan) => {
                self.dock_mount(&id, name, &title, plan.repo, plan.settings, window, cx);
            }
            None => {
                self.dock_mount(
                    &id,
                    name,
                    &title,
                    None,
                    MountSettings::default(),
                    window,
                    cx,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn dock_mount(
        &mut self,
        id: &SpaceId,
        name: &str,
        title: &str,
        repo: Option<String>,
        settings: MountSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match self.mount_plugin(id, name, repo, settings, false, window, cx) {
            Ok(MountOutcome::Mounted(_)) => {
                self.info(format!("Opened {title}"), cx);
                true
            }
            Ok(MountOutcome::Gated(_)) => {
                self.info(
                    format!(
                        "{title} needs your approval: approve it in the Inbox (Home) and it opens"
                    ),
                    cx,
                );
                true
            }
            Err(e) => {
                self.show_op_error(e, cx);
                false
            }
        }
    }

    /// Close every pane of tool `name` in the space shown, as `⌘W` on them does.
    fn dock_close(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_space_id() else {
            return;
        };
        match self.unmount_plugin(&id, UnmountTarget::Plugin(name.to_owned()), window, cx) {
            Ok(_) => self.info(format!("Closed {}", display_name(name)), cx),
            Err(e) => self.show_op_error(e, cx),
        }
    }

    /// Open the setting popover of tool `name` in the space shown (its input holding the default, with the keys),
    /// or close the open one (`None`), giving the keys back to the focused pane.
    fn dock_popover(&mut self, name: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_space_id() else {
            return;
        };
        let plan = name
            .and_then(|name| self.tool_plan(&id, name))
            .and_then(|(_, plan)| plan);
        self.tool_dock.problems_open = None;
        let Some(plan) = plan else {
            if self.tool_dock.popover.take().is_some() {
                self.focus_active_pane(window, cx);
            }
            cx.notify();
            return;
        };
        let input = match &self.tool_dock.input {
            Some(input) => input.clone(),
            None => {
                let input = cx.new(|cx| PromptInput::new(cx).submit_blank(true));
                self.tool_dock.input_subscription = Some(cx.subscribe_in(
                    &input,
                    window,
                    |this, _, event: &PromptEvent, window, cx| match event {
                        PromptEvent::Submit(_) => this.dock_submit(window, cx),
                        PromptEvent::Cancel => this.dock_popover(None, window, cx),
                    },
                ));
                self.tool_dock.input = Some(input.clone());
                input
            }
        };
        input.update(cx, |input, cx| input.set_value(&plan.default, cx));
        self.tool_dock.popover = Some((id, plan.plugin));
        let focus = input.focus_handle(cx);
        focus.focus(window, cx);
        cx.notify();
    }

    /// Open the popover's tool with what its input holds.
    fn dock_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, name)) = self.tool_dock.popover.clone() else {
            return;
        };
        let Some((title, Some(plan))) = self.tool_plan(&id, &name) else {
            return;
        };
        let text = self
            .tool_dock
            .input
            .as_ref()
            .map(|input| input.read(cx).value().to_owned())
            .unwrap_or_default();
        let settings = if text.trim() == plan.default.trim() {
            plan.settings.clone()
        } else {
            mount_settings_for(plan.kind, &text)
        };
        // Close first: the new pane takes the keys.
        self.tool_dock.popover = None;
        if !self.dock_mount(&id, &name, &title, plan.repo, settings, window, cx) {
            // Keep what was typed, so it can be fixed.
            self.tool_dock.popover = Some((id, name));
        }
        cx.notify();
    }

    fn dock_problems(&mut self, open: bool, cx: &mut Context<Self>) {
        self.tool_dock.popover = None;
        self.tool_dock.problems_open = if open { self.active_space_id() } else { None };
        cx.notify();
    }

    /// The dock of the space shown, drawn from `view`.
    pub(crate) fn render_tool_dock(&self, view: ToolDockView, cx: &mut Context<Self>) -> ToolDock {
        let entity = cx.entity().downgrade();
        let (open, close, popover) = (entity.clone(), entity.clone(), entity.clone());
        let (submit, problems) = (entity.clone(), entity);
        let input = view.popover.as_ref().and(self.tool_dock.input.clone());
        let mut dock = ToolDock::new(view)
            .on_open(move |name, window, cx| {
                let name = name.to_string();
                let _ = open.update(cx, |this, cx| this.dock_open(&name, window, cx));
            })
            .on_close(move |name, window, cx| {
                let name = name.to_string();
                let _ = close.update(cx, |this, cx| this.dock_close(&name, window, cx));
            })
            .on_popover(move |name, window, cx| {
                let name: Option<SharedString> = name.cloned();
                let _ = popover.update(cx, |this, cx| {
                    this.dock_popover(name.as_deref(), window, cx)
                });
            })
            .on_submit(move |_, window, cx| {
                let _ = submit.update(cx, |this, cx| this.dock_submit(window, cx));
            })
            .on_problems(move |open, _, cx| {
                let _ = problems.update(cx, |this, cx| this.dock_problems(open, cx));
            });
        if let Some(input) = input {
            dock = dock.setting_input(input);
        }
        dock
    }
}

#[cfg(test)]
mod tests {
    use xctx_layout::PaneId;
    use xctx_plugin::Registry;

    use super::*;

    fn setup(dev: Option<DevServer>) -> DockSetup {
        DockSetup {
            port: 4011,
            space: SpaceId::new("fix-retry"),
            dev,
        }
    }

    const DEV: &str = "[ -d node_modules ] || bun install; exec bun run dev --port \"$PORT\"";

    fn dev_command() -> Vec<String> {
        vec!["/bin/sh".into(), "-c".into(), DEV.into()]
    }

    fn dev() -> DevServer {
        DevServer {
            repo: "web".into(),
            command: dev_command(),
        }
    }

    fn manifest(name: &str) -> Manifest {
        Registry::with_builtins()
            .get(name)
            .map(|p| p.manifest.clone())
            .unwrap_or_else(|| panic!("no built-in {name}"))
    }

    fn mount(settings: MountSettings) -> Mount {
        Mount {
            pane: PaneId(4),
            plugin: "terminal".into(),
            repo: "web".into(),
            settings,
            user_paths: true,
        }
    }

    #[test]
    fn built_ins_are_grouped_by_purpose_with_friendly_names() {
        let registry = Registry::with_builtins();
        let group = |group: ToolGroup| -> Vec<String> {
            registry
                .plugins()
                .iter()
                .filter(|p| group_and_icon(&p.manifest).0 == group)
                .map(|p| display_name(p.name()))
                .collect()
        };
        assert_eq!(
            group(ToolGroup::Panes),
            ["Terminal", "Logs", "Browser", "Files"]
        );
        assert_eq!(group(ToolGroup::Tools), ["API client", "Database"]);
        assert_eq!(group(ToolGroup::Connectors), ["Jira", "Figma"]);
        assert!(group(ToolGroup::Other).is_empty());
        assert_eq!(display_name("my-sql-tool"), "My SQL tool");
        assert_eq!(group_and_icon(&manifest("browser")).1, IconName::Globe);
    }

    #[test]
    fn settings_default_to_the_dev_server_and_the_spaces_port() {
        let redactor = Redactor::default();
        let with_dev = setup(Some(dev()));
        let plan = |name: &str, setup: &DockSetup| {
            setting_for(&manifest(name), setup, &redactor).map(|(_, plan)| plan)
        };
        let terminal = plan("terminal", &with_dev).ok_or("no terminal setting");
        assert_eq!(
            terminal
                .as_ref()
                .map(|p| (p.default.as_str(), p.needs_input, p.repo.as_deref())),
            Ok((DEV, false, Some("web")))
        );
        assert_eq!(
            terminal.map(|p| p.settings.command),
            Ok(Some(dev_command()))
        );
        let logs = plan("logs", &with_dev).map(|p| (p.default, p.needs_input));
        assert_eq!(logs, Some((DEV.to_owned(), false)));
        for (name, default) in [
            ("browser", "http://localhost:4011"),
            ("api-client", "http://localhost:4011"),
            ("database", "db/development.sqlite3"),
        ] {
            let plan = plan(name, &with_dev);
            assert_eq!(
                plan.as_ref().map(|p| p.default.as_str()),
                Some(default),
                "{name}"
            );
            // Unchanged, they open with the manifest's own settings.
            assert_eq!(plan.map(|p| p.settings), Some(MountSettings::default()));
        }
        for name in ["files", "jira", "figma"] {
            assert_eq!(plan(name, &with_dev), None, "{name} takes no setting");
        }
        // Without a dev server a terminal opens your shell, and logs has nothing to tail: it asks.
        let bare = setup(None);
        assert_eq!(
            plan("terminal", &bare).map(|p| (p.default, p.needs_input, p.settings)),
            Some((String::new(), false, MountSettings::default()))
        );
        assert_eq!(plan("logs", &bare).map(|p| p.needs_input), Some(true));
    }

    #[test]
    fn the_dev_server_prefers_the_focused_repo_and_is_skipped_when_running() {
        let app = |cmd: &str| WebApp {
            command: vec![cmd.to_owned()],
        };
        let apps = vec![("site".to_owned(), app("a")), ("web".to_owned(), app("b"))];
        let pick = |focused, mounts: &[Mount]| dev_server(&apps, focused, mounts).map(|d| d.repo);
        assert_eq!(pick(Some("web"), &[]).as_deref(), Some("web"));
        assert_eq!(pick(Some("api"), &[]).as_deref(), Some("site"));
        let running = mount(MountSettings {
            command: Some(vec!["b".into()]),
            ..MountSettings::default()
        });
        assert_eq!(pick(Some("web"), &[running]), None);
        assert_eq!(dev_server(&[], None, &[]), None);
    }

    #[test]
    fn an_open_tool_says_what_it_shows() {
        let redactor = Redactor::default();
        let setup = setup(None);
        let show = |name: &str, settings: MountSettings, live: Option<&str>| {
            mount_showing(&manifest(name), &settings, "api", live, &setup, &redactor)
        };
        assert_eq!(
            show(
                "terminal",
                MountSettings {
                    command: Some(dev_command()),
                    ..MountSettings::default()
                },
                None
            )
            .as_deref(),
            Some(DEV)
        );
        assert_eq!(
            show("terminal", MountSettings::default(), None).as_deref(),
            Some("Your shell")
        );
        assert_eq!(
            show("browser", MountSettings::default(), None).as_deref(),
            Some("http://localhost:4011")
        );
        assert_eq!(
            show(
                "browser",
                MountSettings::default(),
                Some("http://localhost:4011/login")
            )
            .as_deref(),
            Some("http://localhost:4011/login")
        );
        assert_eq!(
            show(
                "logs",
                MountSettings {
                    log_file: Some("log/development.log".into()),
                    ..MountSettings::default()
                },
                None
            )
            .as_deref(),
            Some("log/development.log")
        );
        assert_eq!(
            show("database", MountSettings::default(), None).as_deref(),
            Some("db/development.sqlite3")
        );
        assert_eq!(
            show("files", MountSettings::default(), None).as_deref(),
            Some("api worktree")
        );
        // A connector's page-less pane says nothing.
        assert_eq!(show("jira", MountSettings::default(), None), None);
    }

    #[test]
    fn the_typed_setting_becomes_the_settings() {
        let sh = |script: &str| Some(vec!["/bin/sh".into(), "-c".into(), script.to_owned()]);
        assert_eq!(
            mount_settings_for("logs", " tail -F /var/log/app.log ").command,
            sh("tail -F /var/log/app.log")
        );
        assert_eq!(
            mount_settings_for("logs", "log/development.log")
                .log_file
                .as_deref(),
            Some("log/development.log")
        );
        assert_eq!(
            mount_settings_for("logs", "file: out").log_file.as_deref(),
            Some("out")
        );
        let pg = mount_settings_for("sql", "postgres://localhost/app");
        assert_eq!(
            (pg.engine, pg.database.as_deref()),
            (Some(SqlEngine::Postgres), Some("postgres://localhost/app"))
        );
        assert_eq!(
            mount_settings_for("sql", "db/dev.sqlite3").engine,
            Some(SqlEngine::Sqlite)
        );
        assert_eq!(
            mount_settings_for("http", "http://localhost:9")
                .base_url
                .as_deref(),
            Some("http://localhost:9")
        );
        assert_eq!(
            mount_settings_for("web", "/login").url.as_deref(),
            Some("/login")
        );
        assert_eq!(mount_settings_for("terminal", "k9s").command, sh("k9s"));
        assert_eq!(
            mount_settings_for("terminal", "  "),
            MountSettings::default()
        );
    }
}
