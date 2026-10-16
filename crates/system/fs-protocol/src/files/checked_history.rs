//! Mount-scoped checked history protocol. These types are deliberately distinct
//! from V1's unchecked history-control operation; callers must not downgrade.
use super::{ContentRevisionV1, FileRecordId, FileVersionId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CHECKED_HISTORY_VERSION: u16 = 1;
pub const MAX_CHECKED_HISTORY_REQUEST_BYTES: usize = 1024;
pub const CHECKED_HISTORY_REVIEW_XATTR: &str = "user.tilo.history-review";
pub const CHECKED_HISTORY_RESTORE_XATTR: &str = "user.tilo.history-restore";
pub const CHECKED_HISTORY_RESULT_XATTR_PREFIX: &str = "user.tilo.history-result.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRestoreReviewV1 {
    pub schema_version: u16,
    pub workspace_id: Uuid,
    pub mount_lifetime: Uuid,
    pub record_id: FileRecordId,
    /// Freshness only: never an authorization capability.
    pub frontier: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "operation", deny_unknown_fields)]
pub enum CheckedHistoryOperationV1 {
    Restore { version_id: FileVersionId },
    Undo { restore_intent: Uuid },
    Redo { undo_intent: Uuid },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckedHistoryRequestV1 {
    pub schema_version: u16,
    /// Retain this exact ID and request across uncertain acknowledgement.
    pub intent_id: Uuid,
    pub review: FileRestoreReviewV1,
    pub operation: CheckedHistoryOperationV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRestoreReceiptV1 {
    pub schema_version: u16,
    pub intent_id: Uuid,
    pub workspace_id: Uuid,
    pub mount_lifetime: Uuid,
    pub record_id: FileRecordId,
    pub selected_version: FileVersionId,
    pub prior_revision: ContentRevisionV1,
    pub restored_revision: ContentRevisionV1,
    pub compensates: Option<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome", deny_unknown_fields)]
#[allow(
    clippy::large_enum_variant,
    reason = "moved verbatim from collab-types, whose crate-level allow covered it; boxing the receipt is a public API change outside this move"
)]
pub enum CheckedHistoryOutcomeV1 {
    Applied {
        receipt: FileRestoreReceiptV1,
    },
    Unchanged {
        intent_id: Uuid,
        review: FileRestoreReviewV1,
    },
    RecoveryPending {
        intent_id: Uuid,
    },
    ReviewRequired,
    NotVisible,
    ReadOnly,
    PendingWrites,
    UnsupportedOwner {
        mapping: String,
    },
    Unavailable,
    InvalidRequest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid or unsupported checked history request")]
pub struct CheckedHistoryProtocolError;

impl CheckedHistoryRequestV1 {
    #[cfg(test)]
    fn parse(bytes: &[u8]) -> Result<Self, CheckedHistoryProtocolError> {
        if bytes.len() > MAX_CHECKED_HISTORY_REQUEST_BYTES {
            return Err(CheckedHistoryProtocolError);
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| CheckedHistoryProtocolError)?;
        value.validate()?;
        Ok(value)
    }

    /// Validate the protocol version and required request identifiers.
    ///
    /// # Errors
    /// Returns an error for an unsupported version, a nil intent or operation
    /// identifier, or an invalid restore review.
    pub fn validate(&self) -> Result<(), CheckedHistoryProtocolError> {
        if self.schema_version != CHECKED_HISTORY_VERSION || self.intent_id.is_nil() {
            return Err(CheckedHistoryProtocolError);
        }
        self.review.validate()?;
        let id = match self.operation {
            CheckedHistoryOperationV1::Restore { version_id } => version_id.0,
            CheckedHistoryOperationV1::Undo { restore_intent } => restore_intent,
            CheckedHistoryOperationV1::Redo { undo_intent } => undo_intent,
        };
        if id.is_nil() {
            return Err(CheckedHistoryProtocolError);
        }
        Ok(())
    }
}

impl FileRestoreReviewV1 {
    /// Validate the protocol version and the reviewed file's identity.
    ///
    /// # Errors
    /// Returns an error for an unsupported version or a nil workspace, mount
    /// lifetime, or record identifier.
    pub fn validate(&self) -> Result<(), CheckedHistoryProtocolError> {
        if self.schema_version != CHECKED_HISTORY_VERSION
            || self.workspace_id.is_nil()
            || self.mount_lifetime.is_nil()
            || self.record_id.0.is_nil()
        {
            return Err(CheckedHistoryProtocolError);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> CheckedHistoryRequestV1 {
        CheckedHistoryRequestV1 {
            schema_version: 1,
            intent_id: Uuid::new_v4(),
            review: FileRestoreReviewV1 {
                schema_version: 1,
                workspace_id: Uuid::new_v4(),
                mount_lifetime: Uuid::new_v4(),
                record_id: FileRecordId::new(),
                frontier: [7; 32],
            },
            operation: CheckedHistoryOperationV1::Restore {
                version_id: FileVersionId::new(),
            },
        }
    }
    /// An exact request round-trips; wrong version, nil scope and unchecked requests are rejected.
    #[test]
    fn checked_history_requests_round_trip_exactly_and_reject_malformed_ones() {
        // Phase: checked_history_exact_request_round_trip.
        {
            let request = request();
            assert_eq!(
                CheckedHistoryRequestV1::parse(&serde_json::to_vec(&request).unwrap()).unwrap(),
                request
            );
        }

        // Phase: checked_history_rejects_wrong_version_nil_scope_and_unchecked_request.
        {
            let mut value = request();
            value.review.mount_lifetime = Uuid::nil();
            assert!(CheckedHistoryRequestV1::parse(&serde_json::to_vec(&value).unwrap()).is_err());
            value = request();
            value.schema_version = 2;
            assert!(CheckedHistoryRequestV1::parse(&serde_json::to_vec(&value).unwrap()).is_err());
            assert!(CheckedHistoryRequestV1::parse(br#"{"schema_version":1,"operation":{"operation":"restore","version_id":"00000000-0000-0000-0000-000000000001"}}"#).is_err());
            assert!(CheckedHistoryRequestV1::parse(&vec![b' '; 1025]).is_err());
        }
    }
}

/// Read-only review reply; a failure never embeds a usable review token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckedHistoryReviewResponseV1 {
    pub schema_version: u16,
    pub result: Result<FileRestoreReviewV1, CheckedHistoryOutcomeV1>,
}

/// Bounded result fetched by exact intent ID after a history-restore write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckedHistoryResponseV1 {
    pub schema_version: u16,
    pub outcome: CheckedHistoryOutcomeV1,
}

/// Progress and cancellation of the hydration behind a history restore or a
/// conflict resolution, by the request's id: the restore's `intent_id`, or
/// the `conflict_id` being resolved.
///
/// A restore or resolution whose content is not yet on this device answers
/// its write with `EINPROGRESS` instead of holding the caller while the
/// bytes download. Reading `<prefix><id>` on the same file returns a
/// [`HistoryJobResponseV1`]; writing [`HISTORY_JOB_CANCEL`] to it stops the
/// download. Once the job is ready, the caller repeats the exact request,
/// which commits from local content.
pub const HISTORY_JOB_XATTR_PREFIX: &str = "user.tilo.history-job.";
/// The value written to a history-job attribute to cancel it.
pub const HISTORY_JOB_CANCEL: &[u8] = b"cancel";

/// Where the hydration behind one history request stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", deny_unknown_fields)]
pub enum HistoryJobStateV1 {
    /// `done` of `total` verified bytes are local. `total` is 0 until every
    /// manifest involved is known.
    Hydrating { done: u64, total: u64 },
    /// Every byte is local: repeat the request to commit it.
    Ready,
    /// A peer could not supply the content. Repeating the request retries.
    Failed,
    /// The caller cancelled the download. Repeating the request starts over.
    Cancelled,
}

/// Bounded reply read from a history-job attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryJobResponseV1 {
    pub schema_version: u16,
    pub state: HistoryJobStateV1,
}
