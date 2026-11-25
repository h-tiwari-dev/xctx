//! The window chrome of xctx, drawn with GPUI: the theme, embedded SVG icons, motion helpers,
//! isolated spinner, modal/anchored overlays, non-blocking toasts, debug frame-time overlay, the
//! tab bar, the split view over an `xctx-layout` pane tree, the prompt input, the status bar, the
//! Inbox panel, the Plan Gate card, the Parent Agent drawer, the Agent Graph panel, the Shared
//! Space panel, the loop bar, the fork dialog, the attempt strip and the review sheet for
//! competing attempts, the Plugins menu, and the small buttons, pills and badges they share.
//! Components read their colours from the [`Theme`] global when one is set (`cx.set_global(Theme::dark())`),
//! and fall back to [`Theme::dark`] otherwise. They hold no app state: the app passes in what to show and
//! receives what the user did through callbacks and [`PromptEvent`]s.

pub mod agent_graph;
pub mod attempt_strip;
pub mod components;
pub mod fork_dialog;
pub mod frame_overlay;
pub mod icon;
pub mod inbox;
pub mod loop_bar;
pub mod motion;
pub mod overlay;
pub mod parent_agent;
pub mod plan_gate;
pub mod plugin_menu;
pub mod prompt;
pub mod review_sheet;
pub mod shared_space;
pub mod spinner;
pub mod split_view;
pub mod status_bar;
pub mod tab_bar;
pub mod theme;
pub mod toast;

pub use agent_graph::{
    AgentGraphView, HELPER_KINDS, agent_state_label, agent_state_tone, can_pause, can_resume,
    helper_kind_label, helper_kind_name,
};
pub use attempt_strip::{
    AttemptChip, AttemptChipState, AttemptStrip, CHIP_LABEL_MAX, truncate_label,
};
pub use components::{Badge, Button, ButtonColors, ButtonSize, ButtonVariant, Pill};
pub use fork_dialog::{
    FORK_BUSY_HINT, FORK_FULL, FORK_HINT, FORK_NO_INSTRUCTION, ForkDialog, ForkDialogView,
    fork_label,
};
pub use frame_overlay::{
    DEFAULT_FRAME_CAPACITY, FRAME_BUDGET_MS, FRAME_WINDOW, FrameMetrics, FrameOverlay, FrameStats,
    SLOW_FRAME_LOG_MS, SlowFrameEvent,
};
pub use icon::{Icon, IconName, IconSize, UiAssets};
pub use inbox::{INBOX_EMPTY, InboxAction, InboxEntry, InboxKind, InboxPanel};
pub use loop_bar::{
    DEFAULT_MAX_ITERATIONS, DEFAULT_MAX_TURNS, LOOP_STAGES, LoopAction, LoopBar, autonomy_label,
    autonomy_name, loop_counter, loop_stage_label, loop_status_label, loop_status_tone,
};
pub use motion::{
    MAX_DURATION, MAX_DURATION_MS, MotionFrame, MotionSettings, OVERLAY_DURATION,
    OVERLAY_DURATION_MS, OVERLAY_OFFSET_PX, SPINNER_PERIOD, SPINNER_PERIOD_MS, clamp_duration,
    ease_out_cubic, effective_duration, lerp, offset_px, opacity_for, overlay_motion, progress,
};
pub use overlay::{Overlay, OverlayPlacement};
pub use parent_agent::{AgentStatus, DRAWER_HEIGHT, PARENT_AGENT_ROLE, ParentAgentDrawer};
pub use plan_gate::{PLAN_GATE_HINT, PLAN_NEEDS_REPO, PlanGateCard, PlanGateView, PlanRepo};
pub use plugin_menu::{
    PLUGIN_MENU_EMPTY, PLUGIN_PROBLEM_LIMIT, PLUGIN_ROW_LIMIT, PLUGIN_TRUST_HINT, PluginEntry,
    PluginMenu, PluginMenuView, PluginProblemRow, PluginSource,
};
pub use prompt::{PromptBuffer, PromptEvent, PromptInput};
pub use review_sheet::{
    DIFF_CHAR_LIMIT, DIFF_LINE_CHARS, DIFF_LINE_LIMIT, DiffExcerpt, DiffView, FILE_ROW_LIMIT,
    FileRow, REVIEW_CHANGES_PENDING, REVIEW_EMPTY, REVIEW_NO_SUMMARY, REVIEW_WORKTREE_REMOVED,
    ReproView, ReviewColumn, ReviewSheet, ReviewView, cut_line, diff_excerpt, diff_line_tone,
    diffstat_label, files_note, shows_adopt, shows_discard, tests_line, tests_tone,
};
pub use shared_space::{
    CHANNEL_EMPTY, CONTRACT_EMPTY, SHARED_TABS, SharedSpacePanel, SharedTab, finding_location,
    message_kind_label, message_kind_tone, severity_label, severity_tone,
};
pub use spinner::{Spinner, spinner_angle};
pub use split_view::{PaneView, SplitView, pane_kind_label, ratio_for_drag};
pub use status_bar::{
    BannerKind, KeyHint, StatusBanner, StatusBar, default_key_hints, format_key_hints,
};
pub use tab_bar::{SHAPES, TabBar, TabItem, shape_label};
pub use theme::{Theme, Tone, ToneColors};
pub use toast::{
    DEFAULT_MAX_QUEUED_TOASTS, DEFAULT_MAX_VISIBLE_TOASTS, DEFAULT_TOAST_TTL, Toast, ToastAction,
    ToastEntry, ToastId, ToastQueue, ToastStack, toast_icon,
};
