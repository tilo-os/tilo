//! Crash-safe persistence for authenticated record replicas.
//!
//! The durable form is an append-only operation log of signed envelopes
//! (`crate::record_log`), not an unsigned materialized snapshot: restart
//! replays the log in order and reconstructs the exact replay watermarks and
//! immutable-version collision decisions. Records, stream cursors and the
//! history projection are all derived from it.
//!
//! Writes are grouped: a changeset scope (one native epoch, or any other
//! batch a device publishes together) keeps the local author's envelopes in
//! memory until [`RecordRepository::commit_changeset`] signs them once as a
//! [`SignedChangesetV1`] and stores them with the changeset in one durable
//! transaction. Until then they are visible to the local publisher only:
//! everything served to peers reads durable envelopes. A peer applies a
//! received changeset with [`RecordRepository::apply_changeset`], also in
//! one transaction, so no device ever holds half of one.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fs_protocol::{FileActor, FileBindingV1, FileMutationTransaction, FileRecordId};
use thiserror::Error;
use uuid::Uuid;

use crate::record_changeset::{
    ChangesetError, ChangesetId, ChangesetLeafV1, SignedChangesetV1, envelopes_digest,
};
use crate::record_log::{RecordLog, RecordLogError};
use crate::record_replication::{
    RecordAuthorizer, RecordEnvelopeV1, RecordReplica, RecordReplicationError, RecordSnapshot,
};
use crate::record_transaction::{
    RecordTransactionJournal, RecordTransactionJournalEntry, RecordTransactionJournalError,
    RecordTransactionParticipantBatch, RecordTransactionPhase,
};
use tilo_lock::MutexRecover as _;

/// Records one repository holds. The log itself has no practical limit
/// (positions are `u64`); what bounds a device is the in-memory index,
/// which keeps each record's replica state and envelopes (a few KiB for a
/// typical file record). 2^24 records is tens of GiB of index, beyond what
/// a device serving one workspace holds, so reaching it means corruption or
/// abuse rather than a large tree.
pub const MAX_REPOSITORY_RECORDS: usize = 1 << 24;
pub const MAX_RECORD_OPERATIONS: usize = 4_096;
pub const MAX_RECORD_VERSIONS: usize = 1_024;
pub const MAX_RECORD_CONFLICTS: usize = 256;
pub const MAX_RECORD_BATCH_OPERATIONS: usize = 256;

/// The operation log inside the repository root.
const LOG_FILE: &str = "records.redb";
/// Position of an envelope a changeset scope still holds back.
const PENDING: u64 = u64::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordStreamCursor {
    pub record_id: FileRecordId,
    pub actor: FileActor,
    pub contiguous_sequence: u64,
    pub prefix_digest: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RecordStreamKey {
    pub record_id: FileRecordId,
    pub person: [u8; 32],
    pub device: [u8; 32],
}

impl RecordStreamCursor {
    #[must_use]
    pub fn key(&self) -> RecordStreamKey {
        RecordStreamKey {
            record_id: self.record_id,
            person: *self.actor.person.as_bytes(),
            device: *self.actor.device.as_bytes(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecordRepositoryKey {
    pub workspace_id: Uuid,
    pub record_id: FileRecordId,
}

/// Repository-wide completeness proof for one staged transaction, as tests
/// observe it.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordTransactionStatus {
    pub transaction: FileMutationTransaction,
    pub prepared_participants: usize,
    pub committed_participants: usize,
    pub complete: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordTransactionCheckpoint {
    PreparedJournalDurable,
    ParticipantRecordDurable { index: usize },
    ParticipantPhaseDurable,
}

#[derive(Clone, Debug)]
struct StoredRecord {
    /// Transient fence derived exclusively from the existing durable journal.
    pending_restore: Option<Uuid>,
    replica: RecordReplica,
    envelopes: Vec<RecordEnvelopeV1>,
    positions: Vec<u64>,
    /// Envelopes not yet in the log.
    pending: usize,
    /// The repository's [`RepositoryState::write_generation`] when this
    /// record last had envelopes made durable; 0 for records replayed from
    /// the log at open.
    written: u64,
}

impl StoredRecord {
    fn new(record_id: FileRecordId) -> Self {
        Self {
            pending_restore: None,
            replica: RecordReplica::new(record_id),
            envelopes: Vec::new(),
            positions: Vec::new(),
            pending: 0,
            written: 0,
        }
    }

    /// Accept an envelope already merged into the replica; it is pending
    /// until written.
    fn push_pending(&mut self, envelope: RecordEnvelopeV1) {
        self.envelopes.push(envelope);
        self.positions.push(PENDING);
        self.pending = self.pending.saturating_add(1);
    }

    /// Envelopes in the log, in accepted order. Only these are ever shown
    /// to peers.
    fn durable(&self) -> impl Iterator<Item = &RecordEnvelopeV1> {
        self.envelopes
            .iter()
            .zip(&self.positions)
            .filter(|(_, position)| **position != PENDING)
            .map(|(envelope, _)| envelope)
    }

    fn durable_len(&self) -> usize {
        self.envelopes.len().saturating_sub(self.pending)
    }

    fn durable_highest(&self, actor: &FileActor) -> u64 {
        self.durable()
            .filter(|envelope| envelope.actor == *actor)
            .map(|envelope| envelope.sequence)
            .max()
            .unwrap_or(0)
    }
}

/// Bounded, already-verified state used only to heal the live projection
/// after a crash between repository commit and materialization.
#[derive(Clone, Debug)]
pub(crate) struct RecordReconciliationBatch {
    pub key: RecordRepositoryKey,
    pub envelopes: Vec<RecordEnvelopeV1>,
    pub snapshot: RecordSnapshot,
}

/// The local author's open publish batch.
struct ChangesetScope {
    /// The thread publishing; `None` after a commit that failed, whose
    /// envelopes the next scope carries on.
    thread: Option<std::thread::ThreadId>,
    workspace_id: Uuid,
    actor: FileActor,
    keys: Vec<RecordRepositoryKey>,
    touched: HashSet<RecordRepositoryKey>,
}

/// Every durable changeset and which envelopes each covers.
#[derive(Default)]
struct ChangesetIndex {
    by_id: BTreeMap<ChangesetId, (Arc<SignedChangesetV1>, u64)>,
    /// Per record stream: first sequence → (last sequence, changeset).
    members: HashMap<(RecordRepositoryKey, FileActor), BTreeMap<u64, (u64, ChangesetId)>>,
}

impl ChangesetIndex {
    fn insert(&mut self, changeset: Arc<SignedChangesetV1>, first_position: u64) {
        let id = changeset.header.id();
        let actor = changeset.header.actor;
        for leaf in &changeset.leaves {
            let key = RecordRepositoryKey {
                workspace_id: id.workspace_id,
                record_id: leaf.record_id,
            };
            self.members
                .entry((key, actor))
                .or_default()
                .insert(leaf.first_sequence, (leaf.last_sequence, id));
        }
        self.by_id.insert(id, (changeset, first_position));
    }

    fn member(
        &self,
        key: RecordRepositoryKey,
        actor: &FileActor,
        sequence: u64,
    ) -> Option<ChangesetId> {
        let (_, (last, id)) = self
            .members
            .get(&(key, *actor))?
            .range(..=sequence)
            .next_back()?;
        (*last >= sequence).then_some(*id)
    }

    fn next_sequence(&self, workspace_id: Uuid, device: [u8; 32]) -> u64 {
        let low = ChangesetId {
            workspace_id,
            device,
            sequence: 0,
        };
        let high = ChangesetId {
            workspace_id,
            device,
            sequence: u64::MAX,
        };
        self.by_id
            .range(low..=high)
            .next_back()
            .map_or(1, |(id, _)| id.sequence.saturating_add(1))
    }
}

/// One unit of a pull response, in log order: a whole changeset, or a run
/// of a stream's envelopes that belong to none.
pub enum PullUnit {
    Changeset(Arc<SignedChangesetV1>),
    Stream {
        key: RecordRepositoryKey,
        actor: FileActor,
        after_sequence: u64,
        envelopes: Vec<RecordEnvelopeV1>,
    },
}

/// What [`RecordRepository::apply_changeset`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum ChangesetApplied {
    /// The changeset is now durable here; these records changed.
    Applied(Vec<RecordRepositoryKey>),
    /// This exact changeset was already here.
    AlreadyPresent,
}

/// Single-process durable repository for record-scoped replication state.
///
/// The log has one writer: opening a root this process already has open
/// returns another handle to the same repository.
pub struct RecordRepository {
    state: Arc<RepositoryState>,
}

impl std::ops::Deref for RecordRepository {
    type Target = RepositoryState;

    fn deref(&self) -> &RepositoryState {
        &self.state
    }
}

/// Repositories open in this process, by canonical root.
fn open_repositories() -> &'static Mutex<HashMap<PathBuf, std::sync::Weak<RepositoryState>>> {
    static OPEN: std::sync::OnceLock<Mutex<HashMap<PathBuf, std::sync::Weak<RepositoryState>>>> =
        std::sync::OnceLock::new();
    OPEN.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct RepositoryState {
    root: PathBuf,
    log: RecordLog,
    records: Mutex<BTreeMap<RecordRepositoryKey, StoredRecord>>,
    /// Stream cursors per record, with the durable envelope count they were
    /// computed at. A record's log only grows, so an unchanged count means
    /// unchanged cursors; reconciliation then hashes each envelope once, not
    /// once per exchange.
    cursors: Mutex<BTreeMap<RecordRepositoryKey, CachedCursors>>,
    scope: Mutex<Option<ChangesetScope>>,
    changesets: Mutex<ChangesetIndex>,
    /// Held for writing while a changeset is published or applied, and for
    /// reading by anything that shows namespace or record state to peers or
    /// to the live tree: none of them ever sees half of a changeset.
    gate: tokio::sync::RwLock<()>,
    /// Versions one record may hold: [`MAX_RECORD_VERSIONS`], lowered only
    /// by tests so they can cross the bound cheaply.
    max_versions: std::sync::atomic::AtomicUsize,
    /// Moves on with every durable write, while the records lock is held;
    /// each written record keeps the value in `StoredRecord::written`, so
    /// [`RecordRepository::written_since`] finds what changed since a
    /// reader last looked without snapshotting every record.
    write_generation: std::sync::atomic::AtomicU64,
    /// Announces each move of `write_generation`, so an exact-share host can
    /// hold a recipient's change await open instead of being polled
    /// ([`RecordRepository::subscribe_writes`]).
    writes: tokio::sync::watch::Sender<u64>,
    /// Concurrent-head checks made by
    /// [`crate::record_publisher::RecordPublisher::publish_concurrent_head_conflict`].
    #[cfg(test)]
    concurrent_head_checks: std::sync::atomic::AtomicU64,
}

type CachedCursors = (usize, std::sync::Arc<[RecordStreamCursor]>);
type Records = BTreeMap<RecordRepositoryKey, StoredRecord>;

/// Keeps mutable namespace projection serialized against every repository writer.
/// Acquire only after namespace (and history, if needed); never call public
/// repository methods while retaining this lease.
pub(crate) struct RecordProjectionLease<'a> {
    _records: std::sync::MutexGuard<'a, Records>,
    pub snapshot: RecordSnapshot,
}

pub(crate) struct RepositoryLease<'a> {
    repository: &'a RecordRepository,
    records: std::sync::MutexGuard<'a, Records>,
}

#[derive(Clone, Copy)]
enum ExternalObservation<'a> {
    None(std::marker::PhantomData<&'a ()>),
    #[cfg(all(feature = "filesystem", target_os = "linux"))]
    Completed(&'a crate::external_history::coordinator::ObservedExternalReplacement),
}
impl ExternalObservation<'_> {
    fn check(self, entry: &RecordTransactionJournalEntry) -> Result<(), RecordRepositoryError> {
        match self {
            #[cfg(all(feature = "filesystem", target_os = "linux"))]
            Self::Completed(observed) => observed.check_entry(entry).map_err(|_| {
                RecordRepositoryError::ExternalNativeEffectPending(entry.transaction_id())
            }),
            _ => Err(RecordRepositoryError::ExternalNativeEffectPending(
                entry.transaction_id(),
            )),
        }
    }
    fn supplied(self) -> bool {
        !matches!(self, Self::None(_))
    }
}

pub(crate) struct PreparedRecordTransaction {
    entry: RecordTransactionJournalEntry,
    journal_missing: bool,
    candidates: Vec<(RecordRepositoryKey, StoredRecord)>,
}

impl RepositoryLease<'_> {
    pub fn frontier(
        &self,
        key: RecordRepositoryKey,
    ) -> Result<Vec<RecordStreamCursor>, RecordRepositoryError> {
        let record = self
            .records
            .get(&key)
            .ok_or(RecordRepositoryError::RecordNotFound)?;
        record_stream_cursors(key, record)
    }

    pub fn snapshot(
        &self,
        key: RecordRepositoryKey,
    ) -> Result<RecordSnapshot, RecordRepositoryError> {
        projected_snapshot(&self.records, key, None)
    }

    pub fn highest_sequence(&self, key: RecordRepositoryKey, actor: &FileActor) -> u64 {
        self.records
            .get(&key)
            .and_then(|record| record.replica.highest_sequence_for(actor))
            .unwrap_or(0)
    }

    pub fn prepare<A: RecordAuthorizer + ?Sized>(
        &self,
        journal: &RecordTransactionJournal,
        entry: &RecordTransactionJournalEntry,
        authorizer: &A,
    ) -> Result<PreparedRecordTransaction, RecordRepositoryError> {
        self.prepare_inner(
            journal,
            entry,
            authorizer,
            ExternalObservation::None(std::marker::PhantomData),
        )
    }

    #[cfg(all(feature = "filesystem", target_os = "linux"))]
    pub(crate) fn prepare_external<A: RecordAuthorizer + ?Sized>(
        &self,
        journal: &RecordTransactionJournal,
        entry: &RecordTransactionJournalEntry,
        authorizer: &A,
        observed: &crate::external_history::coordinator::ObservedExternalReplacement,
    ) -> Result<PreparedRecordTransaction, RecordRepositoryError> {
        self.prepare_inner(
            journal,
            entry,
            authorizer,
            ExternalObservation::Completed(observed),
        )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "validates every participant and installs the fences under one R lease in a single pass"
    )]
    fn prepare_inner<A: RecordAuthorizer + ?Sized>(
        &self,
        journal: &RecordTransactionJournal,
        entry: &RecordTransactionJournalEntry,
        authorizer: &A,
        observed: ExternalObservation<'_>,
    ) -> Result<PreparedRecordTransaction, RecordRepositoryError> {
        entry
            .validate()
            .map_err(RecordRepositoryError::TransactionJournal)?;
        let transaction = &entry.transaction;
        let id = transaction.manifest.transaction_id;
        let workspace_id = transaction.manifest.workspace_id;
        // Inspect the persisted entry as well: stripping the caller's local
        // binding must not turn external recovery into ordinary record replay.
        if entry.external_native_effect.is_some()
            || journal
                .get(id)
                .is_some_and(|saved| saved.external_native_effect.is_some())
        {
            observed.check(entry)?;
        } else if observed.supplied() {
            return Err(RecordRepositoryError::ExternalNativeEffectPending(id));
        }
        let journal_missing = if let Some(existing) = journal.get(id) {
            if existing.transaction != entry.transaction
                || existing.participant_batches != entry.participant_batches
                || existing.external_native_effect != entry.external_native_effect
            {
                return Err(RecordRepositoryError::TransactionManifestMismatch(id));
            }
            false
        } else {
            if entry.phase != RecordTransactionPhase::Prepared {
                return Err(RecordRepositoryError::TransactionRecoveryIntentMissing);
            }
            journal
                .validate_prepared(entry)
                .map_err(RecordRepositoryError::TransactionJournal)?;
            true
        };

        let records = &*self.records;
        let missing_records = transaction
            .manifest
            .participants
            .iter()
            .filter(|participant| {
                !records.contains_key(&RecordRepositoryKey {
                    workspace_id,
                    record_id: participant.record_id,
                })
            })
            .count();
        if records.len().saturating_add(missing_records) > MAX_REPOSITORY_RECORDS {
            return Err(RecordRepositoryError::TooManyRecords);
        }
        let mut candidates = Vec::with_capacity(entry.participant_batches.len());
        for (participant, batch) in transaction
            .manifest
            .participants
            .iter()
            .zip(&entry.participant_batches)
        {
            let key = RecordRepositoryKey {
                workspace_id,
                record_id: participant.record_id,
            };
            let current = records.get(&key);
            if let Some(reserved) = current.and_then(|record| record.pending_restore)
                && reserved != id
            {
                return Err(RecordRepositoryError::RestoreRecoveryRequired(reserved));
            }
            let exact_suffix = current.is_some_and(|record| {
                record
                    .envelopes
                    .windows(batch.envelopes.len())
                    .any(|window| window == batch.envelopes)
            });
            let new_sibling = matches!(
                participant.role,
                fs_protocol::FileMutationParticipantRole::KeepBothSibling
            );
            if new_sibling {
                if current.is_some() && !exact_suffix {
                    return Err(RecordRepositoryError::TransactionPreconditionFailed(
                        participant.record_id,
                    ));
                }
            } else if !matches!(
                transaction.manifest.operation,
                fs_protocol::FileMutationOperation::RestoreContent { .. }
            ) || journal_missing
            {
                let snapshot = projected_snapshot(records, key, Some(id))?;
                if snapshot
                    .content_revision
                    .map(|revision| revision.version_id)
                    != Some(participant.expected_version)
                    && !snapshot
                        .versions
                        .iter()
                        .any(|version| version.id == participant.expected_version)
                {
                    return Err(RecordRepositoryError::TransactionPreconditionFailed(
                        participant.record_id,
                    ));
                }
                validate_structural_precondition(
                    &transaction.manifest,
                    participant.record_id,
                    &snapshot,
                )?;
            }

            let mut candidate = current
                .cloned()
                .unwrap_or_else(|| StoredRecord::new(key.record_id));
            if !exact_suffix {
                if candidate
                    .envelopes
                    .len()
                    .saturating_add(batch.envelopes.len())
                    > MAX_RECORD_OPERATIONS
                {
                    return Err(RecordRepositoryError::TooManyOperations);
                }
                for envelope in &batch.envelopes {
                    candidate.replica.merge(envelope, authorizer)?;
                    candidate.push_pending(envelope.clone());
                }
            }
            candidate.pending_restore = None;
            candidates.push((key, candidate));
        }

        Ok(PreparedRecordTransaction {
            entry: entry.clone(),
            journal_missing,
            candidates,
        })
    }

    pub fn commit<F: FnMut(RecordTransactionCheckpoint) -> Result<(), std::io::Error>>(
        &mut self,
        journal: &mut RecordTransactionJournal,
        prepared: PreparedRecordTransaction,
        mut checkpoint: F,
    ) -> Result<(), RecordRepositoryError> {
        let PreparedRecordTransaction {
            entry,
            journal_missing,
            candidates,
        } = prepared;
        let id = entry.transaction_id();
        // No durable side effect occurs until every participant candidate and
        // structural/version precondition above has succeeded.
        let restore = matches!(
            entry.transaction.manifest.operation,
            fs_protocol::FileMutationOperation::RestoreContent { .. }
        );
        if restore {
            // Fence before the first persistence attempt, including ambiguous
            // fsync errors. R remains held through intent installation.
            for (key, _) in &candidates {
                if let Some(record) = self.records.get_mut(key) {
                    record.pending_restore = Some(id);
                }
            }
        }
        if journal_missing {
            journal
                .insert_prepared(entry)
                .map_err(RecordRepositoryError::TransactionJournal)?;
            checkpoint(RecordTransactionCheckpoint::PreparedJournalDurable)
                .map_err(RecordRepositoryError::TransactionFailureInjected)?;
        } else if restore {
            journal
                .ensure_checked_intent_durable(id)
                .map_err(RecordRepositoryError::TransactionJournal)?;
        }
        // Every participant is written in one log transaction: they become
        // durable together.
        let participants = candidates.len();
        self.repository
            .commit_candidates(&mut self.records, candidates, None)?;
        for index in 0..participants {
            checkpoint(RecordTransactionCheckpoint::ParticipantRecordDurable { index })
                .map_err(RecordRepositoryError::TransactionFailureInjected)?;
        }
        if journal
            .get(id)
            .is_some_and(|value| value.phase == RecordTransactionPhase::Prepared)
        {
            journal
                .advance(
                    id,
                    RecordTransactionPhase::ParticipantRecordsCommitted,
                    None,
                )
                .map_err(RecordRepositoryError::TransactionJournal)?;
            checkpoint(RecordTransactionCheckpoint::ParticipantPhaseDurable)
                .map_err(RecordRepositoryError::TransactionFailureInjected)?;
        }
        Ok(())
    }
}

impl RecordRepository {
    pub(crate) fn lease(&self) -> Result<RepositoryLease<'_>, RecordRepositoryError> {
        Ok(RepositoryLease {
            repository: self,
            records: self
                .records
                .lock()
                .map_err(|_| RecordRepositoryError::StatePoisoned)?,
        })
    }

    fn lock_records(&self) -> Result<std::sync::MutexGuard<'_, Records>, RecordRepositoryError> {
        self.records
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)
    }

    /// Held for writing while a changeset is published or applied; readers
    /// that show state to peers or the live tree take it for reading.
    pub fn publication_gate(&self) -> &tokio::sync::RwLock<()> {
        &self.gate
    }

    /// Write every pending envelope of `candidates` to the log in one
    /// transaction (with `changeset` beside them), then publish the
    /// candidates in memory. On failure memory is unchanged.
    fn commit_candidates(
        &self,
        records: &mut Records,
        candidates: Vec<(RecordRepositoryKey, StoredRecord)>,
        changeset: Option<&Arc<SignedChangesetV1>>,
    ) -> Result<(), RecordRepositoryError> {
        let ops: Vec<(Uuid, &RecordEnvelopeV1)> = candidates
            .iter()
            .flat_map(|(key, record)| {
                record
                    .envelopes
                    .iter()
                    .zip(&record.positions)
                    .filter(|(_, position)| **position == PENDING)
                    .map(|(envelope, _)| (key.workspace_id, envelope))
            })
            .collect();
        let count = ops.len();
        let first = self.log.append(&ops, changeset.map(AsRef::as_ref))?;
        drop(ops);
        let generation = self.next_write_generation();
        let mut next = first;
        let mut written = Vec::with_capacity(candidates.len());
        for (key, mut record) in candidates {
            for position in record.positions.iter_mut().filter(|p| **p == PENDING) {
                *position = next;
                next = next.saturating_add(1);
            }
            record.pending = 0;
            record.written = generation;
            records.insert(key, record);
            written.push(key);
        }
        debug_assert_eq!(next, first.saturating_add(count as u64));
        if let Some(changeset) = changeset {
            self.changesets
                .lock()
                .map_err(|_| RecordRepositoryError::StatePoisoned)?
                .insert(Arc::clone(changeset), first);
        }
        self.forget_scope_keys(records, &written);
        Ok(())
    }

    /// Write the pending envelopes of `keys` in place, with `changeset`.
    /// On failure they stay pending.
    fn commit_in_place(
        &self,
        records: &mut Records,
        keys: &[RecordRepositoryKey],
        changeset: Option<&Arc<SignedChangesetV1>>,
    ) -> Result<(), RecordRepositoryError> {
        let first = {
            let ops: Vec<(Uuid, &RecordEnvelopeV1)> = keys
                .iter()
                .filter_map(|key| records.get(key).map(|record| (key, record)))
                .flat_map(|(key, record)| {
                    record
                        .envelopes
                        .iter()
                        .zip(&record.positions)
                        .filter(|(_, position)| **position == PENDING)
                        .map(|(envelope, _)| (key.workspace_id, envelope))
                })
                .collect();
            if ops.is_empty() && changeset.is_none() {
                return Ok(());
            }
            self.log.append(&ops, changeset.map(AsRef::as_ref))?
        };
        let generation = self.next_write_generation();
        let mut next = first;
        for key in keys {
            if let Some(record) = records.get_mut(key) {
                for position in record.positions.iter_mut().filter(|p| **p == PENDING) {
                    *position = next;
                    next = next.saturating_add(1);
                }
                record.pending = 0;
                record.written = generation;
            }
        }
        if let Some(changeset) = changeset {
            self.changesets
                .lock()
                .map_err(|_| RecordRepositoryError::StatePoisoned)?
                .insert(Arc::clone(changeset), first);
        }
        Ok(())
    }

    /// The generation a durable write about to publish its records takes.
    /// Called with the records lock held.
    fn next_write_generation(&self) -> u64 {
        let generation = self
            .write_generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            .saturating_add(1);
        // A woken watcher reads the records through the lock this writer
        // holds, so it sees the write once the writer publishes it.
        self.writes.send_replace(generation);
        generation
    }

    /// Records a write outside the scope made durable are no longer the
    /// scope's to sign.
    fn forget_scope_keys<'a>(
        &self,
        records: &Records,
        written: impl IntoIterator<Item = &'a RecordRepositoryKey>,
    ) {
        if let Ok(mut scope) = self.scope.lock()
            && let Some(scope) = scope.as_mut()
        {
            let mut forgotten = false;
            for key in written {
                if scope.touched.contains(key)
                    && records.get(key).is_none_or(|record| record.pending == 0)
                {
                    scope.touched.remove(key);
                    forgotten = true;
                }
            }
            if forgotten {
                let touched = &scope.touched;
                scope.keys.retain(|key| touched.contains(key));
            }
        }
    }

    /// Whether an envelope by `actor` in `workspace_id` written now belongs
    /// to this thread's open changeset scope.
    fn deferred(&self, workspace_id: Uuid, actor: &FileActor) -> bool {
        self.scope.lock().is_ok_and(|scope| {
            scope.as_ref().is_some_and(|scope| {
                scope.thread == Some(std::thread::current().id())
                    && scope.workspace_id == workspace_id
                    && scope.actor == *actor
            })
        })
    }

    /// Open a changeset scope on this thread: `actor`'s envelopes applied
    /// from this thread are kept in memory (visible to the local publisher
    /// only) until [`Self::commit_changeset`].
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the scope lock covers checking and installing the scope"
    )]
    pub fn begin_changeset(
        &self,
        workspace_id: Uuid,
        actor: FileActor,
    ) -> Result<(), RecordRepositoryError> {
        let mut scope = self
            .scope
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)?;
        let thread = std::thread::current().id();
        match scope.as_mut() {
            Some(open) if open.thread.is_some_and(|t| t != thread) => {
                return Err(RecordRepositoryError::ChangesetInProgress);
            }
            // A scope whose commit failed: its envelopes go into this one.
            Some(open) if open.workspace_id == workspace_id && open.actor == actor => {
                open.thread = Some(thread);
            }
            Some(_) => return Err(RecordRepositoryError::ChangesetInProgress),
            None => {
                *scope = Some(ChangesetScope {
                    thread: Some(thread),
                    workspace_id,
                    actor,
                    keys: Vec::new(),
                    touched: HashSet::new(),
                });
            }
        }
        Ok(())
    }

    /// Sign the open scope's envelopes as one changeset and write them with
    /// it in one durable transaction. Returns `None` when the scope wrote
    /// nothing. On failure the envelopes stay pending, hidden from peers,
    /// and the next scope of the same actor carries them.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the records lock spans signing and the durable write, so nothing changes the batch meanwhile"
    )]
    pub fn commit_changeset(
        &self,
        device_key: &ed25519_dalek::SigningKey,
        created_at_ms: u64,
    ) -> Result<Option<Arc<SignedChangesetV1>>, RecordRepositoryError> {
        let mut records = self.lock_records()?;
        let Some(mut scope) = self
            .scope
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)?
            .take()
        else {
            return Ok(None);
        };
        let mut leaves = Vec::with_capacity(scope.keys.len());
        for key in &scope.keys {
            let Some(record) = records.get(key) else {
                continue;
            };
            let pending: Vec<&RecordEnvelopeV1> = record
                .envelopes
                .iter()
                .zip(&record.positions)
                .filter(|(envelope, position)| {
                    **position == PENDING && envelope.actor == scope.actor
                })
                .map(|(envelope, _)| envelope)
                .collect();
            let (Some(first), Some(last)) = (pending.first(), pending.last()) else {
                continue;
            };
            leaves.push(ChangesetLeafV1 {
                record_id: key.record_id,
                first_sequence: first.sequence,
                last_sequence: last.sequence,
                digest: envelopes_digest(pending.iter().copied())?,
            });
        }
        if leaves.is_empty() {
            return Ok(None);
        }
        let sequence = self
            .changesets
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)?
            .next_sequence(scope.workspace_id, *scope.actor.device.as_bytes());
        let signed = Arc::new(SignedChangesetV1::sign(
            scope.workspace_id,
            scope.actor,
            sequence,
            created_at_ms,
            leaves,
            device_key,
        )?);
        let keys = std::mem::take(&mut scope.keys);
        match self.commit_in_place(&mut records, &keys, Some(&signed)) {
            Ok(()) => Ok(Some(signed)),
            Err(error) => {
                scope.keys = keys;
                scope.thread = None;
                if let Ok(mut slot) = self.scope.lock() {
                    *slot = Some(scope);
                }
                Err(error)
            }
        }
    }

    /// Seed admission fences before exposing a reopened repository to writers.
    /// Caller retains J; this method only acquires R. No second durable owner.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "all admission fences are seeded under one R lease"
    )]
    pub fn restore_admission_fences(
        &self,
        journal: &RecordTransactionJournal,
    ) -> Result<(), RecordRepositoryError> {
        let mut records = self.lock_records()?;
        for entry in journal.entries().filter(|entry| {
            entry.phase == RecordTransactionPhase::Prepared
                && (entry.external_native_effect.is_some()
                    || matches!(
                        entry.transaction.manifest.operation,
                        fs_protocol::FileMutationOperation::RestoreContent { .. }
                    ))
        }) {
            for participant in &entry.transaction.manifest.participants {
                let key = RecordRepositoryKey {
                    workspace_id: entry.transaction.manifest.workspace_id,
                    record_id: participant.record_id,
                };
                let record = records
                    .get_mut(&key)
                    .ok_or(RecordRepositoryError::RecordNotFound)?;
                if record
                    .pending_restore
                    .is_some_and(|id| id != entry.transaction_id())
                {
                    return Err(RecordRepositoryError::TransactionManifestMismatch(
                        entry.transaction_id(),
                    ));
                }
                record.pending_restore = Some(entry.transaction_id());
            }
        }
        Ok(())
    }

    /// Acknowledge repository participation already received or reopened from
    /// disk. J must be held by the caller; this method acquires only R. It
    /// admits no new signed operations and needs no local-author attribution.
    ///
    /// All exact batches and their committed transaction state are checked
    /// before any persistence. Writing whatever of the verified current
    /// records is still pending establishes durability even after an
    /// ambiguous earlier write error and retains any later accepted
    /// envelopes. A phase alone never clears a fence.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the records lock spans validation, the durable write and clearing the fences"
    )]
    pub(crate) fn complete_admitted_restore_participation(
        &self,
        journal: &mut RecordTransactionJournal,
        entry: &RecordTransactionJournalEntry,
    ) -> Result<(), RecordRepositoryError> {
        entry.validate()?;
        let id = entry.transaction_id();
        let admitted = journal
            .get(id)
            .ok_or(RecordRepositoryError::TransactionRecoveryIntentMissing)?;
        if !matches!(
            entry.transaction.manifest.operation,
            fs_protocol::FileMutationOperation::RestoreContent { .. }
        ) || admitted.transaction != entry.transaction
            || admitted.participant_batches != entry.participant_batches
        {
            return Err(RecordRepositoryError::TransactionManifestMismatch(id));
        }
        let mut records = self.lock_records()?;
        let mut keys = Vec::with_capacity(entry.participant_batches.len());
        for batch in &entry.participant_batches {
            let key = RecordRepositoryKey {
                workspace_id: entry.transaction.manifest.workspace_id,
                record_id: batch.record_id,
            };
            let record = records
                .get(&key)
                .ok_or(RecordRepositoryError::RecordNotFound)?;
            if let Some(reserved) = record.pending_restore
                && reserved != id
            {
                return Err(RecordRepositoryError::RestoreRecoveryRequired(reserved));
            }
            // Different actor streams may interleave in the accepted log.
            // Match both exact signed envelopes, not their adjacency.
            let exact_batch = batch
                .envelopes
                .iter()
                .all(|envelope| record.envelopes.contains(envelope));
            let committed =
                record
                    .replica
                    .transaction_state(id)
                    .is_some_and(|(transaction, committed)| {
                        committed && transaction == &entry.transaction
                    });
            if !exact_batch || !committed {
                return Err(RecordRepositoryError::TransactionPreconditionFailed(
                    batch.record_id,
                ));
            }
            keys.push(key);
        }

        journal.ensure_checked_intent_durable(id)?;
        if keys
            .iter()
            .any(|key| records.get(key).is_some_and(|record| record.pending > 0))
        {
            self.commit_in_place(&mut records, &keys, None)?;
        } else {
            // Already in the log; an empty commit is the durability barrier
            // after an ambiguous earlier write error.
            self.log.append(&[], None)?;
        }
        self.forget_scope_keys(&records, &keys);
        if journal
            .get(id)
            .is_some_and(|entry| entry.phase == RecordTransactionPhase::Prepared)
        {
            journal.advance(
                id,
                RecordTransactionPhase::ParticipantRecordsCommitted,
                None,
            )?;
        }
        for key in keys {
            #[expect(
                clippy::expect_used,
                reason = "every key was looked up in `records` above under the same R lease"
            )]
            let record = records
                .get_mut(&key)
                .expect("validated under the same R lease");
            if record.pending_restore == Some(id) {
                record.pending_restore = None;
            }
        }
        Ok(())
    }

    /// Open the repository at `root` and replay its operation log.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, RecordRepositoryError> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        let canonical = std::fs::canonicalize(&root)?;
        let mut open = open_repositories()
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)?;
        open.retain(|_, state| state.strong_count() > 0);
        if let Some(state) = open.get(&canonical).and_then(std::sync::Weak::upgrade) {
            return Ok(Self { state });
        }
        let state = Arc::new(Self::load(root)?);
        open.insert(canonical, Arc::downgrade(&state));
        drop(open);
        Ok(Self { state })
    }

    fn load(root: PathBuf) -> Result<RepositoryState, RecordRepositoryError> {
        let log = RecordLog::open(&root.join(LOG_FILE))?;
        let mut records = Records::new();
        log.replay_ops(|position, workspace_id, envelope| {
            let key = RecordRepositoryKey {
                workspace_id,
                record_id: envelope.record_id,
            };
            let record = records
                .entry(key)
                .or_insert_with(|| StoredRecord::new(key.record_id));
            // A commit that reported failure but reached the disk is
            // written again by the retry: the same envelope twice.
            if record
                .replica
                .highest_sequence_for(&envelope.actor)
                .is_some_and(|highest| envelope.sequence <= highest)
                && record.envelopes.contains(&envelope)
            {
                return Ok(());
            }
            record.replica.restore_trusted(&envelope)?;
            record.envelopes.push(envelope);
            record.positions.push(position);
            Ok::<(), RecordRepositoryError>(())
        })?;
        if records.len() > MAX_REPOSITORY_RECORDS {
            return Err(RecordRepositoryError::TooManyRecords);
        }
        let mut index = ChangesetIndex::default();
        for logged in log.changesets()? {
            index.insert(Arc::new(logged.changeset), logged.first_position);
        }
        Ok(RepositoryState {
            root,
            log,
            records: Mutex::new(records),
            cursors: Mutex::new(BTreeMap::new()),
            scope: Mutex::new(None),
            changesets: Mutex::new(index),
            gate: tokio::sync::RwLock::new(()),
            max_versions: std::sync::atomic::AtomicUsize::new(MAX_RECORD_VERSIONS),
            write_generation: std::sync::atomic::AtomicU64::new(0),
            writes: tokio::sync::watch::Sender::new(0),
            #[cfg(test)]
            concurrent_head_checks: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Versions one record may hold before a write is refused.
    pub(crate) fn max_record_versions(&self) -> usize {
        self.max_versions.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Lower the per-record version bound, so a test crosses it without
    /// writing [`MAX_RECORD_VERSIONS`] versions.
    #[cfg(test)]
    pub(crate) fn set_max_record_versions(&self, versions: usize) {
        self.max_versions
            .store(versions, std::sync::atomic::Ordering::Relaxed);
    }

    /// Make every durable write fail (or succeed again).
    #[cfg(test)]
    pub(crate) fn fail_writes(&self, fail: bool) {
        self.log
            .fail_appends
            .store(fail, std::sync::atomic::Ordering::Relaxed);
    }

    /// Forget the transient admission fences, as a restarted process has
    /// none until [`Self::restore_admission_fences`] seeds them.
    #[cfg(test)]
    pub(crate) fn clear_admission_fences(&self) {
        for record in self.records.lock_recover().values_mut() {
            record.pending_restore = None;
        }
    }

    /// The directory this repository lives in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn reserve_external_operation(
        &self,
        input: &crate::record_log::ExternalReservationInput,
    ) -> Result<crate::record_log::ExternalCanonicalIds, RecordRepositoryError> {
        Ok(self.log.reserve_external_operation(input)?)
    }

    /// Operations in the durable log.
    ///
    /// # Errors
    ///
    /// Returns an error when the log cannot be read.
    pub fn logged_operations(&self) -> Result<u64, RecordRepositoryError> {
        Ok(self.log.op_count()?)
    }

    /// Apply an already-signed envelope through the supplied live authorizer,
    /// persist the candidate state, then publish it in memory. If validation,
    /// authorization or persistence fails, the live repository is unchanged.
    pub fn apply<A: RecordAuthorizer + ?Sized>(
        &self,
        key: RecordRepositoryKey,
        envelope: &RecordEnvelopeV1,
        authorizer: &A,
    ) -> Result<(), RecordRepositoryError> {
        self.apply_batch(key, std::slice::from_ref(envelope), authorizer)
    }

    /// Validate one batch against `records` and the candidates staged so
    /// far, and stage its candidate.
    fn stage_batch<A: RecordAuthorizer + ?Sized>(
        &self,
        records: &Records,
        candidates: &mut BTreeMap<RecordRepositoryKey, StoredRecord>,
        key: RecordRepositoryKey,
        envelopes: &[RecordEnvelopeV1],
        authorizer: &A,
        signed_here: bool,
    ) -> Result<(), RecordRepositoryError> {
        if envelopes.is_empty() || envelopes.len() > MAX_RECORD_BATCH_OPERATIONS {
            return Err(RecordRepositoryError::InvalidBatchSize);
        }
        if envelopes
            .iter()
            .any(|envelope| envelope.record_id != key.record_id)
        {
            return Err(RecordRepositoryError::CrossRecordEnvelope);
        }
        if envelopes.iter().any(|envelope| {
            matches!(
                &envelope.operation,
                crate::record_replication::RecordOperationV1::PrepareTransaction(transaction)
                    if transaction.manifest.workspace_id != key.workspace_id
            )
        }) {
            return Err(RecordRepositoryError::TransactionWorkspaceMismatch);
        }
        let current = candidates.get(&key).or_else(|| records.get(&key));
        if let Some(id) = current.and_then(|record| record.pending_restore) {
            return Err(RecordRepositoryError::RestoreRecoveryRequired(id));
        }
        if current.is_none()
            && records.len().saturating_add(candidates.len()) >= MAX_REPOSITORY_RECORDS
        {
            return Err(RecordRepositoryError::TooManyRecords);
        }
        if current
            .map_or(0, |record| record.envelopes.len())
            .saturating_add(envelopes.len())
            > MAX_RECORD_OPERATIONS
        {
            return Err(RecordRepositoryError::TooManyOperations);
        }
        let mut candidate = current
            .cloned()
            .unwrap_or_else(|| StoredRecord::new(key.record_id));
        for envelope in envelopes {
            if signed_here {
                candidate.replica.merge_own(envelope, authorizer)?;
            } else {
                candidate.replica.merge(envelope, authorizer)?;
            }
            candidate.push_pending(envelope.clone());
        }
        if candidate.replica.version_count() > self.max_record_versions() {
            return Err(RecordRepositoryError::TooManyVersions);
        }
        if candidate.replica.conflict_count() > MAX_RECORD_CONFLICTS {
            return Err(RecordRepositoryError::TooManyConflicts);
        }
        candidates.insert(key, candidate);
        Ok(())
    }

    /// Atomically validate and persist one bounded, topologically ordered
    /// prefix. No envelope becomes visible if any later envelope in the batch
    /// fails authentication, authorization, sequence, DAG, or size checks.
    ///
    /// Inside this thread's changeset scope, the local author's batch is
    /// accepted in memory and written when the changeset commits.
    pub fn apply_batch<A: RecordAuthorizer + ?Sized>(
        &self,
        key: RecordRepositoryKey,
        envelopes: &[RecordEnvelopeV1],
        authorizer: &A,
    ) -> Result<(), RecordRepositoryError> {
        self.apply_batch_from(key, envelopes, authorizer, false)
    }

    /// [`Self::apply_batch`] for envelopes the caller signed itself, with
    /// the actor's device key, just now: their signatures are not checked
    /// again.
    pub(crate) fn apply_own_batch<A: RecordAuthorizer + ?Sized>(
        &self,
        key: RecordRepositoryKey,
        envelopes: &[RecordEnvelopeV1],
        authorizer: &A,
    ) -> Result<(), RecordRepositoryError> {
        self.apply_batch_from(key, envelopes, authorizer, true)
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the writer lock intentionally spans batch validation, durable replacement, and in-memory publication"
    )]
    fn apply_batch_from<A: RecordAuthorizer + ?Sized>(
        &self,
        key: RecordRepositoryKey,
        envelopes: &[RecordEnvelopeV1],
        authorizer: &A,
        signed_here: bool,
    ) -> Result<(), RecordRepositoryError> {
        let deferred = envelopes
            .first()
            .is_some_and(|first| self.deferred(key.workspace_id, &first.actor))
            && envelopes
                .windows(2)
                .all(|pair| matches!(pair, [a, b] if a.actor == b.actor));
        let mut records = self.lock_records()?;
        let mut candidates = BTreeMap::new();
        self.stage_batch(
            &records,
            &mut candidates,
            key,
            envelopes,
            authorizer,
            signed_here,
        )?;
        if deferred {
            records.extend(candidates);
            if let Ok(mut scope) = self.scope.lock()
                && let Some(scope) = scope.as_mut()
                && scope.touched.insert(key)
            {
                scope.keys.push(key);
            }
            return Ok(());
        }
        self.commit_candidates(&mut records, candidates.into_iter().collect(), None)
    }

    /// [`Self::apply_batch`] for several streams at once, all or nothing, in
    /// one durable write. Batches for the same record apply in order.
    ///
    /// # Errors
    ///
    /// As [`Self::apply_batch`], for any batch.
    pub fn apply_batches<A: RecordAuthorizer + ?Sized>(
        &self,
        batches: &[(RecordRepositoryKey, Vec<RecordEnvelopeV1>)],
        authorizer: &A,
    ) -> Result<(), RecordRepositoryError> {
        if batches.is_empty() {
            return Ok(());
        }
        let mut records = self.lock_records()?;
        let mut candidates = BTreeMap::new();
        for (key, envelopes) in batches {
            self.stage_batch(
                &records,
                &mut candidates,
                *key,
                envelopes,
                authorizer,
                false,
            )?;
        }
        self.commit_candidates(&mut records, candidates.into_iter().collect(), None)
    }

    /// Verify a received changeset and apply all of its envelopes in one
    /// durable transaction, or none of them. `envelopes` must be exactly
    /// the envelopes its leaves name; ones this device already holds (the
    /// same bytes) are skipped.
    #[allow(
        clippy::too_many_lines,
        clippy::significant_drop_tightening,
        reason = "one pass validates every leaf against the signed set under the records lock before a single durable write"
    )]
    pub fn apply_changeset<A: RecordAuthorizer + ?Sized>(
        &self,
        workspace_id: Uuid,
        changeset: &SignedChangesetV1,
        envelopes: &[RecordEnvelopeV1],
        authorizer: &A,
    ) -> Result<ChangesetApplied, RecordRepositoryError> {
        changeset.verify()?;
        let header = &changeset.header;
        if header.workspace_id != workspace_id {
            return Err(RecordRepositoryError::TransactionWorkspaceMismatch);
        }
        if let Some((existing, _)) = self
            .changesets
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)?
            .by_id
            .get(&header.id())
        {
            return if **existing == *changeset {
                Ok(ChangesetApplied::AlreadyPresent)
            } else {
                Err(RecordRepositoryError::ChangesetEquivocation)
            };
        }
        let mut by_record: BTreeMap<FileRecordId, Vec<&RecordEnvelopeV1>> = BTreeMap::new();
        for envelope in envelopes {
            if envelope.actor != header.actor {
                return Err(RecordRepositoryError::ChangesetMismatch);
            }
            by_record
                .entry(envelope.record_id)
                .or_default()
                .push(envelope);
        }
        if by_record.len() != changeset.leaves.len() {
            return Err(RecordRepositoryError::ChangesetMismatch);
        }
        for leaf in &changeset.leaves {
            let list = by_record
                .get_mut(&leaf.record_id)
                .ok_or(RecordRepositoryError::ChangesetMismatch)?;
            list.sort_by_key(|envelope| envelope.sequence);
            let contiguous = list.first().map(|e| e.sequence) == Some(leaf.first_sequence)
                && list
                    .iter()
                    .zip(leaf.first_sequence..)
                    .all(|(envelope, sequence)| envelope.sequence == sequence)
                && list.last().map(|e| e.sequence) == Some(leaf.last_sequence);
            if !contiguous || envelopes_digest(list.iter().copied())? != leaf.digest {
                return Err(RecordRepositoryError::ChangesetMismatch);
            }
        }

        let mut records = self.lock_records()?;
        let mut candidates: Vec<(RecordRepositoryKey, StoredRecord)> =
            Vec::with_capacity(changeset.leaves.len());
        let mut new_records = 0_usize;
        for leaf in &changeset.leaves {
            let key = RecordRepositoryKey {
                workspace_id,
                record_id: leaf.record_id,
            };
            let current = records.get(&key);
            if let Some(id) = current.and_then(|record| record.pending_restore) {
                return Err(RecordRepositoryError::RestoreRecoveryRequired(id));
            }
            if current.is_none() {
                new_records = new_records.saturating_add(1);
            }
            let mut candidate = current
                .cloned()
                .unwrap_or_else(|| StoredRecord::new(key.record_id));
            let list = by_record
                .get(&leaf.record_id)
                .ok_or(RecordRepositoryError::ChangesetMismatch)?;
            for envelope in list {
                let highest = candidate
                    .replica
                    .highest_sequence_for(&envelope.actor)
                    .unwrap_or(0);
                if envelope.sequence <= highest {
                    let same = candidate.envelopes.iter().any(|stored| {
                        stored.actor == envelope.actor
                            && stored.sequence == envelope.sequence
                            && stored == *envelope
                    });
                    if !same {
                        return Err(RecordRepositoryError::DivergentStream(leaf.record_id));
                    }
                    continue;
                }
                // The verified changeset signature covers this envelope's
                // exact bytes (its leaf digest): one verification for the
                // whole changeset, as `storage.md` §8.3 has it.
                candidate
                    .replica
                    .merge_authenticated(envelope, authorizer)?;
                candidate.push_pending((*envelope).clone());
            }
            if candidate.envelopes.len() > MAX_RECORD_OPERATIONS {
                return Err(RecordRepositoryError::TooManyOperations);
            }
            if candidate.replica.version_count() > self.max_record_versions() {
                return Err(RecordRepositoryError::TooManyVersions);
            }
            if candidate.replica.conflict_count() > MAX_RECORD_CONFLICTS {
                return Err(RecordRepositoryError::TooManyConflicts);
            }
            candidates.push((key, candidate));
        }
        if records.len().saturating_add(new_records) > MAX_REPOSITORY_RECORDS {
            return Err(RecordRepositoryError::TooManyRecords);
        }
        let touched = candidates
            .iter()
            .filter(|(_, record)| record.pending > 0)
            .map(|(key, _)| *key)
            .collect();
        let changeset = Arc::new(changeset.clone());
        self.commit_candidates(&mut records, candidates, Some(&changeset))?;
        Ok(ChangesetApplied::Applied(touched))
    }

    /// The envelopes one leaf of a durable changeset names.
    pub fn changeset_leaf_envelopes(
        &self,
        changeset: &SignedChangesetV1,
        leaf: &ChangesetLeafV1,
    ) -> Result<Vec<RecordEnvelopeV1>, RecordRepositoryError> {
        let key = RecordRepositoryKey {
            workspace_id: changeset.header.workspace_id,
            record_id: leaf.record_id,
        };
        let records = self.lock_records()?;
        let record = records
            .get(&key)
            .ok_or(RecordRepositoryError::RecordNotFound)?;
        let envelopes: Vec<RecordEnvelopeV1> = record
            .durable()
            .filter(|envelope| {
                envelope.actor == changeset.header.actor
                    && (leaf.first_sequence..=leaf.last_sequence).contains(&envelope.sequence)
            })
            .cloned()
            .collect();
        drop(records);
        let expected = leaf
            .last_sequence
            .saturating_sub(leaf.first_sequence)
            .saturating_add(1);
        if envelopes.len() as u64 != expected {
            return Err(RecordRepositoryError::ChangesetMismatch);
        }
        Ok(envelopes)
    }

    /// What a peer that holds each stream up to `after_sequence` is missing,
    /// as whole changesets and runs of envelopes outside any changeset, in
    /// the order they entered this device's log.
    pub fn pull_units(
        &self,
        workspace_id: Uuid,
        streams: &[(FileRecordId, FileActor, u64)],
    ) -> Result<Vec<PullUnit>, RecordRepositoryError> {
        let records = self.lock_records()?;
        let index = self
            .changesets
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)?;
        let mut changesets: BTreeMap<ChangesetId, u64> = BTreeMap::new();
        let mut runs: Vec<(u64, PullUnit)> = Vec::new();
        for (record_id, actor, after) in streams {
            let key = RecordRepositoryKey {
                workspace_id,
                record_id: *record_id,
            };
            let Some(record) = records.get(&key) else {
                continue;
            };
            let mut run: Option<(u64, u64, Vec<RecordEnvelopeV1>)> = None;
            let flush = |run: &mut Option<(u64, u64, Vec<RecordEnvelopeV1>)>,
                         runs: &mut Vec<(u64, PullUnit)>| {
                if let Some((position, after_sequence, envelopes)) = run.take() {
                    for chunk in envelopes.chunks(MAX_RECORD_BATCH_OPERATIONS) {
                        let after_sequence = chunk
                            .first()
                            .map_or(after_sequence, |e| e.sequence.saturating_sub(1));
                        runs.push((
                            position,
                            PullUnit::Stream {
                                key,
                                actor: *actor,
                                after_sequence,
                                envelopes: chunk.to_vec(),
                            },
                        ));
                    }
                }
            };
            for (envelope, position) in record.envelopes.iter().zip(&record.positions) {
                if *position == PENDING || envelope.actor != *actor || envelope.sequence <= *after {
                    continue;
                }
                if let Some(id) = index.member(key, actor, envelope.sequence) {
                    flush(&mut run, &mut runs);
                    if let Some((_, first)) = index.by_id.get(&id) {
                        changesets.insert(id, *first);
                    }
                    continue;
                }
                match run.as_mut() {
                    Some((_, _, envelopes)) => envelopes.push(envelope.clone()),
                    None => {
                        run = Some((
                            *position,
                            envelope.sequence.saturating_sub(1),
                            vec![envelope.clone()],
                        ));
                    }
                }
            }
            flush(&mut run, &mut runs);
        }
        for (id, first) in changesets {
            if let Some((changeset, _)) = index.by_id.get(&id) {
                runs.push((first, PullUnit::Changeset(Arc::clone(changeset))));
            }
        }
        drop(index);
        drop(records);
        runs.sort_by_key(|(position, _)| *position);
        Ok(runs.into_iter().map(|(_, unit)| unit).collect())
    }

    /// Inspect the exact participant set without projecting a partial commit.
    /// Differing signed manifests under one transaction ID fail closed.
    #[cfg(test)]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "one repository read lock provides a consistent cross-record transaction snapshot"
    )]
    pub fn transaction_status(
        &self,
        workspace_id: Uuid,
        transaction_id: Uuid,
    ) -> Result<Option<RecordTransactionStatus>, RecordRepositoryError> {
        let records = self.lock_records()?;
        let mut transaction: Option<FileMutationTransaction> = None;
        let mut prepared = 0_usize;
        let mut committed = 0_usize;
        for record in records.values() {
            let Some((candidate, is_committed)) = record.replica.transaction_state(transaction_id)
            else {
                continue;
            };
            if candidate.manifest.workspace_id != workspace_id {
                continue;
            }
            if transaction
                .as_ref()
                .is_some_and(|current| current != candidate)
            {
                return Err(RecordRepositoryError::TransactionManifestMismatch(
                    transaction_id,
                ));
            }
            transaction.get_or_insert_with(|| candidate.clone());
            prepared = prepared.saturating_add(1);
            committed = committed.saturating_add(usize::from(is_committed));
        }
        let Some(transaction) = transaction else {
            return Ok(None);
        };
        let expected = transaction.manifest.participants.len();
        Ok(Some(RecordTransactionStatus {
            transaction,
            prepared_participants: prepared,
            committed_participants: committed,
            complete: prepared == expected && committed == expected,
        }))
    }

    /// Validate every participant against one locked repository snapshot,
    /// durably journal the exact intent, then write all participants in one
    /// log transaction. Memory is published only after they are durable. A
    /// crash after the journal write leaves an invisible prepared record
    /// which exact retry completes from the journal.
    pub fn apply_transaction<A: RecordAuthorizer + ?Sized>(
        &self,
        journal: &mut RecordTransactionJournal,
        entry: &RecordTransactionJournalEntry,
        authorizer: &A,
    ) -> Result<(), RecordRepositoryError> {
        self.apply_transaction_with_checkpoint(journal, entry, authorizer, |_| Ok(()))
    }

    /// Failure-injectable form used by deterministic crash-recovery tests.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::apply_transaction`], plus a supplied
    /// checkpoint failure.
    pub fn apply_transaction_with_checkpoint<A, F>(
        &self,
        journal: &mut RecordTransactionJournal,
        entry: &RecordTransactionJournalEntry,
        authorizer: &A,
        checkpoint: F,
    ) -> Result<(), RecordRepositoryError>
    where
        A: RecordAuthorizer + ?Sized,
        F: FnMut(RecordTransactionCheckpoint) -> Result<(), std::io::Error>,
    {
        let mut lease = self.lease()?;
        let prepared = lease.prepare(journal, entry, authorizer)?;
        lease.commit(journal, prepared, checkpoint)
    }

    /// Bounded per-record, per-actor inventory for workspace record-mesh
    /// anti-entropy. Callers must perform workspace-root read authorization
    /// before invoking this method.
    pub fn inventory_page(
        &self,
        workspace_id: Uuid,
        after: Option<RecordStreamKey>,
        limit: usize,
    ) -> Result<Vec<RecordStreamCursor>, RecordRepositoryError> {
        if limit == 0 || limit > MAX_RECORD_BATCH_OPERATIONS {
            return Err(RecordRepositoryError::InvalidBatchSize);
        }
        // Pages start at `after`'s record: earlier records are not visited,
        // and cached cursors are not recomputed.
        Ok(self
            .stream_cursors_in(workspace_id, after, None, None)?
            .into_iter()
            .filter(|cursor| after.is_none_or(|after| cursor.key() > after))
            .take(limit)
            .collect())
    }

    /// Every durable stream cursor of one workspace with a key in
    /// `[lower, upper)` (unbounded where `None`), optionally only streams
    /// whose actor is `person`, in stream-key order. This is the item set of
    /// range-based reconciliation; callers authorise the requester first.
    pub fn stream_cursors_in(
        &self,
        workspace_id: Uuid,
        lower: Option<RecordStreamKey>,
        upper: Option<RecordStreamKey>,
        person: Option<[u8; 32]>,
    ) -> Result<Vec<RecordStreamCursor>, RecordRepositoryError> {
        let records = self.lock_records()?;
        let mut cache = self
            .cursors
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)?;
        let first = RecordRepositoryKey {
            workspace_id,
            record_id: lower.map_or(FileRecordId(Uuid::nil()), |key| key.record_id),
        };
        let mut out = Vec::new();
        for (key, record) in records.range(first..) {
            if key.workspace_id != workspace_id
                || upper.is_some_and(|upper| key.record_id > upper.record_id)
            {
                break;
            }
            let durable = record.durable_len();
            if durable == 0 {
                continue;
            }
            let cursors = match cache.get(key) {
                Some((count, cursors)) if *count == durable => std::sync::Arc::clone(cursors),
                _ => {
                    let cursors: std::sync::Arc<[RecordStreamCursor]> =
                        record_stream_cursors(*key, record)?.into();
                    cache.insert(*key, (durable, std::sync::Arc::clone(&cursors)));
                    cursors
                }
            };
            out.extend(cursors.iter().copied().filter(|cursor| {
                let stream = cursor.key();
                lower.is_none_or(|lower| stream >= lower)
                    && upper.is_none_or(|upper| stream < upper)
                    && person.is_none_or(|person| *cursor.actor.person.as_bytes() == person)
            }));
        }
        drop(cache);
        drop(records);
        Ok(out)
    }

    /// Return a bounded contiguous durable suffix for one actor stream.
    pub fn envelopes_after(
        &self,
        key: RecordRepositoryKey,
        actor: &FileActor,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<RecordEnvelopeV1>, RecordRepositoryError> {
        if limit == 0 || limit > MAX_RECORD_BATCH_OPERATIONS {
            return Err(RecordRepositoryError::InvalidBatchSize);
        }
        let records = self.lock_records()?;
        let record = records
            .get(&key)
            .filter(|record| record.durable_len() > 0)
            .ok_or(RecordRepositoryError::RecordNotFound)?;
        if after_sequence > record.durable_highest(actor) {
            return Err(RecordRepositoryError::CursorAhead);
        }
        let envelopes = record
            .durable()
            .filter(|envelope| envelope.actor == *actor && envelope.sequence > after_sequence)
            .take(limit)
            .cloned()
            .collect();
        drop(records);
        Ok(envelopes)
    }

    /// Recompute the authenticated durable prefix cursor through an exact
    /// sequence. This detects equal-watermark divergence before a mesh peer
    /// accepts a suffix based on a different history.
    pub fn stream_cursor(
        &self,
        key: RecordRepositoryKey,
        actor: &FileActor,
        through_sequence: u64,
    ) -> Result<RecordStreamCursor, RecordRepositoryError> {
        let records = self.lock_records()?;
        let record = records
            .get(&key)
            .filter(|record| record.durable_len() > 0)
            .ok_or(RecordRepositoryError::RecordNotFound)?;
        if through_sequence == 0 || through_sequence > record.durable_highest(actor) {
            return Err(RecordRepositoryError::CursorAhead);
        }
        let envelopes = record
            .durable()
            .filter(|envelope| envelope.actor == *actor && envelope.sequence <= through_sequence)
            .collect::<Vec<_>>();
        if envelopes.len() != usize::try_from(through_sequence).unwrap_or(usize::MAX) {
            return Err(RecordRepositoryError::CursorAhead);
        }
        let prefix_digest = envelope_prefix_digest(envelopes)?;
        drop(records);
        Ok(RecordStreamCursor {
            record_id: key.record_id,
            actor: *actor,
            contiguous_sequence: through_sequence,
            prefix_digest,
        })
    }

    /// Return a bounded record snapshot after a fresh read/certification
    /// decision. The repository's version limit makes the clone bounded.
    pub fn snapshot<A: RecordAuthorizer + ?Sized>(
        &self,
        key: RecordRepositoryKey,
        requester: &FileActor,
        authorizer: &A,
    ) -> Result<RecordSnapshot, RecordRepositoryError> {
        let records = self.lock_records()?;
        let record = records
            .get(&key)
            .ok_or(RecordRepositoryError::RecordNotFound)?;
        record.replica.snapshot(requester, authorizer)?;
        let snapshot = projected_snapshot(&records, key, None)?;
        drop(records);
        Ok(snapshot)
    }

    pub(crate) fn projection_lease(
        &self,
        key: RecordRepositoryKey,
    ) -> Result<RecordProjectionLease<'_>, RecordRepositoryError> {
        let records = self.lock_records()?;
        let snapshot = projected_snapshot(&records, key, None)?;
        Ok(RecordProjectionLease {
            _records: records,
            snapshot,
        })
    }

    /// Highest accepted sequence for one actor and record. `None` means
    /// either the record or that actor's stream does not yet exist.
    #[must_use]
    pub fn highest_sequence(&self, key: RecordRepositoryKey, actor: &FileActor) -> Option<u64> {
        self.records
            .lock_recover()
            .get(&key)
            .and_then(|record| record.replica.highest_sequence_for(actor))
    }

    /// Whether `envelope` is the latest accepted operation for its actor on
    /// this exact record.
    ///
    /// This is deliberately stricter than a general membership check. It is
    /// used only to make the repository-to-materializer handoff retryable: an
    /// exact retry after the repository commit may re-run the idempotent sink,
    /// while an older (stale) operation must never be projected over newer
    /// state.
    #[must_use]
    pub fn is_latest_envelope(
        &self,
        key: RecordRepositoryKey,
        envelope: &RecordEnvelopeV1,
    ) -> bool {
        let records = self.records.lock_recover();
        let Some(record) = records.get(&key) else {
            return false;
        };
        let is_latest = record.replica.highest_sequence_for(&envelope.actor)
            == Some(envelope.sequence)
            && record
                .envelopes
                .iter()
                .rev()
                .find(|stored| stored.actor == envelope.actor)
                == Some(envelope);
        drop(records);
        is_latest
    }

    /// Wakes on every durable write to any record of this repository. A
    /// watcher must still read the record it cares about: the generation
    /// says that something was written, not what.
    #[must_use]
    pub fn subscribe_writes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.writes.subscribe()
    }

    /// The records of one workspace written durably since `since`, a
    /// generation this returned before (`None` for every record), and the
    /// generation they are current to. With nothing written since, it
    /// neither takes the records lock nor visits a record.
    pub(crate) fn written_since(
        &self,
        workspace_id: Uuid,
        since: Option<u64>,
    ) -> (u64, Vec<FileRecordId>) {
        let current = self
            .write_generation
            .load(std::sync::atomic::Ordering::Acquire);
        if since == Some(current) {
            return (current, Vec::new());
        }
        let records = self.records.lock_recover();
        // Writers move the generation under this lock: read again, so
        // every record written up to it is in the answer.
        let current = self
            .write_generation
            .load(std::sync::atomic::Ordering::Acquire);
        let written = records
            .iter()
            .filter(|(key, record)| {
                key.workspace_id == workspace_id && since.is_none_or(|since| record.written > since)
            })
            .map(|(key, _)| key.record_id)
            .collect();
        drop(records);
        (current, written)
    }

    #[cfg(test)]
    pub(crate) fn note_concurrent_head_check(&self) {
        self.concurrent_head_checks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Every record of one workspace, for a full reconciliation pass.
    pub(crate) fn reconciliation_keys(&self, workspace_id: Uuid) -> Vec<RecordRepositoryKey> {
        self.records
            .lock_recover()
            .keys()
            .filter(|key| key.workspace_id == workspace_id)
            .copied()
            .collect()
    }

    /// Every verified projection of one workspace in one pass. Callers build
    /// a derived index from this; taking the records lock once keeps a
    /// startup fold from re-locking per record, and the repository remains
    /// the only source of the projections.
    pub(crate) fn history_projections(
        &self,
        workspace_id: Uuid,
    ) -> Result<Vec<(RecordRepositoryKey, RecordSnapshot)>, RecordRepositoryError> {
        let records = self.lock_records()?;
        records
            .keys()
            .copied()
            .filter(|key| key.workspace_id == workspace_id)
            .map(|key| Ok((key, projected_snapshot(&records, key, None)?)))
            .collect()
    }

    /// The current content revision of every record of one workspace that
    /// has one, in one pass and without cloning envelopes.
    pub(crate) fn current_content_revisions(
        &self,
        workspace_id: Uuid,
    ) -> Vec<fs_protocol::ContentRevisionV1> {
        self.records
            .lock_recover()
            .iter()
            .filter(|(key, _)| key.workspace_id == workspace_id)
            .filter_map(|(_, record)| record.replica.current_content_revision())
            .collect()
    }

    /// The converged signed structural binding of one record, without
    /// cloning its envelopes or immutable versions. `None` means the record
    /// itself is absent; `Some(None)` means it has no binding yet.
    #[expect(
        clippy::option_option,
        reason = "None means the record is absent; Some(None) means it has no binding yet; callers must distinguish them"
    )]
    pub(crate) fn converged_binding(
        &self,
        key: RecordRepositoryKey,
    ) -> Option<Option<FileBindingV1>> {
        self.records
            .lock_recover()
            .get(&key)
            .map(|record| record.replica.converged_binding())
    }

    /// Records whose structure must wait for a completed transaction before
    /// their ordinary binding may be replayed. A fully transaction-owned
    /// record waits so the transaction's atomic result wins. When only some
    /// participants have since been superseded, every participant waits:
    /// replaying a later ordinary binding before the accepted transaction
    /// would lose the participants it has not superseded.
    pub(crate) fn transaction_owned_bindings(
        &self,
        workspace_id: Uuid,
    ) -> BTreeSet<RecordRepositoryKey> {
        let records = self.records.lock_recover();
        let mut owned = BTreeSet::new();
        for (key, record) in records
            .iter()
            .filter(|(key, _)| key.workspace_id == workspace_id)
        {
            for (transaction, committed) in record.replica.transactions() {
                if !committed || transaction.manifest.workspace_id != workspace_id {
                    continue;
                }
                let Some((any_superseded, any_owned)) =
                    transaction_supersession(&records, workspace_id, transaction)
                else {
                    continue;
                };
                if any_superseded && any_owned {
                    for participant in &transaction.manifest.participants {
                        owned.insert(RecordRepositoryKey {
                            workspace_id,
                            record_id: participant.record_id,
                        });
                    }
                } else if !any_superseded {
                    owned.insert(*key);
                }
            }
        }
        drop(records);
        owned
    }

    /// Whether one record's signed history carries file content without
    /// cloning its envelopes.
    pub(crate) fn record_has_file_content(&self, key: RecordRepositoryKey) -> bool {
        self.records
            .lock_recover()
            .get(&key)
            .is_some_and(|record| record.replica.has_file_content())
    }

    /// The content revision a completed transaction owns for this record, if
    /// its participant binding is still the newest structure. Reconciliation
    /// must not overwrite that revision with an older ordinary metadata or
    /// content projection.
    pub(crate) fn transaction_owned_content_revision(
        &self,
        key: RecordRepositoryKey,
    ) -> Option<fs_protocol::ContentRevisionV1> {
        let records = self.records.lock_recover();
        let record = records.get(&key)?;
        let mut newest: Option<(u64, fs_protocol::ContentRevisionV1)> = None;
        for (transaction, committed) in record.replica.transactions() {
            if !committed
                || transaction.manifest.workspace_id != key.workspace_id
                || !transaction_participants_complete(&records, key.workspace_id, transaction)
            {
                continue;
            }
            let Some(sequence) =
                transaction_commit_sequence(record, transaction.manifest.transaction_id)
            else {
                continue;
            };
            if !record
                .replica
                .transaction_binding_is_newer(transaction, sequence)
            {
                continue;
            }
            let Some(revision) = transaction
                .manifest
                .participants
                .iter()
                .find(|participant| participant.record_id == key.record_id)
                .and_then(|participant| participant.proposed_content_revision)
            else {
                continue;
            };
            if newest
                .as_ref()
                .is_none_or(|(current, _)| sequence > *current)
            {
                newest = Some((sequence, revision));
            }
        }
        drop(records);
        newest.map(|(_, revision)| revision)
    }

    /// The per-record form of [`Self::transaction_owned_bindings`], used on
    /// the direct record-protocol write path.
    pub(crate) fn completed_transaction_owns_binding(&self, key: RecordRepositoryKey) -> bool {
        let records = self.records.lock_recover();
        let Some(record) = records.get(&key) else {
            return false;
        };
        let owned = record
            .replica
            .transactions()
            .any(|(transaction, committed)| {
                committed
                    && transaction.manifest.workspace_id == key.workspace_id
                    && transaction_participants_complete(&records, key.workspace_id, transaction)
                    && transaction_commit_sequence(record, transaction.manifest.transaction_id)
                        .is_some_and(|sequence| {
                            record
                                .replica
                                .transaction_binding_is_newer(transaction, sequence)
                        })
            });
        drop(records);
        owned
    }

    /// A complete transaction every participant has since superseded with a
    /// newer ordinary binding: its immutable versions still belong in
    /// history, but replaying its structure would undo the later move.
    pub(crate) fn transaction_is_superseded(
        &self,
        workspace_id: Uuid,
        transaction: &FileMutationTransaction,
    ) -> bool {
        let records = self.records.lock_recover();
        transaction_supersession(&records, workspace_id, transaction)
            .is_some_and(|(any_superseded, any_owned)| any_superseded && !any_owned)
    }

    /// A complete transaction only some of whose participants have since
    /// been renamed, moved, restored or edited over. The accepted transaction
    /// remains atomic for the participants it still owns; the superseding
    /// ordinary bindings stay at their current placement.
    pub(crate) fn transaction_is_partially_superseded(
        &self,
        workspace_id: Uuid,
        transaction: &FileMutationTransaction,
    ) -> bool {
        let records = self.records.lock_recover();
        transaction_supersession(&records, workspace_id, transaction)
            .is_some_and(|(any_superseded, any_owned)| any_superseded && any_owned)
    }

    /// Clone one record-sized recovery batch without holding the repository
    /// lock across namespace or history I/O.
    pub(crate) fn reconciliation_batch(
        &self,
        key: RecordRepositoryKey,
    ) -> Option<RecordReconciliationBatch> {
        self.records
            .lock_recover()
            .get(&key)
            .map(|record| RecordReconciliationBatch {
                key,
                envelopes: record.envelopes.clone(),
                snapshot: record.replica.restored_snapshot(),
            })
    }

    pub(crate) fn complete_transactions(
        &self,
        workspace_id: Uuid,
    ) -> Result<Vec<FileMutationTransaction>, RecordRepositoryError> {
        let records = self.lock_records()?;
        let mut candidates = BTreeMap::<Uuid, FileMutationTransaction>::new();
        for (key, record) in records
            .iter()
            .filter(|(key, _)| key.workspace_id == workspace_id)
        {
            for (transaction, committed) in record.replica.transactions() {
                if !committed || transaction.manifest.workspace_id != key.workspace_id {
                    continue;
                }
                let id = transaction.manifest.transaction_id;
                if candidates
                    .insert(id, transaction.clone())
                    .is_some_and(|existing| existing != *transaction)
                {
                    return Err(RecordRepositoryError::TransactionManifestMismatch(id));
                }
            }
        }
        let mut complete = candidates
            .into_values()
            .filter(|transaction| {
                transaction.manifest.participants.iter().all(|participant| {
                    records
                        .get(&RecordRepositoryKey {
                            workspace_id,
                            record_id: participant.record_id,
                        })
                        .and_then(|record| {
                            record
                                .replica
                                .transaction_state(transaction.manifest.transaction_id)
                        })
                        .is_some_and(|(candidate, committed)| candidate == transaction && committed)
                })
            })
            .collect::<Vec<_>>();
        drop(records);
        complete.sort_by_key(|transaction| {
            (
                transaction.manifest.timestamp_ms,
                transaction.manifest.transaction_id,
            )
        });
        Ok(complete)
    }

    /// Reconstruct the exact already-verified prepare/commit batches for one
    /// complete remote transaction so the shared crash journal can take over
    /// history, namespace persistence and broadcast recovery.
    pub(crate) fn completed_transaction_journal_entry(
        &self,
        workspace_id: Uuid,
        transaction: &FileMutationTransaction,
    ) -> Result<RecordTransactionJournalEntry, RecordRepositoryError> {
        if transaction.manifest.workspace_id != workspace_id {
            return Err(RecordRepositoryError::TransactionWorkspaceMismatch);
        }
        let records = self.lock_records()?;
        let mut participant_batches = Vec::with_capacity(transaction.manifest.participants.len());
        for participant in &transaction.manifest.participants {
            let record = records
                .get(&RecordRepositoryKey {
                    workspace_id,
                    record_id: participant.record_id,
                })
                .ok_or(RecordRepositoryError::RecordNotFound)?;
            let transaction_id = transaction.manifest.transaction_id;
            let prepare = record.envelopes.iter().find(|envelope| {
                matches!(
                    &envelope.operation,
                    crate::record_replication::RecordOperationV1::PrepareTransaction(value)
                        if value == transaction
                )
            });
            let prepare = prepare.ok_or(RecordRepositoryError::TransactionPreconditionFailed(
                participant.record_id,
            ))?;
            let commit = record.envelopes.iter().find(|envelope| {
                envelope.actor == prepare.actor
                    && prepare.sequence.checked_add(1) == Some(envelope.sequence)
                    && matches!(
                        envelope.operation,
                        crate::record_replication::RecordOperationV1::CommitTransaction {
                            transaction_id: value
                        } if value == transaction_id
                    )
            });
            let commit = commit.ok_or(RecordRepositoryError::TransactionPreconditionFailed(
                participant.record_id,
            ))?;
            participant_batches.push(RecordTransactionParticipantBatch {
                record_id: participant.record_id,
                envelopes: vec![prepare.clone(), commit.clone()],
            });
        }
        let entry = RecordTransactionJournalEntry {
            schema_version: crate::record_transaction::RECORD_TRANSACTION_JOURNAL_VERSION,
            transaction: transaction.clone(),
            participant_batches,
            phase: RecordTransactionPhase::Prepared,
            namespace_delta: None,
            external_native_effect: None,
        };
        entry
            .validate()
            .map_err(RecordRepositoryError::TransactionJournal)?;
        drop(records);
        Ok(entry)
    }

    #[must_use]
    pub fn contains(&self, key: RecordRepositoryKey) -> bool {
        self.records.lock_recover().contains_key(&key)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.records.lock_recover().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.lock_recover().is_empty()
    }
}

fn transaction_commit_sequence(record: &StoredRecord, transaction_id: Uuid) -> Option<u64> {
    record.envelopes.iter().find_map(|envelope| {
        matches!(
            envelope.operation,
            crate::record_replication::RecordOperationV1::CommitTransaction {
                transaction_id: committed
            } if committed == transaction_id
        )
        .then_some(envelope.sequence)
    })
}

/// Whether each participant's ordinary binding has superseded the completed
/// transaction. `None` means the transaction is incomplete or one commit
/// sequence is missing. `(true, false)` is fully superseded; `(true, true)` is
/// a legitimate partial supersession; `(false, true)` is wholly owned by the
/// transaction.
fn transaction_supersession(
    records: &Records,
    workspace_id: Uuid,
    transaction: &FileMutationTransaction,
) -> Option<(bool, bool)> {
    if !transaction_participants_complete(records, workspace_id, transaction) {
        return None;
    }
    let mut any_superseded = false;
    let mut any_owned = false;
    for participant in &transaction.manifest.participants {
        let record = records.get(&RecordRepositoryKey {
            workspace_id,
            record_id: participant.record_id,
        })?;
        let sequence = transaction_commit_sequence(record, transaction.manifest.transaction_id)?;
        if record
            .replica
            .binding_supersedes_transaction(transaction, sequence)
        {
            any_superseded = true;
        } else {
            any_owned = true;
        }
    }
    Some((any_superseded, any_owned))
}

fn transaction_participants_complete(
    records: &Records,
    workspace_id: Uuid,
    transaction: &FileMutationTransaction,
) -> bool {
    transaction.manifest.participants.iter().all(|participant| {
        records
            .get(&RecordRepositoryKey {
                workspace_id,
                record_id: participant.record_id,
            })
            .and_then(|record| {
                record
                    .replica
                    .transaction_state(transaction.manifest.transaction_id)
            })
            .is_some_and(|(candidate, committed)| candidate == transaction && committed)
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "projection applies every committed transaction variant in one exhaustive deterministic pass"
)]
fn projected_snapshot(
    records: &BTreeMap<RecordRepositoryKey, StoredRecord>,
    key: RecordRepositoryKey,
    exclude_transaction: Option<Uuid>,
) -> Result<RecordSnapshot, RecordRepositoryError> {
    let record = records
        .get(&key)
        .ok_or(RecordRepositoryError::RecordNotFound)?;
    let mut projection = record.replica.clone();
    let mut snapshot = projection.restored_snapshot();
    let mut complete = record
        .replica
        .transactions()
        .filter(|(transaction, committed)| {
            *committed
                && transaction.manifest.workspace_id == key.workspace_id
                && exclude_transaction != Some(transaction.manifest.transaction_id)
        })
        .filter_map(|(transaction, _)| {
            let all_committed = transaction.manifest.participants.iter().all(|participant| {
                records
                    .get(&RecordRepositoryKey {
                        workspace_id: key.workspace_id,
                        record_id: participant.record_id,
                    })
                    .and_then(|stored| {
                        stored
                            .replica
                            .transaction_state(transaction.manifest.transaction_id)
                    })
                    .is_some_and(|(candidate, committed)| candidate == transaction && committed)
            });
            all_committed.then_some(transaction)
        })
        .collect::<Vec<_>>();
    complete.sort_by_key(|transaction| {
        (
            transaction.manifest.timestamp_ms,
            transaction.manifest.transaction_id,
        )
    });
    for transaction in complete {
        let participant = transaction
            .manifest
            .participants
            .iter()
            .find(|participant| participant.record_id == key.record_id)
            .ok_or(RecordRepositoryError::TransactionProjectionMismatch(
                transaction.manifest.transaction_id,
            ))?;
        let new_sibling = matches!(
            participant.role,
            fs_protocol::FileMutationParticipantRole::KeepBothSibling
        );
        let expected_present = snapshot
            .content_revision
            .is_some_and(|revision| revision.version_id == participant.expected_version)
            || snapshot
                .versions
                .iter()
                .any(|version| version.id == participant.expected_version);
        if !expected_present && !new_sibling {
            return Err(RecordRepositoryError::TransactionPreconditionFailed(
                participant.record_id,
            ));
        }
        if let Some(existing) = snapshot
            .versions
            .iter()
            .find(|version| version.id == participant.proposed_version.id)
            && existing != &participant.proposed_version
        {
            return Err(RecordRepositoryError::TransactionProjectionMismatch(
                transaction.manifest.transaction_id,
            ));
        }
        if !snapshot
            .versions
            .iter()
            .any(|version| version.id == participant.proposed_version.id)
        {
            snapshot.versions.push(participant.proposed_version.clone());
            snapshot.versions.sort_by_key(|version| version.id.0);
        }
        let commit_sequence = record.envelopes.iter().find_map(|envelope| {
            matches!(envelope.operation, crate::record_replication::RecordOperationV1::CommitTransaction { transaction_id }
                if transaction_id == transaction.manifest.transaction_id).then_some(envelope.sequence)
        }).ok_or(RecordRepositoryError::TransactionProjectionMismatch(transaction.manifest.transaction_id))?;
        projection.project_completed_participant(participant, commit_sequence);
        let converged = projection.restored_snapshot();
        snapshot.binding = converged.binding;
        snapshot.content_revision = converged.content_revision;
        if matches!(
            participant.role,
            fs_protocol::FileMutationParticipantRole::KeepBothSource
                | fs_protocol::FileMutationParticipantRole::MergeSource
        ) {
            let (conflict_id, conflict_versions, strategy) = match &transaction.manifest.operation {
                fs_protocol::FileMutationOperation::KeepBothContentConflict {
                    conflict_id,
                    conflict_versions,
                    ..
                } => (
                    *conflict_id,
                    conflict_versions,
                    fs_protocol::ConflictStrategy::KeepBoth,
                ),
                fs_protocol::FileMutationOperation::MergeContentConflict {
                    conflict_id,
                    conflict_versions,
                    ..
                } => (
                    *conflict_id,
                    conflict_versions,
                    fs_protocol::ConflictStrategy::Merge,
                ),
                fs_protocol::FileMutationOperation::ReplacementRename { .. }
                | fs_protocol::FileMutationOperation::AtomicSave { .. }
                | fs_protocol::FileMutationOperation::RestoreContent { .. } => {
                    return Err(RecordRepositoryError::TransactionProjectionMismatch(
                        transaction.manifest.transaction_id,
                    ));
                }
            };
            let conflict = snapshot
                .conflicts
                .iter_mut()
                .find(|conflict| conflict.id == conflict_id)
                .ok_or(RecordRepositoryError::TransactionPreconditionFailed(
                    participant.record_id,
                ))?;
            if conflict.status != fs_protocol::ConflictStatus::Open
                || conflict.versions != *conflict_versions
            {
                return Err(RecordRepositoryError::TransactionPreconditionFailed(
                    participant.record_id,
                ));
            }
            conflict.status = fs_protocol::ConflictStatus::Resolved;
            conflict.resolution = Some(fs_protocol::ConflictResolution {
                strategy,
                result: participant.proposed_version.id,
                actor: transaction.manifest.actor,
                timestamp_ms: transaction.manifest.timestamp_ms,
            });
        }
    }
    let visible_versions = snapshot
        .versions
        .iter()
        .map(|version| version.id)
        .collect::<BTreeSet<_>>();
    snapshot.conflicts.retain(|conflict| {
        conflict
            .versions
            .iter()
            .all(|version| visible_versions.contains(version))
            && conflict
                .resolution
                .as_ref()
                .is_none_or(|resolution| visible_versions.contains(&resolution.result))
    });
    Ok(snapshot)
}

#[expect(
    clippy::too_many_lines,
    reason = "one exhaustive match over the transaction operations and their preconditions"
)]
#[expect(
    clippy::suspicious_operation_groupings,
    reason = "a history version's `id` is the version id the prior revision points at"
)]
fn validate_structural_precondition(
    manifest: &fs_protocol::FileMutationTransactionManifest,
    record_id: FileRecordId,
    snapshot: &RecordSnapshot,
) -> Result<(), RecordRepositoryError> {
    let binding =
        snapshot
            .binding
            .as_ref()
            .ok_or(RecordRepositoryError::TransactionPreconditionFailed(
                record_id,
            ))?;
    let valid = match &manifest.operation {
        fs_protocol::FileMutationOperation::RestoreContent {
            request,
            prior_revision,
            selected_version,
            ..
        } => {
            let proposed = manifest
                .participants
                .iter()
                .find(|value| value.record_id == record_id);
            // Restoring a serialized document writes a new CRDT revision that
            // reaches the selected state, so its bytes are not the selected
            // version's. Every other restore puts the selected bytes back.
            let derived_content = crate::content_kind::RecordContentKind::for_name(&binding.name)
                .is_serialized_document();
            record_id == request.review.record_id
                && snapshot.versions.iter().any(|version| {
                    version.id == prior_revision.version_id
                        && version.content_hash == prior_revision.content_hash
                        && version.metadata_hash == prior_revision.metadata_hash
                })
                && snapshot.versions.iter().any(|version| {
                    version.id == *selected_version
                        && proposed.is_some_and(|proposed| {
                            version.content_hash == proposed.proposed_version.content_hash
                                || (derived_content
                                    && proposed.proposed_version.content_hash.is_some())
                        })
                })
                && !binding.tombstoned
                && binding.kind == fs_protocol::FileNodeKind::File
        }
        fs_protocol::FileMutationOperation::AtomicSave {
            parent_record,
            document_name,
            temporary_name,
            document_record,
            temporary_record,
            prior_document_version,
        } => {
            binding.parent == Some(*parent_record)
                && !binding.tombstoned
                && binding.kind == fs_protocol::FileNodeKind::File
                && ((record_id == *document_record
                    && binding.name == *document_name
                    && snapshot
                        .content_revision
                        .as_ref()
                        .is_some_and(|revision| revision.version_id == *prior_document_version))
                    || (record_id == *temporary_record && binding.name == *temporary_name))
        }
        fs_protocol::FileMutationOperation::ReplacementRename {
            source_parent,
            source_name,
            destination_parent,
            destination_name,
            source_record,
            replaced_record,
        } => {
            if record_id == *source_record {
                binding.parent == Some(*source_parent)
                    && binding.name == *source_name
                    && !binding.tombstoned
            } else if record_id == *replaced_record {
                binding.parent == Some(*destination_parent)
                    && binding.name == *destination_name
                    && !binding.tombstoned
            } else {
                false
            }
        }
        fs_protocol::FileMutationOperation::KeepBothContentConflict {
            source_record,
            parent_record,
            source_name,
            conflict_id,
            conflict_versions,
            ..
        } => {
            record_id == *source_record
                && binding.parent == Some(*parent_record)
                && binding.name == *source_name
                && !binding.tombstoned
                && manifest
                    .participants
                    .iter()
                    .find(|participant| participant.record_id == record_id)
                    .is_some_and(|participant| {
                        snapshot.content_revision.is_some_and(|revision| {
                            revision.version_id == participant.expected_version
                        })
                    })
                && snapshot.conflicts.iter().any(|conflict| {
                    conflict.id == *conflict_id
                        && conflict.status == fs_protocol::ConflictStatus::Open
                        && conflict.versions == *conflict_versions
                })
        }
        fs_protocol::FileMutationOperation::MergeContentConflict {
            source_record,
            parent_record,
            source_name,
            conflict_id,
            conflict_versions,
            current_version,
            ..
        } => {
            record_id == *source_record
                && binding.parent == Some(*parent_record)
                && binding.name == *source_name
                && !binding.tombstoned
                && snapshot
                    .content_revision
                    .is_some_and(|revision| revision.version_id == *current_version)
                && snapshot.conflicts.iter().any(|conflict| {
                    conflict.id == *conflict_id
                        && conflict.status == fs_protocol::ConflictStatus::Open
                        && conflict.versions == *conflict_versions
                })
        }
    };
    if !valid {
        return Err(RecordRepositoryError::TransactionPreconditionFailed(
            record_id,
        ));
    }
    Ok(())
}

fn record_stream_cursors(
    key: RecordRepositoryKey,
    record: &StoredRecord,
) -> Result<Vec<RecordStreamCursor>, RecordRepositoryError> {
    let mut streams = BTreeMap::<([u8; 32], [u8; 32]), Vec<&RecordEnvelopeV1>>::new();
    for envelope in record.durable() {
        streams
            .entry((
                *envelope.actor.person.as_bytes(),
                *envelope.actor.device.as_bytes(),
            ))
            .or_default()
            .push(envelope);
    }
    streams
        .values()
        .map(|envelopes| {
            let first = envelopes
                .first()
                .ok_or(RecordRepositoryError::RecordNotFound)?;
            let last = envelopes
                .last()
                .ok_or(RecordRepositoryError::RecordNotFound)?;
            Ok(RecordStreamCursor {
                record_id: key.record_id,
                actor: first.actor,
                contiguous_sequence: last.sequence,
                prefix_digest: envelope_prefix_digest(envelopes.iter().copied())?,
            })
        })
        .collect()
}

fn envelope_prefix_digest<'a>(
    envelopes: impl IntoIterator<Item = &'a RecordEnvelopeV1>,
) -> Result<[u8; 32], RecordRepositoryError> {
    let mut digest = blake3::Hasher::new();
    digest.update(b"tilo.collaborative-fs.record-stream-prefix.v1\0");
    for envelope in envelopes {
        let bytes = envelope.canonical_bytes()?;
        digest.update(&(bytes.len() as u64).to_be_bytes());
        digest.update(&bytes);
    }
    Ok(*digest.finalize().as_bytes())
}

#[derive(Debug, Error)]
pub enum RecordRepositoryError {
    #[error("native operation {0} has no revalidated replacing-effect receipt")]
    ExternalNativeEffectPending(Uuid),
    #[error(
        "checked restore {0} must finish admitted repository participation before another write; retry after recovery"
    )]
    RestoreRecoveryRequired(Uuid),
    #[error("record repository I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("record log failed: {0}")]
    Log(#[from] RecordLogError),
    #[error("record repository contains invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("persisted record contains an envelope for another record")]
    CrossRecordEnvelope,
    #[error("transaction workspace does not match the repository key")]
    TransactionWorkspaceMismatch,
    #[error("transaction id {0} has different manifests across participants")]
    TransactionManifestMismatch(Uuid),
    #[error("transaction recovery intent is absent from the durable journal")]
    TransactionRecoveryIntentMissing,
    #[error("transaction precondition failed for record {0:?}")]
    TransactionPreconditionFailed(FileRecordId),
    #[error("transaction {0} has an inconsistent complete projection")]
    TransactionProjectionMismatch(Uuid),
    #[error("transaction journal rejected the operation: {0}")]
    TransactionJournal(#[from] RecordTransactionJournalError),
    #[error("transaction failure injected at a durable checkpoint: {0}")]
    TransactionFailureInjected(std::io::Error),
    #[error("record repository has reached its record bound")]
    TooManyRecords,
    #[error("record has reached its signed-operation bound")]
    TooManyOperations,
    #[error("record has reached its immutable-version snapshot bound")]
    TooManyVersions,
    #[error("record has reached its conflict snapshot bound")]
    TooManyConflicts,
    #[error("record does not exist in this workspace")]
    RecordNotFound,
    #[error("record repository batch size is outside its bound")]
    InvalidBatchSize,
    #[error("record stream cursor is ahead of durable state")]
    CursorAhead,
    #[error("another changeset is being published")]
    ChangesetInProgress,
    #[error("changeset is invalid: {0}")]
    Changeset(#[from] ChangesetError),
    #[error("a different changeset was already accepted under the same author and sequence")]
    ChangesetEquivocation,
    #[error("changeset envelopes do not match its signed leaves")]
    ChangesetMismatch,
    #[error("record {0:?} holds different envelopes at the sequences a changeset names")]
    DivergentStream(FileRecordId),
    #[error("record envelope encoding failed: {0}")]
    Canonical(#[from] collab_types::CanonicalError),
    #[error("record operation was rejected: {0}")]
    Replication(#[from] RecordReplicationError),
    #[error("record repository state lock is poisoned")]
    StatePoisoned,
}

#[cfg(test)]
mod tests;
