//! Tool calls as cards: one line per call while collapsed (the kind, what it touched, how it ended), the call's
//! output, diff or content when expanded.

use gpui::{AnyElement, App, ClickEvent, FontWeight, SharedString, Window, div, prelude::*, px};
use serde_json::Value;
use xctx_omp::{ToolRun, ToolState};
use xctx_ui::{Badge, Button, ButtonSize, ButtonVariant, Icon, IconName, IconSize, Theme, Tone};

use crate::changes::CopyCallback;
use crate::markdown::code_block;

/// The kinds of tool card, each with its own summary and body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolKind {
    /// `bash`: a shell command and its output.
    Bash,
    /// `read`: a file read.
    Read,
    /// `edit`: a change shown as a diff.
    Edit,
    /// `write`: a file written whole.
    Write,
    /// `find`, `grep`, `glob`, `search`, `ast_grep`, `lsp`: a search and its hits.
    Search,
    /// `eval`, `python`: code run in a kernel.
    Eval,
    /// `task`: subagents started.
    Task,
    /// `todo`, `todo_write`: the todo list changed.
    Todo,
    /// `web_search`, `fetch`, `browser`: the web.
    Web,
    /// Anything else, shown by name with its arguments.
    Other,
}

impl ToolKind {
    /// The kind of tool `name`.
    pub fn of(name: &str) -> ToolKind {
        match name {
            "bash" | "shell" | "exec" => ToolKind::Bash,
            "read" => ToolKind::Read,
            "edit" | "ast_edit" | "multiedit" => ToolKind::Edit,
            "write" => ToolKind::Write,
            "find" | "grep" | "glob" | "search" | "ast_grep" | "lsp" => ToolKind::Search,
            "eval" | "python" => ToolKind::Eval,
            "task" => ToolKind::Task,
            "todo" | "todo_write" => ToolKind::Todo,
            "web_search" | "fetch" | "web_fetch" | "browser" => ToolKind::Web,
            _ => ToolKind::Other,
        }
    }

    /// The stable name used in the card's test selector (`omp-tool-<slug>`).
    pub fn slug(self) -> &'static str {
        match self {
            ToolKind::Bash => "bash",
            ToolKind::Read => "read",
            ToolKind::Edit => "edit",
            ToolKind::Write => "write",
            ToolKind::Search => "search",
            ToolKind::Eval => "eval",
            ToolKind::Task => "task",
            ToolKind::Todo => "todo",
            ToolKind::Web => "web",
            ToolKind::Other => "other",
        }
    }

    fn icon(self) -> IconName {
        match self {
            ToolKind::Bash => IconName::Terminal,
            ToolKind::Read | ToolKind::Write => IconName::FileText,
            ToolKind::Edit => IconName::PenTool,
            ToolKind::Search => IconName::Search,
            ToolKind::Eval => IconName::Command,
            ToolKind::Task => IconName::Orchestrator,
            ToolKind::Todo => IconName::Check,
            ToolKind::Web => IconName::Globe,
            ToolKind::Other => IconName::Plugin,
        }
    }
}

/// The longest summary a collapsed card shows, in characters.
pub const SUMMARY_CHARS: usize = 120;

/// The most lines an expanded card shows; the rest are counted.
pub const BODY_LINES: usize = 400;

fn arg<'a>(run: &'a ToolRun, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| run.args.get(*k).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim()
}

fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

/// The diff an edit produced (`details.diff`), when omp reported one.
pub fn edit_diff(run: &ToolRun) -> Option<&str> {
    run.details
        .get("diff")
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty())
}

/// Lines added and removed in a unified diff, not counting the `+++`/`---` file headers.
pub fn diffstat(diff: &str) -> (usize, usize) {
    diff.lines().fold((0, 0), |(add, del), line| {
        if line.starts_with("+++") || line.starts_with("---") {
            (add, del)
        } else if line.starts_with('+') {
            (add + 1, del)
        } else if line.starts_with('-') {
            (add, del + 1)
        } else {
            (add, del)
        }
    })
}

/// What a collapsed card says about `run`: the command, path, pattern, query or tasks, at most
/// [`SUMMARY_CHARS`] characters.
pub fn summary(run: &ToolRun) -> String {
    let path = || arg(run, &["path", "file_path", "file"]);
    let text = match ToolKind::of(&run.name) {
        ToolKind::Bash => arg(run, &["command", "cmd"])
            .map(first_line)
            .unwrap_or_default()
            .to_owned(),
        ToolKind::Read => path().unwrap_or_default().to_owned(),
        ToolKind::Edit => {
            let stat = edit_diff(run).map(diffstat);
            match (path(), stat) {
                (Some(p), Some((a, d))) => format!("{p}  +{a} −{d}"),
                (None, Some((a, d))) => format!("+{a} −{d}"),
                (Some(p), None) => p.to_owned(),
                (None, None) => String::new(),
            }
        }
        ToolKind::Write => {
            let lines = arg(run, &["content"]).map(|c| c.lines().count());
            match (path(), lines) {
                (Some(p), Some(n)) => format!("{p}  {n} lines"),
                (Some(p), None) => p.to_owned(),
                (None, _) => String::new(),
            }
        }
        ToolKind::Search => {
            let what = arg(run, &["pattern", "query", "glob", "symbol"]).unwrap_or_default();
            match path() {
                Some(p) => format!("{what}  in {p}"),
                None => what.to_owned(),
            }
        }
        ToolKind::Eval => arg(run, &["code", "input", "source"])
            .map(first_line)
            .unwrap_or_default()
            .to_owned(),
        ToolKind::Task => {
            let names: Vec<&str> = run
                .args
                .get("tasks")
                .and_then(Value::as_array)
                .map(|tasks| {
                    tasks
                        .iter()
                        .filter_map(|t| t.get("name").and_then(Value::as_str))
                        .collect()
                })
                .unwrap_or_default();
            if names.is_empty() {
                arg(run, &["agent", "description"])
                    .unwrap_or_default()
                    .to_owned()
            } else {
                names.join(", ")
            }
        }
        ToolKind::Todo => {
            let count = ["todos", "ops", "tasks"]
                .iter()
                .find_map(|k| run.args.get(*k).and_then(Value::as_array))
                .map_or(0, Vec::len);
            if count == 0 {
                "update the todo list".to_owned()
            } else {
                format!("{count} changes")
            }
        }
        ToolKind::Web => arg(run, &["query", "url"]).unwrap_or_default().to_owned(),
        ToolKind::Other => match &run.args {
            Value::Object(map) if map.is_empty() => String::new(),
            Value::Null => String::new(),
            args => args.to_string(),
        },
    };
    clip(&text, SUMMARY_CHARS)
}

/// The card's title: the tool's name as omp's terminal UI labels the kind.
pub fn title(run: &ToolRun) -> SharedString {
    match ToolKind::of(&run.name) {
        ToolKind::Bash => "Bash".into(),
        ToolKind::Read => "Read".into(),
        ToolKind::Edit => "Edit".into(),
        ToolKind::Write => "Write".into(),
        ToolKind::Search => SharedString::from(capitalise(&run.name)),
        ToolKind::Eval => "Python".into(),
        ToolKind::Task => "Task".into(),
        ToolKind::Todo => "Todo".into(),
        ToolKind::Web => SharedString::from(capitalise(&run.name.replace('_', " "))),
        ToolKind::Other if run.name.is_empty() => "Tool".into(),
        ToolKind::Other => SharedString::from(run.name.clone()),
    }
}

fn capitalise(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `text` cut to [`BODY_LINES`] lines, with how many were left out.
pub fn clip_lines(text: &str) -> (&str, usize) {
    match text.match_indices('\n').nth(BODY_LINES - 1) {
        Some((cut, _)) => (&text[..cut], text[cut + 1..].lines().count()),
        None => (text, 0),
    }
}

fn state_badge(state: ToolState) -> Badge {
    match state {
        ToolState::Pending => Badge::new("queued"),
        ToolState::Running => Badge::new("running").tone(Tone::Accent),
        ToolState::Done => Badge::new("done").tone(Tone::Success),
        ToolState::Failed => Badge::new("failed").tone(Tone::Danger),
    }
}

fn body(run: &ToolRun, theme: &Theme) -> AnyElement {
    let kind = ToolKind::of(&run.name);
    let failed = run.state == ToolState::Failed;
    let mut parts: Vec<AnyElement> = Vec::new();
    match kind {
        ToolKind::Bash => {
            if let Some(command) = arg(run, &["command", "cmd"]) {
                parts.push(code_block(&format!("$ {command}"), false, theme));
            }
        }
        ToolKind::Eval => {
            if let Some(code) = arg(run, &["code", "input", "source"]) {
                parts.push(code_block(clip_lines(code).0, false, theme));
            }
        }
        ToolKind::Write if !failed => {
            if let Some(content) = arg(run, &["content"]) {
                let (shown, more) = clip_lines(content);
                parts.push(code_block(shown, false, theme));
                if more > 0 {
                    parts.push(more_lines(more, theme));
                }
                return column(parts);
            }
        }
        ToolKind::Edit if !failed => {
            if let Some(diff) = edit_diff(run) {
                let (shown, more) = clip_lines(diff);
                parts.push(code_block(shown, true, theme));
                if more > 0 {
                    parts.push(more_lines(more, theme));
                }
                return column(parts);
            }
        }
        _ => {}
    }
    if !run.output.is_empty() {
        let (shown, more) = clip_lines(&run.output);
        let block = if failed {
            div()
                .debug_selector(|| "omp-tool-error".to_owned())
                .w_full()
                .p(Theme::SPACE_2)
                .rounded(Theme::RADIUS_SM)
                .bg(theme.tone(Tone::Danger).bg)
                .font_family("Menlo")
                .text_size(Theme::TEXT_SM)
                .text_color(theme.danger)
                .child(shown.to_owned())
                .into_any_element()
        } else {
            code_block(shown, false, theme)
        };
        parts.push(block);
        if more > 0 {
            parts.push(more_lines(more, theme));
        }
    } else if kind == ToolKind::Other || parts.is_empty() {
        let args = serde_json::to_string_pretty(&run.args).unwrap_or_default();
        if !run.args.is_null() {
            parts.push(code_block(&args, false, theme));
        }
    }
    column(parts)
}

fn more_lines(count: usize, theme: &Theme) -> AnyElement {
    div()
        .text_size(Theme::TEXT_XS)
        .text_color(theme.text_subtle)
        .child(format!("… {count} more lines"))
        .into_any_element()
}

fn column(parts: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(Theme::SPACE_1)
        .w_full()
        .children(parts)
        .into_any_element()
}

/// The most useful text to copy from `run` (unified diff for `edit`, file content for `write`, command/output for
/// `bash`, output or JSON args for others). Returns `None` when nothing meaningful is available yet.
pub fn copyable_text(run: &ToolRun) -> Option<String> {
    let kind = ToolKind::of(&run.name);
    let failed = run.state == ToolState::Failed;
    match kind {
        ToolKind::Edit if !failed => {
            if let Some(diff) = edit_diff(run) {
                return Some(diff.to_owned());
            }
        }
        ToolKind::Write if !failed => {
            if let Some(content) = arg(run, &["content"]) {
                return Some(content.to_owned());
            }
        }
        ToolKind::Bash => {
            let cmd = arg(run, &["command", "cmd"]);
            return match (cmd, run.output.is_empty()) {
                (Some(c), false) => Some(format!("$ {c}\n{}", run.output)),
                (Some(c), true) => Some(c.to_owned()),
                (None, false) => Some(run.output.clone()),
                (None, true) => None,
            };
        }
        ToolKind::Eval => {
            if !run.output.is_empty() {
                return Some(run.output.clone());
            }
            if let Some(code) = arg(run, &["code", "input", "source"]) {
                return Some(code.to_owned());
            }
        }
        _ => {}
    }
    if !run.output.is_empty() {
        return Some(run.output.clone());
    }
    if !run.args.is_null() {
        let pretty = serde_json::to_string_pretty(&run.args).unwrap_or_default();
        if !pretty.is_empty() && pretty != "{}" {
            return Some(pretty);
        }
    }
    None
}

/// Short tooltip label describing what the Copy button copies for `run`.
pub fn copy_tooltip(run: &ToolRun) -> &'static str {
    match ToolKind::of(&run.name) {
        ToolKind::Edit if edit_diff(run).is_some() => "Copy diff",
        ToolKind::Write if arg(run, &["content"]).is_some() => "Copy written content",
        ToolKind::Bash if run.output.is_empty() => "Copy command",
        ToolKind::Bash => "Copy command and output",
        _ => "Copy tool output",
    }
}

type Toggle = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A card for `run`: a header row (icon, title, summary, state) that toggles `expanded` through `on_toggle`, and
/// the body when expanded. Failed calls are outlined in the danger colour. Test selectors: `omp-tool-<slug>` on
/// the header, `omp-tool-expanded` on an open body, `omp-tool-error` on a failed call's output.
pub fn card(run: &ToolRun, expanded: bool, on_toggle: Toggle, theme: &Theme) -> AnyElement {
    card_with_copy(run, expanded, on_toggle, None, None, theme)
}

/// Like [`card`], plus a one-click Copy button in the header (copies diff, written content, command, or output
/// without toggling expansion).
pub fn card_with_copy(
    run: &ToolRun,
    expanded: bool,
    on_toggle: Toggle,
    copied: Option<&str>,
    on_copy: Option<CopyCallback>,
    theme: &Theme,
) -> AnyElement {
    let kind = ToolKind::of(&run.name);
    let failed = run.state == ToolState::Failed;
    let selector = format!("omp-tool-{}", kind.slug());
    let hover = theme.hover;
    let edit_stat = (kind == ToolKind::Edit)
        .then(|| edit_diff(run).map(diffstat))
        .flatten();
    let header_summary = if edit_stat.is_some() {
        arg(run, &["path", "file_path", "file"])
            .map(|p| clip(p, SUMMARY_CHARS))
            .unwrap_or_default()
    } else {
        summary(run)
    };
    let copy_button = on_copy.and_then(|cb| {
        let payload = copyable_text(run)?;
        let copy_key = format!("tool-{}", run.id);
        let is_copied = copied == Some(copy_key.as_str());
        let tip = copy_tooltip(run);
        let sel = format!("omp-copy-tool-{}", run.id);
        Some(
            Button::new(
                SharedString::from(format!("omp-tool-copy-{}", run.id)),
                if is_copied { "Copied" } else { "" },
            )
            .icon(if is_copied {
                IconName::Check
            } else {
                IconName::Copy
            })
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Compact)
            .tooltip(tip)
            .selector(sel)
            .on_click(move |_, _, cx| cb(copy_key.clone(), payload.clone(), cx)),
        )
    });

    let toggle_area = div()
        .id(SharedString::from(format!("omp-tool-header-{}", run.id)))
        .debug_selector(move || selector)
        .flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap(Theme::SPACE_2)
        .h(px(28.0))
        .px(Theme::SPACE_2)
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .on_click(on_toggle)
        .child(
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size(IconSize::Xs)
            .color(theme.text_subtle),
        )
        .child(Icon::new(kind.icon()).size(IconSize::Sm).color(if failed {
            theme.danger
        } else {
            theme.text_muted
        }))
        .child(
            div()
                .flex_none()
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(Theme::TEXT_SM)
                .text_color(theme.text)
                .child(title(run)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family("Menlo")
                .text_size(Theme::TEXT_SM)
                .text_color(theme.text_muted)
                .child(header_summary),
        )
        .children(edit_stat.map(|(added, removed)| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(Theme::SPACE_1)
                .font_family("Menlo")
                .text_size(Theme::TEXT_XS)
                .child(div().text_color(theme.success).child(format!("+{added}")))
                .child(div().text_color(theme.danger).child(format!("−{removed}")))
        }))
        .child(state_badge(run.state));

    let header = div()
        .flex()
        .items_center()
        .w_full()
        .pr(Theme::SPACE_1)
        .child(toggle_area)
        .children(copy_button);

    div()
        .flex()
        .flex_col()
        .w_full()
        .rounded(Theme::RADIUS_MD)
        .border_1()
        .border_color(if failed {
            theme.tone(Tone::Danger).border
        } else {
            theme.border_subtle
        })
        .bg(theme.surface)
        .overflow_hidden()
        .child(header)
        .when(expanded, |c| {
            c.child(
                div()
                    .debug_selector(|| "omp-tool-expanded".to_owned())
                    .w_full()
                    .px(Theme::SPACE_2)
                    .pb(Theme::SPACE_2)
                    .child(body(run, theme)),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn run(name: &str, args: Value, details: Value) -> ToolRun {
        ToolRun {
            id: "t1".into(),
            name: name.into(),
            args,
            state: ToolState::Done,
            output: String::new(),
            details,
        }
    }

    #[test]
    fn each_kind_summarises_what_it_touched() {
        assert_eq!(
            summary(&run(
                "bash",
                json!({"command": "echo hi\necho there", "timeout": 5}),
                Value::Null
            )),
            "echo hi"
        );
        assert_eq!(
            summary(&run("read", json!({"path": "/tmp/a.txt:raw"}), Value::Null)),
            "/tmp/a.txt:raw"
        );
        assert_eq!(
            summary(&run(
                "edit",
                json!({"path": "src/x.rs"}),
                json!({"diff": "--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n+more"})
            )),
            "src/x.rs  +2 −1"
        );
        assert_eq!(
            summary(&run(
                "write",
                json!({"path": "a.md", "content": "one\ntwo\n"}),
                Value::Null
            )),
            "a.md  2 lines"
        );
        assert_eq!(
            summary(&run(
                "grep",
                json!({"pattern": "fn main", "path": "src"}),
                Value::Null
            )),
            "fn main  in src"
        );
        assert_eq!(
            summary(&run(
                "task",
                json!({"tasks": [{"name": "ReadAB"}, {"name": "Scan"}]}),
                Value::Null
            )),
            "ReadAB, Scan"
        );
        assert_eq!(
            summary(&run(
                "web_search",
                json!({"query": "gpui list"}),
                Value::Null
            )),
            "gpui list"
        );
        assert_eq!(
            summary(&run("eval", json!({"code": "x = 1\ny = 2"}), Value::Null)),
            "x = 1"
        );
        assert_eq!(summary(&run("wait", json!({}), Value::Null)), "");
        assert_eq!(
            summary(&run("mystery", json!({"a": 1}), Value::Null)),
            r#"{"a":1}"#
        );
    }

    #[test]
    fn names_map_to_kinds_and_titles() {
        assert_eq!(ToolKind::of("grep"), ToolKind::Search);
        assert_eq!(ToolKind::of("todo_write"), ToolKind::Todo);
        assert_eq!(ToolKind::of("fetch"), ToolKind::Web);
        assert_eq!(ToolKind::of("ask"), ToolKind::Other);
        assert_eq!(
            title(&run("web_search", Value::Null, Value::Null)).as_ref(),
            "Web search"
        );
        assert_eq!(title(&run("ask", Value::Null, Value::Null)).as_ref(), "ask");
    }

    #[test]
    fn long_text_is_clipped_with_a_count() {
        let long = "x".repeat(SUMMARY_CHARS + 10);
        let s = summary(&run("bash", json!({"command": long}), Value::Null));
        assert_eq!(s.chars().count(), SUMMARY_CHARS + 1);
        let body: String = (0..BODY_LINES + 5).map(|i| format!("{i}\n")).collect();
        let (shown, more) = clip_lines(&body);
        assert_eq!(shown.lines().count(), BODY_LINES);
        assert_eq!(more, 5);
        assert_eq!(clip_lines("a\nb"), ("a\nb", 0));
    }

    #[test]
    fn copyable_text_picks_diff_content_command_or_output() {
        let edit = run(
            "edit",
            json!({"path": "a.rs"}),
            json!({"diff": "-old\n+new\n"}),
        );
        assert_eq!(copyable_text(&edit).as_deref(), Some("-old\n+new\n"));

        let write = run(
            "write",
            json!({"path": "b.rs", "content": "fn main() {}\n"}),
            Value::Null,
        );
        assert_eq!(copyable_text(&write).as_deref(), Some("fn main() {}\n"));

        let mut bash = run("bash", json!({"command": "cargo test"}), Value::Null);
        assert_eq!(copyable_text(&bash).as_deref(), Some("cargo test"));
        bash.output = "ok".into();
        assert_eq!(copyable_text(&bash).as_deref(), Some("$ cargo test\nok"));
    }
}
