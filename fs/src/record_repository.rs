//! Crash-safe persistence for authenticated record replicas.
//!
//! Each `(workspace, record)` pair is stored in its own atomically replaced
//! file. The durable form is the ordered set of signed envelopes, not an
//! unsigned materialized snapshot, so restart reconstructs and verifies the
//! exact replay watermarks and immutable-version collision decisions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use collab_types::{FileActor, FileMutationTransactionV1, FileRecordId};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::record_replication::{
    RecordAuthorizer, RecordEnvelopeV1, RecordReplica, RecordReplicationError, RecordSnapshot,
};
use crate::record_transaction::{
    RecordTransactionJournal, RecordTransactionJournalEntry, RecordTransactionJournalError,
    RecordTransactionParticipantBatch, RecordTransactionPhase,
};

pub const RECORD_REPOSITORY_VERSION: u16 = 1;
pub const MAX_REPOSITORY_RECORDS: usize = 16_384;
pub const MAX_RECORD_OPERATIONS: usize = 4_096;
pub const MAX_RECORD_VERSIONS: usize = 1_024;
pub const MAX_RECORD_CONFLICTS: usize = 256;
pub const MAX_RECORD_FILE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_RECORD_BATCH_OPERATIONS: usize = 256;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RecordRepositoryKey {
    pub workspace_id: Uuid,
    pub record_id: FileRecordId,
}

/// Repository-wide completeness proof for one staged transaction. A
/// materializer may expose the mutation only when `complete` is true.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordTransactionStatus {
    pub transaction: FileMutationTransactionV1,
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
    replica: RecordReplica,
    envelopes: Vec<RecordEnvelopeV1>,
}

/// Bounded, already-verified state used only to heal the live projection
/// after a crash between repository commit and materialization.
#[derive(Clone, Debug)]
pub(crate) struct RecordReconciliationBatch {
    pub key: RecordRepositoryKey,
    pub envelopes: Vec<RecordEnvelopeV1>,
    pub snapshot: RecordSnapshot,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedRecordV1 {
    schema_version: u16,
    workspace_id: Uuid,
    record_id: FileRecordId,
    envelopes: Vec<RecordEnvelopeV1>,
}

/// Single-process durable repository for record-scoped replication state.
pub struct RecordRepository {
    root: PathBuf,
    records: Mutex<BTreeMap<RecordRepositoryKey, StoredRecord>>,
}

impl RecordRepository {
    /// Open and fully verify every persisted record. A missing root is an
    /// empty repository; malformed, duplicate, cross-key or oversized state
    /// fails closed rather than being silently discarded.
    ///
    /// # Errors
    ///
    /// Returns an error for I/O, malformed/unsupported state, invalid signed
    /// envelopes, duplicate keys, or any repository bound violation.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, RecordRepositoryError> {
        let root = root.into();
        let records = load_records(&root)?;
        Ok(Self {
            root,
            records: Mutex::new(records),
        })
    }

    /// Apply an already-signed envelope through the supplied live authorizer,
    /// persist the candidate state, then publish it in memory. If validation,
    /// authorization or persistence fails, the live repository is unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error when validation, authorization, capacity, locking, or
    /// durable persistence fails.
    pub fn apply<A: RecordAuthorizer + ?Sized>(
        &self,
        key: RecordRepositoryKey,
        envelope: &RecordEnvelopeV1,
        authorizer: &A,
    ) -> Result<(), RecordRepositoryError> {
        self.apply_batch(key, std::slice::from_ref(envelope), authorizer)
    }

    /// Atomically validate and persist one bounded, topologically ordered
    /// prefix. No envelope becomes visible if any later envelope in the batch
    /// fails authentication, authorization, sequence, DAG, or size checks.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid batch/key/workspace data, authorization or
    /// replication rejection, capacity limits, poisoned state, or persistence.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the writer lock intentionally spans batch validation, durable replacement, and in-memory publication"
    )]
    pub fn apply_batch<A: RecordAuthorizer + ?Sized>(
        &self,
        key: RecordRepositoryKey,
        envelopes: &[RecordEnvelopeV1],
        authorizer: &A,
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
        let mut records = self
            .records
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)?;
        let current = records.get(&key);
        if current.is_none() && records.len() >= MAX_REPOSITORY_RECORDS {
            return Err(RecordRepositoryError::TooManyRecords);
        }
        if current.is_some_and(|record| {
            record.envelopes.len().saturating_add(envelopes.len()) > MAX_RECORD_OPERATIONS
        }) || current.is_none() && envelopes.len() > MAX_RECORD_OPERATIONS
        {
            return Err(RecordRepositoryError::TooManyOperations);
        }

        let mut candidate = current.cloned().unwrap_or_else(|| StoredRecord {
            replica: RecordReplica::new(key.record_id),
            envelopes: Vec::new(),
        });
        for envelope in envelopes {
            candidate.replica.merge(envelope, authorizer)?;
            candidate.envelopes.push(envelope.clone());
        }
        if candidate.replica.version_count() > MAX_RECORD_VERSIONS {
            return Err(RecordRepositoryError::TooManyVersions);
        }
        if candidate.replica.conflict_count() > MAX_RECORD_CONFLICTS {
            return Err(RecordRepositoryError::TooManyConflicts);
        }
        persist_record(&self.root, key, &candidate.envelopes)?;
        records.insert(key, candidate);
        Ok(())
    }

    /// Inspect the exact participant set without projecting a partial commit.
    /// Differing signed manifests under one transaction ID fail closed.
    ///
    /// # Errors
    ///
    /// Returns an error for poisoned state or inconsistent signed manifests.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "one repository read lock provides a consistent cross-record transaction snapshot"
    )]
    pub fn transaction_status(
        &self,
        workspace_id: Uuid,
        transaction_id: Uuid,
    ) -> Result<Option<RecordTransactionStatus>, RecordRepositoryError> {
        let records = self
            .records
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)?;
        let mut transaction: Option<FileMutationTransactionV1> = None;
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
    /// durably journal the exact intent, then replace all participant files.
    /// Memory is published only after every file is durable. A crash after one
    /// physical replace leaves an invisible prepared record which exact retry
    /// completes from the journal.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid transaction/journal state, failed
    /// preconditions, authorization/capacity failure, or persistence I/O.
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
    #[allow(
        clippy::too_many_lines,
        clippy::significant_drop_tightening,
        reason = "the transaction coordinator intentionally keeps one writer lock across all participant validation and durable record replacement"
    )]
    pub fn apply_transaction_with_checkpoint<A, F>(
        &self,
        journal: &mut RecordTransactionJournal,
        entry: &RecordTransactionJournalEntry,
        authorizer: &A,
        mut checkpoint: F,
    ) -> Result<(), RecordRepositoryError>
    where
        A: RecordAuthorizer + ?Sized,
        F: FnMut(RecordTransactionCheckpoint) -> Result<(), std::io::Error>,
    {
        entry
            .validate()
            .map_err(RecordRepositoryError::TransactionJournal)?;
        let transaction = &entry.transaction;
        let id = transaction.manifest.transaction_id;
        let workspace_id = transaction.manifest.workspace_id;
        let journal_missing = if let Some(existing) = journal.get(id) {
            if existing.transaction != entry.transaction
                || existing.participant_batches != entry.participant_batches
            {
                return Err(RecordRepositoryError::TransactionManifestMismatch(id));
            }
            false
        } else {
            if entry.phase != RecordTransactionPhase::Prepared {
                return Err(RecordRepositoryError::TransactionRecoveryIntentMissing);
            }
            true
        };

        let mut records = self
            .records
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)?;
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
            let exact_suffix =
                current.is_some_and(|record| record.envelopes.ends_with(&batch.envelopes));
            let new_sibling = matches!(
                participant.role,
                collab_types::FileMutationParticipantRoleV1::KeepBothSibling
            );
            if new_sibling {
                if current.is_some() && !exact_suffix {
                    return Err(RecordRepositoryError::TransactionPreconditionFailed(
                        participant.record_id,
                    ));
                }
            } else {
                let snapshot = projected_snapshot(&records, key, Some(id))?;
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

            let mut candidate = current.cloned().unwrap_or_else(|| StoredRecord {
                replica: RecordReplica::new(key.record_id),
                envelopes: Vec::new(),
            });
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
                    candidate.envelopes.push(envelope.clone());
                }
            }
            candidates.push((key, candidate));
        }

        // No durable side effect occurs until every participant candidate and
        // structural/version precondition above has succeeded.
        if journal_missing {
            journal
                .insert_prepared(entry.clone())
                .map_err(RecordRepositoryError::TransactionJournal)?;
            checkpoint(RecordTransactionCheckpoint::PreparedJournalDurable)
                .map_err(RecordRepositoryError::TransactionFailureInjected)?;
        }
        for (index, (key, candidate)) in candidates.iter().enumerate() {
            persist_record(&self.root, *key, &candidate.envelopes)?;
            checkpoint(RecordTransactionCheckpoint::ParticipantRecordDurable { index })
                .map_err(RecordRepositoryError::TransactionFailureInjected)?;
        }
        for (key, candidate) in candidates {
            records.insert(key, candidate);
        }
        drop(records);
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

    /// Bounded per-record, per-actor inventory for workspace record-mesh
    /// anti-entropy. Callers must perform workspace-root read authorization
    /// before invoking this method.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid page limit, poisoned state, or envelope
    /// encoding failure while computing a prefix digest.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "one read lock provides a consistent paginated inventory across all record streams"
    )]
    pub fn inventory_page(
        &self,
        workspace_id: Uuid,
        after: Option<RecordStreamKey>,
        limit: usize,
    ) -> Result<Vec<RecordStreamCursor>, RecordRepositoryError> {
        if limit == 0 || limit > MAX_RECORD_BATCH_OPERATIONS {
            return Err(RecordRepositoryError::InvalidBatchSize);
        }
        let records = self
            .records
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)?;
        let mut cursors = Vec::new();
        for (key, record) in records
            .iter()
            .filter(|(key, _)| key.workspace_id == workspace_id)
        {
            let mut streams = BTreeMap::<([u8; 32], [u8; 32]), Vec<&RecordEnvelopeV1>>::new();
            for envelope in &record.envelopes {
                streams
                    .entry((
                        *envelope.actor.person.as_bytes(),
                        *envelope.actor.device.as_bytes(),
                    ))
                    .or_default()
                    .push(envelope);
            }
            for envelopes in streams.values() {
                let Some(first) = envelopes.first() else {
                    continue;
                };
                let Some(last) = envelopes.last() else {
                    continue;
                };
                let actor = first.actor;
                let cursor = RecordStreamCursor {
                    record_id: key.record_id,
                    actor,
                    contiguous_sequence: last.sequence,
                    prefix_digest: envelope_prefix_digest(envelopes.iter().copied())?,
                };
                if after.is_none_or(|after| cursor.key() > after) {
                    cursors.push(cursor);
                    if cursors.len() == limit {
                        return Ok(cursors);
                    }
                }
            }
        }
        Ok(cursors)
    }

    /// Return a bounded contiguous suffix for one actor stream.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid bounds, poisoned state, unknown record, or
    /// a cursor ahead of durable state.
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
        let records = self
            .records
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)?;
        let record = records
            .get(&key)
            .ok_or(RecordRepositoryError::RecordNotFound)?;
        let highest = record.replica.highest_sequence_for(actor).unwrap_or(0);
        if after_sequence > highest {
            return Err(RecordRepositoryError::CursorAhead);
        }
        let envelopes = record
            .envelopes
            .iter()
            .filter(|envelope| envelope.actor == *actor && envelope.sequence > after_sequence)
            .take(limit)
            .cloned()
            .collect();
        drop(records);
        Ok(envelopes)
    }

    /// Recompute the authenticated prefix cursor through an exact sequence.
    /// This detects equal-watermark divergence before a mesh peer accepts a
    /// suffix based on a different history.
    ///
    /// # Errors
    ///
    /// Returns an error for poisoned/unknown state, an invalid cursor, or
    /// envelope prefix encoding failure.
    pub fn stream_cursor(
        &self,
        key: RecordRepositoryKey,
        actor: &FileActor,
        through_sequence: u64,
    ) -> Result<RecordStreamCursor, RecordRepositoryError> {
        let records = self
            .records
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)?;
        let record = records
            .get(&key)
            .ok_or(RecordRepositoryError::RecordNotFound)?;
        let highest = record.replica.highest_sequence_for(actor).unwrap_or(0);
        if through_sequence == 0 || through_sequence > highest {
            return Err(RecordRepositoryError::CursorAhead);
        }
        let envelopes = record
            .envelopes
            .iter()
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
    ///
    /// # Errors
    ///
    /// Returns an error for poisoned/unknown state, denied authorization, or
    /// inconsistent transaction projection.
    pub fn snapshot<A: RecordAuthorizer + ?Sized>(
        &self,
        key: RecordRepositoryKey,
        requester: &FileActor,
        authorizer: &A,
    ) -> Result<RecordSnapshot, RecordRepositoryError> {
        let records = self
            .records
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)?;
        let record = records
            .get(&key)
            .ok_or(RecordRepositoryError::RecordNotFound)?;
        record.replica.snapshot(requester, authorizer)?;
        let snapshot = projected_snapshot(&records, key, None)?;
        drop(records);
        Ok(snapshot)
    }

    /// Highest durably accepted sequence for one actor and record. `None`
    /// means either the record or that actor's stream does not yet exist.
    #[must_use]
    pub fn highest_sequence(&self, key: RecordRepositoryKey, actor: &FileActor) -> Option<u64> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .and_then(|record| record.replica.highest_sequence_for(actor))
    }

    /// Whether `envelope` is the latest durably accepted operation for its
    /// actor on this exact record.
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
        let records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

    /// Stable keys for one workspace's persisted records. The repository's
    /// global cardinality bound also bounds this allocation.
    pub(crate) fn reconciliation_keys(&self, workspace_id: Uuid) -> Vec<RecordRepositoryKey> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .filter(|key| key.workspace_id == workspace_id)
            .copied()
            .collect()
    }

    /// Clone one record-sized recovery batch without holding the repository
    /// lock across namespace or history I/O.
    pub(crate) fn reconciliation_batch(
        &self,
        key: RecordRepositoryKey,
    ) -> Option<RecordReconciliationBatch> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    ) -> Result<Vec<FileMutationTransactionV1>, RecordRepositoryError> {
        let records = self
            .records
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)?;
        let mut candidates = BTreeMap::<Uuid, FileMutationTransactionV1>::new();
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
        transaction: &FileMutationTransactionV1,
    ) -> Result<RecordTransactionJournalEntry, RecordRepositoryError> {
        if transaction.manifest.workspace_id != workspace_id {
            return Err(RecordRepositoryError::TransactionWorkspaceMismatch);
        }
        let records = self
            .records
            .lock()
            .map_err(|_| RecordRepositoryError::StatePoisoned)?;
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
        };
        entry
            .validate()
            .map_err(RecordRepositoryError::TransactionJournal)?;
        drop(records);
        Ok(entry)
    }

    #[must_use]
    pub fn contains(&self, key: RecordRepositoryKey) -> bool {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&key)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    }
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
    let mut snapshot = record.replica.restored_snapshot();
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
            collab_types::FileMutationParticipantRoleV1::KeepBothSibling
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
        snapshot.binding = Some(participant.proposed_binding.clone());
        snapshot.content_revision = participant.proposed_content_revision;
        if matches!(
            participant.role,
            collab_types::FileMutationParticipantRoleV1::KeepBothSource
                | collab_types::FileMutationParticipantRoleV1::MergeSource
        ) {
            let (conflict_id, conflict_versions, strategy) = match &transaction.manifest.operation {
                collab_types::FileMutationOperationV1::KeepBothContentConflict {
                    conflict_id,
                    conflict_versions,
                    ..
                } => (
                    *conflict_id,
                    conflict_versions,
                    collab_types::ConflictStrategy::KeepBoth,
                ),
                collab_types::FileMutationOperationV1::MergeContentConflict {
                    conflict_id,
                    conflict_versions,
                    ..
                } => (
                    *conflict_id,
                    conflict_versions,
                    collab_types::ConflictStrategy::Merge,
                ),
                collab_types::FileMutationOperationV1::ReplacementRename { .. } => {
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
            if conflict.status != collab_types::ConflictStatus::Open
                || conflict.versions != *conflict_versions
            {
                return Err(RecordRepositoryError::TransactionPreconditionFailed(
                    participant.record_id,
                ));
            }
            conflict.status = collab_types::ConflictStatus::Resolved;
            conflict.resolution = Some(collab_types::ConflictResolution {
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

fn validate_structural_precondition(
    manifest: &collab_types::FileMutationTransactionManifestV1,
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
        collab_types::FileMutationOperationV1::ReplacementRename {
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
        collab_types::FileMutationOperationV1::KeepBothContentConflict {
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
                        && conflict.status == collab_types::ConflictStatus::Open
                        && conflict.versions == *conflict_versions
                })
        }
        collab_types::FileMutationOperationV1::MergeContentConflict {
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
                        && conflict.status == collab_types::ConflictStatus::Open
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

fn envelope_prefix_digest<'a>(
    envelopes: impl IntoIterator<Item = &'a RecordEnvelopeV1>,
) -> Result<[u8; 32], RecordRepositoryError> {
    let mut digest = blake3::Hasher::new();
    digest.update(b"qos.collaborative-fs.record-stream-prefix.v1\0");
    for envelope in envelopes {
        let bytes = envelope.canonical_bytes()?;
        digest.update(&(bytes.len() as u64).to_be_bytes());
        digest.update(&bytes);
    }
    Ok(*digest.finalize().as_bytes())
}

fn load_records(
    root: &Path,
) -> Result<BTreeMap<RecordRepositoryKey, StoredRecord>, RecordRepositoryError> {
    let root_entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.into()),
    };
    let mut records = BTreeMap::new();
    for workspace_entry in root_entries {
        let workspace_entry = workspace_entry?;
        let workspace_name = entry_name(&workspace_entry)?;
        if is_atomic_temp(&workspace_name)
            || crate::durable_backup::is_backup_directory_name(&workspace_name)
        {
            continue;
        }
        if !workspace_entry.file_type()?.is_dir() {
            return Err(RecordRepositoryError::UnexpectedEntry(
                workspace_entry.path(),
            ));
        }
        let workspace_id = Uuid::parse_str(&workspace_name)
            .map_err(|_| RecordRepositoryError::UnexpectedEntry(workspace_entry.path()))?;
        for record_entry in std::fs::read_dir(workspace_entry.path())? {
            let record_entry = record_entry?;
            let record_name = entry_name(&record_entry)?;
            if is_atomic_temp(&record_name)
                || crate::durable_backup::is_backup_directory_name(&record_name)
            {
                continue;
            }
            if !record_entry.file_type()?.is_file() {
                return Err(RecordRepositoryError::UnexpectedEntry(record_entry.path()));
            }
            let record_text = record_name
                .strip_suffix(".json")
                .ok_or_else(|| RecordRepositoryError::UnexpectedEntry(record_entry.path()))?;
            let path_record_id = FileRecordId(
                Uuid::parse_str(record_text)
                    .map_err(|_| RecordRepositoryError::UnexpectedEntry(record_entry.path()))?,
            );
            let key = RecordRepositoryKey {
                workspace_id,
                record_id: path_record_id,
            };
            let primary = crate::durable_backup::read_primary(&record_entry.path())?
                .ok_or_else(|| RecordRepositoryError::UnexpectedEntry(record_entry.path()))?;
            let outcome = match decode_stored_record(&primary, key) {
                Ok(stored) => {
                    if records.insert(key, stored).is_some() {
                        return Err(RecordRepositoryError::DuplicateRecord);
                    }
                    if records.len() > MAX_REPOSITORY_RECORDS {
                        return Err(RecordRepositoryError::TooManyRecords);
                    }
                    continue;
                }
                Err(primary_error) => {
                    match crate::durable_backup::read_or_recover(&record_entry.path(), |bytes| {
                        bytes.len() as u64 <= MAX_RECORD_FILE_BYTES
                            && decode_stored_record(bytes, key).is_ok()
                    }) {
                        Ok(Some(outcome)) => outcome,
                        Ok(None) => return Err(primary_error),
                        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                            return Err(primary_error);
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            };
            if let Some(recovery) = &outcome.recovery {
                eprintln!(
                    "RECOVERED damaged signed record {} from {}; evidence {}; receipt {}",
                    record_entry.path().display(),
                    recovery.backup_path.display(),
                    recovery.damaged_evidence_path.display(),
                    recovery.receipt_path.display()
                );
            }
            let stored = decode_stored_record(&outcome.bytes, key)?;
            if records.insert(key, stored).is_some() {
                return Err(RecordRepositoryError::DuplicateRecord);
            }
            if records.len() > MAX_REPOSITORY_RECORDS {
                return Err(RecordRepositoryError::TooManyRecords);
            }
        }
    }
    Ok(records)
}

fn decode_stored_record(
    bytes: &[u8],
    key: RecordRepositoryKey,
) -> Result<StoredRecord, RecordRepositoryError> {
    if bytes.len() as u64 > MAX_RECORD_FILE_BYTES {
        return Err(RecordRepositoryError::RecordFileTooLarge);
    }
    let persisted: PersistedRecordV1 = serde_json::from_slice(bytes)?;
    if persisted.schema_version != RECORD_REPOSITORY_VERSION {
        return Err(RecordRepositoryError::UnsupportedVersion(
            persisted.schema_version,
        ));
    }
    if persisted.workspace_id != key.workspace_id || persisted.record_id != key.record_id {
        return Err(RecordRepositoryError::PathKeyMismatch);
    }
    if persisted.envelopes.is_empty() {
        return Err(RecordRepositoryError::EmptyRecord);
    }
    if persisted.envelopes.len() > MAX_RECORD_OPERATIONS {
        return Err(RecordRepositoryError::TooManyOperations);
    }
    let mut replica = RecordReplica::new(key.record_id);
    for envelope in &persisted.envelopes {
        if envelope.record_id != key.record_id {
            return Err(RecordRepositoryError::CrossRecordEnvelope);
        }
        if matches!(
            &envelope.operation,
            crate::record_replication::RecordOperationV1::PrepareTransaction(transaction)
                if transaction.manifest.workspace_id != key.workspace_id
        ) {
            return Err(RecordRepositoryError::TransactionWorkspaceMismatch);
        }
        replica.restore_envelope(envelope)?;
        if replica.version_count() > MAX_RECORD_VERSIONS {
            return Err(RecordRepositoryError::TooManyVersions);
        }
        if replica.conflict_count() > MAX_RECORD_CONFLICTS {
            return Err(RecordRepositoryError::TooManyConflicts);
        }
    }
    Ok(StoredRecord {
        replica,
        envelopes: persisted.envelopes,
    })
}

fn persist_record(
    root: &Path,
    key: RecordRepositoryKey,
    envelopes: &[RecordEnvelopeV1],
) -> Result<(), RecordRepositoryError> {
    let state = PersistedRecordV1 {
        schema_version: RECORD_REPOSITORY_VERSION,
        workspace_id: key.workspace_id,
        record_id: key.record_id,
        envelopes: envelopes.to_vec(),
    };
    let bytes = serde_json::to_vec(&state)?;
    if bytes.len() as u64 > MAX_RECORD_FILE_BYTES {
        return Err(RecordRepositoryError::RecordFileTooLarge);
    }
    let path = record_path(root, key);
    crate::durable_backup::write(&path, &bytes, |candidate| {
        candidate.len() as u64 <= MAX_RECORD_FILE_BYTES
            && decode_stored_record(candidate, key).is_ok()
    })?;
    Ok(())
}

fn record_path(root: &Path, key: RecordRepositoryKey) -> PathBuf {
    root.join(key.workspace_id.to_string())
        .join(format!("{}.json", key.record_id.0))
}

fn entry_name(entry: &std::fs::DirEntry) -> Result<String, RecordRepositoryError> {
    entry
        .file_name()
        .into_string()
        .map_err(|_| RecordRepositoryError::UnexpectedEntry(entry.path()))
}

fn is_atomic_temp(name: &str) -> bool {
    name.starts_with('.')
        && name
            .rsplit_once('.')
            .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("tmp"))
}

#[derive(Debug, Error)]
pub enum RecordRepositoryError {
    #[error("record repository I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("record repository contains invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported record repository schema version {0}")]
    UnsupportedVersion(u16),
    #[error("record repository contains an unexpected entry at {0:?}")]
    UnexpectedEntry(PathBuf),
    #[error("persisted record path and embedded workspace/record key differ")]
    PathKeyMismatch,
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
    #[error("persisted record contains no accepted operations")]
    EmptyRecord,
    #[error("record repository contains the same workspace/record more than once")]
    DuplicateRecord,
    #[error("record repository has reached its record bound")]
    TooManyRecords,
    #[error("record has reached its signed-operation bound")]
    TooManyOperations,
    #[error("record has reached its immutable-version snapshot bound")]
    TooManyVersions,
    #[error("record has reached its conflict snapshot bound")]
    TooManyConflicts,
    #[error("persisted record exceeds its byte bound")]
    RecordFileTooLarge,
    #[error("record does not exist in this workspace")]
    RecordNotFound,
    #[error("record repository batch size is outside its bound")]
    InvalidBatchSize,
    #[error("record stream cursor is ahead of durable state")]
    CursorAhead,
    #[error("record envelope encoding failed: {0}")]
    Canonical(#[from] collab_types::CanonicalError),
    #[error("record operation was rejected: {0}")]
    Replication(#[from] RecordReplicationError),
    #[error("record repository state lock is poisoned")]
    StatePoisoned,
}

#[cfg(test)]
mod tests {
    use collab_types::{
        CONTENT_REVISION_VERSION, ConflictMergeCodecV1, ContentHash, ContentRevisionV1,
        FILE_BINDING_VERSION, FILE_MUTATION_TRANSACTION_VERSION, FileBindingV1, FileMetadataV1,
        FileMutationOperationV1, FileMutationParticipantRoleV1, FileMutationParticipantV1,
        FileMutationTransactionManifestV1, FileMutationTransactionV1, FileNodeKind, FileProvenance,
        FileVersionId, FileVersionV1, IndexStatus, generate_keypair,
    };
    use std::collections::BTreeSet;

    use super::*;
    use crate::record_replication::{RecordAction, RecordOperationV1};

    struct TestAuthorizer {
        actor: FileActor,
        record: FileRecordId,
        read: bool,
        write: bool,
    }

    struct MultiRecordAuthorizer {
        actor: FileActor,
        records: BTreeSet<FileRecordId>,
    }

    impl RecordAuthorizer for MultiRecordAuthorizer {
        fn actor_is_certified(&self, actor: &FileActor) -> bool {
            *actor == self.actor
        }

        fn allows(&self, actor: &FileActor, record: FileRecordId, _action: RecordAction) -> bool {
            *actor == self.actor && self.records.contains(&record)
        }
    }

    impl RecordAuthorizer for TestAuthorizer {
        fn actor_is_certified(&self, actor: &FileActor) -> bool {
            *actor == self.actor
        }

        fn allows(&self, actor: &FileActor, record: FileRecordId, action: RecordAction) -> bool {
            *actor == self.actor
                && record == self.record
                && match action {
                    RecordAction::Read => self.read,
                    RecordAction::Write => self.write,
                }
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        root: PathBuf,
        key: RecordRepositoryKey,
        device_key: ed25519_dalek::SigningKey,
        actor: FileActor,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("records");
            let (device_key, device) = generate_keypair();
            let (_, person) = generate_keypair();
            Self {
                dir,
                root,
                key: RecordRepositoryKey {
                    workspace_id: Uuid::new_v4(),
                    record_id: FileRecordId::new(),
                },
                device_key,
                actor: FileActor { person, device },
            }
        }

        fn auth(&self, read: bool, write: bool) -> TestAuthorizer {
            TestAuthorizer {
                actor: self.actor,
                record: self.key.record_id,
                read,
                write,
            }
        }

        fn metadata_envelope(&self, sequence: u64) -> RecordEnvelopeV1 {
            RecordEnvelopeV1::signed(
                self.key.record_id,
                self.actor,
                sequence,
                RecordOperationV1::SetMetadata(FileMetadataV1 {
                    schema_version: 1,
                    record_id: self.key.record_id,
                    content_hash: Some(ContentHash([u8::try_from(sequence).unwrap(); 32])),
                    byte_len: sequence,
                    mime: Some("text/plain".into()),
                    previews: Vec::new(),
                    latest_actor: Some(self.actor),
                    observed_at_ms: sequence,
                    index_status: IndexStatus::Indexed,
                    properties: None,
                }),
                &self.device_key,
            )
            .unwrap()
        }

        fn version_envelope(
            &self,
            sequence: u64,
            id: FileVersionId,
            hash_byte: u8,
        ) -> RecordEnvelopeV1 {
            RecordEnvelopeV1::signed(
                self.key.record_id,
                self.actor,
                sequence,
                RecordOperationV1::AppendVersion(FileVersionV1 {
                    id,
                    record_id: self.key.record_id,
                    parents: Vec::new(),
                    actor: self.actor,
                    timestamp_ms: sequence,
                    content_hash: Some(ContentHash([hash_byte; 32])),
                    metadata_hash: ContentHash([9; 32]),
                    provenance: FileProvenance::Edit,
                }),
                &self.device_key,
            )
            .unwrap()
        }
    }

    fn replacement_transaction(
        fixture: &Fixture,
        replaced: FileRecordId,
    ) -> FileMutationTransactionV1 {
        let destination_parent = FileRecordId::new();
        let participant = |record_id, role| {
            let expected = FileVersionId::new();
            let version = FileVersionV1 {
                id: FileVersionId::new(),
                record_id,
                parents: vec![expected],
                actor: fixture.actor,
                timestamp_ms: 8,
                content_hash: None,
                metadata_hash: ContentHash([8; 32]),
                provenance: match role {
                    FileMutationParticipantRoleV1::Source => FileProvenance::Rename,
                    FileMutationParticipantRoleV1::Replaced => FileProvenance::Delete,
                    _ => unreachable!("replacement fixture role"),
                },
            };
            let mut participant = FileMutationParticipantV1 {
                record_id,
                role,
                expected_version: expected,
                proposed_binding: FileBindingV1 {
                    schema_version: FILE_BINDING_VERSION,
                    record_id,
                    parent: Some(destination_parent),
                    name: "destination.txt".into(),
                    kind: FileNodeKind::File,
                    tombstoned: role == FileMutationParticipantRoleV1::Replaced,
                    actor: fixture.actor,
                    observed_at_ms: 8,
                },
                proposed_content_revision: Some(collab_types::ContentRevisionV1 {
                    schema_version: CONTENT_REVISION_VERSION,
                    record_id,
                    version_id: version.id,
                    content_hash: None,
                    byte_len: 0,
                    metadata_hash: version.metadata_hash,
                }),
                proposed_version: version,
                projection_hash: ContentHash([0; 32]),
            };
            participant.projection_hash = participant.computed_projection_hash().unwrap();
            participant
        };
        let mut participants = vec![
            participant(fixture.key.record_id, FileMutationParticipantRoleV1::Source),
            participant(replaced, FileMutationParticipantRoleV1::Replaced),
        ];
        participants.sort_by_key(|participant| participant.record_id);
        FileMutationTransactionV1::signed(
            FileMutationTransactionManifestV1 {
                schema_version: FILE_MUTATION_TRANSACTION_VERSION,
                transaction_id: Uuid::new_v4(),
                workspace_id: fixture.key.workspace_id,
                actor: fixture.actor,
                timestamp_ms: 8,
                operation: FileMutationOperationV1::ReplacementRename {
                    source_parent: FileRecordId::new(),
                    source_name: "source.txt".into(),
                    destination_parent,
                    destination_name: "destination.txt".into(),
                    source_record: fixture.key.record_id,
                    replaced_record: replaced,
                },
                participants,
            },
            &fixture.device_key,
        )
        .unwrap()
    }

    #[allow(clippy::too_many_lines)] // Builds a complete signed transaction fixture.
    fn keep_both_transaction(
        fixture: &Fixture,
    ) -> (
        FileMutationTransactionV1,
        FileRecordId,
        FileRecordId,
        FileVersionV1,
        FileVersionV1,
        collab_types::FileConflictV1,
    ) {
        let parent = FileRecordId::new();
        let sibling = FileRecordId::new();
        let current = FileVersionV1 {
            id: FileVersionId::new(),
            record_id: fixture.key.record_id,
            parents: Vec::new(),
            actor: fixture.actor,
            timestamp_ms: 1,
            content_hash: Some(ContentHash([1; 32])),
            metadata_hash: ContentHash([11; 32]),
            provenance: FileProvenance::Create,
        };
        let selected = FileVersionV1 {
            id: FileVersionId::new(),
            record_id: fixture.key.record_id,
            parents: vec![current.id],
            actor: fixture.actor,
            timestamp_ms: 2,
            content_hash: Some(ContentHash([2; 32])),
            metadata_hash: ContentHash([12; 32]),
            provenance: FileProvenance::Edit,
        };
        let conflict = collab_types::FileConflictV1 {
            id: Uuid::new_v4(),
            record_id: fixture.key.record_id,
            versions: vec![current.id, selected.id],
            status: collab_types::ConflictStatus::Open,
            resolution: None,
        };
        let participant = |record_id, role| {
            let is_source = role == FileMutationParticipantRoleV1::KeepBothSource;
            let version = FileVersionV1 {
                id: FileVersionId::new(),
                record_id,
                parents: if is_source {
                    conflict.versions.clone()
                } else {
                    Vec::new()
                },
                actor: fixture.actor,
                timestamp_ms: 3,
                content_hash: if is_source {
                    current.content_hash
                } else {
                    selected.content_hash
                },
                metadata_hash: ContentHash([13_u8.saturating_add(u8::from(!is_source)); 32]),
                provenance: if is_source {
                    FileProvenance::ConflictResolution
                } else {
                    FileProvenance::Create
                },
            };
            let mut value = FileMutationParticipantV1 {
                record_id,
                role,
                expected_version: if is_source { current.id } else { selected.id },
                proposed_binding: FileBindingV1 {
                    schema_version: FILE_BINDING_VERSION,
                    record_id,
                    parent: Some(parent),
                    name: if is_source {
                        "source.txt".into()
                    } else {
                        "source (conflict-deadbeef).txt".into()
                    },
                    kind: FileNodeKind::File,
                    tombstoned: false,
                    actor: fixture.actor,
                    observed_at_ms: 3,
                },
                proposed_content_revision: Some(collab_types::ContentRevisionV1 {
                    schema_version: CONTENT_REVISION_VERSION,
                    record_id,
                    version_id: version.id,
                    content_hash: version.content_hash,
                    byte_len: 1,
                    metadata_hash: version.metadata_hash,
                }),
                proposed_version: version,
                projection_hash: ContentHash([0; 32]),
            };
            value.projection_hash = value.computed_projection_hash().unwrap();
            value
        };
        let mut participants = vec![
            participant(
                fixture.key.record_id,
                FileMutationParticipantRoleV1::KeepBothSource,
            ),
            participant(sibling, FileMutationParticipantRoleV1::KeepBothSibling),
        ];
        participants.sort_by_key(|value| value.record_id);
        let transaction = FileMutationTransactionV1::signed(
            FileMutationTransactionManifestV1 {
                schema_version: collab_types::files::FILE_MUTATION_TRANSACTION_VERSION_V2,
                transaction_id: Uuid::new_v4(),
                workspace_id: fixture.key.workspace_id,
                actor: fixture.actor,
                timestamp_ms: 3,
                operation: FileMutationOperationV1::KeepBothContentConflict {
                    source_record: fixture.key.record_id,
                    sibling_record: sibling,
                    parent_record: parent,
                    source_name: "source.txt".into(),
                    sibling_name: "source (conflict-deadbeef).txt".into(),
                    conflict_id: conflict.id,
                    conflict_versions: conflict.versions.clone(),
                    selected_version: selected.id,
                },
                participants,
            },
            &fixture.device_key,
        )
        .unwrap();
        (transaction, parent, sibling, current, selected, conflict)
    }

    fn merge_transaction(
        fixture: &Fixture,
        codec: ConflictMergeCodecV1,
    ) -> (
        FileMutationTransactionV1,
        FileRecordId,
        FileVersionV1,
        FileVersionV1,
        FileVersionV1,
        collab_types::FileConflictV1,
    ) {
        let parent = FileRecordId::new();
        let base = FileVersionV1 {
            id: FileVersionId::new(),
            record_id: fixture.key.record_id,
            parents: Vec::new(),
            actor: fixture.actor,
            timestamp_ms: 1,
            content_hash: Some(ContentHash([0; 32])),
            metadata_hash: ContentHash([10; 32]),
            provenance: FileProvenance::Create,
        };
        let current = FileVersionV1 {
            id: FileVersionId::new(),
            record_id: fixture.key.record_id,
            parents: vec![base.id],
            actor: fixture.actor,
            timestamp_ms: 2,
            content_hash: Some(ContentHash([1; 32])),
            metadata_hash: ContentHash([11; 32]),
            provenance: FileProvenance::Edit,
        };
        let other = FileVersionV1 {
            id: FileVersionId::new(),
            record_id: fixture.key.record_id,
            parents: vec![base.id],
            actor: fixture.actor,
            timestamp_ms: 3,
            content_hash: Some(ContentHash([2; 32])),
            metadata_hash: ContentHash([12; 32]),
            provenance: FileProvenance::Edit,
        };
        let conflict = collab_types::FileConflictV1 {
            id: Uuid::new_v4(),
            record_id: fixture.key.record_id,
            versions: vec![current.id, other.id],
            status: collab_types::ConflictStatus::Open,
            resolution: None,
        };
        let result = FileVersionV1 {
            id: FileVersionId::new(),
            record_id: fixture.key.record_id,
            parents: conflict.versions.clone(),
            actor: fixture.actor,
            timestamp_ms: 4,
            content_hash: Some(ContentHash([3; 32])),
            metadata_hash: ContentHash([13; 32]),
            provenance: FileProvenance::ConflictResolution,
        };
        let mut participant = FileMutationParticipantV1 {
            record_id: fixture.key.record_id,
            role: FileMutationParticipantRoleV1::MergeSource,
            expected_version: current.id,
            proposed_binding: FileBindingV1 {
                schema_version: FILE_BINDING_VERSION,
                record_id: fixture.key.record_id,
                parent: Some(parent),
                name: "source.txt".into(),
                kind: FileNodeKind::File,
                tombstoned: false,
                actor: fixture.actor,
                observed_at_ms: 4,
            },
            proposed_content_revision: Some(collab_types::ContentRevisionV1 {
                schema_version: CONTENT_REVISION_VERSION,
                record_id: fixture.key.record_id,
                version_id: result.id,
                content_hash: result.content_hash,
                byte_len: 3,
                metadata_hash: result.metadata_hash,
            }),
            proposed_version: result,
            projection_hash: ContentHash([0; 32]),
        };
        participant.projection_hash = participant.computed_projection_hash().unwrap();
        let transaction = FileMutationTransactionV1::signed(
            FileMutationTransactionManifestV1 {
                schema_version: collab_types::files::FILE_MUTATION_TRANSACTION_VERSION_V2,
                transaction_id: Uuid::new_v4(),
                workspace_id: fixture.key.workspace_id,
                actor: fixture.actor,
                timestamp_ms: 4,
                operation: FileMutationOperationV1::MergeContentConflict {
                    source_record: fixture.key.record_id,
                    parent_record: parent,
                    source_name: "source.txt".into(),
                    conflict_id: conflict.id,
                    conflict_versions: conflict.versions.clone(),
                    current_version: current.id,
                    base_version: matches!(codec, ConflictMergeCodecV1::PlainUtf8ThreeWayV1)
                        .then_some(base.id),
                    codec,
                },
                participants: vec![participant],
            },
            &fixture.device_key,
        )
        .unwrap();
        (transaction, parent, base, current, other, conflict)
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Atomic merge and restart scenario.
    fn completed_single_record_merge_projects_atomically_and_survives_restart() {
        for codec in [
            ConflictMergeCodecV1::PlainUtf8ThreeWayV1,
            ConflictMergeCodecV1::QdocAutomergeV1,
        ] {
            let fixture = Fixture::new();
            let (transaction, parent, base, current, other, conflict) =
                merge_transaction(&fixture, codec);
            let transaction_id = transaction.manifest.transaction_id;
            let result_id = transaction.manifest.participants[0].proposed_version.id;
            let repository = RecordRepository::open(&fixture.root).unwrap();
            let auth = TestAuthorizer {
                actor: fixture.actor,
                record: fixture.key.record_id,
                read: true,
                write: true,
            };
            repository
                .apply_batch(
                    fixture.key,
                    &[
                        RecordEnvelopeV1::signed(
                            fixture.key.record_id,
                            fixture.actor,
                            1,
                            RecordOperationV1::SetBinding(FileBindingV1 {
                                schema_version: FILE_BINDING_VERSION,
                                record_id: fixture.key.record_id,
                                parent: Some(parent),
                                name: "source.txt".into(),
                                kind: FileNodeKind::File,
                                tombstoned: false,
                                actor: fixture.actor,
                                observed_at_ms: 1,
                            }),
                            &fixture.device_key,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            fixture.key.record_id,
                            fixture.actor,
                            2,
                            RecordOperationV1::AppendVersion(base),
                            &fixture.device_key,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            fixture.key.record_id,
                            fixture.actor,
                            3,
                            RecordOperationV1::AppendVersion(current.clone()),
                            &fixture.device_key,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            fixture.key.record_id,
                            fixture.actor,
                            4,
                            RecordOperationV1::AppendVersion(other),
                            &fixture.device_key,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            fixture.key.record_id,
                            fixture.actor,
                            5,
                            RecordOperationV1::SetContentRevision(ContentRevisionV1 {
                                schema_version: CONTENT_REVISION_VERSION,
                                record_id: fixture.key.record_id,
                                version_id: current.id,
                                content_hash: current.content_hash,
                                byte_len: 1,
                                metadata_hash: current.metadata_hash,
                            }),
                            &fixture.device_key,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            fixture.key.record_id,
                            fixture.actor,
                            6,
                            RecordOperationV1::AppendConflict(conflict.clone()),
                            &fixture.device_key,
                        )
                        .unwrap(),
                    ],
                    &auth,
                )
                .unwrap();
            repository
                .apply(
                    fixture.key,
                    &RecordEnvelopeV1::signed(
                        fixture.key.record_id,
                        fixture.actor,
                        7,
                        RecordOperationV1::PrepareTransaction(transaction.clone()),
                        &fixture.device_key,
                    )
                    .unwrap(),
                    &auth,
                )
                .unwrap();
            assert!(
                !repository
                    .transaction_status(fixture.key.workspace_id, transaction_id)
                    .unwrap()
                    .unwrap()
                    .complete
            );
            assert_eq!(
                repository
                    .snapshot(fixture.key, &fixture.actor, &auth)
                    .unwrap()
                    .content_revision
                    .unwrap()
                    .version_id,
                current.id
            );
            repository
                .apply(
                    fixture.key,
                    &RecordEnvelopeV1::signed(
                        fixture.key.record_id,
                        fixture.actor,
                        8,
                        RecordOperationV1::CommitTransaction { transaction_id },
                        &fixture.device_key,
                    )
                    .unwrap(),
                    &auth,
                )
                .unwrap();
            let snapshot = repository
                .snapshot(fixture.key, &fixture.actor, &auth)
                .unwrap();
            assert_eq!(snapshot.content_revision.unwrap().version_id, result_id);
            assert_eq!(
                snapshot
                    .conflicts
                    .iter()
                    .find(|value| value.id == conflict.id)
                    .unwrap()
                    .resolution
                    .as_ref()
                    .unwrap()
                    .strategy,
                collab_types::ConflictStrategy::Merge
            );
            drop(repository);
            let repository = RecordRepository::open(&fixture.root).unwrap();
            assert_eq!(
                repository
                    .snapshot(fixture.key, &fixture.actor, &auth)
                    .unwrap()
                    .content_revision
                    .unwrap()
                    .version_id,
                result_id
            );
            assert_eq!(
                repository
                    .complete_transactions(fixture.key.workspace_id)
                    .unwrap(),
                vec![transaction]
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Both participant orders form one contract matrix.
    fn keep_both_remote_participant_orders_never_project_one_half() {
        for source_first in [true, false] {
            let fixture = Fixture::new();
            let (transaction, parent, sibling, current, selected, conflict) =
                keep_both_transaction(&fixture);
            let repository = RecordRepository::open(&fixture.root).unwrap();
            let auth = MultiRecordAuthorizer {
                actor: fixture.actor,
                records: [fixture.key.record_id, sibling].into_iter().collect(),
            };
            repository
                .apply_batch(
                    fixture.key,
                    &[
                        RecordEnvelopeV1::signed(
                            fixture.key.record_id,
                            fixture.actor,
                            1,
                            RecordOperationV1::SetBinding(FileBindingV1 {
                                schema_version: FILE_BINDING_VERSION,
                                record_id: fixture.key.record_id,
                                parent: Some(parent),
                                name: "source.txt".into(),
                                kind: FileNodeKind::File,
                                tombstoned: false,
                                actor: fixture.actor,
                                observed_at_ms: 1,
                            }),
                            &fixture.device_key,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            fixture.key.record_id,
                            fixture.actor,
                            2,
                            RecordOperationV1::AppendVersion(current.clone()),
                            &fixture.device_key,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            fixture.key.record_id,
                            fixture.actor,
                            3,
                            RecordOperationV1::AppendVersion(selected.clone()),
                            &fixture.device_key,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            fixture.key.record_id,
                            fixture.actor,
                            4,
                            RecordOperationV1::SetContentRevision(
                                collab_types::ContentRevisionV1 {
                                    schema_version: CONTENT_REVISION_VERSION,
                                    record_id: fixture.key.record_id,
                                    version_id: current.id,
                                    content_hash: current.content_hash,
                                    byte_len: 1,
                                    metadata_hash: current.metadata_hash,
                                },
                            ),
                            &fixture.device_key,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            fixture.key.record_id,
                            fixture.actor,
                            5,
                            RecordOperationV1::AppendConflict(conflict),
                            &fixture.device_key,
                        )
                        .unwrap(),
                    ],
                    &auth,
                )
                .unwrap();
            let order = if source_first {
                [fixture.key.record_id, sibling]
            } else {
                [sibling, fixture.key.record_id]
            };
            for (index, record_id) in order.into_iter().enumerate() {
                let sequence = if record_id == fixture.key.record_id {
                    6
                } else {
                    1
                };
                repository
                    .apply_batch(
                        RecordRepositoryKey {
                            workspace_id: fixture.key.workspace_id,
                            record_id,
                        },
                        &[
                            RecordEnvelopeV1::signed(
                                record_id,
                                fixture.actor,
                                sequence,
                                RecordOperationV1::PrepareTransaction(transaction.clone()),
                                &fixture.device_key,
                            )
                            .unwrap(),
                            RecordEnvelopeV1::signed(
                                record_id,
                                fixture.actor,
                                sequence + 1,
                                RecordOperationV1::CommitTransaction {
                                    transaction_id: transaction.manifest.transaction_id,
                                },
                                &fixture.device_key,
                            )
                            .unwrap(),
                        ],
                        &auth,
                    )
                    .unwrap();
                let status = repository
                    .transaction_status(
                        fixture.key.workspace_id,
                        transaction.manifest.transaction_id,
                    )
                    .unwrap()
                    .unwrap();
                assert_eq!(status.complete, index == 1);
                let source = repository
                    .snapshot(fixture.key, &fixture.actor, &auth)
                    .unwrap();
                if index == 0 {
                    assert_eq!(source.content_revision.unwrap().version_id, current.id);
                    let target = repository.snapshot(
                        RecordRepositoryKey {
                            workspace_id: fixture.key.workspace_id,
                            record_id: sibling,
                        },
                        &fixture.actor,
                        &auth,
                    );
                    match target {
                        Ok(snapshot) => assert!(snapshot.binding.is_none()),
                        Err(RecordRepositoryError::RecordNotFound) => {}
                        Err(error) => panic!("unexpected target projection error: {error:?}"),
                    }
                } else {
                    assert_eq!(
                        source.content_revision.unwrap().version_id,
                        transaction
                            .manifest
                            .participants
                            .iter()
                            .find(|value| value.record_id == fixture.key.record_id)
                            .unwrap()
                            .proposed_version
                            .id
                    );
                    assert!(
                        repository
                            .snapshot(
                                RecordRepositoryKey {
                                    workspace_id: fixture.key.workspace_id,
                                    record_id: sibling,
                                },
                                &fixture.actor,
                                &auth,
                            )
                            .unwrap()
                            .binding
                            .is_some()
                    );
                }
            }
        }
    }

    #[test]
    fn transaction_stays_incomplete_until_every_exact_commit_and_survives_restart() {
        let fixture = Fixture::new();
        let replaced = FileRecordId::new();
        let transaction = replacement_transaction(&fixture, replaced);
        let transaction_id = transaction.manifest.transaction_id;
        let repository = RecordRepository::open(&fixture.root).unwrap();
        for (index, record_id) in [fixture.key.record_id, replaced].into_iter().enumerate() {
            let key = RecordRepositoryKey {
                workspace_id: fixture.key.workspace_id,
                record_id,
            };
            let auth = TestAuthorizer {
                actor: fixture.actor,
                record: record_id,
                read: true,
                write: true,
            };
            let prepare = RecordEnvelopeV1::signed(
                record_id,
                fixture.actor,
                1,
                RecordOperationV1::PrepareTransaction(transaction.clone()),
                &fixture.device_key,
            )
            .unwrap();
            repository.apply(key, &prepare, &auth).unwrap();
            let status = repository
                .transaction_status(fixture.key.workspace_id, transaction_id)
                .unwrap()
                .unwrap();
            assert!(!status.complete);
            assert_eq!(status.prepared_participants, index + 1);
            let commit = RecordEnvelopeV1::signed(
                record_id,
                fixture.actor,
                2,
                RecordOperationV1::CommitTransaction { transaction_id },
                &fixture.device_key,
            )
            .unwrap();
            repository.apply(key, &commit, &auth).unwrap();
        }
        assert!(
            repository
                .transaction_status(fixture.key.workspace_id, transaction_id)
                .unwrap()
                .unwrap()
                .complete
        );
        assert_eq!(
            repository
                .complete_transactions(fixture.key.workspace_id)
                .unwrap(),
            vec![transaction.clone()]
        );
        let recovered = repository
            .completed_transaction_journal_entry(fixture.key.workspace_id, &transaction)
            .unwrap();
        assert_eq!(recovered.transaction, transaction);
        assert_eq!(
            recovered
                .participant_batches
                .iter()
                .map(|batch| batch.record_id)
                .collect::<Vec<_>>(),
            recovered
                .transaction
                .manifest
                .participants
                .iter()
                .map(|participant| participant.record_id)
                .collect::<Vec<_>>()
        );
        assert!(recovered.participant_batches.iter().all(|batch| {
            matches!(
                batch.envelopes.as_slice(),
                [
                    RecordEnvelopeV1 {
                        operation: RecordOperationV1::PrepareTransaction(_),
                        ..
                    },
                    RecordEnvelopeV1 {
                        operation: RecordOperationV1::CommitTransaction { .. },
                        ..
                    }
                ]
            )
        }));
        drop(repository);
        assert!(
            RecordRepository::open(&fixture.root)
                .unwrap()
                .transaction_status(fixture.key.workspace_id, transaction_id)
                .unwrap()
                .unwrap()
                .complete
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Crash-journal replay is checked phase by phase.
    fn journaled_repository_commit_projects_neither_half_and_replays_exactly() {
        let fixture = Fixture::new();
        let replaced = FileRecordId::new();
        let transaction = replacement_transaction(&fixture, replaced);
        let repository = RecordRepository::open(&fixture.root).unwrap();
        let records = [fixture.key.record_id, replaced]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let auth = MultiRecordAuthorizer {
            actor: fixture.actor,
            records,
        };
        let collab_types::FileMutationOperationV1::ReplacementRename {
            source_parent,
            source_name,
            destination_parent,
            destination_name,
            source_record,
            replaced_record,
        } = &transaction.manifest.operation
        else {
            unreachable!("replacement fixture operation")
        };
        for participant in &transaction.manifest.participants {
            let (parent, name) = if participant.record_id == *source_record {
                (*source_parent, source_name.clone())
            } else {
                assert_eq!(participant.record_id, *replaced_record);
                (*destination_parent, destination_name.clone())
            };
            let baseline = FileVersionV1 {
                id: participant.expected_version,
                record_id: participant.record_id,
                parents: Vec::new(),
                actor: fixture.actor,
                timestamp_ms: 1,
                content_hash: None,
                metadata_hash: ContentHash([1; 32]),
                provenance: FileProvenance::Create,
            };
            let key = RecordRepositoryKey {
                workspace_id: fixture.key.workspace_id,
                record_id: participant.record_id,
            };
            repository
                .apply_batch(
                    key,
                    &[
                        RecordEnvelopeV1::signed(
                            participant.record_id,
                            fixture.actor,
                            1,
                            RecordOperationV1::SetBinding(FileBindingV1 {
                                schema_version: FILE_BINDING_VERSION,
                                record_id: participant.record_id,
                                parent: Some(parent),
                                name,
                                kind: FileNodeKind::File,
                                tombstoned: false,
                                actor: fixture.actor,
                                observed_at_ms: 1,
                            }),
                            &fixture.device_key,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            participant.record_id,
                            fixture.actor,
                            2,
                            RecordOperationV1::AppendVersion(baseline.clone()),
                            &fixture.device_key,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            participant.record_id,
                            fixture.actor,
                            3,
                            RecordOperationV1::SetContentRevision(
                                collab_types::ContentRevisionV1 {
                                    schema_version: CONTENT_REVISION_VERSION,
                                    record_id: participant.record_id,
                                    version_id: baseline.id,
                                    content_hash: None,
                                    byte_len: 0,
                                    metadata_hash: baseline.metadata_hash,
                                },
                            ),
                            &fixture.device_key,
                        )
                        .unwrap(),
                    ],
                    &auth,
                )
                .unwrap();
        }
        let participant_batches = transaction
            .manifest
            .participants
            .iter()
            .map(
                |participant| crate::record_transaction::RecordTransactionParticipantBatch {
                    record_id: participant.record_id,
                    envelopes: vec![
                        RecordEnvelopeV1::signed(
                            participant.record_id,
                            fixture.actor,
                            4,
                            RecordOperationV1::PrepareTransaction(transaction.clone()),
                            &fixture.device_key,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            participant.record_id,
                            fixture.actor,
                            5,
                            RecordOperationV1::CommitTransaction {
                                transaction_id: transaction.manifest.transaction_id,
                            },
                            &fixture.device_key,
                        )
                        .unwrap(),
                    ],
                },
            )
            .collect();
        let prepared = crate::record_transaction::RecordTransactionJournalEntry {
            schema_version: crate::record_transaction::RECORD_TRANSACTION_JOURNAL_VERSION,
            transaction: transaction.clone(),
            participant_batches,
            phase: crate::record_transaction::RecordTransactionPhase::Prepared,
            namespace_delta: None,
        };
        let journal_root = fixture.dir.path().join("transaction-journal");
        let mut journal =
            crate::record_transaction::RecordTransactionJournal::open(&journal_root).unwrap();
        let failure = repository.apply_transaction_with_checkpoint(
            &mut journal,
            &prepared,
            &auth,
            |checkpoint| {
                if matches!(
                    checkpoint,
                    RecordTransactionCheckpoint::ParticipantRecordDurable { index: 0 }
                ) {
                    Err(std::io::Error::other("simulated crash"))
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(
            failure,
            Err(RecordRepositoryError::TransactionFailureInjected(_))
        ));
        assert_eq!(
            journal
                .get(transaction.manifest.transaction_id)
                .unwrap()
                .phase,
            crate::record_transaction::RecordTransactionPhase::Prepared
        );
        drop(repository);
        drop(journal);
        let repository = RecordRepository::open(&fixture.root).unwrap();
        let mut journal =
            crate::record_transaction::RecordTransactionJournal::open(&journal_root).unwrap();
        let recovery = journal
            .get(transaction.manifest.transaction_id)
            .unwrap()
            .clone();
        repository
            .apply_transaction(&mut journal, &recovery, &auth)
            .unwrap();
        assert_eq!(repository.len(), 2);
        for participant in &transaction.manifest.participants {
            let snapshot = repository
                .snapshot(
                    RecordRepositoryKey {
                        workspace_id: fixture.key.workspace_id,
                        record_id: participant.record_id,
                    },
                    &fixture.actor,
                    &auth,
                )
                .unwrap();
            assert_eq!(
                snapshot.binding.as_ref(),
                Some(&participant.proposed_binding)
            );
            assert!(snapshot.versions.contains(&participant.proposed_version));
        }
    }

    #[test]
    fn restart_preserves_snapshot_and_replay_watermark() {
        let fixture = Fixture::new();
        let repo = RecordRepository::open(&fixture.root).unwrap();
        let first = fixture.metadata_envelope(1);
        repo.apply(fixture.key, &first, &fixture.auth(true, true))
            .unwrap();
        drop(repo);

        let reopened = RecordRepository::open(&fixture.root).unwrap();
        let snapshot = reopened
            .snapshot(fixture.key, &fixture.actor, &fixture.auth(true, false))
            .unwrap();
        assert_eq!(snapshot.metadata.unwrap().observed_at_ms, 1);
        assert!(matches!(
            reopened.apply(fixture.key, &first, &fixture.auth(true, true)),
            Err(RecordRepositoryError::Replication(
                RecordReplicationError::Replay { sequence: 1 }
            ))
        ));
        assert!(reopened.is_latest_envelope(fixture.key, &first));
        let second = fixture.metadata_envelope(2);
        reopened
            .apply(fixture.key, &second, &fixture.auth(true, true))
            .unwrap();
        assert!(!reopened.is_latest_envelope(fixture.key, &first));
        assert!(reopened.is_latest_envelope(fixture.key, &second));
    }

    #[test]
    fn tampered_signed_state_is_rejected_on_restart() {
        let fixture = Fixture::new();
        let repo = RecordRepository::open(&fixture.root).unwrap();
        repo.apply(
            fixture.key,
            &fixture.metadata_envelope(1),
            &fixture.auth(true, true),
        )
        .unwrap();
        drop(repo);
        let path = record_path(&fixture.root, fixture.key);
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["envelopes"][0]["sequence"] = serde_json::json!(2);
        std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(matches!(
            RecordRepository::open(&fixture.root),
            Err(RecordRepositoryError::Replication(
                RecordReplicationError::Signature(_)
            ))
        ));
    }

    #[test]
    fn tampered_primary_recovers_latest_valid_signed_generation_with_evidence() {
        let fixture = Fixture::new();
        let repo = RecordRepository::open(&fixture.root).unwrap();
        repo.apply(
            fixture.key,
            &fixture.metadata_envelope(1),
            &fixture.auth(true, true),
        )
        .unwrap();
        repo.apply(
            fixture.key,
            &fixture.metadata_envelope(2),
            &fixture.auth(true, true),
        )
        .unwrap();
        drop(repo);

        let path = record_path(&fixture.root, fixture.key);
        let damaged = b"{\"schema_version\":1,\"tampered\":true}";
        std::fs::write(&path, damaged).unwrap();
        let reopened = RecordRepository::open(&fixture.root).unwrap();
        let snapshot = reopened
            .snapshot(fixture.key, &fixture.actor, &fixture.auth(true, false))
            .unwrap();
        assert_eq!(snapshot.metadata.unwrap().observed_at_ms, 1);

        let backup_dir = path.with_file_name(format!(
            ".{}.qos-backups",
            path.file_name().unwrap().to_string_lossy()
        ));
        let entries = std::fs::read_dir(backup_dir)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect::<Vec<_>>();
        let evidence = entries
            .iter()
            .find(|entry| entry.file_name().to_string_lossy().starts_with("evidence-"))
            .unwrap();
        assert_eq!(std::fs::read(evidence.path()).unwrap(), damaged);
        assert!(
            entries
                .iter()
                .any(|entry| { entry.file_name().to_string_lossy().starts_with("recovery-") })
        );
    }

    #[test]
    fn read_only_authorizer_cannot_create_durable_state() {
        let fixture = Fixture::new();
        let repo = RecordRepository::open(&fixture.root).unwrap();
        assert!(matches!(
            repo.apply(
                fixture.key,
                &fixture.metadata_envelope(1),
                &fixture.auth(true, false)
            ),
            Err(RecordRepositoryError::Replication(
                RecordReplicationError::WriteDenied
            ))
        ));
        assert!(repo.is_empty());
        assert!(!record_path(&fixture.root, fixture.key).exists());
    }

    #[test]
    fn immutable_version_collision_does_not_replace_persisted_state() {
        let fixture = Fixture::new();
        let repo = RecordRepository::open(&fixture.root).unwrap();
        let version_id = FileVersionId::new();
        let first = fixture.version_envelope(1, version_id, 1);
        repo.apply(fixture.key, &first, &fixture.auth(true, true))
            .unwrap();
        let collision = fixture.version_envelope(2, version_id, 2);
        assert!(matches!(
            repo.apply(fixture.key, &collision, &fixture.auth(true, true)),
            Err(RecordRepositoryError::Replication(
                RecordReplicationError::VersionIdCollision(id)
            )) if id == version_id
        ));
        drop(repo);
        let reopened = RecordRepository::open(&fixture.root).unwrap();
        let snapshot = reopened
            .snapshot(fixture.key, &fixture.actor, &fixture.auth(true, false))
            .unwrap();
        assert_eq!(
            snapshot.versions,
            vec![match first.operation {
                RecordOperationV1::AppendVersion(version) => version,
                _ => unreachable!(),
            }]
        );
    }

    #[test]
    fn out_of_order_and_duplicate_persisted_operations_fail_closed() {
        let fixture = Fixture::new();
        let repo = RecordRepository::open(&fixture.root).unwrap();
        assert!(matches!(
            repo.apply(
                fixture.key,
                &fixture.metadata_envelope(2),
                &fixture.auth(true, true)
            ),
            Err(RecordRepositoryError::Replication(
                RecordReplicationError::SequenceGap {
                    expected: 1,
                    received: 2
                }
            ))
        ));
        repo.apply(
            fixture.key,
            &fixture.metadata_envelope(1),
            &fixture.auth(true, true),
        )
        .unwrap();
        assert!(matches!(
            repo.apply(
                fixture.key,
                &fixture.metadata_envelope(1),
                &fixture.auth(true, true)
            ),
            Err(RecordRepositoryError::Replication(
                RecordReplicationError::Replay { sequence: 1 }
            ))
        ));
        drop(repo);

        let path = record_path(&fixture.root, fixture.key);
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let duplicate = value["envelopes"][0].clone();
        value["envelopes"].as_array_mut().unwrap().push(duplicate);
        std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(matches!(
            RecordRepository::open(&fixture.root),
            Err(RecordRepositoryError::Replication(
                RecordReplicationError::Replay { sequence: 1 }
            ))
        ));
    }

    #[test]
    fn cross_record_state_is_rejected_on_apply_and_restart() {
        let fixture = Fixture::new();
        let repo = RecordRepository::open(&fixture.root).unwrap();
        let other_key = RecordRepositoryKey {
            workspace_id: fixture.key.workspace_id,
            record_id: FileRecordId::new(),
        };
        assert!(matches!(
            repo.apply(
                other_key,
                &fixture.metadata_envelope(1),
                &fixture.auth(true, true)
            ),
            Err(RecordRepositoryError::CrossRecordEnvelope)
        ));
        repo.apply(
            fixture.key,
            &fixture.metadata_envelope(1),
            &fixture.auth(true, true),
        )
        .unwrap();
        drop(repo);

        let old_path = record_path(&fixture.root, fixture.key);
        let new_path = record_path(&fixture.root, other_key);
        std::fs::rename(old_path, new_path).unwrap();
        assert!(matches!(
            RecordRepository::open(&fixture.root),
            Err(RecordRepositoryError::PathKeyMismatch)
        ));
    }

    #[test]
    fn failed_atomic_replace_does_not_publish_candidate_in_memory() {
        let fixture = Fixture::new();
        let repo = RecordRepository::open(&fixture.root).unwrap();
        repo.apply(
            fixture.key,
            &fixture.metadata_envelope(1),
            &fixture.auth(true, true),
        )
        .unwrap();
        let path = record_path(&fixture.root, fixture.key);
        let temp_name = format!(
            ".{}.{}.tmp",
            path.file_name().unwrap().to_string_lossy(),
            std::process::id()
        );
        let temp_path = path.parent().unwrap().join(temp_name);
        std::fs::create_dir(&temp_path).unwrap();

        assert!(matches!(
            repo.apply(
                fixture.key,
                &fixture.metadata_envelope(2),
                &fixture.auth(true, true)
            ),
            Err(RecordRepositoryError::Io(_))
        ));
        let snapshot = repo
            .snapshot(fixture.key, &fixture.actor, &fixture.auth(true, false))
            .unwrap();
        assert_eq!(snapshot.metadata.unwrap().observed_at_ms, 1);
        std::fs::remove_dir(temp_path).unwrap();
        drop(repo);

        let reopened = RecordRepository::open(&fixture.root).unwrap();
        let snapshot = reopened
            .snapshot(fixture.key, &fixture.actor, &fixture.auth(true, false))
            .unwrap();
        assert_eq!(snapshot.metadata.unwrap().observed_at_ms, 1);
    }

    #[test]
    fn atomic_batch_inventory_and_suffix_survive_restart() {
        let fixture = Fixture::new();
        let repo = RecordRepository::open(&fixture.root).unwrap();
        let first = fixture.metadata_envelope(1);
        let second = fixture.metadata_envelope(2);
        repo.apply_batch(
            fixture.key,
            &[first, second.clone()],
            &fixture.auth(true, true),
        )
        .unwrap();
        let inventory = repo
            .inventory_page(fixture.key.workspace_id, None, 8)
            .unwrap();
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].record_id, fixture.key.record_id);
        assert_eq!(inventory[0].actor, fixture.actor);
        assert_eq!(inventory[0].contiguous_sequence, 2);
        assert_eq!(
            repo.stream_cursor(fixture.key, &fixture.actor, 2).unwrap(),
            inventory[0]
        );
        assert!(
            repo.inventory_page(fixture.key.workspace_id, Some(inventory[0].key()), 8)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            repo.envelopes_after(fixture.key, &fixture.actor, 1, 8)
                .unwrap(),
            vec![second]
        );
        let digest = inventory[0].prefix_digest;
        drop(repo);
        let reopened = RecordRepository::open(&fixture.root).unwrap();
        assert_eq!(
            reopened
                .inventory_page(fixture.key.workspace_id, None, 8)
                .unwrap()[0]
                .prefix_digest,
            digest
        );
    }

    #[test]
    fn invalid_late_batch_operation_commits_no_prefix() {
        let fixture = Fixture::new();
        let repo = RecordRepository::open(&fixture.root).unwrap();
        let result = repo.apply_batch(
            fixture.key,
            &[fixture.metadata_envelope(1), fixture.metadata_envelope(3)],
            &fixture.auth(true, true),
        );
        assert!(matches!(
            result,
            Err(RecordRepositoryError::Replication(
                RecordReplicationError::SequenceGap {
                    expected: 2,
                    received: 3
                }
            ))
        ));
        assert!(repo.is_empty());
        assert!(!record_path(&fixture.root, fixture.key).exists());
    }
}
