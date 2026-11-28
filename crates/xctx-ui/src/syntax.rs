//! Syntax highlighting backed by `syntect` + `two-face`'s ~200-language grammar pack, styled with
//! `gpui-component`'s [`HighlightTheme`] so code blocks in `xctx-omp-view` and source previews in
//! `xctx-plugin-view` share the same dark/light syntax palette as `gpui-component`'s `TextView`,
//! `Input`, and `Diff`.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::LazyLock;

use gpui::HighlightStyle;
use gpui_component::highlighter::HighlightTheme;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::kit::is_dark_theme;
use crate::theme::Theme;

/// Deserialized once on first highlight and shared across every pane.
static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);

/// Maximum code payload (bytes) highlighted synchronously in one call; larger payloads degrade
/// cleanly to plain text so frame times stay well under the 8ms budget.
const MAX_HIGHLIGHT_BYTES: usize = 128 * 1024;

/// Map a file path (extension or special filename) to a language token recognized by
/// [`highlight_code`] and [`highlight_lines`].
pub fn language_for_path(path: &str) -> Option<&'static str> {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    match file_name {
        "Cargo.toml" | "mise.toml" => return Some("toml"),
        "Cargo.lock" => return Some("toml"),
        "Makefile" | "GNUmakefile" | "makefile" => return Some("make"),
        "Dockerfile" => return Some("dockerfile"),
        "Justfile" | "justfile" => return Some("make"),
        ".bashrc" | ".zshrc" | ".profile" | ".zprofile" => return Some("sh"),
        _ => {}
    }
    let ext = file_name.rsplit_once('.')?.1;
    match ext.to_ascii_lowercase().as_str() {
        "rs" => Some("rust"),
        "ts" | "mts" | "cts" => Some("ts"),
        "tsx" => Some("tsx"),
        "js" | "mjs" | "cjs" => Some("js"),
        "jsx" => Some("jsx"),
        "py" | "pyi" => Some("py"),
        "go" => Some("go"),
        "c" | "h" => Some("c"),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Some("cpp"),
        "cs" => Some("cs"),
        "java" => Some("java"),
        "kt" | "kts" => Some("kotlin"),
        "swift" => Some("swift"),
        "rb" => Some("ruby"),
        "php" => Some("php"),
        "lua" => Some("lua"),
        "zig" => Some("zig"),
        "ex" | "exs" => Some("elixir"),
        "scala" => Some("scala"),
        "sh" | "bash" | "zsh" | "fish" => Some("sh"),
        "sql" => Some("sql"),
        "json" | "jsonc" => Some("json"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        "xml" | "svg" | "plist" => Some("xml"),
        "html" | "htm" => Some("html"),
        "css" => Some("css"),
        "scss" => Some("scss"),
        "md" | "markdown" | "mdx" => Some("md"),
        "diff" | "patch" => Some("diff"),
        "proto" => Some("proto"),
        "graphql" | "gql" => Some("graphql"),
        "dockerfile" => Some("dockerfile"),
        "nix" => Some("nix"),
        "tf" | "tfvars" => Some("hcl"),
        "ini" | "cfg" | "conf" => Some("ini"),
        _ => None,
    }
}

/// Highlight `code` in `language` using `gpui-component`'s dark or light [`HighlightTheme`]
/// matching `theme`. Returns strictly ordered, non-overlapping UTF-8 byte ranges suitable for
/// GPUI's `StyledText::with_highlights`.
pub fn highlight_code(
    language: &str,
    code: &str,
    theme: &Theme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    if code.is_empty() || code.len() > MAX_HIGHLIGHT_BYTES {
        return Vec::new();
    }
    let Some(syntax) = find_syntax(language) else {
        return Vec::new();
    };
    let hl_theme = highlight_theme_for(theme);
    let syntaxes = &*SYNTAXES;
    let mut parser = ParseState::new(syntax);
    let mut scopes = ScopeStack::new();
    let mut scope_cache = HashMap::<Scope, Option<&'static str>>::new();
    let mut out: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let mut offset = 0usize;

    for line in LinesWithEndings::from(code) {
        if let Ok(ops) = parser.parse_line(line, syntaxes) {
            let mut cursor = 0usize;
            for (index, op) in ops {
                let index = index.min(line.len());
                if index > cursor {
                    push_span(
                        offset + cursor..offset + index,
                        &scopes,
                        &mut scope_cache,
                        &hl_theme,
                        &mut out,
                    );
                }
                let _ = scopes.apply(&op);
                cursor = index;
            }
            if cursor < line.len() {
                push_span(
                    offset + cursor..offset + line.len(),
                    &scopes,
                    &mut scope_cache,
                    &hl_theme,
                    &mut out,
                );
            }
        }
        offset += line.len();
    }
    out
}

/// Highlight `lines` sequentially with persistent parser state across line breaks (so multi-line
/// comments and strings stay highlighted), returning one highlight vector per line with byte
/// offsets local to each line (without any synthetic newline).
pub fn highlight_lines<S: AsRef<str>>(
    language: &str,
    lines: &[S],
    theme: &Theme,
) -> Vec<Vec<(Range<usize>, HighlightStyle)>> {
    let total_bytes: usize = lines.iter().map(|s| s.as_ref().len()).sum();
    if lines.is_empty() || total_bytes > MAX_HIGHLIGHT_BYTES {
        return vec![Vec::new(); lines.len()];
    }
    let Some(syntax) = find_syntax(language) else {
        return vec![Vec::new(); lines.len()];
    };
    let hl_theme = highlight_theme_for(theme);
    let syntaxes = &*SYNTAXES;
    let mut parser = ParseState::new(syntax);
    let mut scopes = ScopeStack::new();
    let mut scope_cache = HashMap::<Scope, Option<&'static str>>::new();
    let mut per_line = Vec::with_capacity(lines.len());
    let mut buf = String::new();

    for line in lines {
        let line = line.as_ref();
        let mut out: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
        buf.clear();
        buf.push_str(line);
        buf.push('\n');
        let line_len = line.len();
        if let Ok(ops) = parser.parse_line(&buf, syntaxes) {
            let mut cursor = 0usize;
            for (index, op) in ops {
                let clamped = index.min(line_len);
                if clamped > cursor {
                    push_span(
                        cursor..clamped,
                        &scopes,
                        &mut scope_cache,
                        &hl_theme,
                        &mut out,
                    );
                }
                let _ = scopes.apply(&op);
                cursor = clamped;
            }
            if cursor < line_len {
                push_span(
                    cursor..line_len,
                    &scopes,
                    &mut scope_cache,
                    &hl_theme,
                    &mut out,
                );
            }
        }
        per_line.push(out);
    }
    per_line
}

fn highlight_theme_for(theme: &Theme) -> std::sync::Arc<HighlightTheme> {
    if is_dark_theme(theme) {
        HighlightTheme::default_dark()
    } else {
        HighlightTheme::default_light()
    }
}

fn push_span(
    range: Range<usize>,
    scopes: &ScopeStack,
    cache: &mut HashMap<Scope, Option<&'static str>>,
    hl_theme: &HighlightTheme,
    out: &mut Vec<(Range<usize>, HighlightStyle)>,
) {
    if range.is_empty() {
        return;
    }
    let Some(name) = scopes
        .scopes
        .iter()
        .rev()
        .find_map(|scope| *cache.entry(*scope).or_insert_with(|| semantic_name(*scope)))
    else {
        return;
    };
    let Some(style) = hl_theme.style.syntax.style(name) else {
        return;
    };
    if style == HighlightStyle::default() {
        return;
    }
    match out.last_mut() {
        Some((prev_range, prev_style)) if prev_range.end == range.start && *prev_style == style => {
            prev_range.end = range.end;
        }
        _ => out.push((range, style)),
    }
}

fn find_syntax(language: &str) -> Option<&'static SyntaxReference> {
    let trimmed = language.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let alias = match lower.as_str() {
        "text" | "txt" | "plain" | "plaintext" | "code" => return None,
        "rust" => "rs",
        "typescript" => "ts",
        "javascript" => "js",
        "python" | "py3" => "py",
        "shell" | "bash" | "zsh" => "sh",
        "csharp" | "c#" => "cs",
        "c++" => "cpp",
        "make" | "makefile" => "Makefile",
        "markdown" => "md",
        "yml" => "yaml",
        "dockerfile" => "Dockerfile",
        other => other,
    };
    let syntaxes = &*SYNTAXES;
    [trimmed, lower.as_str(), alias]
        .into_iter()
        .find_map(|tok| syntaxes.find_syntax_by_token(tok))
        .filter(|syntax| syntax.name != "Plain Text")
}

/// Map a TextMate scope to the semantic syntax name used by `gpui-component`'s [`HighlightTheme`].
fn semantic_name(scope: Scope) -> Option<&'static str> {
    let scope = scope.build_string();
    let is = |prefix: &str| {
        scope == prefix
            || scope
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('.'))
    };

    let name = if is("punctuation.definition.string")
        || is("punctuation.definition.comment")
        || is("invalid")
        || is("meta")
        || is("source")
        || is("text")
    {
        return None;
    } else if is("comment.block.documentation") || is("comment.line.documentation") {
        "comment.doc"
    } else if is("comment") {
        "comment"
    } else if is("constant.character.escape") {
        "string.escape"
    } else if is("string.regexp") {
        "string.regex"
    } else if is("string") {
        "string"
    } else if is("constant.numeric") {
        "number"
    } else if is("constant.language.boolean") {
        "boolean"
    } else if is("constant") || is("support.constant") || is("entity.name.constant") {
        "constant"
    } else if is("keyword.operator") {
        "operator"
    } else if is("keyword.control.import") || is("meta.preprocessor") {
        "preproc"
    } else if is("keyword") || is("storage.modifier") {
        "keyword"
    } else if is("storage.type") {
        if [
            "class",
            "enum",
            "function",
            "impl",
            "interface",
            "module",
            "namespace",
            "struct",
            "trait",
            "type",
            "let",
            "var",
            "const",
        ]
        .iter()
        .any(|word| scope.contains(word))
        {
            "keyword"
        } else {
            "type"
        }
    } else if is("entity.name.function")
        || is("support.function")
        || is("variable.function")
        || is("meta.function-call")
    {
        "function"
    } else if is("entity.name.type")
        || is("entity.name.class")
        || is("entity.name.struct")
        || is("entity.name.enum")
        || is("entity.name.trait")
        || is("entity.name.interface")
        || is("entity.other.inherited-class")
        || is("support.type")
        || is("support.class")
    {
        "type"
    } else if is("entity.name.tag") {
        "tag"
    } else if is("entity.other.attribute-name") {
        "attribute"
    } else if is("entity.name.label") {
        "label"
    } else if is("entity.name.section") || is("markup.heading") {
        "title"
    } else if is("variable.language") {
        "variable.special"
    } else if is("variable.other.member")
        || is("variable.other.property")
        || is("support.variable.property")
        || is("meta.mapping.key")
    {
        "property"
    } else if is("variable") {
        "variable"
    } else if is("markup.bold") {
        "emphasis.strong"
    } else if is("markup.italic") {
        "emphasis"
    } else if is("markup.underline.link") {
        "link_uri"
    } else if is("markup.raw") {
        "text.literal"
    } else if is("markup.list") {
        "punctuation.list_marker"
    } else if is("punctuation.section") || is("punctuation.definition.block") {
        "punctuation.bracket"
    } else if is("punctuation.separator") || is("punctuation.terminator") {
        "punctuation.delimiter"
    } else if is("punctuation") {
        "punctuation"
    } else {
        return None;
    };
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_rust_and_typescript_with_valid_non_overlapping_ranges() {
        let dark = Theme::dark();
        let rust_src = "pub fn answer(x: i32) -> i32 { // hi\n    let s = \"ok\";\n    x + 1\n}";
        let spans = highlight_code("rust", rust_src, &dark);
        assert!(!spans.is_empty());
        let mut prev_end = 0;
        for (range, _) in &spans {
            assert!(range.start >= prev_end);
            assert!(range.start < range.end);
            assert!(rust_src.is_char_boundary(range.start));
            assert!(rust_src.is_char_boundary(range.end));
            prev_end = range.end;
        }

        let ts_spans = highlight_code(
            "tsx",
            "const greet = (n: string) => `hi ${n}`;",
            &Theme::light(),
        );
        assert!(!ts_spans.is_empty());
        assert!(highlight_code("plain", "hello world", &dark).is_empty());
        assert!(highlight_code("", "hello world", &dark).is_empty());
    }

    #[test]
    fn highlight_lines_preserves_multiline_block_comments() {
        let lines = vec![
            "/* start".to_owned(),
            "   middle".to_owned(),
            "   end */".to_owned(),
            "let x = 42;".to_owned(),
        ];
        let per_line = highlight_lines("rs", &lines, &Theme::dark());
        assert_eq!(per_line.len(), 4);
        assert!(
            !per_line[1].is_empty(),
            "middle of block comment should be highlighted"
        );
        assert!(
            !per_line[3].is_empty(),
            "code after block comment should be highlighted"
        );
    }

    #[test]
    fn language_for_path_detects_extensions_and_special_names() {
        assert_eq!(language_for_path("crates/xctx/src/main.rs"), Some("rust"));
        assert_eq!(language_for_path("Cargo.toml"), Some("toml"));
        assert_eq!(language_for_path("Makefile"), Some("make"));
        assert_eq!(language_for_path("web/App.tsx"), Some("tsx"));
        assert_eq!(language_for_path("README.md"), Some("md"));
        assert_eq!(language_for_path("no_extension"), None);
    }
}
