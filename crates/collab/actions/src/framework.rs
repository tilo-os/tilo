//! Changes the framework makes to a record, which no app action produces.
//!
//! Every change a device signs names what it does: an app's verb or field
//! edit names itself in its history entry ([`crate::ActionContext`]), and a
//! change the framework writes on its own names one of these in its commit
//! message, so a receiver can decide it against the author's grants like
//! any other.

use serde::{Deserialize, Serialize};

/// Prefix of a framework change's commit message.
pub const FRAMEWORK_MESSAGE_PREFIX: &str = "tilo.framework/1 ";

/// A change the framework writes rather than an app's action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FrameworkChange {
    /// What a brand-new record starts with: its genesis and first content.
    Genesis,
    /// A storage migration to the model's current version. Written with a
    /// history entry ([`crate::HistoryOp::Migrate`]), not this message.
    Migrate,
    /// An empty container or the initial text every member derives the
    /// same way, made the first time the record needs it.
    Structure,
    /// The record leaving its room for another, or arriving in one.
    Transfer,
    /// A window's direct edit of the record that names no finer action
    /// (widget input outside a bound action). It is decided as an edit of
    /// the whole record.
    Native,
}

impl FrameworkChange {
    /// The commit message naming this change.
    #[must_use]
    pub fn change_message(self) -> String {
        let name = match self {
            Self::Genesis => "genesis",
            Self::Migrate => "migrate",
            Self::Structure => "structure",
            Self::Transfer => "transfer",
            Self::Native => "native",
        };
        format!("{FRAMEWORK_MESSAGE_PREFIX}{name}")
    }

    /// The framework change a commit message names, if it names one.
    #[must_use]
    pub fn from_change_message(message: &str) -> Option<Self> {
        Some(match message.strip_prefix(FRAMEWORK_MESSAGE_PREFIX)? {
            "genesis" => Self::Genesis,
            "migrate" => Self::Migrate,
            "structure" => Self::Structure,
            "transfer" => Self::Transfer,
            "native" => Self::Native,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_framework_change_reads_back_from_its_message() {
        for change in [
            FrameworkChange::Genesis,
            FrameworkChange::Migrate,
            FrameworkChange::Structure,
            FrameworkChange::Transfer,
            FrameworkChange::Native,
        ] {
            assert_eq!(
                FrameworkChange::from_change_message(&change.change_message()),
                Some(change)
            );
        }
        assert_eq!(
            FrameworkChange::from_change_message("tilo.entry/1 {}"),
            None
        );
        assert_eq!(
            FrameworkChange::from_change_message("tilo.framework/1 other"),
            None
        );
    }
}
