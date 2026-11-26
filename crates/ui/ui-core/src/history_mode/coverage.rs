//! What History can honestly do with a resource type: one classification the panel, the read API and
//! the restore review all read, so "no past version here" is never confused with "this text does not
//! read" or "this type keeps no content history". The classification is
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

/// Why a version of a resource cannot be shown or restored, in the two ways they differ for a
/// person: the type cannot show its history at all, or this record holds nothing to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoricalGap {
    /// The resource type declares no history support for this scope (an external effect, a file with no
    /// versions): nothing is wrong with the record, History is simply not offered.
    Unsupported(String),
    /// This particular version cannot be read: it is absent from the record's history, its content
    /// is no longer where it says it is, or it is written at a storage version this build cannot
    /// bring up.
    Unavailable(String),
}

impl HistoricalGap {
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
            Self::Unsupported(reason) | Self::Unavailable(reason) => reason,
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
