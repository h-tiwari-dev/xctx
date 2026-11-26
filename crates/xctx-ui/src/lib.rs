//! The window chrome of xctx, drawn with GPUI: the theme, embedded SVG icons, motion helpers,
//! isolated spinner, modal/anchored overlays, non-blocking toasts, debug frame-time overlay, the
//! title bar row, the Roster sidebar, the command palette, drag-and-drop payloads, the split view
//! over an `xctx-layout` pane tree, the prompt input, the status bar, the Inbox panel, the Plan
//! Gate card, the Parent Agent drawer, the Flow view of a space (stage track, agent canvas, agent
//! detail, timeline), the fork dialog, the attempt strip and the review sheet for competing
//! attempts, the tool dock, and the small buttons, pills and badges they share.
//! Components read their colours from the [`Theme`] global when one is set (`cx.set_global(Theme::dark())`),
//! and fall back to [`Theme::dark`] otherwise. They hold no app state: the app passes in what to show and
//! receives what the user did through callbacks and [`PromptEvent`]s.

pub mod agents;
pub mod attempt_strip;
pub mod command_palette;
pub mod components;
pub mod drag;
pub mod flow_timeline;
pub mod flow_view;
pub mod fork_dialog;
pub mod frame_overlay;
pub mod icon;
pub mod inbox;
pub mod motion;
pub mod overlay;
pub mod parent_agent;
pub mod plan_gate;
pub mod prompt;
pub mod repo_picker;
pub mod review_sheet;
pub mod roster;
pub mod spinner;
pub mod split_view;
pub mod stage_track;
pub mod status_bar;
pub mod theme;
pub mod title_bar;
pub mod toast;
pub mod tool_dock;
pub mod window_chrome;

pub use agents::{
    HELPER_KINDS, PaneLive, agent_identity_color, agent_state_label, agent_state_tone, can_pause,
    can_resume, card_badge, helper_kind_label, helper_kind_name,
};
pub use attempt_strip::{
    AttemptChip, AttemptChipState, AttemptStrip, CHIP_LABEL_MAX, truncate_label,
};
pub use command_palette::{CommandPalette, PALETTE_CONTEXT, PaletteEvent, bind_palette_keys};
pub use components::{Badge, Button, ButtonColors, ButtonSize, ButtonVariant, Pill, Tooltip};
pub use drag::{DragPreview, DraggedPane, DraggedTool, DropEdge};
pub use flow_timeline::{
    FlowTimeline, TIMELINE_EMPTY, TIMELINE_LIMIT, message_kind_label, message_kind_tone,
    relative_time, severity_label, severity_tone, sync_list_state,
};
pub use flow_view::{AgentDetail, FlowCanvas, FlowView};
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
pub use motion::{
    MAX_DURATION, MAX_DURATION_MS, MotionFrame, MotionSettings, OVERLAY_DURATION,
    OVERLAY_DURATION_MS, OVERLAY_OFFSET_PX, SPINNER_PERIOD, SPINNER_PERIOD_MS, clamp_duration,
    ease_out_cubic, effective_duration, lerp, offset_px, opacity_for, overlay_motion, progress,
};
pub use overlay::{Overlay, OverlayPlacement};
pub use parent_agent::{AgentStatus, DRAWER_HEIGHT, PARENT_AGENT_ROLE, ParentAgentDrawer};
pub use plan_gate::{PLAN_GATE_HINT, PLAN_NEEDS_REPO, PlanGateCard, PlanGateView, PlanRepo};
pub use prompt::{PromptBuffer, PromptEvent, PromptInput};
pub use repo_picker::{RepoChoice, RepoPicker};
pub use review_sheet::{
    DIFF_CHAR_LIMIT, DIFF_LINE_CHARS, DIFF_LINE_LIMIT, DiffExcerpt, DiffView, FILE_ROW_LIMIT,
    FileRow, REVIEW_CHANGES_PENDING, REVIEW_EMPTY, REVIEW_NO_SUMMARY, REVIEW_WORKTREE_REMOVED,
    ReproView, ReviewColumn, ReviewSheet, ReviewView, cut_line, diff_excerpt, diff_line_tone,
    diffstat_label, files_note, shows_adopt, shows_discard, tests_line, tests_tone,
};
pub use roster::{
    ROSTER_AGENT_LIMIT, ROSTER_RAIL_WIDTH, ROSTER_WIDTH, Roster, RosterAgentItem, RosterSection,
    RosterSpaceItem, RowState,
};
pub use spinner::{Spinner, spinner_angle};
pub use split_view::{PaneView, SplitView, pane_kind_label, ratio_for_drag};
pub use stage_track::{
    DEFAULT_MAX_ITERATIONS, DEFAULT_MAX_TURNS, StageTrack, autonomy_label, loop_counter,
    loop_stage_label,
};
pub use status_bar::{
    BannerKind, KeyHint, StatusBanner, StatusBar, default_key_hints, format_key_hints,
    is_production_env, keycap,
};
pub use theme::{Theme, Tone, ToneColors};
pub use title_bar::{SHAPES, SpaceHeaderInfo, TitleBar, shape_label};
pub use toast::{
    DEFAULT_MAX_QUEUED_TOASTS, DEFAULT_MAX_VISIBLE_TOASTS, DEFAULT_TOAST_TTL, Toast, ToastAction,
    ToastEntry, ToastId, ToastQueue, ToastStack, toast_icon,
};
pub use tool_dock::{
    DockItem, TOOL_DOCK_WIDTH, TOOL_ITEM_LIMIT, TOOL_LOCKED_HINT, TOOL_PROBLEM_LIMIT, ToolDock,
    ToolDockView, ToolGroup, ToolProblem, ToolSetting,
};
pub use window_chrome::window_drag_area;
