//! Masking secret values before text leaves the user: control responses for agents, shared notes and Inbox
//! previews.
//!
//! A [`Redactor`] holds named secrets (usually [`crate::mise_env::EnvReport::secrets`]) and replaces every
//! occurrence of a value with `[REDACTED:<NAME>]`. Values shorter than [`MIN_SECRET_LEN`] are not replaced inside
//! free text, where they would mangle ordinary words, but JSON fields named after any secret are always masked.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde_json::Value;

/// The shortest value replaced wherever it appears in text.
pub const MIN_SECRET_LEN: usize = 4;

/// What a secret named `name` is replaced with.
pub fn mask(name: &str) -> String {
    format!("[REDACTED:{name}]")
}

/// A set of named secret values to mask.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Redactor {
    /// `(value, name)`, longest value first, so a secret containing another is masked whole.
    secrets: Vec<(String, String)>,
    names: Vec<String>,
}

impl Redactor {
    /// A redactor for `secrets`, as `(name, value)` pairs. Empty values are ignored.
    pub fn new(secrets: impl IntoIterator<Item = (String, String)>) -> Redactor {
        let mut redactor = Redactor::default();
        redactor.extend(secrets);
        redactor
    }

    /// Add more secrets; a value already known keeps its first name.
    pub fn extend(&mut self, secrets: impl IntoIterator<Item = (String, String)>) {
        for (name, value) in secrets {
            if !self.names.contains(&name) {
                self.names.push(name.clone());
            }
            if value.is_empty() || self.secrets.iter().any(|(v, _)| *v == value) {
                continue;
            }
            self.secrets.push((value, name));
        }
        self.secrets
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    }

    /// True when there is nothing to mask.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// The secrets' names.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// `text` with every secret value of at least [`MIN_SECRET_LEN`] characters masked. Borrows when nothing
    /// changed.
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut out: Cow<'a, str> = Cow::Borrowed(text);
        for (value, name) in &self.secrets {
            if value.chars().count() < MIN_SECRET_LEN || !out.contains(value.as_str()) {
                continue;
            }
            out = Cow::Owned(out.replace(value.as_str(), &mask(name)));
        }
        out
    }

    /// Mask secrets throughout a JSON value: every string is [`Redactor::redact`]ed, and an object field named
    /// after a secret has its whole value replaced, whatever its length.
    pub fn redact_json(&self, value: &mut Value) {
        match value {
            Value::String(s) => {
                if let Cow::Owned(new) = self.redact(s) {
                    *s = new;
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|v| self.redact_json(v)),
            Value::Object(map) => {
                for (key, v) in map.iter_mut() {
                    if self.names.iter().any(|n| n == key) && !v.is_null() {
                        *v = Value::String(mask(key));
                    } else {
                        self.redact_json(v);
                    }
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
}

impl From<&BTreeMap<String, String>> for Redactor {
    fn from(secrets: &BTreeMap<String, String>) -> Self {
        Redactor::new(secrets.iter().map(|(k, v)| (k.clone(), v.clone())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn redactor() -> Redactor {
        Redactor::new([
            ("API_TOKEN".to_owned(), "tok-123".to_owned()),
            ("LONG".to_owned(), "tok-123-and-more".to_owned()),
            ("PIN".to_owned(), "42".to_owned()),
            ("EMPTY".to_owned(), String::new()),
        ])
    }

    #[test]
    fn values_are_masked_longest_first_and_short_ones_left_in_text() {
        let r = redactor();
        assert_eq!(
            r.redact("curl -H tok-123 x tok-123-and-more pin 42"),
            "curl -H [REDACTED:API_TOKEN] x [REDACTED:LONG] pin 42"
        );
        assert!(matches!(r.redact("nothing here"), Cow::Borrowed(_)));
    }

    #[test]
    fn json_strings_and_secret_named_fields_are_masked() {
        let mut v = json!({
            "PIN": "42",
            "note": ["has tok-123 inside", 3],
            "EMPTY": null,
            "nested": { "API_TOKEN": 7 },
        });
        redactor().redact_json(&mut v);
        assert_eq!(
            v,
            json!({
                "PIN": "[REDACTED:PIN]",
                "note": ["has [REDACTED:API_TOKEN] inside", 3],
                "EMPTY": null,
                "nested": { "API_TOKEN": "[REDACTED:API_TOKEN]" },
            })
        );
    }

    #[test]
    fn an_empty_redactor_changes_nothing() {
        let r = Redactor::default();
        assert!(r.is_empty());
        assert_eq!(r.redact("tok-123"), "tok-123");
    }
}
