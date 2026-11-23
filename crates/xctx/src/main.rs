use gpui::{
    App, Bounds, Context, FocusHandle, KeyBinding, Menu, MenuItem, SystemMenuType, TitlebarOptions,
    Window, WindowBounds, WindowOptions, actions, div, prelude::*, px, rgb, size,
};

actions!(xctx, [CloseWindow, Quit]);

struct RootView {
    focus_handle: FocusHandle,
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .on_action(|_: &CloseWindow, window, _| {
                window.remove_window();
            })
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .bg(rgb(0x18181b))
            .text_color(rgb(0xf4f4f5))
            .child(div().text_2xl().child(xctx_core::APP_NAME))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0xa1a1aa))
                    .child(xctx_core::placeholder_message()),
            )
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-w", CloseWindow, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
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

        let bounds = Bounds::centered(None, size(px(960.0), px(640.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(xctx_core::APP_NAME.into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let focus_handle = cx.focus_handle();
                    focus_handle.focus(window, cx);
                    RootView { focus_handle }
                })
            },
        )
        .expect("failed to open main window");

        cx.activate(true);
    });
}
