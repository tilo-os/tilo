//! What History can honestly do with a resource type: one classification the
//! panel, the read door and the restore review all read, so "no past version
//! here" is never confused with "this text does not read", "written by a
//! newer app" or "cannot be shown as text".
//!
//! The classification is derived from what the resource type itself
//! declared, never guessed per app: the schema history restores content
//! with, and whether the type reads its record as text. A binary resource
//! has no restore schema and no text rendering, so it reports
//! [`HistoricalSupport::MetadataOnly`]: its versions, its metadata and its
//! content addresses are real, a textual diff of it would be invented.

use super::registration::HistoryScopeDefinition;
use super::restore::HistoryRestoreSchema;

/// Whether a resource type can show its past as text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextRendering {
    /// The type declared a read adapter that renders its record as text, so
    /// History can show a version and diff two as strings.
    Text,
    /// The type reads its record but not as text (a collection of records,
    /// a property bag): its versions are listed and compared by content
    /// address, not as lines.
    Structured,
    /// The type has no read adapter: it has nothing to show as text.
    None,
}

impl TextRendering {
    /// The rendering a type's own declaration implies: `reads_content` says
    /// it declared a read adapter (`Resource::READABLE`), `renders_text` that
    /// its record shows as text (`Resource::HISTORY_READS_TEXT`).
    #[must_use]
    pub fn from_declaration(reads_content: bool, renders_text: bool) -> Self {
        match (reads_content, renders_text) {
            (true, true) => Self::Text,
            (true, false) => Self::Structured,
            (false, _) => Self::None,
        }
    }

    /// Whether two versions of the type can be diffed as text.
    #[must_use]
    pub fn diffs_as_text(self) -> bool {
        matches!(self, Self::Text)
    }
}

/// What History can do with one resource type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HistoricalSupport {
    /// Every version can be shown, compared and restored by content: the
    /// type declared a restore schema.
    Content { text: TextRendering },
    /// A version lists with its metadata and the exact bytes it holds, and a
    /// restore brings those bytes back byte for byte. There is no textual
    /// diff of it, and none is invented.
    MetadataOnly { text: TextRendering },
    /// The type declares no content it could restore or show: its versions
    /// are named by address only.
    Unsupported { text: TextRendering },
}

impl HistoricalSupport {
    /// What the scope's own declaration says its resource type supports.
    #[must_use]
    pub fn of(scope: &HistoryScopeDefinition, text: TextRendering) -> Self {
        if scope.content_schema.is_some() {
            Self::Content { text }
        } else if text == TextRendering::None {
            Self::Unsupported { text }
        } else {
            Self::MetadataOnly { text }
        }
    }

    /// Whether the scope's declaration carries a restore schema: a `Some`
    /// even when that schema cannot name what replaces what.
    #[must_use]
    pub fn declares_schema(self) -> bool {
        matches!(self, Self::Content { .. })
    }

    /// Whether a version of this type can be restored at all.
    #[must_use]
    pub fn restorable(self) -> bool {
        !matches!(self, Self::Unsupported { .. })
    }

    /// Whether two versions of this type are compared as text.
    #[must_use]
    pub fn diffs_as_text(self) -> bool {
        let text = match self {
            Self::Content { text } | Self::MetadataOnly { text } | Self::Unsupported { text } => {
                text
            }
        };
        text.diffs_as_text() && !matches!(self, Self::Unsupported { .. })
    }

    /// The text rendering this support was classified with.
    #[must_use]
    pub fn text(self) -> TextRendering {
        match self {
            Self::Content { text } | Self::MetadataOnly { text } | Self::Unsupported { text } => {
                text
            }
        }
    }
}

/// Why a version of a resource cannot be shown or restored, in the three
/// ways they differ for a person: the type cannot at all, this record holds
/// nothing to show, or the version was written by another schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoricalGap {
    /// The resource type declares no history support for this scope (an
    /// external effect, a file with no versions): nothing is wrong with the
    /// record, History is simply not offered.
    Unsupported(String),
    /// This particular version cannot be read: it is absent from the
    /// record's history, or its content is no longer where it says it is.
    Unavailable(String),
    /// The version exists but was written by a storage version this app
    /// cannot read, so showing it would be showing something else.
    SchemaIncompatible {
        stored: u32,
        expected: u32,
        reason: String,
    },
}

impl HistoricalGap {
    /// The gap a stored schema version implies, or `None` when the app reads
    /// it.
    #[must_use]
    pub fn of_version(stored: u32, expected: u32) -> Option<Self> {
        match stored.cmp(&expected) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some(Self::SchemaIncompatible {
                stored,
                expected,
                reason: format!(
                    "written by a newer version of the app (storage version {stored}, this app reads {expected})"
                ),
            }),
            std::cmp::Ordering::Less => Some(Self::SchemaIncompatible {
                stored,
                expected,
                reason: format!(
                    "stored at version {stored}; it is brought up to version {expected} when it opens"
                ),
            }),
        }
    }

    /// That this version is not in the record.
    #[must_use]
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable(reason.into())
    }

    /// That this scope offers no history for its resource type.
    #[must_use]
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self::Unsupported(reason.into())
    }

    /// The reason to show a person, one name per gap.
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Unsupported(reason)
            | Self::Unavailable(reason)
            | Self::SchemaIncompatible { reason, .. } => reason,
        }
    }
}

impl std::fmt::Display for HistoricalGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())
    }
}

/// What a version of a binary resource says about itself: enough to name it
/// and to fetch exactly its bytes, nothing that pretends to be its content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BinaryVersion<'a> {
    /// The BLAKE3 hash of the bytes this version holds.
    pub hash: &'a [u8; 32],
    /// How many bytes it holds.
    pub len: u64,
}

impl BinaryVersion<'_> {
    /// The content address a person or a script fetches the exact bytes by.
    #[must_use]
    pub fn address(&self) -> String {
        format!(
            "blake3:{}:{}",
            blake3::Hash::from_bytes(*self.hash).to_hex(),
            self.len
        )
    }
}

/// What the common historical surface of a resource type answers, whatever
/// the app: the check run when a restore schema is registered, and the
/// per-type classification the History panel and the read door read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeHistory {
    pub kind: &'static str,
    pub version: u32,
    pub support: HistoricalSupport,
}

impl TypeHistory {
    /// Classify a resource type by what it declared. `kind` and `version`
    /// are the type's own; the read declarations say whether it renders as
    /// text.
    #[must_use]
    pub fn of(
        kind: &'static str,
        version: u32,
        scope: &HistoryScopeDefinition,
        text: TextRendering,
    ) -> Self {
        Self {
            kind,
            version,
            support: HistoricalSupport::of(scope, text),
        }
    }

    /// Why the registered restore schema cannot be used on this type, if it
    /// cannot: a schema that names no selector, or that would restore the
    /// record's own bookkeeping.
    ///
    /// # Errors
    /// Returns the failure the registration refuses with.
    pub fn check_schema(schema: &'static HistoryRestoreSchema) -> Result<(), String> {
        super::restore::check_schema(schema).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
#[path = "coverage_tests.rs"]
mod tests;
