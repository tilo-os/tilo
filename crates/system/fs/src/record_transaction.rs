//! Crash-safe intent journal for signed multi-record mutations.
//!
//! The journal never invents envelopes during recovery. Each prepared entry
//! contains the exact signed manifest and exact participant batches which will
//! be retried at every monotonic phase until broadcast has completed.

mod checked;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use fs_protocol::{FileMutationTransaction, FileRecordId};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::record_replication::{RecordEnvelopeV1, RecordOperationV1};

pub const RECORD_TRANSACTION_JOURNAL_VERSION: u16 = 1;
pub const MAX_RECORD_TRANSACTION_ENTRIES: usize = 1_024;
pub const MAX_RECORD_TRANSACTION_ENTRY_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordTransactionPhase {
    Prepared,
    ParticipantRecordsCommitted,
    HistoryCommitted,
    NamespaceCommitted,
    BroadcastCommitted,
}

impl RecordTransactionPhase {
    fn successor(self) -> Option<Self> {
        match self {
            Self::Prepared => Some(Self::ParticipantRecordsCommitted),
            Self::ParticipantRecordsCommitted => Some(Self::HistoryCommitted),
            Self::HistoryCommitted => Some(Self::NamespaceCommitted),
            Self::NamespaceCommitted => Some(Self::BroadcastCommitted),
            Self::BroadcastCommitted => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordTransactionParticipantBatch {
    pub record_id: FileRecordId,
    pub envelopes: Vec<RecordEnvelopeV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordTransactionJournalEntry {
    pub schema_version: u16,
    pub transaction: FileMutationTransaction,
    /// Exact batches sorted by participant record ID.
    pub participant_batches: Vec<RecordTransactionParticipantBatch>,
    pub phase: RecordTransactionPhase,
    /// Exact persisted namespace delta once `NamespaceCommitted` is reached.
    pub namespace_delta: Option<Vec<u8>>,
    /// Local protected native intent, not an authority or observed-effect proof.
    /// Generic record replay cannot complete it. No successful native adapter is
    /// enrolled until the actual replacing `FsJob` receipt can be revalidated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_native_effect: Option<ExternalNativeEffectBinding>,
}

/// Exact local coupling to a genuine native operation and admitted source map.
/// These persisted identities describe the expected operation; they grant no
/// access and cannot assert that any native syscall occurred.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalNativeEffectBinding {
    pub(crate) operation_id: Uuid,
    pub(crate) source_owner: [u8; 32],
    pub(crate) source_identity: [u64; 2],
    pub(crate) victim_identity: [u64; 2],
}

impl RecordTransactionJournalEntry {
    /// Validate the complete journal entry and its signed participant batches.
    ///
    /// # Errors
    ///
    /// Returns an error when any schema, transaction, participant, phase, or
    /// namespace-delta invariant is invalid.
    pub fn validate(&self) -> Result<(), RecordTransactionJournalError> {
        if self.schema_version != RECORD_TRANSACTION_JOURNAL_VERSION {
            return Err(RecordTransactionJournalError::UnsupportedVersion(
                self.schema_version,
            ));
        }
        self.transaction
            .verify()
            .map_err(|_| RecordTransactionJournalError::InvalidTransaction)?;
        if let Some(native) = &self.external_native_effect
            && (native.operation_id != self.transaction.manifest.transaction_id
                || native.source_owner == [0; 32]
                || native.source_identity[1] == 0
                || native.victim_identity[1] == 0
                || !matches!(
                    self.transaction.manifest.operation,
                    fs_protocol::FileMutationOperation::ReplacementRename { .. }
                        | fs_protocol::FileMutationOperation::AtomicSave { .. }
                ))
        {
            return Err(RecordTransactionJournalError::InvalidExternalNativeEffect);
        }
        if self.participant_batches.len() != self.transaction.manifest.participants.len()
            || self
                .participant_batches
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left.record_id >= right.record_id))
        {
            return Err(RecordTransactionJournalError::InvalidParticipantSet);
        }
        for (batch, participant) in self
            .participant_batches
            .iter()
            .zip(&self.transaction.manifest.participants)
        {
            if batch.record_id != participant.record_id || batch.envelopes.len() != 2 {
                return Err(RecordTransactionJournalError::InvalidParticipantSet);
            }
            let [prepare, commit] = batch.envelopes.as_slice() else {
                return Err(RecordTransactionJournalError::InvalidParticipantSet);
            };
            prepare
                .verify()
                .map_err(|_| RecordTransactionJournalError::InvalidEnvelope)?;
            commit
                .verify()
                .map_err(|_| RecordTransactionJournalError::InvalidEnvelope)?;
            if prepare.record_id != batch.record_id
                || commit.record_id != batch.record_id
                || prepare.actor != self.transaction.manifest.actor
                || commit.actor != self.transaction.manifest.actor
                || prepare.sequence.checked_add(1) != Some(commit.sequence)
                || !matches!(
                    &prepare.operation,
                    RecordOperationV1::PrepareTransaction(value) if value == &self.transaction
                )
                || !matches!(
                    commit.operation,
                    RecordOperationV1::CommitTransaction { transaction_id }
                        if transaction_id == self.transaction.manifest.transaction_id
                )
            {
                return Err(RecordTransactionJournalError::InvalidEnvelopeBatch);
            }
        }
        if self.phase < RecordTransactionPhase::HistoryCommitted
            && self.namespace_delta.is_some()
            && !matches!(
                self.transaction.manifest.operation,
                fs_protocol::FileMutationOperation::RestoreContent { .. }
            )
            || self.phase >= RecordTransactionPhase::NamespaceCommitted
                && self.namespace_delta.is_none()
        {
            return Err(RecordTransactionJournalError::InvalidNamespaceDelta);
        }
        Ok(())
    }

    pub fn transaction_id(&self) -> Uuid {
        self.transaction.manifest.transaction_id
    }
}

pub struct RecordTransactionJournal {
    root: PathBuf,
    entries: BTreeMap<Uuid, RecordTransactionJournalEntry>,
}

impl RecordTransactionJournal {
    /// Open and validate a durable transaction journal.
    ///
    /// # Errors
    ///
    /// Returns an error when directory entries cannot be read or validated.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, RecordTransactionJournalError> {
        let root = root.into();
        let entries = load_entries(&root)?;
        Ok(Self { root, entries })
    }

    pub fn entries(&self) -> impl Iterator<Item = &RecordTransactionJournalEntry> {
        self.entries.values()
    }

    pub fn get(&self, id: Uuid) -> Option<&RecordTransactionJournalEntry> {
        self.entries.get(&id)
    }

    /// Durably install a prepared intent. An exact retry is idempotent;
    /// transaction-ID reuse with different bytes is corruption.
    pub fn insert_prepared(
        &mut self,
        entry: RecordTransactionJournalEntry,
    ) -> Result<bool, RecordTransactionJournalError> {
        if !self.validate_prepared(&entry)? {
            return Ok(false);
        }
        let id = entry.transaction_id();
        if let Err(error) = persist_entry(&self.root, &entry) {
            if matches!(
                entry.transaction.manifest.operation,
                fs_protocol::FileMutationOperation::RestoreContent { .. }
            ) {
                self.entries.insert(id, entry);
            }
            return Err(error);
        }
        self.entries.insert(id, entry);
        Ok(true)
    }

    pub(crate) fn validate_prepared(
        &self,
        entry: &RecordTransactionJournalEntry,
    ) -> Result<bool, RecordTransactionJournalError> {
        entry.validate()?;
        if entry.phase != RecordTransactionPhase::Prepared {
            return Err(RecordTransactionJournalError::InvalidInitialPhase);
        }
        let id = entry.transaction_id();
        if let Some(existing) = self.entries.get(&id) {
            return if existing == entry {
                Ok(false)
            } else {
                Err(RecordTransactionJournalError::TransactionIdCollision(id))
            };
        }
        if self.retained_count() >= MAX_RECORD_TRANSACTION_ENTRIES {
            return Err(RecordTransactionJournalError::TooManyEntries);
        }
        if serde_json::to_vec(entry)?.len() > MAX_RECORD_TRANSACTION_ENTRY_BYTES {
            return Err(RecordTransactionJournalError::EntryTooLarge);
        }
        Ok(true)
    }

    /// Persist exactly one monotonic phase transition. Repeating the current
    /// phase with identical delta is success; skipping phases is rejected.
    pub fn advance(
        &mut self,
        id: Uuid,
        phase: RecordTransactionPhase,
        namespace_delta: Option<Vec<u8>>,
    ) -> Result<bool, RecordTransactionJournalError> {
        let current = self
            .entries
            .get(&id)
            .ok_or(RecordTransactionJournalError::UnknownTransaction(id))?;
        if current.phase == phase {
            return if current.namespace_delta == namespace_delta {
                Ok(false)
            } else {
                Err(RecordTransactionJournalError::InvalidNamespaceDelta)
            };
        }
        if current.phase.successor() != Some(phase) {
            return Err(RecordTransactionJournalError::InvalidPhaseTransition {
                current: current.phase,
                proposed: phase,
            });
        }
        let mut next = current.clone();
        next.phase = phase;
        next.namespace_delta = namespace_delta;
        next.validate()?;
        persist_entry(&self.root, &next)?;
        self.entries.insert(id, next);
        Ok(true)
    }

    /// Store the deterministic namespace delta before mutating the live
    /// namespace. This closes the crash window between namespace persistence
    /// and the `NamespaceCommitted` phase write.
    pub fn stage_namespace_delta(
        &mut self,
        id: Uuid,
        delta: Vec<u8>,
    ) -> Result<bool, RecordTransactionJournalError> {
        let current = self
            .entries
            .get(&id)
            .ok_or(RecordTransactionJournalError::UnknownTransaction(id))?;
        if current.phase != RecordTransactionPhase::HistoryCommitted {
            return Err(RecordTransactionJournalError::InvalidPhaseTransition {
                current: current.phase,
                proposed: RecordTransactionPhase::HistoryCommitted,
            });
        }
        if let Some(existing) = &current.namespace_delta {
            return if existing == &delta {
                Ok(false)
            } else {
                Err(RecordTransactionJournalError::InvalidNamespaceDelta)
            };
        }
        let mut next = current.clone();
        next.namespace_delta = Some(delta);
        next.validate()?;
        persist_entry(&self.root, &next)?;
        self.entries.insert(id, next);
        Ok(true)
    }

    /// Remove only a fully broadcast entry and fsync the journal directory.
    pub fn remove_completed(&mut self, id: Uuid) -> Result<(), RecordTransactionJournalError> {
        let entry = self
            .entries
            .get(&id)
            .ok_or(RecordTransactionJournalError::UnknownTransaction(id))?;
        if entry.phase != RecordTransactionPhase::BroadcastCommitted {
            return Err(RecordTransactionJournalError::TransactionNotComplete(id));
        }
        // Completed checked restores are durable dedupe/compensation evidence.
        // Capacity remains reserved; never forget an ID and reinterpret retry
        // as a new authored operation. Ordinary transactions retain old cleanup.
        if matches!(
            entry.transaction.manifest.operation,
            fs_protocol::FileMutationOperation::RestoreContent { .. }
        ) {
            return Ok(());
        }
        let path = entry_path(&self.root, id);
        fs::remove_file(path)?;
        sync_dir(&self.root);
        self.entries.remove(&id);
        Ok(())
    }
}

fn load_entries(
    root: &Path,
) -> Result<BTreeMap<Uuid, RecordTransactionJournalEntry>, RecordTransactionJournalError> {
    if !root.exists() {
        return Ok(BTreeMap::new());
    }
    let mut entries = BTreeMap::new();
    for item in fs::read_dir(root)? {
        let item = item?;
        if !item.file_type()?.is_file() {
            return Err(RecordTransactionJournalError::UnexpectedEntry(item.path()));
        }
        let name = item
            .file_name()
            .into_string()
            .map_err(|_| RecordTransactionJournalError::UnexpectedEntry(item.path()))?;
        if name.starts_with('.') && name.to_ascii_lowercase().ends_with(".tmp") {
            continue;
        }
        let id = name
            .strip_suffix(".json")
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or_else(|| RecordTransactionJournalError::UnexpectedEntry(item.path()))?;
        let bytes = fs::read(item.path())?;
        if bytes.len() > MAX_RECORD_TRANSACTION_ENTRY_BYTES {
            return Err(RecordTransactionJournalError::EntryTooLarge);
        }
        let entry: RecordTransactionJournalEntry = serde_json::from_slice(&bytes)?;
        entry.validate()?;
        if entry.transaction_id() != id {
            return Err(RecordTransactionJournalError::PathIdMismatch);
        }
        if entries.insert(id, entry).is_some() {
            return Err(RecordTransactionJournalError::TransactionIdCollision(id));
        }
        if entries.len() > MAX_RECORD_TRANSACTION_ENTRIES {
            return Err(RecordTransactionJournalError::TooManyEntries);
        }
    }
    Ok(entries)
}

fn persist_entry(
    root: &Path,
    entry: &RecordTransactionJournalEntry,
) -> Result<(), RecordTransactionJournalError> {
    let bytes = serde_json::to_vec(entry)?;
    if bytes.len() > MAX_RECORD_TRANSACTION_ENTRY_BYTES {
        return Err(RecordTransactionJournalError::EntryTooLarge);
    }
    tilo_files::native::atomic::write_creating_parent(
        &entry_path(root, entry.transaction_id()),
        &bytes,
    )?;
    Ok(())
}

fn entry_path(root: &Path, id: Uuid) -> PathBuf {
    root.join(format!("{id}.json"))
}

fn sync_dir(path: &Path) {
    if let Ok(directory) = fs::File::open(path) {
        let _ = directory.sync_all();
    }
}

#[derive(Debug, Error)]
pub enum RecordTransactionJournalError {
    #[error("transaction journal I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("transaction journal contains invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported transaction journal entry version {0}")]
    UnsupportedVersion(u16),
    #[error("transaction signature or manifest is invalid")]
    InvalidTransaction,
    #[error("transaction envelope is invalid")]
    InvalidEnvelope,
    #[error("transaction participant set is not exact and sorted")]
    InvalidParticipantSet,
    #[error("transaction participant envelope batch is inconsistent")]
    InvalidEnvelopeBatch,
    #[error("external native effect binding differs from its replacement intent")]
    InvalidExternalNativeEffect,
    #[error("namespace delta does not match the journal phase")]
    InvalidNamespaceDelta,
    #[error("a new transaction must begin in Prepared")]
    InvalidInitialPhase,
    #[error("transaction journal has reached its entry bound")]
    TooManyEntries,
    #[error("transaction journal entry exceeds its byte bound")]
    EntryTooLarge,
    #[error("transaction id {0} is reused for a different intent")]
    TransactionIdCollision(Uuid),
    #[error("transaction {0} does not exist")]
    UnknownTransaction(Uuid),
    #[error("transaction {0} has not completed broadcast")]
    TransactionNotComplete(Uuid),
    #[error("invalid transaction phase transition from {current:?} to {proposed:?}")]
    InvalidPhaseTransition {
        current: RecordTransactionPhase,
        proposed: RecordTransactionPhase,
    },
    #[error("journal filename and embedded transaction id differ")]
    PathIdMismatch,
    #[error("transaction journal contains an unexpected entry at {0:?}")]
    UnexpectedEntry(PathBuf),
}

#[cfg(test)]
mod tests {
    use collab_types::Ed25519PublicKey;
    use fs_protocol::{
        CONTENT_REVISION_VERSION, ContentHash, FILE_BINDING_VERSION,
        FILE_MUTATION_TRANSACTION_VERSION, FileActor, FileBindingV1, FileMutationOperation,
        FileMutationParticipant, FileMutationParticipantRole, FileMutationTransactionManifest,
        FileNodeKind, FileProvenance, FileVersionId, FileVersionV1,
    };
    use getrandom::{SysRng, rand_core::UnwrapErr};

    use super::*;

    #[allow(
        clippy::too_many_lines,
        reason = "The journal fixture builds a signed replacement rename with source/replaced projections and prepare/commit envelopes."
    )] // Complete signed journal fixture construction.
    fn entry() -> RecordTransactionJournalEntry {
        let key = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let actor = FileActor {
            person: Ed25519PublicKey([7; 32]),
            device: Ed25519PublicKey(key.verifying_key().to_bytes()),
        };
        let source = FileRecordId::new();
        let replaced = FileRecordId::new();
        let destination_parent = FileRecordId::new();
        let participant = |record_id, role| {
            let expected = FileVersionId::new();
            let version = FileVersionV1 {
                id: FileVersionId::new(),
                record_id,
                parents: vec![expected],
                actor,
                timestamp_ms: 10,
                content_hash: None,
                metadata_hash: ContentHash([10; 32]),
                provenance: match role {
                    FileMutationParticipantRole::Source => FileProvenance::Rename,
                    FileMutationParticipantRole::Replaced => FileProvenance::Delete,
                    _ => unreachable!("replacement fixture role"),
                },
            };
            let mut participant = FileMutationParticipant {
                record_id,
                role,
                expected_version: expected,
                proposed_binding: FileBindingV1 {
                    schema_version: FILE_BINDING_VERSION,
                    record_id,
                    parent: Some(destination_parent),
                    name: "destination.txt".into(),
                    kind: FileNodeKind::File,
                    tombstoned: role == FileMutationParticipantRole::Replaced,
                    actor,
                    observed_at_ms: 10,
                },
                proposed_content_revision: Some(fs_protocol::ContentRevisionV1 {
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
            participant(source, FileMutationParticipantRole::Source),
            participant(replaced, FileMutationParticipantRole::Replaced),
        ];
        participants.sort_by_key(|participant| participant.record_id);
        let transaction = FileMutationTransaction::signed(
            FileMutationTransactionManifest {
                schema_version: FILE_MUTATION_TRANSACTION_VERSION,
                transaction_id: Uuid::new_v4(),
                workspace_id: Uuid::new_v4(),
                actor,
                timestamp_ms: 10,
                operation: FileMutationOperation::ReplacementRename {
                    source_parent: FileRecordId::new(),
                    source_name: "source.txt".into(),
                    destination_parent,
                    destination_name: "destination.txt".into(),
                    source_record: source,
                    replaced_record: replaced,
                },
                participants,
            },
            &key,
        )
        .unwrap();
        let participant_batches = transaction
            .manifest
            .participants
            .iter()
            .map(|participant| RecordTransactionParticipantBatch {
                record_id: participant.record_id,
                envelopes: vec![
                    RecordEnvelopeV1::signed(
                        participant.record_id,
                        actor,
                        1,
                        RecordOperationV1::PrepareTransaction(transaction.clone()),
                        &key,
                    )
                    .unwrap(),
                    RecordEnvelopeV1::signed(
                        participant.record_id,
                        actor,
                        2,
                        RecordOperationV1::CommitTransaction {
                            transaction_id: transaction.manifest.transaction_id,
                        },
                        &key,
                    )
                    .unwrap(),
                ],
            })
            .collect();
        RecordTransactionJournalEntry {
            schema_version: RECORD_TRANSACTION_JOURNAL_VERSION,
            transaction,
            participant_batches,
            phase: RecordTransactionPhase::Prepared,
            namespace_delta: None,
            external_native_effect: None,
        }
    }

    #[test]
    fn journal_restarts_exactly_with_monotonic_phases_and_whole_batches() {
        {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("transactions");
            let prepared = entry();
            let id = prepared.transaction_id();
            let mut journal = RecordTransactionJournal::open(&root).unwrap();
            assert!(journal.insert_prepared(prepared.clone()).unwrap());
            assert!(!journal.insert_prepared(prepared).unwrap());
            assert!(matches!(
                journal.advance(id, RecordTransactionPhase::HistoryCommitted, None),
                Err(RecordTransactionJournalError::InvalidPhaseTransition { .. })
            ));
            journal
                .advance(
                    id,
                    RecordTransactionPhase::ParticipantRecordsCommitted,
                    None,
                )
                .unwrap();
            journal
                .advance(id, RecordTransactionPhase::HistoryCommitted, None)
                .unwrap();
            let delta = vec![1, 2, 3];
            assert!(journal.stage_namespace_delta(id, delta.clone()).unwrap());
            assert!(!journal.stage_namespace_delta(id, delta.clone()).unwrap());
            journal
                .advance(
                    id,
                    RecordTransactionPhase::NamespaceCommitted,
                    Some(delta.clone()),
                )
                .unwrap();
            drop(journal);

            let mut reopened = RecordTransactionJournal::open(&root).unwrap();
            assert_eq!(
                reopened.get(id).unwrap().phase,
                RecordTransactionPhase::NamespaceCommitted
            );
            reopened
                .advance(id, RecordTransactionPhase::BroadcastCommitted, Some(delta))
                .unwrap();
            reopened.remove_completed(id).unwrap();
            assert!(reopened.get(id).is_none());
            assert_eq!(
                RecordTransactionJournal::open(root)
                    .unwrap()
                    .entries()
                    .count(),
                0
            );
        }
        {
            let mut value = entry();
            value.participant_batches.pop();
            assert!(matches!(
                value.validate(),
                Err(RecordTransactionJournalError::InvalidParticipantSet)
            ));

            let mut value = entry();
            value.participant_batches[0].envelopes.swap(0, 1);
            assert!(matches!(
                value.validate(),
                Err(RecordTransactionJournalError::InvalidEnvelopeBatch)
            ));
        }
    }

    fn signed_restore_entry() -> RecordTransactionJournalEntry {
        use fs_protocol::files::{
            CheckedHistoryOperationV1, CheckedHistoryRequestV1, FileRestoreReviewV1,
        };
        let key = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let mut manifest = entry().transaction.manifest;
        manifest.schema_version = fs_protocol::files::FILE_MUTATION_TRANSACTION_VERSION_V3;
        manifest.actor.device = Ed25519PublicKey(key.verifying_key().to_bytes());
        manifest
            .participants
            .retain(|participant| participant.role == FileMutationParticipantRole::Source);
        let selected = FileVersionId::new();
        let participant = &mut manifest.participants[0];
        participant.role = FileMutationParticipantRole::RestoreContent;
        participant.proposed_binding.actor = manifest.actor;
        participant.proposed_version.actor = manifest.actor;
        participant.proposed_version.provenance = FileProvenance::Restore { from: selected };
        participant.projection_hash = participant.computed_projection_hash().unwrap();
        let request = CheckedHistoryRequestV1 {
            schema_version: 1,
            intent_id: manifest.transaction_id,
            review: FileRestoreReviewV1 {
                schema_version: 1,
                workspace_id: manifest.workspace_id,
                mount_lifetime: Uuid::new_v4(),
                record_id: participant.record_id,
                frontier: [1; 32],
            },
            operation: CheckedHistoryOperationV1::Restore {
                version_id: selected,
            },
        };
        manifest.operation = FileMutationOperation::RestoreContent {
            request,
            selected_version: selected,
            prior_revision: fs_protocol::ContentRevisionV1 {
                version_id: participant.expected_version,
                ..participant.proposed_content_revision.unwrap()
            },
            compensates: None,
        };
        let record_id = participant.record_id;
        let actor = manifest.actor;
        let transaction = FileMutationTransaction::signed(manifest, &key).unwrap();
        let envelopes = vec![
            RecordEnvelopeV1::signed(
                record_id,
                actor,
                1,
                RecordOperationV1::PrepareTransaction(transaction.clone()),
                &key,
            )
            .unwrap(),
            RecordEnvelopeV1::signed(
                record_id,
                actor,
                2,
                RecordOperationV1::CommitTransaction {
                    transaction_id: transaction.manifest.transaction_id,
                },
                &key,
            )
            .unwrap(),
        ];
        let value = RecordTransactionJournalEntry {
            schema_version: RECORD_TRANSACTION_JOURNAL_VERSION,
            transaction,
            participant_batches: vec![RecordTransactionParticipantBatch {
                record_id,
                envelopes,
            }],
            phase: RecordTransactionPhase::Prepared,
            namespace_delta: Some(vec![1, 2, 3]),
            external_native_effect: None,
        };
        value.validate().unwrap();
        value
    }

    #[test]
    fn checked_journal_retention_keeps_signed_dedupe_and_ordinary_pruning() {
        let directory = tempfile::tempdir().unwrap();
        let mut journal = RecordTransactionJournal::open(directory.path()).unwrap();
        let ordinary = entry();
        let id = ordinary.transaction_id();
        let restore = signed_restore_entry();
        let restore_id = restore.transaction_id();
        journal.insert_prepared(ordinary).unwrap();
        journal.insert_prepared(restore.clone()).unwrap();
        let request = journal.checked_request(restore_id).unwrap();
        let receipt = journal.checked_receipt(restore_id).unwrap();
        for phase in [
            RecordTransactionPhase::ParticipantRecordsCommitted,
            RecordTransactionPhase::HistoryCommitted,
            RecordTransactionPhase::NamespaceCommitted,
            RecordTransactionPhase::BroadcastCommitted,
        ] {
            let delta =
                (phase >= RecordTransactionPhase::NamespaceCommitted).then(|| vec![1, 2, 3]);
            journal.advance(id, phase, delta).unwrap();
            journal
                .advance(restore_id, phase, Some(vec![1, 2, 3]))
                .unwrap();
        }
        let mixed = RecordTransactionJournal::open(directory.path()).unwrap();
        assert_eq!(mixed.retained_count(), 2);
        assert!(mixed.get(id).is_some());
        assert_eq!(mixed.checked_request(restore_id), Some(request));
        drop(mixed);
        journal.remove_completed(id).unwrap();
        journal.remove_completed(restore_id).unwrap();
        let reopened = RecordTransactionJournal::open(directory.path()).unwrap();
        assert!(reopened.get(id).is_none());
        assert_eq!(reopened.checked_request(restore_id), Some(request));
        assert_eq!(reopened.checked_receipt(restore_id), Some(receipt));
        assert!(matches!(
            journal.insert_prepared(restore),
            Err(RecordTransactionJournalError::TransactionIdCollision(collision)) if collision == restore_id
        ));
    }

    #[test]
    fn checked_journal_capacity_never_evicts_and_rejects_invalid_signed_entries() {
        let directory = tempfile::tempdir().unwrap();
        let mut journal = RecordTransactionJournal::open(directory.path()).unwrap();
        let restore = signed_restore_entry();
        for _ in 0..MAX_RECORD_TRANSACTION_ENTRIES {
            let value = signed_restore_entry();
            journal.entries.insert(value.transaction_id(), value);
        }
        assert!(matches!(
            journal.insert_prepared(restore.clone()),
            Err(RecordTransactionJournalError::TooManyEntries)
        ));
        assert_eq!(journal.retained_count(), MAX_RECORD_TRANSACTION_ENTRIES);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        let mut inconsistent = restore.clone();
        if let FileMutationOperation::RestoreContent { compensates, .. } =
            &mut inconsistent.transaction.manifest.operation
        {
            *compensates = Some(Uuid::new_v4());
        }
        assert!(inconsistent.validate().is_err());
        let mut oversized = restore;
        oversized
            .namespace_delta
            .as_mut()
            .unwrap()
            .resize(MAX_RECORD_TRANSACTION_ENTRY_BYTES, 255);
        let mut empty = RecordTransactionJournal::open(directory.path()).unwrap();
        assert!(matches!(
            empty.insert_prepared(oversized),
            Err(RecordTransactionJournalError::EntryTooLarge)
        ));
        assert_eq!(empty.retained_count(), 0);
    }

    fn external_entry() -> RecordTransactionJournalEntry {
        let mut value = entry();
        value.external_native_effect = Some(ExternalNativeEffectBinding {
            operation_id: value.transaction_id(),
            source_owner: [9; 32],
            source_identity: [1, 2],
            victim_identity: [1, 3],
        });
        value.validate().unwrap();
        value
    }

    struct ExactAuthorizer {
        actor: fs_protocol::FileActor,
        records: Vec<FileRecordId>,
    }
    impl crate::record_replication::RecordAuthorizer for ExactAuthorizer {
        fn actor_is_certified(&self, actor: &fs_protocol::FileActor) -> bool {
            *actor == self.actor
        }
        fn allows(
            &self,
            actor: &fs_protocol::FileActor,
            record: FileRecordId,
            _: crate::record_replication::RecordAction,
        ) -> bool {
            *actor == self.actor && self.records.contains(&record)
        }
    }

    #[test]
    fn external_native_intent_never_replays_as_observed_effect() {
        use crate::record_repository::{RecordRepository, RecordRepositoryError};
        use crate::transaction_coordinator::{
            RecordTransactionCoordinatorError, complete_replacement_transaction,
            complete_replacement_transaction_after_repository,
        };
        use std::sync::{Arc, Mutex};
        let disk = tempfile::tempdir().unwrap();
        let value = external_entry();
        let id = value.transaction_id();
        let authorizer = ExactAuthorizer {
            actor: value.transaction.manifest.actor,
            records: value
                .transaction
                .manifest
                .participants
                .iter()
                .map(|p| p.record_id)
                .collect(),
        };
        let journal_path = disk.path().join("journal");
        let mut journal = RecordTransactionJournal::open(&journal_path).unwrap();
        journal.insert_prepared(value.clone()).unwrap();
        drop(journal);
        let mut journal = RecordTransactionJournal::open(&journal_path).unwrap();
        assert_eq!(journal.get(id).unwrap(), &value);
        let repository = RecordRepository::open(disk.path().join("records")).unwrap();
        let history =
            crate::history::FileHistoryStore::open(disk.path().join("history.bin")).unwrap();
        let namespace = Arc::new(Mutex::new(
            crate::namespace::Namespace::new("external-negative").unwrap(),
        ));
        let namespace_path = disk.path().join("namespace.bin");
        let mut stripped = value.clone();
        stripped.external_native_effect = None;
        assert!(matches!(
            journal.insert_prepared(stripped.clone()),
            Err(RecordTransactionJournalError::TransactionIdCollision(pending)) if pending == id
        ));
        for supplied in [&value, &stripped] {
            assert!(matches!(
                repository.apply_transaction(&mut journal, supplied, &authorizer),
                Err(RecordRepositoryError::ExternalNativeEffectPending(pending)) if pending == id
            ));
            assert!(matches!(
                complete_replacement_transaction(
                    &repository, &mut journal, &history, &namespace,
                    &namespace_path, supplied, &authorizer, |_| panic!("unobserved native effect must never broadcast"),
                ),
                Err(RecordTransactionCoordinatorError::ExternalNativeEffectPending(pending)) if pending == id
            ));
            assert!(matches!(
                complete_replacement_transaction_after_repository(
                    &mut journal, &history, &namespace, &namespace_path,
                    supplied, |_| panic!("unobserved native effect must never broadcast"),
                ),
                Err(RecordTransactionCoordinatorError::ExternalNativeEffectPending(pending)) if pending == id
            ));
        }
        assert!(repository.is_empty());
        assert!(!namespace_path.exists());
        for participant in &value.transaction.manifest.participants {
            assert!(history.snapshot(participant.record_id).is_none());
        }
        assert_eq!(journal.get(id).unwrap(), &value);
    }

    #[test]
    fn legacy_signed_journal_has_no_native_binding_and_reopens_unchanged() {
        let value = entry();
        let bytes = serde_json::to_vec(&value).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json.get("external_native_effect").is_none());
        let reopened: RecordTransactionJournalEntry = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(reopened, value);
        reopened.validate().unwrap();
    }

    #[test]
    fn signed_external_intent_rejects_different_native_operation_and_empty_owner() {
        let mut value = external_entry();
        value.external_native_effect.as_mut().unwrap().operation_id = Uuid::new_v4();
        assert!(matches!(
            value.validate(),
            Err(RecordTransactionJournalError::InvalidExternalNativeEffect)
        ));
        let mut value = external_entry();
        value.external_native_effect.as_mut().unwrap().source_owner = [0; 32];
        assert!(matches!(
            value.validate(),
            Err(RecordTransactionJournalError::InvalidExternalNativeEffect)
        ));
    }
}
