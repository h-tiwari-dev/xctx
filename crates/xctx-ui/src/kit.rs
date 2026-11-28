//! GPUI Kit (`gpui-component`) integration: unified icon assets, one-call initialization, and
//! live synchronization from [`Theme`] into `gpui_component::Theme`.
//!
//! All `gpui-component` primitives (dialogs, sheets, popovers, selects, menus, inputs, diffs,
//! rich Markdown `TextView`, tables, trees, scrollbars, tooltips, notifications, etc.) are
//! re-exported via [`gpui_component`] so both `xctx` and `xctx-omp-view` share one themed toolkit.

use std::borrow::Cow;
use std::collections::BTreeSet;

use gpui::{App, AssetSource, Hsla, Result, SharedString, WindowAppearance, px, transparent_black};
pub use gpui_component;
use gpui_component::ThemeMode;
pub use gpui_component::scroll::{ScrollableElement, Scrollbar, ScrollbarAxis};

use crate::icon::UiAssets;
use crate::theme::Theme;

/// Unified [`AssetSource`] serving `xctx-ui`'s embedded Lucide icons first, then falling back to
/// `gpui-kit-assets` so both `xctx-ui` and every `gpui-component` widget resolve their SVG icons
/// with zero runtime file I/O.
#[derive(Debug, Clone, Copy, Default)]
pub struct KitAssets;

impl AssetSource for KitAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = UiAssets.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut merged = BTreeSet::new();
        for item in UiAssets.list(path)? {
            merged.insert(item);
        }
        for item in gpui_kit_assets::Assets.list(path)? {
            merged.insert(item);
        }
        Ok(merged.into_iter().collect())
    }
}

/// Initialize `gpui-component` and sync its global theme to the currently active [`Theme`].
///
/// Call once during application startup (before opening windows) after setting `Theme`.
pub fn init_kit(cx: &mut App) {
    gpui_component::init(cx);
    let theme = Theme::of(cx);
    let appearance = if is_dark_theme(&theme) {
        WindowAppearance::Dark
    } else {
        WindowAppearance::Light
    };
    sync_kit_theme(theme, appearance, cx);
}

/// Whether `theme` is a dark theme (based on its `surface` luminance).
pub fn is_dark_theme(theme: &Theme) -> bool {
    theme.surface.l < 0.5
}

/// Strip translucency from `color` so floating `gpui-component` surfaces (popovers, menus,
/// dialogs, tables) stay crisp even when the window uses [`Theme::translucent`].
fn opaque(color: Hsla) -> Hsla {
    Hsla { a: 1.0, ..color }
}

/// Map `theme` and `appearance` onto `gpui_component::Theme` so every `gpui-component` widget
/// (scrollbars, Markdown `TextView`, `Diff`, `Input`, `Popover`, `Menu`, `Dialog`, `Sheet`,
/// `Notification`, etc.) matches xctx's dark/light palette and radii.
pub fn sync_kit_theme(theme: Theme, appearance: WindowAppearance, cx: &mut App) {
    if !cx.has_global::<gpui_component::Theme>() {
        gpui_component::init(cx);
    }

    let mode = match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeMode::Light,
        WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
    };
    if gpui_component::Theme::global(cx).mode != mode {
        gpui_component::Theme::change(mode, None, cx);
    }

    let bg = opaque(theme.chrome);
    let surface = opaque(theme.surface);
    let raised = opaque(theme.raised);
    let overlay = opaque(theme.overlay);
    let danger_hover = theme.danger.alpha(0.88);
    let danger_active = theme.danger.alpha(0.78);
    let success_hover = theme.success.alpha(0.88);
    let success_active = theme.success.alpha(0.78);
    let warning_hover = theme.warning.alpha(0.88);
    let warning_active = theme.warning.alpha(0.78);

    gpui_component::Theme::update(cx, |kit| {
        // Keep GPUI's default 16px rem scale so existing xctx layouts stay unchanged.
        kit.font_size = px(16.0);
        kit.mono_font_family = "Menlo".into();
        kit.mono_font_size = Theme::TEXT_SM;
        kit.radius = Theme::RADIUS_MD;
        kit.radius_lg = Theme::RADIUS_LG;

        let c = &mut kit.colors;
        c.background = bg;
        c.foreground = theme.text;
        c.border = theme.border;
        c.input = theme.border;
        c.ring = theme.accent;
        c.caret = theme.accent;
        c.selection = theme.selection;

        // In gpui-component's shadcn convention, `accent` is hover surface on list/menu items
        // while `primary` is the brand accent fill.
        c.accent = theme.hover;
        c.accent_foreground = theme.text;
        c.primary = theme.accent;
        c.primary_hover = theme.accent_hover;
        c.primary_active = theme.accent_hover;
        c.primary_foreground = theme.on_accent;

        c.secondary = raised;
        c.secondary_hover = overlay;
        c.secondary_active = overlay;
        c.secondary_foreground = theme.text;

        c.muted = surface;
        c.muted_foreground = theme.text_muted;
        c.popover = overlay;
        c.popover_foreground = theme.text;
        c.overlay = theme.scrim;

        c.link = theme.accent;
        c.link_hover = theme.accent_hover;
        c.link_active = theme.accent_hover;

        c.danger = theme.danger;
        c.danger_hover = danger_hover;
        c.danger_active = danger_active;
        c.danger_foreground = theme.on_accent;

        c.success = theme.success;
        c.success_hover = success_hover;
        c.success_active = success_active;
        c.success_foreground = theme.on_accent;

        c.warning = theme.warning;
        c.warning_hover = warning_hover;
        c.warning_active = warning_active;
        c.warning_foreground = theme.on_accent;

        c.info = theme.accent;
        c.info_hover = theme.accent_hover;
        c.info_active = theme.accent_hover;
        c.info_foreground = theme.on_accent;

        c.button = raised;
        c.button_hover = overlay;
        c.button_active = overlay;
        c.button_foreground = theme.text;
        c.button_primary = theme.accent;
        c.button_primary_hover = theme.accent_hover;
        c.button_primary_active = theme.accent_hover;
        c.button_primary_foreground = theme.on_accent;
        c.button_secondary = raised;
        c.button_secondary_hover = overlay;
        c.button_secondary_active = overlay;
        c.button_secondary_foreground = theme.text;
        c.button_danger = theme.danger;
        c.button_danger_hover = danger_hover;
        c.button_danger_active = danger_active;
        c.button_danger_foreground = theme.on_accent;
        c.button_success = theme.success;
        c.button_success_hover = success_hover;
        c.button_success_active = success_active;
        c.button_success_foreground = theme.on_accent;
        c.button_warning = theme.warning;
        c.button_warning_hover = warning_hover;
        c.button_warning_active = warning_active;
        c.button_warning_foreground = theme.on_accent;
        c.button_info = theme.accent;
        c.button_info_hover = theme.accent_hover;
        c.button_info_active = theme.accent_hover;
        c.button_info_foreground = theme.on_accent;

        c.accordion = surface;
        c.group_box = raised;
        c.group_box_foreground = theme.text;
        c.description_list_label = raised;
        c.description_list_label_foreground = theme.text_muted;

        c.list = transparent_black();
        c.list_head = raised;
        c.list_hover = theme.hover;
        c.list_active = theme.selection;
        c.list_active_border = theme.accent;
        c.list_even = raised.alpha(0.28);
        c.list_highlight_foreground = theme.accent;

        c.table = surface;
        c.table_head = raised;
        c.table_head_foreground = theme.text;
        c.table_foot = raised;
        c.table_foot_foreground = theme.text_muted;
        c.table_hover = theme.hover;
        c.table_active = theme.selection;
        c.table_active_border = theme.accent;
        c.table_even = raised.alpha(0.28);
        c.table_row_border = theme.border_subtle;

        c.tab_bar = bg;
        c.tab_bar_segmented = raised;
        c.tab = transparent_black();
        c.tab_foreground = theme.text_muted;
        c.tab_active = surface;
        c.tab_active_foreground = theme.text;

        c.sidebar = theme.chrome;
        c.sidebar_foreground = theme.text;
        c.sidebar_border = theme.border_subtle;
        c.sidebar_accent = theme.hover;
        c.sidebar_accent_foreground = theme.text;
        c.sidebar_primary = theme.accent;
        c.sidebar_primary_foreground = theme.on_accent;

        c.title_bar = theme.chrome;
        c.title_bar_border = theme.border_subtle;
        c.status_bar = theme.chrome;
        c.status_bar_border = theme.border_subtle;

        c.scrollbar = transparent_black();
        c.scrollbar_thumb = theme.text_subtle.alpha(0.42);
        c.scrollbar_thumb_hover = theme.text_muted.alpha(0.70);

        c.drag_border = theme.accent;
        c.drop_target = theme.selection;
        c.progress_bar = theme.accent;
        c.skeleton = raised;
        c.slider_bar = theme.accent;
        c.slider_thumb = theme.text;
        c.switch = theme.border_strong;
        c.switch_thumb = theme.text;

        c.chart_1 = theme.agent[0];
        c.chart_2 = theme.agent[1];
        c.chart_3 = theme.agent[2];
        c.chart_4 = theme.agent[3];
        c.chart_5 = theme.agent[4];
        c.chart_bullish = theme.success;
        c.chart_bearish = theme.danger;
        c.chart_grid = theme.border_subtle;

        c.red = theme.danger;
        c.red_light = theme.danger.alpha(0.18);
        c.green = theme.success;
        c.green_light = theme.success.alpha(0.18);
        c.blue = theme.accent;
        c.blue_light = theme.accent.alpha(0.18);
        c.yellow = theme.warning;
        c.yellow_light = theme.warning.alpha(0.18);
        c.magenta = theme.agent[0];
        c.magenta_light = theme.agent[0].alpha(0.18);
        c.cyan = theme.agent[1];
        c.cyan_light = theme.agent[1].alpha(0.18);
    });
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use gpui_component::IconNamed as _;

    use super::*;
    use crate::icon::IconName;

    #[test]
    fn kit_assets_resolves_both_xctx_and_gpui_component_icons() {
        let assets = KitAssets;
        assert!(assets.load(IconName::Check.path()).unwrap().is_some());
        assert!(
            assets
                .load(IconName::Orchestrator.path())
                .unwrap()
                .is_some()
        );
        // A bundled gpui-component icon that xctx-ui does not define itself.
        let kit_path = gpui_component::IconName::Info.path();
        assert!(assets.load(kit_path.as_ref()).unwrap().is_some());
        let listed = assets.list("icons").unwrap();
        assert!(listed.len() > IconName::ALL.len());
    }

    #[gpui::test]
    fn init_and_sync_kit_theme_tracks_dark_and_light(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(Theme::dark());
            init_kit(cx);
            let dark_kit = gpui_component::Theme::global(cx);
            assert_eq!(dark_kit.mode, ThemeMode::Dark);
            assert_eq!(dark_kit.colors.primary, Theme::dark().accent);
            assert_eq!(dark_kit.colors.foreground, Theme::dark().text);

            let light = Theme::light().translucent();
            cx.set_global(light);
            sync_kit_theme(light, WindowAppearance::Light, cx);
            let light_kit = gpui_component::Theme::global(cx);
            assert_eq!(light_kit.mode, ThemeMode::Light);
            assert_eq!(light_kit.colors.primary, light.accent);
            assert_eq!(light_kit.colors.background.a, 1.0);
        });
    }
}
