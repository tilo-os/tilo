//! The id an app is known by: its reverse-DNS name, `so.tilo.chat`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub mod apps;
pub mod chat;

pub const MAX_APP_ID_BYTES: usize = 255;

const _: () = assert!(MAX_APP_ID_BYTES == u8::MAX as usize);

/// Whether `segment` is one non-empty component of an application identifier:
/// ASCII letters, digits, `-` and `_`.
#[must_use]
pub fn is_valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Whether `segment` is one non-empty lowercase component of an application
/// identifier: lowercase ASCII letters, digits, `-` and `_`.
#[must_use]
pub fn is_valid_lowercase_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    Mixed,
    Lower,
}

/// Rules for checking an application identifier against the shared segment
/// grammar: non-empty `.`-separated segments, never empty, and never starting
/// or ending with `.`.
///
/// [`Self::CANONICAL`] matches [`AppId::parse`]. Sites that deliberately vary
/// the length cap, dot requirement, case or leading hyphen start from
/// [`Self::CANONICAL`] so no call site writes its own character or segment
/// loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grammar {
    max_bytes: u8,
    require_dot: bool,
    case: Case,
    allow_leading_hyphen: bool,
}

impl Grammar {
    /// The grammar [`AppId::parse`] enforces: at most [`MAX_APP_ID_BYTES`],
    /// dot required, uppercase and `_` allowed, and no empty segment.
    pub const CANONICAL: Self = Self {
        max_bytes: u8::MAX,
        require_dot: true,
        case: Case::Mixed,
        allow_leading_hyphen: true,
    };

    /// Narrow the maximum length of a matching identifier to at most
    /// `max_bytes` bytes.
    ///
    /// Taking `u8` makes a cap above [`MAX_APP_ID_BYTES`] (`u8::MAX`)
    /// unrepresentable, and a larger cap never relaxes one already set.
    #[must_use]
    pub const fn max_bytes(mut self, max_bytes: u8) -> Self {
        if max_bytes < self.max_bytes {
            self.max_bytes = max_bytes;
        }
        self
    }

    /// Allow a single segment without a `.` (such as a desktop-file id).
    #[must_use]
    pub const fn allow_single_segment(mut self) -> Self {
        self.require_dot = false;
        self
    }

    /// Refuse ASCII uppercase letters.
    #[must_use]
    pub const fn lowercase_only(mut self) -> Self {
        self.case = Case::Lower;
        self
    }

    /// Refuse a leading `-` so the identifier cannot be parsed as a flag.
    #[must_use]
    pub const fn forbid_leading_hyphen(mut self) -> Self {
        self.allow_leading_hyphen = false;
        self
    }

    /// Whether `text` satisfies this grammar.
    #[must_use]
    pub fn matches(self, text: &str) -> bool {
        if text.len() > usize::from(self.max_bytes) {
            return false;
        }
        if self.require_dot && !text.contains('.') {
            return false;
        }
        if !self.allow_leading_hyphen && text.starts_with('-') {
            return false;
        }
        match self.case {
            Case::Mixed => text.split('.').all(is_valid_segment),
            Case::Lower => text.split('.').all(is_valid_lowercase_segment),
        }
    }
}

/// What a person installs and runs, named: its reverse-DNS id, which the dot keeps
/// apart from a label or a scope token.
///
/// [`Self::parse`] is the one check of the grammar; [`Self::new`] takes an id a
/// launcher, a manifest or a package produced as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AppId(String);

impl AppId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

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
    pub fn is_valid(text: &str) -> bool {
        Grammar::CANONICAL.matches(text)
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

impl From<&AppId> for AppId {
    fn from(id: &AppId) -> Self {
        id.clone()
    }
}

impl From<AppId> for String {
    fn from(id: AppId) -> Self {
        id.0
    }
}

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
    use super::{AppId, Grammar, is_valid_lowercase_segment, is_valid_segment};

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

    #[test]
    fn canonical_grammar_matches_parse() {
        let longest = format!("a.{}", "b".repeat(253));
        let too_long = format!("a.{}", "b".repeat(254));
        for (id, expected) in [
            ("so.tilo.chat", true),
            ("org.gnome.Chess-2", true),
            ("org.example.My_App", true),
            ("-org.example.Flag", true),
            (longest.as_str(), true),
            (too_long.as_str(), false),
            ("", false),
            ("chat", false),
            (".so.tilo", false),
            ("so.tilo.", false),
            ("so..chat", false),
            ("..", false),
            (".", false),
            ("so.tilo/chat", false),
            ("so.tilo chat", false),
            ("so.tilo.n\u{f6}tes", false),
            ("so.tilo.chat\0", false),
        ] {
            assert_eq!(AppId::parse(id).is_ok(), expected, "{id:?}");
            assert_eq!(AppId::is_valid(id), expected, "{id:?}");
            assert_eq!(Grammar::CANONICAL.matches(id), expected, "{id:?}");
        }
    }

    #[test]
    fn segments_and_grammar_modifiers_enforce_each_rule() {
        for (segment, mixed, lower) in [
            ("chat", true, true),
            ("my_app-2", true, true),
            ("Chat", true, false),
            ("-flag", true, true),
            ("", false, false),
            ("a.b", false, false),
            ("a/b", false, false),
            ("a b", false, false),
        ] {
            assert_eq!(is_valid_segment(segment), mixed, "{segment:?}");
            assert_eq!(is_valid_lowercase_segment(segment), lower, "{segment:?}");
        }

        let single = Grammar::CANONICAL.allow_single_segment();
        assert!(single.matches("firefox"));
        assert!(!single.matches(""));
        assert!(!single.matches(".firefox"));
        assert!(!single.matches("firefox."));
        assert!(!single.matches("a..b"));

        let lower = Grammar::CANONICAL.max_bytes(128).lowercase_only();
        assert!(lower.matches("so.tilo.my_app-2"));
        assert!(!lower.matches("so.tilo.MyApp"));
        assert!(!lower.matches("notes"));
        assert!(lower.matches(&format!("a.{}", "b".repeat(126))));
        assert!(!lower.matches(&format!("a.{}", "b".repeat(127))));

        let no_flag = Grammar::CANONICAL.forbid_leading_hyphen();
        assert!(no_flag.matches("org.gnome.Chess-2"));
        assert!(!no_flag.matches("-org.gnome.Chess"));
        assert!(!no_flag.matches("--all.flag"));

        // `max_bytes` cannot exceed `MAX_APP_ID_BYTES` (`u8::MAX`) or relax a
        // tighter cap already set on the grammar.
        let narrowed = Grammar::CANONICAL.max_bytes(160).max_bytes(u8::MAX);
        assert!(narrowed.matches(&format!("a.{}", "b".repeat(158))));
        assert!(!narrowed.matches(&format!("a.{}", "b".repeat(159))));
        let canonical_cap = Grammar::CANONICAL.max_bytes(u8::MAX);
        assert!(canonical_cap.matches(&format!("a.{}", "b".repeat(253))));
        assert!(!canonical_cap.matches(&format!("a.{}", "b".repeat(254))));
    }
}
