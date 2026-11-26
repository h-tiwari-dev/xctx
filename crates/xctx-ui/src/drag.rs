//! What can be dragged inside the window, and the small chip that follows the pointer while it is.
//!
//! A pane header drags a [`DraggedPane`] and a tool dock item drags a [`DraggedTool`]; both are dropped on a pane
//! of the [`crate::SplitView`], at the [`DropEdge`] under the pointer. Files dragged in from Finder arrive as GPUI's
//! own `ExternalPaths`.

use gpui::{
    Context, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Window, div,
};
use xctx_layout::PaneId;

pub use xctx_layout::DropEdge;

use crate::icon::{Icon, IconName, IconSize};
use crate::theme::Theme;

/// A pane being dragged by its header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraggedPane {
    /// The pane being moved.
    pub pane: PaneId,
    /// Its header title, shown on the drag chip.
    pub title: SharedString,
    /// Its icon, shown on the drag chip.
    pub icon: IconName,
}

/// A tool being dragged out of the tool dock, to open it beside (or in place of) a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraggedTool {
    /// The tool's manifest name, as in [`crate::DockItem::name`].
    pub name: SharedString,
    /// The name shown on the drag chip.
    pub title: SharedString,
    /// Its icon.
    pub icon: IconName,
}

/// The chip that follows the pointer during a drag: an icon and a label on an overlay surface. Build one with
/// `cx.new(|_| DragPreview::new(..))` in an `on_drag` constructor.
pub struct DragPreview {
    icon: IconName,
    label: SharedString,
}

impl DragPreview {
    /// A chip showing `icon` and `label`.
    pub fn new(icon: IconName, label: impl Into<SharedString>) -> Self {
        Self {
            icon,
            label: label.into(),
        }
    }
}

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .flex()
            .items_center()
            .gap(Theme::SPACE_2)
            .h(gpui::px(28.0))
            .px(Theme::SPACE_3)
            .rounded(Theme::RADIUS_MD)
            .border_1()
            .border_color(theme.accent.alpha(0.6))
            .bg(theme.overlay)
            .shadow_lg()
            .text_size(Theme::TEXT_SM)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.text)
            .child(Icon::new(self.icon).size(IconSize::Sm).color(theme.accent))
            .child(self.label.clone())
    }
}
