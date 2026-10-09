//! Per-entry sync/hydration state, exposed to apps through the
//! `user.tilo.sync` extended attribute.
//!
//! The FS projects remote metadata immediately but only hydrates content on
//! first read, so an app needs a way to tell local from remote from
//! mid-download. `getxattr(path, "user.tilo.sync")` returns `local` /
//! `projected` / `downloading` — rides the existing mount, no Tilo-specific
//! client needed, and is sandbox-safe: any process that can `stat` the file can read it, nothing more.

use fs_protocol::{FileRecordId, FileVersionId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::availability::{
    AvailabilityPolicy, RecordAvailability, Residency, TransferPhase, TransferProgress,
};

/// The xattr name the driver answers with an entry's sync state (`getxattr(path, "user.tilo.sync")`).
pub const SYNC_XATTR: &str = "user.tilo.sync";

/// The xattr name for a file's thumbnail preview. `ENODATA` when not previewable, not locally generatable, and not gossiped yet.
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
pub const METADATA_PROJECTION_VERSION: u16 = 1;
pub const MAX_METADATA_XATTR_BYTES: usize = 2_048;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataProjectionV1 {
    pub schema_version: u16,
    pub record_id: FileRecordId,
    pub properties: fs_protocol::FilePropertiesV1,
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
}

/// Bounded workspace-root catalog of every record visible through this mount.
/// Paths are mutable provenance; consumers activate by stable `record_id` and
/// resolve the latest path from a fresh projection.
pub const RECORD_INDEX_XATTR: &str = "user.tilo.record-index";
pub const RECORD_INDEX_PROJECTION_VERSION: u16 = 1;
pub const MAX_RECORD_INDEX_ENTRIES: usize = 256;
/// Stay below Linux's usual 64 KiB xattr ceiling, including filesystem framing.
pub const MAX_RECORD_INDEX_XATTR_BYTES: usize = 60 * 1024;

/// Revisioned, pageable successor to [`RECORD_INDEX_XATTR`]. The manifest is
/// deliberately tiny; pages are requested as
/// `user.tilo.record-index.v2.<revision>.<offset>`.  A stale revision is absent
/// rather than silently serving a different ordering.
pub const RECORD_INDEX_V2_XATTR: &str = "user.tilo.record-index.v2";
pub const RECORD_INDEX_V3_XATTR: &str = "user.tilo.record-index.v3";
/// Exact record resolution is separate from pagination so a late resource can
/// be reopened without materialising a mount catalogue.
pub const RECORD_INDEX_BY_ID_XATTR_PREFIX: &str = "user.tilo.record-index.by-id.";
pub const RECORD_INDEX_V2_VERSION: u16 = 2;
/// Keep a worst-case ordinary page within a single Linux filesystem xattr
/// block. Some supported hosts (including ext4-backed containers) reject
/// larger values with `ENOSPC` even when the volume has free space.
pub const RECORD_INDEX_PAGE_ENTRIES: usize = 32;
pub const MAX_RECORD_INDEX_PAGE_BYTES: usize = 48 * 1024;

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

impl RecordIndexManifestV3 {
    /// # Errors
    ///
    /// Returns an error when an opaque authority manifest is malformed.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != 3
            || self.revision.len() != 64
            || !self.revision.bytes().all(|byte| byte.is_ascii_hexdigit())
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
        if self.schema_version != 3
            || self.revision.len() != 64
            || !self.revision.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.records.len() > RECORD_INDEX_PAGE_ENTRIES
            || self
                .next_cursor
                .as_ref()
                .is_some_and(|cursor| cursor.len() != 36)
        {
            return Err("invalid opaque record-index page");
        }
        let mut previous = None;
        for record in &self.records {
            if !record.path.starts_with('/')
                || record.path.contains('\0')
                || previous.is_some_and(|value| value >= record.record_id)
            {
                return Err("invalid opaque record-index entry");
            }
            previous = Some(record.record_id);
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
            || self.revision.len() != 64
            || !self.revision.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !self.record.path.starts_with('/')
            || self.record.path.contains('\0')
        {
            return Err("invalid exact record-index projection");
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
            || self.revision.len() != 64
            || !self.revision.bytes().all(|byte| byte.is_ascii_hexdigit())
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
            || self.revision.len() != 64
            || !self.revision.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.records.len() > RECORD_INDEX_PAGE_ENTRIES
            || !self.offset.is_multiple_of(RECORD_INDEX_PAGE_ENTRIES)
        {
            return Err("invalid record-index page");
        }
        let mut previous = None;
        for record in &self.records {
            if !record.path.starts_with('/') || record.path.contains('\0') {
                return Err("record-index path is not an absolute mount path");
            }
            if previous.is_some_and(|value| value >= record.record_id) {
                return Err("record-index identities are not canonical and unique");
            }
            previous = Some(record.record_id);
        }
        if self.next_offset.is_some_and(|next| {
            next <= self.offset || !next.is_multiple_of(RECORD_INDEX_PAGE_ENTRIES)
        }) {
            return Err("record-index page continuation is invalid");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordIndexProjectionV1 {
    pub schema_version: u16,
    pub truncated: bool,
    pub records: Vec<RecordIndexEntryV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordIndexEntryV1 {
    pub record_id: FileRecordId,
    /// Absolute path within this mount, never global authority.
    pub path: String,
    pub is_dir: bool,
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
        let mut previous = None;
        for record in &self.records {
            if !record.path.starts_with('/') || record.path.contains('\0') {
                return Err("record-index path is not an absolute mount path");
            }
            if previous.is_some_and(|value| value >= record.record_id) {
                return Err("record-index identities are not canonical and unique");
            }
            previous = Some(record.record_id);
        }
        Ok(())
    }
}

/// Versioned availability snapshot for Files. Unlike [`SYNC_XATTR`], this
/// distinguishes transfer, retention policy, replica evidence, and freshness.
pub const AVAILABILITY_XATTR: &str = "user.tilo.availability";
/// Writable local-retention policy: `pinned` or `automatic`.
pub const PIN_XATTR: &str = "user.tilo.pin";
/// Write-only availability action. The only currently accepted value is
/// `retry`, which schedules a bounded background hydration of the current
/// content. No bearer, peer address, or path is carried in this control.
pub const AVAILABILITY_CONTROL_XATTR: &str = "user.tilo.availability-control";

/// Versioned, redacted record-sharing state sourced from [`crate::ShareAuthority`].
pub const SHARE_XATTR: &str = "user.tilo.share";

/// Absolute owner-only Unix socket used by Files for response-bearing share
/// creation/revocation. This is only a locator, never a bearer credential.
pub const SHARE_CONTROL_XATTR: &str = "user.tilo.share-control";

/// Version of the JSON value carried by [`AVAILABILITY_XATTR`].
pub const AVAILABILITY_PROJECTION_VERSION: u16 = 1;

/// Hard input/output bound for the availability xattr. FUSE can construct the
/// projection entirely from already-loaded state; parsing an untrusted xattr
/// cannot allocate in proportion to an arbitrary value.
pub const MAX_AVAILABILITY_XATTR_BYTES: usize = 2_048;

/// Bound on a user-visible transfer diagnostic in the projection. The durable
/// registry may retain a longer error; the xattr indicates when it truncated.
pub const MAX_PROJECTED_ERROR_BYTES: usize = 512;

/// Bounded, versioned availability value projected through an xattr.
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
    /// Effective policy. `QuickFS` should pass the ancestry-resolved policy when
    /// projecting a descendant of a pinned directory.
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

#[derive(Debug, Error)]
pub enum AvailabilityProjectionError {
    #[error("availability projection exceeds the {MAX_AVAILABILITY_XATTR_BYTES}-byte bound")]
    TooLarge,
    #[error("availability projection is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported availability projection version {0}")]
    UnsupportedVersion(u16),
    #[error("availability transfer error exceeds its wire bound")]
    ErrorTooLong,
    #[error("availability transfer progress exceeds its declared total")]
    InvalidProgress,
    #[error("availability transfer error fields are inconsistent")]
    InvalidTransferError,
    #[error("known replica counts lack current-version acknowledgement evidence")]
    InvalidReplicaEvidence,
}

impl AvailabilityProjectionV1 {
    /// Project a record using its direct policy. Call
    /// [`Self::from_record_with_policy`] when directory-pin inheritance has
    /// already been resolved by the registry.
    pub fn from_record(record: &RecordAvailability) -> Self {
        Self::from_record_with_policy(record, record.policy)
    }

    /// Pure in-memory conversion suitable for a FUSE `getxattr` callback.
    pub fn from_record_with_policy(
        record: &RecordAvailability,
        effective_policy: AvailabilityPolicy,
    ) -> Self {
        let (last_error, error_truncated, last_error_at_ms) = record
            .transfer
            .last_error
            .as_ref()
            .map_or((None, false, None), |error| {
                let (message, truncated) = truncate_utf8(&error.message, MAX_PROJECTED_ERROR_BYTES);
                (Some(message), truncated, Some(error.occurred_at_ms))
            });

        // Counts without an observation time, or for any version other than
        // the current one, are not current evidence. Preserve the last replica
        // observation timestamp separately, but report the counts as unknown.
        let replica_evidence_is_current = record.current_version.is_some()
            && record.replicas.acknowledged_version == record.current_version
            && record.freshness.replicas_observed_at_ms.is_some();
        let replicas = if replica_evidence_is_current {
            ProjectedReplicaEvidence::Known {
                reachable: record.replicas.reachable,
                durable: record.replicas.durable,
            }
        } else {
            ProjectedReplicaEvidence::Unknown
        };

        Self {
            schema_version: AVAILABILITY_PROJECTION_VERSION,
            record_id: record.record_id,
            current_version: record.current_version,
            residency: record.residency,
            transfer: ProjectedTransfer {
                phase: record.transfer.phase,
                progress: record.transfer.progress,
                last_error,
                last_error_at_ms,
                error_truncated,
            },
            policy: if record.policy == AvailabilityPolicy::Pinned {
                AvailabilityPolicy::Pinned
            } else {
                effective_policy
            },
            policy_inherited: record.policy != AvailabilityPolicy::Pinned
                && effective_policy == AvailabilityPolicy::Pinned,
            replicas,
            freshness: ProjectedFreshness {
                metadata_observed_at_ms: record.freshness.metadata_observed_at_ms,
                local_verified_at_ms: record.freshness.local_verified_at_ms,
                replicas_observed_at_ms: record.freshness.replicas_observed_at_ms,
            },
        }
    }

    /// Serialize the projection for [`AVAILABILITY_XATTR`].
    ///
    /// # Errors
    ///
    /// Returns an error when validation, serialization, or size bounds fail.
    pub fn to_xattr_bytes(&self) -> Result<Vec<u8>, AvailabilityProjectionError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_AVAILABILITY_XATTR_BYTES {
            return Err(AvailabilityProjectionError::TooLarge);
        }
        Ok(bytes)
    }

    /// Parse and validate a value read from [`AVAILABILITY_XATTR`].
    ///
    /// # Errors
    ///
    /// Returns an error when decoding, validation, or size bounds fail.
    pub fn parse_xattr(bytes: &[u8]) -> Result<Self, AvailabilityProjectionError> {
        if bytes.len() > MAX_AVAILABILITY_XATTR_BYTES {
            return Err(AvailabilityProjectionError::TooLarge);
        }
        let projection: Self = serde_json::from_slice(bytes)?;
        projection.validate()?;
        Ok(projection)
    }

    fn validate(&self) -> Result<(), AvailabilityProjectionError> {
        if self.schema_version != AVAILABILITY_PROJECTION_VERSION {
            return Err(AvailabilityProjectionError::UnsupportedVersion(
                self.schema_version,
            ));
        }
        if self
            .transfer
            .last_error
            .as_ref()
            .is_some_and(|message| message.len() > MAX_PROJECTED_ERROR_BYTES)
        {
            return Err(AvailabilityProjectionError::ErrorTooLong);
        }
        if self.transfer.progress.is_some_and(|progress| {
            progress
                .total_bytes
                .is_some_and(|total| progress.completed_bytes > total)
        }) {
            return Err(AvailabilityProjectionError::InvalidProgress);
        }
        if self.transfer.last_error.is_some() != self.transfer.last_error_at_ms.is_some()
            || (self.transfer.last_error.is_none() && self.transfer.error_truncated)
        {
            return Err(AvailabilityProjectionError::InvalidTransferError);
        }
        if matches!(self.replicas, ProjectedReplicaEvidence::Known { .. })
            && (self.current_version.is_none() || self.freshness.replicas_observed_at_ms.is_none())
        {
            return Err(AvailabilityProjectionError::InvalidReplicaEvidence);
        }
        Ok(())
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let end = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= max_bytes)
        .last()
        .unwrap_or(0);
    (value.get(..end).unwrap_or_default().to_owned(), true)
}

/// The hydration state of one filesystem entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// Fully present locally — a read never touches the network. Directories, empty files, and CRDT documents are always `Local`.
    Local,
    /// Metadata is projected but some content is not yet local; reading it will hydrate the missing bytes from a peer.
    Projected,
    /// A hydration fetch for this file's content is in flight right now.
    Downloading,
}

impl SyncState {
    /// The stable wire string carried in the `user.tilo.sync` xattr.
    pub fn as_str(self) -> &'static str {
        match self {
            SyncState::Local => "local",
            SyncState::Projected => "projected",
            SyncState::Downloading => "downloading",
        }
    }

    /// Parse the xattr string back into a state (`None` for anything else).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "local" => Some(SyncState::Local),
            "projected" => Some(SyncState::Projected),
            "downloading" => Some(SyncState::Downloading),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_state_projections_round_trip_every_dimension() {
        // Phase `rich_metadata_projection_is_small_and_self_validating`.
        {
            let projection = MetadataProjectionV1 {
                schema_version: METADATA_PROJECTION_VERSION,
                record_id: FileRecordId::new(),
                properties: fs_protocol::FilePropertiesV1 {
                    schema_version: fs_protocol::files::FILE_PROPERTIES_VERSION,
                    image_width: None,
                    image_height: None,
                    duration_ms: None,
                    page_count: Some(12),
                    codec: Some("pdf".into()),
                    container: Some("pdf".into()),
                },
            };
            projection.validate().unwrap();
            assert!(serde_json::to_vec(&projection).unwrap().len() <= MAX_METADATA_XATTR_BYTES);
        }
        // Phase `state_strings_round_trip`.
        {
            for s in [
                SyncState::Local,
                SyncState::Projected,
                SyncState::Downloading,
            ] {
                assert_eq!(SyncState::parse(s.as_str()), Some(s), "round-trip {s:?}");
            }
            assert_eq!(SyncState::parse("bogus"), None);
            // The xattr name is the documented, stable contract with the app.
            assert_eq!(SYNC_XATTR, "user.tilo.sync");
            assert_eq!(SHARE_XATTR, "user.tilo.share");
        }
        // Phase `availability_projection_round_trips_every_ui_dimension`.
        {
            let record = rich_record();
            let projection = AvailabilityProjectionV1::from_record_with_policy(
                &record,
                AvailabilityPolicy::Pinned,
            );
            let bytes = projection.to_xattr_bytes().unwrap();

            assert!(bytes.len() <= MAX_AVAILABILITY_XATTR_BYTES);
            assert_eq!(
                AvailabilityProjectionV1::parse_xattr(&bytes).unwrap(),
                projection
            );
            assert_eq!(projection.schema_version, AVAILABILITY_PROJECTION_VERSION);
            assert_eq!(projection.record_id, record.record_id);
            assert_eq!(projection.residency, record.residency);
            assert_eq!(projection.transfer.phase, TransferPhase::Downloading);
            assert_eq!(projection.policy, AvailabilityPolicy::Pinned);
            assert_eq!(
                projection.replicas,
                ProjectedReplicaEvidence::Known {
                    reachable: 2,
                    durable: 1
                }
            );
            assert_eq!(projection.freshness.replicas_observed_at_ms, Some(7_900));
            assert_eq!(AVAILABILITY_XATTR, "user.tilo.availability");
        }
        // Phase `replica_counts_are_unknown_without_current_version_evidence`.
        {
            let mut record = rich_record();
            let old_version = FileVersionId::new();
            record.replicas.acknowledged_version = Some(old_version);
            let projection = AvailabilityProjectionV1::from_record(&record);
            assert_eq!(projection.replicas, ProjectedReplicaEvidence::Unknown);

            record.replicas.acknowledged_version = record.current_version;
            record.freshness.replicas_observed_at_ms = None;
            let projection = AvailabilityProjectionV1::from_record(&record);
            assert_eq!(projection.replicas, ProjectedReplicaEvidence::Unknown);

            // Unknown is distinct from a valid observation that found zero peers.
            record.freshness.replicas_observed_at_ms = Some(9_000);
            record.replicas.reachable = 0;
            record.replicas.durable = 0;
            let projection = AvailabilityProjectionV1::from_record(&record);
            assert_eq!(
                projection.replicas,
                ProjectedReplicaEvidence::Known {
                    reachable: 0,
                    durable: 0
                }
            );
        }
    }
    use crate::availability::{
        Freshness, RecordKind, ReplicaEvidence, TransferError, TransferState,
    };

    fn rich_record() -> RecordAvailability {
        let id = FileRecordId::new();
        let version = FileVersionId::new();
        RecordAvailability {
            record_id: id,
            kind: RecordKind::File,
            parent: None,
            current_version: Some(version),
            residency: Residency::Partial {
                resident_bytes: 128,
            },
            transfer: TransferState {
                phase: TransferPhase::Downloading,
                progress: Some(TransferProgress {
                    completed_bytes: 128,
                    total_bytes: Some(1_024),
                }),
                last_error: Some(TransferError {
                    message: "peer became unavailable".into(),
                    occurred_at_ms: 8_000,
                }),
                retry_count: 1,
                retry_at_ms: Some(9_000),
            },
            policy: AvailabilityPolicy::Automatic,
            replicas: ReplicaEvidence {
                reachable: 2,
                durable: 1,
                acknowledged_version: Some(version),
            },
            replica_observations: Vec::new(),
            freshness: Freshness {
                metadata_observed_at_ms: Some(7_000),
                local_verified_at_ms: Some(7_500),
                replicas_observed_at_ms: Some(7_900),
            },
            retained_versions: Vec::new(),
        }
    }

    #[test]
    fn sync_state_parsing_bounds_errors_and_rejects_inconsistent_values() {
        // Phase `long_utf8_errors_are_truncated_safely_and_marked`.
        {
            let mut record = rich_record();
            record.transfer.last_error.as_mut().unwrap().message = "é".repeat(300);
            let projection = AvailabilityProjectionV1::from_record(&record);
            let message = projection.transfer.last_error.as_deref().unwrap();
            assert!(message.len() <= MAX_PROJECTED_ERROR_BYTES);
            assert!(message.is_char_boundary(message.len()));
            assert!(projection.transfer.error_truncated);
            assert!(projection.to_xattr_bytes().unwrap().len() <= MAX_AVAILABILITY_XATTR_BYTES);
        }
        // Phase `parser_rejects_oversize_future_and_inconsistent_values`.
        {
            assert!(matches!(
                AvailabilityProjectionV1::parse_xattr(&vec![
                    b' ';
                    MAX_AVAILABILITY_XATTR_BYTES + 1
                ]),
                Err(AvailabilityProjectionError::TooLarge)
            ));

            let mut projection = AvailabilityProjectionV1::from_record(&rich_record());
            projection.schema_version = AVAILABILITY_PROJECTION_VERSION + 1;
            let bytes = serde_json::to_vec(&projection).unwrap();
            assert!(matches!(
                AvailabilityProjectionV1::parse_xattr(&bytes),
                Err(AvailabilityProjectionError::UnsupportedVersion(_))
            ));

            let mut projection = AvailabilityProjectionV1::from_record(&rich_record());
            projection.transfer.progress = Some(TransferProgress {
                completed_bytes: 2,
                total_bytes: Some(1),
            });
            let bytes = serde_json::to_vec(&projection).unwrap();
            assert!(matches!(
                AvailabilityProjectionV1::parse_xattr(&bytes),
                Err(AvailabilityProjectionError::InvalidProgress)
            ));

            let mut projection = AvailabilityProjectionV1::from_record(&rich_record());
            projection.transfer.last_error = None;
            let bytes = serde_json::to_vec(&projection).unwrap();
            assert!(matches!(
                AvailabilityProjectionV1::parse_xattr(&bytes),
                Err(AvailabilityProjectionError::InvalidTransferError)
            ));

            let mut projection = AvailabilityProjectionV1::from_record(&rich_record());
            projection.current_version = None;
            let bytes = serde_json::to_vec(&projection).unwrap();
            assert!(matches!(
                AvailabilityProjectionV1::parse_xattr(&bytes),
                Err(AvailabilityProjectionError::InvalidReplicaEvidence)
            ));
        }
    }
}
