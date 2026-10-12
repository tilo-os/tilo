//! The per-entry projections the filesystem answers through extended
//! attributes, and the names it answers them under.
//!
//! The daemon writes each value with `to_xattr_bytes` and every reader
//! decodes it with `parse_xattr`, so the JSON below is the one contract
//! between the mount and the apps reading it. Every projection is bounded and
//! self-validating: a value a reader accepts is one the daemon could have
//! written.

use std::collections::BTreeSet;

use collab_types::Ed25519PublicKey;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use uuid::Uuid;

use crate::file_invite::FilePermission;
use crate::files::{
    ConflictStatus, FileConflictV1, FilePropertiesV1, FileRecordError, FileRecordId, FileVersionId,
    FileVersionV1, MAX_CONFLICT_VERSIONS,
};

/// An entry's sync state: `local`, `projected` or `downloading`.
pub const SYNC_XATTR: &str = "user.tilo.sync";
/// A file's thumbnail preview. Absent when not previewable, not locally
/// generatable, and not gossiped yet.
pub const THUMB_XATTR: &str = "user.tilo.thumb";
/// Raw 32-byte BLAKE3 manifest hash for a file's current content. Clients use
/// this stable content identity for metadata/thumbnail caches, so identical
/// bytes deduplicate across paths and a rename does not invalidate a preview.
/// Absent for directories, empty files, and CRDT documents without an opaque
/// content manifest.
pub const CONTENT_HASH_XATTR: &str = "user.tilo.content-hash";
/// Raw 16-byte UUID for the stable collaborative file record. Unlike a path,
/// this identity survives rename and move and is the scope used by sharing.
pub const RECORD_ID_XATTR: &str = "user.tilo.record-id";
/// Bounded rich metadata copied from the signed replicated record.
pub const METADATA_XATTR: &str = "user.tilo.metadata";
/// Bounded workspace-root catalog of every record visible through this mount.
/// Paths are mutable provenance; consumers activate by stable `record_id` and
/// resolve the latest path from a fresh projection.
pub const RECORD_INDEX_XATTR: &str = "user.tilo.record-index";
/// Revisioned, pageable successor to [`RECORD_INDEX_XATTR`]. The manifest is
/// deliberately tiny; pages are requested as
/// `user.tilo.record-index.v2.<revision>.<offset>`. A stale revision is absent
/// rather than silently serving a different ordering.
pub const RECORD_INDEX_V2_XATTR: &str = "user.tilo.record-index.v2";
/// Opaque-cursor record index for ACL mounts; pages are requested as
/// `user.tilo.record-index.v3.<revision>.<cursor>`.
pub const RECORD_INDEX_V3_XATTR: &str = "user.tilo.record-index.v3";
/// Exact record resolution is separate from pagination so a late resource can
/// be reopened without materialising a mount catalogue.
pub const RECORD_INDEX_BY_ID_XATTR_PREFIX: &str = "user.tilo.record-index.by-id.";
/// Versioned availability snapshot. Unlike [`SYNC_XATTR`], this distinguishes
/// transfer, retention policy, replica evidence, and freshness.
pub const AVAILABILITY_XATTR: &str = "user.tilo.availability";
/// Writable local-retention policy: `pinned` or `automatic`.
pub const PIN_XATTR: &str = "user.tilo.pin";
/// Write-only availability action. The only accepted value is `retry`, which
/// schedules a bounded background hydration of the current content. No
/// bearer, peer address, or path is carried in this control.
pub const AVAILABILITY_CONTROL_XATTR: &str = "user.tilo.availability-control";
/// Versioned, redacted record-sharing state.
pub const SHARE_XATTR: &str = "user.tilo.share";
/// Absolute owner-only Unix socket used for response-bearing share
/// creation/revocation. This is only a locator, never a bearer credential.
pub const SHARE_CONTROL_XATTR: &str = "user.tilo.share-control";
/// Bounded recent history and open conflicts of one record.
pub const HISTORY_XATTR: &str = "user.tilo.history";
/// Write-only history control (restore, conflict resolution).
pub const HISTORY_CONTROL_XATTR: &str = "user.tilo.history-control";

pub const METADATA_PROJECTION_VERSION: u16 = 1;
pub const MAX_METADATA_XATTR_BYTES: usize = 2_048;
pub const RECORD_INDEX_PROJECTION_VERSION: u16 = 1;
pub const MAX_RECORD_INDEX_ENTRIES: usize = 256;
/// Stay below Linux's usual 64 KiB xattr ceiling, including filesystem framing.
pub const MAX_RECORD_INDEX_XATTR_BYTES: usize = 60 * 1024;
pub const RECORD_INDEX_V2_VERSION: u16 = 2;
pub const RECORD_INDEX_V3_VERSION: u16 = 3;
/// Keep a worst-case ordinary page within a single Linux filesystem xattr
/// block. Some supported hosts (including ext4-backed containers) reject
/// larger values with `ENOSPC` even when the volume has free space.
pub const RECORD_INDEX_PAGE_ENTRIES: usize = 32;
pub const MAX_RECORD_INDEX_PAGE_BYTES: usize = 48 * 1024;
pub const AVAILABILITY_PROJECTION_VERSION: u16 = 1;
/// Hard input/output bound for the availability xattr. Parsing an untrusted
/// value cannot allocate in proportion to an arbitrary value.
pub const MAX_AVAILABILITY_XATTR_BYTES: usize = 2_048;
/// Bound on a user-visible transfer diagnostic in the projection. The durable
/// registry may retain a longer error; the projection indicates when it
/// truncated.
pub const MAX_PROJECTED_ERROR_BYTES: usize = 512;
pub const SHARE_PROJECTION_VERSION: u16 = 1;
pub const MAX_SHARES_PER_RECORD: usize = 32;
pub const MAX_SHARE_PROJECTION_BYTES: usize = 16 * 1024;
pub const HISTORY_PROJECTION_VERSION: u16 = 1;
pub const MAX_HISTORY_PROJECTION_BYTES: usize = 16 * 1024;
pub const MAX_PROJECTED_VERSIONS: usize = 12;
pub const MAX_PROJECTED_CONFLICTS: usize = 8;

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
    Record(#[from] FileRecordError),
    #[error("projection names a different record")]
    WrongRecord,
    #[error("projection exceeds its entry bound")]
    TooManyEntries,
    #[error("availability transfer error exceeds its wire bound")]
    ErrorTooLong,
    #[error("availability transfer progress exceeds its declared total")]
    InvalidProgress,
    #[error("availability transfer error fields are inconsistent")]
    InvalidTransferError,
    #[error("known replica counts lack current-version acknowledgement evidence")]
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

/// The hydration state of one filesystem entry, carried in [`SYNC_XATTR`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// Fully present locally — a read never touches the network. Directories,
    /// empty files, and CRDT documents are always `Local`.
    Local,
    /// Metadata is projected but some content is not yet local; reading it
    /// will hydrate the missing bytes from a peer.
    Projected,
    /// A hydration fetch for this file's content is in flight right now.
    Downloading,
}

impl SyncState {
    /// The stable wire string carried in [`SYNC_XATTR`].
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

/// The value of [`METADATA_XATTR`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataProjectionV1 {
    pub schema_version: u16,
    pub record_id: FileRecordId,
    pub properties: FilePropertiesV1,
}

impl MetadataProjectionV1 {
    /// Validate the metadata projection schema, scope, and bounded properties.
    ///
    /// # Errors
    ///
    /// Returns an error when any projection invariant is invalid.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != METADATA_PROJECTION_VERSION || self.record_id.0.is_nil() {
            return Err("invalid metadata projection scope");
        }
        self.properties
            .validate()
            .map_err(|_| "invalid file properties")
    }

    /// # Errors
    ///
    /// Returns an error when validation, serialization, or the size bound fails.
    pub fn to_xattr_bytes(&self) -> Result<Vec<u8>, ProjectionError> {
        self.validate().map_err(ProjectionError::Invalid)?;
        encode(self, MAX_METADATA_XATTR_BYTES)
    }

    /// # Errors
    ///
    /// Returns an error when decoding, validation, or the size bound fails.
    pub fn parse_xattr(bytes: &[u8]) -> Result<Self, ProjectionError> {
        let value: Self = decode(bytes, MAX_METADATA_XATTR_BYTES)?;
        value.validate().map_err(ProjectionError::Invalid)?;
        Ok(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordIndexEntryV1 {
    pub record_id: FileRecordId,
    /// Absolute path within this mount, never global authority.
    pub path: String,
    pub is_dir: bool,
}

/// The single bounded value of [`RECORD_INDEX_XATTR`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordIndexProjectionV1 {
    pub schema_version: u16,
    pub truncated: bool,
    pub records: Vec<RecordIndexEntryV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordIndexManifestV2 {
    pub schema_version: u16,
    /// Opaque content revision, derived from the namespace heads. It contains
    /// no host path or authority material.
    pub revision: String,
    pub records: usize,
    pub page_entries: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordIndexPageV2 {
    pub schema_version: u16,
    pub revision: String,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub records: Vec<RecordIndexEntryV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordIndexExactV2 {
    pub schema_version: u16,
    pub revision: String,
    pub record: RecordIndexEntryV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordIndexManifestV3 {
    pub schema_version: u16,
    pub revision: String,
    /// Fixed, opaque capability for exactly one bounded source window.
    pub initial_cursor: String,
    pub page_entries: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordIndexPageV3 {
    pub schema_version: u16,
    pub revision: String,
    pub next_cursor: Option<String>,
    pub records: Vec<RecordIndexEntryV1>,
}

/// A record-index revision is a 64-digit hex digest.
pub fn valid_record_index_revision(revision: &str) -> bool {
    revision.len() == 64 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_record_index_path(path: &str) -> bool {
    path.starts_with('/') && !path.contains('\0')
}

fn strictly_ascending(records: &[RecordIndexEntryV1]) -> bool {
    records
        .windows(2)
        .all(|pair| matches!(pair, [first, second] if first.record_id < second.record_id))
}

impl RecordIndexProjectionV1 {
    /// Validate the bounded, canonical record-index projection.
    ///
    /// # Errors
    ///
    /// Returns an error when version, bounds, ordering, identity, or path
    /// invariants are invalid.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != RECORD_INDEX_PROJECTION_VERSION {
            return Err("unsupported record-index projection version");
        }
        if self.records.len() > MAX_RECORD_INDEX_ENTRIES {
            return Err("record-index projection exceeds its entry bound");
        }
        if !self
            .records
            .iter()
            .all(|record| valid_record_index_path(&record.path))
        {
            return Err("record-index path is not an absolute mount path");
        }
        if !strictly_ascending(&self.records) {
            return Err("record-index identities are not canonical and unique");
        }
        Ok(())
    }
}

impl RecordIndexManifestV2 {
    /// # Errors
    ///
    /// Returns an error when a peer-supplied manifest violates the bounded
    /// page protocol or contains a non-canonical revision.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != RECORD_INDEX_V2_VERSION
            || !valid_record_index_revision(&self.revision)
            || self.page_entries == 0
            || self.page_entries > RECORD_INDEX_PAGE_ENTRIES
        {
            return Err("invalid record-index manifest");
        }
        Ok(())
    }
}

impl RecordIndexPageV2 {
    /// # Errors
    ///
    /// Returns an error when a peer-supplied page exceeds its bounds or could
    /// mix non-canonical identities/paths into an authority generation.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != RECORD_INDEX_V2_VERSION
            || !valid_record_index_revision(&self.revision)
            || self.records.len() > RECORD_INDEX_PAGE_ENTRIES
            || !self.offset.is_multiple_of(RECORD_INDEX_PAGE_ENTRIES)
        {
            return Err("invalid record-index page");
        }
        if !self
            .records
            .iter()
            .all(|record| valid_record_index_path(&record.path))
        {
            return Err("record-index path is not an absolute mount path");
        }
        if !strictly_ascending(&self.records) {
            return Err("record-index identities are not canonical and unique");
        }
        if self.next_offset.is_some_and(|next| {
            next <= self.offset || !next.is_multiple_of(RECORD_INDEX_PAGE_ENTRIES)
        }) {
            return Err("record-index page continuation is invalid");
        }
        Ok(())
    }
}

impl RecordIndexExactV2 {
    /// # Errors
    ///
    /// Returns an error when an exact authority projection is malformed.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != RECORD_INDEX_V2_VERSION
            || !valid_record_index_revision(&self.revision)
            || !valid_record_index_path(&self.record.path)
        {
            return Err("invalid exact record-index projection");
        }
        Ok(())
    }
}

impl RecordIndexManifestV3 {
    /// # Errors
    ///
    /// Returns an error when an opaque authority manifest is malformed.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != RECORD_INDEX_V3_VERSION
            || !valid_record_index_revision(&self.revision)
            || self.initial_cursor.len() != 36
            || self.page_entries != RECORD_INDEX_PAGE_ENTRIES
        {
            return Err("invalid opaque record-index manifest");
        }
        Ok(())
    }
}

impl RecordIndexPageV3 {
    /// # Errors
    ///
    /// Returns an error when an opaque authority page is malformed.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != RECORD_INDEX_V3_VERSION
            || !valid_record_index_revision(&self.revision)
            || self.records.len() > RECORD_INDEX_PAGE_ENTRIES
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
            .all(|record| valid_record_index_path(&record.path))
            || !strictly_ascending(&self.records)
        {
            return Err("invalid opaque record-index entry");
        }
        Ok(())
    }
}

/// How much of the current payload is resident on this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum Residency {
    /// Metadata is present, but no payload bytes are promised locally.
    NotResident,
    /// Some, but not all, payload bytes are present.
    Partial { resident_bytes: u64 },
    /// The complete current payload has been verified locally.
    Resident { bytes: u64 },
}

/// Current transfer lifecycle, independent of local residency.
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

/// Explicit local retention policy. Directory policy applies transitively.
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
    pub record_id: FileRecordId,
    pub current_version: Option<FileVersionId>,
    pub residency: Residency,
    pub transfer: ProjectedTransfer,
    /// Effective policy, with directory-pin inheritance already resolved.
    pub policy: AvailabilityPolicy,
    /// True when `policy == pinned` because a stable ancestor directory is
    /// pinned rather than because this record has a direct pin.
    #[serde(default)]
    pub policy_inherited: bool,
    pub replicas: ProjectedReplicaEvidence,
    pub freshness: ProjectedFreshness,
}

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

/// Replica counts are either explicitly known for the current version or
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
    /// # Errors
    ///
    /// Returns an error when validation, serialization, or the size bound fails.
    pub fn to_xattr_bytes(&self) -> Result<Vec<u8>, ProjectionError> {
        self.validate()?;
        encode(self, MAX_AVAILABILITY_XATTR_BYTES)
    }

    /// # Errors
    ///
    /// Returns an error when decoding, validation, or the size bound fails.
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
            && (self.current_version.is_none() || self.freshness.replicas_observed_at_ms.is_none())
        {
            return Err(ProjectionError::InvalidReplicaEvidence);
        }
        Ok(())
    }

    /// At least one durable replica acknowledged the current version.
    pub fn safely_replicated(&self) -> bool {
        matches!(self.replicas, ProjectedReplicaEvidence::Known { durable, .. } if durable > 0)
    }
}

/// Redacted, versioned value of [`SHARE_XATTR`]. Bearer secrets and secret
/// hashes are never projected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareProjectionV1 {
    pub schema_version: u16,
    pub record_id: FileRecordId,
    pub observed_at_ms: u64,
    pub grants: Vec<ShareGrantProjectionV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "these booleans are explicit redundant fields in the versioned projection schema and are cross-validated on decode"
)]
pub struct ShareGrantProjectionV1 {
    pub grant_id: Uuid,
    pub subject: ShareSubjectProjection,
    pub permission: FilePermission,
    pub expires_at_ms: Option<u64>,
    pub revoked: bool,
    pub expired: bool,
    pub used: bool,
    pub exhausted: bool,
    pub status: ShareGrantStatus,
    pub redemptions: u32,
    pub max_redemptions: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ShareSubjectProjection {
    Person { person: Ed25519PublicKey },
    BearerLink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareGrantStatus {
    Active,
    Expired,
    Revoked,
    Exhausted,
}

impl ShareGrantStatus {
    /// The one status a grant reports; revocation outranks expiry, which
    /// outranks exhaustion.
    pub fn derive(revoked: bool, expired: bool, exhausted: bool) -> Self {
        if revoked {
            Self::Revoked
        } else if expired {
            Self::Expired
        } else if exhausted {
            Self::Exhausted
        } else {
            Self::Active
        }
    }
}

impl ShareProjectionV1 {
    /// # Errors
    ///
    /// Returns an error for inconsistent state, JSON failure, or a projection
    /// above the byte bound.
    pub fn to_xattr_bytes(&self) -> Result<Vec<u8>, ProjectionError> {
        self.validate()?;
        encode(self, MAX_SHARE_PROJECTION_BYTES)
    }

    /// # Errors
    ///
    /// Returns an error for oversized/malformed JSON, unsupported schema, or
    /// inconsistent/duplicate grant state.
    pub fn parse_xattr(bytes: &[u8]) -> Result<Self, ProjectionError> {
        let projection: Self = decode(bytes, MAX_SHARE_PROJECTION_BYTES)?;
        projection.validate()?;
        Ok(projection)
    }

    fn validate(&self) -> Result<(), ProjectionError> {
        if self.schema_version != SHARE_PROJECTION_VERSION {
            return Err(ProjectionError::UnsupportedVersion(self.schema_version));
        }
        if self.grants.len() > MAX_SHARES_PER_RECORD {
            return Err(ProjectionError::TooManyEntries);
        }
        let mut ids = BTreeSet::new();
        for grant in &self.grants {
            if !ids.insert(grant.grant_id) {
                return Err(ProjectionError::InvalidGrantState);
            }
            let expired = grant
                .expires_at_ms
                .is_some_and(|deadline| self.observed_at_ms >= deadline);
            let used = grant.redemptions > 0;
            let exhausted = grant
                .max_redemptions
                .is_some_and(|max| grant.redemptions >= max);
            if grant.expired != expired
                || grant.used != used
                || grant.exhausted != exhausted
                || grant.status != ShareGrantStatus::derive(grant.revoked, expired, exhausted)
                || grant
                    .max_redemptions
                    .is_some_and(|max| max == 0 || grant.redemptions > max)
                || matches!(grant.subject, ShareSubjectProjection::Person { .. })
                    && grant.redemptions != 0
            {
                return Err(ProjectionError::InvalidGrantState);
            }
        }
        Ok(())
    }

    pub fn active_grants(&self) -> usize {
        self.grants
            .iter()
            .filter(|grant| grant.status == ShareGrantStatus::Active)
            .count()
    }
}

/// Bounded value of [`HISTORY_XATTR`]: the newest versions and conflicts of
/// one record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryProjectionV1 {
    pub schema_version: u16,
    pub record_id: FileRecordId,
    pub total_versions: u64,
    pub versions_truncated: bool,
    pub heads: Vec<FileVersionId>,
    pub versions: Vec<FileVersionV1>,
    pub conflicts_truncated: bool,
    pub conflicts: Vec<FileConflictV1>,
}

impl HistoryProjectionV1 {
    /// # Errors
    ///
    /// Returns an error when JSON encoding fails or the result exceeds the
    /// projection byte limit.
    pub fn to_xattr_bytes(&self) -> Result<Vec<u8>, ProjectionError> {
        encode(self, MAX_HISTORY_PROJECTION_BYTES)
    }

    /// # Errors
    ///
    /// Returns an error for oversized or malformed JSON, unsupported schema,
    /// invalid records/conflicts, or an inconsistent record id/count.
    pub fn parse_xattr(bytes: &[u8]) -> Result<Self, ProjectionError> {
        let value: Self = decode(bytes, MAX_HISTORY_PROJECTION_BYTES)?;
        if value.schema_version != HISTORY_PROJECTION_VERSION {
            return Err(ProjectionError::UnsupportedVersion(value.schema_version));
        }
        if value.versions.len() > MAX_PROJECTED_VERSIONS
            || value.conflicts.len() > MAX_PROJECTED_CONFLICTS
            || value.heads.len() > MAX_CONFLICT_VERSIONS
            || value.total_versions < value.versions.len() as u64
        {
            return Err(ProjectionError::TooManyEntries);
        }
        for version in &value.versions {
            version.validate()?;
            if version.record_id != value.record_id {
                return Err(ProjectionError::WrongRecord);
            }
        }
        for conflict in &value.conflicts {
            conflict.validate()?;
            if conflict.record_id != value.record_id {
                return Err(ProjectionError::WrongRecord);
            }
        }
        Ok(value)
    }

    pub fn open_conflicts(&self) -> impl Iterator<Item = &FileConflictV1> {
        self.conflicts
            .iter()
            .filter(|conflict| conflict.status == ConflictStatus::Open)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn availability_json(replicas: &serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
            "current_version": "a0d4e23f-c7cb-4ded-a427-b32cf9da9342",
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
        // Metadata round-trips and is bounded.
        {
            let projection = MetadataProjectionV1 {
                schema_version: METADATA_PROJECTION_VERSION,
                record_id: FileRecordId(Uuid::from_u128(7)),
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
        // The three sync states round-trip; junk is rejected.
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
            // Trailing whitespace/newline (some xattr tools append one) is tolerated.
            assert_eq!(SyncState::from_xattr(b"local\n"), Some(SyncState::Local));
            assert_eq!(SyncState::from_xattr(b"bogus"), None);
            assert_eq!(SyncState::from_xattr(&[0xff, 0xfe]), None);
        }
        // Unknown replica evidence stays distinct from a known zero.
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
        // Share and history projections decode the daemon's JSON.
        {
            let share = serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "observed_at_ms": 10,
                "grants": [{
                    "grant_id": "c0d4e23f-c7cb-4ded-a427-b32cf9da9343",
                    "subject": {"kind": "person", "person": vec![1; 32]},
                    "permission": "View",
                    "expires_at_ms": null,
                    "revoked": false,
                    "expired": false,
                    "used": false,
                    "exhausted": false,
                    "status": "active",
                    "redemptions": 0,
                    "max_redemptions": null
                }]
            }))
            .unwrap();
            let share = ShareProjectionV1::parse_xattr(&share).unwrap();
            assert_eq!(share.active_grants(), 1);

            let history = serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "total_versions": 1,
                "versions_truncated": false,
                "heads": ["a0d4e23f-c7cb-4ded-a427-b32cf9da9342"],
                "versions": [{
                    "id": "a0d4e23f-c7cb-4ded-a427-b32cf9da9342",
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "parents": [],
                    "actor": {"person": vec![1; 32], "device": vec![2; 32]},
                    "timestamp_ms": 9,
                    "content_hash": null,
                    "metadata_hash": vec![3; 32],
                    "provenance": "Create"
                }],
                "conflicts_truncated": false,
                "conflicts": []
            }))
            .unwrap();
            let history = HistoryProjectionV1::parse_xattr(&history).unwrap();
            assert_eq!(
                history.versions[0].provenance,
                crate::files::FileProvenance::Create
            );
            assert!(
                ShareProjectionV1::parse_xattr(&vec![b' '; MAX_SHARE_PROJECTION_BYTES + 1])
                    .is_err()
            );
            assert!(
                HistoryProjectionV1::parse_xattr(&vec![b' '; MAX_HISTORY_PROJECTION_BYTES + 1])
                    .is_err()
            );
        }
    }
}
