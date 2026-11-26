//! xctx: a native macOS agent multiplexer. One window holds Home and a tab per living space; each space runs its
//! agents and shells in its own worktrees.

mod add_agent;
mod add_repo;
mod agent_brief;
mod agent_session;
mod agent_skills;
mod attempt_jobs;
mod attempt_ui;
mod attempts;
mod control;
mod dev_setup;
mod dialogs;
mod flow;
mod home;
mod inbox_view;
mod keymap;
mod lifecycle;
#[cfg(test)]
mod lifecycle_tests;
mod omp_agent;
mod omp_panes;
mod overlays;
mod parent_agent;
mod persist;
mod plugin_backend;
mod plugin_browser;
mod plugin_host;
mod plugin_jobs;
mod plugin_panes;
#[cfg(test)]
mod plugin_tests;
mod repo_links;
mod serve_hints;
mod shell;
mod space_graph;
mod space_layout;
mod space_tab;
mod spawn;
mod tool_dock;
mod ui_prefs;
mod workspace;

use gpui::{
    App, AppContext, Bounds, Menu, MenuItem, SystemMenuType, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, point, px, size,
};
use xctx_ui::{Theme, UiAssets};

use crate::keymap::{Quit, bind_keys};
use crate::workspace::Workspace;

fn main() {
    gpui_platform::application()
        .with_assets(UiAssets)
        .run(|cx: &mut App| {
            cx.set_global(Theme::dark());
            bind_keys(cx);
            // The workspace handles Quit itself when its window is active; this covers the rest.
            cx.on_action(|_: &Quit, cx| {
                let busy = cx
                    .windows()
                    .into_iter()
                    .filter_map(|window| window.downcast::<Workspace>())
                    .any(|window| {
                        window
                            .update(cx, |workspace, _, cx| workspace.refuse_while_busy(cx))
                            .unwrap_or(false)
                    });
                if !busy {
                    cx.quit();
                }
            });
            cx.set_menus(vec![Menu::new(xctx_core::APP_NAME).items([
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Quit xctx", Quit),
            ])]);
            cx.on_window_closed(|cx, _window_id| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let prefs = ui_prefs::UiPrefs::load(xctx_core::store::data_dir().ok().as_deref());
            let bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);
            let (light_x, light_y) = Theme::TRAFFIC_LIGHT_POSITION;
            let opened = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some(xctx_core::APP_NAME.into()),
                        // The window draws its own title bar row (Roster toggle, space header) under the traffic
                        // lights, and moves itself from that row's empty parts.
                        appears_transparent: true,
                        traffic_light_position: Some(point(light_x, light_y)),
                    }),
                    app_owns_titlebar_drag: true,
                    window_background: if prefs.translucent {
                        WindowBackgroundAppearance::Blurred
                    } else {
                        WindowBackgroundAppearance::Opaque
                    },
                    window_min_size: Some(size(px(760.0), px(480.0))),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Workspace::new(prefs, window, cx)),
            );
            if let Err(e) = opened {
                eprintln!("xctx: could not open the main window: {e}");
                cx.quit();
                return;
            }

            cx.activate(true);
        });
}
