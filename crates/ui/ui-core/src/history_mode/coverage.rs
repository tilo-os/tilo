//! What History can honestly do with a resource type: one classification the panel, the read API and
//! the restore review all read, so "no past version here" is never confused with "this text does not
//! read", "written by a newer app" or "this type keeps no content history". The classification is
//! derived from what the resource type itself declared, never guessed per app: whether the type has a
//! read adapter.

/// What History can do with one resource type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HistoricalSupport {
    /// Every version can be shown, compared and restored: the type reads its record, and a restore is
    /// checked against that read by the store's history verbs.
    Content,
    /// The type declares no content it could restore or show: its versions are named by address only.
    Unsupported,
}

impl HistoricalSupport {
    /// What the type supports; `reads_content` says it reads its stored content: a read adapter
    /// (`ResourceSpec::READABLE`) or a record read field by field.
    #[must_use]
    pub fn of(reads_content: bool) -> Self {
        if reads_content {
            Self::Content
        } else {
            Self::Unsupported
        }
    }

    /// Whether a version of this type can be restored at all.
    #[must_use]
    pub fn restorable(self) -> bool {
        !matches!(self, Self::Unsupported)
    }
}

/// Why a version of a resource cannot be shown or restored, in the three ways they differ for a person:
/// the type cannot at all, this record holds nothing to show, or the version was written by another
/// schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoricalGap {
    /// The resource type declares no history support for this scope (an external effect, a file with no
    /// versions): nothing is wrong with the record, History is simply not offered.
    Unsupported(String),
    /// This particular version cannot be read: it is absent from the record's history, or its content
    /// is no longer where it says it is.
    Unavailable(String),
    /// The version exists but was written by a storage version this app cannot read, so showing it
    /// would be showing something else.
    SchemaIncompatible {
        stored: u32,
        expected: u32,
        reason: String,
    },
}

impl HistoricalGap {
    /// The gap a stored schema version implies, or `None` when the app reads it.
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

#[cfg(test)]
#[path = "coverage_tests.rs"]
mod tests;
