//! Matching plugin events in output lines (a tool's output, or a pane's), and the loop action each maps to.

use serde::Serialize;
use serde_json::Value;

use crate::manifest::{EventMatcher, LoopAction, Manifest};

/// An event that fired on a line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FiredEvent {
    /// The event's name.
    pub name: String,
    /// The loop action it maps to.
    pub action: LoopAction,
    /// The line that fired it, as given (mask it before it leaves the space, if it was not already).
    pub line: String,
}

impl EventMatcher {
    /// True when `line` fires the event: the regex matches somewhere in it, or it is a JSON object whose field
    /// (a dotted path) is present and not null, and equal to `equals` when given. A number or boolean is
    /// compared as its JSON text, so `equals = "500"` matches `{"status": 500}`.
    pub fn matches(&self, line: &str) -> bool {
        match self {
            EventMatcher::LineRegex(pattern) => pattern.regex().is_match(line),
            EventMatcher::JsonField { field, equals } => {
                let trimmed = line.trim();
                if !trimmed.starts_with('{') {
                    return false;
                }
                let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                    return false;
                };
                let mut current = &value;
                for part in field.split('.') {
                    match current.get(part) {
                        Some(next) => current = next,
                        None => return false,
                    }
                }
                match (equals, current) {
                    (_, Value::Null) => false,
                    (None, _) => true,
                    (Some(want), Value::String(s)) => s == want,
                    (Some(want), Value::Number(n)) => n.to_string() == *want,
                    (Some(want), Value::Bool(b)) => b.to_string() == *want,
                    (Some(_), Value::Array(_) | Value::Object(_)) => false,
                }
            }
        }
    }
}

/// Every event of `manifest` that fires on each of `lines`, in line order and then manifest order. One line can
/// fire several events.
pub fn match_events<'a>(
    manifest: &Manifest,
    lines: impl IntoIterator<Item = &'a str>,
) -> Vec<FiredEvent> {
    let mut fired = Vec::new();
    if manifest.events.is_empty() {
        return fired;
    }
    for line in lines {
        for event in &manifest.events {
            if event.matcher.matches(line) {
                fired.push(FiredEvent {
                    name: event.name.clone(),
                    action: event.action,
                    line: line.to_owned(),
                });
            }
        }
    }
    fired
}

/// The strongest of `fired`'s actions for one loop stage: `fail` wins over `pass`, which wins over `notice`.
/// None when nothing fired.
pub fn loop_outcome(fired: &[FiredEvent]) -> Option<LoopAction> {
    let rank = |a: LoopAction| match a {
        LoopAction::Fail => 2,
        LoopAction::Pass => 1,
        LoopAction::Notice => 0,
    };
    fired.iter().map(|e| e.action).max_by_key(|a| rank(*a))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Pattern;

    #[test]
    fn json_fields_compare_as_text() {
        let m = EventMatcher::JsonField {
            field: "response.status".into(),
            equals: Some("500".into()),
        };
        assert!(m.matches(r#"{"response": {"status": 500}}"#));
        assert!(!m.matches(r#"{"response": {"status": 200}}"#));
        assert!(!m.matches("not json"));
        assert!(!m.matches(r#"{"response": null}"#));
        let present = EventMatcher::JsonField {
            field: "error".into(),
            equals: None,
        };
        assert!(present.matches(r#"  {"error": "boom"}"#));
        assert!(!present.matches(r#"{"error": null}"#));
        let flag = EventMatcher::JsonField {
            field: "ok".into(),
            equals: Some("true".into()),
        };
        assert!(flag.matches(r#"{"ok": true}"#));
    }

    #[test]
    fn regex_matches_anywhere() {
        let m = EventMatcher::LineRegex(
            Pattern::new(r"HTTP/1\.1 5\d\d").unwrap_or_else(|e| panic!("{e}")),
        );
        assert!(m.matches("< HTTP/1.1 503 Service Unavailable"));
        assert!(!m.matches("< HTTP/1.1 200 OK"));
    }

    #[test]
    fn fail_beats_pass_beats_notice() {
        let e = |action| FiredEvent {
            name: "a.b".into(),
            action,
            line: String::new(),
        };
        assert_eq!(loop_outcome(&[]), None);
        assert_eq!(
            loop_outcome(&[e(LoopAction::Notice), e(LoopAction::Pass)]),
            Some(LoopAction::Pass)
        );
        assert_eq!(
            loop_outcome(&[
                e(LoopAction::Pass),
                e(LoopAction::Fail),
                e(LoopAction::Notice)
            ]),
            Some(LoopAction::Fail)
        );
    }
}
