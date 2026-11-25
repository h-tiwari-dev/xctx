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
        /// Open or close the agent graph panel of the space being shown.
        ToggleGraph,
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
        /// Open the Plugins menu of the space being shown.
        OpenPlugins,
    ]
);

/// Show the space in tab `0` (zero-based); bound to Cmd+1 to Cmd+9.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = xctx, no_json)]
pub struct ActivateTab(pub usize);

/// The keys that show tabs 1 to 9.
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
        KeyBinding::new("cmd-g", ToggleGraph, ws),
        KeyBinding::new("cmd-shift-f", ForkAttempts, ws),
        KeyBinding::new("cmd-shift-r", ReviewAttempts, ws),
        KeyBinding::new("cmd-shift-p", OpenPlugins, ws),
        KeyBinding::new("cmd-0", ShowHome, ws),
        KeyBinding::new("cmd-enter", LaunchDraft, ws),
        KeyBinding::new("escape", DismissDialog, Some(DIALOG_CONTEXT)),
    ];
    for (index, key) in TAB_KEYS.into_iter().enumerate() {
        bindings.push(KeyBinding::new(key, ActivateTab(index), ws));
    }
    cx.bind_keys(bindings);
}

/// The hints shown in a space. `fork` is whether ⌘⇧F can fork the writer it would pick (its runner can fork
/// sessions); without it the fork hint is left out.
pub fn space_key_hints(fork: bool) -> Vec<KeyHint> {
    let mut hints = vec![
        KeyHint::new("⌘D/⌘⇧D", "split"),
        KeyHint::new("⌘W", "close"),
        KeyHint::new("⌘⌥←→↑↓", "focus"),
        KeyHint::new("⌘⌃←→↑↓", "resize"),
        KeyHint::new("⌘⇧Z", "zoom"),
        KeyHint::new("⌘G", "graph"),
        KeyHint::new("⌘⇧F", "fork"),
        KeyHint::new("⌘⇧R", "review"),
        KeyHint::new("⌘⇧P", "plugins"),
        KeyHint::new("⌘0", "home"),
    ];
    if !fork {
        hints.retain(|hint| hint.keys.as_ref() != "⌘⇧F");
    }
    hints
}

/// The hints shown on Home.
pub fn home_key_hints() -> Vec<KeyHint> {
    vec![
        KeyHint::new("⌘⏎", "launch"),
        KeyHint::new("⌘1-9", "spaces"),
        KeyHint::new("⌘Q", "quit"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fork_hint_shows_only_when_forking_is_possible() {
        let keys = |fork| -> Vec<String> {
            space_key_hints(fork)
                .iter()
                .map(|h| h.keys.to_string())
                .collect()
        };
        assert!(keys(true).contains(&"⌘⇧F".to_owned()));
        assert!(!keys(false).contains(&"⌘⇧F".to_owned()));
        assert!(keys(false).contains(&"⌘⇧P".to_owned()));
    }
}
