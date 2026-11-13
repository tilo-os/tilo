//! The id an app is known by: its reverse-DNS name, `so.tilo.chat`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The longest id accepted.
pub const MAX_APP_ID_BYTES: usize = 255;

/// What a person installs and runs, named: `so.tilo.chat`. Always the full id; the dot keeps it
/// apart from a label or a scope token.
///
/// [`Self::parse`] is the one check of the grammar. An id a launcher, a manifest or a package
/// produced is already that: [`Self::new`] takes it as it is.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AppId(String);

impl AppId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// # Errors
    ///
    /// [`InvalidAppId`] unless the id is dot-separated, non-empty parts of ASCII letters, digits,
    /// `-` and `_`.
    pub fn parse(text: &str) -> Result<Self, InvalidAppId> {
        let parts_ok = text.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        });
        if parts_ok && text.contains('.') && text.len() <= MAX_APP_ID_BYTES {
            Ok(Self(text.to_owned()))
        } else {
            Err(InvalidAppId(text.to_owned()))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for AppId {
    type Err = InvalidAppId;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

impl PartialEq<str> for AppId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for AppId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl std::ops::Deref for AppId {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for AppId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for AppId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for AppId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

impl From<String> for AppId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl From<AppId> for String {
    fn from(id: AppId) -> Self {
        id.0
    }
}

/// Text that is not an app id, kept as it came so the message can show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidAppId(String);

impl fmt::Display for InvalidAppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} is not an app id", self.0)
    }
}

impl std::error::Error for InvalidAppId {}

#[cfg(test)]
mod tests {
    use super::AppId;

    #[test]
    fn an_app_id_is_dot_separated() {
        assert!(AppId::parse("so.tilo.chat").is_ok());
        assert!(AppId::parse("chat").is_err(), "an app id always has a dot");
        assert!(AppId::parse("so..chat").is_err());
        assert!(AppId::parse("so.tilo.").is_err());
        assert!(AppId::parse(".so.tilo").is_err());
        assert!(AppId::parse("so.tilo.chat/x").is_err());
    }

    #[test]
    fn an_empty_id_is_kept_apart_from_a_named_one() {
        assert!(AppId::parse("").is_err());
        assert_eq!(AppId::new(""), AppId::from(""));
    }
}
