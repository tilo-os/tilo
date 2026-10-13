//! Recovery coordinator for one signed replacement-rename transaction.

use std::path::Path;
use std::sync::{Arc, Mutex};

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::OnceLock;

use thiserror::Error;

use crate::history::{FileHistoryStore, HistoryError};
use crate::namespace::{Namespace, TransactionProjectionError};
use crate::record_replication::RecordAuthorizer;
use crate::record_repository::{RecordRepository, RecordRepositoryError};
use crate::record_transaction::{
    RecordTransactionJournal, RecordTransactionJournalEntry, RecordTransactionJournalError,
    RecordTransactionPhase,
};

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoordinatorFaultPoint {
    AfterHistoryAppend,
    AfterNamespaceDeltaStaged,
}

#[cfg(test)]
static COORDINATOR_FAULTS: OnceLock<Mutex<HashMap<uuid::Uuid, CoordinatorFaultPoint>>> =
    OnceLock::new();

#[cfg(test)]
pub(crate) fn arm_coordinator_fault(id: uuid::Uuid, point: CoordinatorFaultPoint) {
    COORDINATOR_FAULTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(id, point);
}

#[cfg(test)]
fn fail_if_armed(
    id: uuid::Uuid,
    point: CoordinatorFaultPoint,
) -> Result<(), RecordTransactionCoordinatorError> {
    let mut faults = COORDINATOR_FAULTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    let result = if faults.get(&id) == Some(&point) {
        faults.remove(&id);
        Err(RecordTransactionCoordinatorError::InjectedFailure(point))
    } else {
        Ok(())
    };
    drop(faults);
    result
}

/// Resume every durable phase for one exact journal intent. Every operation is
/// idempotent, so a crash can re-enter with the entry in any persisted phase.
///
/// # Errors
///
/// Returns an error when any repository, history, namespace, journal, or
/// broadcast phase fails.
pub fn complete_replacement_transaction<A, F>(
    repository: &RecordRepository,
    journal: &mut RecordTransactionJournal,
    history: &FileHistoryStore,
    namespace: &Arc<Mutex<Namespace>>,
    namespace_path: &Path,
    entry: &RecordTransactionJournalEntry,
    authorizer: &A,
    broadcast: F,
) -> Result<(), RecordTransactionCoordinatorError>
where
    A: RecordAuthorizer + ?Sized,
    F: FnMut(Vec<u8>) -> Result<(), std::io::Error>,
{
    if matches!(
        entry.transaction.manifest.operation,
        fs_protocol::FileMutationOperation::RestoreContent { .. }
    ) {
        if journal
            .get(entry.transaction_id())
            .is_some_and(|value| value.phase == RecordTransactionPhase::BroadcastCommitted)
        {
            return Ok(());
        }
        repository.apply_transaction(journal, entry, authorizer)?;
        return complete_checked_restore_after_repository(
            repository,
            journal,
            history,
            namespace,
            namespace_path,
            entry,
            broadcast,
        );
    }
    repository.apply_transaction(journal, entry, authorizer)?;

    complete_replacement_transaction_after_repository(
        journal,
        history,
        namespace,
        namespace_path,
        entry,
        broadcast,
    )
}

/// Recover a bounded record delta against current converged repository state.
/// Later valid binding/content changes survive; the receipt remains the exact
/// admitted manifest rather than whichever revision now wins projection.
#[expect(
    clippy::significant_drop_tightening,
    reason = "namespace, history and repository stay leased until the namespace commit is journaled"
)]
pub(crate) fn complete_checked_restore_after_repository<F>(
    repository: &RecordRepository,
    journal: &mut RecordTransactionJournal,
    history: &FileHistoryStore,
    namespace: &Arc<Mutex<Namespace>>,
    namespace_path: &Path,
    entry: &RecordTransactionJournalEntry,
    mut broadcast: F,
) -> Result<(), RecordTransactionCoordinatorError>
where
    F: FnMut(Vec<u8>) -> Result<(), std::io::Error>,
{
    let id = entry.transaction_id();
    let participant = entry
        .transaction
        .manifest
        .participants
        .first()
        .ok_or(RecordTransactionCoordinatorError::InvalidKeepBothTransaction)?;
    if journal
        .get(id)
        .is_some_and(|value| value.phase == RecordTransactionPhase::Prepared)
    {
        repository.complete_admitted_restore_participation(journal, entry)?;
    }
    if journal
        .get(id)
        .is_some_and(|value| value.phase <= RecordTransactionPhase::HistoryCommitted)
    {
        let mut namespace = namespace
            .lock()
            .map_err(|_| RecordTransactionCoordinatorError::NamespacePoisoned)?;
        let mut history = history.lease()?;
        let repository = repository.lease()?;
        let snapshot = repository.snapshot(crate::record_repository::RecordRepositoryKey {
            workspace_id: entry.transaction.manifest.workspace_id,
            record_id: participant.record_id,
        })?;
        history.append_batch(std::slice::from_ref(&participant.proposed_version))?;
        if journal
            .get(id)
            .is_some_and(|value| value.phase == RecordTransactionPhase::ParticipantRecordsCommitted)
        {
            journal.advance(id, RecordTransactionPhase::HistoryCommitted, None)?;
        }
        let mut candidate = namespace.clone();
        let _ = candidate.save_incremental();
        let staged = journal
            .get(id)
            .and_then(|value| value.namespace_delta.clone());
        let projection_actor = candidate.begin_restore_projection(
            entry.transaction.manifest.workspace_id,
            participant.record_id,
            id,
        );
        if let Some(delta) = &staged {
            candidate
                .apply_incremental(delta)
                .map_err(|_| RecordTransactionCoordinatorError::NamespaceDeltaMismatch)?;
        }
        let binding = snapshot
            .binding
            .as_ref()
            .ok_or(RecordTransactionCoordinatorError::MissingNamespaceDelta)?;
        candidate
            .apply_signed_binding(binding)
            .map_err(|_| RecordTransactionCoordinatorError::NamespaceDeltaMismatch)?;
        if !binding.tombstoned {
            let revision = snapshot
                .content_revision
                .ok_or(RecordTransactionCoordinatorError::MissingNamespaceDelta)?;
            let ino = candidate
                .ensure_ino_for_record_id(participant.record_id)
                .ok_or(RecordTransactionCoordinatorError::MissingNamespaceDelta)?;
            if candidate.content_revision_for(ino) != Some(revision) {
                let timestamp = snapshot
                    .versions
                    .iter()
                    .find(|v| v.id == revision.version_id)
                    .map_or(0, |v| v.timestamp_ms);
                if !candidate.set_content_revision(ino, &revision, timestamp) {
                    return Err(RecordTransactionCoordinatorError::MissingNamespaceDelta);
                }
            }
        }
        let delta = staged.unwrap_or_else(|| candidate.save_incremental());
        candidate.end_restore_projection(projection_actor);
        journal.stage_namespace_delta(id, delta.clone())?;
        crate::durable_backup::write(namespace_path, &candidate.save(), |bytes| {
            Namespace::from_bytes("checked-restore-recovery", bytes).is_ok()
        })?;
        *namespace = candidate;
        journal.advance(id, RecordTransactionPhase::NamespaceCommitted, Some(delta))?;
    }
    if journal
        .get(id)
        .is_some_and(|value| value.phase == RecordTransactionPhase::NamespaceCommitted)
    {
        let delta = journal
            .get(id)
            .and_then(|value| value.namespace_delta.clone())
            .ok_or(RecordTransactionCoordinatorError::MissingNamespaceDelta)?;
        broadcast(delta.clone())?;
        journal.advance(id, RecordTransactionPhase::BroadcastCommitted, Some(delta))?;
    }
    Ok(())
}

/// Resume the durable projection phases after the repository has already
/// accepted every exact participant, as happens for independently delivered
/// remote record streams.
#[allow(
    clippy::too_many_lines,
    reason = "durable history, staged namespace persistence and broadcast phases stay together so journal replay preserves their order"
)]
pub(crate) fn complete_replacement_transaction_after_repository<F>(
    journal: &mut RecordTransactionJournal,
    history: &FileHistoryStore,
    namespace: &Arc<Mutex<Namespace>>,
    namespace_path: &Path,
    entry: &RecordTransactionJournalEntry,
    mut broadcast: F,
) -> Result<(), RecordTransactionCoordinatorError>
where
    F: FnMut(Vec<u8>) -> Result<(), std::io::Error>,
{
    let id = entry.transaction_id();
    if journal
        .get(id)
        .is_some_and(|value| value.phase == RecordTransactionPhase::Prepared)
    {
        journal.advance(
            id,
            RecordTransactionPhase::ParticipantRecordsCommitted,
            None,
        )?;
    }

    if journal
        .get(id)
        .is_some_and(|value| value.phase == RecordTransactionPhase::ParticipantRecordsCommitted)
    {
        let versions = entry
            .transaction
            .manifest
            .participants
            .iter()
            .map(|participant| participant.proposed_version.clone())
            .collect::<Vec<_>>();
        history.append_batch(&versions)?;
        #[cfg(test)]
        fail_if_armed(id, CoordinatorFaultPoint::AfterHistoryAppend)?;
        if let Some((source_record, conflict_id, strategy, role)) =
            match &entry.transaction.manifest.operation {
                fs_protocol::FileMutationOperation::KeepBothContentConflict {
                    source_record,
                    conflict_id,
                    ..
                } => Some((
                    *source_record,
                    *conflict_id,
                    fs_protocol::ConflictStrategy::KeepBoth,
                    fs_protocol::FileMutationParticipantRole::KeepBothSource,
                )),
                fs_protocol::FileMutationOperation::MergeContentConflict {
                    source_record,
                    conflict_id,
                    ..
                } => Some((
                    *source_record,
                    *conflict_id,
                    fs_protocol::ConflictStrategy::Merge,
                    fs_protocol::FileMutationParticipantRole::MergeSource,
                )),
                fs_protocol::FileMutationOperation::ReplacementRename { .. }
                | fs_protocol::FileMutationOperation::RestoreContent { .. } => None,
            }
        {
            let source = entry
                .transaction
                .manifest
                .participants
                .iter()
                .find(|participant| participant.role == role)
                .ok_or(RecordTransactionCoordinatorError::InvalidKeepBothTransaction)?;
            history.resolve(
                source_record,
                conflict_id,
                fs_protocol::ConflictResolution {
                    strategy,
                    result: source.proposed_version.id,
                    actor: entry.transaction.manifest.actor,
                    timestamp_ms: entry.transaction.manifest.timestamp_ms,
                },
            )?;
        }
        journal.advance(id, RecordTransactionPhase::HistoryCommitted, None)?;
    }

    if journal
        .get(id)
        .is_some_and(|value| value.phase == RecordTransactionPhase::HistoryCommitted)
    {
        let staged = if let Some(delta) = journal
            .get(id)
            .and_then(|value| value.namespace_delta.clone())
        {
            delta
        } else {
            let mut preview = namespace
                .lock()
                .map_err(|_| RecordTransactionCoordinatorError::NamespacePoisoned)?
                .clone();
            let delta = match preview.apply_mutation_transaction(&entry.transaction)? {
                Some(delta) => delta,
                None if matches!(
                    entry.transaction.manifest.operation,
                    fs_protocol::FileMutationOperation::MergeContentConflict { .. }
                ) =>
                {
                    Vec::new()
                }
                None => return Err(RecordTransactionCoordinatorError::MissingNamespaceDelta),
            };
            journal.stage_namespace_delta(id, delta.clone())?;
            #[cfg(test)]
            fail_if_armed(id, CoordinatorFaultPoint::AfterNamespaceDeltaStaged)?;
            delta
        };
        let mut namespace = namespace
            .lock()
            .map_err(|_| RecordTransactionCoordinatorError::NamespacePoisoned)?;
        let mut candidate = namespace.clone();
        if matches!(
            entry.transaction.manifest.operation,
            fs_protocol::FileMutationOperation::KeepBothContentConflict { .. }
        ) {
            candidate.apply_staged_keep_both_delta(&entry.transaction, &staged)?;
        } else if matches!(
            entry.transaction.manifest.operation,
            fs_protocol::FileMutationOperation::MergeContentConflict { .. }
        ) {
            candidate.apply_staged_merge_delta(&entry.transaction, &staged)?;
        } else if let Some(actual) = candidate.apply_mutation_transaction(&entry.transaction)?
            && actual != staged
        {
            return Err(RecordTransactionCoordinatorError::NamespaceDeltaMismatch);
        }
        let full_snapshot = candidate.save();
        crate::durable_backup::write(namespace_path, &full_snapshot, |bytes| {
            Namespace::from_bytes("transaction-backup-validator", bytes).is_ok()
        })?;
        *namespace = candidate;
        drop(namespace);
        journal.advance(id, RecordTransactionPhase::NamespaceCommitted, Some(staged))?;
    }

    if journal
        .get(id)
        .is_some_and(|value| value.phase == RecordTransactionPhase::NamespaceCommitted)
    {
        let delta = journal
            .get(id)
            .and_then(|value| value.namespace_delta.clone())
            .ok_or(RecordTransactionCoordinatorError::MissingNamespaceDelta)?;
        broadcast(delta.clone())?;
        journal.advance(id, RecordTransactionPhase::BroadcastCommitted, Some(delta))?;
    }
    if journal
        .get(id)
        .is_some_and(|value| value.phase == RecordTransactionPhase::BroadcastCommitted)
    {
        journal.remove_completed(id)?;
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum RecordTransactionCoordinatorError {
    #[error(transparent)]
    Repository(#[from] RecordRepositoryError),
    #[error(transparent)]
    Journal(#[from] RecordTransactionJournalError),
    #[error(transparent)]
    History(#[from] HistoryError),
    #[error(transparent)]
    Namespace(#[from] TransactionProjectionError),
    #[error("transaction namespace lock is poisoned")]
    NamespacePoisoned,
    #[error("transaction could not reconstruct its namespace delta")]
    MissingNamespaceDelta,
    #[error("keep-both transaction is missing its source participant")]
    InvalidKeepBothTransaction,
    #[error("transaction namespace delta differs from the staged bytes")]
    NamespaceDeltaMismatch,
    #[error("transaction namespace persistence or broadcast failed: {0}")]
    Io(#[from] std::io::Error),
    #[cfg(test)]
    #[error("transaction failure injected at coordinator checkpoint {0:?}")]
    InjectedFailure(CoordinatorFaultPoint),
}

#[cfg(test)]
mod tests {

    use collab_types::Ed25519PublicKey;
    use ed25519_dalek::SigningKey;
    use fs_protocol::{
        CONTENT_REVISION_VERSION, ContentHash, FILE_BINDING_VERSION,
        FILE_MUTATION_TRANSACTION_VERSION, FileActor, FileBindingV1, FileMutationOperation,
        FileMutationParticipant, FileMutationParticipantRole, FileMutationTransaction,
        FileMutationTransactionManifest, FileNodeKind, FileProvenance, FileVersionId,
        FileVersionV1,
    };
    use getrandom::{SysRng, rand_core::UnwrapErr};
    use uuid::Uuid;

    use super::*;
    use crate::namespace::ROOT_INO;
    use crate::record_replication::{RecordEnvelopeV1, RecordOperationV1};
    use crate::record_repository::RecordRepositoryKey;
    use crate::record_transaction::{
        RECORD_TRANSACTION_JOURNAL_VERSION, RecordTransactionParticipantBatch,
    };

    use crate::record_replication::test_authorizers::AllowActorOn;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the signed replacement fixture follows a failed broadcast through retry and checks exact namespace state without duplicate history"
    )] // Crash recovery through every durable phase.
    fn broadcast_failure_recovers_exact_committed_namespace_without_duplicate_history() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_id = Uuid::new_v4();
        let key = SigningKey::generate(&mut UnwrapErr(SysRng));
        let actor = FileActor {
            person: Ed25519PublicKey([6; 32]),
            device: Ed25519PublicKey(key.verifying_key().to_bytes()),
        };
        let namespace = Arc::new(Mutex::new(Namespace::new("owner").unwrap()));
        let (source_ino, victim_ino, root_record, source_record, victim_record) = {
            let mut namespace = namespace.lock().unwrap();
            let source = namespace
                .create_file(ROOT_INO, "source.txt", None, 0, 1)
                .unwrap();
            let victim = namespace
                .create_file(ROOT_INO, "destination.txt", None, 0, 1)
                .unwrap();
            (
                source,
                victim,
                namespace.record_id_for(ROOT_INO).unwrap(),
                namespace.record_id_for(source).unwrap(),
                namespace.record_id_for(victim).unwrap(),
            )
        };
        let source_expected = FileVersionId::new();
        let victim_expected = FileVersionId::new();
        let baseline = |record_id, version_id| FileVersionV1 {
            id: version_id,
            record_id,
            parents: Vec::new(),
            actor,
            timestamp_ms: 1,
            content_hash: None,
            metadata_hash: ContentHash([1; 32]),
            provenance: FileProvenance::Create,
        };
        let source_baseline = baseline(source_record, source_expected);
        let victim_baseline = baseline(victim_record, victim_expected);
        {
            let mut namespace = namespace.lock().unwrap();
            for (ino, version) in [
                (source_ino, &source_baseline),
                (victim_ino, &victim_baseline),
            ] {
                assert!(namespace.set_content_revision(
                    ino,
                    &fs_protocol::ContentRevisionV1 {
                        schema_version: CONTENT_REVISION_VERSION,
                        record_id: version.record_id,
                        version_id: version.id,
                        content_hash: None,
                        byte_len: 0,
                        metadata_hash: version.metadata_hash,
                    },
                    1,
                ));
            }
            drop(namespace);
        }
        let participant = |record_id, expected_version, role| {
            let version = FileVersionV1 {
                id: FileVersionId::new(),
                record_id,
                parents: vec![expected_version],
                actor,
                timestamp_ms: 2,
                content_hash: None,
                metadata_hash: ContentHash([2; 32]),
                provenance: match role {
                    FileMutationParticipantRole::Source => FileProvenance::Rename,
                    FileMutationParticipantRole::Replaced => FileProvenance::Delete,
                    _ => unreachable!("replacement fixture role"),
                },
            };
            let mut participant = FileMutationParticipant {
                record_id,
                role,
                expected_version,
                proposed_binding: FileBindingV1 {
                    schema_version: FILE_BINDING_VERSION,
                    record_id,
                    parent: Some(root_record),
                    name: "destination.txt".into(),
                    kind: FileNodeKind::File,
                    tombstoned: role == FileMutationParticipantRole::Replaced,
                    actor,
                    observed_at_ms: 2,
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
            participant(
                source_record,
                source_expected,
                FileMutationParticipantRole::Source,
            ),
            participant(
                victim_record,
                victim_expected,
                FileMutationParticipantRole::Replaced,
            ),
        ];
        participants.sort_by_key(|value| value.record_id);
        let transaction = FileMutationTransaction::signed(
            FileMutationTransactionManifest {
                schema_version: FILE_MUTATION_TRANSACTION_VERSION,
                transaction_id: Uuid::new_v4(),
                workspace_id,
                actor,
                timestamp_ms: 2,
                operation: FileMutationOperation::ReplacementRename {
                    source_parent: root_record,
                    source_name: "source.txt".into(),
                    destination_parent: root_record,
                    destination_name: "destination.txt".into(),
                    source_record,
                    replaced_record: victim_record,
                },
                participants,
            },
            &key,
        )
        .unwrap();
        let repository = RecordRepository::open(temp.path().join("records")).unwrap();
        let authorizer = AllowActorOn {
            actor,
            records: [source_record, victim_record].into_iter().collect(),
        };
        let history = FileHistoryStore::open(temp.path().join("history.json")).unwrap();
        history
            .append_batch(&[source_baseline.clone(), victim_baseline.clone()])
            .unwrap();
        for (record_id, name, baseline) in [
            (source_record, "source.txt", source_baseline),
            (victim_record, "destination.txt", victim_baseline),
        ] {
            repository
                .apply_batch(
                    RecordRepositoryKey {
                        workspace_id,
                        record_id,
                    },
                    &[
                        RecordEnvelopeV1::signed(
                            record_id,
                            actor,
                            1,
                            RecordOperationV1::SetBinding(FileBindingV1 {
                                schema_version: FILE_BINDING_VERSION,
                                record_id,
                                parent: Some(root_record),
                                name: name.into(),
                                kind: FileNodeKind::File,
                                tombstoned: false,
                                actor,
                                observed_at_ms: 1,
                            }),
                            &key,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            record_id,
                            actor,
                            2,
                            RecordOperationV1::AppendVersion(baseline.clone()),
                            &key,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            record_id,
                            actor,
                            3,
                            RecordOperationV1::SetContentRevision(fs_protocol::ContentRevisionV1 {
                                schema_version: CONTENT_REVISION_VERSION,
                                record_id,
                                version_id: baseline.id,
                                content_hash: None,
                                byte_len: 0,
                                metadata_hash: baseline.metadata_hash,
                            }),
                            &key,
                        )
                        .unwrap(),
                    ],
                    &authorizer,
                )
                .unwrap();
        }
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
                        4,
                        RecordOperationV1::PrepareTransaction(transaction.clone()),
                        &key,
                    )
                    .unwrap(),
                    RecordEnvelopeV1::signed(
                        participant.record_id,
                        actor,
                        5,
                        RecordOperationV1::CommitTransaction {
                            transaction_id: transaction.manifest.transaction_id,
                        },
                        &key,
                    )
                    .unwrap(),
                ],
            })
            .collect();
        let entry = RecordTransactionJournalEntry {
            schema_version: RECORD_TRANSACTION_JOURNAL_VERSION,
            transaction,
            participant_batches,
            phase: RecordTransactionPhase::Prepared,
            namespace_delta: None,
        };
        let journal_root = temp.path().join("journal");
        let namespace_path = temp.path().join("namespace.bin");
        let mut journal = RecordTransactionJournal::open(&journal_root).unwrap();
        let first = complete_replacement_transaction(
            &repository,
            &mut journal,
            &history,
            &namespace,
            &namespace_path,
            &entry,
            &authorizer,
            |_| Err(std::io::Error::other("offline broadcaster")),
        );
        assert!(matches!(
            first,
            Err(RecordTransactionCoordinatorError::Io(_))
        ));
        assert_eq!(
            journal.get(entry.transaction_id()).unwrap().phase,
            RecordTransactionPhase::NamespaceCommitted
        );
        assert!(namespace_path.exists());
        let source_history_len = history.snapshot(source_record).unwrap().versions.len();

        let mut broadcasts = Vec::new();
        complete_replacement_transaction(
            &repository,
            &mut journal,
            &history,
            &namespace,
            &namespace_path,
            &entry,
            &authorizer,
            |delta| {
                broadcasts.push(delta);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(broadcasts.len(), 1);
        assert!(journal.get(entry.transaction_id()).is_none());
        assert_eq!(
            history.snapshot(source_record).unwrap().versions.len(),
            source_history_len
        );
        let mut namespace = namespace.lock().unwrap();
        assert_eq!(
            namespace.lookup(ROOT_INO, "destination.txt").unwrap().ino.0,
            source_ino
        );
        assert!(namespace.lookup(ROOT_INO, "source.txt").is_none());
        drop(namespace);
    }
}
