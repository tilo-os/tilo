//! Wire contract for workspace-wide signed-record anti-entropy.
//!
//! This module deliberately contains only bounded DTO validation and framing.
//! Authentication, workspace-root authorization, repository reads, and Iroh
//! handler registration belong to the transport integration layer.

use std::io;

use fs_protocol::{FileActor, FileRecordId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;
use wire_frame::Framing;

use crate::canonical_frame::{self, CanonicalFrameError};
use crate::record_replication::RecordEnvelopeV1;
use crate::record_repository::{
    RecordRepository, RecordRepositoryKey, RecordStreamCursor, RecordStreamKey,
};

pub const TILO_FS_RECORD_MESH_ALPN: &[u8] = b"tilo-fs-record-mesh/2";
pub const RECORD_MESH_PROTOCOL_VERSION: u16 = 2;
pub const MAX_RECORD_MESH_FRAME_BYTES: usize = 256 * 1024;
pub const MAX_RECORD_MESH_INVENTORY_PAGE_STREAMS: usize = 256;
pub const MAX_RECORD_MESH_PULL_BATCH_ENVELOPES: usize = 256;

/// Stable identity of one record actor stream, also used as the exclusive
/// continuation key for inventory pagination.
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

/// A request always addresses exactly one non-nil workspace. The peer's
/// authenticated workspace-root capability must be checked separately.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordMeshRequestV1 {
    pub protocol_version: u16,
    pub workspace_id: Uuid,
    pub operation: RecordMeshOperationV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum RecordMeshOperationV1 {
    /// Return streams strictly after `after`, in canonical stream-key order.
    Inventory {
        after: Option<RecordMeshStreamV1>,
        limit: u16,
    },
    /// Return the next contiguous suffix of one stream. `after = None` means
    /// sequence zero; otherwise the cursor commits to the caller's prefix.
    Pull {
        stream: RecordMeshStreamV1,
        after: Option<RecordStreamCursor>,
        limit: u16,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum RecordMeshEndV1 {
    Inventory,
    Pull { stream: RecordMeshStreamV1 },
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

/// Finite, payload-free failures avoid exposing authorization or backend
/// details. Empty successful pages/batches are represented only by `End`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordMeshResponseV1 {
    pub protocol_version: u16,
    pub result: RecordMeshResultV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum RecordMeshResultV1 {
    InventoryPage {
        workspace_id: Uuid,
        cursors: Vec<RecordStreamCursor>,
        /// Exclusive continuation key. It must equal the final page entry.
        next: RecordMeshStreamV1,
    },
    EnvelopeBatch {
        workspace_id: Uuid,
        stream: RecordMeshStreamV1,
        /// Cursor supplied by the request that produced this suffix.
        after_sequence: u64,
        envelopes: Vec<RecordEnvelopeV1>,
    },
    End {
        workspace_id: Uuid,
        operation: RecordMeshEndV1,
    },
    Denied,
    Error(RecordMeshErrorCodeV1),
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RecordMeshValidationError {
    #[error("protocol version is unsupported")]
    UnsupportedVersion,
    #[error("workspace id must not be nil")]
    NilWorkspace,
    #[error("page or batch limit is outside the protocol bounds")]
    InvalidLimit,
    #[error("pull cursor does not identify the requested stream")]
    CrossStreamCursor,
    #[error("an inventory page must be non-empty and bounded")]
    InvalidInventoryPage,
    #[error("inventory cursors are not strictly ordered")]
    InventoryNotOrdered,
    #[error("inventory continuation does not match the final cursor")]
    InvalidInventoryContinuation,
    #[error("an envelope batch must be non-empty and bounded")]
    InvalidEnvelopeBatch,
    #[error("an envelope does not belong to the requested stream")]
    CrossStreamEnvelope,
    #[error("envelope batch is not a contiguous suffix")]
    NonContiguousEnvelopeBatch,
}

impl RecordMeshRequestV1 {
    /// Validate protocol version, scope, limits, and cursors.
    ///
    /// # Errors
    ///
    /// Returns an error when any request invariant is invalid.
    pub fn validate(&self) -> Result<(), RecordMeshValidationError> {
        if self.protocol_version != RECORD_MESH_PROTOCOL_VERSION {
            return Err(RecordMeshValidationError::UnsupportedVersion);
        }
        validate_workspace(self.workspace_id)?;
        match &self.operation {
            RecordMeshOperationV1::Inventory { limit, .. } => {
                validate_limit(*limit, MAX_RECORD_MESH_INVENTORY_PAGE_STREAMS)
            }
            RecordMeshOperationV1::Pull {
                stream,
                after,
                limit,
            } => {
                validate_limit(*limit, MAX_RECORD_MESH_PULL_BATCH_ENVELOPES)?;
                if after.as_ref().is_some_and(|cursor| {
                    cursor.record_id != stream.record_id || cursor.actor != stream.actor
                }) {
                    return Err(RecordMeshValidationError::CrossStreamCursor);
                }
                Ok(())
            }
        }
    }
}

impl RecordMeshResponseV1 {
    /// Validate protocol version, ordering, bounds, and stream continuity.
    ///
    /// # Errors
    ///
    /// Returns an error when any response invariant is invalid.
    pub fn validate(&self) -> Result<(), RecordMeshValidationError> {
        if self.protocol_version != RECORD_MESH_PROTOCOL_VERSION {
            return Err(RecordMeshValidationError::UnsupportedVersion);
        }
        match &self.result {
            RecordMeshResultV1::InventoryPage {
                workspace_id,
                cursors,
                next,
            } => {
                validate_workspace(*workspace_id)?;
                if cursors.is_empty() || cursors.len() > MAX_RECORD_MESH_INVENTORY_PAGE_STREAMS {
                    return Err(RecordMeshValidationError::InvalidInventoryPage);
                }
                if !cursors.windows(2).all(|pair| {
                    let [left, right] = pair else {
                        return false;
                    };
                    compare_streams(
                        &RecordMeshStreamV1::from(left),
                        &RecordMeshStreamV1::from(right),
                    )
                    .is_lt()
                }) {
                    return Err(RecordMeshValidationError::InventoryNotOrdered);
                }
                let Some(last) = cursors.last() else {
                    return Err(RecordMeshValidationError::InvalidInventoryPage);
                };
                if RecordMeshStreamV1::from(last) != *next {
                    return Err(RecordMeshValidationError::InvalidInventoryContinuation);
                }
                Ok(())
            }
            RecordMeshResultV1::EnvelopeBatch {
                workspace_id,
                stream,
                after_sequence,
                envelopes,
            } => {
                validate_workspace(*workspace_id)?;
                if envelopes.is_empty() || envelopes.len() > MAX_RECORD_MESH_PULL_BATCH_ENVELOPES {
                    return Err(RecordMeshValidationError::InvalidEnvelopeBatch);
                }
                let mut expected = after_sequence
                    .checked_add(1)
                    .ok_or(RecordMeshValidationError::NonContiguousEnvelopeBatch)?;
                for envelope in envelopes {
                    if envelope.record_id != stream.record_id || envelope.actor != stream.actor {
                        return Err(RecordMeshValidationError::CrossStreamEnvelope);
                    }
                    if envelope.sequence != expected {
                        return Err(RecordMeshValidationError::NonContiguousEnvelopeBatch);
                    }
                    expected = expected
                        .checked_add(1)
                        .ok_or(RecordMeshValidationError::NonContiguousEnvelopeBatch)?;
                }
                Ok(())
            }
            RecordMeshResultV1::End { workspace_id, .. } => validate_workspace(*workspace_id),
            RecordMeshResultV1::Denied | RecordMeshResultV1::Error(_) => Ok(()),
        }
    }
}

#[derive(Debug, Error)]
pub enum RecordMeshFrameError {
    #[error("record mesh transport failed: {0}")]
    Io(#[from] io::Error),
    #[error("record mesh frame announced {announced} bytes, maximum is {maximum}")]
    TooLarge { announced: usize, maximum: usize },
    #[error("record mesh frame is not canonical postcard")]
    NonCanonical,
    #[error("record mesh message is invalid: {0}")]
    Invalid(#[from] RecordMeshValidationError),
}

/// Write one validated, bounded mesh request.
///
/// # Errors
///
/// Returns an error on validation, encoding, bounds, or I/O failure.
pub async fn write_record_mesh_request<W: AsyncWrite + Unpin>(
    writer: &mut W,
    request: &RecordMeshRequestV1,
) -> Result<(), RecordMeshFrameError> {
    request.validate()?;
    write_frame(writer, request).await
}

/// Read and validate one bounded mesh request.
///
/// # Errors
///
/// Returns an error on I/O, bounds, decoding, or validation failure.
pub async fn read_record_mesh_request<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<RecordMeshRequestV1, RecordMeshFrameError> {
    let request: RecordMeshRequestV1 = read_frame(reader).await?;
    request.validate()?;
    Ok(request)
}

/// Write one validated, bounded mesh response.
///
/// # Errors
///
/// Returns an error on validation, encoding, bounds, or I/O failure.
pub async fn write_record_mesh_response<W: AsyncWrite + Unpin>(
    writer: &mut W,
    response: &RecordMeshResponseV1,
) -> Result<(), RecordMeshFrameError> {
    response.validate()?;
    write_frame(writer, response).await
}

/// Read and validate one bounded mesh response.
///
/// # Errors
///
/// Returns an error on I/O, bounds, decoding, or validation failure.
pub async fn read_record_mesh_response<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<RecordMeshResponseV1, RecordMeshFrameError> {
    let response: RecordMeshResponseV1 = read_frame(reader).await?;
    response.validate()?;
    Ok(response)
}

/// Build one finite response after the transport layer has mutually
/// authenticated the peer and authorized workspace-root read access.
/// Authorization intentionally is not inferred from possession of a cursor.
pub fn serve_authorized_request(
    repository: &RecordRepository,
    workspace_id: Uuid,
    request: &RecordMeshRequestV1,
) -> RecordMeshResponseV1 {
    let result = if request.validate().is_err() || request.workspace_id != workspace_id {
        RecordMeshResultV1::Error(RecordMeshErrorCodeV1::InvalidRequest)
    } else {
        match &request.operation {
            RecordMeshOperationV1::Inventory { after, limit } => {
                let after = after.map(|stream| RecordStreamKey {
                    record_id: stream.record_id,
                    person: *stream.actor.person.as_bytes(),
                    device: *stream.actor.device.as_bytes(),
                });
                match repository.inventory_page(workspace_id, after, usize::from(*limit)) {
                    Ok(cursors) if cursors.is_empty() => RecordMeshResultV1::End {
                        workspace_id,
                        operation: RecordMeshEndV1::Inventory,
                    },
                    Ok(cursors) => {
                        if let Some(last) = cursors.last() {
                            let next = RecordMeshStreamV1::from(last);
                            RecordMeshResultV1::InventoryPage {
                                workspace_id,
                                cursors,
                                next,
                            }
                        } else {
                            RecordMeshResultV1::End {
                                workspace_id,
                                operation: RecordMeshEndV1::Inventory,
                            }
                        }
                    }
                    Err(_) => RecordMeshResultV1::Error(RecordMeshErrorCodeV1::Internal),
                }
            }
            RecordMeshOperationV1::Pull {
                stream,
                after,
                limit,
            } => {
                let key = RecordRepositoryKey {
                    workspace_id,
                    record_id: stream.record_id,
                };
                let after_sequence = after
                    .as_ref()
                    .map_or(0, |cursor| cursor.contiguous_sequence);
                if let Some(cursor) = after {
                    match repository.stream_cursor(key, &stream.actor, cursor.contiguous_sequence) {
                        Ok(local) if local == *cursor => {}
                        Ok(_) => {
                            return RecordMeshResponseV1 {
                                protocol_version: RECORD_MESH_PROTOCOL_VERSION,
                                result: RecordMeshResultV1::Error(
                                    RecordMeshErrorCodeV1::PrefixMismatch,
                                ),
                            };
                        }
                        Err(_) => {
                            return RecordMeshResponseV1 {
                                protocol_version: RECORD_MESH_PROTOCOL_VERSION,
                                result: RecordMeshResultV1::Error(
                                    RecordMeshErrorCodeV1::InvalidRequest,
                                ),
                            };
                        }
                    }
                }
                match repository.envelopes_after(
                    key,
                    &stream.actor,
                    after_sequence,
                    usize::from(*limit),
                ) {
                    Ok(envelopes) if envelopes.is_empty() => RecordMeshResultV1::End {
                        workspace_id,
                        operation: RecordMeshEndV1::Pull { stream: *stream },
                    },
                    Ok(envelopes) => RecordMeshResultV1::EnvelopeBatch {
                        workspace_id,
                        stream: *stream,
                        after_sequence,
                        envelopes,
                    },
                    Err(_) => RecordMeshResultV1::Error(RecordMeshErrorCodeV1::InvalidRequest),
                }
            }
        }
    };
    RecordMeshResponseV1 {
        protocol_version: RECORD_MESH_PROTOCOL_VERSION,
        result,
    }
}

/// Each mesh request and response: a big-endian `u32` length, then the
/// canonical encoding.
const RECORD_MESH_FRAMING: Framing = Framing::big_endian(MAX_RECORD_MESH_FRAME_BYTES);

impl From<CanonicalFrameError> for RecordMeshFrameError {
    fn from(error: CanonicalFrameError) -> Self {
        match error {
            CanonicalFrameError::Io(error) => Self::Io(error),
            CanonicalFrameError::TooLarge(announced) => Self::TooLarge {
                announced,
                maximum: MAX_RECORD_MESH_FRAME_BYTES,
            },
            CanonicalFrameError::Empty
            | CanonicalFrameError::Encode(_)
            | CanonicalFrameError::Decode(_) => Self::NonCanonical,
        }
    }
}

async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), RecordMeshFrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize + Sync,
{
    canonical_frame::write(RECORD_MESH_FRAMING, writer, value).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_frame<R, T>(reader: &mut R) -> Result<T, RecordMeshFrameError>
where
    R: AsyncRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    Ok(canonical_frame::read_required(RECORD_MESH_FRAMING, reader).await?)
}

fn validate_workspace(workspace_id: Uuid) -> Result<(), RecordMeshValidationError> {
    if workspace_id.is_nil() {
        Err(RecordMeshValidationError::NilWorkspace)
    } else {
        Ok(())
    }
}

fn validate_limit(limit: u16, maximum: usize) -> Result<(), RecordMeshValidationError> {
    if limit == 0 || usize::from(limit) > maximum {
        Err(RecordMeshValidationError::InvalidLimit)
    } else {
        Ok(())
    }
}

fn compare_streams(left: &RecordMeshStreamV1, right: &RecordMeshStreamV1) -> std::cmp::Ordering {
    left.record_id
        .cmp(&right.record_id)
        .then_with(|| {
            left.actor
                .person
                .as_bytes()
                .cmp(right.actor.person.as_bytes())
        })
        .then_with(|| {
            left.actor
                .device
                .as_bytes()
                .cmp(right.actor.device.as_bytes())
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::Ed25519PublicKey;
    use ed25519_dalek::SigningKey;
    use fs_protocol::{FileMetadataV1, IndexStatus};

    use crate::record_replication::RecordOperationV1;
    use crate::record_replication::test_authorizers::AllowActorOn;
    use crate::record_repository::RecordRepositoryKey;

    fn actor(seed: u8) -> FileActor {
        FileActor {
            person: Ed25519PublicKey::from_bytes([seed; 32]),
            device: Ed25519PublicKey::from_bytes([seed.wrapping_add(1); 32]),
        }
    }

    fn cursor(record_id: FileRecordId, actor: FileActor, sequence: u64) -> RecordStreamCursor {
        RecordStreamCursor {
            record_id,
            actor,
            contiguous_sequence: sequence,
            prefix_digest: [u8::try_from(sequence).unwrap(); 32],
        }
    }

    fn envelope(record_id: FileRecordId, actor: FileActor, sequence: u64) -> RecordEnvelopeV1 {
        let device = SigningKey::from_bytes(actor.device.as_bytes());
        let metadata = FileMetadataV1 {
            schema_version: 1,
            record_id,
            content_hash: None,
            byte_len: sequence,
            mime: None,
            previews: vec![],
            latest_actor: Some(actor),
            observed_at_ms: sequence,
            index_status: IndexStatus::Pending,
            properties: None,
        };
        RecordEnvelopeV1::signed(
            record_id,
            actor,
            sequence,
            RecordOperationV1::SetMetadata(metadata),
            &device,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn mesh_frames_round_trip_bounded_and_contiguous() {
        // Phase `request_round_trips_over_bounded_duplex_frame`.
        {
            let workspace_id = Uuid::new_v4();
            let record_id = FileRecordId::new();
            let actor = actor(7);
            let request = RecordMeshRequestV1 {
                protocol_version: RECORD_MESH_PROTOCOL_VERSION,
                workspace_id,
                operation: RecordMeshOperationV1::Pull {
                    stream: RecordMeshStreamV1 { record_id, actor },
                    after: Some(cursor(record_id, actor, 3)),
                    limit: 32,
                },
            };
            let (mut writer, mut reader) = tokio::io::duplex(32);
            let expected = request.clone();
            let write = tokio::spawn(async move {
                write_record_mesh_request(&mut writer, &request)
                    .await
                    .unwrap();
            });
            assert_eq!(
                read_record_mesh_request(&mut reader).await.unwrap(),
                expected
            );
            write.await.unwrap();
        }
        // Phase `response_round_trips_and_enforces_contiguous_stream`.
        {
            let workspace_id = Uuid::new_v4();
            let record_id = FileRecordId::new();
            let actor = actor(11);
            let response = RecordMeshResponseV1 {
                protocol_version: RECORD_MESH_PROTOCOL_VERSION,
                result: RecordMeshResultV1::EnvelopeBatch {
                    workspace_id,
                    stream: RecordMeshStreamV1 { record_id, actor },
                    after_sequence: 0,
                    envelopes: vec![envelope(record_id, actor, 1), envelope(record_id, actor, 2)],
                },
            };
            let (mut writer, mut reader) = tokio::io::duplex(64);
            let expected = response.clone();
            let write = tokio::spawn(async move {
                write_record_mesh_response(&mut writer, &response)
                    .await
                    .unwrap();
            });
            assert_eq!(
                read_record_mesh_response(&mut reader).await.unwrap(),
                expected
            );
            write.await.unwrap();
        }
        // Phase `oversized_announced_frame_is_rejected_before_payload_read`.
        {
            let (mut writer, mut reader) = tokio::io::duplex(8);
            let write = tokio::spawn(async move {
                writer
                    .write_all(
                        &u32::try_from(MAX_RECORD_MESH_FRAME_BYTES + 1)
                            .unwrap()
                            .to_be_bytes(),
                    )
                    .await
                    .unwrap();
            });
            assert!(matches!(
                read_record_mesh_request(&mut reader).await,
                Err(RecordMeshFrameError::TooLarge { .. })
            ));
            write.await.unwrap();
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn mesh_pages_and_batches_require_exact_order_scope_and_prefix() {
        // Phase `rejects_nil_workspace_zero_limits_and_cross_stream_cursors`.
        {
            let record_id = FileRecordId::new();
            let other_record = FileRecordId::new();
            let actor = actor(3);
            let nil = RecordMeshRequestV1 {
                protocol_version: RECORD_MESH_PROTOCOL_VERSION,
                workspace_id: Uuid::nil(),
                operation: RecordMeshOperationV1::Inventory {
                    after: None,
                    limit: 1,
                },
            };
            assert_eq!(nil.validate(), Err(RecordMeshValidationError::NilWorkspace));

            let zero = RecordMeshRequestV1 {
                protocol_version: RECORD_MESH_PROTOCOL_VERSION,
                workspace_id: Uuid::new_v4(),
                operation: RecordMeshOperationV1::Inventory {
                    after: None,
                    limit: 0,
                },
            };
            assert_eq!(
                zero.validate(),
                Err(RecordMeshValidationError::InvalidLimit)
            );

            let cross = RecordMeshRequestV1 {
                protocol_version: RECORD_MESH_PROTOCOL_VERSION,
                workspace_id: Uuid::new_v4(),
                operation: RecordMeshOperationV1::Pull {
                    stream: RecordMeshStreamV1 { record_id, actor },
                    after: Some(cursor(other_record, actor, 1)),
                    limit: 1,
                },
            };
            assert_eq!(
                cross.validate(),
                Err(RecordMeshValidationError::CrossStreamCursor)
            );
        }
        // Phase `inventory_page_requires_order_and_exact_continuation`.
        {
            let workspace_id = Uuid::new_v4();
            let first_id = FileRecordId(Uuid::from_u128(1));
            let second_id = FileRecordId(Uuid::from_u128(2));
            let actor = actor(1);
            let valid = RecordMeshResponseV1 {
                protocol_version: RECORD_MESH_PROTOCOL_VERSION,
                result: RecordMeshResultV1::InventoryPage {
                    workspace_id,
                    cursors: vec![cursor(first_id, actor, 1), cursor(second_id, actor, 2)],
                    next: RecordMeshStreamV1 {
                        record_id: second_id,
                        actor,
                    },
                },
            };
            assert_eq!(valid.validate(), Ok(()));

            let reversed = RecordMeshResponseV1 {
                protocol_version: RECORD_MESH_PROTOCOL_VERSION,
                result: RecordMeshResultV1::InventoryPage {
                    workspace_id,
                    cursors: vec![cursor(second_id, actor, 2), cursor(first_id, actor, 1)],
                    next: RecordMeshStreamV1 {
                        record_id: first_id,
                        actor,
                    },
                },
            };
            assert_eq!(
                reversed.validate(),
                Err(RecordMeshValidationError::InventoryNotOrdered)
            );
        }
        // Phase `batch_rejects_gaps_and_cross_stream_envelopes`.
        {
            let workspace_id = Uuid::new_v4();
            let record_id = FileRecordId::new();
            let other = FileRecordId::new();
            let actor = actor(22);
            let gap = RecordMeshResponseV1 {
                protocol_version: RECORD_MESH_PROTOCOL_VERSION,
                result: RecordMeshResultV1::EnvelopeBatch {
                    workspace_id,
                    stream: RecordMeshStreamV1 { record_id, actor },
                    after_sequence: 0,
                    envelopes: vec![envelope(record_id, actor, 2)],
                },
            };
            assert_eq!(
                gap.validate(),
                Err(RecordMeshValidationError::NonContiguousEnvelopeBatch)
            );

            let cross = RecordMeshResponseV1 {
                protocol_version: RECORD_MESH_PROTOCOL_VERSION,
                result: RecordMeshResultV1::EnvelopeBatch {
                    workspace_id,
                    stream: RecordMeshStreamV1 { record_id, actor },
                    after_sequence: 0,
                    envelopes: vec![envelope(other, actor, 1)],
                },
            };
            assert_eq!(
                cross.validate(),
                Err(RecordMeshValidationError::CrossStreamEnvelope)
            );
        }
        // Phase `authorized_repository_response_pages_and_checks_prefix_digest`.
        {
            let dir = tempfile::tempdir().unwrap();
            let repository = RecordRepository::open(dir.path()).unwrap();
            let workspace_id = Uuid::new_v4();
            let record_id = FileRecordId::new();
            let device = SigningKey::from_bytes(&[41; 32]);
            let person = SigningKey::from_bytes(&[42; 32]);
            let actor = FileActor {
                person: Ed25519PublicKey::from_bytes(person.verifying_key().to_bytes()),
                device: Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
            };
            let make_envelope = |sequence| {
                RecordEnvelopeV1::signed(
                    record_id,
                    actor,
                    sequence,
                    RecordOperationV1::SetMetadata(FileMetadataV1 {
                        schema_version: 1,
                        record_id,
                        content_hash: None,
                        byte_len: sequence,
                        mime: None,
                        previews: vec![],
                        latest_actor: Some(actor),
                        observed_at_ms: sequence,
                        index_status: IndexStatus::Pending,
                        properties: None,
                    }),
                    &device,
                )
                .unwrap()
            };
            let key = RecordRepositoryKey {
                workspace_id,
                record_id,
            };
            repository
                .apply_batch(
                    key,
                    &[make_envelope(1), make_envelope(2)],
                    &AllowActorOn {
                        actor,
                        records: [record_id].into(),
                    },
                )
                .unwrap();

            let inventory = serve_authorized_request(
                &repository,
                workspace_id,
                &RecordMeshRequestV1 {
                    protocol_version: RECORD_MESH_PROTOCOL_VERSION,
                    workspace_id,
                    operation: RecordMeshOperationV1::Inventory {
                        after: None,
                        limit: 8,
                    },
                },
            );
            let RecordMeshResultV1::InventoryPage { cursors, .. } = inventory.result else {
                panic!("expected inventory page");
            };
            assert_eq!(cursors.len(), 1);

            let mut prefix = repository.stream_cursor(key, &actor, 1).unwrap();
            let pull = |after| RecordMeshRequestV1 {
                protocol_version: RECORD_MESH_PROTOCOL_VERSION,
                workspace_id,
                operation: RecordMeshOperationV1::Pull {
                    stream: RecordMeshStreamV1 { record_id, actor },
                    after: Some(after),
                    limit: 8,
                },
            };
            let response = serve_authorized_request(&repository, workspace_id, &pull(prefix));
            let RecordMeshResultV1::EnvelopeBatch { envelopes, .. } = response.result else {
                panic!("expected suffix batch");
            };
            assert_eq!(envelopes.len(), 1);
            assert_eq!(envelopes[0].sequence, 2);

            prefix.prefix_digest[0] ^= 1;
            assert_eq!(
                serve_authorized_request(&repository, workspace_id, &pull(prefix)).result,
                RecordMeshResultV1::Error(RecordMeshErrorCodeV1::PrefixMismatch)
            );
        }
    }

    #[tokio::test]
    async fn frames_are_big_endian_length_then_canonical_bytes() {
        let mut wire = Vec::new();
        write_record_mesh_response(
            &mut wire,
            &RecordMeshResponseV1 {
                protocol_version: RECORD_MESH_PROTOCOL_VERSION,
                result: RecordMeshResultV1::Error(RecordMeshErrorCodeV1::PrefixMismatch),
            },
        )
        .await
        .unwrap();
        assert_eq!(wire, [0, 0, 0, 3, 2, 4, 4]);

        let mut wire: Vec<u8> = vec![0, 0, 0, 21, 2, 16];
        wire.extend_from_slice(&[0x33; 16]);
        wire.extend_from_slice(&[0, 0, 16]);
        assert_eq!(
            read_record_mesh_request(&mut wire.as_slice())
                .await
                .unwrap(),
            RecordMeshRequestV1 {
                protocol_version: RECORD_MESH_PROTOCOL_VERSION,
                workspace_id: Uuid::from_bytes([0x33; 16]),
                operation: RecordMeshOperationV1::Inventory {
                    after: None,
                    limit: 16,
                },
            }
        );
    }

    #[tokio::test]
    async fn empty_and_truncated_frames_are_rejected() {
        assert!(matches!(
            read_record_mesh_request(&mut &[0, 0, 0, 0][..]).await,
            Err(RecordMeshFrameError::NonCanonical)
        ));
        for wire in [&[][..], &[0, 0][..], &[0, 0, 0, 3, 2][..]] {
            assert!(
                matches!(
                    read_record_mesh_request(&mut &wire[..]).await,
                    Err(RecordMeshFrameError::Io(ref error)) if error.kind() == io::ErrorKind::UnexpectedEof
                ),
                "{wire:?}"
            );
        }
    }
}
