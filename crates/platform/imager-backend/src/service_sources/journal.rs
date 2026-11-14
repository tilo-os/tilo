//! A service migration's journal: the review the person approved and every item its app
//! already has. The migration store keeps it durably beside the folder copies' operations
//! ([`crate::migration_store::MigrationStore::save_service`]).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{ServiceReport, ServiceReview};

const VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalState {
    /// Reviewed; the person has not started it.
    AwaitingReview,
    Running,
    /// Stopped by the person or by the process ending: starting it again resumes.
    Interrupted,
    Failed,
    Complete,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceJournal {
    pub version: u32,
    /// The operation's id: letters, digits, `-` and `_`.
    pub id: String,
    pub review: ServiceReview,
    pub state: JournalState,
    /// The items whose app has them.
    pub landed: BTreeSet<String>,
    pub failure: Option<String>,
    pub report: Option<ServiceReport>,
}

impl ServiceJournal {
    /// A journal for `review`, waiting for the person to start it. Its id names the source, so
    /// reviewing the same source again continues the same journal.
    #[must_use]
    pub fn new(review: ServiceReview) -> Self {
        let id = format!(
            "{}-{}",
            review.kind.key(),
            super::short_digest(&review.account)
        );
        Self {
            version: VERSION,
            id,
            review,
            state: JournalState::AwaitingReview,
            landed: BTreeSet::new(),
            failure: None,
            report: None,
        }
    }

    /// A fresh review of the same source: its choices and items replace the old review's, and
    /// what already landed stays landed.
    pub fn reviewed_again(&mut self, review: ServiceReview) {
        self.review = review;
        self.state = JournalState::AwaitingReview;
        self.failure = None;
    }

    /// The journal a process left running is interrupted: starting it again resumes.
    pub fn recovered(&mut self) {
        if self.state == JournalState::Running {
            self.state = JournalState::Interrupted;
        }
    }

    /// # Errors
    /// The bytes are not a journal this version reads.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let journal: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if journal.version != VERSION || !valid_id(&journal.id) {
            return Err("the service journal is not one this version reads".into());
        }
        Ok(journal)
    }

    /// # Errors
    /// The journal cannot be encoded.
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|error| error.to_string())
    }
}

/// A journal id names a file of the migration store: letters, digits, `-` and `_`.
#[must_use]
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
