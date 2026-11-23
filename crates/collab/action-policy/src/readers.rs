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
}

impl std::fmt::Display for ReaderRule {
    /// The rule as a type's options write it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Author { until: None } => f.write_str("author"),
            Self::Author { until: Some(field) } => write!(f, "author until {field}"),
            Self::Field { field } => f.write_str(field),
        }
    }
}
