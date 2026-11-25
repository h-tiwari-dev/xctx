//! Reading a built-in's arguments, checked again here so a call that skipped `plan_call` cannot slip through.

use serde_json::{Map, Value};

use crate::error::{ConnectorError, Result};

/// A call's arguments, with the names the built-in accepts.
pub(crate) struct Args<'a> {
    tool: &'a str,
    map: &'a Map<String, Value>,
}

impl<'a> Args<'a> {
    /// Check that `map` names only `known` parameters.
    pub(crate) fn new(tool: &'a str, map: &'a Map<String, Value>, known: &[&str]) -> Result<Self> {
        if let Some(k) = map.keys().find(|k| !known.contains(&k.as_str())) {
            return Err(ConnectorError::BadArgs(format!(
                "{tool} has no parameter {k:?}; it takes {}",
                known.join(", ")
            )));
        }
        Ok(Args { tool, map })
    }

    fn get(&self, name: &str) -> Option<&'a Value> {
        self.map.get(name).filter(|v| !v.is_null())
    }

    /// An optional string.
    pub(crate) fn str(&self, name: &str) -> Result<Option<&'a str>> {
        match self.get(name) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.as_str())),
            Some(_) => Err(self.bad(name, "expected a string")),
        }
    }

    /// A required, non-blank string.
    pub(crate) fn required_str(&self, name: &str) -> Result<&'a str> {
        match self.str(name)? {
            Some(s) if !s.trim().is_empty() => Ok(s),
            _ => Err(self.bad(name, "is required")),
        }
    }

    /// An optional whole number in `min..=max`.
    pub(crate) fn int(&self, name: &str, min: i64, max: i64) -> Result<Option<i64>> {
        match self.get(name) {
            None => Ok(None),
            Some(Value::Number(n)) => match n.as_i64() {
                Some(i) if (min..=max).contains(&i) => Ok(Some(i)),
                _ => Err(self.bad(
                    name,
                    &format!("expected a whole number from {min} to {max}"),
                )),
            },
            Some(_) => Err(self.bad(name, "expected a whole number")),
        }
    }

    fn bad(&self, name: &str, message: &str) -> ConnectorError {
        ConnectorError::BadArgs(format!("{}: {name} {message}", self.tool))
    }
}
