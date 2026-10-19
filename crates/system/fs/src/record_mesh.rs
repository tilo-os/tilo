//! Record-stream identities and cursors as the session protocol carries
//! them (`sync::session`), and the error codes it answers a refused request
//! with.

use fs_protocol::{FileActor, FileRecordId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::record_repository::RecordStreamCursor;

/// Stable identity of one record actor stream, also a bound of a
/// reconciliation range (`record_reconcile`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordMeshStreamV1 {
    pub record_id: FileRecordId,
    pub actor: FileActor,
}

impl From<&RecordStreamCursor> for RecordMeshStreamV1 {
    fn from(cursor: &RecordStreamCursor) -> Self {
        Self {
            record_id: cursor.record_id,
            actor: cursor.actor,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum RecordMeshErrorCodeV1 {
    InvalidRequest,
    UnsupportedVersion,
    RequestTooLarge,
    ResponseTooLarge,
    PrefixMismatch,
    Internal,
}

// RecordStreamCursor intentionally lives beside repository semantics. Its wire
// representation is defined here so the repository remains transport-agnostic.
impl Serialize for RecordStreamCursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct WireCursor {
            record_id: FileRecordId,
            actor: FileActor,
            contiguous_sequence: u64,
            prefix_digest: [u8; 32],
        }
        WireCursor {
            record_id: self.record_id,
            actor: self.actor,
            contiguous_sequence: self.contiguous_sequence,
            prefix_digest: self.prefix_digest,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RecordStreamCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireCursor {
            record_id: FileRecordId,
            actor: FileActor,
            contiguous_sequence: u64,
            prefix_digest: [u8; 32],
        }
        let wire = WireCursor::deserialize(deserializer)?;
        Ok(Self {
            record_id: wire.record_id,
            actor: wire.actor,
            contiguous_sequence: wire.contiguous_sequence,
            prefix_digest: wire.prefix_digest,
        })
    }
}
