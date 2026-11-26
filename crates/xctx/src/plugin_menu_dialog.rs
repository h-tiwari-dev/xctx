//! The Plugins menu of a space (`⌘⇧P`, or Plugins in the tab bar): what is in the space (Show focuses a plugin's
//! pane, Remove unmounts it), what can be added, grouped by purpose, and the plugin folders whose manifest could
//! not be used.
//!
//! Mounting here is the user's own (`from_agent = false`), so only an untrusted repo plugin waits in the Inbox.
//! A plugin that takes a setting opens an inline input prefilled with a default (see [`setting_for`]): a command
//! or a log file for a terminal or a logs pane (the space's web dev server when `dev_setup` finds one), a
//! database, a base URL or a page. What is typed becomes the mount's settings (see [`mount_settings_for`]).

use gpui::{Context, Focusable, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use xctx_control::protocol::{PluginSource as ProtocolSource, PluginSummary};
use xctx_core::{Redactor, Space, SpaceId};
use xctx_plugin::{Manifest, PaneSpec, Registry, SqlEngine};
use xctx_ui::{
    IconName, MountedEntry, PluginEntry, PluginGroup, PluginMenu, PluginMenuView, PluginProblemRow,
    PluginSetting, PluginSource, PromptEvent, PromptInput, Theme,
};

use crate::dialogs::Dialog;
use crate::keymap::DismissDialog;
use crate::plugin_host::{MountSettings, MountedPlugin, PaneSettings};
use crate::plugin_jobs::{MountOutcome, UnmountTarget};
use crate::workspace::Workspace;

/// The open Plugins menu.
pub(crate) struct PluginsState {
    pub(crate) space: SpaceId,
    view: PluginMenuView,
    /// How each plugin with a setting mounts.
    plans: Vec<SettingPlan>,
    /// What the last action did (a mount waiting in the Inbox, say).
    note: Option<String>,
    /// The open setting row's input.
    setting: gpui::Entity<PromptInput>,
}

/// How a plugin that takes a setting mounts with what is typed in its row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SettingPlan {
    /// The plugin's name.
    plugin: String,
    /// Its pane kind, which says what the text is (see [`mount_settings_for`]).
    kind: &'static str,
    /// The text the row's input starts with, masked.
    default: String,
    /// Submitting [`SettingPlan::default`] unchanged mounts with the manifest's own settings (it is the manifest's
    /// default, filled in for display), rather than with the text.
    manifest_default: bool,
    /// The repo the mount works in: the one the dev server default came from; `None` for the focused pane's.
    repo: Option<String>,
}

/// The web dev server of one of the space's repos (see [`crate::dev_setup::web_app`]): the default command of a
/// terminal or logs pane added from the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DevServer {
    /// The repo it serves from.
    pub(crate) repo: String,
    /// Its command (`/bin/sh -c …`).
    pub(crate) command: Vec<String>,
}

/// What the menu's defaults are filled from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MenuSetup {
    /// The space's `PORT`.
    pub(crate) port: u16,
    /// The space's slug.
    pub(crate) space: SpaceId,
    /// The space's dev server, unless none was found or a mounted pane already runs it.
    pub(crate) dev: Option<DevServer>,
}

/// The name a plugin is shown by: its manifest name with dashes as spaces, capitalised, with `api`, `sql`, `http`,
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

/// The group and icon of a plugin with `manifest`: terminals, logs, files and browsers on a page are panes; the API
/// client and databases are tools; a page-less web pane with secrets is a connector (Jira, Figma); the rest is
/// other.
fn group_and_icon(manifest: &Manifest) -> (PluginGroup, IconName) {
    match &manifest.pane {
        PaneSpec::Terminal { .. } | PaneSpec::Logs { .. } => {
            (PluginGroup::Panes, IconName::Terminal)
        }
        PaneSpec::Files => (PluginGroup::Panes, IconName::Git),
        PaneSpec::Web { url: Some(_) } => (PluginGroup::Panes, IconName::ExternalLink),
        PaneSpec::Http { .. } | PaneSpec::Sql { .. } => (PluginGroup::Tools, IconName::Plugin),
        PaneSpec::Web { url: None } if !manifest.secrets.is_empty() => {
            (PluginGroup::Connectors, IconName::Plugin)
        }
        PaneSpec::Web { url: None } => (PluginGroup::Other, IconName::Plugin),
    }
}

/// A manifest default as the user would type it: `{PORT}` and `{SPACE}` filled, a leading `{WORKTREE}/` dropped
/// (paths are relative to the worktree).
fn fill_default(text: &str, setup: &MenuSetup) -> String {
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

/// The setting `manifest`'s plugin asks for when added from the menu, and how it mounts with it; `None` when it
/// mounts on one click (files, connectors, and a terminal or logs pane whose manifest already names what it runs).
/// A terminal or logs pane starts with `setup`'s dev server (else blank: a shell for a terminal), a database with
/// the manifest's, the API client with its base URL and the browser with its page (`http://localhost:{PORT}` by
/// default). Defaults are masked with `redactor`.
pub(crate) fn setting_for(
    manifest: &Manifest,
    setup: &MenuSetup,
    redactor: &Redactor,
) -> Option<(PluginSetting, SettingPlan)> {
    let local = format!("http://localhost:{}", setup.port);
    let dev = setup.dev.as_ref();
    let dev_hint = |blank: &str| match dev {
        Some(dev) => format!(
            "Your web app's dev server in {} on PORT {}. Runs with /bin/sh; {blank}",
            dev.repo, setup.port
        ),
        None => format!("Runs with /bin/sh in the worktree; {blank}"),
    };
    let (label, default, hint, manifest_default) = match &manifest.pane {
        PaneSpec::Terminal { run } if run.is_empty() => (
            "Command",
            dev.map(|d| command_text(&d.command)).unwrap_or_default(),
            dev_hint("blank for your shell."),
            false,
        ),
        PaneSpec::Logs {
            run: None,
            file: None,
        } => (
            "Command or log file",
            dev.map(|d| command_text(&d.command)).unwrap_or_default(),
            dev_hint("or a file: a path ending in .log, .txt or .jsonl, or file:<path>."),
            false,
        ),
        PaneSpec::Sql { database, .. } => (
            "Database",
            fill_default(database, setup),
            "A sqlite file (relative to the worktree) or a postgres:// URL.".to_owned(),
            true,
        ),
        PaneSpec::Http { base_url } => (
            "Base URL",
            base_url
                .as_deref()
                .map_or_else(|| local.clone(), |url| fill_default(url, setup)),
            "Requests typed in the pane are relative to it.".to_owned(),
            true,
        ),
        PaneSpec::Web { url: Some(url) } => (
            "Page",
            fill_default(url, setup),
            "The page it opens with.".to_owned(),
            true,
        ),
        _ => return None,
    };
    let default = xctx_http::mask_url(&default, redactor);
    let repo = match &manifest.pane {
        PaneSpec::Terminal { .. } | PaneSpec::Logs { .. } => dev.map(|d| d.repo.clone()),
        _ => None,
    };
    Some((
        PluginSetting {
            label: label.into(),
            default: default.clone().into(),
            hint: Some(hint.into()),
        },
        SettingPlan {
            plugin: manifest.name.clone(),
            kind: manifest.pane.kind(),
            default,
            manifest_default,
            repo,
        },
    ))
}

/// What a mounted `kind` pane shows, from its resolved `settings` (and the live browser's page, `live_url`, for a
/// web pane), masked with `redactor`: a terminal's command ("Your shell" without one), what a logs pane tails, a
/// database, a base URL, a page, or a files pane's repo. `None` when nothing says.
pub(crate) fn pane_showing(
    kind: &str,
    settings: &PaneSettings,
    live_url: Option<&str>,
    repo: &str,
    redactor: &Redactor,
) -> Option<String> {
    let mask = |text: &str| redactor.redact(text).into_owned();
    let url = |text: &str| xctx_http::mask_url(text, redactor);
    match kind {
        "terminal" => Some(
            settings
                .terminal
                .as_deref()
                .map_or_else(|| "Your shell".to_owned(), |c| mask(&command_text(c))),
        ),
        "logs" => settings
            .logs_command
            .as_deref()
            .map(|c| mask(&command_text(c)))
            .or_else(|| settings.logs_file.as_deref().map(mask)),
        "sql" => settings.database.as_ref().map(|d| url(&d.location)),
        "http" => settings.base_url.as_deref().map(url),
        "web" => settings
            .url
            .as_deref()
            .map(|page| url(live_url.unwrap_or(page))),
        "files" => Some(format!("{} worktree", mask(repo))),
        _ => None,
    }
}

/// The menu for a space (from `setup`): its plugins (`summaries`, from [`Workspace::plugin_menu_entries`]) with
/// their manifests from `registry`, what is mounted (`mounted`, each with what it shows, from [`pane_showing`]),
/// and the registry's load problems, all masked with `redactor`; and how each plugin with a setting mounts. A repo
/// plugin not trusted yet needs trust (mounting posts a gate); the user's own mounts of a connector are not gated.
pub(crate) fn plugin_menu_view(
    summaries: &[PluginSummary],
    registry: &Registry,
    mounted: &[(MountedPlugin, Option<String>)],
    setup: &MenuSetup,
    redactor: &Redactor,
) -> (PluginMenuView, Vec<SettingPlan>) {
    let mask = |text: &str| redactor.redact(text).into_owned();
    let mut plans = Vec::new();
    let plugins = summaries
        .iter()
        .map(|s| {
            let manifest = registry.get(&s.name).map(|p| &p.manifest);
            let (group, icon) =
                manifest.map_or((PluginGroup::Other, IconName::Plugin), group_and_icon);
            let setting =
                manifest
                    .and_then(|m| setting_for(m, setup, redactor))
                    .map(|(setting, plan)| {
                        plans.push(plan);
                        setting
                    });
            PluginEntry {
                name: s.name.clone().into(),
                title: display_name(&s.name).into(),
                description: mask(&s.description).into(),
                icon,
                source: match s.source {
                    ProtocolSource::Builtin => PluginSource::Builtin,
                    ProtocolSource::User => PluginSource::User,
                    ProtocolSource::Repo => PluginSource::Repo,
                },
                group,
                needs_trust: s.trusted == Some(false),
                setting,
            }
        })
        .collect();
    let mut shown: Vec<(&str, Vec<String>)> = Vec::new();
    for (mount, showing) in mounted {
        let index = match shown.iter().position(|(name, _)| *name == mount.plugin) {
            Some(index) => index,
            None => {
                shown.push((&mount.plugin, Vec::new()));
                shown.len() - 1
            }
        };
        if let Some(text) = showing
            && !shown[index].1.contains(text)
        {
            shown[index].1.push(text.clone());
        }
    }
    let mounted = shown
        .into_iter()
        .map(|(name, showing)| MountedEntry {
            name: name.to_owned().into(),
            title: display_name(name).into(),
            icon: registry
                .get(name)
                .map_or(IconName::Plugin, |p| group_and_icon(&p.manifest).1),
            showing: (!showing.is_empty()).then(|| showing.join(" · ").into()),
        })
        .collect();
    let problems = registry
        .problems()
        .iter()
        .map(|p| PluginProblemRow {
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
    let view = PluginMenuView {
        mounted,
        plugins,
        expanded: None,
        problems,
        busy: false,
        error: None,
    };
    (view, plans)
}

/// The settings a mount of a `kind` plugin gets from the `text` typed in its row (blank for none): for logs a log
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

impl Workspace {
    /// Open the Plugins menu of the space being shown. Nothing happens while another dialog is open.
    pub(crate) fn open_plugins_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        let Some(space) = self.active_tab().map(|tab| tab.space.clone()) else {
            return;
        };
        let (view, plans) = self.plugin_menu_for(&space);
        let setting = cx.new(|cx| PromptInput::new(cx).submit_blank(true));
        // Escape never reaches the input: the dialog's Esc binding runs first (see `menu_escape`).
        self.dialog_subscription = Some(cx.subscribe_in(
            &setting,
            window,
            |this, _, event: &PromptEvent, window, cx| match event {
                PromptEvent::Submit(_) => this.menu_submit(window, cx),
                PromptEvent::Cancel => this.menu_expand(None, window, cx),
            },
        ));
        self.dialog = Some(Dialog::Plugins(PluginsState {
            space: space.id.clone(),
            view,
            plans,
            note: None,
            setting,
        }));
        self.dialog_focus.focus(window, cx);
        cx.notify();
    }

    /// Give the keys to the open menu: its open setting's input, else the dialog (so Esc closes it).
    fn focus_plugins_menu(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Dialog::Plugins(state)) = &self.dialog {
            if state.view.expanded.is_some() {
                let focus = state.setting.focus_handle(cx);
                focus.focus(window, cx);
            } else {
                self.dialog_focus.focus(window, cx);
            }
        }
    }

    /// The web dev server a terminal or logs pane added to `space` defaults to: the first web app among its repos,
    /// the focused pane's repo first; `None` when there is none or a mounted pane already runs it.
    fn menu_dev_server(&self, space: &Space, mounted: &[MountedPlugin]) -> Option<DevServer> {
        let focused = self
            .tabs
            .get(&space.id)
            .and_then(|tab| tab.layout.role(tab.layout.focused()))
            .map(|role| role.repo.clone());
        let mut repos: Vec<_> = space.repos.iter().collect();
        repos.sort_by_key(|repo| Some(&repo.name) != focused.as_ref());
        let (repo, app) = repos.into_iter().find_map(|repo| {
            crate::dev_setup::web_app(&repo.worktree).map(|app| (repo.name.clone(), app))
        })?;
        let running = mounted
            .iter()
            .any(|m| m.settings.command.as_ref() == Some(&app.command));
        (!running).then_some(DevServer {
            repo,
            command: app.command,
        })
    }

    fn plugin_menu_for(&mut self, space: &Space) -> (PluginMenuView, Vec<SettingPlan>) {
        let summaries = self.plugin_menu_entries(space);
        let registry = self.plugins.registry(space);
        let mounted = self.mounted_plugins(&space.id);
        let live_url = self.browsers.view(&space.id).and_then(|view| view.url());
        let showing: Vec<(MountedPlugin, Option<String>)> = mounted
            .iter()
            .map(|m| {
                let text = self
                    .plugins
                    .mounted_pane(space, m.pane, &self.redactor)
                    .and_then(|(kind, settings)| {
                        pane_showing(
                            kind,
                            &settings?,
                            live_url.as_deref(),
                            &m.repo,
                            &self.redactor,
                        )
                    });
                (m.clone(), text)
            })
            .collect();
        let setup = MenuSetup {
            port: space.port,
            space: space.id.clone(),
            dev: self.menu_dev_server(space, &mounted),
        };
        plugin_menu_view(&summaries, &registry, &showing, &setup, &self.redactor)
    }

    /// Read the open menu again, showing `error` or `note` from what was just done, with `expanded`'s setting row
    /// open.
    fn refresh_plugins_menu(
        &mut self,
        error: Option<String>,
        note: Option<String>,
        expanded: Option<SharedString>,
    ) {
        let Some(Dialog::Plugins(state)) = &self.dialog else {
            return;
        };
        let Some(space) = self.active_space(&state.space) else {
            return;
        };
        let (mut view, plans) = self.plugin_menu_for(&space);
        view.error = error.map(|e| self.redactor.redact(&e).into_owned().into());
        view.expanded = expanded.filter(|name| plans.iter().any(|p| p.plugin == name.as_ref()));
        if let Some(Dialog::Plugins(state)) = self.dialog.as_mut() {
            state.view = view;
            state.plans = plans;
            state.note = note;
        }
    }

    /// Read the open Plugins menu of `id` again after a plugin was mounted or removed (by the menu, an agent's
    /// `plugin.mount`, or a closed pane), keeping its note and open setting row.
    pub(crate) fn plugins_changed(&mut self, id: &SpaceId) {
        let Some(Dialog::Plugins(state)) = &self.dialog else {
            return;
        };
        if &state.space != id {
            return;
        }
        let (note, expanded) = (state.note.clone(), state.view.expanded.clone());
        self.refresh_plugins_menu(None, note, expanded);
    }

    /// Open the setting row of `name` (its input holding the default, with the keys), or close the open one.
    fn menu_expand(
        &mut self,
        name: Option<&SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Dialog::Plugins(state)) = self.dialog.as_mut() else {
            return;
        };
        let plan = name.and_then(|name| state.plans.iter().find(|p| p.plugin == name.as_ref()));
        state.view.expanded = plan.map(|p| p.plugin.clone().into());
        if let Some(plan) = plan {
            let default = plan.default.clone();
            state
                .setting
                .update(cx, |input, cx| input.set_value(&default, cx));
        }
        self.focus_plugins_menu(window, cx);
        cx.notify();
    }

    /// Esc in the menu: close the open setting row, else the menu.
    fn menu_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.dialog {
            Some(Dialog::Plugins(state)) if state.view.expanded.is_some() => {
                self.menu_expand(None, window, cx);
            }
            _ => self.dismiss_dialog(window, cx),
        }
    }

    /// Mount the open setting row's plugin with what its input holds.
    fn menu_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::Plugins(state)) = &self.dialog else {
            return;
        };
        let Some(plan) = state
            .view
            .expanded
            .as_ref()
            .and_then(|name| state.plans.iter().find(|p| p.plugin == name.as_ref()))
            .cloned()
        else {
            return;
        };
        let text = state.setting.read(cx).value().to_owned();
        let settings = if plan.manifest_default && text.trim() == plan.default.trim() {
            MountSettings::default()
        } else {
            mount_settings_for(plan.kind, &text)
        };
        self.menu_mount(&plan.plugin, plan.repo.clone(), settings, window, cx);
    }

    fn menu_mount(
        &mut self,
        name: &str,
        repo: Option<String>,
        settings: MountSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Dialog::Plugins(state)) = &self.dialog else {
            return;
        };
        let id = state.space.clone();
        let expanded = state.view.expanded.clone();
        let title = display_name(name);
        let (error, note, expanded) = match self
            .mount_plugin(&id, name, repo, settings, false, window, cx)
        {
            Ok(MountOutcome::Mounted(_)) => (None, Some(format!("Added {title}.")), None),
            Ok(MountOutcome::Gated(_)) => (
                None,
                Some(format!(
                    "{title} is a repo plugin you have not trusted yet: approve it in the Inbox (Home) and it mounts."
                )),
                None,
            ),
            // Keep the setting open, so what was typed can be fixed.
            Err(e) => (Some(e.message), None, expanded),
        };
        self.refresh_plugins_menu(error, note, expanded);
        self.focus_plugins_menu(window, cx);
        cx.notify();
    }

    /// Close the menu and focus the first pane of mounted plugin `name`.
    fn menu_show(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::Plugins(state)) = &self.dialog else {
            return;
        };
        let id = state.space.clone();
        let pane = self
            .mounted_plugins(&id)
            .into_iter()
            .find(|m| m.plugin == name)
            .map(|m| m.pane);
        self.dismiss_dialog(window, cx);
        if let Some(pane) = pane
            && let Err(e) = self.focus_pane_in(&id, pane, window, cx)
        {
            self.show_op_error(e, cx);
        }
    }

    fn menu_remove(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::Plugins(state)) = &self.dialog else {
            return;
        };
        let id = state.space.clone();
        let expanded = state.view.expanded.clone();
        let title = display_name(name);
        let (error, note) =
            match self.unmount_plugin(&id, UnmountTarget::Plugin(name.to_owned()), window, cx) {
                Ok(_) => (None, Some(format!("Removed {title}."))),
                Err(e) => (Some(e.message), None),
            };
        self.refresh_plugins_menu(error, note, expanded);
        self.focus_plugins_menu(window, cx);
        cx.notify();
    }

    /// The open Plugins menu's panel.
    pub(crate) fn render_plugins_menu(
        &self,
        state: &PluginsState,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let theme = Theme::of(cx);
        let entity = cx.entity().downgrade();
        let (add, expand, submit) = (entity.clone(), entity.clone(), entity.clone());
        let (show, remove, close) = (entity.clone(), entity.clone(), entity);
        let mut menu = PluginMenu::new(state.view.clone())
            .on_add(move |name, window, cx| {
                let name = name.to_string();
                let _ = add.update(cx, |this, cx| {
                    this.menu_mount(&name, None, MountSettings::default(), window, cx)
                });
            })
            .on_expand(move |name, window, cx| {
                let name = name.cloned();
                let _ = expand.update(cx, |this, cx| this.menu_expand(name.as_ref(), window, cx));
            })
            .on_submit(move |_, window, cx| {
                let _ = submit.update(cx, |this, cx| this.menu_submit(window, cx));
            })
            .on_show(move |name, window, cx| {
                let name = name.to_string();
                let _ = show.update(cx, |this, cx| this.menu_show(&name, window, cx));
            })
            .on_remove(move |name, window, cx| {
                let name = name.to_string();
                let _ = remove.update(cx, |this, cx| this.menu_remove(&name, window, cx));
            })
            .on_close(move |window, cx| {
                let _ = close.update(cx, |this, cx| this.dismiss_dialog(window, cx));
            });
        if state.view.expanded.is_some() {
            menu = menu.setting_input(state.setting.clone());
        }
        let note = state.note.clone().map(|note| {
            div()
                .debug_selector(|| "plugin-menu-note".to_owned())
                .text_size(Theme::TEXT_SM)
                .text_color(theme.text_muted)
                .child(note)
        });
        div()
            .on_action(
                cx.listener(|this, _: &DismissDialog, window, cx| this.menu_escape(window, cx)),
            )
            .flex()
            .flex_col()
            .gap(Theme::SPACE_2)
            .w(px(620.0))
            .max_h(px(600.0))
            .p(Theme::SPACE_3)
            .rounded(Theme::RADIUS_LG)
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .shadow_lg()
            .child(
                div()
                    .text_size(Theme::TEXT_SM)
                    .text_color(theme.text_muted)
                    .child(format!(
                        "{}: new panes open in the plugin column on the right.",
                        state.space
                    )),
            )
            .children(note)
            .child(div().flex_1().min_h_0().child(menu))
    }
}

#[cfg(test)]
mod tests {
    use xctx_layout::PaneId;

    use super::*;

    fn setup(dev: Option<DevServer>) -> MenuSetup {
        MenuSetup {
            port: 4011,
            space: SpaceId::new("fix-retry"),
            dev,
        }
    }

    fn dev_server() -> DevServer {
        DevServer {
            repo: "web".into(),
            command: vec![
                "/bin/sh".into(),
                "-c".into(),
                "[ -d node_modules ] || bun install; exec bun run dev --port \"$PORT\"".into(),
            ],
        }
    }

    fn builtin_view(
        mounted: &[(MountedPlugin, Option<String>)],
        dev: Option<DevServer>,
    ) -> (PluginMenuView, Vec<SettingPlan>) {
        let registry = Registry::with_builtins();
        let summaries: Vec<PluginSummary> = registry
            .plugins()
            .iter()
            .map(|p| crate::plugin_host::plugin_summary(p, None, Vec::new()))
            .collect();
        plugin_menu_view(
            &summaries,
            &registry,
            mounted,
            &setup(dev),
            &Redactor::default(),
        )
    }

    fn mount(pane: u64, plugin: &str, kind: &'static str) -> MountedPlugin {
        MountedPlugin {
            pane: PaneId(pane),
            plugin: plugin.into(),
            repo: "api".into(),
            kind,
            settings: MountSettings::default(),
        }
    }

    #[test]
    fn built_ins_are_grouped_by_purpose_with_friendly_names() {
        let (view, _) = builtin_view(&[], None);
        let group = |group: PluginGroup| -> Vec<String> {
            view.plugins
                .iter()
                .filter(|p| p.group == group)
                .map(|p| p.title.to_string())
                .collect()
        };
        assert_eq!(
            group(PluginGroup::Panes),
            ["Terminal", "Logs", "Browser", "Files"]
        );
        assert_eq!(group(PluginGroup::Tools), ["API client", "Database"]);
        assert_eq!(group(PluginGroup::Connectors), ["Jira", "Figma"]);
        assert!(group(PluginGroup::Other).is_empty());
        assert!(
            view.plugins
                .iter()
                .all(|p| p.source == PluginSource::Builtin)
        );
        // Connectors are not gated when the user mounts them.
        assert!(view.plugins.iter().all(|p| !p.needs_trust));
        assert!(view.mounted.is_empty());
        assert!(view.problems.is_empty());
        assert_eq!(display_name("my-sql-tool"), "My SQL tool");
    }

    #[test]
    fn settings_default_to_the_dev_server_and_the_spaces_port() {
        let (view, plans) = builtin_view(&[], Some(dev_server()));
        let default = |name: &str| {
            view.plugins
                .iter()
                .find(|p| p.name.as_ref() == name)
                .and_then(|p| p.setting.as_ref())
                .map(|s| s.default.to_string())
        };
        let dev = "[ -d node_modules ] || bun install; exec bun run dev --port \"$PORT\"";
        assert_eq!(default("terminal").as_deref(), Some(dev));
        assert_eq!(default("logs").as_deref(), Some(dev));
        assert_eq!(default("browser").as_deref(), Some("http://localhost:4011"));
        assert_eq!(
            default("api-client").as_deref(),
            Some("http://localhost:4011")
        );
        assert_eq!(
            default("database").as_deref(),
            Some("db/development.sqlite3")
        );
        for name in ["files", "jira", "figma"] {
            assert_eq!(default(name), None, "{name} mounts on one click");
        }
        let plan = |name: &str| plans.iter().find(|p| p.plugin == name).cloned();
        assert_eq!(
            plan("terminal").map(|p| (p.manifest_default, p.repo)),
            Some((false, Some("web".to_owned())))
        );
        assert_eq!(
            plan("browser").map(|p| (p.manifest_default, p.repo)),
            Some((true, None))
        );
        // Without a dev server, a terminal starts blank (the user's shell).
        let (view, _) = builtin_view(&[], None);
        let terminal = view.plugins.iter().find(|p| p.name.as_ref() == "terminal");
        assert_eq!(
            terminal
                .and_then(|p| p.setting.as_ref())
                .map(|s| s.default.to_string()),
            Some(String::new())
        );
    }

    #[test]
    fn mounted_plugins_say_what_they_show_once_per_plugin() {
        let mounted = [
            (mount(4, "terminal", "terminal"), Some("npm run dev".into())),
            (
                mount(5, "browser", "web"),
                Some("http://localhost:4011/".into()),
            ),
            (mount(6, "terminal", "terminal"), Some("Your shell".into())),
            (mount(7, "files", "files"), None),
        ];
        let (view, _) = builtin_view(&mounted, None);
        let rows: Vec<(String, Option<String>)> = view
            .mounted
            .iter()
            .map(|m| {
                (
                    m.title.to_string(),
                    m.showing.as_ref().map(|s| s.to_string()),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("Terminal".into(), Some("npm run dev · Your shell".into())),
                ("Browser".into(), Some("http://localhost:4011/".into())),
                ("Files".into(), None),
            ]
        );
    }

    #[test]
    fn a_pane_shows_its_command_page_or_database() {
        let redactor = Redactor::default();
        let terminal = PaneSettings {
            terminal: Some(dev_server().command),
            ..PaneSettings::default()
        };
        assert_eq!(
            pane_showing("terminal", &terminal, None, "web", &redactor).as_deref(),
            Some("[ -d node_modules ] || bun install; exec bun run dev --port \"$PORT\"")
        );
        assert_eq!(
            pane_showing("terminal", &PaneSettings::default(), None, "web", &redactor).as_deref(),
            Some("Your shell")
        );
        let web = PaneSettings {
            url: Some("http://localhost:4011".into()),
            ..PaneSettings::default()
        };
        assert_eq!(
            pane_showing(
                "web",
                &web,
                Some("http://localhost:4011/login"),
                "web",
                &redactor
            )
            .as_deref(),
            Some("http://localhost:4011/login")
        );
        let logs = PaneSettings {
            logs_file: Some("log/development.log".into()),
            ..PaneSettings::default()
        };
        assert_eq!(
            pane_showing("logs", &logs, None, "api", &redactor).as_deref(),
            Some("log/development.log")
        );
        assert_eq!(
            pane_showing("files", &PaneSettings::default(), None, "api", &redactor).as_deref(),
            Some("api worktree")
        );
        // A connector's page-less pane says nothing.
        assert_eq!(
            pane_showing("web", &PaneSettings::default(), None, "api", &redactor),
            None
        );
    }

    #[test]
    fn the_typed_setting_becomes_the_mount_settings() {
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
