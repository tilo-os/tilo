//! Durable local availability policy for collaborative file records.
//!
//! Availability is deliberately keyed by [`FileRecordId`], never by a path:
//! paths are mutable presentation metadata, while pin and eviction decisions
//! must survive renames and moves.  This module records policy and evidence;
//! the content store remains responsible for retaining or deleting bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use collab_types::{Ed25519PublicKey, Signature, verify_event};
use fs_protocol::{ContentHash, FileActor, FileRecordId, FileVersionId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// On-disk schema written by [`AvailabilityRegistry`].
pub const AVAILABILITY_SCHEMA_VERSION: u16 = 2;
pub const MAX_REPLICA_OBSERVATIONS_PER_RECORD: usize = 64;

/// One cryptographically verified receipt from a distinct certified device.
/// The signed fields are retained so restart never turns an aggregate counter
/// into evidence whose origin can no longer be audited.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplicaObservation {
    pub protocol_version: u16,
    pub workspace_id: uuid::Uuid,
    pub record_id: FileRecordId,
    pub person: Ed25519PublicKey,
    pub device: Ed25519PublicKey,
    pub version_id: FileVersionId,
    pub content_hash: Option<ContentHash>,
    pub byte_len: u64,
    pub metadata_hash: ContentHash,
    pub nonce: [u8; 32],
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub observed_at_ms: u64,
    pub signature: Signature,
}

#[derive(Serialize)]
struct ReplicaObservationView<'a> {
    domain: &'static str,
    protocol_version: u16,
    workspace_id: uuid::Uuid,
    revision: &'a fs_protocol::ContentRevisionV1,
    nonce: [u8; 32],
    actor: FileActor,
    issued_at_ms: u64,
    expires_at_ms: u64,
}

impl ReplicaObservation {
    fn verify_persisted(&self) -> bool {
        let revision = fs_protocol::ContentRevisionV1 {
            schema_version: fs_protocol::CONTENT_REVISION_VERSION,
            record_id: self.record_id,
            version_id: self.version_id,
            content_hash: self.content_hash,
            byte_len: self.byte_len,
            metadata_hash: self.metadata_hash,
        };
        let view = ReplicaObservationView {
            domain: crate::replica_ack::SIGNATURE_DOMAIN,
            protocol_version: self.protocol_version,
            workspace_id: self.workspace_id,
            revision: &revision,
            nonce: self.nonce,
            actor: FileActor {
                person: self.person,
                device: self.device,
            },
            issued_at_ms: self.issued_at_ms,
            expires_at_ms: self.expires_at_ms,
        };
        self.protocol_version == crate::replica_ack::REPLICA_ACK_PROTOCOL_VERSION
            && !self.workspace_id.is_nil()
            && self.nonce != [0; 32]
            && verify_event(&view, &self.signature, &self.device).is_ok()
    }
}

/// Whether the record represents a directory or a file-like object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    File,
    Directory,
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

/// Last transfer failure. It is retained separately from the current phase so
/// a retry can show both live progress and the previous diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferError {
    pub message: String,
    pub occurred_at_ms: u64,
}

/// Transfer state shown by Files and maintained by hydration/replication.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferState {
    pub phase: TransferPhase,
    pub progress: Option<TransferProgress>,
    pub last_error: Option<TransferError>,
    /// Consecutive automatic hydration failures for the current version.
    /// Persisting this prevents a restart from turning an unavailable peer
    /// into a tight retry loop.
    #[serde(default)]
    pub retry_count: u32,
    /// Absolute wall-clock deadline for the next automatic attempt. An
    /// explicit user retry clears this field.
    #[serde(default)]
    pub retry_at_ms: Option<u64>,
}

/// Explicit local retention policy. Directory policy applies transitively.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityPolicy {
    #[default]
    Automatic,
    Pinned,
}

/// Replica evidence for exactly one immutable version of this record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaEvidence {
    /// Peers that recently acknowledged possessing the complete version and
    /// were reachable when the observation was made. The replication layer
    /// must count only authenticated, authorized, non-revoked replicas.
    pub reachable: u32,
    /// Recently acknowledged durable replicas (which may later be offline),
    /// after applying the workspace's acceptable-host policy.
    pub durable: u32,
    /// The version to which both counts apply. `None` is never eviction proof.
    pub acknowledged_version: Option<FileVersionId>,
}

/// Freshness timestamps kept distinct because metadata, local bytes, and
/// replica observations advance independently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Freshness {
    pub metadata_observed_at_ms: Option<u64>,
    pub local_verified_at_ms: Option<u64>,
    pub replicas_observed_at_ms: Option<u64>,
}

/// Durable availability state for one stable collaborative-file record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordAvailability {
    pub record_id: FileRecordId,
    pub kind: RecordKind,
    /// Stable parent record, not a path. This makes directory pins apply to
    /// descendants created after the directory was pinned.
    pub parent: Option<FileRecordId>,
    /// Current immutable version. Replica evidence must name this exact value.
    pub current_version: Option<FileVersionId>,
    pub residency: Residency,
    pub transfer: TransferState,
    pub policy: AvailabilityPolicy,
    pub replicas: ReplicaEvidence,
    /// Verified receipts keyed semantically by certified device.
    pub replica_observations: Vec<ReplicaObservation>,
    pub freshness: Freshness,
    /// Historical versions intentionally retained only on this device. IDs,
    /// rather than a count, let history/GC remove the precise retention reason.
    pub retained_versions: Vec<FileVersionId>,
}

impl RecordAvailability {
    pub fn new(record_id: FileRecordId, kind: RecordKind) -> Self {
        Self {
            record_id,
            kind,
            parent: None,
            current_version: None,
            residency: Residency::NotResident,
            transfer: TransferState::default(),
            policy: AvailabilityPolicy::Automatic,
            replicas: ReplicaEvidence::default(),
            replica_observations: Vec::new(),
            freshness: Freshness::default(),
            retained_versions: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryDocument {
    schema_version: u16,
    records: Vec<RecordAvailability>,
}

/// Replica requirements used for a normal, non-destructive eviction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvictionRequirements {
    pub minimum_reachable: u32,
    pub minimum_durable: u32,
    /// Maximum age of the explicit replica acknowledgement.
    pub maximum_replica_age_ms: u64,
}

impl Default for EvictionRequirements {
    fn default() -> Self {
        Self {
            minimum_reachable: 0,
            minimum_durable: 1,
            maximum_replica_age_ms: 5 * 60 * 1000,
        }
    }
}

/// Why a payload may or may not be removed from the local content store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvictionDecision {
    Allowed,
    /// Caller explicitly accepted permanent data loss. UI must present this as
    /// destructive rather than as ordinary cache eviction.
    AllowedDestructiveOverride,
    Denied(EvictionBlocker),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvictionBlocker {
    UnknownRecord,
    ActiveTransfer,
    Pinned,
    RetainedVersions,
    CurrentVersionUnknown,
    ReplicaVersionMismatch,
    ReplicaAcknowledgementMissing,
    ReplicaAcknowledgementFromFuture,
    ReplicaAcknowledgementStale,
    InsufficientReachableReplicas,
    InsufficientDurableReplicas,
    InvalidAncestry,
}

#[derive(Debug, Error)]
pub enum AvailabilityError {
    #[error("availability registry I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("availability registry is malformed: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("unsupported availability schema version {0}")]
    UnsupportedVersion(u16),
    #[error("availability registry contains duplicate record {0:?}")]
    DuplicateRecord(FileRecordId),
    #[error("record {record:?} refers to missing parent {parent:?}")]
    MissingParent {
        record: FileRecordId,
        parent: FileRecordId,
    },
    #[error("record {record:?} has non-directory parent {parent:?}")]
    ParentNotDirectory {
        record: FileRecordId,
        parent: FileRecordId,
    },
    #[error("record ancestry contains a cycle at {0:?}")]
    AncestryCycle(FileRecordId),
    #[error("transfer progress exceeds its declared total")]
    InvalidTransferProgress,
    #[error("replica observation refers to an unknown record {0:?}")]
    UnknownRecord(FileRecordId),
    #[error("the local device cannot count itself as a remote replica")]
    SelfReplicaObservation,
    #[error("replica observation is stale, expired, or from the future")]
    StaleReplicaObservation,
    #[error("replica observation does not name the record's current version")]
    ReplicaVersionMismatch,
    #[error("replica observation replays or predates that device's last receipt")]
    ReplicaReplay,
    #[error("record exceeds {MAX_REPLICA_OBSERVATIONS_PER_RECORD} replica observations")]
    TooManyReplicaObservations,
    #[error("current content revision is invalid")]
    InvalidCurrentRevision,
}

/// Crash-safe per-device registry for local availability state.
pub struct AvailabilityRegistry {
    path: PathBuf,
    records: BTreeMap<FileRecordId, RecordAvailability>,
    /// Inside [`Self::batch`]: mutations are validated and persisted once,
    /// when the batch ends.
    deferred: bool,
    /// Bumped by every commit that changes what pinned hydration acts on;
    /// see [`Self::hydration_generation`].
    hydration_generation: u64,
    /// Announces each bump of `hydration_generation`, so the pinned
    /// hydration worker waits for one instead of polling.
    hydration_watch: tokio::sync::watch::Sender<u64>,
    #[cfg(test)]
    persists: std::sync::atomic::AtomicU64,
}

/// The part of a record pinned hydration reacts to. Transfer bookkeeping,
/// replica evidence and freshness stamps are deliberately absent: they are
/// either written by the hydration worker itself or never change its plan.
fn hydration_view(
    record: &RecordAvailability,
) -> (
    AvailabilityPolicy,
    RecordKind,
    Option<FileRecordId>,
    Option<FileVersionId>,
    Residency,
) {
    (
        record.policy,
        record.kind,
        record.parent,
        record.current_version,
        record.residency,
    )
}

impl AvailabilityRegistry {
    /// Open an existing registry or create an empty in-memory registry when
    /// the file is absent. The first mutation creates parent directories.
    ///
    /// # Errors
    ///
    /// Returns an error when the registry cannot be read, decoded, or fully
    /// validated.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, AvailabilityError> {
        let path = path.into();
        if !path.exists() {
            return Ok(Self {
                path,
                records: BTreeMap::new(),
                deferred: false,
                hydration_generation: 0,
                hydration_watch: tokio::sync::watch::Sender::new(0),
                #[cfg(test)]
                persists: std::sync::atomic::AtomicU64::new(0),
            });
        }

        let document: RegistryDocument = serde_json::from_slice(&fs::read(&path)?)?;
        if document.schema_version != AVAILABILITY_SCHEMA_VERSION {
            return Err(AvailabilityError::UnsupportedVersion(
                document.schema_version,
            ));
        }
        let mut records = BTreeMap::new();
        for record in document.records {
            let id = record.record_id;
            if records.insert(id, record).is_some() {
                return Err(AvailabilityError::DuplicateRecord(id));
            }
        }
        validate_records(&records)?;
        Ok(Self {
            path,
            records,
            deferred: false,
            hydration_generation: 0,
            hydration_watch: tokio::sync::watch::Sender::new(0),
            #[cfg(test)]
            persists: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub fn get(&self, record_id: FileRecordId) -> Option<&RecordAvailability> {
        self.records.get(&record_id)
    }

    pub fn records(&self) -> impl Iterator<Item = &RecordAvailability> {
        self.records.values()
    }

    /// Changes whenever a commit alters a record's policy, kind, parent,
    /// current version or residency, or adds a record. Transfer progress,
    /// replica receipts and freshness stamps leave it alone, so a pinned
    /// hydration worker can compare it to learn, without walking the
    /// namespace, that nothing it acts on changed since its last pass.
    #[must_use]
    pub fn hydration_generation(&self) -> u64 {
        self.hydration_generation
    }

    /// A receiver that sees every later change of
    /// [`Self::hydration_generation`]: a pin, an unpin, a new version or a
    /// residency change, from any writer of this registry.
    #[must_use]
    pub fn subscribe_hydration(&self) -> tokio::sync::watch::Receiver<u64> {
        self.hydration_watch.subscribe()
    }

    fn bump_hydration_generation(&mut self) {
        self.hydration_generation = self.hydration_generation.wrapping_add(1);
        self.hydration_watch.send_replace(self.hydration_generation);
    }

    /// Durable writes of the whole registry so far.
    #[cfg(test)]
    #[must_use]
    pub fn persist_count(&self) -> u64 {
        self.persists.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Whether any record explicitly promises offline residency. Descendants
    /// inherit that promise, so one explicit row is enough to require a
    /// namespace reconciliation; with none, the periodic worker can stay
    /// idle without walking and sorting the complete workspace.
    pub fn has_pinned_policy(&self) -> bool {
        self.records
            .values()
            .any(|record| record.policy == AvailabilityPolicy::Pinned)
    }

    /// Cancel durable automatic-hydration state below a pin that was removed.
    /// The explicit unpin action pays this cost once, allowing the periodic
    /// worker to take the zero-scan fast path whenever no pins remain.
    ///
    /// # Errors
    ///
    /// Returns an error if clearing the durable transfer state cannot persist.
    pub fn clear_automatic_transfer_subtree(
        &mut self,
        root: FileRecordId,
    ) -> Result<bool, AvailabilityError> {
        let is_in_subtree = |record_id: FileRecordId| {
            let mut cursor = Some(record_id);
            let mut remaining = self.records.len().saturating_add(1);
            while let Some(id) = cursor {
                if id == root {
                    return true;
                }
                if remaining == 0 {
                    return false;
                }
                remaining = remaining.saturating_sub(1);
                cursor = self.records.get(&id).and_then(|record| record.parent);
            }
            false
        };
        let ids = self
            .records
            .values()
            .filter(|record| {
                is_in_subtree(record.record_id)
                    && matches!(
                        self.effective_policy(record.record_id),
                        Ok(Some(AvailabilityPolicy::Automatic))
                    )
                    && record.transfer != TransferState::default()
            })
            .map(|record| record.record_id)
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(false);
        }
        let previous = self.records.clone();
        for id in ids {
            if let Some(record) = self.records.get_mut(&id) {
                record.transfer = TransferState::default();
            }
        }
        if let Err(error) = self.persist() {
            self.records = previous;
            return Err(error);
        }
        Ok(true)
    }

    /// Install the authenticated current revision before accepting replica
    /// evidence for it. Ordinary whole-workspace records use the same durable
    /// availability path as record-scoped shares; without this row a valid NAS
    /// acknowledgement would be discarded as `UnknownRecord`.
    ///
    /// # Errors
    ///
    /// Returns an error when the supplied revision is invalid or state cannot persist.
    pub fn track_current_revision(
        &mut self,
        revision: fs_protocol::ContentRevisionV1,
        kind: RecordKind,
        parent: Option<FileRecordId>,
        observed_at_ms: u64,
    ) -> Result<bool, AvailabilityError> {
        revision
            .validate()
            .map_err(|_| AvailabilityError::InvalidCurrentRevision)?;
        let mut record = self
            .records
            .get(&revision.record_id)
            .cloned()
            .unwrap_or_else(|| RecordAvailability::new(revision.record_id, kind));
        let version_changed = record.current_version != Some(revision.version_id);
        let metadata_changed = record.kind != kind
            || record.parent != parent
            || record.freshness.metadata_observed_at_ms != Some(observed_at_ms);
        if !version_changed && !metadata_changed {
            return Ok(false);
        }
        record.kind = kind;
        record.parent = parent;
        record.freshness.metadata_observed_at_ms = Some(observed_at_ms);
        if version_changed {
            record.current_version = Some(revision.version_id);
            record.residency = Residency::NotResident;
            record.transfer = TransferState::default();
            record.replicas = ReplicaEvidence::default();
            record.replica_observations.clear();
            record.freshness.local_verified_at_ms = None;
            record.freshness.replicas_observed_at_ms = None;
        }
        self.put(record)?;
        Ok(true)
    }

    /// Runs `apply`'s mutations and commits them with one validation and one
    /// durable write of the registry, instead of one of each per mutation:
    /// receipts for a thousand records cost one write, not a thousand
    /// rewrites of the whole registry. When validation or persistence of the
    /// batch fails, memory and disk are left exactly as before it.
    ///
    /// # Errors
    ///
    /// Returns an error when the resulting registry is invalid or cannot be
    /// persisted.
    pub fn batch<T>(&mut self, apply: impl FnOnce(&mut Self) -> T) -> Result<T, AvailabilityError> {
        let before = self.records.clone();
        self.deferred = true;
        let value = apply(self);
        self.deferred = false;
        // A batch that changed nothing has nothing to validate or write.
        if self.records == before {
            return Ok(value);
        }
        if let Err(error) = validate_records(&self.records).and_then(|()| self.persist()) {
            self.records = before;
            return Err(error);
        }
        let hydration_changed = before.len() != self.records.len()
            || self.records.iter().any(|(id, record)| {
                before
                    .get(id)
                    .is_none_or(|previous| hydration_view(previous) != hydration_view(record))
            });
        if hydration_changed {
            self.bump_hydration_generation();
        }
        Ok(value)
    }

    /// Insert or replace a record and durably commit the complete registry.
    /// Invalid ancestry leaves both memory and disk unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error when the candidate registry is invalid or its atomic
    /// persistence fails.
    pub fn put(&mut self, record: RecordAvailability) -> Result<(), AvailabilityError> {
        let id = record.record_id;
        let previous = self.records.insert(id, record);
        if self.deferred {
            return Ok(());
        }
        if let Err(error) = validate_records(&self.records) {
            if let Some(previous) = previous {
                self.records.insert(id, previous);
            } else {
                self.records.remove(&id);
            }
            return Err(error);
        }
        if let Err(error) = self.persist() {
            if let Some(previous) = previous {
                self.records.insert(id, previous);
            } else {
                self.records.remove(&id);
            }
            return Err(error);
        }
        if previous.as_ref().map(hydration_view) != self.records.get(&id).map(hydration_view) {
            self.bump_hydration_generation();
        }
        Ok(())
    }

    /// Persist a freshly verified remote receipt and rebuild the aggregate UI
    /// projection from distinct, unexpired devices. Exact replay and an older
    /// receipt from the same device fail closed.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown records, self/stale/replayed receipts,
    /// version mismatches, observation overflow, or persistence failure.
    pub fn observe_replica(
        &mut self,
        record_id: FileRecordId,
        local_device: Ed25519PublicKey,
        observation: ReplicaObservation,
        now_ms: u64,
    ) -> Result<(), AvailabilityError> {
        if observation.device == local_device {
            return Err(AvailabilityError::SelfReplicaObservation);
        }
        if observation.observed_at_ms != now_ms
            || observation.issued_at_ms > now_ms
            || observation.expires_at_ms < now_ms
        {
            return Err(AvailabilityError::StaleReplicaObservation);
        }
        let current = self
            .records
            .get(&record_id)
            .ok_or(AvailabilityError::UnknownRecord(record_id))?;
        if current.current_version != Some(observation.version_id) {
            return Err(AvailabilityError::ReplicaVersionMismatch);
        }
        if current.replica_observations.iter().any(|prior| {
            prior.device == observation.device && prior.issued_at_ms >= observation.issued_at_ms
        }) {
            return Err(AvailabilityError::ReplicaReplay);
        }
        let mut next = current.clone();
        next.replica_observations
            .retain(|prior| prior.device != observation.device && prior.expires_at_ms >= now_ms);
        if next.replica_observations.len() >= MAX_REPLICA_OBSERVATIONS_PER_RECORD {
            return Err(AvailabilityError::TooManyReplicaObservations);
        }
        next.replica_observations.push(observation);
        refresh_replica_projection(&mut next, now_ms);
        self.put(next)
    }

    /// Effective policy, including any pinned stable directory ancestor.
    ///
    /// # Errors
    ///
    /// Returns an error when ancestry contains a cycle or names a missing
    /// parent record.
    pub fn effective_policy(
        &self,
        record_id: FileRecordId,
    ) -> Result<Option<AvailabilityPolicy>, AvailabilityError> {
        let Some(mut record) = self.records.get(&record_id) else {
            return Ok(None);
        };
        let mut seen = BTreeSet::new();
        loop {
            if !seen.insert(record.record_id) {
                return Err(AvailabilityError::AncestryCycle(record.record_id));
            }
            if record.policy == AvailabilityPolicy::Pinned {
                return Ok(Some(AvailabilityPolicy::Pinned));
            }
            let Some(parent) = record.parent else {
                return Ok(Some(AvailabilityPolicy::Automatic));
            };
            record = self
                .records
                .get(&parent)
                .ok_or(AvailabilityError::MissingParent {
                    record: record.record_id,
                    parent,
                })?;
        }
    }

    /// Evaluate cache eviction using explicit, version-bound replica evidence.
    /// This does not remove bytes; the store integration must do that only
    /// after an allowed decision and then persist the new residency state.
    pub fn eviction_decision(
        &self,
        record_id: FileRecordId,
        now_ms: u64,
        requirements: EvictionRequirements,
        destructive_override: bool,
    ) -> EvictionDecision {
        if destructive_override {
            return EvictionDecision::AllowedDestructiveOverride;
        }
        let Some(record) = self.records.get(&record_id) else {
            return EvictionDecision::Denied(EvictionBlocker::UnknownRecord);
        };
        if record.transfer.phase.is_active() {
            return EvictionDecision::Denied(EvictionBlocker::ActiveTransfer);
        }
        match self.effective_policy(record_id) {
            Ok(Some(AvailabilityPolicy::Pinned)) => {
                return EvictionDecision::Denied(EvictionBlocker::Pinned);
            }
            Err(_) => return EvictionDecision::Denied(EvictionBlocker::InvalidAncestry),
            _ => {}
        }
        if !record.retained_versions.is_empty() {
            return EvictionDecision::Denied(EvictionBlocker::RetainedVersions);
        }
        let Some(version) = record.current_version else {
            return EvictionDecision::Denied(EvictionBlocker::CurrentVersionUnknown);
        };
        let valid = record
            .replica_observations
            .iter()
            .filter(|receipt| {
                receipt.version_id == version
                    && receipt.issued_at_ms <= now_ms
                    && receipt.expires_at_ms >= now_ms
            })
            .count();
        let valid = u32::try_from(valid).unwrap_or(u32::MAX);
        if record.replicas.acknowledged_version != Some(version) {
            return EvictionDecision::Denied(EvictionBlocker::ReplicaVersionMismatch);
        }
        if valid == 0 {
            return EvictionDecision::Denied(EvictionBlocker::ReplicaAcknowledgementMissing);
        }
        let Some(acknowledged_at_ms) = record.freshness.replicas_observed_at_ms else {
            return EvictionDecision::Denied(EvictionBlocker::ReplicaAcknowledgementMissing);
        };
        if acknowledged_at_ms > now_ms {
            return EvictionDecision::Denied(EvictionBlocker::ReplicaAcknowledgementFromFuture);
        }
        if now_ms.saturating_sub(acknowledged_at_ms) > requirements.maximum_replica_age_ms {
            return EvictionDecision::Denied(EvictionBlocker::ReplicaAcknowledgementStale);
        }
        if valid < requirements.minimum_reachable {
            return EvictionDecision::Denied(EvictionBlocker::InsufficientReachableReplicas);
        }
        if valid < requirements.minimum_durable {
            return EvictionDecision::Denied(EvictionBlocker::InsufficientDurableReplicas);
        }
        EvictionDecision::Allowed
    }

    fn persist(&self) -> Result<(), AvailabilityError> {
        #[cfg(test)]
        self.persists
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let document = RegistryDocument {
            schema_version: AVAILABILITY_SCHEMA_VERSION,
            records: self.records.values().cloned().collect(),
        };
        let bytes = serde_json::to_vec_pretty(&document)?;
        tilo_files::native::atomic::write_creating_parent(&self.path, &bytes)?;
        Ok(())
    }
}

/// Whether `record_id` is pinned, itself or through a stable ancestor, in a
/// captured snapshot of `records`. Missing or cyclic ancestry is invalid
/// durable state and counts as pinned, so it can never become permission to
/// release local data.
pub(crate) fn pinned_in_snapshot(records: &[RecordAvailability], record_id: FileRecordId) -> bool {
    let mut current = Some(record_id);
    let mut remaining = records.len().saturating_add(1);
    while let Some(record_id) = current {
        if remaining == 0 {
            return true;
        }
        remaining = remaining.saturating_sub(1);
        let Some(record) = records.iter().find(|record| record.record_id == record_id) else {
            return true;
        };
        if record.policy == AvailabilityPolicy::Pinned {
            return true;
        }
        current = record.parent;
    }
    false
}

fn refresh_replica_projection(record: &mut RecordAvailability, now_ms: u64) {
    let Some(version) = record.current_version else {
        record.replicas = ReplicaEvidence::default();
        record.freshness.replicas_observed_at_ms = None;
        return;
    };
    let valid = record
        .replica_observations
        .iter()
        .filter(|receipt| {
            receipt.version_id == version
                && receipt.issued_at_ms <= now_ms
                && receipt.expires_at_ms >= now_ms
        })
        .count();
    let valid = u32::try_from(valid).unwrap_or(u32::MAX);
    record.replicas = ReplicaEvidence {
        reachable: valid,
        durable: valid,
        acknowledged_version: (valid > 0).then_some(version),
    };
    record.freshness.replicas_observed_at_ms = record
        .replica_observations
        .iter()
        .filter(|receipt| receipt.version_id == version && receipt.expires_at_ms >= now_ms)
        .map(|receipt| receipt.observed_at_ms)
        .max();
}

fn validate_records(
    records: &BTreeMap<FileRecordId, RecordAvailability>,
) -> Result<(), AvailabilityError> {
    for record in records.values() {
        if record.replica_observations.len() > MAX_REPLICA_OBSERVATIONS_PER_RECORD {
            return Err(AvailabilityError::TooManyReplicaObservations);
        }
        let mut devices = BTreeSet::new();
        for observation in &record.replica_observations {
            if observation.record_id != record.record_id
                || observation.version_id.0.is_nil()
                || observation.issued_at_ms > observation.expires_at_ms
                || !devices.insert(observation.device)
                || !observation.verify_persisted()
            {
                return Err(AvailabilityError::StaleReplicaObservation);
            }
        }
        if record
            .transfer
            .progress
            .is_some_and(|p| p.total_bytes.is_some_and(|total| p.completed_bytes > total))
        {
            return Err(AvailabilityError::InvalidTransferProgress);
        }
        if let Some(parent) = record.parent {
            let parent_record = records
                .get(&parent)
                .ok_or(AvailabilityError::MissingParent {
                    record: record.record_id,
                    parent,
                })?;
            if parent_record.kind != RecordKind::Directory {
                return Err(AvailabilityError::ParentNotDirectory {
                    record: record.record_id,
                    parent,
                });
            }
        }

        let mut next = Some(record.record_id);
        let mut seen = BTreeSet::new();
        while let Some(id) = next {
            if !seen.insert(id) {
                return Err(AvailabilityError::AncestryCycle(id));
            }
            next = records.get(&id).and_then(|entry| entry.parent);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use getrandom::{SysRng, rand_core::UnwrapErr};
    use tempfile::TempDir;

    fn file(id: FileRecordId, version: FileVersionId) -> RecordAvailability {
        let mut record = RecordAvailability::new(id, RecordKind::File);
        record.current_version = Some(version);
        record.residency = Residency::Resident { bytes: 1024 };
        record
    }

    fn safe_evidence(record: &mut RecordAvailability, version: FileVersionId, now_ms: u64) {
        let device = SigningKey::generate(&mut UnwrapErr(SysRng));
        let revision = fs_protocol::ContentRevisionV1 {
            schema_version: fs_protocol::CONTENT_REVISION_VERSION,
            record_id: record.record_id,
            version_id: version,
            content_hash: Some(ContentHash([4; 32])),
            byte_len: 10,
            metadata_hash: ContentHash([5; 32]),
        };
        let request =
            crate::replica_ack::ReplicaAckRequestV1::new(uuid::Uuid::new_v4(), revision).unwrap();
        let actor = FileActor {
            person: Ed25519PublicKey(device.verifying_key().to_bytes()),
            device: Ed25519PublicKey(device.verifying_key().to_bytes()),
        };
        let receipt =
            crate::replica_ack::ReplicaAckV1::signed(&request, actor, now_ms, &device).unwrap();
        record.replicas = ReplicaEvidence {
            reachable: 1,
            durable: 1,
            acknowledged_version: Some(version),
        };
        record.freshness.replicas_observed_at_ms = Some(now_ms);
        record.replica_observations = vec![ReplicaObservation {
            protocol_version: receipt.protocol_version,
            workspace_id: receipt.workspace_id,
            record_id: receipt.revision.record_id,
            person: receipt.actor.person,
            device: receipt.actor.device,
            version_id: receipt.revision.version_id,
            content_hash: receipt.revision.content_hash,
            byte_len: receipt.revision.byte_len,
            metadata_hash: receipt.revision.metadata_hash,
            nonce: receipt.nonce,
            issued_at_ms: receipt.issued_at_ms,
            expires_at_ms: receipt.expires_at_ms,
            observed_at_ms: now_ms,
            signature: receipt.signature,
        }];
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn availability_registry_persists_by_stable_record_id_and_rejects_tampering() {
        // Phase `registry_survives_restart_and_is_keyed_by_stable_record_id`.
        {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("availability/state.json");
            let id = FileRecordId::new();
            let version = FileVersionId::new();

            let mut first = AvailabilityRegistry::open(&path).unwrap();
            let mut record = file(id, version);
            record.policy = AvailabilityPolicy::Pinned;
            record.freshness.metadata_observed_at_ms = Some(41);
            first.put(record.clone()).unwrap();
            drop(first);

            let reopened = AvailabilityRegistry::open(&path).unwrap();
            assert_eq!(reopened.get(id), Some(&record));
            assert_eq!(reopened.records().count(), 1);
        }
        // Phase `other_schema_versions_fail_registry_open`.
        {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("availability.json");
            tilo_files::native::atomic::write_creating_parent(
                &path,
                &serde_json::to_vec(&serde_json::json!({
                    "schema_version": AVAILABILITY_SCHEMA_VERSION - 1,
                    "records": []
                }))
                .unwrap(),
            )
            .unwrap();
            assert!(matches!(
                AvailabilityRegistry::open(path),
                Err(AvailabilityError::UnsupportedVersion(1))
            ));
        }
        // Phase `tampered_persisted_receipt_fails_registry_open`.
        {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("availability.json");
            let id = FileRecordId::new();
            let version = FileVersionId::new();
            let mut record = file(id, version);
            safe_evidence(&mut record, version, 10);
            let mut registry = AvailabilityRegistry::open(&path).unwrap();
            registry.put(record).unwrap();
            drop(registry);
            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            value["records"][0]["replica_observations"][0]["byte_len"] = serde_json::json!(11);
            tilo_files::native::atomic::write(&path, &serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(matches!(
                AvailabilityRegistry::open(path),
                Err(AvailabilityError::StaleReplicaObservation)
            ));
        }
        // Phase `a_rename_cannot_change_availability_identity`.
        {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("availability.json");
            let id = FileRecordId::new();
            let version = FileVersionId::new();
            let old_name = "draft.png";

            let mut registry = AvailabilityRegistry::open(&path).unwrap();
            let mut record = file(id, version);
            record.policy = AvailabilityPolicy::Pinned;
            registry.put(record).unwrap();

            // A namespace rename changes only presentation metadata. There is no
            // path/name key in this registry to migrate or accidentally duplicate.
            let new_name = "approved.png";
            assert_ne!(old_name, new_name);
            assert_eq!(
                registry.effective_policy(id).unwrap(),
                Some(AvailabilityPolicy::Pinned)
            );
            assert_eq!(registry.records().count(), 1);
        }
        // Phase `malformed_ancestry_and_progress_are_rejected_without_mutation`.
        {
            let temp = TempDir::new().unwrap();
            let mut registry = AvailabilityRegistry::open(temp.path().join("a.json")).unwrap();
            let id = FileRecordId::new();
            let mut record = file(id, FileVersionId::new());
            record.parent = Some(FileRecordId::new());
            assert!(matches!(
                registry.put(record),
                Err(AvailabilityError::MissingParent { .. })
            ));
            assert!(registry.get(id).is_none());

            let mut record = file(id, FileVersionId::new());
            record.transfer.progress = Some(TransferProgress {
                completed_bytes: 2,
                total_bytes: Some(1),
            });
            assert!(matches!(
                registry.put(record),
                Err(AvailabilityError::InvalidTransferProgress)
            ));
            assert!(registry.get(id).is_none());
        }
    }

    #[test]
    fn pins_follow_descendants_moves_and_namespace_reconciliation() {
        // Phase `directory_pin_applies_to_future_descendants_and_survives_move`.
        {
            let temp = TempDir::new().unwrap();
            let mut registry = AvailabilityRegistry::open(temp.path().join("a.json")).unwrap();
            let pinned_dir = FileRecordId::new();
            let other_dir = FileRecordId::new();
            let child = FileRecordId::new();

            let mut directory = RecordAvailability::new(pinned_dir, RecordKind::Directory);
            directory.policy = AvailabilityPolicy::Pinned;
            registry.put(directory).unwrap();
            registry
                .put(RecordAvailability::new(other_dir, RecordKind::Directory))
                .unwrap();

            // The child is created only after the directory was pinned.
            let mut child_record = file(child, FileVersionId::new());
            child_record.parent = Some(pinned_dir);
            registry.put(child_record.clone()).unwrap();
            assert_eq!(
                registry.effective_policy(child).unwrap(),
                Some(AvailabilityPolicy::Pinned)
            );

            // Stable ancestry, rather than a path prefix, controls inherited pin.
            child_record.parent = Some(other_dir);
            registry.put(child_record).unwrap();
            assert_eq!(
                registry.effective_policy(child).unwrap(),
                Some(AvailabilityPolicy::Automatic)
            );
        }
        // Phase `explicit_pin_presence_drives_periodic_namespace_reconciliation`.
        {
            let temp = TempDir::new().unwrap();
            let mut registry = AvailabilityRegistry::open(temp.path().join("a.json")).unwrap();
            assert!(!registry.has_pinned_policy());

            let id = FileRecordId::new();
            let mut record = file(id, FileVersionId::new());
            registry.put(record.clone()).unwrap();
            assert!(!registry.has_pinned_policy());

            record.policy = AvailabilityPolicy::Pinned;
            registry.put(record.clone()).unwrap();
            assert!(registry.has_pinned_policy());

            record.policy = AvailabilityPolicy::Automatic;
            registry.put(record).unwrap();
            assert!(!registry.has_pinned_policy());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn eviction_requires_a_fresh_exact_ack_unless_explicitly_overridden() {
        // Phase `safe_eviction_requires_fresh_ack_for_the_exact_current_version`.
        {
            let temp = TempDir::new().unwrap();
            let mut registry = AvailabilityRegistry::open(temp.path().join("a.json")).unwrap();
            let id = FileRecordId::new();
            let old_version = FileVersionId::new();
            let current_version = FileVersionId::new();
            let now = 50_000;
            let requirements = EvictionRequirements {
                minimum_reachable: 1,
                minimum_durable: 1,
                maximum_replica_age_ms: 1_000,
            };

            let mut record = file(id, current_version);
            safe_evidence(&mut record, old_version, now);
            registry.put(record.clone()).unwrap();
            assert_eq!(
                registry.eviction_decision(id, now, requirements, false),
                EvictionDecision::Denied(EvictionBlocker::ReplicaVersionMismatch)
            );

            safe_evidence(&mut record, current_version, now - 1_001);
            registry.put(record.clone()).unwrap();
            assert_eq!(
                registry.eviction_decision(id, now, requirements, false),
                EvictionDecision::Denied(EvictionBlocker::ReplicaAcknowledgementStale)
            );

            safe_evidence(&mut record, current_version, now);
            registry.put(record).unwrap();
            assert_eq!(
                registry.eviction_decision(id, now, requirements, false),
                EvictionDecision::Allowed
            );
        }
        // Phase `every_local_retention_reason_blocks_normal_eviction`.
        {
            let temp = TempDir::new().unwrap();
            let mut registry = AvailabilityRegistry::open(temp.path().join("a.json")).unwrap();
            let id = FileRecordId::new();
            let version = FileVersionId::new();
            let now = 100;
            let requirements = EvictionRequirements::default();
            let mut record = file(id, version);
            safe_evidence(&mut record, version, now);

            record.transfer.phase = TransferPhase::Downloading;
            registry.put(record.clone()).unwrap();
            assert_eq!(
                registry.eviction_decision(id, now, requirements, false),
                EvictionDecision::Denied(EvictionBlocker::ActiveTransfer)
            );

            record.transfer.phase = TransferPhase::Idle;
            record.policy = AvailabilityPolicy::Pinned;
            registry.put(record.clone()).unwrap();
            assert_eq!(
                registry.eviction_decision(id, now, requirements, false),
                EvictionDecision::Denied(EvictionBlocker::Pinned)
            );

            record.policy = AvailabilityPolicy::Automatic;
            record.retained_versions = vec![FileVersionId::new(), FileVersionId::new()];
            registry.put(record).unwrap();
            assert_eq!(
                registry.eviction_decision(id, now, requirements, false),
                EvictionDecision::Denied(EvictionBlocker::RetainedVersions)
            );
        }
        // Phase `normal_eviction_fails_closed_without_an_acceptable_replica_ack`.
        {
            let temp = TempDir::new().unwrap();
            let mut registry = AvailabilityRegistry::open(temp.path().join("a.json")).unwrap();
            let id = FileRecordId::new();
            let version = FileVersionId::new();
            let now = 10_000;
            let requirements = EvictionRequirements {
                minimum_reachable: 1,
                minimum_durable: 1,
                maximum_replica_age_ms: 1_000,
            };
            let mut record = file(id, version);
            record.replicas.acknowledged_version = Some(version);
            registry.put(record.clone()).unwrap();
            assert_eq!(
                registry.eviction_decision(id, now, requirements, false),
                EvictionDecision::Denied(EvictionBlocker::ReplicaAcknowledgementMissing)
            );

            // Prototype-era aggregate counters, even if internally consistent,
            // are not proof without a retained signed per-device receipt.
            record.freshness.replicas_observed_at_ms = Some(now);
            record.replicas.reachable = 1;
            record.replicas.durable = 1;
            registry.put(record).unwrap();
            assert_eq!(
                registry.eviction_decision(id, now, requirements, false),
                EvictionDecision::Denied(EvictionBlocker::ReplicaAcknowledgementMissing)
            );
        }
        // Phase `destructive_override_is_explicit_and_bypasses_safety_blocks`.
        {
            let temp = TempDir::new().unwrap();
            let registry = AvailabilityRegistry::open(temp.path().join("a.json")).unwrap();
            assert_eq!(
                registry.eviction_decision(
                    FileRecordId::new(),
                    0,
                    EvictionRequirements::default(),
                    true
                ),
                EvictionDecision::AllowedDestructiveOverride
            );
        }
    }
}
