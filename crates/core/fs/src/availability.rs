//! Durable local availability policy for collaborative file records.
//!
//! Keyed by [`FileDocumentId`], never by a path: paths are mutable presentation metadata, while pin
//! and eviction decisions must survive renames and moves. This records policy and evidence; the
//! content store retains or deletes bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fs_protocol::projection::{
    AVAILABILITY_PROJECTION_VERSION, MAX_PROJECTED_ERROR_BYTES, ProjectedFreshness,
    ProjectedReplicaEvidence,
};
use fs_protocol::{
    AvailabilityPolicy, AvailabilityProjectionV1, ContentHash, FileDocumentId, ProjectedTransfer,
    Residency, TransferPhase, TransferProgress,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const AVAILABILITY_SCHEMA_VERSION: u16 = 3;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
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
    /// Consecutive automatic hydration failures for the current content; persisting this keeps a
    /// restart from turning an unavailable peer into a tight retry loop.
    #[serde(default)]
    pub retry_count: u32,
    /// Absolute wall-clock deadline for the next automatic attempt; an explicit retry clears it.
    #[serde(default)]
    pub retry_at_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaEvidence {
    /// Peers that recently acknowledged possessing the complete content and were reachable then.
    /// Only authenticated, authorized, non-revoked replicas count.
    pub reachable: u32,
    /// Recently acknowledged durable replicas, which may since be offline, after applying the
    /// workspace's acceptable-host policy.
    pub durable: u32,
    /// The content both counts apply to, by its hash. `None` is never eviction proof.
    pub acknowledged_hash: Option<ContentHash>,
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
pub struct DocumentAvailability {
    pub document_id: FileDocumentId,
    pub kind: EntryKind,
    /// Stable parent record, not a path, so a directory pin reaches descendants created after it.
    pub parent: Option<FileDocumentId>,
    /// The current content, by its hash (the node's manifest hash); none for a folder or an
    /// empty file. Replica evidence must name this exact value.
    pub current_hash: Option<ContentHash>,
    pub residency: Residency,
    pub transfer: TransferState,
    pub policy: AvailabilityPolicy,
    pub replicas: ReplicaEvidence,
    pub freshness: Freshness,
}

impl DocumentAvailability {
    pub fn new(document_id: FileDocumentId, kind: EntryKind) -> Self {
        Self {
            document_id,
            kind,
            parent: None,
            current_hash: None,
            residency: Residency::NotResident,
            transfer: TransferState::default(),
            policy: AvailabilityPolicy::Automatic,
            replicas: ReplicaEvidence::default(),
            freshness: Freshness::default(),
        }
    }
}

impl DocumentAvailability {
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

        // Counts without an observation time, or for any content but the current one, are not
        // current evidence: report them as unknown, keeping the last observation timestamp.
        let replica_evidence_is_current = self.current_hash.is_some()
            && self.replicas.acknowledged_hash == self.current_hash
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
            document_id: self.document_id,
            current_hash: self.current_hash,
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

/// The format a saved registry or queue says it is written in.
#[derive(Clone, Copy, Deserialize)]
pub(crate) struct SchemaVersion {
    pub(crate) schema_version: u16,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryDocument {
    schema_version: u16,
    records: Vec<DocumentAvailability>,
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
    UnknownDocument,
    ActiveTransfer,
    Pinned,
    /// The file has no content to release: a folder or an empty file.
    NoContent,
    ReplicaContentMismatch,
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
    DuplicateDocument(FileDocumentId),
    #[error("record {record:?} refers to missing parent {parent:?}")]
    MissingParent {
        record: FileDocumentId,
        parent: FileDocumentId,
    },
    #[error("record {record:?} has non-directory parent {parent:?}")]
    ParentNotDirectory {
        record: FileDocumentId,
        parent: FileDocumentId,
    },
    #[error("record ancestry contains a cycle at {0:?}")]
    AncestryCycle(FileDocumentId),
    #[error("transfer progress exceeds its declared total")]
    InvalidTransferProgress,
}

/// How long what a file's current content is may stay off disk before the next such change
/// writes the whole registry: a checkout closes thousands of files in a burst, and a write of
/// every row per file would make it cost the workspace rather than the file.
const CONTENT_PERSIST_INTERVAL: Duration = Duration::from_millis(250);

pub struct AvailabilityRegistry {
    path: PathBuf,
    records: BTreeMap<FileDocumentId, DocumentAvailability>,
    /// Inside [`Self::batch`]: mutations are validated and persisted once, when the batch ends.
    deferred: bool,
    /// Current content changed since the registry was last written ([`Self::flush`]).
    content_unsaved: bool,
    /// When the registry was last written whole.
    persisted_at: Instant,
    /// Bumped by every commit that changes what pinned hydration acts on; see
    /// [`Self::hydration_generation`].
    hydration_generation: u64,
    hydration_watch: tokio::sync::watch::Sender<u64>,
    /// Records whose committed state changed since [`Self::take_changed_documents`] last took them,
    /// kept while someone subscribes through [`Self::subscribe_changes`].
    changed: BTreeSet<FileDocumentId>,
    changes_watch: tokio::sync::watch::Sender<()>,
}

/// The part of a record pinned hydration reacts to. Transfer bookkeeping, replica evidence and
/// freshness stamps are absent: they are written by the hydration worker itself or never change
/// its plan.
fn hydration_view(
    record: &DocumentAvailability,
) -> (
    AvailabilityPolicy,
    EntryKind,
    Option<FileDocumentId>,
    Option<ContentHash>,
    Residency,
) {
    (
        record.policy,
        record.kind,
        record.parent,
        record.current_hash,
        record.residency,
    )
}

impl AvailabilityRegistry {
    /// Open an existing registry, or an empty in-memory one when the file is absent or written
    /// in an older format, which the first mutation replaces. That mutation also creates parent
    /// directories. A format newer than this build's is refused.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, AvailabilityError> {
        let path = path.into();
        let bytes = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let format = bytes
            .as_deref()
            .map(serde_json::from_slice::<SchemaVersion>)
            .transpose()?;
        if let Some(SchemaVersion { schema_version }) = format
            && schema_version > AVAILABILITY_SCHEMA_VERSION
        {
            return Err(AvailabilityError::UnsupportedVersion(schema_version));
        }
        let Some(bytes) = bytes.filter(|_| {
            format.is_some_and(|format| format.schema_version == AVAILABILITY_SCHEMA_VERSION)
        }) else {
            return Ok(Self {
                path,
                records: BTreeMap::new(),
                deferred: false,
                content_unsaved: false,
                persisted_at: Instant::now(),
                hydration_generation: 0,
                hydration_watch: tokio::sync::watch::Sender::new(0),
                changed: BTreeSet::new(),
                changes_watch: tokio::sync::watch::Sender::new(()),
            });
        };

        let document: RegistryDocument = serde_json::from_slice(&bytes)?;
        let mut records = BTreeMap::new();
        for record in document.records {
            let id = record.document_id;
            if records.insert(id, record).is_some() {
                return Err(AvailabilityError::DuplicateDocument(id));
            }
        }
        validate_documents(&records)?;
        Ok(Self {
            path,
            records,
            deferred: false,
            content_unsaved: false,
            persisted_at: Instant::now(),
            hydration_generation: 0,
            hydration_watch: tokio::sync::watch::Sender::new(0),
            changed: BTreeSet::new(),
            changes_watch: tokio::sync::watch::Sender::new(()),
        })
    }

    pub fn get(&self, document_id: FileDocumentId) -> Option<&DocumentAvailability> {
        self.records.get(&document_id)
    }

    pub fn records(&self) -> impl Iterator<Item = &DocumentAvailability> {
        self.records.values()
    }

    /// Changes whenever a commit alters a record's policy, kind, parent, current content or
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

    /// A receiver told of each committed change to any record; [`Self::take_changed_documents`] says
    /// which.
    #[must_use]
    pub fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.changes_watch.subscribe()
    }

    pub fn take_changed_documents(&mut self) -> BTreeSet<FileDocumentId> {
        std::mem::take(&mut self.changed)
    }

    fn note_changed(&mut self, ids: impl IntoIterator<Item = FileDocumentId>) {
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

    /// Content hashes of current contents another device proved it holds.
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
        root: FileDocumentId,
    ) -> Result<bool, AvailabilityError> {
        let is_in_subtree = |document_id: FileDocumentId| {
            let mut cursor = Some(document_id);
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
                is_in_subtree(record.document_id)
                    && matches!(
                        self.effective_policy(record.document_id),
                        Ok(Some(AvailabilityPolicy::Automatic))
                    )
                    && record.transfer != TransferState::default()
            })
            .map(|record| record.document_id)
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
        ancestry: &[(FileDocumentId, EntryKind, Option<FileDocumentId>)],
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
                .unwrap_or_else(|| DocumentAvailability::new(id, kind));
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
        if let Err(error) = validate_documents(&self.records).and_then(|()| self.persist()) {
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
        let changed: Vec<FileDocumentId> = self
            .records
            .iter()
            .filter(|(id, record)| before.get(id) != Some(*record))
            .map(|(id, _)| *id)
            .collect();
        self.note_changed(changed);
        Ok(value)
    }

    /// Note what a file's current content is, on a row that already exists: the mount says this
    /// for every version it writes, so a checkout would otherwise write the whole registry once
    /// per file. Content alone changes no ancestry and no policy, so nothing is validated again,
    /// and the write of the registry is shared by a burst of these calls
    /// ([`CONTENT_PERSIST_INTERVAL`], [`Self::flush`]). A row this device holds nothing of is
    /// left to the reconciliation that makes one.
    pub fn update_current_content(
        &mut self,
        document_id: FileDocumentId,
        content: Option<ContentHash>,
    ) -> bool {
        let Some(entry) = self.records.get_mut(&document_id) else {
            return false;
        };
        if entry.current_hash == content {
            return true;
        }
        entry.current_hash = content;
        self.bump_hydration_generation();
        self.note_changed([document_id]);
        if self.persisted_at.elapsed() >= CONTENT_PERSIST_INTERVAL {
            self.persist_or_note();
        } else {
            self.content_unsaved = true;
        }
        true
    }

    /// Write what [`Self::update_current_content`] left pending, if anything.
    ///
    /// # Errors
    /// The registry cannot be written; what it holds stays in memory and is written by the next
    /// [`Self::flush`].
    pub fn flush(&mut self) -> Result<(), AvailabilityError> {
        if !self.content_unsaved {
            return Ok(());
        }
        self.persist()
    }

    /// Write the registry now. A failure leaves the change noted as unsaved, so a later flush,
    /// or dropping the registry, tries again.
    fn persist_or_note(&mut self) {
        if let Err(error) = self.persist() {
            self.content_unsaved = true;
            eprintln!("the availability registry is not written on disk: {error}");
        }
    }

    /// Insert or replace a record and durably commit the whole registry; invalid ancestry leaves
    /// memory and disk unchanged.
    pub fn put(&mut self, record: DocumentAvailability) -> Result<(), AvailabilityError> {
        let id = record.document_id;
        let previous = self.records.insert(id, record);
        if self.deferred {
            return Ok(());
        }
        if let Err(error) = validate_documents(&self.records) {
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
        document_id: FileDocumentId,
    ) -> Result<Option<AvailabilityPolicy>, AvailabilityError> {
        let Some(mut record) = self.records.get(&document_id) else {
            return Ok(None);
        };
        let mut seen = BTreeSet::new();
        loop {
            if !seen.insert(record.document_id) {
                return Err(AvailabilityError::AncestryCycle(record.document_id));
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
                    record: record.document_id,
                    parent,
                })?;
        }
    }

    /// Whether local bytes may be released without losing the last copy: another device's replica
    /// acknowledgement must be bound to exactly this content. Nothing acknowledges replicas since
    /// the tree became the record, so only a caller who accepts losing the bytes may release them.
    pub fn eviction_decision(
        &self,
        document_id: FileDocumentId,
        _now_ms: u64,
        _requirements: EvictionRequirements,
        destructive_override: bool,
    ) -> EvictionDecision {
        if destructive_override {
            return EvictionDecision::AllowedDestructiveOverride;
        }
        let Some(record) = self.records.get(&document_id) else {
            return EvictionDecision::Denied(EvictionBlocker::UnknownDocument);
        };
        if record.transfer.phase.is_active() {
            return EvictionDecision::Denied(EvictionBlocker::ActiveTransfer);
        }
        match self.effective_policy(document_id) {
            Ok(Some(AvailabilityPolicy::Pinned)) => {
                return EvictionDecision::Denied(EvictionBlocker::Pinned);
            }
            Err(_) => return EvictionDecision::Denied(EvictionBlocker::InvalidAncestry),
            _ => {}
        }
        if record.current_hash.is_none() {
            return EvictionDecision::Denied(EvictionBlocker::NoContent);
        }
        EvictionDecision::Denied(EvictionBlocker::ReplicaAcknowledgementMissing)
    }

    fn persist(&mut self) -> Result<(), AvailabilityError> {
        let document = RegistryDocument {
            schema_version: AVAILABILITY_SCHEMA_VERSION,
            records: self.records.values().cloned().collect(),
        };
        let bytes = serde_json::to_vec_pretty(&document)?;
        tilo_files::native::atomic::write_creating_parent(&self.path, &bytes)?;
        self.content_unsaved = false;
        self.persisted_at = Instant::now();
        Ok(())
    }
}

impl Drop for AvailabilityRegistry {
    /// What a burst of content notes left unwritten goes out with the registry, so a clean end
    /// never costs the newest rows.
    fn drop(&mut self) {
        if self.content_unsaved
            && let Err(error) = self.persist()
        {
            eprintln!("the availability registry is not written on disk: {error}");
        }
    }
}

/// Whether `document_id` is pinned, itself or through a stable ancestor, in a captured snapshot of
/// `records`. Missing or cyclic ancestry counts as pinned, so it can never become permission to
/// release local data.
pub(crate) fn pinned_in_snapshot(
    records: &[DocumentAvailability],
    document_id: FileDocumentId,
) -> bool {
    let mut current = Some(document_id);
    let mut remaining = records.len().saturating_add(1);
    while let Some(document_id) = current {
        if remaining == 0 {
            return true;
        }
        remaining = remaining.saturating_sub(1);
        let Some(record) = records
            .iter()
            .find(|record| record.document_id == document_id)
        else {
            return true;
        };
        if record.policy == AvailabilityPolicy::Pinned {
            return true;
        }
        current = record.parent;
    }
    false
}

fn validate_documents(
    records: &BTreeMap<FileDocumentId, DocumentAvailability>,
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
            let parent_document = records
                .get(&parent)
                .ok_or(AvailabilityError::MissingParent {
                    record: record.document_id,
                    parent,
                })?;
            if parent_document.kind != EntryKind::Directory {
                return Err(AvailabilityError::ParentNotDirectory {
                    record: record.document_id,
                    parent,
                });
            }
        }

        let mut next = Some(record.document_id);
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

/// A content hash no other test content has.
#[cfg(test)]
pub(crate) fn test_hash() -> ContentHash {
    ContentHash(*blake3::hash(uuid::Uuid::new_v4().as_bytes()).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs_protocol::projection::{MAX_AVAILABILITY_XATTR_BYTES, ProjectionError};
    use tempfile::TempDir;

    fn file(id: FileDocumentId, content: ContentHash) -> DocumentAvailability {
        let mut record = DocumentAvailability::new(id, EntryKind::File);
        record.current_hash = Some(content);
        record.residency = Residency::Resident { bytes: 1024 };
        record
    }

    tilo_test_env::scenario! {
        fn the_availability_registry_tells_subscribers_and_follows_pins;

        phase a_subscriber_learns_which_records_changed;
        // Whoever subscribes learns which records changed, by any writer: a
        // put, a batch, an unpin clearing transfers. Nothing is kept for
        // nobody, and a write that changes nothing is no change.
        {
            let dir = TempDir::new().unwrap();
            let mut registry = AvailabilityRegistry::open(dir.path().join("a.json")).unwrap();
            let (first, second) = (FileDocumentId::new(), FileDocumentId::new());
            registry.put(file(first, test_hash())).unwrap();
            assert!(
                registry.take_changed_documents().is_empty(),
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
            assert_eq!(registry.take_changed_documents(), BTreeSet::from([first]));

            registry
                .batch(|registry| {
                    let _ = registry.put(file(second, test_hash()));
                })
                .unwrap();
            assert!(changes.has_changed().unwrap());
            assert_eq!(registry.take_changed_documents(), BTreeSet::from([second]));

            assert!(registry.clear_automatic_transfer_subtree(first).unwrap());
            assert_eq!(registry.take_changed_documents(), BTreeSet::from([first]));
            assert!(registry.take_changed_documents().is_empty());
        }

        phase pins_follow_descendants_moves_and_namespace_reconciliation;
        {
            {
                let temp = TempDir::new().unwrap();
                let mut registry = AvailabilityRegistry::open(temp.path().join("a.json")).unwrap();
                let pinned_dir = FileDocumentId::new();
                let other_dir = FileDocumentId::new();
                let child = FileDocumentId::new();

                let mut directory = DocumentAvailability::new(pinned_dir, EntryKind::Directory);
                directory.policy = AvailabilityPolicy::Pinned;
                registry.put(directory).unwrap();
                registry
                    .put(DocumentAvailability::new(other_dir, EntryKind::Directory))
                    .unwrap();

                let mut child_document = file(child, test_hash());
                child_document.parent = Some(pinned_dir);
                registry.put(child_document.clone()).unwrap();
                assert_eq!(
                    registry.effective_policy(child).unwrap(),
                    Some(AvailabilityPolicy::Pinned)
                );

                child_document.parent = Some(other_dir);
                registry.put(child_document).unwrap();
                assert_eq!(
                    registry.effective_policy(child).unwrap(),
                    Some(AvailabilityPolicy::Automatic)
                );
            }
            {
                let temp = TempDir::new().unwrap();
                let mut registry = AvailabilityRegistry::open(temp.path().join("a.json")).unwrap();
                assert!(!registry.has_pinned_policy());

                let id = FileDocumentId::new();
                let mut record = file(id, test_hash());
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
    }

    #[test]
    fn availability_registry_persists_by_stable_document_id_and_rejects_tampering() {
        {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("availability/state.json");
            let id = FileDocumentId::new();
            let version = test_hash();

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
            assert_eq!(
                AvailabilityRegistry::open(&path).unwrap().records().count(),
                0,
                "a registry in an older format is dropped"
            );
            tilo_files::native::atomic::write_creating_parent(
                &path,
                &serde_json::to_vec(&serde_json::json!({
                    "schema_version": AVAILABILITY_SCHEMA_VERSION + 1,
                    "records": []
                }))
                .unwrap(),
            )
            .unwrap();
            assert!(matches!(
                AvailabilityRegistry::open(path),
                Err(AvailabilityError::UnsupportedVersion(_))
            ));
        }
        {
            let temp = TempDir::new().unwrap();
            let mut registry = AvailabilityRegistry::open(temp.path().join("a.json")).unwrap();
            let id = FileDocumentId::new();
            let mut record = file(id, test_hash());
            record.parent = Some(FileDocumentId::new());
            assert!(matches!(
                registry.put(record),
                Err(AvailabilityError::MissingParent { .. })
            ));
            assert!(registry.get(id).is_none());

            let mut record = file(id, test_hash());
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

    /// A burst of content notes writes the registry once, and a flush writes the last of them;
    /// a row this device holds nothing of is left for the reconciliation that makes one.
    #[test]
    fn a_burst_of_content_notes_is_kept_by_a_flush_as_the_last_of_them() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("a.json");
        let mut registry = AvailabilityRegistry::open(&path).unwrap();
        let id = FileDocumentId::new();
        let first = test_hash();
        registry.put(file(id, first)).unwrap();
        let mut last = first;
        for _ in 0..500 {
            last = test_hash();
            assert!(registry.update_current_content(id, Some(last)));
        }
        assert!(!registry.update_current_content(FileDocumentId::new(), Some(last)));
        registry.flush().unwrap();
        let reopened = AvailabilityRegistry::open(&path).unwrap();
        assert_eq!(reopened.get(id).unwrap().current_hash, Some(last));
    }

    tilo_test_env::scenario! {
        fn the_availability_projection_round_trips_and_is_bounded;

        phase projection_round_trips_every_ui_dimension;
        {
            {
                let record = rich_document();
                let projection = record.projection_with_policy(AvailabilityPolicy::Pinned);
                let bytes = projection.to_xattr_bytes().unwrap();

                assert!(bytes.len() <= MAX_AVAILABILITY_XATTR_BYTES);
                assert_eq!(
                    AvailabilityProjectionV1::parse_xattr(&bytes).unwrap(),
                    projection
                );
                assert_eq!(projection.schema_version, AVAILABILITY_PROJECTION_VERSION);
                assert_eq!(projection.document_id, record.document_id);
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
                let mut record = rich_document();
                record.replicas.acknowledged_hash = Some(test_hash());
                let projection = record.projection();
                assert_eq!(projection.replicas, ProjectedReplicaEvidence::Unknown);

                record.replicas.acknowledged_hash = record.current_hash;
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

        phase projection_bounds_errors_and_rejects_inconsistent_values;
        {
            {
                let mut record = rich_document();
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

                let mut projection = rich_document().projection();
                projection.schema_version = AVAILABILITY_PROJECTION_VERSION + 1;
                let bytes = serde_json::to_vec(&projection).unwrap();
                assert!(matches!(
                    AvailabilityProjectionV1::parse_xattr(&bytes),
                    Err(ProjectionError::UnsupportedVersion(_))
                ));

                let mut projection = rich_document().projection();
                projection.transfer.progress = Some(TransferProgress {
                    completed_bytes: 2,
                    total_bytes: Some(1),
                });
                let bytes = serde_json::to_vec(&projection).unwrap();
                assert!(matches!(
                    AvailabilityProjectionV1::parse_xattr(&bytes),
                    Err(ProjectionError::InvalidProgress)
                ));

                let mut projection = rich_document().projection();
                projection.transfer.last_error = None;
                let bytes = serde_json::to_vec(&projection).unwrap();
                assert!(matches!(
                    AvailabilityProjectionV1::parse_xattr(&bytes),
                    Err(ProjectionError::InvalidTransferError)
                ));

                let mut projection = rich_document().projection();
                projection.current_hash = None;
                let bytes = serde_json::to_vec(&projection).unwrap();
                assert!(matches!(
                    AvailabilityProjectionV1::parse_xattr(&bytes),
                    Err(ProjectionError::InvalidReplicaEvidence)
                ));
            }
        }
    }

    fn rich_document() -> DocumentAvailability {
        let id = FileDocumentId::new();
        let version = test_hash();
        DocumentAvailability {
            document_id: id,
            kind: EntryKind::File,
            parent: None,
            current_hash: Some(version),
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
                acknowledged_hash: Some(version),
            },
            freshness: Freshness {
                metadata_observed_at_ms: Some(7_000),
                local_verified_at_ms: Some(7_500),
                replicas_observed_at_ms: Some(7_900),
            },
        }
    }
}
