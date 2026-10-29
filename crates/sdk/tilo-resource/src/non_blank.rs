//! Text that says something: a title, a name, a message.
//!
//! An action that refuses blank text takes a [`NonBlank`] instead of a
//! `String`. The only ways to make one are [`NonBlank::parse`] and reading it
//! from JSON, and both refuse text that is empty or only whitespace. So a
//! view cannot hand such an action whatever its field holds: it parses the
//! field, and a control bound through [`ActionRef::try_map`] stays disabled
//! while the field does not parse. An agent, a remote peer and the shell's
//! argument step are checked by the same type at the boundary, and the
//! schema they read says it (`minLength` 1, a non-space character).
//!
//! The text is kept trimmed: what is stored is what the person meant, never
//! the spaces around it.
//!
//! [`ActionRef::try_map`]: crate::ActionRef::try_map
//!
//! A `String` is not a `NonBlank`; it has to be parsed:
//!
//! ```compile_fail
//! fn rename(name: tilo_resource::NonBlank) -> String {
//!     name.into()
//! }
//! rename(String::from("Groceries"));
//! ```
//!
//! ```compile_fail
//! let name: tilo_resource::NonBlank = String::from("Groceries").into();
//! ```
//!
//! ```
//! use tilo_resource::NonBlank;
//! assert_eq!(NonBlank::parse("  Groceries ").unwrap().as_str(), "Groceries");
//! assert!(NonBlank::parse(" \t").is_none());
//! ```

use std::fmt;

use serde_json::{Value, json};

/// Text with at least one character that is not whitespace, trimmed. See
/// the [module](self).
///
/// It is written, read and printed through the code for `String` (reading
/// then checks the text, in code compiled once), so an app that takes one
/// reuses what it already has for strings rather than a copy for this type.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct NonBlank(String);

impl NonBlank {
    /// `text` trimmed, or `None` when nothing is left.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| Self(trimmed.to_owned()))
    }

    /// `text` trimmed, or [`Blank`] when nothing is left: [`Self::parse`]
    /// for text already owned, as JSON reads it.
    #[inline(never)]
    fn from_owned(text: String) -> Result<Self, Blank> {
        if text.len() == text.trim().len() {
            return if text.is_empty() {
                Err(Blank)
            } else {
                Ok(Self(text))
            };
        }
        Self::parse(&text).ok_or(Blank)
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The text, owned.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

/// Why text is not a [`NonBlank`]: there is nothing in it but whitespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blank;

/// What [`Blank`] says.
const BLANK: &str = "blank text where something must be said";

impl fmt::Display for Blank {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(BLANK)
    }
}

impl std::error::Error for Blank {}

impl std::str::FromStr for NonBlank {
    type Err = Blank;

    fn from_str(text: &str) -> Result<Self, Blank> {
        Self::parse(text).ok_or(Blank)
    }
}

impl std::ops::Deref for NonBlank {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for NonBlank {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<NonBlank> for String {
    fn from(text: NonBlank) -> Self {
        text.0
    }
}

impl fmt::Debug for NonBlank {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for NonBlank {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialEq<str> for NonBlank {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for NonBlank {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl serde::Serialize for NonBlank {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for NonBlank {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Read as a `String` is (the reader every app already has), then
        // checked by code compiled once; the error is made from the plain
        // message, which needs no formatting machinery per reader.
        let text = String::deserialize(deserializer)?;
        Self::from_owned(text).map_err(|Blank| <D::Error as serde::de::Error>::custom(BLANK))
    }
}

impl crate::JsonSchema for NonBlank {
    fn json_schema() -> Value {
        json!({"type": "string", "minLength": 1, "pattern": "\\S"})
    }
    fn field_type(_: &mut crate::TypeSet) -> crate::FieldType {
        crate::FieldType::Text
    }
}

#[cfg(test)]
mod tests {
    use super::NonBlank;
    use crate::JsonSchema as _;

    #[test]
    fn parsing_trims_and_refuses_blank_text() {
        assert_eq!(NonBlank::parse(" a b ").unwrap(), "a b");
        assert_eq!(NonBlank::parse(""), None);
        assert_eq!(NonBlank::parse(" \n\t "), None);
    }

    #[test]
    fn json_is_read_through_the_same_parse() {
        let read = |value| serde_json::from_value::<NonBlank>(value);
        assert_eq!(read(serde_json::json!(" Ship it ")).unwrap(), "Ship it");
        assert!(read(serde_json::json!("   ")).is_err());
        assert!(read(serde_json::json!("")).is_err());
        assert!(read(serde_json::json!(null)).is_err());
        let written = serde_json::to_value(NonBlank::parse("x").unwrap()).unwrap();
        assert_eq!(written, serde_json::json!("x"));
    }

    #[test]
    fn the_schema_refuses_what_parsing_refuses() {
        let schema = NonBlank::json_schema();
        assert_eq!(schema["minLength"], 1);
        let pattern = schema["pattern"].as_str().unwrap();
        assert_eq!(pattern, "\\S");
    }
}
