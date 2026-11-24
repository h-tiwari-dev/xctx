//! The error type of the shared space and the scratch directory.

use std::path::PathBuf;

/// A shared-space or scratch-directory failure the app can show.
#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    /// Reading or writing a file or directory failed.
    #[error(transparent)]
    Store(#[from] xctx_core::Error),

    /// `shared.json` exists but is not a shared space this build can parse. It is left untouched.
    #[error("{path} is damaged: {source}")]
    Corrupt {
        /// The file.
        path: PathBuf,
        /// What the parser reported.
        source: serde_json::Error,
    },

    /// `shared.json` was written by a newer xctx. It is left untouched.
    #[error("{path} is version {found}; this xctx reads up to version {supported}")]
    UnsupportedVersion {
        /// The file.
        path: PathBuf,
        /// Its version.
        found: u32,
        /// The newest this build reads.
        supported: u32,
    },

    /// A space slug that cannot name a directory safely (empty, `.`-prefixed, or with characters other than ASCII
    /// letters, digits, `-` and `_`).
    #[error("{0:?} is not a valid space slug")]
    InvalidSlug(String),

    /// A required text field was empty or only whitespace.
    #[error("{0} must not be empty")]
    EmptyField(&'static str),

    /// The space has no contract yet, so there is nothing to agree to.
    #[error("the space has no contract yet")]
    NoContract,

    /// No finding has this id.
    #[error("no finding {0}")]
    FindingNotFound(u64),

    /// A scratch artifact path that is not a plain relative path inside the scratch directory.
    #[error("{path:?} is not a valid artifact path: {reason}")]
    InvalidArtifactPath {
        /// The path as given.
        path: String,
        /// Why it was refused.
        reason: &'static str,
    },
}

/// The result type of the shared space's fallible operations.
pub type Result<T, E = ChannelError> = std::result::Result<T, E>;

/// Refuse `value` for `field` when it is empty or only whitespace.
pub(crate) fn require(field: &'static str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(ChannelError::EmptyField(field))
    } else {
        Ok(())
    }
}

/// Refuse a slug that could escape or confuse the directory it names.
pub(crate) fn validate_slug(slug: &str) -> Result<()> {
    let valid = !slug.is_empty()
        && !slug.starts_with('.')
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(ChannelError::InvalidSlug(slug.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_limited_to_safe_characters() {
        assert!(validate_slug("rate-limit_2").is_ok());
        for bad in [
            "", ".", "..", ".hidden", "a/b", "a\\b", "a\0b", "sp ace", "ü",
        ] {
            assert!(
                matches!(validate_slug(bad), Err(ChannelError::InvalidSlug(_))),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn blank_fields_are_refused() {
        assert!(require("body", "x").is_ok());
        assert!(matches!(
            require("body", " \n\t"),
            Err(ChannelError::EmptyField("body"))
        ));
    }
}
