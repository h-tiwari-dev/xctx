//! Embedded Lucide SVG icons (ISC licence) and the [`Icon`] GPUI component.
//!
//! All SVGs are compiled directly into the binary via `include_bytes!` and exposed through
//! [`UiAssets`] (implementing [`gpui::AssetSource`]), so rendering icons performs zero runtime
//! file I/O. Register [`UiAssets`] when starting the application:
//!
//! ```ignore
//! gpui_platform::application()
//!     .with_assets(xctx_ui::UiAssets)
//!     .run(|cx| { /* ... */ });
//! ```

use std::borrow::Cow;

use gpui::{
    App, AssetSource, Hsla, Pixels, Radians, Result, SharedString, Transformation, Window,
    prelude::*, px, svg,
};

use crate::theme::{Theme, Tone};

/// Every built-in icon bundled with `xctx-ui` (Lucide, ISC licence).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconName {
    // States
    /// Active / running spinner ring.
    Running,
    /// Paused state indicator.
    Paused,
    /// Warning / needs attention triangle.
    Warning,
    /// Error / failed circle-x.
    Error,
    /// Idle / quiet circle.
    Idle,
    /// Completed / approved checkmark.
    Check,

    // Roles and pane kinds
    /// Orchestrator agent coordinating subagents.
    Orchestrator,
    /// Writer agent editing a repository.
    Writer,
    /// Read-only helper agent.
    Helper,
    /// Shell / terminal pane.
    Terminal,
    /// Git branch / diff pane.
    Git,
    /// Plugin pane or extension.
    Plugin,

    // Shell and pane actions
    /// Home view.
    Home,
    /// Inbox / gates overlay.
    Inbox,
    /// Search / command palette.
    Search,
    /// Left sidebar / Roster toggle.
    Sidebar,
    /// Right Inspector panel toggle.
    Inspector,
    /// Settings view.
    Settings,
    /// Create / add item.
    Plus,
    /// Close / dismiss control.
    Close,
    /// Collapsed disclosure chevron.
    ChevronRight,
    /// Expanded disclosure chevron.
    ChevronDown,
    /// Split pane horizontally (side by side).
    SplitH,
    /// Split pane vertically (top and bottom).
    SplitV,
    /// Maximize / zoom pane.
    Maximize,
    /// Ship a space.
    Ship,
    /// Discard / delete action.
    Trash,
    /// Fork competing attempts.
    Fork,
    /// Start or resume execution.
    Play,
    /// Pause execution.
    Pause,
    /// Refresh / reload.
    Refresh,
    /// Open external link.
    ExternalLink,
    /// Overflow menu (`…`).
    More,

    // Tools
    /// Locked: a tool waiting for the user's approval.
    Lock,
    /// A web page: the browser tool.
    Globe,
    /// A database tool.
    Database,
    /// A folder: the files tool.
    Folder,
    /// A text file: the logs tool.
    FileText,
    /// Sending a request: the API client tool.
    Send,
    /// A ticket: an issue tracker connector.
    Ticket,
    /// A pen tool: a design connector.
    PenTool,
    /// A grip of dots: a drag handle.
    Grip,
    /// Two arrows pointing inwards: end a zoom.
    Minimize,
    /// Stacked sheets: window appearance (translucency).
    Layers,
    /// The command key: the command palette.
    Command,
}

impl IconName {
    /// All bundled icons in declaration order, grouped by state, role and action.
    pub const ALL: &'static [Self] = &[
        Self::Running,
        Self::Paused,
        Self::Warning,
        Self::Error,
        Self::Idle,
        Self::Check,
        Self::Orchestrator,
        Self::Writer,
        Self::Helper,
        Self::Terminal,
        Self::Git,
        Self::Plugin,
        Self::Home,
        Self::Inbox,
        Self::Search,
        Self::Sidebar,
        Self::Inspector,
        Self::Settings,
        Self::Plus,
        Self::Close,
        Self::ChevronRight,
        Self::ChevronDown,
        Self::SplitH,
        Self::SplitV,
        Self::Maximize,
        Self::Ship,
        Self::Trash,
        Self::Fork,
        Self::Play,
        Self::Pause,
        Self::Refresh,
        Self::ExternalLink,
        Self::More,
        Self::Lock,
        Self::Globe,
        Self::Database,
        Self::Folder,
        Self::FileText,
        Self::Send,
        Self::Ticket,
        Self::PenTool,
        Self::Grip,
        Self::Minimize,
        Self::Layers,
        Self::Command,
    ];

    /// The asset path used with [`UiAssets`] and [`gpui::svg`].
    pub const fn path(self) -> &'static str {
        match self {
            Self::Running => "icons/running.svg",
            Self::Paused => "icons/paused.svg",
            Self::Warning => "icons/warning.svg",
            Self::Error => "icons/error.svg",
            Self::Idle => "icons/idle.svg",
            Self::Check => "icons/check.svg",
            Self::Orchestrator => "icons/orchestrator.svg",
            Self::Writer => "icons/writer.svg",
            Self::Helper => "icons/helper.svg",
            Self::Terminal => "icons/terminal.svg",
            Self::Git => "icons/git.svg",
            Self::Plugin => "icons/plugin.svg",
            Self::Home => "icons/home.svg",
            Self::Inbox => "icons/inbox.svg",
            Self::Search => "icons/search.svg",
            Self::Sidebar => "icons/sidebar.svg",
            Self::Inspector => "icons/inspector.svg",
            Self::Settings => "icons/settings.svg",
            Self::Plus => "icons/plus.svg",
            Self::Close => "icons/close.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::SplitH => "icons/split-h.svg",
            Self::SplitV => "icons/split-v.svg",
            Self::Maximize => "icons/maximize.svg",
            Self::Ship => "icons/ship.svg",
            Self::Trash => "icons/trash.svg",
            Self::Fork => "icons/fork.svg",
            Self::Play => "icons/play.svg",
            Self::Pause => "icons/pause.svg",
            Self::Refresh => "icons/refresh.svg",
            Self::ExternalLink => "icons/external-link.svg",
            Self::More => "icons/more.svg",
            Self::Lock => "icons/lock.svg",
            Self::Globe => "icons/globe.svg",
            Self::Database => "icons/database.svg",
            Self::Folder => "icons/folder.svg",
            Self::FileText => "icons/file-text.svg",
            Self::Send => "icons/send.svg",
            Self::Ticket => "icons/ticket.svg",
            Self::PenTool => "icons/pen-tool.svg",
            Self::Grip => "icons/grip.svg",
            Self::Minimize => "icons/minimize.svg",
            Self::Layers => "icons/layers.svg",
            Self::Command => "icons/command.svg",
        }
    }

    /// Short kebab-case name matching the SVG file stem.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Idle => "idle",
            Self::Check => "check",
            Self::Orchestrator => "orchestrator",
            Self::Writer => "writer",
            Self::Helper => "helper",
            Self::Terminal => "terminal",
            Self::Git => "git",
            Self::Plugin => "plugin",
            Self::Home => "home",
            Self::Inbox => "inbox",
            Self::Search => "search",
            Self::Sidebar => "sidebar",
            Self::Inspector => "inspector",
            Self::Settings => "settings",
            Self::Plus => "plus",
            Self::Close => "close",
            Self::ChevronRight => "chevron-right",
            Self::ChevronDown => "chevron-down",
            Self::SplitH => "split-h",
            Self::SplitV => "split-v",
            Self::Maximize => "maximize",
            Self::Ship => "ship",
            Self::Trash => "trash",
            Self::Fork => "fork",
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Refresh => "refresh",
            Self::ExternalLink => "external-link",
            Self::More => "more",
            Self::Lock => "lock",
            Self::Globe => "globe",
            Self::Database => "database",
            Self::Folder => "folder",
            Self::FileText => "file-text",
            Self::Send => "send",
            Self::Ticket => "ticket",
            Self::PenTool => "pen-tool",
            Self::Grip => "grip",
            Self::Minimize => "minimize",
            Self::Layers => "layers",
            Self::Command => "command",
        }
    }

    /// Raw embedded SVG bytes for this icon.
    pub const fn bytes(self) -> &'static [u8] {
        match self {
            Self::Running => include_bytes!("../assets/icons/running.svg"),
            Self::Paused => include_bytes!("../assets/icons/paused.svg"),
            Self::Warning => include_bytes!("../assets/icons/warning.svg"),
            Self::Error => include_bytes!("../assets/icons/error.svg"),
            Self::Idle => include_bytes!("../assets/icons/idle.svg"),
            Self::Check => include_bytes!("../assets/icons/check.svg"),
            Self::Orchestrator => include_bytes!("../assets/icons/orchestrator.svg"),
            Self::Writer => include_bytes!("../assets/icons/writer.svg"),
            Self::Helper => include_bytes!("../assets/icons/helper.svg"),
            Self::Terminal => include_bytes!("../assets/icons/terminal.svg"),
            Self::Git => include_bytes!("../assets/icons/git.svg"),
            Self::Plugin => include_bytes!("../assets/icons/plugin.svg"),
            Self::Home => include_bytes!("../assets/icons/home.svg"),
            Self::Inbox => include_bytes!("../assets/icons/inbox.svg"),
            Self::Search => include_bytes!("../assets/icons/search.svg"),
            Self::Sidebar => include_bytes!("../assets/icons/sidebar.svg"),
            Self::Inspector => include_bytes!("../assets/icons/inspector.svg"),
            Self::Settings => include_bytes!("../assets/icons/settings.svg"),
            Self::Plus => include_bytes!("../assets/icons/plus.svg"),
            Self::Close => include_bytes!("../assets/icons/close.svg"),
            Self::ChevronRight => include_bytes!("../assets/icons/chevron-right.svg"),
            Self::ChevronDown => include_bytes!("../assets/icons/chevron-down.svg"),
            Self::SplitH => include_bytes!("../assets/icons/split-h.svg"),
            Self::SplitV => include_bytes!("../assets/icons/split-v.svg"),
            Self::Maximize => include_bytes!("../assets/icons/maximize.svg"),
            Self::Ship => include_bytes!("../assets/icons/ship.svg"),
            Self::Trash => include_bytes!("../assets/icons/trash.svg"),
            Self::Fork => include_bytes!("../assets/icons/fork.svg"),
            Self::Play => include_bytes!("../assets/icons/play.svg"),
            Self::Pause => include_bytes!("../assets/icons/pause.svg"),
            Self::Refresh => include_bytes!("../assets/icons/refresh.svg"),
            Self::ExternalLink => include_bytes!("../assets/icons/external-link.svg"),
            Self::More => include_bytes!("../assets/icons/more.svg"),
            Self::Lock => include_bytes!("../assets/icons/lock.svg"),
            Self::Globe => include_bytes!("../assets/icons/globe.svg"),
            Self::Database => include_bytes!("../assets/icons/database.svg"),
            Self::Folder => include_bytes!("../assets/icons/folder.svg"),
            Self::FileText => include_bytes!("../assets/icons/file-text.svg"),
            Self::Send => include_bytes!("../assets/icons/send.svg"),
            Self::Ticket => include_bytes!("../assets/icons/ticket.svg"),
            Self::PenTool => include_bytes!("../assets/icons/pen-tool.svg"),
            Self::Grip => include_bytes!("../assets/icons/grip.svg"),
            Self::Minimize => include_bytes!("../assets/icons/minimize.svg"),
            Self::Layers => include_bytes!("../assets/icons/layers.svg"),
            Self::Command => include_bytes!("../assets/icons/command.svg"),
        }
    }

    /// Look up an icon by its [`IconName::path`].
    pub fn from_path(path: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|icon| icon.path() == path)
    }
}

/// Standard icon sizes aligned to the 4px shell density grid.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum IconSize {
    /// 12px: status line, compact badges, inline indicators.
    Xs,
    /// 14px: pane headers (24px), roster rows (28px), compact buttons.
    #[default]
    Sm,
    /// 16px: regular buttons, inspector tabs, toast icons.
    Md,
    /// 20px: empty states and section headers.
    Lg,
    /// Explicit size in logical pixels.
    Custom(Pixels),
}

impl IconSize {
    /// The icon's square side length in pixels.
    pub fn pixels(self) -> Pixels {
        match self {
            Self::Xs => px(12.0),
            Self::Sm => px(14.0),
            Self::Md => px(16.0),
            Self::Lg => px(20.0),
            Self::Custom(size) => size,
        }
    }
}

/// Zero-I/O [`AssetSource`] serving all embedded `xctx-ui` SVG icons.
#[derive(Debug, Clone, Copy, Default)]
pub struct UiAssets;

impl AssetSource for UiAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(IconName::from_path(path).map(|icon| Cow::Borrowed(icon.bytes())))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let prefix = path.trim_end_matches('/');
        let items = IconName::ALL
            .iter()
            .map(|icon| icon.path())
            .filter(|asset_path| {
                prefix.is_empty()
                    || *asset_path == prefix
                    || asset_path
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .map(SharedString::new_static)
            .collect();
        Ok(items)
    }
}

/// A monochrome SVG icon drawn from [`UiAssets`] and tinted via `text_color`.
#[derive(IntoElement)]
pub struct Icon {
    name: IconName,
    size: IconSize,
    color: Option<Hsla>,
    tone: Option<Tone>,
    rotation: Option<Radians>,
}

impl Icon {
    /// Create a new icon at [`IconSize::Sm`] (14px) using the theme's muted text colour by default.
    pub fn new(name: IconName) -> Self {
        Self {
            name,
            size: IconSize::default(),
            color: None,
            tone: None,
            rotation: None,
        }
    }

    /// Set the icon size from a preset or [`IconSize::Custom`].
    pub fn size(mut self, size: IconSize) -> Self {
        self.size = size;
        self
    }

    /// Set the icon size directly in logical pixels.
    pub fn size_px(mut self, size: Pixels) -> Self {
        self.size = IconSize::Custom(size);
        self
    }

    /// Tint the icon with an explicit colour, overriding any [`Tone`].
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }

    /// Tint the icon with the foreground colour of `tone` from the active [`Theme`].
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }

    /// Rotate the rendered SVG around its centre without affecting layout.
    pub fn rotation(mut self, angle: Radians) -> Self {
        self.rotation = Some(angle);
        self
    }

    /// Resolve the effective tint colour against `theme`.
    pub fn resolved_color(&self, theme: &Theme) -> Hsla {
        if let Some(color) = self.color {
            color
        } else if let Some(tone) = self.tone {
            theme.tone(tone).fg
        } else {
            theme.text_muted
        }
    }
}

impl RenderOnce for Icon {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let color = self.resolved_color(&theme);
        let mut element = svg()
            .path(self.name.path())
            .size(self.size.pixels())
            .flex_none()
            .text_color(color);
        if let Some(angle) = self.rotation {
            element = element.with_transformation(Transformation::rotate(angle));
        }
        element
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use gpui::SvgRenderer;

    use super::*;

    #[test]
    fn all_icons_have_unique_paths_and_labels() {
        let mut paths = HashSet::new();
        let mut labels = HashSet::new();
        for &icon in IconName::ALL {
            assert!(paths.insert(icon.path()), "duplicate path {}", icon.path());
            assert!(
                labels.insert(icon.label()),
                "duplicate label {}",
                icon.label()
            );
            assert_eq!(IconName::from_path(icon.path()), Some(icon));
        }
        assert!(IconName::ALL.len() >= 30);
    }

    #[test]
    fn ui_assets_loads_and_lists_every_embedded_svg() {
        let assets = UiAssets;
        for &icon in IconName::ALL {
            let loaded = assets
                .load(icon.path())
                .expect("UiAssets::load should not fail")
                .expect("every IconName path must resolve");
            let text = std::str::from_utf8(&loaded).expect("SVG must be valid UTF-8");
            assert!(text.starts_with("<svg "), "invalid svg: {}", icon.path());
            assert!(
                text.contains(r#"stroke="currentColor""#),
                "missing currentColor stroke: {}",
                icon.path()
            );
        }
        assert!(assets.load("icons/does-not-exist.svg").unwrap().is_none());

        let listed = assets.list("icons").unwrap();
        assert_eq!(listed.len(), IconName::ALL.len());
        let all_listed = assets.list("").unwrap();
        assert_eq!(all_listed.len(), IconName::ALL.len());
        assert!(assets.list("other").unwrap().is_empty());
    }

    #[test]
    fn every_embedded_svg_parses_and_rasterizes_with_gpui() {
        let renderer = SvgRenderer::new(Arc::new(UiAssets));
        for &icon in IconName::ALL {
            let parsed = renderer
                .parse_svg(icon.bytes())
                .unwrap_or_else(|e| panic!("failed to parse {}: {e}", icon.path()));
            let rendered = renderer
                .render_parsed(&parsed, 1.0)
                .unwrap_or_else(|e| panic!("failed to rasterize {}: {e}", icon.path()));
            let size = rendered.size(0);
            assert!(
                size.width.0 > 0 && size.height.0 > 0,
                "empty render for {}",
                icon.path()
            );
        }
    }

    #[test]
    fn icon_color_precedence_and_sizes() {
        let theme = Theme::dark();
        assert_eq!(IconSize::Xs.pixels(), px(12.0));
        assert_eq!(IconSize::Sm.pixels(), px(14.0));
        assert_eq!(IconSize::Md.pixels(), px(16.0));
        assert_eq!(IconSize::Lg.pixels(), px(20.0));
        assert_eq!(IconSize::Custom(px(18.0)).pixels(), px(18.0));

        assert_eq!(
            Icon::new(IconName::Check).resolved_color(&theme),
            theme.text_muted
        );
        assert_eq!(
            Icon::new(IconName::Check)
                .tone(Tone::Success)
                .resolved_color(&theme),
            theme.success
        );
        assert_eq!(
            Icon::new(IconName::Check)
                .tone(Tone::Success)
                .color(theme.danger)
                .resolved_color(&theme),
            theme.danger
        );
    }
}
