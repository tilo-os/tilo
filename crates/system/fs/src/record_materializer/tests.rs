use super::*;

use std::time::UNIX_EPOCH;

use collab_types::Ed25519PublicKey;
use ed25519_dalek::SigningKey;
use fs_protocol::{
    ConflictMergeCodecV1, ConflictResolution, ConflictStatus, ConflictStrategy, ContentHash,
    ContentRevisionV1, FILE_BINDING_VERSION, FILE_MUTATION_TRANSACTION_VERSION, FileActor,
    FileBindingV1, FileConflictV1, FileMetadataV1, FileMutationOperation, FileMutationParticipant,
    FileMutationParticipantRole, FileMutationTransaction, FileMutationTransactionManifest,
    FileNodeKind, FileProvenance, FileVersionId, FileVersionV1, IndexStatus,
};

use crate::namespace::ROOT_INO;
use crate::record_access::RecordAccessSessionId;
use crate::record_replication::test_authorizers::{AllowActor, AllowAll};
use crate::record_repository::RecordRepositoryKey;

struct Fixture {
    workspace: Uuid,
    namespace: Arc<Mutex<Namespace>>,
    history: Arc<FileHistoryStore>,
    materializer: RecordMaterializer,
    record: FileRecordId,
    ino: u64,
    actor: FileActor,
    device: SigningKey,
    temp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let workspace = Uuid::new_v4();
        let mut namespace = Namespace::new("materializer-test").unwrap();
        let ino = namespace
            .create_file(ROOT_INO, "before.txt", None, 0, 1)
            .unwrap();
        let record = namespace.record_id_for(ino).unwrap();
        let namespace = Arc::new(Mutex::new(namespace));
        let temp = tempfile::tempdir().unwrap();
        let history = Arc::new(FileHistoryStore::open(temp.path().join("history.json")).unwrap());
        let device = SigningKey::from_bytes(&[7; 32]);
        let actor = FileActor {
            person: Ed25519PublicKey::from_bytes([8; 32]),
            device: Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
        };
        let materializer = RecordMaterializer::new(
            workspace,
            Arc::clone(&namespace),
            Some(Arc::clone(&history)),
        );
        Self {
            workspace,
            namespace,
            history,
            materializer,
            record,
            ino,
            actor,
            device,
            temp,
        }
    }

    fn binding(&self) -> RecordAccessBinding {
        RecordAccessBinding {
            session_id: RecordAccessSessionId(Uuid::new_v4()),
            grant_id: Uuid::new_v4(),
            workspace_id: self.workspace,
            record_id: self.record,
            person: self.actor.person,
            device: self.actor.device,
            expires_at_ms: u64::MAX,
        }
    }

    fn envelope(&self, sequence: u64, operation: RecordOperationV1) -> RecordEnvelopeV1 {
        RecordEnvelopeV1::signed(self.record, self.actor, sequence, operation, &self.device)
            .unwrap()
    }

    fn snapshot_for(&self, envelope: &RecordEnvelopeV1) -> RecordSnapshot {
        match &envelope.operation {
            RecordOperationV1::SetBinding(binding) => RecordSnapshot {
                record_id: self.record,
                binding: Some(binding.clone()),
                metadata: None,
                content_revision: None,
                versions: vec![],
                conflicts: vec![],
                attributions: vec![],
            },
            RecordOperationV1::SetMetadata(metadata) => RecordSnapshot {
                record_id: self.record,
                binding: None,
                metadata: Some(metadata.clone()),
                content_revision: None,
                versions: vec![],
                conflicts: vec![],
                attributions: vec![],
            },
            RecordOperationV1::AppendVersion(version) => RecordSnapshot {
                record_id: self.record,
                binding: None,
                metadata: None,
                content_revision: None,
                versions: vec![version.clone()],
                conflicts: vec![],
                attributions: vec![],
            },
            RecordOperationV1::AppendConflict(conflict) => RecordSnapshot {
                record_id: self.record,
                binding: None,
                metadata: None,
                content_revision: None,
                versions: vec![],
                conflicts: vec![conflict.clone()],
                attributions: vec![],
            },
            RecordOperationV1::ResolveConflict { .. }
            | RecordOperationV1::PrepareTransaction(_)
            | RecordOperationV1::CommitTransaction { .. }
            | RecordOperationV1::AttributeVersion(_) => RecordSnapshot {
                record_id: self.record,
                binding: None,
                metadata: None,
                content_revision: None,
                versions: vec![],
                conflicts: vec![],
                attributions: vec![],
            },
            RecordOperationV1::SetContentRevision(revision) => RecordSnapshot {
                record_id: self.record,
                binding: None,
                metadata: None,
                content_revision: Some(*revision),
                versions: vec![],
                conflicts: vec![],
                attributions: vec![],
            },
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn materialization_uses_current_signed_state_and_refuses_unknown_records() {
    {
        use crate::record_repository::RecordRepositoryKey;
        let fixture = Fixture::new();
        let repository =
            RecordRepository::open(fixture.temp.path().join("delayed-records")).unwrap();
        let key = RecordRepositoryKey {
            workspace_id: fixture.workspace,
            record_id: fixture.record,
        };
        let old = FileMetadataV1 {
            schema_version: fs_protocol::files::FILE_METADATA_VERSION,
            record_id: fixture.record,
            content_hash: Some(ContentHash([41; 32])),
            byte_len: 41,
            mime: None,
            previews: vec![],
            latest_actor: Some(fixture.actor),
            observed_at_ms: 41,
            index_status: IndexStatus::Pending,
            properties: None,
        };
        let old_envelope = fixture.envelope(1, RecordOperationV1::SetMetadata(old.clone()));
        repository.apply(key, &old_envelope, &AllowAll).unwrap();
        let captured = repository.snapshot(key, &fixture.actor, &AllowAll).unwrap();
        let current = FileMetadataV1 {
            content_hash: Some(ContentHash([42; 32])),
            byte_len: 42,
            observed_at_ms: 42,
            ..old
        };
        let current_envelope = fixture.envelope(2, RecordOperationV1::SetMetadata(current));
        // Deterministically withhold N while the delayed sink hands off its
        // captured snapshot. The repository can accept a later operation while
        // projection waits; releasing N must select that current projection.
        let (entered, wait) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let namespace = fixture.namespace.lock().unwrap();
            let worker = scope.spawn(|| {
                entered.send(()).unwrap();
                fixture
                    .materializer
                    .record_mutation(&repository, fixture.binding(), &old_envelope, &captured)
                    .unwrap();
            });
            wait.recv_timeout(tilo_test_env::hang_bound()).unwrap();
            repository.apply(key, &current_envelope, &AllowAll).unwrap();
            drop(namespace);
            worker.join().unwrap();
        });
        let namespace = fixture.namespace.lock().unwrap();
        assert_eq!(
            namespace.hash_for(fixture.ino).unwrap().as_bytes(),
            &[42; 32]
        );
        assert_eq!(namespace.getattr(fixture.ino).unwrap().size, 42);
        drop(namespace);
    }
    {
        let fixture = Fixture::new();
        assert!(fixture.namespace.lock().unwrap().rename(
            ROOT_INO,
            "before.txt",
            ROOT_INO,
            "after.txt",
            2,
        ));
        let metadata = FileMetadataV1 {
            schema_version: fs_protocol::files::FILE_METADATA_VERSION,
            record_id: fixture.record,
            content_hash: Some(ContentHash([4; 32])),
            byte_len: 99,
            mime: Some("text/plain".into()),
            previews: vec![],
            latest_actor: Some(fixture.actor),
            observed_at_ms: 1234,
            index_status: IndexStatus::Pending,
            properties: None,
        };
        let converged = metadata.clone();
        let envelope = fixture.envelope(1, RecordOperationV1::SetMetadata(metadata));
        fixture
            .materializer
            .apply(
                fixture.binding(),
                &envelope,
                &fixture.snapshot_for(&envelope),
            )
            .unwrap();

        let mut namespace = fixture.namespace.lock().unwrap();
        assert!(namespace.lookup(ROOT_INO, "before.txt").is_none());
        assert_eq!(
            namespace.lookup(ROOT_INO, "after.txt").unwrap().ino.0,
            fixture.ino
        );
        assert_eq!(
            namespace.hash_for(fixture.ino).unwrap().as_bytes(),
            &[4; 32]
        );
        let attr = namespace.getattr(fixture.ino).unwrap();
        assert_eq!(attr.size, 99);
        assert_eq!(
            attr.mtime.duration_since(UNIX_EPOCH).unwrap().as_millis(),
            1234
        );
        drop(namespace);

        // A later-arriving envelope can lose the replica's LWW comparison.
        // The live namespace must follow the converged snapshot, not arrival
        // order.
        let losing = fixture.envelope(
            2,
            RecordOperationV1::SetMetadata(FileMetadataV1 {
                content_hash: Some(ContentHash([9; 32])),
                byte_len: 7,
                observed_at_ms: 10,
                ..converged.clone()
            }),
        );
        fixture
            .materializer
            .apply(
                fixture.binding(),
                &losing,
                &RecordSnapshot {
                    record_id: fixture.record,
                    binding: None,
                    metadata: Some(converged),
                    content_revision: None,
                    versions: vec![],
                    conflicts: vec![],
                    attributions: vec![],
                },
            )
            .unwrap();
        let namespace = fixture.namespace.lock().unwrap();
        assert_eq!(
            namespace.hash_for(fixture.ino).unwrap().as_bytes(),
            &[4; 32]
        );
        assert_eq!(namespace.getattr(fixture.ino).unwrap().size, 99);
        drop(namespace);
    }
    {
        let fixture = Fixture::new();
        let parent = fixture
            .namespace
            .lock()
            .unwrap()
            .record_id_for(ROOT_INO)
            .unwrap();
        let binding = FileBindingV1 {
            schema_version: fs_protocol::FILE_BINDING_VERSION,
            record_id: fixture.record,
            parent: Some(parent),
            name: "after.txt".into(),
            kind: FileNodeKind::File,
            tombstoned: false,
            actor: fixture.actor,
            observed_at_ms: 2,
        };
        let envelope = fixture.envelope(1, RecordOperationV1::SetBinding(binding.clone()));
        let snapshot = RecordSnapshot {
            record_id: fixture.record,
            binding: Some(binding),
            metadata: None,
            content_revision: None,
            versions: vec![],
            conflicts: vec![],
            attributions: vec![],
        };
        fixture
            .materializer
            .apply(fixture.binding(), &envelope, &snapshot)
            .unwrap();
        fixture
            .materializer
            .apply(fixture.binding(), &envelope, &snapshot)
            .unwrap();
        let mut namespace = fixture.namespace.lock().unwrap();
        assert!(namespace.lookup(ROOT_INO, "before.txt").is_none());
        assert_eq!(
            namespace.lookup(ROOT_INO, "after.txt").unwrap().ino.0,
            fixture.ino
        );
        drop(namespace);
    }
    {
        let fixture = Fixture::new();
        let metadata = FileMetadataV1 {
            schema_version: fs_protocol::files::FILE_METADATA_VERSION,
            record_id: fixture.record,
            content_hash: None,
            byte_len: 1,
            mime: None,
            previews: vec![],
            latest_actor: Some(fixture.actor),
            observed_at_ms: 1,
            index_status: IndexStatus::Pending,
            properties: None,
        };
        let envelope = fixture.envelope(1, RecordOperationV1::SetMetadata(metadata));
        let snapshot = fixture.snapshot_for(&envelope);

        let mut wrong_workspace = fixture.binding();
        wrong_workspace.workspace_id = Uuid::new_v4();
        assert_eq!(
            fixture
                .materializer
                .apply(wrong_workspace, &envelope, &snapshot),
            Err(RecordMaterializerError::WrongWorkspace)
        );
        let mut wrong_record = fixture.binding();
        wrong_record.record_id = FileRecordId::new();
        assert_eq!(
            fixture
                .materializer
                .apply(wrong_record, &envelope, &snapshot),
            Err(RecordMaterializerError::BindingMismatch)
        );

        assert!(fixture.namespace.lock().unwrap().remove(fixture.ino));
        assert_eq!(
            fixture
                .materializer
                .apply(fixture.binding(), &envelope, &snapshot),
            Err(RecordMaterializerError::UnknownOrTombstonedRecord(
                fixture.record
            ))
        );
    }
    {
        let fixture = Fixture::new();
        let version = FileVersionV1 {
            id: FileVersionId::new(),
            record_id: fixture.record,
            parents: vec![],
            actor: fixture.actor,
            timestamp_ms: 10,
            content_hash: Some(ContentHash([5; 32])),
            metadata_hash: ContentHash([6; 32]),
            provenance: FileProvenance::Create,
        };
        let envelope = fixture.envelope(1, RecordOperationV1::AppendVersion(version.clone()));
        let snapshot = fixture.snapshot_for(&envelope);
        fixture
            .materializer
            .apply(fixture.binding(), &envelope, &snapshot)
            .unwrap();
        fixture
            .materializer
            .apply(fixture.binding(), &envelope, &snapshot)
            .unwrap();

        let snapshot = fixture.history.snapshot(fixture.record).unwrap();
        assert_eq!(snapshot.versions, vec![version.clone()]);

        let mut collision = version;
        collision.metadata_hash = ContentHash([99; 32]);
        let collision_envelope = fixture.envelope(2, RecordOperationV1::AppendVersion(collision));
        let collision_snapshot = fixture.snapshot_for(&collision_envelope);
        assert_eq!(
            fixture
                .materializer
                .apply(fixture.binding(), &collision_envelope, &collision_snapshot),
            Err(RecordMaterializerError::HistoryRejected)
        );
        assert_eq!(
            fixture.history.snapshot(fixture.record).unwrap().versions,
            snapshot.versions
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn conflicts_project_repository_resolution_into_typed_history() {
    {
        use crate::record_repository::RecordRepositoryKey;
        let fixture = Fixture::new();
        let repository = RecordRepository::open(fixture.temp.path().join("conflict-race")).unwrap();
        let key = RecordRepositoryKey {
            workspace_id: fixture.workspace,
            record_id: fixture.record,
        };
        let first = FileVersionV1 {
            id: FileVersionId::new(),
            record_id: fixture.record,
            parents: vec![],
            actor: fixture.actor,
            timestamp_ms: 1,
            content_hash: None,
            metadata_hash: ContentHash([1; 32]),
            provenance: FileProvenance::Create,
        };
        let second = FileVersionV1 {
            id: FileVersionId::new(),
            parents: vec![first.id],
            timestamp_ms: 2,
            provenance: FileProvenance::Edit,
            ..first
        };
        let result = FileVersionV1 {
            id: FileVersionId::new(),
            parents: vec![first.id, second.id],
            timestamp_ms: 3,
            provenance: FileProvenance::ConflictResolution,
            ..first
        };
        fixture
            .history
            .append_batch(&[first.clone(), second.clone(), result.clone()])
            .unwrap();
        let conflict = FileConflictV1 {
            id: Uuid::new_v4(),
            record_id: fixture.record,
            versions: vec![first.id, second.id],
            status: ConflictStatus::Open,
            resolution: None,
        };
        let resolution = ConflictResolution {
            strategy: ConflictStrategy::Merge,
            result: result.id,
            actor: fixture.actor,
            timestamp_ms: 3,
        };
        let operations = [
            RecordOperationV1::AppendVersion(first),
            RecordOperationV1::AppendVersion(second),
            RecordOperationV1::AppendConflict(conflict.clone()),
            RecordOperationV1::AppendVersion(result),
            RecordOperationV1::ResolveConflict {
                conflict_id: conflict.id,
                resolution: resolution.clone(),
            },
        ];
        let mut captured = None;
        for (index, operation) in operations.into_iter().enumerate() {
            let envelope = fixture.envelope(index as u64 + 1, operation);
            repository.apply(key, &envelope, &AllowAll).unwrap();
            if index == 2 {
                captured = Some((
                    envelope,
                    repository.snapshot(key, &fixture.actor, &AllowAll).unwrap(),
                ));
            }
        }
        let (envelope, snapshot) = captured.unwrap();
        fixture
            .materializer
            .record_mutation(&repository, fixture.binding(), &envelope, &snapshot)
            .unwrap();
        let state = fixture.history.snapshot(fixture.record).unwrap();
        assert_eq!(state.conflicts[0].status, ConflictStatus::Resolved);
        assert_eq!(state.conflicts[0].resolution, Some(resolution));
    }
    {
        let fixture = Fixture::new();
        let first = FileVersionV1 {
            id: FileVersionId::new(),
            record_id: fixture.record,
            parents: vec![],
            actor: fixture.actor,
            timestamp_ms: 1,
            content_hash: Some(ContentHash([1; 32])),
            metadata_hash: ContentHash([11; 32]),
            provenance: FileProvenance::Create,
        };
        let second = FileVersionV1 {
            id: FileVersionId::new(),
            record_id: first.record_id,
            parents: vec![first.id],
            actor: first.actor,
            timestamp_ms: 2,
            content_hash: Some(ContentHash([2; 32])),
            metadata_hash: ContentHash([12; 32]),
            provenance: FileProvenance::Edit,
        };
        let result = FileVersionV1 {
            id: FileVersionId::new(),
            record_id: first.record_id,
            parents: vec![first.id, second.id],
            actor: first.actor,
            timestamp_ms: 3,
            content_hash: Some(ContentHash([3; 32])),
            metadata_hash: ContentHash([13; 32]),
            provenance: FileProvenance::ConflictResolution,
        };
        let conflict = FileConflictV1 {
            id: Uuid::new_v4(),
            record_id: fixture.record,
            versions: vec![first.id, second.id],
            status: ConflictStatus::Open,
            resolution: None,
        };
        let resolution = ConflictResolution {
            strategy: ConflictStrategy::Merge,
            result: result.id,
            actor: fixture.actor,
            timestamp_ms: result.timestamp_ms,
        };
        let operations = [
            RecordOperationV1::AppendVersion(first),
            RecordOperationV1::AppendVersion(second),
            RecordOperationV1::AppendConflict(conflict.clone()),
            RecordOperationV1::AppendVersion(result),
            RecordOperationV1::ResolveConflict {
                conflict_id: conflict.id,
                resolution: resolution.clone(),
            },
        ];
        for (index, operation) in operations.into_iter().enumerate() {
            let envelope = fixture.envelope(index as u64 + 1, operation);
            fixture
                .materializer
                .apply(
                    fixture.binding(),
                    &envelope,
                    &fixture.snapshot_for(&envelope),
                )
                .unwrap();
        }
        let projection = fixture.history.projection(fixture.record).unwrap();
        assert_eq!(projection.conflicts.len(), 1);
        assert_eq!(projection.conflicts[0].status, ConflictStatus::Resolved);
        assert_eq!(projection.conflicts[0].resolution, Some(resolution));
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn remote_transactions_and_merges_materialize_exactly_and_heal_after_restart() {
    {
        use crate::record_repository::{RecordRepository, RecordRepositoryKey};

        let fixture = Fixture::new();
        let repository =
            RecordRepository::open(fixture.temp.path().join("recovery-records")).unwrap();
        let key = RecordRepositoryKey {
            workspace_id: fixture.workspace,
            record_id: fixture.record,
        };
        let metadata = FileMetadataV1 {
            schema_version: fs_protocol::files::FILE_METADATA_VERSION,
            record_id: fixture.record,
            content_hash: Some(ContentHash([31; 32])),
            byte_len: 314,
            mime: Some("application/octet-stream".into()),
            previews: vec![],
            latest_actor: Some(fixture.actor),
            observed_at_ms: 31,
            index_status: IndexStatus::Pending,
            properties: None,
        };
        let version = FileVersionV1 {
            id: FileVersionId::new(),
            record_id: fixture.record,
            parents: vec![],
            actor: fixture.actor,
            timestamp_ms: 32,
            content_hash: Some(ContentHash([31; 32])),
            metadata_hash: ContentHash([32; 32]),
            provenance: FileProvenance::Edit,
        };
        let version_envelope =
            fixture.envelope(1, RecordOperationV1::AppendVersion(version.clone()));
        let revision = fs_protocol::ContentRevisionV1 {
            schema_version: fs_protocol::CONTENT_REVISION_VERSION,
            record_id: fixture.record,
            version_id: version.id,
            content_hash: version.content_hash,
            byte_len: 314,
            metadata_hash: version.metadata_hash,
        };
        let revision_envelope =
            fixture.envelope(2, RecordOperationV1::SetContentRevision(revision));
        let metadata_envelope = fixture.envelope(3, RecordOperationV1::SetMetadata(metadata));
        fixture
            .namespace
            .lock()
            .unwrap()
            .enable_signed_content_authority();
        repository
            .apply(key, &version_envelope, &AllowActor(fixture.actor))
            .unwrap();
        repository
            .apply(key, &revision_envelope, &AllowActor(fixture.actor))
            .unwrap();
        repository
            .apply(key, &metadata_envelope, &AllowActor(fixture.actor))
            .unwrap();

        assert!(
            fixture
                .namespace
                .lock()
                .unwrap()
                .hash_for(fixture.ino)
                .is_none()
        );
        assert!(fixture.history.snapshot(fixture.record).is_none());

        let report = fixture
            .materializer
            .reconcile_repository(&repository)
            .unwrap();
        assert_eq!(report.records_reconciled, 1);
        assert_eq!(report.operations_replayed, 3);
        assert_eq!(report.unknown_or_tombstoned_records, 0);
        assert_eq!(
            fixture
                .namespace
                .lock()
                .unwrap()
                .hash_for(fixture.ino)
                .unwrap()
                .as_bytes(),
            &[31; 32]
        );
        assert_eq!(
            fixture.history.snapshot(fixture.record).unwrap().versions,
            vec![version]
        );
        assert_eq!(
            fixture
                .namespace
                .lock()
                .unwrap()
                .content_revision_for(fixture.ino),
            Some(revision)
        );

        fixture
            .materializer
            .reconcile_repository(&repository)
            .unwrap();
        assert_eq!(
            fixture
                .history
                .snapshot(fixture.record)
                .unwrap()
                .versions
                .len(),
            1
        );
    }
    // Phase `remote_replacement_transaction_materializes_only_after_both_participants_in_either_order`.
    // Model the record-mesh delivery boundary: each participant stream is
    // persisted independently with the same signed prepare+commit pair, and
    // the materializer runs after every received stream batch.
    {
        use crate::record_repository::{RecordRepository, RecordRepositoryKey};

        for source_first in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let workspace = Uuid::new_v4();
            let device = SigningKey::from_bytes(&[41; 32]);
            let actor = FileActor {
                person: Ed25519PublicKey::from_bytes([42; 32]),
                device: Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
            };
            let authorizer = AllowActor(actor);

            let mut initial_namespace =
                Namespace::new("remote-transaction-before-restart").unwrap();
            let source_ino = initial_namespace
                .create_file(
                    ROOT_INO,
                    "source.txt",
                    Some(&iroh_blobs::Hash::from([1; 32])),
                    1,
                    1,
                )
                .unwrap();
            let replaced_ino = initial_namespace
                .create_file(
                    ROOT_INO,
                    "destination.txt",
                    Some(&iroh_blobs::Hash::from([2; 32])),
                    1,
                    1,
                )
                .unwrap();
            let source_record = initial_namespace.record_id_for(source_ino).unwrap();
            let replaced_record = initial_namespace.record_id_for(replaced_ino).unwrap();
            let root_record = initial_namespace.record_id_for(ROOT_INO).unwrap();
            let source_expected = FileVersionId::new();
            let replaced_expected = FileVersionId::new();
            let initial = [
                (
                    source_record,
                    source_expected,
                    "source.txt",
                    ContentHash([1; 32]),
                ),
                (
                    replaced_record,
                    replaced_expected,
                    "destination.txt",
                    ContentHash([2; 32]),
                ),
            ];
            for (ino, (record_id, version_id, _, hash)) in
                [source_ino, replaced_ino].into_iter().zip(initial)
            {
                assert!(initial_namespace.set_content_revision(
                    ino,
                    &ContentRevisionV1 {
                        schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                        record_id,
                        version_id,
                        content_hash: Some(hash),
                        byte_len: 1,
                        metadata_hash: ContentHash([3; 32]),
                    },
                    1,
                ));
            }
            // This is the namespace snapshot already durable before record
            // mesh delivery starts. The staged participant itself must not
            // require or produce a namespace write.
            let persisted_namespace_before_transaction = initial_namespace.save();
            let namespace_path = temp.path().join("namespace.bin");
            tilo_files::native::atomic::write_creating_parent(
                &namespace_path,
                &persisted_namespace_before_transaction,
            )
            .unwrap();
            let journal_path = temp.path().join("transaction-journal");
            let namespace = Arc::new(Mutex::new(initial_namespace));
            let history_path = temp.path().join("history.json");
            let history = Arc::new(FileHistoryStore::open(&history_path).unwrap());
            let repository_path = temp.path().join("records");
            let repository = RecordRepository::open(&repository_path).unwrap();

            for (record_id, version_id, name, hash) in initial {
                let binding = FileBindingV1 {
                    schema_version: FILE_BINDING_VERSION,
                    record_id,
                    parent: Some(root_record),
                    name: name.into(),
                    kind: FileNodeKind::File,
                    tombstoned: false,
                    actor,
                    observed_at_ms: 1,
                };
                let version = FileVersionV1 {
                    id: version_id,
                    record_id,
                    parents: vec![],
                    actor,
                    timestamp_ms: 1,
                    content_hash: Some(hash),
                    metadata_hash: ContentHash([3; 32]),
                    provenance: FileProvenance::Create,
                };
                let revision = ContentRevisionV1 {
                    schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                    record_id,
                    version_id,
                    content_hash: Some(hash),
                    byte_len: 1,
                    metadata_hash: version.metadata_hash,
                };
                let envelopes = [
                    RecordEnvelopeV1::signed(
                        record_id,
                        actor,
                        1,
                        RecordOperationV1::SetBinding(binding),
                        &device,
                    )
                    .unwrap(),
                    RecordEnvelopeV1::signed(
                        record_id,
                        actor,
                        2,
                        RecordOperationV1::AppendVersion(version),
                        &device,
                    )
                    .unwrap(),
                    RecordEnvelopeV1::signed(
                        record_id,
                        actor,
                        3,
                        RecordOperationV1::SetContentRevision(revision),
                        &device,
                    )
                    .unwrap(),
                ];
                repository
                    .apply_batch(
                        RecordRepositoryKey {
                            workspace_id: workspace,
                            record_id,
                        },
                        &envelopes,
                        &authorizer,
                    )
                    .unwrap();
            }

            let materializer = RecordMaterializer::new(
                workspace,
                Arc::clone(&namespace),
                Some(Arc::clone(&history)),
            )
            .with_transaction_recovery(
                Arc::new(Mutex::new(
                    crate::record_transaction::RecordTransactionJournal::open(&journal_path)
                        .unwrap(),
                )),
                namespace_path.clone(),
            );
            materializer.reconcile_repository(&repository).unwrap();

            let participant = |record_id, expected_version, role, hash: ContentHash| {
                let version = FileVersionV1 {
                    id: FileVersionId::new(),
                    record_id,
                    parents: vec![expected_version],
                    actor,
                    timestamp_ms: 2,
                    content_hash: Some(hash),
                    metadata_hash: ContentHash([3; 32]),
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
                    proposed_content_revision: Some(ContentRevisionV1 {
                        schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                        record_id,
                        version_id: version.id,
                        content_hash: version.content_hash,
                        byte_len: 1,
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
                    ContentHash([1; 32]),
                ),
                participant(
                    replaced_record,
                    replaced_expected,
                    FileMutationParticipantRole::Replaced,
                    ContentHash([2; 32]),
                ),
            ];
            participants.sort_by_key(|value| value.record_id);
            let transaction = FileMutationTransaction::signed(
                FileMutationTransactionManifest {
                    schema_version: FILE_MUTATION_TRANSACTION_VERSION,
                    transaction_id: Uuid::new_v4(),
                    workspace_id: workspace,
                    actor,
                    timestamp_ms: 2,
                    operation: FileMutationOperation::ReplacementRename {
                        source_parent: root_record,
                        source_name: "source.txt".into(),
                        destination_parent: root_record,
                        destination_name: "destination.txt".into(),
                        source_record,
                        replaced_record,
                    },
                    participants,
                },
                &device,
            )
            .unwrap();
            let before_delivery_probe = namespace
                .lock()
                .unwrap()
                .clone()
                .apply_replacement_transaction(&transaction);
            assert!(
                before_delivery_probe.is_ok(),
                "valid initial namespace must accept the transaction: {before_delivery_probe:?}"
            );

            let delivery_order = if source_first {
                [source_record, replaced_record]
            } else {
                [replaced_record, source_record]
            };
            let deliver = |repository: &RecordRepository, record_id| {
                let envelopes = [
                    RecordEnvelopeV1::signed(
                        record_id,
                        actor,
                        4,
                        RecordOperationV1::PrepareTransaction(transaction.clone()),
                        &device,
                    )
                    .unwrap(),
                    RecordEnvelopeV1::signed(
                        record_id,
                        actor,
                        5,
                        RecordOperationV1::CommitTransaction {
                            transaction_id: transaction.manifest.transaction_id,
                        },
                        &device,
                    )
                    .unwrap(),
                ];
                repository
                    .apply_batch(
                        RecordRepositoryKey {
                            workspace_id: workspace,
                            record_id,
                        },
                        &envelopes,
                        &authorizer,
                    )
                    .unwrap();
            };

            deliver(&repository, delivery_order[0]);
            let first_report = materializer.reconcile_repository(&repository).unwrap();
            assert_eq!(first_report.transactions_reconciled, 0);
            {
                let mut namespace = namespace.lock().unwrap();
                assert_eq!(
                    namespace.lookup(ROOT_INO, "source.txt").unwrap().ino.0,
                    source_ino
                );
                assert_eq!(
                    namespace.lookup(ROOT_INO, "destination.txt").unwrap().ino.0,
                    replaced_ino
                );
                drop(namespace);
            }
            assert_eq!(history.snapshot(source_record).unwrap().versions.len(), 1);
            assert_eq!(history.snapshot(replaced_record).unwrap().versions.len(), 1);

            // Simulate a process restart while exactly one participant is
            // durable. Both the record log and namespace are reloaded before
            // the other participant arrives.
            drop(materializer);
            drop(repository);
            drop(history);
            let restarted_namespace = Arc::new(Mutex::new(
                Namespace::from_bytes(
                    "remote-transaction-after-restart",
                    &persisted_namespace_before_transaction,
                )
                .unwrap(),
            ));
            let restarted_history = Arc::new(FileHistoryStore::open(&history_path).unwrap());
            let restarted_repository = RecordRepository::open(&repository_path).unwrap();
            let blocked_namespace_path = temp.path().join("namespace-write-blocked");
            if source_first {
                std::fs::create_dir(&blocked_namespace_path).unwrap();
            }
            let restarted_materializer = RecordMaterializer::new(
                workspace,
                Arc::clone(&restarted_namespace),
                Some(Arc::clone(&restarted_history)),
            )
            .with_transaction_recovery(
                Arc::new(Mutex::new(
                    crate::record_transaction::RecordTransactionJournal::open(&journal_path)
                        .unwrap(),
                )),
                if source_first {
                    blocked_namespace_path.clone()
                } else {
                    namespace_path.clone()
                },
            );
            {
                let namespace = restarted_namespace.lock().unwrap();
                assert_eq!(
                    namespace
                        .entry_at("source.txt")
                        .map(|entry| entry.record_id),
                    Some(source_record)
                );
                assert_eq!(
                    namespace
                        .entry_at("destination.txt")
                        .map(|entry| entry.record_id),
                    Some(replaced_record)
                );
                drop(namespace);
            }

            deliver(&restarted_repository, delivery_order[1]);
            let projection_probe = restarted_namespace
                .lock()
                .unwrap()
                .clone()
                .apply_replacement_transaction(&transaction);
            assert!(
                projection_probe.is_ok(),
                "complete remote transaction must project after restart: {projection_probe:?}"
            );
            let second_result = restarted_materializer.reconcile_repository(&restarted_repository);
            let second_report = if source_first {
                assert_eq!(
                    second_result,
                    Err(RecordMaterializerError::TransactionRecoveryUnavailable)
                );
                {
                    let mut namespace = restarted_namespace.lock().unwrap();
                    assert!(namespace.lookup(ROOT_INO, "source.txt").is_some());
                    assert_eq!(
                        namespace
                            .entry_at("destination.txt")
                            .map(|entry| entry.record_id),
                        Some(replaced_record)
                    );
                    drop(namespace);
                }
                let recovering_materializer = RecordMaterializer::new(
                    workspace,
                    Arc::clone(&restarted_namespace),
                    Some(Arc::clone(&restarted_history)),
                )
                .with_transaction_recovery(
                    Arc::new(Mutex::new(
                        crate::record_transaction::RecordTransactionJournal::open(&journal_path)
                            .unwrap(),
                    )),
                    namespace_path.clone(),
                );
                recovering_materializer
                    .reconcile_repository(&restarted_repository)
                    .unwrap()
            } else {
                second_result.unwrap()
            };
            assert_eq!(second_report.transactions_reconciled, 1);
            let mut namespace = restarted_namespace.lock().unwrap();
            assert!(namespace.lookup(ROOT_INO, "source.txt").is_none());
            assert_eq!(
                namespace.lookup(ROOT_INO, "destination.txt").unwrap().ino.0,
                namespace.ino_for_record_id(source_record).unwrap()
            );
            assert!(namespace.ino_for_record_id(replaced_record).is_none());
            drop(namespace);
            assert_eq!(
                restarted_history
                    .snapshot(source_record)
                    .unwrap()
                    .versions
                    .len(),
                2
            );
            assert_eq!(
                restarted_history
                    .snapshot(replaced_record)
                    .unwrap()
                    .versions
                    .len(),
                2
            );
            assert!(
                crate::record_transaction::RecordTransactionJournal::open(&journal_path)
                    .unwrap()
                    .entries()
                    .next()
                    .is_none(),
                "successful remote projection removes its fully committed journal intent"
            );
            let persisted_after = std::fs::read(&namespace_path).unwrap();
            let mut durable_namespace =
                Namespace::from_bytes("remote-transaction-durable", &persisted_after).unwrap();
            assert!(durable_namespace.lookup(ROOT_INO, "source.txt").is_none());
            assert_eq!(
                durable_namespace
                    .entry_at("destination.txt")
                    .map(|entry| entry.record_id),
                Some(source_record)
            );
            assert!(
                durable_namespace
                    .ino_for_record_id(replaced_record)
                    .is_none()
            );

            // Re-running startup reconciliation is an exact replay, not a
            // second history or namespace transition.
            restarted_materializer
                .reconcile_repository(&restarted_repository)
                .unwrap();
            assert_eq!(
                restarted_history
                    .snapshot(source_record)
                    .unwrap()
                    .versions
                    .len(),
                2
            );
            assert_eq!(
                restarted_history
                    .snapshot(replaced_record)
                    .unwrap()
                    .versions
                    .len(),
                2
            );

            // A later ordinary binding for every participant supersedes the
            // transaction. Its immutable versions and content survive, but its
            // structure does not move the records back.
            let source_revision = transaction
                .manifest
                .participants
                .iter()
                .find(|participant| participant.record_id == source_record)
                .and_then(|participant| participant.proposed_content_revision)
                .expect("replacement source has a proposed revision");
            for (record_id, name) in [
                (source_record, "later-source.txt"),
                (replaced_record, "later-victim.txt"),
            ] {
                restarted_repository
                    .apply(
                        RecordRepositoryKey {
                            workspace_id: workspace,
                            record_id,
                        },
                        &RecordEnvelopeV1::signed(
                            record_id,
                            actor,
                            6,
                            RecordOperationV1::SetBinding(FileBindingV1 {
                                schema_version: FILE_BINDING_VERSION,
                                record_id,
                                parent: Some(root_record),
                                name: name.into(),
                                kind: FileNodeKind::File,
                                tombstoned: false,
                                actor,
                                observed_at_ms: 10,
                            }),
                            &device,
                        )
                        .unwrap(),
                        &authorizer,
                    )
                    .unwrap();
            }
            let later = restarted_materializer
                .reconcile_repository(&restarted_repository)
                .unwrap();
            assert_eq!(later.transactions_reconciled, 1);
            let mut later_namespace = restarted_namespace.lock().unwrap();
            assert_eq!(
                later_namespace
                    .entry_at("later-source.txt")
                    .map(|entry| entry.record_id),
                Some(source_record)
            );
            assert_eq!(
                later_namespace
                    .entry_at("later-victim.txt")
                    .map(|entry| entry.record_id),
                Some(replaced_record)
            );
            assert!(later_namespace.entry_at("destination.txt").is_none());
            let later_source_ino = later_namespace
                .ensure_ino_for_record_id(source_record)
                .unwrap();
            assert_eq!(
                later_namespace.content_revision_for(later_source_ino),
                Some(source_revision)
            );
            drop(later_namespace);
        }
    }
    {
        for codec in [
            ConflictMergeCodecV1::PlainUtf8ThreeWayV1,
            ConflictMergeCodecV1::TiloDocAutomergeV1,
        ] {
            remote_completed_merge_case(codec);
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "The shared UTF-8 and Automerge merge fixture checks exact revision projection, history resolution, journal cleanup, and restart replay."
)] // Shared end-to-end merge/restart scenario helper.
fn remote_completed_merge_case(codec: ConflictMergeCodecV1) {
    use crate::record_repository::{RecordRepository, RecordRepositoryKey};

    let temp = tempfile::tempdir().unwrap();
    let workspace = Uuid::new_v4();
    let device = SigningKey::from_bytes(&[51; 32]);
    let actor = FileActor {
        person: Ed25519PublicKey::from_bytes([52; 32]),
        device: Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
    };
    let authorizer = AllowActor(actor);
    let mut initial_namespace = Namespace::new("remote-merge").unwrap();
    let ino = initial_namespace
        .create_file(
            ROOT_INO,
            "merge.txt",
            Some(&iroh_blobs::Hash::from([2; 32])),
            9,
            1,
        )
        .unwrap();
    let record_id = initial_namespace.record_id_for(ino).unwrap();
    let root_record = initial_namespace.record_id_for(ROOT_INO).unwrap();
    let base = FileVersionV1 {
        id: FileVersionId::new(),
        record_id,
        parents: Vec::new(),
        actor,
        timestamp_ms: 1,
        content_hash: Some(ContentHash([1; 32])),
        metadata_hash: ContentHash([11; 32]),
        provenance: FileProvenance::Create,
    };
    let current = FileVersionV1 {
        id: FileVersionId::new(),
        record_id,
        parents: vec![base.id],
        actor,
        timestamp_ms: 2,
        content_hash: Some(ContentHash([2; 32])),
        metadata_hash: ContentHash([12; 32]),
        provenance: FileProvenance::Edit,
    };
    let other = FileVersionV1 {
        id: FileVersionId::new(),
        record_id,
        parents: vec![base.id],
        actor,
        timestamp_ms: 3,
        content_hash: Some(ContentHash([3; 32])),
        metadata_hash: ContentHash([13; 32]),
        provenance: FileProvenance::Edit,
    };
    let conflict = FileConflictV1 {
        id: Uuid::new_v4(),
        record_id,
        versions: vec![current.id, other.id],
        status: ConflictStatus::Open,
        resolution: None,
    };
    let current_revision = ContentRevisionV1 {
        schema_version: fs_protocol::CONTENT_REVISION_VERSION,
        record_id,
        version_id: current.id,
        content_hash: current.content_hash,
        byte_len: 9,
        metadata_hash: current.metadata_hash,
    };
    assert!(initial_namespace.set_content_revision(ino, &current_revision, 2));
    let namespace_path = temp.path().join("namespace.bin");
    tilo_files::native::atomic::write_creating_parent(&namespace_path, &initial_namespace.save())
        .unwrap();
    let namespace = Arc::new(Mutex::new(initial_namespace));
    let history_path = temp.path().join("history.json");
    let history = Arc::new(FileHistoryStore::open(&history_path).unwrap());
    let repository_path = temp.path().join("records");
    let repository = RecordRepository::open(&repository_path).unwrap();
    let key = RecordRepositoryKey {
        workspace_id: workspace,
        record_id,
    };
    let initial = [
        RecordEnvelopeV1::signed(
            record_id,
            actor,
            1,
            RecordOperationV1::SetBinding(FileBindingV1 {
                schema_version: FILE_BINDING_VERSION,
                record_id,
                parent: Some(root_record),
                name: "merge.txt".into(),
                kind: FileNodeKind::File,
                tombstoned: false,
                actor,
                observed_at_ms: 1,
            }),
            &device,
        )
        .unwrap(),
        RecordEnvelopeV1::signed(
            record_id,
            actor,
            2,
            RecordOperationV1::AppendVersion(base.clone()),
            &device,
        )
        .unwrap(),
        RecordEnvelopeV1::signed(
            record_id,
            actor,
            3,
            RecordOperationV1::AppendVersion(current.clone()),
            &device,
        )
        .unwrap(),
        RecordEnvelopeV1::signed(
            record_id,
            actor,
            4,
            RecordOperationV1::AppendVersion(other),
            &device,
        )
        .unwrap(),
        RecordEnvelopeV1::signed(
            record_id,
            actor,
            5,
            RecordOperationV1::SetContentRevision(current_revision),
            &device,
        )
        .unwrap(),
        RecordEnvelopeV1::signed(
            record_id,
            actor,
            6,
            RecordOperationV1::AppendConflict(conflict.clone()),
            &device,
        )
        .unwrap(),
    ];
    repository.apply_batch(key, &initial, &authorizer).unwrap();
    let journal_path = temp.path().join("journal");
    let journal = Arc::new(Mutex::new(
        crate::record_transaction::RecordTransactionJournal::open(&journal_path).unwrap(),
    ));
    let materializer = RecordMaterializer::new(
        workspace,
        Arc::clone(&namespace),
        Some(Arc::clone(&history)),
    )
    .with_transaction_recovery(Arc::clone(&journal), namespace_path.clone());
    materializer.reconcile_repository(&repository).unwrap();

    let result = FileVersionV1 {
        id: FileVersionId::new(),
        record_id,
        parents: conflict.versions.clone(),
        actor,
        timestamp_ms: 4,
        content_hash: Some(ContentHash([4; 32])),
        metadata_hash: ContentHash([14; 32]),
        provenance: FileProvenance::ConflictResolution,
    };
    let result_revision = ContentRevisionV1 {
        schema_version: fs_protocol::CONTENT_REVISION_VERSION,
        record_id,
        version_id: result.id,
        content_hash: result.content_hash,
        byte_len: 15,
        metadata_hash: result.metadata_hash,
    };
    let mut participant = FileMutationParticipant {
        record_id,
        role: FileMutationParticipantRole::MergeSource,
        expected_version: current.id,
        proposed_binding: FileBindingV1 {
            schema_version: FILE_BINDING_VERSION,
            record_id,
            parent: Some(root_record),
            name: "merge.txt".into(),
            kind: FileNodeKind::File,
            tombstoned: false,
            actor,
            observed_at_ms: 4,
        },
        proposed_version: result.clone(),
        proposed_content_revision: Some(result_revision),
        projection_hash: ContentHash([0; 32]),
    };
    participant.projection_hash = participant.computed_projection_hash().unwrap();
    let transaction = FileMutationTransaction::signed(
        FileMutationTransactionManifest {
            schema_version: fs_protocol::files::FILE_MUTATION_TRANSACTION_VERSION_V2,
            transaction_id: Uuid::new_v4(),
            workspace_id: workspace,
            actor,
            timestamp_ms: 4,
            operation: FileMutationOperation::MergeContentConflict {
                source_record: record_id,
                parent_record: root_record,
                source_name: "merge.txt".into(),
                conflict_id: conflict.id,
                conflict_versions: conflict.versions,
                current_version: current.id,
                base_version: matches!(codec, ConflictMergeCodecV1::PlainUtf8ThreeWayV1)
                    .then_some(base.id),
                codec,
            },
            participants: vec![participant],
        },
        &device,
    )
    .unwrap();
    repository
        .apply(
            key,
            &RecordEnvelopeV1::signed(
                record_id,
                actor,
                7,
                RecordOperationV1::PrepareTransaction(transaction.clone()),
                &device,
            )
            .unwrap(),
            &authorizer,
        )
        .unwrap();
    assert_eq!(
        materializer
            .reconcile_repository(&repository)
            .unwrap()
            .transactions_reconciled,
        0
    );
    assert_eq!(
        namespace.lock().unwrap().content_revision_for(ino),
        Some(current_revision)
    );
    assert!(
        history.snapshot(record_id).unwrap().conflicts[0]
            .resolution
            .is_none()
    );

    repository
        .apply(
            key,
            &RecordEnvelopeV1::signed(
                record_id,
                actor,
                8,
                RecordOperationV1::CommitTransaction {
                    transaction_id: transaction.manifest.transaction_id,
                },
                &device,
            )
            .unwrap(),
            &authorizer,
        )
        .unwrap();
    assert_eq!(
        materializer
            .reconcile_repository(&repository)
            .unwrap()
            .transactions_reconciled,
        1
    );
    assert_eq!(
        namespace.lock().unwrap().content_revision_for(ino),
        Some(result_revision)
    );
    let state = history.snapshot(record_id).unwrap();
    assert_eq!(
        state
            .versions
            .iter()
            .filter(|value| **value == result)
            .count(),
        1
    );
    assert_eq!(
        state.conflicts[0].resolution.as_ref().unwrap().strategy,
        ConflictStrategy::Merge
    );
    assert!(journal.lock().unwrap().entries().next().is_none());

    drop(materializer);
    drop(namespace);
    drop(history);
    drop(repository);
    let restarted_namespace = Arc::new(Mutex::new(
        Namespace::from_bytes(
            "remote-merge-restart",
            &std::fs::read(&namespace_path).unwrap(),
        )
        .unwrap(),
    ));
    let restarted_history = Arc::new(FileHistoryStore::open(&history_path).unwrap());
    let restarted_repository = RecordRepository::open(&repository_path).unwrap();
    let restarted_journal = Arc::new(Mutex::new(
        crate::record_transaction::RecordTransactionJournal::open(&journal_path).unwrap(),
    ));
    let restarted = RecordMaterializer::new(
        workspace,
        Arc::clone(&restarted_namespace),
        Some(Arc::clone(&restarted_history)),
    )
    .with_transaction_recovery(Arc::clone(&restarted_journal), namespace_path);
    restarted
        .reconcile_repository(&restarted_repository)
        .unwrap();
    assert_eq!(
        restarted_namespace
            .lock()
            .unwrap()
            .content_revision_for(ino),
        Some(result_revision)
    );
    assert_eq!(
        restarted_history
            .snapshot(record_id)
            .unwrap()
            .versions
            .iter()
            .filter(|value| **value == result)
            .count(),
        1
    );
    assert!(restarted_journal.lock().unwrap().entries().next().is_none());
}

struct SignedLog {
    workspace: Uuid,
    repository: RecordRepository,
    actor: FileActor,
    device: SigningKey,
    authorizer: AllowActor,
}

impl SignedLog {
    fn new(temp: &tempfile::TempDir, workspace: Uuid, seed: u8) -> Self {
        let device = SigningKey::from_bytes(&[seed; 32]);
        let actor = FileActor {
            person: Ed25519PublicKey::from_bytes([seed.wrapping_add(1); 32]),
            device: Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
        };
        let repository = RecordRepository::open(temp.path().join("records")).unwrap();
        Self {
            workspace,
            repository,
            actor,
            device,
            authorizer: AllowActor(actor),
        }
    }

    fn envelope(
        &self,
        record_id: FileRecordId,
        sequence: u64,
        operation: RecordOperationV1,
    ) -> RecordEnvelopeV1 {
        RecordEnvelopeV1::signed(record_id, self.actor, sequence, operation, &self.device).unwrap()
    }

    fn apply(&self, record_id: FileRecordId, envelopes: &[RecordEnvelopeV1]) {
        self.repository
            .apply_batch(
                RecordRepositoryKey {
                    workspace_id: self.workspace,
                    record_id,
                },
                envelopes,
                &self.authorizer,
            )
            .unwrap();
    }
}

fn file_version(
    record_id: FileRecordId,
    actor: FileActor,
    timestamp_ms: u64,
    hash: [u8; 32],
    byte_len: u64,
) -> (FileVersionV1, ContentRevisionV1, FileMetadataV1) {
    let metadata_hash = ContentHash([hash[0].wrapping_add(1); 32]);
    let id = FileVersionId::new();
    let version = FileVersionV1 {
        id,
        record_id,
        parents: vec![],
        actor,
        timestamp_ms,
        content_hash: Some(ContentHash(hash)),
        metadata_hash,
        provenance: FileProvenance::Create,
    };
    let revision = ContentRevisionV1 {
        schema_version: fs_protocol::CONTENT_REVISION_VERSION,
        record_id,
        version_id: id,
        content_hash: Some(ContentHash(hash)),
        byte_len,
        metadata_hash,
    };
    let metadata = FileMetadataV1 {
        schema_version: fs_protocol::files::FILE_METADATA_VERSION,
        record_id,
        content_hash: Some(ContentHash(hash)),
        byte_len,
        mime: None,
        previews: vec![],
        latest_actor: Some(actor),
        observed_at_ms: timestamp_ms,
        index_status: IndexStatus::Pending,
        properties: None,
    };
    (version, revision, metadata)
}

fn directory_version(
    record_id: FileRecordId,
    actor: FileActor,
    timestamp_ms: u64,
) -> FileVersionV1 {
    FileVersionV1 {
        id: FileVersionId::new(),
        record_id,
        parents: vec![],
        actor,
        timestamp_ms,
        content_hash: None,
        metadata_hash: ContentHash([9; 32]),
        provenance: FileProvenance::Create,
    }
}

fn create_record_envelopes(
    log: &SignedLog,
    record_id: FileRecordId,
    binding: FileBindingV1,
    version: Option<FileVersionV1>,
    revision: Option<ContentRevisionV1>,
    metadata: Option<FileMetadataV1>,
) -> Vec<RecordEnvelopeV1> {
    let mut envelopes = vec![log.envelope(record_id, 1, RecordOperationV1::SetBinding(binding))];
    if let Some(version) = version {
        envelopes.push(log.envelope(
            record_id,
            envelopes.len().saturating_add(1) as u64,
            RecordOperationV1::AppendVersion(version),
        ));
    }
    if let Some(revision) = revision {
        envelopes.push(log.envelope(
            record_id,
            envelopes.len().saturating_add(1) as u64,
            RecordOperationV1::SetContentRevision(revision),
        ));
    }
    if let Some(metadata) = metadata {
        envelopes.push(log.envelope(
            record_id,
            envelopes.len().saturating_add(1) as u64,
            RecordOperationV1::SetMetadata(metadata),
        ));
    }
    envelopes
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one log-only rebuild scenario keeps its first pass, repeat, and restart assertions together"
)]
fn a_signed_record_log_alone_rebuilds_the_structural_namespace() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = Uuid::new_v4();
    let log = SignedLog::new(&temp, workspace, 71);
    let mut origin = Namespace::new("origin").unwrap();
    let docs = origin.create_dir(ROOT_INO, "docs", 10).unwrap();
    let file = origin
        .create_file(docs, "a.txt", Some(&iroh_blobs::Hash::from([1; 32])), 7, 11)
        .unwrap();
    let root_record = origin.record_id_for(ROOT_INO).unwrap();
    let docs_record = origin.record_id_for(docs).unwrap();
    let file_record = origin.record_id_for(file).unwrap();
    let root_binding = origin.signed_binding_for(ROOT_INO, log.actor, 1).unwrap();
    let docs_binding = origin.signed_binding_for(docs, log.actor, 10).unwrap();
    let file_binding = origin.signed_binding_for(file, log.actor, 11).unwrap();
    let (file_version, file_revision, file_metadata) =
        file_version(file_record, log.actor, 11, [1; 32], 7);
    let docs_version = directory_version(docs_record, log.actor, 10);

    // Deliberately child-first: the file arrives before the directory and
    // the root identity arrives last. Reconciliation must settle the tree
    // from the signed bindings alone, with no namespace snapshot.
    log.apply(
        file_record,
        &create_record_envelopes(
            &log,
            file_record,
            file_binding,
            Some(file_version.clone()),
            Some(file_revision),
            Some(file_metadata),
        ),
    );
    log.apply(
        docs_record,
        &create_record_envelopes(
            &log,
            docs_record,
            docs_binding,
            Some(docs_version),
            None,
            None,
        ),
    );
    log.apply(
        root_record,
        &[log.envelope(root_record, 1, RecordOperationV1::SetBinding(root_binding))],
    );

    let rebuilt = Arc::new(Mutex::new(Namespace::new("rebuild").unwrap()));
    let history = Arc::new(FileHistoryStore::open(temp.path().join("history.json")).unwrap());
    let materializer =
        RecordMaterializer::new(workspace, Arc::clone(&rebuilt), Some(Arc::clone(&history)));
    let report = materializer.reconcile_repository(&log.repository).unwrap();
    assert_eq!(report.unknown_or_tombstoned_records, 0);
    assert_eq!(report.deferred_records, 0);
    assert!(report.bindings_projected >= 3);

    let mut namespace = rebuilt.lock().unwrap();
    assert_eq!(namespace.record_id_for(ROOT_INO), Some(root_record));
    let docs_entry = namespace.entry_at("docs").expect("rebuilt directory");
    assert_eq!(docs_entry.record_id, docs_record);
    assert_eq!(docs_entry.kind, NodeKind::Dir);
    let docs_ino = namespace.resolve_path_ino("docs").unwrap();
    let file_entry = namespace.entry_at("docs/a.txt").expect("rebuilt file");
    assert_eq!(file_entry.record_id, file_record);
    assert_eq!(file_entry.kind, NodeKind::File);
    let file_ino = namespace.resolve_path_ino("docs/a.txt").unwrap();
    assert_eq!(
        namespace.id_for_ino(file_ino),
        Some(format!("record:{}", file_record.0).as_str())
    );
    assert_eq!(
        namespace.content_revision_for(file_ino),
        Some(file_revision)
    );
    assert_eq!(
        namespace.hash_for(file_ino),
        Some(iroh_blobs::Hash::from([1; 32]))
    );
    assert_eq!(namespace.getattr(file_ino).unwrap().size, 7);
    assert_eq!(namespace.lookup(docs_ino, "a.txt").unwrap().ino.0, file_ino);
    drop(namespace);
    assert!(history.snapshot(file_record).is_some());
    assert!(history.snapshot(docs_record).is_some());

    // A second pass is an exact replay: no binding changes, no deferrals,
    // no history duplication.
    let second = materializer.reconcile_repository(&log.repository).unwrap();
    assert_eq!(second.bindings_projected, 0);
    assert_eq!(second.deferred_records, 0);
    assert_eq!(second.unknown_or_tombstoned_records, 0);
    assert_eq!(
        history.snapshot(file_record).unwrap().versions,
        vec![file_version]
    );

    // A restart rebuilds the same tree from the same log into a new,
    // empty namespace.
    drop(materializer);
    drop(rebuilt);
    let restarted = Arc::new(Mutex::new(Namespace::new("restarted").unwrap()));
    let restarted_history =
        Arc::new(FileHistoryStore::open(temp.path().join("history.json")).unwrap());
    let restarted_materializer =
        RecordMaterializer::new(workspace, Arc::clone(&restarted), Some(restarted_history));
    restarted_materializer
        .reconcile_repository(&log.repository)
        .unwrap();
    let mut restarted = restarted.lock().unwrap();
    assert_eq!(restarted.record_id_for(ROOT_INO), Some(root_record));
    assert_eq!(
        restarted
            .entry_at("docs/a.txt")
            .map(|entry| entry.record_id),
        Some(file_record)
    );
    let restarted_file_ino = restarted.resolve_path_ino("docs/a.txt").unwrap();
    assert_eq!(
        restarted.content_revision_for(restarted_file_ino),
        Some(file_revision)
    );
    drop(restarted);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one transition scenario keeps moves, tombstones, resurrection, repeat and restart assertions together"
)]
fn signed_moves_tombstones_and_resurrections_rebuild_repeat_and_restart() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = Uuid::new_v4();
    let log = SignedLog::new(&temp, workspace, 81);
    let mut origin = Namespace::new("origin").unwrap();
    let docs = origin.create_dir(ROOT_INO, "docs", 10).unwrap();
    let moved = origin
        .create_file(
            docs,
            "moved.txt",
            Some(&iroh_blobs::Hash::from([2; 32])),
            2,
            11,
        )
        .unwrap();
    let revived = origin
        .create_file(
            docs,
            "revived.txt",
            Some(&iroh_blobs::Hash::from([3; 32])),
            3,
            12,
        )
        .unwrap();
    let deleted = origin
        .create_file(
            docs,
            "deleted.txt",
            Some(&iroh_blobs::Hash::from([4; 32])),
            4,
            13,
        )
        .unwrap();
    let root_record = origin.record_id_for(ROOT_INO).unwrap();
    let docs_record = origin.record_id_for(docs).unwrap();
    let moved_record = origin.record_id_for(moved).unwrap();
    let revived_record = origin.record_id_for(revived).unwrap();
    let deleted_record = origin.record_id_for(deleted).unwrap();
    let root_binding = origin.signed_binding_for(ROOT_INO, log.actor, 1).unwrap();
    let docs_binding = origin.signed_binding_for(docs, log.actor, 10).unwrap();
    let moved_binding = origin.signed_binding_for(moved, log.actor, 11).unwrap();
    let revived_binding = origin.signed_binding_for(revived, log.actor, 12).unwrap();
    let deleted_binding = origin.signed_binding_for(deleted, log.actor, 13).unwrap();
    let (moved_version, moved_revision, moved_metadata) =
        file_version(moved_record, log.actor, 11, [2; 32], 2);
    let (revived_version, revived_revision, revived_metadata) =
        file_version(revived_record, log.actor, 12, [3; 32], 3);
    let (deleted_version, deleted_revision, deleted_metadata) =
        file_version(deleted_record, log.actor, 13, [4; 32], 4);
    let docs_version = directory_version(docs_record, log.actor, 10);

    // Each record's stream is in sequence order; records arrive before the
    // directory and root they depend on.
    let mut moved_envelopes = create_record_envelopes(
        &log,
        moved_record,
        moved_binding.clone(),
        Some(moved_version),
        Some(moved_revision),
        Some(moved_metadata),
    );
    moved_envelopes.push(log.envelope(
        moved_record,
        moved_envelopes.len() as u64 + 1,
        RecordOperationV1::SetBinding(FileBindingV1 {
            parent: Some(root_record),
            name: "moved.txt".into(),
            observed_at_ms: 20,
            ..moved_binding
        }),
    ));
    let mut revived_envelopes = create_record_envelopes(
        &log,
        revived_record,
        revived_binding.clone(),
        Some(revived_version.clone()),
        Some(revived_revision),
        Some(revived_metadata),
    );
    revived_envelopes.push(log.envelope(
        revived_record,
        revived_envelopes.len() as u64 + 1,
        RecordOperationV1::SetBinding(FileBindingV1 {
            tombstoned: true,
            observed_at_ms: 21,
            ..revived_binding.clone()
        }),
    ));
    revived_envelopes.push(log.envelope(
        revived_record,
        revived_envelopes.len() as u64 + 1,
        RecordOperationV1::SetBinding(FileBindingV1 {
            observed_at_ms: 22,
            ..revived_binding
        }),
    ));
    let mut deleted_envelopes = create_record_envelopes(
        &log,
        deleted_record,
        deleted_binding.clone(),
        Some(deleted_version.clone()),
        Some(deleted_revision),
        Some(deleted_metadata),
    );
    deleted_envelopes.push(log.envelope(
        deleted_record,
        deleted_envelopes.len() as u64 + 1,
        RecordOperationV1::SetBinding(FileBindingV1 {
            tombstoned: true,
            observed_at_ms: 21,
            ..deleted_binding
        }),
    ));
    deleted_envelopes.push(log.envelope(
        deleted_record,
        deleted_envelopes.len() as u64 + 1,
        RecordOperationV1::AppendVersion(FileVersionV1 {
            id: FileVersionId::new(),
            record_id: deleted_record,
            parents: vec![deleted_version.id],
            actor: log.actor,
            timestamp_ms: 21,
            content_hash: None,
            metadata_hash: ContentHash([21; 32]),
            provenance: FileProvenance::Delete,
        }),
    ));

    log.apply(deleted_record, &deleted_envelopes);
    log.apply(revived_record, &revived_envelopes);
    log.apply(moved_record, &moved_envelopes);
    log.apply(
        docs_record,
        &create_record_envelopes(
            &log,
            docs_record,
            docs_binding,
            Some(docs_version),
            None,
            None,
        ),
    );
    log.apply(
        root_record,
        &[log.envelope(root_record, 1, RecordOperationV1::SetBinding(root_binding))],
    );

    let rebuilt = Arc::new(Mutex::new(Namespace::new("transitions").unwrap()));
    let history = Arc::new(FileHistoryStore::open(temp.path().join("history.json")).unwrap());
    let materializer =
        RecordMaterializer::new(workspace, Arc::clone(&rebuilt), Some(Arc::clone(&history)));
    let report = materializer.reconcile_repository(&log.repository).unwrap();
    assert_eq!(report.unknown_or_tombstoned_records, 0);
    assert_eq!(report.deferred_records, 0);

    let namespace = rebuilt.lock().unwrap();
    assert_eq!(
        namespace.entry_at("moved.txt").map(|entry| entry.record_id),
        Some(moved_record),
        "a signed move from docs/ to the root is a placement transition"
    );
    assert!(namespace.entry_at("docs/moved.txt").is_none());
    assert_eq!(
        namespace
            .entry_at("docs/revived.txt")
            .map(|entry| entry.record_id),
        Some(revived_record),
        "a newer non-tombstone binding wins a delete/placement conflict"
    );
    assert!(namespace.entry_at("docs/deleted.txt").is_none());
    assert!(namespace.entry_at("docs").is_some());
    drop(namespace);

    // The tombstoned record keeps its immutable history even though it has
    // no live path; history is record-scoped, never guessed from a path.
    let deleted_history = history.snapshot(deleted_record).unwrap();
    assert_eq!(deleted_history.versions.len(), 2);
    assert_eq!(
        deleted_history
            .versions
            .last()
            .map(|version| version.provenance.clone()),
        Some(FileProvenance::Delete)
    );
    assert_eq!(
        history.snapshot(revived_record).unwrap().versions,
        vec![revived_version]
    );

    let second = materializer.reconcile_repository(&log.repository).unwrap();
    assert_eq!(second.bindings_projected, 0);
    assert_eq!(second.deferred_records, 0);
    assert_eq!(second.unknown_or_tombstoned_records, 0);

    drop(materializer);
    drop(rebuilt);
    drop(history);
    let restarted = Arc::new(Mutex::new(Namespace::new("transitions-restart").unwrap()));
    let restarted_history =
        Arc::new(FileHistoryStore::open(temp.path().join("history.json")).unwrap());
    let restarted_materializer =
        RecordMaterializer::new(workspace, Arc::clone(&restarted), Some(restarted_history));
    restarted_materializer
        .reconcile_repository(&log.repository)
        .unwrap();
    let restarted = restarted.lock().unwrap();
    assert_eq!(
        restarted.entry_at("moved.txt").map(|entry| entry.record_id),
        Some(moved_record)
    );
    assert_eq!(
        restarted
            .entry_at("docs/revived.txt")
            .map(|entry| entry.record_id),
        Some(revived_record)
    );
    assert!(restarted.entry_at("docs/deleted.txt").is_none());
    drop(restarted);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one adversarial scenario keeps denial, malformed kind, unknown tombstone and unchanged-tree assertions together"
)]
fn malformed_bindings_and_denied_writes_never_enter_the_tree() {
    struct DenyAll;
    impl crate::record_replication::RecordAuthorizer for DenyAll {
        fn actor_is_certified(&self, _: &FileActor) -> bool {
            true
        }
        fn allows(
            &self,
            _: &FileActor,
            _: FileRecordId,
            _: crate::record_replication::RecordAction,
        ) -> bool {
            false
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let workspace = Uuid::new_v4();
    let log = SignedLog::new(&temp, workspace, 91);
    let mut origin = Namespace::new("origin").unwrap();
    let root_record = origin.record_id_for(ROOT_INO).unwrap();
    let root_binding = origin.signed_binding_for(ROOT_INO, log.actor, 1).unwrap();
    log.apply(
        root_record,
        &[log.envelope(root_record, 1, RecordOperationV1::SetBinding(root_binding))],
    );

    let denied_record = FileRecordId::new();
    let denied_binding = FileBindingV1 {
        schema_version: FILE_BINDING_VERSION,
        record_id: denied_record,
        parent: Some(root_record),
        name: "denied.txt".into(),
        kind: FileNodeKind::File,
        tombstoned: false,
        actor: log.actor,
        observed_at_ms: 2,
    };
    let denied = log.envelope(
        denied_record,
        1,
        RecordOperationV1::SetBinding(denied_binding),
    );
    assert!(
        log.repository
            .apply(
                RecordRepositoryKey {
                    workspace_id: workspace,
                    record_id: denied_record,
                },
                &denied,
                &DenyAll,
            )
            .is_err()
    );

    let file = origin
        .create_file(
            ROOT_INO,
            "shape.txt",
            Some(&iroh_blobs::Hash::from([5; 32])),
            1,
            2,
        )
        .unwrap();
    let file_record = origin.record_id_for(file).unwrap();
    let file_binding = origin.signed_binding_for(file, log.actor, 2).unwrap();
    let (version, revision, metadata) = file_version(file_record, log.actor, 2, [5; 32], 1);
    log.apply(
        file_record,
        &create_record_envelopes(
            &log,
            file_record,
            file_binding.clone(),
            Some(version),
            Some(revision),
            Some(metadata),
        ),
    );
    let rebuilt = Arc::new(Mutex::new(Namespace::new("malformed").unwrap()));
    let history = Arc::new(FileHistoryStore::open(temp.path().join("history.json")).unwrap());
    let materializer =
        RecordMaterializer::new(workspace, Arc::clone(&rebuilt), Some(Arc::clone(&history)));
    let first = materializer.reconcile_repository(&log.repository).unwrap();
    assert_eq!(first.unknown_or_tombstoned_records, 0);
    assert_eq!(
        rebuilt
            .lock()
            .unwrap()
            .entry_at("shape.txt")
            .map(|entry| entry.kind),
        Some(NodeKind::File)
    );

    // A later signed binding cannot change the immutable kind of an
    // existing record. It is rejected at projection and the file stays.
    let wrong_kind = log.envelope(
        file_record,
        5,
        RecordOperationV1::SetBinding(FileBindingV1 {
            kind: FileNodeKind::Directory,
            observed_at_ms: 3,
            ..file_binding
        }),
    );
    log.apply(file_record, &[wrong_kind]);
    // An unknown record's tombstone is a valid no-op, not an unknown
    // record error.
    let unknown = FileRecordId::new();
    log.apply(
        unknown,
        &[log.envelope(
            unknown,
            1,
            RecordOperationV1::SetBinding(FileBindingV1 {
                schema_version: FILE_BINDING_VERSION,
                record_id: unknown,
                parent: Some(root_record),
                name: "never-existed.txt".into(),
                kind: FileNodeKind::File,
                tombstoned: true,
                actor: log.actor,
                observed_at_ms: 4,
            }),
        )],
    );
    let report = materializer.reconcile_repository(&log.repository).unwrap();
    assert!(report.unknown_or_tombstoned_records >= 1);
    let namespace = rebuilt.lock().unwrap();
    assert!(namespace.entry_at("denied.txt").is_none());
    assert!(namespace.entry_at("never-existed.txt").is_none());
    assert_eq!(
        namespace.entry_at("shape.txt").map(|entry| entry.kind),
        Some(NodeKind::File),
        "a kind-changing binding is rejected, not applied over the file"
    );
    assert_eq!(namespace.record_id_for(ROOT_INO), Some(root_record));
    // The immutable history of the file is still there after the rejected
    // binding changed only the structural projection.
    assert!(history.snapshot(file_record).is_some());
    drop(namespace);
}

#[derive(Clone, Copy, Debug)]
enum LaterParticipantChange {
    RenameSource,
    RestoreVictim,
    RenameAndEditSource,
}

#[test]
fn partial_supersession_keeps_transaction_participants_that_were_not_later_changed() {
    for later in [
        LaterParticipantChange::RenameSource,
        LaterParticipantChange::RestoreVictim,
        LaterParticipantChange::RenameAndEditSource,
    ] {
        partial_supersession_after_atomic_replacement_case(later);
    }
}

#[allow(
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "one end-to-end replacement scenario keeps its signed transaction, later per-record operation, restart and log-only rebuild assertions together"
)]
fn partial_supersession_after_atomic_replacement_case(later: LaterParticipantChange) {
    let temp = tempfile::tempdir().unwrap();
    let workspace = Uuid::new_v4();
    let log = SignedLog::new(&temp, workspace, 101);
    let mut origin = Namespace::new("partial-supersession-origin").unwrap();
    let source_ino = origin
        .create_file(
            ROOT_INO,
            "source.txt",
            Some(&iroh_blobs::Hash::from([1; 32])),
            1,
            1,
        )
        .unwrap();
    let victim_ino = origin
        .create_file(
            ROOT_INO,
            "destination.txt",
            Some(&iroh_blobs::Hash::from([2; 32])),
            1,
            1,
        )
        .unwrap();
    let root_record = origin.record_id_for(ROOT_INO).unwrap();
    let source_record = origin.record_id_for(source_ino).unwrap();
    let victim_record = origin.record_id_for(victim_ino).unwrap();
    let root_binding = origin.signed_binding_for(ROOT_INO, log.actor, 1).unwrap();
    let source_binding = origin.signed_binding_for(source_ino, log.actor, 1).unwrap();
    let victim_binding = origin.signed_binding_for(victim_ino, log.actor, 1).unwrap();
    let (source_version, source_revision, source_metadata) =
        file_version(source_record, log.actor, 1, [1; 32], 1);
    let (victim_version, victim_revision, victim_metadata) =
        file_version(victim_record, log.actor, 1, [2; 32], 1);
    log.apply(
        root_record,
        &[log.envelope(root_record, 1, RecordOperationV1::SetBinding(root_binding))],
    );
    log.apply(
        source_record,
        &create_record_envelopes(
            &log,
            source_record,
            source_binding,
            Some(source_version.clone()),
            Some(source_revision),
            Some(source_metadata),
        ),
    );
    log.apply(
        victim_record,
        &create_record_envelopes(
            &log,
            victim_record,
            victim_binding,
            Some(victim_version.clone()),
            Some(victim_revision),
            Some(victim_metadata),
        ),
    );

    let transaction_source_version = FileVersionV1 {
        id: FileVersionId::new(),
        record_id: source_record,
        parents: vec![source_version.id],
        actor: log.actor,
        timestamp_ms: 2,
        content_hash: Some(ContentHash([1; 32])),
        metadata_hash: ContentHash([3; 32]),
        provenance: FileProvenance::Rename,
    };
    let transaction_victim_version = FileVersionV1 {
        id: FileVersionId::new(),
        record_id: victim_record,
        parents: vec![victim_version.id],
        actor: log.actor,
        timestamp_ms: 2,
        content_hash: Some(ContentHash([2; 32])),
        metadata_hash: ContentHash([4; 32]),
        provenance: FileProvenance::Delete,
    };
    let participant = |record_id,
                       role,
                       expected_version,
                       binding: FileBindingV1,
                       version: FileVersionV1,
                       content_hash| {
        let mut participant = FileMutationParticipant {
            record_id,
            role,
            expected_version,
            proposed_binding: binding,
            proposed_content_revision: Some(ContentRevisionV1 {
                schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                record_id,
                version_id: version.id,
                content_hash: Some(content_hash),
                byte_len: 1,
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
            FileMutationParticipantRole::Source,
            source_version.id,
            FileBindingV1 {
                schema_version: FILE_BINDING_VERSION,
                record_id: source_record,
                parent: Some(root_record),
                name: "destination.txt".into(),
                kind: FileNodeKind::File,
                tombstoned: false,
                actor: log.actor,
                observed_at_ms: 2,
            },
            transaction_source_version.clone(),
            ContentHash([1; 32]),
        ),
        participant(
            victim_record,
            FileMutationParticipantRole::Replaced,
            victim_version.id,
            FileBindingV1 {
                schema_version: FILE_BINDING_VERSION,
                record_id: victim_record,
                parent: Some(root_record),
                name: "destination.txt".into(),
                kind: FileNodeKind::File,
                tombstoned: true,
                actor: log.actor,
                observed_at_ms: 2,
            },
            transaction_victim_version,
            ContentHash([2; 32]),
        ),
    ];
    participants.sort_by_key(|value| value.record_id);
    let transaction = FileMutationTransaction::signed(
        FileMutationTransactionManifest {
            schema_version: FILE_MUTATION_TRANSACTION_VERSION,
            transaction_id: Uuid::new_v4(),
            workspace_id: workspace,
            actor: log.actor,
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
        &log.device,
    )
    .unwrap();
    for record_id in [source_record, victim_record] {
        log.apply(
            record_id,
            &[
                log.envelope(
                    record_id,
                    5,
                    RecordOperationV1::PrepareTransaction(transaction.clone()),
                ),
                log.envelope(
                    record_id,
                    6,
                    RecordOperationV1::CommitTransaction {
                        transaction_id: transaction.manifest.transaction_id,
                    },
                ),
            ],
        );
    }

    let namespace = Arc::new(Mutex::new(Namespace::new("partial-supersession").unwrap()));
    let history_path = temp.path().join("partial-history.json");
    let history = Arc::new(FileHistoryStore::open(&history_path).unwrap());
    let materializer = RecordMaterializer::new(
        workspace,
        Arc::clone(&namespace),
        Some(Arc::clone(&history)),
    );
    let first = materializer.reconcile_repository(&log.repository).unwrap();
    assert_eq!(first.transactions_reconciled, 1);
    {
        let namespace = namespace.lock().unwrap();
        assert_eq!(
            namespace
                .entry_at("destination.txt")
                .map(|entry| entry.record_id),
            Some(source_record)
        );
        assert!(namespace.ino_for_record_id(victim_record).is_none());
    }

    let mut source_sequence: u64 = 6;
    let mut later_revision = None;
    match later {
        LaterParticipantChange::RenameSource => {
            source_sequence = source_sequence.saturating_add(1);
            log.apply(
                source_record,
                &[log.envelope(
                    source_record,
                    source_sequence,
                    RecordOperationV1::SetBinding(FileBindingV1 {
                        schema_version: FILE_BINDING_VERSION,
                        record_id: source_record,
                        parent: Some(root_record),
                        name: "renamed-source.txt".into(),
                        kind: FileNodeKind::File,
                        tombstoned: false,
                        actor: log.actor,
                        observed_at_ms: 20,
                    }),
                )],
            );
        }
        LaterParticipantChange::RestoreVictim => {
            log.apply(
                victim_record,
                &[log.envelope(
                    victim_record,
                    7,
                    RecordOperationV1::SetBinding(FileBindingV1 {
                        schema_version: FILE_BINDING_VERSION,
                        record_id: victim_record,
                        parent: Some(root_record),
                        name: "restored-victim.txt".into(),
                        kind: FileNodeKind::File,
                        tombstoned: false,
                        actor: log.actor,
                        observed_at_ms: 20,
                    }),
                )],
            );
        }
        LaterParticipantChange::RenameAndEditSource => {
            source_sequence = source_sequence.saturating_add(1);
            log.apply(
                source_record,
                &[log.envelope(
                    source_record,
                    source_sequence,
                    RecordOperationV1::SetBinding(FileBindingV1 {
                        schema_version: FILE_BINDING_VERSION,
                        record_id: source_record,
                        parent: Some(root_record),
                        name: "edited-source.txt".into(),
                        kind: FileNodeKind::File,
                        tombstoned: false,
                        actor: log.actor,
                        observed_at_ms: 20,
                    }),
                )],
            );
            let (mut version, revision, metadata) =
                file_version(source_record, log.actor, 21, [9; 32], 9);
            version.parents = vec![transaction_source_version.id];
            source_sequence = source_sequence.saturating_add(1);
            log.apply(
                source_record,
                &[log.envelope(
                    source_record,
                    source_sequence,
                    RecordOperationV1::AppendVersion(version),
                )],
            );
            source_sequence = source_sequence.saturating_add(1);
            log.apply(
                source_record,
                &[log.envelope(
                    source_record,
                    source_sequence,
                    RecordOperationV1::SetContentRevision(revision),
                )],
            );
            source_sequence = source_sequence.saturating_add(1);
            log.apply(
                source_record,
                &[log.envelope(
                    source_record,
                    source_sequence,
                    RecordOperationV1::SetMetadata(metadata),
                )],
            );
            later_revision = Some(revision);
        }
    }

    let incremental = materializer.reconcile_repository(&log.repository).unwrap();
    assert_eq!(incremental.transactions_reconciled, 1);
    {
        let mut namespace = namespace.lock().unwrap();
        match later {
            LaterParticipantChange::RenameSource => {
                assert_eq!(
                    namespace
                        .entry_at("renamed-source.txt")
                        .map(|entry| entry.record_id),
                    Some(source_record)
                );
                assert!(namespace.entry_at("destination.txt").is_none());
                assert!(namespace.ino_for_record_id(victim_record).is_none());
            }
            LaterParticipantChange::RestoreVictim => {
                assert_eq!(
                    namespace
                        .entry_at("destination.txt")
                        .map(|entry| entry.record_id),
                    Some(source_record)
                );
                assert_eq!(
                    namespace
                        .entry_at("restored-victim.txt")
                        .map(|entry| entry.record_id),
                    Some(victim_record)
                );
            }
            LaterParticipantChange::RenameAndEditSource => {
                let ino = namespace
                    .resolve_path_ino("edited-source.txt")
                    .expect("edited source stays at its later name");
                assert_eq!(
                    namespace.content_revision_for(ino),
                    later_revision,
                    "a later edit's content revision survives the transaction replay"
                );
                assert!(namespace.entry_at("destination.txt").is_none());
                assert!(namespace.ino_for_record_id(victim_record).is_none());
            }
        }
    }
    drop(namespace);

    // A restart from an empty namespace rebuilds the same current state from
    // the accepted record log alone, not from the previous namespace.
    let restarted = Arc::new(Mutex::new(
        Namespace::new("partial-supersession-restart").unwrap(),
    ));
    let restarted_history =
        Arc::new(FileHistoryStore::open(temp.path().join("restart-history.json")).unwrap());
    let restarted_materializer =
        RecordMaterializer::new(workspace, Arc::clone(&restarted), Some(restarted_history));
    restarted_materializer
        .reconcile_repository(&log.repository)
        .unwrap();
    let mut restarted = restarted.lock().unwrap();
    match later {
        LaterParticipantChange::RenameSource => {
            assert_eq!(
                restarted
                    .entry_at("renamed-source.txt")
                    .map(|entry| entry.record_id),
                Some(source_record)
            );
            assert!(restarted.entry_at("destination.txt").is_none());
            assert!(restarted.ino_for_record_id(victim_record).is_none());
        }
        LaterParticipantChange::RestoreVictim => {
            assert_eq!(
                restarted
                    .entry_at("destination.txt")
                    .map(|entry| entry.record_id),
                Some(source_record)
            );
            assert_eq!(
                restarted
                    .entry_at("restored-victim.txt")
                    .map(|entry| entry.record_id),
                Some(victim_record)
            );
        }
        LaterParticipantChange::RenameAndEditSource => {
            let ino = restarted
                .resolve_path_ino("edited-source.txt")
                .expect("log-only rebuild keeps the later name");
            assert_eq!(restarted.content_revision_for(ino), later_revision);
            assert!(restarted.entry_at("destination.txt").is_none());
            assert!(restarted.ino_for_record_id(victim_record).is_none());
        }
    }
}
