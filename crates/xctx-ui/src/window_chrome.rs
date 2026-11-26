//! The window has no system title bar (it is transparent, and the app owns dragging it), so the parts of the
//! title bar row that are not controls move the window, and a double-click on them does what the system's
//! title bar double-click does (zoom or minimise, from the user's settings).

use gpui::{ElementId, InteractiveElement, MouseButton, Stateful, Styled, div};

/// An empty, growing area of the title bar row that moves the window when dragged and zooms it (or what the
/// user chose in System Settings) when double-clicked. Put it between the row's controls; never put controls
/// inside it, as a press anywhere in it starts a window move.
pub fn window_drag_area(id: impl Into<ElementId>) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .h_full()
        .on_mouse_down(MouseButton::Left, |event, window, _| {
            if event.click_count >= 2 {
                window.titlebar_double_click();
            } else {
                window.start_window_move();
            }
        })
}
