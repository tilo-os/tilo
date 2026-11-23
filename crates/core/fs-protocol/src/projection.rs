//! The per-entry projections the filesystem answers through extended
//! attributes, and the names it answers them under.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

use crate::files::{ContentHash, FileDocumentError, FileDocumentId, FilePropertiesV1};

pub const SYNC_XATTR: &str = "user.tilo.sync";
/// A file's thumbnail preview. Absent when not previewable, not locally
/// generatable, and not replicated yet.
pub const THUMB_XATTR: &str = "user.tilo.thumb";
/// Raw 32-byte BLAKE3 manifest hash for a file's current content. Clients use
/// this stable content identity for metadata/thumbnail caches, so identical
/// bytes deduplicate across paths and a rename does not invalidate a preview.
/// Absent for directories, empty files, and CRDT documents without an opaque
/// content manifest.
pub const CONTENT_HASH_XATTR: &str = "user.tilo.content-hash";
/// Bounded rich metadata copied from the signed replicated record.
pub const METADATA_XATTR: &str = "user.tilo.metadata";
/// Revisioned, pageable workspace-root catalog of every record visible through
/// a mount without an ACL. Paths are mutable provenance; consumers activate by
/// stable `document_id` and resolve the latest path from a fresh page. The
/// manifest is deliberately tiny; pages are requested as
/// `user.tilo.record-index.v2.<revision>.<offset>`. A stale revision is absent
/// rather than silently serving a different ordering.
pub const DOCUMENT_INDEX_V2_XATTR: &str = "user.tilo.record-index.v2";
/// Opaque-cursor record index for ACL mounts; pages are requested as
/// `user.tilo.record-index.v3.<revision>.<cursor>`.
pub const DOCUMENT_INDEX_V3_XATTR: &str = "user.tilo.record-index.v3";
/// Exact record resolution is separate from pagination so a late resource can
/// be reopened without materialising a mount catalogue.
pub const DOCUMENT_INDEX_BY_ID_XATTR_PREFIX: &str = "user.tilo.record-index.by-id.";
/// Versioned availability snapshot. Unlike [`SYNC_XATTR`], this distinguishes
/// transfer, retention policy, replica evidence, and freshness.
pub const AVAILABILITY_XATTR: &str = "user.tilo.availability";
pub const PIN_XATTR: &str = "user.tilo.pin";
/// Write-only availability action. The only accepted value is `retry`, which
/// schedules a bounded background hydration of the current content. No
/// bearer, peer address, or path is carried in this control.
pub const AVAILABILITY_CONTROL_XATTR: &str = "user.tilo.availability-control";

pub const METADATA_PROJECTION_VERSION: u16 = 1;
pub const MAX_METADATA_XATTR_BYTES: usize = 2_048;
pub const DOCUMENT_INDEX_V2_VERSION: u16 = 2;
pub const DOCUMENT_INDEX_V3_VERSION: u16 = 3;
/// Keep a worst-case ordinary page within a single Linux filesystem xattr
/// block. Some supported hosts (including ext4-backed containers) reject
/// larger values with `ENOSPC` even when the volume has free space.
pub const DOCUMENT_INDEX_PAGE_ENTRIES: usize = 32;
pub const MAX_DOCUMENT_INDEX_PAGE_BYTES: usize = 48 * 1024;
pub const AVAILABILITY_PROJECTION_VERSION: u16 = 2;
/// Hard input/output bound for the availability xattr. Parsing an untrusted
/// value cannot allocate in proportion to an arbitrary value.
pub const MAX_AVAILABILITY_XATTR_BYTES: usize = 2_048;
/// Bound on a user-visible transfer diagnostic in the projection. The durable
/// registry may retain a longer error; the projection indicates when it
/// truncated.
pub const MAX_PROJECTED_ERROR_BYTES: usize = 512;

#[derive(Debug, Error)]
pub enum ProjectionError {
    #[error("projection of {len} bytes exceeds its {max}-byte bound")]
    TooLarge { len: usize, max: usize },
    #[error("projection is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported projection version {0}")]
    UnsupportedVersion(u16),
    #[error("invalid projection: {0}")]
    Invalid(&'static str),
    #[error("invalid file record: {0}")]
    Record(#[from] FileDocumentError),
    #[error("projection names a different record")]
    WrongDocument,
    #[error("projection exceeds its entry bound")]
    TooManyEntries,
    #[error("availability transfer error exceeds its wire bound")]
    ErrorTooLong,
    #[error("availability transfer progress exceeds its declared total")]
    InvalidProgress,
    #[error("availability transfer error fields are inconsistent")]
    InvalidTransferError,
    #[error("known replica counts lack current-content acknowledgement evidence")]
    InvalidReplicaEvidence,
    #[error("share projection contains inconsistent or duplicate grant state")]
    InvalidGrantState,
}

fn decode<T: DeserializeOwned>(bytes: &[u8], max: usize) -> Result<T, ProjectionError> {
    if bytes.len() > max {
        return Err(ProjectionError::TooLarge {
            len: bytes.len(),
            max,
        });
    }
    Ok(serde_json::from_slice(bytes)?)
}

fn encode<T: Serialize>(value: &T, max: usize) -> Result<Vec<u8>, ProjectionError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > max {
        return Err(ProjectionError::TooLarge {
            len: bytes.len(),
            max,
        });
    }
    Ok(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// Fully present locally — a read never touches the network. Directories,
    /// empty files, and CRDT documents are always `Local`.
    Local,
    /// Metadata is projected but some content is not yet local; reading it
    /// will hydrate the missing bytes from a peer.
    Projected,
    Downloading,
}

impl SyncState {
    pub fn as_str(self) -> &'static str {
        match self {
            SyncState::Local => "local",
            SyncState::Projected => "projected",
            SyncState::Downloading => "downloading",
        }
    }

    /// Parse an xattr value; surrounding whitespace (some xattr tools append
    /// a newline) is tolerated, anything else is `None`.
    pub fn from_xattr(bytes: &[u8]) -> Option<Self> {
        match std::str::from_utf8(bytes).ok()?.trim() {
            "local" => Some(SyncState::Local),
            "projected" => Some(SyncState::Projected),
            "downloading" => Some(SyncState::Downloading),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataProjectionV1 {
    pub schema_version: u16,
    pub document_id: FileDocumentId,
    pub properties: FilePropertiesV1,
}

impl MetadataProjectionV1 {
    /// Validate the metadata projection schema, scope, and bounded properties.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != METADATA_PROJECTION_VERSION || self.document_id.0.is_nil() {
            return Err("invalid metadata projection scope");
        }
        self.properties
            .validate()
            .map_err(|_| "invalid file properties")
    }

    pub fn to_xattr_bytes(&self) -> Result<Vec<u8>, ProjectionError> {
        self.validate().map_err(ProjectionError::Invalid)?;
        encode(self, MAX_METADATA_XATTR_BYTES)
    }

    pub fn parse_xattr(bytes: &[u8]) -> Result<Self, ProjectionError> {
        let value: Self = decode(bytes, MAX_METADATA_XATTR_BYTES)?;
        value.validate().map_err(ProjectionError::Invalid)?;
        Ok(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentIndexEntryV1 {
    pub document_id: FileDocumentId,
    /// Absolute path within this mount, never global authority.
    pub path: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentIndexManifestV2 {
    pub schema_version: u16,
    /// Opaque content revision, derived from the namespace heads. It contains
    /// no host path or authority material.
    pub revision: String,
    pub records: usize,
    pub page_entries: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentIndexPageV2 {
    pub schema_version: u16,
    pub revision: String,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub records: Vec<DocumentIndexEntryV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentIndexExactV2 {
    pub schema_version: u16,
    pub revision: String,
    pub record: DocumentIndexEntryV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentIndexManifestV3 {
    pub schema_version: u16,
    pub revision: String,
    /// Fixed, opaque capability for exactly one bounded source window.
    pub initial_cursor: String,
    pub page_entries: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentIndexPageV3 {
    pub schema_version: u16,
    pub revision: String,
    pub next_cursor: Option<String>,
    pub records: Vec<DocumentIndexEntryV1>,
}

pub fn valid_document_index_revision(revision: &str) -> bool {
    revision.len() == 64 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_document_index_path(path: &str) -> bool {
    path.starts_with('/') && !path.contains('\0')
}

fn strictly_ascending(records: &[DocumentIndexEntryV1]) -> bool {
    records
        .windows(2)
        .all(|pair| matches!(pair, [first, second] if first.document_id < second.document_id))
}

impl DocumentIndexManifestV2 {
    /// Decode JSON through the shared protocol implementation. Callers still
    /// check their transport bounds and the expected authority generation.
    pub fn from_json(bytes: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(bytes)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != DOCUMENT_INDEX_V2_VERSION
            || !valid_document_index_revision(&self.revision)
            || self.page_entries == 0
            || self.page_entries > DOCUMENT_INDEX_PAGE_ENTRIES
        {
            return Err("invalid record-index manifest");
        }
        Ok(())
    }
}

impl DocumentIndexPageV2 {
    /// Decode JSON through the shared protocol implementation. Callers still
    /// check their transport bounds and the expected authority generation.
    pub fn from_json(bytes: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(bytes)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != DOCUMENT_INDEX_V2_VERSION
            || !valid_document_index_revision(&self.revision)
            || self.records.len() > DOCUMENT_INDEX_PAGE_ENTRIES
            || !self.offset.is_multiple_of(DOCUMENT_INDEX_PAGE_ENTRIES)
        {
            return Err("invalid record-index page");
        }
        if !self
            .records
            .iter()
            .all(|record| valid_document_index_path(&record.path))
        {
            return Err("record-index path is not an absolute mount path");
        }
        if !strictly_ascending(&self.records) {
            return Err("record-index identities are not canonical and unique");
        }
        if self.next_offset.is_some_and(|next| {
            next <= self.offset || !next.is_multiple_of(DOCUMENT_INDEX_PAGE_ENTRIES)
        }) {
            return Err("record-index page continuation is invalid");
        }
        Ok(())
    }
}

impl DocumentIndexExactV2 {
    /// Decode JSON through the shared protocol implementation. Callers still
    /// check their transport bounds and the expected authority generation.
    pub fn from_json(bytes: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(bytes)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != DOCUMENT_INDEX_V2_VERSION
            || !valid_document_index_revision(&self.revision)
            || !valid_document_index_path(&self.record.path)
        {
            return Err("invalid exact record-index projection");
        }
        Ok(())
    }
}

impl DocumentIndexManifestV3 {
    /// Decode JSON through the shared protocol implementation. Callers still
    /// check their transport bounds and the expected authority generation.
    pub fn from_json(bytes: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(bytes)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != DOCUMENT_INDEX_V3_VERSION
            || !valid_document_index_revision(&self.revision)
            || self.initial_cursor.len() != 36
            || self.page_entries != DOCUMENT_INDEX_PAGE_ENTRIES
        {
            return Err("invalid opaque record-index manifest");
        }
        Ok(())
    }
}

impl DocumentIndexPageV3 {
    /// Decode JSON through the shared protocol implementation. Callers still
    /// check their transport bounds and the expected authority generation.
    pub fn from_json(bytes: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(bytes)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != DOCUMENT_INDEX_V3_VERSION
            || !valid_document_index_revision(&self.revision)
            || self.records.len() > DOCUMENT_INDEX_PAGE_ENTRIES
            || self
                .next_cursor
                .as_ref()
                .is_some_and(|cursor| cursor.len() != 36)
        {
            return Err("invalid opaque record-index page");
        }
        if !self
            .records
            .iter()
            .all(|record| valid_document_index_path(&record.path))
            || !strictly_ascending(&self.records)
        {
            return Err("invalid opaque record-index entry");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum Residency {
    /// Metadata is present, but no payload bytes are promised locally.
    NotResident,
    /// Some, but not all, payload bytes are present.
    Partial {
        resident_bytes: u64,
    },
    Resident {
        bytes: u64,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferPhase {
    #[default]
    Idle,
    Queued,
    Downloading,
    Uploading,
    Verifying,
}

impl TransferPhase {
    pub fn is_active(self) -> bool {
        self != Self::Idle
    }
}

/// Byte progress for the current transfer, if its size is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferProgress {
    pub completed_bytes: u64,
    pub total_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityPolicy {
    #[default]
    Automatic,
    Pinned,
}

/// Bounded, versioned availability value of [`AVAILABILITY_XATTR`].
///
/// This intentionally contains no path and no peer identities. Stable record
/// identity survives rename, while aggregate replica evidence does not reveal
/// who else has access to the file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityProjectionV1 {
    pub schema_version: u16,
    pub document_id: FileDocumentId,
    /// The file's current content, by its hash; none for a folder or an empty file.
    pub current_hash: Option<ContentHash>,
    pub residency: Residency,
    pub transfer: ProjectedTransfer,
    pub policy: AvailabilityPolicy,
    /// True when `policy == pinned` because a stable ancestor directory is
    /// pinned rather than because this record has a direct pin.
    #[serde(default)]
    pub policy_inherited: bool,
    pub replicas: ProjectedReplicaEvidence,
    pub freshness: ProjectedFreshness,
}

/// The [`ProjectedTransfer::last_error`] of a file whose bytes no device
/// delivered: a fetch, or a read that waited its bound, found no peer that
/// had them. Files shows such a file as unavailable until a device that
/// keeps it is back online.
pub const NO_PEER_DELIVERED_ERROR: &str = "No available replica could provide this file";

/// Bounded transfer status for the xattr/UI boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectedTransfer {
    pub phase: TransferPhase,
    pub progress: Option<TransferProgress>,
    pub last_error: Option<String>,
    pub last_error_at_ms: Option<u64>,
    pub error_truncated: bool,
}

/// Replica counts are either explicitly known for the current content or
/// unknown. Zero is a valid known count and is never used as a missing-value
/// sentinel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ProjectedReplicaEvidence {
    Unknown,
    Known { reachable: u32, durable: u32 },
}

/// Independent observation clocks used to communicate stale/degraded state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectedFreshness {
    pub metadata_observed_at_ms: Option<u64>,
    pub local_verified_at_ms: Option<u64>,
    pub replicas_observed_at_ms: Option<u64>,
}

impl AvailabilityProjectionV1 {
    pub fn to_xattr_bytes(&self) -> Result<Vec<u8>, ProjectionError> {
        self.validate()?;
        encode(self, MAX_AVAILABILITY_XATTR_BYTES)
    }

    pub fn parse_xattr(bytes: &[u8]) -> Result<Self, ProjectionError> {
        let projection: Self = decode(bytes, MAX_AVAILABILITY_XATTR_BYTES)?;
        projection.validate()?;
        Ok(projection)
    }

    fn validate(&self) -> Result<(), ProjectionError> {
        if self.schema_version != AVAILABILITY_PROJECTION_VERSION {
            return Err(ProjectionError::UnsupportedVersion(self.schema_version));
        }
        if self
            .transfer
            .last_error
            .as_ref()
            .is_some_and(|message| message.len() > MAX_PROJECTED_ERROR_BYTES)
        {
            return Err(ProjectionError::ErrorTooLong);
        }
        if self.transfer.progress.is_some_and(|progress| {
            progress
                .total_bytes
                .is_some_and(|total| progress.completed_bytes > total)
        }) {
            return Err(ProjectionError::InvalidProgress);
        }
        if self.transfer.last_error.is_some() != self.transfer.last_error_at_ms.is_some()
            || (self.transfer.last_error.is_none() && self.transfer.error_truncated)
        {
            return Err(ProjectionError::InvalidTransferError);
        }
        if matches!(self.replicas, ProjectedReplicaEvidence::Known { .. })
            && (self.current_hash.is_none() || self.freshness.replicas_observed_at_ms.is_none())
        {
            return Err(ProjectionError::InvalidReplicaEvidence);
        }
        Ok(())
    }

    pub fn safely_replicated(&self) -> bool {
        matches!(self.replicas, ProjectedReplicaEvidence::Known { durable, .. } if durable > 0)
    }

    /// Bytes of the current payload are missing here and no device
    /// delivered them when they were last asked for: the device that keeps
    /// the file is offline, so opening it would fail.
    pub fn unavailable(&self) -> bool {
        !matches!(self.residency, Residency::Resident { .. })
            && self.transfer.last_error.as_deref() == Some(NO_PEER_DELIVERED_ERROR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn shared_record_index_decoders_keep_the_wire_schema() {
        let record = DocumentIndexEntryV1 {
            document_id: FileDocumentId(Uuid::from_u128(7)),
            path: "/Documents/report.txt".into(),
            is_dir: false,
        };
        let revision = "a".repeat(64);
        macro_rules! round_trip {
            ($ty:ty, $value:expr) => {{
                let expected = $value;
                let bytes = serde_json::to_vec(&expected).unwrap();
                assert_eq!(<$ty>::from_json(&bytes).unwrap(), expected);
                assert!(<$ty>::from_json(b"{}").is_err());
                assert!(<$ty>::from_json(b"not JSON").is_err());
                let mut unknown = serde_json::to_value(&expected).unwrap();
                unknown["unknown"] = serde_json::Value::Bool(true);
                assert!(<$ty>::from_json(&serde_json::to_vec(&unknown).unwrap()).is_err());
            }};
        }
        round_trip!(
            DocumentIndexManifestV2,
            DocumentIndexManifestV2 {
                schema_version: 2,
                revision: revision.clone(),
                records: 1,
                page_entries: DOCUMENT_INDEX_PAGE_ENTRIES,
            }
        );
        round_trip!(
            DocumentIndexPageV2,
            DocumentIndexPageV2 {
                schema_version: 2,
                revision: revision.clone(),
                offset: 0,
                next_offset: Some(DOCUMENT_INDEX_PAGE_ENTRIES),
                records: vec![record.clone()],
            }
        );
        round_trip!(
            DocumentIndexExactV2,
            DocumentIndexExactV2 {
                schema_version: 2,
                revision: revision.clone(),
                record: record.clone(),
            }
        );
        round_trip!(
            DocumentIndexManifestV3,
            DocumentIndexManifestV3 {
                schema_version: 3,
                revision: revision.clone(),
                initial_cursor: Uuid::from_u128(11).to_string(),
                page_entries: DOCUMENT_INDEX_PAGE_ENTRIES,
            }
        );
        round_trip!(
            DocumentIndexPageV3,
            DocumentIndexPageV3 {
                schema_version: 3,
                revision,
                next_cursor: Some(Uuid::from_u128(13).to_string()),
                records: vec![record],
            }
        );
    }

    fn availability_json(replicas: &serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 2,
            "document_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
            "current_hash": "4242424242424242424242424242424242424242424242424242424242424242",
            "residency": {"state": "resident", "bytes": 42},
            "transfer": {
                "phase": "idle", "progress": null, "last_error": null,
                "last_error_at_ms": null, "error_truncated": false
            },
            "policy": "automatic",
            "replicas": replicas,
            "freshness": {
                "metadata_observed_at_ms": 1,
                "local_verified_at_ms": 2,
                "replicas_observed_at_ms": 3
            }
        }))
        .unwrap()
    }

    /// Metadata, sync state, availability, share and history projections are
    /// bounded, validated and keep unknown distinct.
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario per projection, each labelled and self-contained"
    )]
    #[test]
    fn projections_are_bounded_and_self_validating() {
        {
            let projection = MetadataProjectionV1 {
                schema_version: METADATA_PROJECTION_VERSION,
                document_id: FileDocumentId(Uuid::from_u128(7)),
                properties: FilePropertiesV1 {
                    schema_version: crate::files::FILE_PROPERTIES_VERSION,
                    image_width: Some(800),
                    image_height: Some(600),
                    duration_ms: None,
                    page_count: None,
                    codec: Some("png".into()),
                    container: Some("png".into()),
                },
            };
            let bytes = projection.to_xattr_bytes().unwrap();
            assert_eq!(
                MetadataProjectionV1::parse_xattr(&bytes).unwrap(),
                projection
            );
            assert!(matches!(
                MetadataProjectionV1::parse_xattr(&vec![b' '; MAX_METADATA_XATTR_BYTES + 1]),
                Err(ProjectionError::TooLarge { .. })
            ));
        }
        {
            for state in [
                SyncState::Local,
                SyncState::Projected,
                SyncState::Downloading,
            ] {
                assert_eq!(
                    SyncState::from_xattr(state.as_str().as_bytes()),
                    Some(state)
                );
            }
            assert_eq!(SyncState::from_xattr(b"local\n"), Some(SyncState::Local));
            assert_eq!(SyncState::from_xattr(b"bogus"), None);
            assert_eq!(SyncState::from_xattr(&[0xff, 0xfe]), None);
        }
        {
            let unknown = AvailabilityProjectionV1::parse_xattr(&availability_json(
                &serde_json::json!({"state": "unknown"}),
            ))
            .unwrap();
            assert_eq!(unknown.replicas, ProjectedReplicaEvidence::Unknown);
            assert!(!unknown.safely_replicated());

            let zero = AvailabilityProjectionV1::parse_xattr(&availability_json(
                &serde_json::json!({"state": "known", "reachable": 0, "durable": 0}),
            ))
            .unwrap();
            assert_eq!(
                zero.replicas,
                ProjectedReplicaEvidence::Known {
                    reachable: 0,
                    durable: 0
                }
            );
            assert!(!zero.safely_replicated());
            assert!(
                AvailabilityProjectionV1::parse_xattr(&vec![
                    b' ';
                    MAX_AVAILABILITY_XATTR_BYTES + 1
                ])
                .is_err()
            );
        }
    }
}
