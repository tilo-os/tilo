//! What History can honestly do with a resource type: one classification the
//! panel, the read door and the restore review all read, so "no past version
//! here" is never confused with "this text does not read", "written by a
//! newer app" or "this type keeps no content history".
//!
//! The classification is derived from what the resource type itself
//! declared, never guessed per app: the schema history restores content
//! with, and whether the type has a read adapter. A version of a type that
//! reads its record is answered by that adapter, exactly as the live record
//! is; one with neither a restore schema nor a read adapter has nothing of
//! its content to show.

use super::registration::HistoryScopeDefinition;
use super::restore::HistoryRestoreSchema;

/// What History can do with one resource type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HistoricalSupport {
    /// Every version can be shown, compared and restored by content: the
    /// type declared a restore schema.
    Content,
    /// The type reads its record but declares no restore schema: a version
    /// reads through the type's own adapter and lists with its metadata, and
    /// a restore brings what it held back byte for byte.
    MetadataOnly,
    /// The type declares no content it could restore or show: its versions
    /// are named by address only.
    Unsupported,
}

impl HistoricalSupport {
    /// What the scope's own declaration says its resource type supports;
    /// `reads_content` says the type declared a read adapter
    /// (`ResourceSpec::READABLE`).
    #[must_use]
    pub fn of(scope: &HistoryScopeDefinition, reads_content: bool) -> Self {
        if scope.content_schema.is_some() {
            Self::Content
        } else if reads_content {
            Self::MetadataOnly
        } else {
            Self::Unsupported
        }
    }

    /// Whether the scope's declaration carries a restore schema: a `Some`
    /// even when that schema cannot name what replaces what.
    #[must_use]
    pub fn declares_schema(self) -> bool {
        matches!(self, Self::Content)
    }

    /// Whether a version of this type can be restored at all.
    #[must_use]
    pub fn restorable(self) -> bool {
        !matches!(self, Self::Unsupported)
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
    /// are the type's own; `reads_content` says it declared a read adapter.
    #[must_use]
    pub fn of(
        kind: &'static str,
        version: u32,
        scope: &HistoryScopeDefinition,
        reads_content: bool,
    ) -> Self {
        Self {
            kind,
            version,
            support: HistoricalSupport::of(scope, reads_content),
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
