//! The window chrome of xctx, drawn with GPUI: the theme, the tab bar, the split view over an `xctx-layout`
//! pane tree, the prompt input, the status bar, and the small buttons, pills and badges they share.
//!
//! Components read their colours from the [`Theme`] global when one is set (`cx.set_global(Theme::dark())`),
//! and fall back to [`Theme::dark`] otherwise. They hold no app state: the app passes in what to show and
//! receives what the user did through callbacks and [`PromptEvent`]s.

pub mod components;
pub mod prompt;
pub mod split_view;
pub mod status_bar;
pub mod tab_bar;
pub mod theme;

pub use components::{Badge, Button, ButtonColors, ButtonSize, ButtonVariant, Pill};
pub use prompt::{PromptBuffer, PromptEvent, PromptInput};
pub use split_view::{PaneView, SplitView, pane_kind_label, ratio_for_drag};
pub use status_bar::{
    BannerKind, KeyHint, StatusBanner, StatusBar, default_key_hints, format_key_hints,
};
pub use tab_bar::{SHAPES, TabBar, TabItem, shape_label};
pub use theme::{Theme, Tone, ToneColors};
