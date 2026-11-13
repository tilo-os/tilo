//! Durable local availability policy for collaborative file records.
//!
//! Keyed by [`FileRecordId`], never by a path: paths are mutable presentation metadata, while pin
//! and eviction decisions must survive renames and moves. This records policy and evidence; the
//! content store retains or deletes bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use fs_protocol::projection::{
    AVAILABILITY_PROJECTION_VERSION, MAX_PROJECTED_ERROR_BYTES, ProjectedFreshness,
    ProjectedReplicaEvidence,
};
use fs_protocol::{
    AvailabilityPolicy, AvailabilityProjectionV1, FileRecordId, FileVersionId, ProjectedTransfer,
    Residency, TransferPhase, TransferProgress,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const AVAILABILITY_SCHEMA_VERSION: u16 = 2;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    File,
    Directory,
}

/// Last transfer failure, kept apart from the current phase so a retry shows both progress and
/// the previous diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferError {
    pub message: String,
    pub occurred_at_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferState {
    pub phase: TransferPhase,
    pub progress: Option<TransferProgress>,
    pub last_error: Option<TransferError>,
    /// Consecutive automatic hydration failures for the current version; persisting this keeps a
    /// restart from turning an unavailable peer into a tight retry loop.
    #[serde(default)]
    pub retry_count: u32,
    /// Absolute wall-clock deadline for the next automatic attempt; an explicit retry clears it.
    #[serde(default)]
    pub retry_at_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaEvidence {
    /// Peers that recently acknowledged possessing the complete version and were reachable then.
    /// Only authenticated, authorized, non-revoked replicas count.
    pub reachable: u32,
    /// Recently acknowledged durable replicas, which may since be offline, after applying the
    /// workspace's acceptable-host policy.
    pub durable: u32,
    /// The version both counts apply to. `None` is never eviction proof.
    pub acknowledged_version: Option<FileVersionId>,
}

/// Freshness timestamps, kept apart because metadata, local bytes and replica observations
/// advance independently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Freshness {
    pub metadata_observed_at_ms: Option<u64>,
    pub local_verified_at_ms: Option<u64>,
    pub replicas_observed_at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordAvailability {
    pub record_id: FileRecordId,
    pub kind: RecordKind,
    /// Stable parent record, not a path, so a directory pin reaches descendants created after it.
    pub parent: Option<FileRecordId>,
    /// Current immutable version. Replica evidence must name this exact value.
    pub current_version: Option<FileVersionId>,
    pub residency: Residency,
    pub transfer: TransferState,
    pub policy: AvailabilityPolicy,
    pub replicas: ReplicaEvidence,
    pub freshness: Freshness,
    /// Historical versions retained only on this device. IDs rather than a count, so history and
    /// GC can remove the precise retention reason.
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
            freshness: Freshness::default(),
            retained_versions: Vec::new(),
        }
    }
}

impl RecordAvailability {
    pub fn projection(&self) -> AvailabilityProjectionV1 {
        self.projection_with_policy(self.policy)
    }

    pub fn projection_with_policy(
        &self,
        effective_policy: AvailabilityPolicy,
    ) -> AvailabilityProjectionV1 {
        let (last_error, error_truncated, last_error_at_ms) = self
            .transfer
            .last_error
            .as_ref()
            .map_or((None, false, None), |error| {
                let (message, truncated) = truncate_utf8(&error.message, MAX_PROJECTED_ERROR_BYTES);
                (Some(message), truncated, Some(error.occurred_at_ms))
            });

        // Counts without an observation time, or for any version but the current one, are not
        // current evidence: report them as unknown, keeping the last observation timestamp.
        let replica_evidence_is_current = self.current_version.is_some()
            && self.replicas.acknowledged_version == self.current_version
            && self.freshness.replicas_observed_at_ms.is_some();
        let replicas = if replica_evidence_is_current {
            ProjectedReplicaEvidence::Known {
                reachable: self.replicas.reachable,
                durable: self.replicas.durable,
            }
        } else {
            ProjectedReplicaEvidence::Unknown
        };

        AvailabilityProjectionV1 {
            schema_version: AVAILABILITY_PROJECTION_VERSION,
            record_id: self.record_id,
            current_version: self.current_version,
            residency: self.residency,
            transfer: ProjectedTransfer {
                phase: self.transfer.phase,
                progress: self.transfer.progress,
                last_error,
                last_error_at_ms,
                error_truncated,
            },
            policy: if self.policy == AvailabilityPolicy::Pinned {
                AvailabilityPolicy::Pinned
            } else {
                effective_policy
            },
            policy_inherited: self.policy != AvailabilityPolicy::Pinned
                && effective_policy == AvailabilityPolicy::Pinned,
            replicas,
            freshness: ProjectedFreshness {
                metadata_observed_at_ms: self.freshness.metadata_observed_at_ms,
                local_verified_at_ms: self.freshness.local_verified_at_ms,
                replicas_observed_at_ms: self.freshness.replicas_observed_at_ms,
            },
        }
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryDocument {
    schema_version: u16,
    records: Vec<RecordAvailability>,
}

/// What an eviction of local bytes would cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvictionRequirements {
    pub minimum_reachable: u32,
    pub minimum_durable: u32,
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
    /// Caller explicitly accepted permanent data loss; UI presents this as destructive rather than
    /// as ordinary cache eviction.
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
    #[error("current content revision is invalid")]
    InvalidCurrentRevision,
}

pub struct AvailabilityRegistry {
    path: PathBuf,
    records: BTreeMap<FileRecordId, RecordAvailability>,
    /// Inside [`Self::batch`]: mutations are validated and persisted once, when the batch ends.
    deferred: bool,
    /// Bumped by every commit that changes what pinned hydration acts on; see
    /// [`Self::hydration_generation`].
    hydration_generation: u64,
    hydration_watch: tokio::sync::watch::Sender<u64>,
    /// Records whose committed state changed since [`Self::take_changed_records`] last took them,
    /// kept while someone subscribes through [`Self::subscribe_changes`].
    changed: BTreeSet<FileRecordId>,
    changes_watch: tokio::sync::watch::Sender<()>,
    #[cfg(test)]
    persists: std::sync::atomic::AtomicU64,
}

/// The part of a record pinned hydration reacts to. Transfer bookkeeping, replica evidence and
/// freshness stamps are absent: they are written by the hydration worker itself or never change
/// its plan.
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
    /// Open an existing registry, or an empty in-memory one when the file is absent. The first
    /// mutation creates parent directories.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, AvailabilityError> {
        let path = path.into();
        if !path.exists() {
            return Ok(Self {
                path,
                records: BTreeMap::new(),
                deferred: false,
                hydration_generation: 0,
                hydration_watch: tokio::sync::watch::Sender::new(0),
                changed: BTreeSet::new(),
                changes_watch: tokio::sync::watch::Sender::new(()),
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
            changed: BTreeSet::new(),
            changes_watch: tokio::sync::watch::Sender::new(()),
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

    /// Changes whenever a commit alters a record's policy, kind, parent, current version or
    /// residency, or adds a record. Transfer progress, replica receipts and freshness stamps leave
    /// it alone, so the pinned hydration worker learns from it that nothing it acts on changed.
    #[must_use]
    pub fn hydration_generation(&self) -> u64 {
        self.hydration_generation
    }

    /// A receiver that sees every later change of [`Self::hydration_generation`], from any writer
    /// of this registry.
    #[must_use]
    pub fn subscribe_hydration(&self) -> tokio::sync::watch::Receiver<u64> {
        self.hydration_watch.subscribe()
    }

    /// A receiver told of each committed change to any record; [`Self::take_changed_records`] says
    /// which.
    #[must_use]
    pub fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.changes_watch.subscribe()
    }

    pub fn take_changed_records(&mut self) -> BTreeSet<FileRecordId> {
        std::mem::take(&mut self.changed)
    }

    fn note_changed(&mut self, ids: impl IntoIterator<Item = FileRecordId>) {
        if self.changes_watch.receiver_count() == 0 {
            return;
        }
        let before = self.changed.len();
        self.changed.extend(ids);
        if self.changed.len() != before {
            self.changes_watch.send_replace(());
        }
    }

    fn bump_hydration_generation(&mut self) {
        self.hydration_generation = self.hydration_generation.wrapping_add(1);
        self.hydration_watch.send_replace(self.hydration_generation);
    }

    #[cfg(test)]
    #[must_use]
    pub fn persist_count(&self) -> u64 {
        self.persists.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Content ids of current versions another device proved it holds.
    ///
    /// Nothing acknowledges replicas since the tree became the record, so this is empty.
    #[must_use]
    pub fn held_elsewhere(&self) -> std::collections::HashSet<[u8; 32]> {
        std::collections::HashSet::new()
    }

    /// Whether any record explicitly promises offline residency. Descendants inherit the promise,
    /// so one explicit row requires a namespace reconciliation; with none, the periodic worker can
    /// stay idle.
    pub fn has_pinned_policy(&self) -> bool {
        self.records
            .values()
            .any(|record| record.policy == AvailabilityPolicy::Pinned)
    }

    /// Cancel durable automatic-hydration state below a pin that was removed, so the periodic
    /// worker can take the zero-scan fast path whenever no pins remain.
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
        for id in &ids {
            if let Some(record) = self.records.get_mut(id) {
                record.transfer = TransferState::default();
            }
        }
        if let Err(error) = self.persist() {
            self.records = previous;
            return Err(error);
        }
        self.note_changed(ids);
        Ok(true)
    }

    /// Set `policy` on a record, given its `ancestry`: every record from the workspace root down to
    /// it, with its kind and parent. Recording each ancestor's kind and parent lets a directory's
    /// pin reach what is created in it later. Unpinning cancels the automatic transfers below.
    pub fn set_policy_along(
        &mut self,
        ancestry: &[(FileRecordId, RecordKind, Option<FileRecordId>)],
        policy: AvailabilityPolicy,
    ) -> Result<(), AvailabilityError> {
        let Some(&(target, ..)) = ancestry.last() else {
            return Ok(());
        };
        for &(id, kind, parent) in ancestry {
            let mut record = self
                .records
                .get(&id)
                .cloned()
                .unwrap_or_else(|| RecordAvailability::new(id, kind));
            record.kind = kind;
            record.parent = parent;
            if id == target {
                record.policy = policy;
            }
            self.put(record)?;
        }
        if policy == AvailabilityPolicy::Automatic {
            self.clear_automatic_transfer_subtree(target)?;
        }
        Ok(())
    }

    /// Install the authenticated current revision before accepting replica evidence for it;
    /// without this row a valid acknowledgement would be discarded as `UnknownRecord`.
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
            record.freshness.local_verified_at_ms = None;
            record.freshness.replicas_observed_at_ms = None;
        }
        self.put(record)?;
        Ok(true)
    }

    /// Runs `apply`'s mutations and commits them with one validation and one durable write, so
    /// receipts for a thousand records cost one write. A failed batch leaves memory and disk as
    /// they were.
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
        let changed: Vec<FileRecordId> = self
            .records
            .iter()
            .filter(|(id, record)| before.get(id) != Some(*record))
            .map(|(id, _)| *id)
            .collect();
        self.note_changed(changed);
        Ok(value)
    }

    /// Insert or replace a record and durably commit the whole registry; invalid ancestry leaves
    /// memory and disk unchanged.
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
        if previous.as_ref() != self.records.get(&id) {
            self.note_changed([id]);
        }
        Ok(())
    }

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

    /// Whether local bytes may be released without losing the last copy: another device's replica
    /// acknowledgement must be bound to exactly this version. Nothing acknowledges replicas since
    /// the tree became the record, so only a caller who accepts losing the bytes may release them.
    pub fn eviction_decision(
        &self,
        record_id: FileRecordId,
        _now_ms: u64,
        _requirements: EvictionRequirements,
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
        if record.current_version.is_none() {
            return EvictionDecision::Denied(EvictionBlocker::CurrentVersionUnknown);
        }
        EvictionDecision::Denied(EvictionBlocker::ReplicaAcknowledgementMissing)
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

/// Whether `record_id` is pinned, itself or through a stable ancestor, in a captured snapshot of
/// `records`. Missing or cyclic ancestry counts as pinned, so it can never become permission to
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

fn validate_records(
    records: &BTreeMap<FileRecordId, RecordAvailability>,
) -> Result<(), AvailabilityError> {
    for record in records.values() {
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
    use fs_protocol::projection::{MAX_AVAILABILITY_XATTR_BYTES, ProjectionError};
    use tempfile::TempDir;

    fn file(id: FileRecordId, version: FileVersionId) -> RecordAvailability {
        let mut record = RecordAvailability::new(id, RecordKind::File);
        record.current_version = Some(version);
        record.residency = Residency::Resident { bytes: 1024 };
        record
    }

    /// Whoever subscribes learns which records changed, by any writer: a
    /// put, a batch, an unpin clearing transfers. Nothing is kept for
    /// nobody, and a write that changes nothing is no change.
    #[test]
    fn a_subscriber_learns_which_records_changed() {
        let dir = TempDir::new().unwrap();
        let mut registry = AvailabilityRegistry::open(dir.path().join("a.json")).unwrap();
        let (first, second) = (FileRecordId::new(), FileRecordId::new());
        registry.put(file(first, FileVersionId::new())).unwrap();
        assert!(
            registry.take_changed_records().is_empty(),
            "nothing is kept while nobody subscribes"
        );

        let mut changes = registry.subscribe_changes();
        let mut record = registry.get(first).unwrap().clone();
        registry.put(record.clone()).unwrap();
        assert!(!changes.has_changed().unwrap(), "an identical write");
        record.transfer.last_error = Some(TransferError {
            message: fs_protocol::NO_PEER_DELIVERED_ERROR.into(),
            occurred_at_ms: 3,
        });
        registry.put(record).unwrap();
        assert!(changes.has_changed().unwrap());
        changes.borrow_and_update();
        assert_eq!(registry.take_changed_records(), BTreeSet::from([first]));

        registry
            .batch(|registry| {
                let _ = registry.put(file(second, FileVersionId::new()));
            })
            .unwrap();
        assert!(changes.has_changed().unwrap());
        assert_eq!(registry.take_changed_records(), BTreeSet::from([second]));

        assert!(registry.clear_automatic_transfer_subtree(first).unwrap());
        assert_eq!(registry.take_changed_records(), BTreeSet::from([first]));
        assert!(registry.take_changed_records().is_empty());
    }

    #[test]
    fn availability_registry_persists_by_stable_record_id_and_rejects_tampering() {
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

            let new_name = "approved.png";
            assert_ne!(old_name, new_name);
            assert_eq!(
                registry.effective_policy(id).unwrap(),
                Some(AvailabilityPolicy::Pinned)
            );
            assert_eq!(registry.records().count(), 1);
        }
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

            let mut child_record = file(child, FileVersionId::new());
            child_record.parent = Some(pinned_dir);
            registry.put(child_record.clone()).unwrap();
            assert_eq!(
                registry.effective_policy(child).unwrap(),
                Some(AvailabilityPolicy::Pinned)
            );

            child_record.parent = Some(other_dir);
            registry.put(child_record).unwrap();
            assert_eq!(
                registry.effective_policy(child).unwrap(),
                Some(AvailabilityPolicy::Automatic)
            );
        }
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
    fn projection_round_trips_every_ui_dimension() {
        {
            let record = rich_record();
            let projection = record.projection_with_policy(AvailabilityPolicy::Pinned);
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
        }
        {
            let mut record = rich_record();
            let old_version = FileVersionId::new();
            record.replicas.acknowledged_version = Some(old_version);
            let projection = record.projection();
            assert_eq!(projection.replicas, ProjectedReplicaEvidence::Unknown);

            record.replicas.acknowledged_version = record.current_version;
            record.freshness.replicas_observed_at_ms = None;
            let projection = record.projection();
            assert_eq!(projection.replicas, ProjectedReplicaEvidence::Unknown);

            record.freshness.replicas_observed_at_ms = Some(9_000);
            record.replicas.reachable = 0;
            record.replicas.durable = 0;
            let projection = record.projection();
            assert_eq!(
                projection.replicas,
                ProjectedReplicaEvidence::Known {
                    reachable: 0,
                    durable: 0
                }
            );
        }
    }

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
            freshness: Freshness {
                metadata_observed_at_ms: Some(7_000),
                local_verified_at_ms: Some(7_500),
                replicas_observed_at_ms: Some(7_900),
            },
            retained_versions: Vec::new(),
        }
    }

    #[test]
    fn projection_bounds_errors_and_rejects_inconsistent_values() {
        {
            let mut record = rich_record();
            record.transfer.last_error.as_mut().unwrap().message = "é".repeat(300);
            let projection = record.projection();
            let message = projection.transfer.last_error.as_deref().unwrap();
            assert!(message.len() <= MAX_PROJECTED_ERROR_BYTES);
            assert!(message.is_char_boundary(message.len()));
            assert!(projection.transfer.error_truncated);
            assert!(projection.to_xattr_bytes().unwrap().len() <= MAX_AVAILABILITY_XATTR_BYTES);
        }
        {
            assert!(matches!(
                AvailabilityProjectionV1::parse_xattr(&vec![
                    b' ';
                    MAX_AVAILABILITY_XATTR_BYTES + 1
                ]),
                Err(ProjectionError::TooLarge { .. })
            ));

            let mut projection = rich_record().projection();
            projection.schema_version = AVAILABILITY_PROJECTION_VERSION + 1;
            let bytes = serde_json::to_vec(&projection).unwrap();
            assert!(matches!(
                AvailabilityProjectionV1::parse_xattr(&bytes),
                Err(ProjectionError::UnsupportedVersion(_))
            ));

            let mut projection = rich_record().projection();
            projection.transfer.progress = Some(TransferProgress {
                completed_bytes: 2,
                total_bytes: Some(1),
            });
            let bytes = serde_json::to_vec(&projection).unwrap();
            assert!(matches!(
                AvailabilityProjectionV1::parse_xattr(&bytes),
                Err(ProjectionError::InvalidProgress)
            ));

            let mut projection = rich_record().projection();
            projection.transfer.last_error = None;
            let bytes = serde_json::to_vec(&projection).unwrap();
            assert!(matches!(
                AvailabilityProjectionV1::parse_xattr(&bytes),
                Err(ProjectionError::InvalidTransferError)
            ));

            let mut projection = rich_record().projection();
            projection.current_version = None;
            let bytes = serde_json::to_vec(&projection).unwrap();
            assert!(matches!(
                AvailabilityProjectionV1::parse_xattr(&bytes),
                Err(ProjectionError::InvalidReplicaEvidence)
            ));
        }
    }
}
