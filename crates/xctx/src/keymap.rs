//! The app's actions and the keys bound to them, and the key hints the status bar shows.

use gpui::{App, KeyBinding, actions};
use xctx_ui::KeyHint;

/// The key context of the main window's root element; every binding below except the dialog ones lives in it.
pub const WORKSPACE_CONTEXT: &str = "Workspace";

/// The key context of an open confirmation dialog.
pub const DIALOG_CONTEXT: &str = "Dialog";

actions!(
    xctx,
    [
        /// Quit the app, unless a launch, ship or discard is running.
        Quit,
        /// Close the focused pane; on Home, only say how to close the window or quit.
        ClosePane,
        /// Close the window, unless a launch, ship or discard is running.
        CloseWindow,
        /// Split the focused pane side by side, with a new shell.
        SplitRight,
        /// Split the focused pane top and bottom, with a new shell.
        SplitDown,
        /// Focus the pane to the left.
        FocusLeft,
        /// Focus the pane to the right.
        FocusRight,
        /// Focus the pane above.
        FocusUp,
        /// Focus the pane below.
        FocusDown,
        /// Focus the next pane in order.
        FocusNext,
        /// Focus the previous pane in order.
        FocusPrev,
        /// Shrink the focused pane sideways.
        ResizeLeft,
        /// Grow the focused pane sideways.
        ResizeRight,
        /// Shrink the focused pane vertically.
        ResizeUp,
        /// Grow the focused pane vertically.
        ResizeDown,
        /// Zoom the focused pane, or end the zoom.
        ToggleZoom,
        /// Show the Flow view of the space being shown (its loop, agent graph and timeline), or its panes again.
        ToggleFlow,
        /// Show Home.
        ShowHome,
        /// Launch the Parent Agent's draft shown on Home.
        LaunchDraft,
        /// Close the open dialog.
        DismissDialog,
        /// Open the fork dialog for the focused writer of the space being shown (or its only writer).
        ForkAttempts,
        /// Open the review sheet on the latest round of attempts of the space being shown.
        ReviewAttempts,
        /// Open the command palette, or close it.
        TogglePalette,
        /// Collapse the Roster to a rail of icons, or expand it.
        ToggleRoster,
        /// Make the window translucent (the blurred desktop shows through), or opaque.
        ToggleTranslucency,
        /// Open the Inbox over whatever is shown, or close it.
        ToggleInbox,
        /// Show the Parent Agent as a dock over the shown space, or hide it; on Home, focus it.
        ToggleDock,
    ]
);

/// Show the space in Roster slot `0` (zero-based, in Roster order); bound to Cmd+1 to Cmd+9.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = xctx, no_json)]
pub struct ActivateTab(pub usize);

/// The keys that show Roster slots 1 to 9.
const TAB_KEYS: [&str; 9] = [
    "cmd-1", "cmd-2", "cmd-3", "cmd-4", "cmd-5", "cmd-6", "cmd-7", "cmd-8", "cmd-9",
];

/// Register every key binding.
pub fn bind_keys(cx: &mut App) {
    let ws = Some(WORKSPACE_CONTEXT);
    let mut bindings = vec![
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-w", ClosePane, ws),
        KeyBinding::new("cmd-shift-w", CloseWindow, None),
        KeyBinding::new("cmd-d", SplitRight, ws),
        KeyBinding::new("cmd-shift-d", SplitDown, ws),
        KeyBinding::new("cmd-alt-left", FocusLeft, ws),
        KeyBinding::new("cmd-alt-right", FocusRight, ws),
        KeyBinding::new("cmd-alt-up", FocusUp, ws),
        KeyBinding::new("cmd-alt-down", FocusDown, ws),
        KeyBinding::new("cmd-]", FocusNext, ws),
        KeyBinding::new("cmd-[", FocusPrev, ws),
        KeyBinding::new("cmd-ctrl-left", ResizeLeft, ws),
        KeyBinding::new("cmd-ctrl-right", ResizeRight, ws),
        KeyBinding::new("cmd-ctrl-up", ResizeUp, ws),
        KeyBinding::new("cmd-ctrl-down", ResizeDown, ws),
        KeyBinding::new("cmd-shift-z", ToggleZoom, ws),
        KeyBinding::new("cmd-g", ToggleFlow, ws),
        KeyBinding::new("cmd-shift-f", ForkAttempts, ws),
        KeyBinding::new("cmd-shift-r", ReviewAttempts, ws),
        KeyBinding::new("cmd-0", ShowHome, ws),
        KeyBinding::new("cmd-enter", LaunchDraft, ws),
        KeyBinding::new("cmd-k", TogglePalette, ws),
        KeyBinding::new("cmd-b", ToggleRoster, ws),
        KeyBinding::new("cmd-shift-i", ToggleInbox, ws),
        KeyBinding::new("cmd-j", ToggleDock, ws),
        KeyBinding::new("escape", DismissDialog, Some(DIALOG_CONTEXT)),
    ];
    for (index, key) in TAB_KEYS.into_iter().enumerate() {
        bindings.push(KeyBinding::new(key, ActivateTab(index), ws));
    }
    cx.bind_keys(bindings);
    xctx_ui::command_palette::bind_palette_keys(cx);
}

/// The one hint the status bar shows: every other key is found, with its chord, in the command palette.
pub fn palette_hint() -> Vec<KeyHint> {
    vec![KeyHint::new("⌘K", "Commands")]
}
