//! Placeholders in argv elements and URLs: `{param}` for a tool parameter, `{PORT}`, `{WORKTREE}` and `{SPACE}`
//! for the space, and `{{`/`}}` for literal braces.
//!
//! An element is parsed once into [`Piece`]s and filled in one pass, so a value is never scanned again: a value
//! containing `{other}` stays that literal text.

/// A value the space supplies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CtxVar {
    /// `{PORT}`: the space's port.
    Port,
    /// `{WORKTREE}`: the space's worktree.
    Worktree,
    /// `{SPACE}`: the space's slug.
    Space,
}

/// One part of a templated element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    /// Text used as is.
    Lit(String),
    /// `{name}`: a tool parameter.
    Param(String),
    /// `{PORT}`, `{WORKTREE}` or `{SPACE}`.
    Ctx(CtxVar),
}

/// True for a valid parameter name: a lowercase letter, then lowercase letters, digits and `_`.
pub(crate) fn is_param_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Split `element` into pieces, or say why it is not a valid template.
pub(crate) fn parse(element: &str) -> Result<Vec<Piece>, String> {
    let mut pieces = Vec::new();
    let mut lit = String::new();
    let mut chars = element.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                lit.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                lit.push('}');
            }
            '}' => return Err("a lone `}`; write `}}` for a literal brace".to_owned()),
            '{' => {
                let mut name = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '}' {
                        closed = true;
                        break;
                    }
                    name.push(c);
                }
                if !closed {
                    return Err("an unclosed `{`; write `{{` for a literal brace".to_owned());
                }
                let piece = match name.as_str() {
                    "PORT" => Piece::Ctx(CtxVar::Port),
                    "WORKTREE" => Piece::Ctx(CtxVar::Worktree),
                    "SPACE" => Piece::Ctx(CtxVar::Space),
                    n if is_param_name(n) => Piece::Param(n.to_owned()),
                    n => {
                        return Err(format!(
                            "`{{{n}}}` is not a placeholder: use a parameter name, {{PORT}}, {{WORKTREE}} or \
                             {{SPACE}}, or `{{{{`/`}}}}` for literal braces"
                        ));
                    }
                };
                if !lit.is_empty() {
                    pieces.push(Piece::Lit(std::mem::take(&mut lit)));
                }
                pieces.push(piece);
            }
            c => lit.push(c),
        }
    }
    if !lit.is_empty() || pieces.is_empty() {
        pieces.push(Piece::Lit(lit));
    }
    Ok(pieces)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_and_escapes() {
        assert_eq!(
            parse("--port={PORT}/{path}").ok(),
            Some(vec![
                Piece::Lit("--port=".into()),
                Piece::Ctx(CtxVar::Port),
                Piece::Lit("/".into()),
                Piece::Param("path".into()),
            ])
        );
        assert_eq!(parse("a{{2}}").ok(), Some(vec![Piece::Lit("a{2}".into())]));
        assert_eq!(parse("").ok(), Some(vec![Piece::Lit(String::new())]));
        assert!(parse("{Bad}").is_err());
        assert!(parse("{open").is_err());
        assert!(parse("close}").is_err());
        assert!(parse("{}").is_err());
    }
}
