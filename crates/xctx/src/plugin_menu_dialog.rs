//! The Plugins menu of a space (`⌘⇧P`, or Plugins in the tab bar): what the space can mount, Mount and Unmount,
//! and the plugin folders whose manifest could not be used.
//!
//! Mounting here is the user's own (`from_agent = false`), so only an untrusted repo plugin waits in the Inbox.
//! An optional target typed above the list is given to the next mount as its settings (see
//! [`mount_settings_for`]): a command or a log file for a logs pane, a command for a terminal, a database for
//! a database pane, a base URL or a page for the API client or the browser.

use gpui::{Context, Entity, Focusable, ParentElement, Styled, Window, div, prelude::*, px};
use xctx_control::protocol::{PluginSource as ProtocolSource, PluginSummary};
use xctx_core::{Redactor, Space, SpaceId};
use xctx_plugin::{Registry, SqlEngine};
use xctx_ui::{
    PluginEntry, PluginMenu, PluginMenuView, PluginProblemRow, PluginSource, PromptEvent,
    PromptInput, Theme,
};

use crate::dialogs::Dialog;
use crate::plugin_host::{MountSettings, MountedPlugin};
use crate::plugin_jobs::{MountOutcome, UnmountTarget};
use crate::workspace::Workspace;

/// The open Plugins menu.
pub(crate) struct PluginsState {
    pub(crate) space: SpaceId,
    view: PluginMenuView,
    /// What the last action did (a mount waiting in the Inbox, say).
    note: Option<String>,
    /// The optional target for the next mount.
    target: Entity<PromptInput>,
}

/// The menu for `space`: its plugins (`summaries`, from [`Workspace::plugin_menu_entries`]) with their pane kinds
/// from `registry`, which are mounted (`mounted`), and the registry's load problems, all masked with
/// `redactor`. A repo plugin not trusted yet needs trust (mounting posts a gate); the user's own mounts of a
/// connector are not gated.
pub(crate) fn plugin_menu_view(
    summaries: &[PluginSummary],
    registry: &Registry,
    mounted: &[MountedPlugin],
    redactor: &Redactor,
) -> PluginMenuView {
    let mask = |text: &str| redactor.redact(text).into_owned();
    let plugins = summaries
        .iter()
        .map(|s| PluginEntry {
            name: s.name.clone().into(),
            description: mask(&s.description).into(),
            source: match s.source {
                ProtocolSource::Builtin => PluginSource::Builtin,
                ProtocolSource::User => PluginSource::User,
                ProtocolSource::Repo => PluginSource::Repo,
            },
            pane: registry.get(&s.name).map(|p| p.manifest.pane.kind().into()),
            needs_trust: s.trusted == Some(false),
            mounted: mounted.iter().any(|m| m.plugin == s.name),
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
    PluginMenuView {
        plugins,
        problems,
        busy: false,
        error: None,
    }
}

/// The settings a mount of a `kind` plugin gets from the menu's target `text` (blank for none): for logs a log
/// file when it starts with `file:` or ends in `.log`, `.txt` or `.jsonl`, else a command; for a terminal a
/// command; for a database its location (postgres for a `postgres://` URL, else a sqlite file); for the API
/// client its base URL; for the browser its page. Commands are split on whitespace and run without a shell.
pub(crate) fn mount_settings_for(kind: &str, text: &str) -> MountSettings {
    let text = text.trim();
    let mut settings = MountSettings::default();
    if text.is_empty() {
        return settings;
    }
    let command = || Some(text.split_whitespace().map(str::to_owned).collect());
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
        let view = self.plugin_menu_for(&space);
        let target = cx.new(|cx| {
            PromptInput::new(cx).placeholder(
                "Optional, for the next mount: a command or log file (logs, terminal), a database, a URL",
            )
        });
        self.dialog_subscription = Some(cx.subscribe_in(
            &target,
            window,
            |this, _, event: &PromptEvent, window, cx| {
                if let PromptEvent::Cancel = event {
                    this.dismiss_dialog(window, cx);
                }
            },
        ));
        // The keys go to the target, so what is typed before Mount is not lost; Esc there closes the menu (and
        // the dialog's own Esc binding still applies, as the input sits inside the dialog's key context).
        let focus = target.focus_handle(cx);
        self.dialog = Some(Dialog::Plugins(PluginsState {
            space: space.id.clone(),
            view,
            note: None,
            target,
        }));
        focus.focus(window, cx);
        cx.notify();
    }

    /// Give the keys back to the open menu's target, after a click on the menu or a mount moved them.
    fn focus_plugins_target(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Dialog::Plugins(state)) = &self.dialog {
            let focus = state.target.focus_handle(cx);
            focus.focus(window, cx);
        }
    }

    fn plugin_menu_for(&mut self, space: &Space) -> PluginMenuView {
        let summaries = self.plugin_menu_entries(space);
        let registry = self.plugins.registry(space);
        let mounted = self.mounted_plugins(&space.id);
        plugin_menu_view(&summaries, &registry, &mounted, &self.redactor)
    }

    /// Read the open menu's plugins again, showing `error` or `note` from what was just done.
    fn refresh_plugins_menu(&mut self, error: Option<String>, note: Option<String>) {
        let Some(Dialog::Plugins(state)) = &self.dialog else {
            return;
        };
        let Some(space) = self.active_space(&state.space) else {
            return;
        };
        let mut view = self.plugin_menu_for(&space);
        view.error = error.map(|e| self.redactor.redact(&e).into_owned().into());
        if let Some(Dialog::Plugins(state)) = self.dialog.as_mut() {
            state.view = view;
            state.note = note;
        }
    }

    fn menu_mount(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::Plugins(state)) = &self.dialog else {
            return;
        };
        let id = state.space.clone();
        let kind = state
            .view
            .plugins
            .iter()
            .find(|p| p.name.as_ref() == name)
            .and_then(|p| p.pane.clone())
            .unwrap_or_default();
        let target = state.target.clone();
        let settings = mount_settings_for(&kind, target.read(cx).value());
        let (error, note) = match self.mount_plugin(&id, name, None, settings, false, window, cx) {
            Ok(MountOutcome::Mounted(_)) => {
                target.update(cx, |input, cx| input.clear(cx));
                (None, Some(format!("Mounted {name}.")))
            }
            Ok(MountOutcome::Gated(_)) => (
                None,
                Some(format!(
                    "{name} is a repo plugin you have not trusted yet: approve it in the Inbox (Home) and it mounts."
                )),
            ),
            Err(e) => (Some(e.message), None),
        };
        self.refresh_plugins_menu(error, note);
        self.focus_plugins_target(window, cx);
        cx.notify();
    }

    fn menu_unmount(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::Plugins(state)) = &self.dialog else {
            return;
        };
        let id = state.space.clone();
        let (error, note) =
            match self.unmount_plugin(&id, UnmountTarget::Plugin(name.to_owned()), window, cx) {
                Ok(_) => (None, Some(format!("Unmounted {name}."))),
                Err(e) => (Some(e.message), None),
            };
        self.refresh_plugins_menu(error, note);
        self.focus_plugins_target(window, cx);
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
        let (mount, unmount, close) = (entity.clone(), entity.clone(), entity);
        let menu = PluginMenu::new(state.view.clone())
            .on_mount(move |name, window, cx| {
                let name = name.to_string();
                let _ = mount.update(cx, |this, cx| this.menu_mount(&name, window, cx));
            })
            .on_unmount(move |name, window, cx| {
                let name = name.to_string();
                let _ = unmount.update(cx, |this, cx| this.menu_unmount(&name, window, cx));
            })
            .on_close(move |window, cx| {
                let _ = close.update(cx, |this, cx| this.dismiss_dialog(window, cx));
            });
        let note = state.note.clone().map(|note| {
            div()
                .debug_selector(|| "plugin-menu-note".to_owned())
                .text_size(Theme::TEXT_SM)
                .text_color(theme.text_muted)
                .child(note)
        });
        div()
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
                        "Plugins in {}. A new plugin pane goes into the plugin column on the right; ⌘W on it unmounts it.",
                        state.space
                    )),
            )
            .child(state.target.clone())
            .children(note)
            .child(div().flex_1().min_h_0().child(menu))
    }
}

#[cfg(test)]
mod tests {
    use xctx_layout::PaneId;

    use super::*;

    #[test]
    fn the_menu_lists_plugins_with_their_panes_and_what_is_mounted() {
        let registry = Registry::with_builtins();
        let summaries: Vec<PluginSummary> = registry
            .plugins()
            .iter()
            .map(|p| crate::plugin_host::plugin_summary(p, None, Vec::new()))
            .collect();
        let mounted = [MountedPlugin {
            pane: PaneId(4),
            plugin: "logs".into(),
            repo: "api".into(),
            kind: "logs",
            settings: MountSettings::default(),
        }];
        let view = plugin_menu_view(&summaries, &registry, &mounted, &Redactor::default());
        let entry = |name: &str| {
            view.plugins
                .iter()
                .find(|p| p.name.as_ref() == name)
                .cloned()
        };
        let logs = entry("logs").ok_or("no logs");
        assert_eq!(logs.as_ref().map(|l| l.mounted), Ok(true));
        assert_eq!(
            logs.as_ref()
                .map(|l| l.pane.as_ref().map(|p| p.to_string())),
            Ok(Some("logs".to_owned()))
        );
        let api = entry("api-client").ok_or("no api-client");
        assert_eq!(
            api.map(|a| (a.mounted, a.needs_trust, a.source)),
            Ok((false, false, PluginSource::Builtin))
        );
        // Connectors are not gated when the user mounts them.
        assert_eq!(entry("jira").map(|j| j.needs_trust), Some(false));
        assert!(view.problems.is_empty());
    }

    #[test]
    fn the_menu_target_becomes_the_mount_settings() {
        let logs = mount_settings_for("logs", " tail -F /var/log/app.log ");
        assert_eq!(
            logs.command,
            Some(vec!["tail".into(), "-F".into(), "/var/log/app.log".into()])
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
        assert_eq!(
            mount_settings_for("terminal", "k9s").command,
            Some(vec!["k9s".into()])
        );
        assert_eq!(mount_settings_for("logs", "  "), MountSettings::default());
    }
}
