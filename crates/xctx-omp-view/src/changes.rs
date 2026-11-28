//! In-session git changes and unified diff preview (Tern's `omp.app.git.*` drawer).
//!
//! The view stays decoupled from the filesystem and git: the host scans the pane's worktree off the UI thread and
//! pushes a [`SessionChanges`] snapshot into [`crate::OmpView::set_changes`]. Every file path, file diff, and the
//! combined patch can be copied to the clipboard in one click.

use std::rc::Rc;

use gpui::{AnyElement, App, FontWeight, SharedString, div, prelude::*, px};
use xctx_ui::{Badge, Button, ButtonSize, ButtonVariant, Icon, IconName, IconSize, Theme, Tone};

use crate::markdown::diff_line_color;

/// A callback the host registers so the Changes panel's Refresh button can re-scan git.
pub type ChangesRefresher = Rc<dyn Fn(&mut App)>;

/// Callback invoked when the user clicks a copy control: `(copy_key, clipboard_text, cx)`.
pub type CopyCallback = Rc<dyn Fn(String, String, &mut App)>;

pub(crate) type SelectIndex = Rc<dyn Fn(usize, &mut App)>;
pub(crate) type VoidCallback = Rc<dyn Fn(&mut App)>;

/// How a file changed in the pane's worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    /// Modified tracked file (`M`).
    Modified,
    /// Newly added file in the index (`A`).
    Added,
    /// Deleted file (`D`).
    Deleted,
    /// Renamed file (`R`).
    Renamed,
    /// Untracked file on disk (`?`).
    Untracked,
    /// Unmerged / conflicted file (`U`).
    Conflicted,
}

impl ChangeKind {
    /// One-character git status marker.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Untracked => "?",
            Self::Conflicted => "U",
        }
    }

    /// Human-readable label for tooltips and headers.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Modified => "modified",
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::Untracked => "untracked",
            Self::Conflicted => "conflicted",
        }
    }

    /// Colour tone for the status marker.
    pub const fn tone(self) -> Tone {
        match self {
            Self::Added | Self::Untracked => Tone::Success,
            Self::Modified | Self::Renamed => Tone::Warning,
            Self::Deleted | Self::Conflicted => Tone::Danger,
        }
    }
}

/// One changed file in the agent's worktree, along with its diffstat and unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// Path relative to the worktree root.
    pub path: SharedString,
    /// Kind of change reported by git.
    pub kind: ChangeKind,
    /// Added line count (`+` lines in the unified diff).
    pub additions: usize,
    /// Removed line count (`-` lines in the unified diff).
    pub deletions: usize,
    /// Unified diff text (redacted and size-capped by the host).
    pub diff: SharedString,
}

/// A snapshot of uncommitted git changes in the agent's worktree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionChanges {
    /// Current branch name, if known.
    pub branch: Option<SharedString>,
    /// Changed files in the worktree.
    pub files: Vec<ChangedFile>,
    /// Error message when git could not inspect the worktree.
    pub error: Option<SharedString>,
}

/// Count added and removed lines in a unified diff (ignoring `+++` / `---` file headers).
pub fn diff_counts(diff: &str) -> (usize, usize) {
    let (mut added, mut removed) = (0usize, 0usize);
    for line in diff.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            added += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            removed += 1;
        }
    }
    (added, removed)
}

/// Total `(additions, deletions)` across all changed files.
pub fn total_diffstat(files: &[ChangedFile]) -> (usize, usize) {
    files
        .iter()
        .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions))
}

/// Combine all non-empty file diffs into a single unified patch suitable for the clipboard.
pub fn combined_diff(files: &[ChangedFile]) -> String {
    let mut out = String::new();
    for file in files {
        let diff = file.diff.trim_end();
        if diff.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        if !diff.starts_with("diff --git") && !diff.starts_with("---") {
            out.push_str(&format!("# {} ({})\n", file.path, file.kind.label()));
        }
        out.push_str(diff);
        out.push('\n');
    }
    out
}

/// Inputs for rendering the in-session Changes panel.
pub(crate) struct ChangesPanelProps<'a> {
    pub changes: &'a SessionChanges,
    pub selected: usize,
    pub copied: Option<&'a str>,
    pub has_refresher: bool,
    pub on_select: SelectIndex,
    pub on_copy: CopyCallback,
    pub on_refresh: VoidCallback,
    pub on_close: VoidCallback,
}

/// Render the collapsible in-session Changes & Diff panel.
pub(crate) fn render_changes_panel(props: ChangesPanelProps<'_>, theme: &Theme) -> AnyElement {
    let files = &props.changes.files;
    let (total_add, total_del) = total_diffstat(files);
    let selected = props.selected.min(files.len().saturating_sub(1));
    let active_file = files.get(selected);
    let all_patch = combined_diff(files);
    let has_patch = !all_patch.is_empty();
    let copied_all = props.copied == Some("changes-all");

    let on_copy_all = props.on_copy.clone();
    let on_refresh = props.on_refresh.clone();
    let on_close = props.on_close.clone();

    let header = div()
        .flex()
        .items_center()
        .gap(Theme::SPACE_2)
        .w_full()
        .h(px(30.0))
        .px(Theme::SPACE_3)
        .border_b_1()
        .border_color(theme.border_subtle)
        .bg(theme.raised)
        .child(
            Icon::new(IconName::Git)
                .size(IconSize::Xs)
                .color(theme.accent),
        )
        .child(
            div()
                .text_size(Theme::TEXT_SM)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(format!("Changes ({})", files.len())),
        )
        .children(
            props
                .changes
                .branch
                .as_ref()
                .map(|branch| Badge::new(branch.clone()).tone(Tone::Neutral)),
        )
        .when(total_add > 0 || total_del > 0, |row| {
            row.child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .font_family("Menlo")
                    .text_size(Theme::TEXT_XS)
                    .child(
                        div()
                            .text_color(theme.success)
                            .child(format!("+{total_add}")),
                    )
                    .child(
                        div()
                            .text_color(theme.danger)
                            .child(format!("−{total_del}")),
                    ),
            )
        })
        .child(div().flex_1())
        .child(
            Button::new(
                "omp-changes-copy-all",
                if copied_all {
                    "Copied"
                } else {
                    "Copy all diffs"
                },
            )
            .icon(if copied_all {
                IconName::Check
            } else {
                IconName::Copy
            })
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Compact)
            .disabled(!has_patch)
            .tooltip("Copy combined unified diff for all changed files")
            .selector("omp-changes-copy-all")
            .on_click(move |_, _, cx| {
                on_copy_all("changes-all".to_owned(), all_patch.clone(), cx);
            }),
        )
        .when(props.has_refresher, |row| {
            row.child(
                Button::new("omp-changes-refresh", "")
                    .icon(IconName::Refresh)
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .tooltip("Refresh git changes")
                    .selector("omp-changes-refresh")
                    .on_click(move |_, _, cx| on_refresh(cx)),
            )
        })
        .child(
            Button::new("omp-changes-close", "")
                .icon(IconName::Close)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .tooltip("Close changes panel")
                .selector("omp-changes-close")
                .on_click(move |_, _, cx| on_close(cx)),
        );

    let body: AnyElement = if let Some(err) = &props.changes.error {
        div()
            .p(Theme::SPACE_3)
            .text_size(Theme::TEXT_SM)
            .text_color(theme.danger)
            .child(err.clone())
            .into_any_element()
    } else if files.is_empty() {
        div()
            .debug_selector(|| "omp-changes-empty".to_owned())
            .p(Theme::SPACE_3)
            .text_size(Theme::TEXT_SM)
            .text_color(theme.text_muted)
            .child("No uncommitted changes in this worktree.")
            .into_any_element()
    } else {
        let hover = theme.hover;
        let selected_bg = theme.selection;
        let file_list = div()
            .id("omp-changes-files")
            .debug_selector(|| "omp-changes-files".to_owned())
            .flex()
            .flex_col()
            .w(px(230.0))
            .flex_none()
            .max_h(px(260.0))
            .overflow_y_scroll()
            .border_r_1()
            .border_color(theme.border_subtle)
            .py(Theme::SPACE_1)
            .children(files.iter().enumerate().map(|(i, file)| {
                let is_sel = i == selected;
                let tone = theme.tone(file.kind.tone());
                let on_select = props.on_select.clone();
                let on_copy = props.on_copy.clone();
                let file_diff = file.diff.to_string();
                let copy_key = format!("changes-file-{i}");
                let is_copied = props.copied == Some(copy_key.as_str());
                let has_diff = !file_diff.trim().is_empty();
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .px(Theme::SPACE_1)
                    .when(is_sel, |row| row.bg(selected_bg))
                    .child(
                        div()
                            .id(("omp-change-file", i))
                            .debug_selector(move || format!("omp-change-file-{i}"))
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap(Theme::SPACE_1)
                            .px(Theme::SPACE_1)
                            .py(px(3.0))
                            .rounded(Theme::RADIUS_SM)
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .on_click(move |_, _, cx| on_select(i, cx))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(14.0))
                                    .font_family("Menlo")
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_size(Theme::TEXT_XS)
                                    .text_color(tone.fg)
                                    .child(file.kind.code()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_family("Menlo")
                                    .text_size(Theme::TEXT_XS)
                                    .text_color(theme.text)
                                    .child(file.path.clone()),
                            )
                            .when(file.additions > 0 || file.deletions > 0, |row| {
                                row.child(
                                    div()
                                        .flex_none()
                                        .font_family("Menlo")
                                        .text_size(Theme::TEXT_XS)
                                        .text_color(theme.text_subtle)
                                        .child(format!("+{} −{}", file.additions, file.deletions)),
                                )
                            }),
                    )
                    .child(
                        Button::new(("omp-changes-copy-file", i), "")
                            .icon(if is_copied {
                                IconName::Check
                            } else {
                                IconName::Copy
                            })
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .disabled(!has_diff)
                            .tooltip("Copy file diff")
                            .selector(format!("omp-changes-copy-file-{i}"))
                            .on_click(move |_, _, cx| {
                                on_copy(copy_key.clone(), file_diff.clone(), cx);
                            }),
                    )
            }));

        let diff_pane: AnyElement = if let Some(file) = active_file {
            let on_copy_path = props.on_copy.clone();
            let on_copy_diff = props.on_copy.clone();
            let path_text = file.path.to_string();
            let diff_text = file.diff.to_string();
            let has_diff = !diff_text.trim().is_empty();
            let copied_path = props.copied == Some("changes-path");
            let copied_diff = props.copied == Some("changes-diff");

            let diff_header = div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .w_full()
                .px(Theme::SPACE_2)
                .py(Theme::SPACE_1)
                .border_b_1()
                .border_color(theme.border_subtle)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family("Menlo")
                        .text_size(Theme::TEXT_XS)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(file.path.clone()),
                )
                .child(
                    Button::new(
                        "omp-changes-copy-path",
                        if copied_path { "Copied" } else { "Copy path" },
                    )
                    .icon(if copied_path {
                        IconName::Check
                    } else {
                        IconName::Copy
                    })
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .tooltip("Copy relative file path")
                    .selector("omp-changes-copy-path")
                    .on_click(move |_, _, cx| {
                        on_copy_path("changes-path".to_owned(), path_text.clone(), cx);
                    }),
                )
                .child(
                    Button::new(
                        "omp-changes-copy-diff",
                        if copied_diff { "Copied" } else { "Copy diff" },
                    )
                    .icon(if copied_diff {
                        IconName::Check
                    } else {
                        IconName::Copy
                    })
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .disabled(!has_diff)
                    .tooltip("Copy unified diff for this file")
                    .selector("omp-changes-copy-diff")
                    .on_click(move |_, _, cx| {
                        on_copy_diff("changes-diff".to_owned(), diff_text.clone(), cx);
                    }),
                );

            let diff_lines = div()
                .id("omp-changes-diff-scroll")
                .debug_selector(|| "omp-changes-diff".to_owned())
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .max_h(px(228.0))
                .overflow_y_scroll()
                .p(Theme::SPACE_2)
                .font_family("Menlo")
                .text_size(Theme::TEXT_XS)
                .children(if has_diff {
                    file.diff
                        .lines()
                        .map(|line| {
                            div()
                                .text_color(diff_line_color(line, theme))
                                .child(line.to_owned())
                                .into_any_element()
                        })
                        .collect::<Vec<_>>()
                } else {
                    vec![
                        div()
                            .text_color(theme.text_subtle)
                            .child("No diff preview available.")
                            .into_any_element(),
                    ]
                });

            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(diff_header)
                .child(diff_lines)
                .into_any_element()
        } else {
            div().flex_1().into_any_element()
        };

        div()
            .flex()
            .w_full()
            .child(file_list)
            .child(diff_pane)
            .into_any_element()
    };

    div()
        .debug_selector(|| "omp-changes-panel".to_owned())
        .flex()
        .flex_col()
        .flex_none()
        .w_full()
        .border_b_1()
        .border_color(theme.border_subtle)
        .bg(theme.surface)
        .child(header)
        .child(body)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_counts_ignores_file_headers_and_counts_edits() {
        let diff = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n fn a() {}\n-let old = 1;\n+let new = 2;\n+let extra = 3;\n";
        assert_eq!(diff_counts(diff), (2, 1));
    }

    #[test]
    fn combined_diff_concatenates_non_empty_patches() {
        let files = vec![
            ChangedFile {
                path: "a.rs".into(),
                kind: ChangeKind::Modified,
                additions: 1,
                deletions: 1,
                diff: "diff --git a/a.rs b/a.rs\n-old\n+new\n".into(),
            },
            ChangedFile {
                path: "empty.rs".into(),
                kind: ChangeKind::Added,
                additions: 0,
                deletions: 0,
                diff: "".into(),
            },
            ChangedFile {
                path: "b.rs".into(),
                kind: ChangeKind::Untracked,
                additions: 1,
                deletions: 0,
                diff: "+hello\n".into(),
            },
        ];
        assert_eq!(total_diffstat(&files), (2, 1));
        let patch = combined_diff(&files);
        assert!(patch.contains("diff --git a/a.rs b/a.rs"));
        assert!(patch.contains("# b.rs (untracked)"));
        assert!(patch.contains("+hello"));
        assert!(!patch.contains("empty.rs"));
    }
}
