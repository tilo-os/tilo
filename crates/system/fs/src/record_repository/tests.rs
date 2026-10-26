use collab_types::generate_keypair;
use fs_protocol::{
    CONTENT_REVISION_VERSION, ConflictMergeCodecV1, ContentHash, ContentRevisionV1,
    FILE_BINDING_VERSION, FILE_MUTATION_TRANSACTION_VERSION, FileBindingV1, FileMetadataV1,
    FileMutationOperation, FileMutationParticipant, FileMutationParticipantRole,
    FileMutationTransaction, FileMutationTransactionManifest, FileNodeKind, FileProvenance,
    FileVersionId, FileVersionV1, IndexStatus,
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

use crate::record_replication::test_authorizers::{AllowActor, AllowActorOn};

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

fn replacement_transaction(fixture: &Fixture, replaced: FileRecordId) -> FileMutationTransaction {
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
                actor: fixture.actor,
                observed_at_ms: 8,
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
        participant(fixture.key.record_id, FileMutationParticipantRole::Source),
        participant(replaced, FileMutationParticipantRole::Replaced),
    ];
    participants.sort_by_key(|participant| participant.record_id);
    FileMutationTransaction::signed(
        FileMutationTransactionManifest {
            schema_version: FILE_MUTATION_TRANSACTION_VERSION,
            transaction_id: Uuid::new_v4(),
            workspace_id: fixture.key.workspace_id,
            actor: fixture.actor,
            timestamp_ms: 8,
            operation: FileMutationOperation::ReplacementRename {
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

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "exercise two actual signed journal commits and durable reopen without replacing document identity"
)]
fn atomic_save_two_commits_keep_document_record_and_exact_version_parents() {
    let fixture = Fixture::new();
    let parent = FileRecordId::new();
    let document = fixture.key.record_id;
    let temp1 = FileRecordId::new();
    let temp2 = FileRecordId::new();
    let initial = FileVersionId::new();
    let auth = AllowActorOn {
        actor: fixture.actor,
        records: [document, temp1, temp2].into_iter().collect(),
    };
    let repository = RecordRepository::open(&fixture.root).unwrap();
    let seed = |record, version, name: &str| {
        let baseline = FileVersionV1 {
            id: version,
            record_id: record,
            parents: Vec::new(),
            actor: fixture.actor,
            timestamp_ms: 1,
            content_hash: None,
            metadata_hash: ContentHash([1; 32]),
            provenance: FileProvenance::Create,
        };
        let binding = FileBindingV1 {
            schema_version: FILE_BINDING_VERSION,
            record_id: record,
            parent: Some(parent),
            name: name.into(),
            kind: FileNodeKind::File,
            tombstoned: false,
            actor: fixture.actor,
            observed_at_ms: 1,
        };
        let revision = fs_protocol::ContentRevisionV1 {
            schema_version: CONTENT_REVISION_VERSION,
            record_id: record,
            version_id: version,
            content_hash: None,
            byte_len: 1,
            metadata_hash: baseline.metadata_hash,
        };
        repository
            .apply_batch(
                RecordRepositoryKey {
                    workspace_id: fixture.key.workspace_id,
                    record_id: record,
                },
                &[
                    RecordEnvelopeV1::signed(
                        record,
                        fixture.actor,
                        1,
                        RecordOperationV1::SetBinding(binding),
                        &fixture.device_key,
                    )
                    .unwrap(),
                    RecordEnvelopeV1::signed(
                        record,
                        fixture.actor,
                        2,
                        RecordOperationV1::AppendVersion(baseline),
                        &fixture.device_key,
                    )
                    .unwrap(),
                    RecordEnvelopeV1::signed(
                        record,
                        fixture.actor,
                        3,
                        RecordOperationV1::SetContentRevision(revision),
                        &fixture.device_key,
                    )
                    .unwrap(),
                ],
                &auth,
            )
            .unwrap();
    };
    seed(document, initial, "report.txt");
    let mut prior = initial;
    let mut committed = Vec::new();
    let mut journal = crate::record_transaction::RecordTransactionJournal::open(
        fixture.dir.path().join("atomic-journal"),
    )
    .unwrap();
    for (index, temporary) in [temp1, temp2].into_iter().enumerate() {
        let temp_version = FileVersionId::new();
        seed(temporary, temp_version, "save-temp");
        let at = u64::try_from(index + 2).unwrap();
        let make = |record, role, expected, name: &str| {
            let version = FileVersionV1 {
                id: FileVersionId::new(),
                record_id: record,
                parents: vec![expected],
                actor: fixture.actor,
                timestamp_ms: at,
                content_hash: None,
                metadata_hash: ContentHash([u8::try_from(at).unwrap(); 32]),
                provenance: if role == FileMutationParticipantRole::AtomicSaveDocument {
                    FileProvenance::Edit
                } else {
                    FileProvenance::Delete
                },
            };
            let mut p = FileMutationParticipant {
                record_id: record,
                role,
                expected_version: expected,
                proposed_binding: FileBindingV1 {
                    schema_version: FILE_BINDING_VERSION,
                    record_id: record,
                    parent: Some(parent),
                    name: name.into(),
                    kind: FileNodeKind::File,
                    tombstoned: role == FileMutationParticipantRole::AtomicSaveTemporary,
                    actor: fixture.actor,
                    observed_at_ms: at,
                },
                proposed_content_revision: Some(fs_protocol::ContentRevisionV1 {
                    schema_version: CONTENT_REVISION_VERSION,
                    record_id: record,
                    version_id: version.id,
                    content_hash: None,
                    byte_len: at,
                    metadata_hash: version.metadata_hash,
                }),
                proposed_version: version,
                projection_hash: ContentHash([0; 32]),
            };
            p.projection_hash = p.computed_projection_hash().unwrap();
            p
        };
        let mut participants = vec![
            make(
                document,
                FileMutationParticipantRole::AtomicSaveDocument,
                prior,
                "report.txt",
            ),
            make(
                temporary,
                FileMutationParticipantRole::AtomicSaveTemporary,
                temp_version,
                "save-temp",
            ),
        ];
        participants.sort_by_key(|p| p.record_id);
        let transaction = FileMutationTransaction::signed(
            FileMutationTransactionManifest {
                schema_version: fs_protocol::files::FILE_MUTATION_TRANSACTION_VERSION_V4,
                transaction_id: Uuid::new_v4(),
                workspace_id: fixture.key.workspace_id,
                actor: fixture.actor,
                timestamp_ms: at,
                operation: FileMutationOperation::AtomicSave {
                    parent_record: parent,
                    document_name: "report.txt".into(),
                    temporary_name: "save-temp".into(),
                    document_record: document,
                    temporary_record: temporary,
                    prior_document_version: prior,
                },
                participants,
            },
            &fixture.device_key,
        )
        .unwrap();
        let batches = transaction
            .manifest
            .participants
            .iter()
            .map(|p| {
                let sequence = if p.record_id == document {
                    4 + u64::try_from(index).unwrap() * 2
                } else {
                    4
                };
                crate::record_transaction::RecordTransactionParticipantBatch {
                    record_id: p.record_id,
                    envelopes: vec![
                        RecordEnvelopeV1::signed(
                            p.record_id,
                            fixture.actor,
                            sequence,
                            RecordOperationV1::PrepareTransaction(transaction.clone()),
                            &fixture.device_key,
                        )
                        .unwrap(),
                        RecordEnvelopeV1::signed(
                            p.record_id,
                            fixture.actor,
                            sequence + 1,
                            RecordOperationV1::CommitTransaction {
                                transaction_id: transaction.manifest.transaction_id,
                            },
                            &fixture.device_key,
                        )
                        .unwrap(),
                    ],
                }
            })
            .collect();
        let entry = crate::record_transaction::RecordTransactionJournalEntry {
            schema_version: crate::record_transaction::RECORD_TRANSACTION_JOURNAL_VERSION,
            transaction: transaction.clone(),
            participant_batches: batches,
            phase: crate::record_transaction::RecordTransactionPhase::Prepared,
            namespace_delta: None,
            external_native_effect: None,
        };
        repository
            .apply_transaction(&mut journal, &entry, &auth)
            .unwrap();
        let current = repository
            .snapshot(fixture.key, &fixture.actor, &auth)
            .unwrap();
        let version = current.content_revision.unwrap().version_id;
        assert_eq!(current.binding.as_ref().unwrap().record_id, document);
        assert!(!current.binding.as_ref().unwrap().tombstoned);
        assert_eq!(
            current
                .versions
                .iter()
                .find(|v| v.id == version)
                .unwrap()
                .parents,
            [prior]
        );
        assert!(
            repository
                .snapshot(
                    RecordRepositoryKey {
                        workspace_id: fixture.key.workspace_id,
                        record_id: temporary
                    },
                    &fixture.actor,
                    &auth
                )
                .unwrap()
                .binding
                .unwrap()
                .tombstoned
        );
        committed.push(version);
        prior = version;
    }
    drop(repository);
    let reopened = RecordRepository::open(&fixture.root).unwrap();
    let current = reopened
        .snapshot(fixture.key, &fixture.actor, &auth)
        .unwrap();
    assert_eq!(current.versions.len(), 3);
    assert_eq!(current.content_revision.unwrap().version_id, committed[1]);
    assert_eq!(
        current
            .versions
            .iter()
            .find(|v| v.id == committed[1])
            .unwrap()
            .parents,
        [committed[0]]
    );
}

#[allow(
    clippy::too_many_lines,
    reason = "one fixture builds and signs the source and sibling versions, bindings and conflict manifest for keep-both transactions"
)] // Builds a complete signed transaction fixture.
fn keep_both_transaction(
    fixture: &Fixture,
) -> (
    FileMutationTransaction,
    FileRecordId,
    FileRecordId,
    FileVersionV1,
    FileVersionV1,
    fs_protocol::FileConflictV1,
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
    let conflict = fs_protocol::FileConflictV1 {
        id: Uuid::new_v4(),
        record_id: fixture.key.record_id,
        versions: vec![current.id, selected.id],
        status: fs_protocol::ConflictStatus::Open,
        resolution: None,
    };
    let participant = |record_id, role| {
        let is_source = role == FileMutationParticipantRole::KeepBothSource;
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
        let mut value = FileMutationParticipant {
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
            proposed_content_revision: Some(fs_protocol::ContentRevisionV1 {
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
            FileMutationParticipantRole::KeepBothSource,
        ),
        participant(sibling, FileMutationParticipantRole::KeepBothSibling),
    ];
    participants.sort_by_key(|value| value.record_id);
    let transaction = FileMutationTransaction::signed(
        FileMutationTransactionManifest {
            schema_version: fs_protocol::files::FILE_MUTATION_TRANSACTION_VERSION_V2,
            transaction_id: Uuid::new_v4(),
            workspace_id: fixture.key.workspace_id,
            actor: fixture.actor,
            timestamp_ms: 3,
            operation: FileMutationOperation::KeepBothContentConflict {
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
    FileMutationTransaction,
    FileRecordId,
    FileVersionV1,
    FileVersionV1,
    FileVersionV1,
    fs_protocol::FileConflictV1,
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
    let conflict = fs_protocol::FileConflictV1 {
        id: Uuid::new_v4(),
        record_id: fixture.key.record_id,
        versions: vec![current.id, other.id],
        status: fs_protocol::ConflictStatus::Open,
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
    let mut participant = FileMutationParticipant {
        record_id: fixture.key.record_id,
        role: FileMutationParticipantRole::MergeSource,
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
        proposed_content_revision: Some(fs_protocol::ContentRevisionV1 {
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
    let transaction = FileMutationTransaction::signed(
        FileMutationTransactionManifest {
            schema_version: fs_protocol::files::FILE_MUTATION_TRANSACTION_VERSION_V2,
            transaction_id: Uuid::new_v4(),
            workspace_id: fixture.key.workspace_id,
            actor: fixture.actor,
            timestamp_ms: 4,
            operation: FileMutationOperation::MergeContentConflict {
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
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn signed_transactions_merges_and_batches_project_whole_and_survive_restart() {
    {
        {
            let fixture = Fixture::new();
            let repository = RecordRepository::open(&fixture.root).unwrap();
            let (transaction, parent, base, current, other, conflict) =
                merge_transaction(&fixture, ConflictMergeCodecV1::TiloDocAutomergeV1);
            let proposed = transaction.manifest.participants[0].clone();
            let binding = FileBindingV1 {
                schema_version: FILE_BINDING_VERSION,
                record_id: fixture.key.record_id,
                parent: Some(parent),
                name: "source.txt".into(),
                kind: FileNodeKind::File,
                tombstoned: false,
                actor: fixture.actor,
                observed_at_ms: 1,
            };
            let current_revision = fs_protocol::ContentRevisionV1 {
                schema_version: CONTENT_REVISION_VERSION,
                record_id: fixture.key.record_id,
                version_id: current.id,
                content_hash: current.content_hash,
                byte_len: 1,
                metadata_hash: current.metadata_hash,
            };
            let operations = [
                RecordOperationV1::SetBinding(binding.clone()),
                RecordOperationV1::AppendVersion(base),
                RecordOperationV1::AppendVersion(current),
                RecordOperationV1::AppendVersion(other),
                RecordOperationV1::SetContentRevision(current_revision),
                RecordOperationV1::AppendConflict(conflict),
                RecordOperationV1::PrepareTransaction(transaction.clone()),
                RecordOperationV1::CommitTransaction {
                    transaction_id: transaction.manifest.transaction_id,
                },
            ];
            for (index, operation) in operations.into_iter().enumerate() {
                repository
                    .apply(
                        fixture.key,
                        &RecordEnvelopeV1::signed(
                            fixture.key.record_id,
                            fixture.actor,
                            index as u64 + 1,
                            operation,
                            &fixture.device_key,
                        )
                        .unwrap(),
                        &fixture.auth(true, true),
                    )
                    .unwrap();
            }
            assert_eq!(
                repository
                    .snapshot(fixture.key, &fixture.actor, &fixture.auth(true, true))
                    .unwrap()
                    .content_revision,
                proposed.proposed_content_revision
            );
            let later = FileVersionV1 {
                id: FileVersionId::new(),
                parents: vec![proposed.proposed_version.id],
                timestamp_ms: 5,
                provenance: FileProvenance::Edit,
                ..proposed.proposed_version
            };
            let later_revision = fs_protocol::ContentRevisionV1 {
                version_id: later.id,
                ..proposed.proposed_content_revision.unwrap()
            };
            let later_binding = FileBindingV1 {
                name: "later.txt".into(),
                observed_at_ms: 5,
                ..binding
            };
            for (sequence, operation) in [
                (9, RecordOperationV1::AppendVersion(later)),
                (10, RecordOperationV1::SetContentRevision(later_revision)),
                (11, RecordOperationV1::SetBinding(later_binding.clone())),
            ] {
                repository
                    .apply(
                        fixture.key,
                        &RecordEnvelopeV1::signed(
                            fixture.key.record_id,
                            fixture.actor,
                            sequence,
                            operation,
                            &fixture.device_key,
                        )
                        .unwrap(),
                        &fixture.auth(true, true),
                    )
                    .unwrap();
            }
            drop(repository);
            let repository = RecordRepository::open(&fixture.root).unwrap();
            let snapshot = repository
                .snapshot(fixture.key, &fixture.actor, &fixture.auth(true, true))
                .unwrap();
            assert_eq!(snapshot.content_revision, Some(later_revision));
            assert_eq!(snapshot.binding, Some(later_binding));
        }
        {
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
        {
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
        {
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
            assert_eq!(repo.logged_operations().unwrap(), 0);
        }
    }
    {
        for codec in [
            ConflictMergeCodecV1::PlainUtf8ThreeWayV1,
            ConflictMergeCodecV1::TiloDocAutomergeV1,
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
                fs_protocol::ConflictStrategy::Merge
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
    {
        for source_first in [true, false] {
            let fixture = Fixture::new();
            let (transaction, parent, sibling, current, selected, conflict) =
                keep_both_transaction(&fixture);
            let repository = RecordRepository::open(&fixture.root).unwrap();
            let auth = AllowActorOn {
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
                            RecordOperationV1::SetContentRevision(fs_protocol::ContentRevisionV1 {
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
    {
        let fixture = Fixture::new();
        let replaced = FileRecordId::new();
        let transaction = replacement_transaction(&fixture, replaced);
        let repository = RecordRepository::open(&fixture.root).unwrap();
        let records = [fixture.key.record_id, replaced]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let auth = AllowActorOn {
            actor: fixture.actor,
            records,
        };
        let fs_protocol::FileMutationOperation::ReplacementRename {
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
                            RecordOperationV1::SetContentRevision(fs_protocol::ContentRevisionV1 {
                                schema_version: CONTENT_REVISION_VERSION,
                                record_id: participant.record_id,
                                version_id: baseline.id,
                                content_hash: None,
                                byte_len: 0,
                                metadata_hash: baseline.metadata_hash,
                            }),
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
            external_native_effect: None,
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
}

#[test]
fn restart_preserves_snapshot_and_replay_watermark() {
    {
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
}

#[test]
fn invalid_or_unauthorized_operations_fail_closed_without_publishing() {
    {
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
        assert_eq!(repo.logged_operations().unwrap(), 0);
    }
    {
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
    {
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
    }
    {
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
    }
}

/// Two records' worth of one actor's envelopes, as a publisher signs
/// them within one changeset.
fn two_record_batches(fixture: &Fixture) -> Vec<(RecordRepositoryKey, Vec<RecordEnvelopeV1>)> {
    let second = Fixture {
        dir: tempfile::tempdir().unwrap(),
        root: fixture.root.clone(),
        key: RecordRepositoryKey {
            workspace_id: fixture.key.workspace_id,
            record_id: FileRecordId::new(),
        },
        device_key: fixture.device_key.clone(),
        actor: fixture.actor,
    };
    vec![
        (
            fixture.key,
            vec![fixture.metadata_envelope(1), fixture.metadata_envelope(2)],
        ),
        (second.key, vec![second.metadata_envelope(1)]),
    ]
}

#[test]
fn an_uncommitted_changeset_is_never_served_and_is_gone_after_a_crash() {
    let fixture = Fixture::new();
    let batches = two_record_batches(&fixture);
    let auth = AllowActor(fixture.actor);
    let repo = RecordRepository::open(&fixture.root).unwrap();
    repo.begin_changeset(fixture.key.workspace_id, fixture.actor)
        .unwrap();
    for (key, envelopes) in &batches {
        repo.apply_batch(*key, envelopes, &auth).unwrap();
    }
    assert_eq!(repo.highest_sequence(fixture.key, &fixture.actor), Some(2));
    // ... peers see none of it.
    assert!(
        repo.stream_cursors_in(fixture.key.workspace_id, None, None, None)
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        repo.envelopes_after(fixture.key, &fixture.actor, 0, 8),
        Err(RecordRepositoryError::RecordNotFound)
    ));
    assert!(
        repo.pull_units(
            fixture.key.workspace_id,
            &[(fixture.key.record_id, fixture.actor, 0)]
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(repo.logged_operations().unwrap(), 0);
    // A crash before the commit: nothing of the changeset survives.
    drop(repo);
    let reopened = RecordRepository::open(&fixture.root).unwrap();
    assert!(reopened.is_empty());
}

#[test]
fn a_committed_changeset_is_durable_whole_and_served_as_one_unit() {
    let fixture = Fixture::new();
    let batches = two_record_batches(&fixture);
    let auth = AllowActor(fixture.actor);
    let repo = RecordRepository::open(&fixture.root).unwrap();
    repo.begin_changeset(fixture.key.workspace_id, fixture.actor)
        .unwrap();
    for (key, envelopes) in &batches {
        repo.apply_batch(*key, envelopes, &auth).unwrap();
    }
    let signed = repo
        .commit_changeset(&fixture.device_key, 42)
        .unwrap()
        .unwrap();
    assert_eq!(signed.header.sequence, 1);
    assert_eq!(signed.leaves.len(), 2);
    signed.verify().unwrap();
    assert_eq!(repo.logged_operations().unwrap(), 3);
    repo.begin_changeset(fixture.key.workspace_id, fixture.actor)
        .unwrap();
    assert!(
        repo.commit_changeset(&fixture.device_key, 43)
            .unwrap()
            .is_none()
    );
    drop(repo);

    let reopened = RecordRepository::open(&fixture.root).unwrap();
    assert_eq!(reopened.len(), 2);
    let wanted: Vec<_> = batches
        .iter()
        .map(|(key, _)| (key.record_id, fixture.actor, 0))
        .collect();
    let units = reopened
        .pull_units(fixture.key.workspace_id, &wanted)
        .unwrap();
    assert_eq!(units.len(), 1, "both records come as the one changeset");
    let PullUnit::Changeset(served) = &units[0] else {
        panic!("expected the changeset");
    };
    assert_eq!(**served, *signed);
    reopened
        .begin_changeset(fixture.key.workspace_id, fixture.actor)
        .unwrap();
    reopened
        .apply(fixture.key, &fixture.metadata_envelope(3), &auth)
        .unwrap();
    let next = reopened
        .commit_changeset(&fixture.device_key, 44)
        .unwrap()
        .unwrap();
    assert_eq!(next.header.sequence, 2);
    assert_eq!(next.leaves[0].first_sequence, 3);
}

#[test]
fn a_received_changeset_applies_whole_or_not_at_all() {
    let author = Fixture::new();
    let batches = two_record_batches(&author);
    let auth = AllowActor(author.actor);
    let repo = RecordRepository::open(&author.root).unwrap();
    repo.begin_changeset(author.key.workspace_id, author.actor)
        .unwrap();
    for (key, envelopes) in &batches {
        repo.apply_batch(*key, envelopes, &auth).unwrap();
    }
    let signed = repo
        .commit_changeset(&author.device_key, 1)
        .unwrap()
        .unwrap();
    let all: Vec<RecordEnvelopeV1> = batches
        .iter()
        .flat_map(|(_, envelopes)| envelopes.iter().cloned())
        .collect();
    let workspace = author.key.workspace_id;

    let peer_dir = tempfile::tempdir().unwrap();
    let peer = RecordRepository::open(peer_dir.path().join("records")).unwrap();
    // Missing one envelope: nothing is applied.
    assert!(matches!(
        peer.apply_changeset(workspace, &signed, &all[..2], &auth),
        Err(RecordRepositoryError::ChangesetMismatch)
    ));
    // Envelopes other than the signed ones: nothing is applied.
    let mut swapped = all.clone();
    swapped[2] = author.metadata_envelope(3);
    assert!(
        peer.apply_changeset(workspace, &signed, &swapped, &auth)
            .is_err()
    );
    let mut forged = (*signed).clone();
    forged.leaves[0].digest[0] ^= 1;
    assert!(
        peer.apply_changeset(workspace, &forged, &all, &auth)
            .is_err()
    );
    assert!(peer.is_empty());
    assert_eq!(peer.logged_operations().unwrap(), 0);

    let applied = peer
        .apply_changeset(workspace, &signed, &all, &auth)
        .unwrap();
    let ChangesetApplied::Applied(mut keys) = applied else {
        panic!("expected the changeset to apply");
    };
    keys.sort();
    let mut expected: Vec<_> = batches.iter().map(|(key, _)| *key).collect();
    expected.sort();
    assert_eq!(keys, expected);
    assert_eq!(peer.logged_operations().unwrap(), 3);
    assert_eq!(
        peer.apply_changeset(workspace, &signed, &all, &auth)
            .unwrap(),
        ChangesetApplied::AlreadyPresent
    );
    // It relays: the peer serves the same changeset onward.
    let wanted: Vec<_> = expected
        .iter()
        .map(|key| (key.record_id, author.actor, 0))
        .collect();
    let units = peer.pull_units(workspace, &wanted).unwrap();
    assert!(matches!(&units[..], [PullUnit::Changeset(c)] if **c == *signed));
    drop(peer);
    let reopened = RecordRepository::open(peer_dir.path().join("records")).unwrap();
    assert_eq!(reopened.len(), 2);
}

/// A peer that serves another workspace's changeset, complete and
/// validly signed by its author, gets nothing applied here.
#[test]
fn a_validly_signed_changeset_of_another_workspace_is_refused() {
    let author = Fixture::new();
    let batches = two_record_batches(&author);
    let auth = AllowActor(author.actor);
    let repo = RecordRepository::open(&author.root).unwrap();
    repo.begin_changeset(author.key.workspace_id, author.actor)
        .unwrap();
    for (key, envelopes) in &batches {
        repo.apply_batch(*key, envelopes, &auth).unwrap();
    }
    let signed = repo
        .commit_changeset(&author.device_key, 1)
        .unwrap()
        .unwrap();
    signed.verify().unwrap();
    let all: Vec<RecordEnvelopeV1> = batches
        .iter()
        .flat_map(|(_, envelopes)| envelopes.iter().cloned())
        .collect();

    let peer_dir = tempfile::tempdir().unwrap();
    let peer = RecordRepository::open(peer_dir.path().join("records")).unwrap();
    let this_workspace = Uuid::new_v4();
    assert_ne!(this_workspace, author.key.workspace_id);
    assert!(matches!(
        peer.apply_changeset(this_workspace, &signed, &all, &auth),
        Err(RecordRepositoryError::TransactionWorkspaceMismatch)
    ));
    assert!(peer.is_empty());
    assert_eq!(peer.logged_operations().unwrap(), 0);
}

#[test]
fn a_write_outside_the_changeset_takes_its_pending_envelopes_along() {
    let fixture = Fixture::new();
    let auth = fixture.auth(true, true);
    let repo = std::sync::Arc::new(RecordRepository::open(&fixture.root).unwrap());
    repo.begin_changeset(fixture.key.workspace_id, fixture.actor)
        .unwrap();
    repo.apply(fixture.key, &fixture.metadata_envelope(1), &auth)
        .unwrap();
    // Another thread writes the next envelope of the same stream: it
    // cannot be durable without the one before it.
    let other = std::sync::Arc::clone(&repo);
    let next = fixture.metadata_envelope(2);
    let key = fixture.key;
    let actor = fixture.actor;
    let record = fixture.key.record_id;
    std::thread::spawn(move || {
        other
            .apply(
                key,
                &next,
                &TestAuthorizer {
                    actor,
                    record,
                    read: true,
                    write: true,
                },
            )
            .unwrap();
    })
    .join()
    .unwrap();
    assert_eq!(repo.logged_operations().unwrap(), 2);
    assert!(
        repo.commit_changeset(&fixture.device_key, 1)
            .unwrap()
            .is_none()
    );
    drop(repo);
    let reopened = RecordRepository::open(&fixture.root).unwrap();
    assert_eq!(
        reopened.highest_sequence(fixture.key, &fixture.actor),
        Some(2)
    );
}
