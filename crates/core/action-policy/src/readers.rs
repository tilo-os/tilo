//! Who reads a record of a kind, when not every member of the workspace does.

use serde::{Deserialize, Serialize};

/// Who reads a record of a kind, when it is not every member: what a tilolang type says with
/// `(readers: ...)` and a Rust resource with `#[resource(readers = "...")]`, carried in the
/// kind's published surface. The store works out the reader set from it, the record's author and
/// its fields, and places the record in a document of that set alone, sealed to its key group; a
/// record whose set changes moves to the new set's group, sealed afresh. Whoever left the set
/// keeps what they already held of it, its ciphertext and the keys they had, as after any
/// revocation, and nothing written after.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderRule {
    /// `author`, `author until sent`: the person who made the record, on each of their devices;
    /// every member once the `Bool` field `until` names holds, which it never stops holding (a
    /// draft until it is sent).
    Author {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<String>,
    },
    /// A field's name: the people its `Set<Actor>` names.
    Field { field: String },
    /// `author and moderator`: the person who made the record, and every member whose role
    /// includes `role` ([`crate::Role::includes`]) whenever it is read. Who holds the role is
    /// the workspace's governance's to say at each moment, never written into the record: a
    /// member given the role reads what was written before, and one who loses it or leaves
    /// reads nothing written after.
    AuthorAnd { role: crate::Role },
}

impl ReaderRule {
    /// The rule a type's options write as `words` (`author`, `author until sent`,
    /// `author and moderator`, or a field's name); `None` when they name no rule.
    #[must_use]
    pub fn parse(words: &str) -> Option<Self> {
        let field = |word: &str| {
            !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        match words.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["author"] => Some(Self::Author { until: None }),
            ["author", "until", until] if field(until) => Some(Self::Author {
                until: Some((*until).to_owned()),
            }),
            ["author", "and", role] => {
                crate::Role::parse(role).map(|role| Self::AuthorAnd { role })
            }
            [name] if field(name) && *name != "author" => Some(Self::Field {
                field: (*name).to_owned(),
            }),
            _ => None,
        }
    }
}

impl std::fmt::Display for ReaderRule {
    /// The rule as a type's options write it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Author { until: None } => f.write_str("author"),
            Self::Author { until: Some(field) } => write!(f, "author until {field}"),
            Self::Field { field } => f.write_str(field),
            Self::AuthorAnd { role } => write!(f, "author and {}", role.key()),
        }
    }
}
