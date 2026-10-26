use super::*;

fn h(b: u8) -> Hash {
    Hash::from([b; 32])
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "verify two signed AtomicSave namespace publications, stable identity and durable replay"
)]
fn atomic_save_namespace_keeps_document_node_across_two_publications() {
    use ed25519_dalek::SigningKey;
    use fs_protocol::{
        ContentHash, FileMutationOperation, FileMutationParticipant,
        FileMutationTransactionManifest, FileProvenance, FileVersionId, FileVersionV1,
    };
    use getrandom::{SysRng, rand_core::UnwrapErr};

    let mut ns = Namespace::new("atomic-save").unwrap();
    let mut document_ino = ns.create_file(ROOT_INO, "report.txt", None, 1, 1).unwrap();
    let document = ns.record_id_for(document_ino).unwrap();
    let document_node = ns.id_for_ino(document_ino).unwrap().to_owned();
    let parent = ns.record_id_for(ROOT_INO).unwrap();
    let mut prior = FileVersionId::new();
    assert!(ns.set_content_revision(
        document_ino,
        &ContentRevisionV1 {
            schema_version: fs_protocol::CONTENT_REVISION_VERSION,
            record_id: document,
            version_id: prior,
            content_hash: None,
            byte_len: 1,
            metadata_hash: ContentHash([1; 32]),
        },
        1,
    ));
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let actor = FileActor {
        person: collab_types::Ed25519PublicKey([9; 32]),
        device: collab_types::Ed25519PublicKey(key.verifying_key().to_bytes()),
    };
    let workspace = Uuid::new_v4();
    for at in [2_u64, 3] {
        let temporary_ino = ns.create_file(ROOT_INO, "save-temp", None, 1, at).unwrap();
        let temporary = ns.record_id_for(temporary_ino).unwrap();
        let temp_prior = FileVersionId::new();
        assert!(ns.set_content_revision(
            temporary_ino,
            &ContentRevisionV1 {
                schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                record_id: temporary,
                version_id: temp_prior,
                content_hash: None,
                byte_len: at,
                metadata_hash: ContentHash([2; 32]),
            },
            at,
        ));
        let participant = |record_id, expected_version, role, name: &str| {
            let version = FileVersionV1 {
                id: FileVersionId::new(),
                record_id,
                parents: vec![expected_version],
                actor,
                timestamp_ms: at,
                content_hash: None,
                metadata_hash: ContentHash([u8::try_from(at).unwrap(); 32]),
                provenance: if role == FileMutationParticipantRole::AtomicSaveDocument {
                    FileProvenance::Edit
                } else {
                    FileProvenance::Delete
                },
            };
            let mut result = FileMutationParticipant {
                record_id,
                role,
                expected_version,
                proposed_binding: FileBindingV1 {
                    schema_version: FILE_BINDING_VERSION,
                    record_id,
                    parent: Some(parent),
                    name: name.into(),
                    kind: FileNodeKind::File,
                    tombstoned: role == FileMutationParticipantRole::AtomicSaveTemporary,
                    actor,
                    observed_at_ms: at,
                },
                proposed_content_revision: Some(ContentRevisionV1 {
                    schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                    record_id,
                    version_id: version.id,
                    content_hash: None,
                    byte_len: at,
                    metadata_hash: version.metadata_hash,
                }),
                proposed_version: version,
                projection_hash: ContentHash([0; 32]),
            };
            result.projection_hash = result.computed_projection_hash().unwrap();
            result
        };
        let mut participants = vec![
            participant(
                document,
                prior,
                FileMutationParticipantRole::AtomicSaveDocument,
                "report.txt",
            ),
            participant(
                temporary,
                temp_prior,
                FileMutationParticipantRole::AtomicSaveTemporary,
                "save-temp",
            ),
        ];
        participants.sort_by_key(|participant| participant.record_id);
        let transaction = FileMutationTransaction::signed(
            FileMutationTransactionManifest {
                schema_version: fs_protocol::files::FILE_MUTATION_TRANSACTION_VERSION_V4,
                transaction_id: Uuid::new_v4(),
                workspace_id: workspace,
                actor,
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
            &key,
        )
        .unwrap();
        let document_participant = transaction
            .manifest
            .participants
            .iter()
            .find(|participant| participant.record_id == document)
            .unwrap();
        assert_eq!(document_participant.proposed_version.parents, [prior]);
        let expected = document_participant.proposed_content_revision.unwrap();
        assert!(
            ns.apply_mutation_transaction(&transaction)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            ns.lookup(ROOT_INO, "report.txt").unwrap().ino.0,
            document_ino
        );
        assert_eq!(ns.record_id_for(document_ino), Some(document));
        assert_eq!(ns.content_revision_for(document_ino), Some(expected));
        assert!(ns.lookup(ROOT_INO, "save-temp").is_none());
        assert!(ns.ino_for_record_id(temporary).is_none());
        assert_eq!(ns.apply_mutation_transaction(&transaction), Ok(None));
        let saved = ns.save();
        ns = Namespace::from_bytes("atomic-save", &saved).unwrap();
        document_ino = ns.lookup(ROOT_INO, "report.txt").unwrap().ino.0;
        assert_eq!(ns.id_for_ino(document_ino), Some(document_node.as_str()));
        assert_eq!(ns.record_id_for(document_ino), Some(document));
        assert_eq!(ns.content_revision_for(document_ino), Some(expected));
        prior = expected.version_id;
    }
}

#[test]
fn tree_operations_create_move_trash_and_persist() {
    {
        let mut ns = Namespace::new("a").unwrap();
        assert_eq!(ns.id_for_ino(ROOT_INO), Some(ROOT_ID));
        assert!(ns.record_id_for(ROOT_INO).is_some());
        assert_eq!(ns.getattr(ROOT_INO).unwrap().kind, FileType::Directory);
        assert_eq!(ns.readdir(ROOT_INO).unwrap().len(), 2); // "." ".."
    }
    {
        let mut ns = Namespace::new("a").unwrap();
        let ino = ns
            .create_file(ROOT_INO, "hello.txt", Some(&h(7)), 12, 1)
            .unwrap();
        let attr = ns.lookup(ROOT_INO, "hello.txt").unwrap();
        assert_eq!(attr.ino, INodeNo(ino));
        assert_eq!(attr.size, 12);
        assert_eq!(ns.hash_for(ino), Some(h(7)));

        let dir = ns.create_dir(ROOT_INO, "docs", 1).unwrap();
        ns.create_file(dir, "a.txt", Some(&h(1)), 1, 1).unwrap();
        assert!(
            ns.readdir(dir)
                .unwrap()
                .into_iter()
                .any(|(_, _, name)| name == "a.txt")
        );
        let dotdot = ns
            .readdir(dir)
            .unwrap()
            .into_iter()
            .find(|(_, _, n)| n == "..")
            .unwrap();
        assert_eq!(dotdot.0, ROOT_INO);
    }
    {
        let mut ns = Namespace::new("a").unwrap();
        let dir = ns.create_dir(ROOT_INO, "d", 1).unwrap();
        let child = ns.create_file(dir, "c.txt", Some(&h(1)), 1, 1).unwrap();
        assert!(ns.remove(dir));
        assert!(ns.getattr(dir).is_none(), "removed dir not visible");
        assert!(
            ns.getattr(child).is_none(),
            "child follows parent into trash"
        );
        assert!(ns.lookup(ROOT_INO, "d").is_none());
    }
    {
        let mut ns = Namespace::new("a").unwrap();
        let ino = ns.create_file(ROOT_INO, "old", Some(&h(9)), 4, 1).unwrap();
        let record_id = ns.record_id_for(ino).unwrap();
        let dir = ns.create_dir(ROOT_INO, "sub", 1).unwrap();
        assert!(ns.rename(ROOT_INO, "old", dir, "new", 2));
        assert!(ns.lookup(ROOT_INO, "old").is_none());
        let moved = ns.lookup(dir, "new").unwrap();
        assert_eq!(moved.ino, INodeNo(ino), "inode preserved across move");
        assert_eq!(
            ns.record_id_for(moved.ino.0),
            Some(record_id),
            "rename and move preserve collaborative identity"
        );
        assert_eq!(ns.hash_for(ino), Some(h(9)));
    }
    {
        let mut ns = Namespace::new("a").unwrap();
        let d1 = ns.create_dir(ROOT_INO, "d1", 1).unwrap();
        let d2 = ns.create_dir(d1, "d2", 1).unwrap();
        assert!(
            !ns.rename(ROOT_INO, "d1", d2, "d1", 2),
            "cannot move d1 under d1/d2"
        );
        assert!(ns.lookup(ROOT_INO, "d1").is_some());
    }
    {
        let mut a = Namespace::new("a").unwrap();
        let dir = a.create_dir(ROOT_INO, "docs", 1).unwrap();
        a.create_file(dir, "f.txt", Some(&h(5)), 5, 1).unwrap();
        let bytes = a.save();

        let mut b = Namespace::from_bytes("a", &bytes).unwrap();
        let d = b.lookup(ROOT_INO, "docs").expect("dir restored");
        let f = b.lookup(d.ino.0, "f.txt").expect("nested file restored");
        assert_eq!(b.hash_for(f.ino.0), Some(h(5)));
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "The signed two-record fixture verifies source replacement, victim tombstoning, emitted namespace delta, and idempotent replay together."
)]
fn signed_bindings_and_replacements_are_verified_exact_and_fail_closed() {
    {
        let mut ns = Namespace::new("a").unwrap();
        let left = ns.create_dir(ROOT_INO, "left", 1).unwrap();
        let right = ns.create_dir(ROOT_INO, "right", 1).unwrap();
        let file = ns
            .create_file(left, "before.txt", Some(&h(1)), 1, 1)
            .unwrap();
        let occupied = ns
            .create_file(right, "occupied.txt", Some(&h(2)), 1, 1)
            .unwrap();
        let actor = FileActor {
            person: collab_types::Ed25519PublicKey([3; 32]),
            device: collab_types::Ed25519PublicKey([4; 32]),
        };
        let binding = FileBindingV1 {
            schema_version: FILE_BINDING_VERSION,
            record_id: ns.record_id_for(file).unwrap(),
            parent: Some(ns.record_id_for(right).unwrap()),
            name: "after.txt".into(),
            kind: FileNodeKind::File,
            tombstoned: false,
            actor,
            observed_at_ms: 2,
        };
        assert_eq!(ns.apply_signed_binding(&binding), Ok(true));
        assert_eq!(ns.lookup(right, "after.txt").unwrap().ino.0, file);
        assert_eq!(ns.apply_signed_binding(&binding), Ok(false));

        let occupied_binding = FileBindingV1 {
            name: "occupied.txt".into(),
            ..binding.clone()
        };
        let file_record = ns.record_id_for(file).unwrap();
        let occupied_record = ns.record_id_for(occupied).unwrap();
        assert_eq!(ns.apply_signed_binding(&occupied_binding), Ok(true));
        // Two stable records asked for one name. The smaller record id
        // keeps it; the other becomes a deterministic conflict sibling, so
        // both survive and every replay order converges on the same tree.
        let (winner, loser_record) = if file_record < occupied_record {
            (file, occupied_record)
        } else {
            (occupied, file_record)
        };
        assert_eq!(ns.lookup(right, "occupied.txt").unwrap().ino.0, winner);
        let conflict = structural_conflict_name(b"occupied.txt", loser_record);
        assert!(ns.lookup(right, conflict.as_bytes()).is_some());
        assert_eq!(ns.apply_signed_binding(&occupied_binding), Ok(false));

        let deleted = FileBindingV1 {
            tombstoned: true,
            ..binding
        };
        assert_eq!(ns.apply_signed_binding(&deleted), Ok(true));
        assert_eq!(ns.apply_signed_binding(&deleted), Ok(false));
        assert!(ns.lookup(right, "after.txt").is_none());
        // Deleting the winner releases the name the loser asked for.
        assert_eq!(ns.lookup(right, "occupied.txt").unwrap().ino.0, occupied);
    }
    // Phase `complete_replacement_transaction_moves_source_tombstones_victim_and_replays`.
    // This scenario intentionally covers every durable phase and replay of a
    // two-record replacement transaction in one linear assertion trace.
    {
        use ed25519_dalek::SigningKey;
        use fs_protocol::{
            ContentHash, FILE_MUTATION_TRANSACTION_VERSION, FileMutationOperation,
            FileMutationParticipant, FileMutationTransactionManifest, FileProvenance,
            FileVersionId, FileVersionV1,
        };
        use getrandom::{SysRng, rand_core::UnwrapErr};

        let mut ns = Namespace::new("a").unwrap();
        let source_ino = ns
            .create_file(ROOT_INO, "source.txt", Some(&h(1)), 1, 1)
            .unwrap();
        let victim_ino = ns
            .create_file(ROOT_INO, "destination.txt", Some(&h(2)), 1, 1)
            .unwrap();
        let source_record = ns.record_id_for(source_ino).unwrap();
        let victim_record = ns.record_id_for(victim_ino).unwrap();
        let root_record = ns.record_id_for(ROOT_INO).unwrap();
        let source_expected = FileVersionId::new();
        let victim_expected = FileVersionId::new();
        for (ino, record_id, version_id, byte) in [
            (source_ino, source_record, source_expected, 1_u8),
            (victim_ino, victim_record, victim_expected, 2_u8),
        ] {
            assert!(ns.set_content_revision(
                ino,
                &ContentRevisionV1 {
                    schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                    record_id,
                    version_id,
                    content_hash: Some(ContentHash([byte; 32])),
                    byte_len: 1,
                    metadata_hash: ContentHash([byte + 2; 32]),
                },
                1,
            ));
        }
        let key = SigningKey::generate(&mut UnwrapErr(SysRng));
        let actor = FileActor {
            person: collab_types::Ed25519PublicKey([9; 32]),
            device: collab_types::Ed25519PublicKey(key.verifying_key().to_bytes()),
        };
        let participant = |record_id, expected_version, role, content_byte| {
            let version = FileVersionV1 {
                id: FileVersionId::new(),
                record_id,
                parents: vec![expected_version],
                actor,
                timestamp_ms: 2,
                content_hash: Some(ContentHash([content_byte; 32])),
                metadata_hash: ContentHash([content_byte + 2; 32]),
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
                1,
            ),
            participant(
                victim_record,
                victim_expected,
                FileMutationParticipantRole::Replaced,
                2,
            ),
        ];
        participants.sort_by_key(|participant| participant.record_id);
        let transaction = fs_protocol::FileMutationTransaction::signed(
            FileMutationTransactionManifest {
                schema_version: FILE_MUTATION_TRANSACTION_VERSION,
                transaction_id: Uuid::new_v4(),
                workspace_id: Uuid::new_v4(),
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
        let delta = ns
            .apply_replacement_transaction(&transaction)
            .unwrap()
            .unwrap();
        assert!(!delta.is_empty());
        assert_eq!(
            ns.lookup(ROOT_INO, "destination.txt").unwrap().ino.0,
            source_ino
        );
        assert!(ns.lookup(ROOT_INO, "source.txt").is_none());
        assert!(ns.ino_for_record_id(victim_record).is_none());
        assert_eq!(ns.apply_replacement_transaction(&transaction), Ok(None));
    }
    {
        let mut ns = Namespace::new("owner").unwrap();
        let ino = ns
            .create_file(ROOT_INO, "verified.bin", Some(&h(1)), 1, 1)
            .unwrap();
        assert!(ns.set_thumb(ino, &h(2)));
        let record_id = ns.record_id_for(ino).unwrap();
        ns.enable_signed_content_authority();
        assert_eq!(ns.hash_for(ino), None);
        assert_eq!(ns.getattr(ino).unwrap().size, 0);
        assert_eq!(ns.thumb_hash(ino), None);
        assert!(ns.live_paths_for_blob(h(1)).is_empty());

        let revision = ContentRevisionV1 {
            schema_version: fs_protocol::CONTENT_REVISION_VERSION,
            record_id,
            version_id: fs_protocol::FileVersionId::new(),
            content_hash: Some(fs_protocol::ContentHash([3; 32])),
            byte_len: 3,
            metadata_hash: fs_protocol::ContentHash([4; 32]),
        };
        assert!(ns.install_verified_content_revision(revision));
        assert!(ns.install_verified_thumbnail(record_id, Some(h(4))));

        let id = ns.id_for_ino(ino).unwrap().to_string();
        ns.set_field(&id, "hash", hash_to_hex(&h(9)));
        ns.set_field(&id, "size", 999_u64);
        ns.set_field(&id, "thumb", hash_to_hex(&h(9)));
        let mut forged = revision;
        forged.version_id = fs_protocol::FileVersionId::new();
        forged.content_hash = Some(fs_protocol::ContentHash([9; 32]));
        forged.byte_len = 999;
        ns.set_field(
            &id,
            CONTENT_REVISION_KEY,
            ScalarValue::Bytes(revision_to_bytes(&forged).unwrap()),
        );

        assert_eq!(ns.hash_for(ino), Some(h(3)));
        assert_eq!(ns.getattr(ino).unwrap().size, 3);
        assert_eq!(ns.thumb_hash(ino), Some(h(4)));
        assert_eq!(ns.content_revision_for(ino), Some(revision));
        assert!(ns.live_paths_for_blob(h(9)).is_empty());
        assert_eq!(ns.live_paths_for_blob(h(3)), vec!["/verified.bin"]);
        assert!(!ns.set_content(ino, Some(&h(8)), 8, 8));
        assert!(!ns.set_thumb(ino, &h(8)));
    }
    {
        let mut ns = Namespace::new("owner").unwrap();
        let first = ns.create_file(ROOT_INO, "a", None, 0, 1).unwrap();
        let duplicate = ns.create_file(ROOT_INO, "b", None, 0, 1).unwrap();
        let record_id = ns.record_id_for(first).unwrap();
        let duplicate_id = ns.id_for_ino(duplicate).unwrap().to_string();
        let duplicate_obj = ns.node(&duplicate_id).unwrap();
        ns.doc
            .put(&duplicate_obj, RECORD_ID_KEY, record_id.0.to_string())
            .unwrap();
        ns.rebuild_record_index();
        assert!(
            ns.indexed_record(record_id).is_none(),
            "an ambiguous durable record identity must be withheld, not routed to an arbitrary node"
        );
        ns.enable_signed_content_authority();
        let revision = ContentRevisionV1 {
            schema_version: fs_protocol::CONTENT_REVISION_VERSION,
            record_id,
            version_id: fs_protocol::FileVersionId::new(),
            content_hash: Some(fs_protocol::ContentHash([3; 32])),
            byte_len: 3,
            metadata_hash: fs_protocol::ContentHash([4; 32]),
        };
        assert!(!ns.install_verified_content_revision(revision));
        assert_eq!(ns.hash_for(first), None);
        assert_eq!(ns.hash_for(duplicate), None);
    }
    {
        let mut ns = Namespace::new("recipient").unwrap();
        let record = FileRecordId::new();
        let ino = ns
            .create_projected_file(ROOT_INO, "shared.png", record, Some(&h(8)), 42, 9)
            .unwrap();
        assert_eq!(ns.record_id_for(ino), Some(record));
        assert_eq!(ns.hash_for(ino), Some(h(8)));
        assert_eq!(ns.getattr(ino).unwrap().size, 42);
        assert!(
            ns.create_projected_file(ROOT_INO, "alias.png", record, None, 0, 0)
                .is_none()
        );
        assert!(
            ns.create_projected_file(ROOT_INO, "shared.png", FileRecordId::new(), None, 0, 0)
                .is_none()
        );
        assert!(
            ns.create_projected_file(ROOT_INO, "../escape", FileRecordId::new(), None, 0, 0)
                .is_none()
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn record_ids_are_stable_persisted_backfilled_and_resolve_live_blobs() {
    {
        let mut original = Namespace::new("a").unwrap();
        let file = original
            .create_file(ROOT_INO, "stable.txt", Some(&h(1)), 1, 1)
            .unwrap();
        let root_record = original.record_id_for(ROOT_INO).unwrap();
        let file_record = original.record_id_for(file).unwrap();
        assert_ne!(root_record, file_record);

        let bytes = original.save();
        let mut reloaded = Namespace::from_bytes("a", &bytes).unwrap();
        let reloaded_file = reloaded.lookup(ROOT_INO, "stable.txt").unwrap().ino.0;
        assert_eq!(reloaded.record_id_for(ROOT_INO), Some(root_record));
        assert_eq!(reloaded.record_id_for(reloaded_file), Some(file_record));
        assert_eq!(
            reloaded.entry_at("stable.txt").unwrap().record_id,
            file_record
        );
    }
    {
        let mut legacy = Namespace::new("legacy").unwrap();
        legacy
            .create_file(ROOT_INO, "old.txt", Some(&h(2)), 2, 1)
            .unwrap();
        let ids: Vec<_> = legacy.doc.keys(&legacy.nodes).collect();
        let _ = legacy.doc.delete(&ROOT, ROOT_RECORD_ID_KEY);
        for id in ids {
            let node = legacy.node(&id).unwrap();
            let _ = legacy.doc.delete(&node, RECORD_ID_KEY);
        }
        let legacy_bytes = legacy.doc.save();

        let mut first = Namespace::from_bytes("first", &legacy_bytes).unwrap();
        let mut second = Namespace::from_bytes("second", &legacy_bytes).unwrap();
        let first_ino = first.lookup(ROOT_INO, "old.txt").unwrap().ino.0;
        let second_ino = second.lookup(ROOT_INO, "old.txt").unwrap().ino.0;
        assert_eq!(
            first.record_id_for(ROOT_INO),
            second.record_id_for(ROOT_INO),
            "root migration must not depend on the loading peer"
        );
        assert_eq!(
            first.record_id_for(first_ino),
            second.record_id_for(second_ino),
            "node migration must not depend on the loading peer"
        );

        let expected_root = first.record_id_for(ROOT_INO).unwrap();
        let expected_file = first.record_id_for(first_ino).unwrap();
        let migrated_bytes = first.save();
        let mut persisted = Namespace::from_bytes("third", &migrated_bytes).unwrap();
        let persisted_ino = persisted.lookup(ROOT_INO, "old.txt").unwrap().ino.0;
        assert_eq!(persisted.record_id_for(ROOT_INO), Some(expected_root));
        assert_eq!(persisted.record_id_for(persisted_ino), Some(expected_file));
    }
    {
        let mut ns = Namespace::new("a").unwrap();
        let original = ns.create_file(ROOT_INO, "doc", Some(&h(1)), 1, 1).unwrap();
        let original_record = ns.record_id_for(original).unwrap();
        assert!(ns.create_conflict_copy(ROOT_ID, "doc.sync-conflict", &h(2), 1, 2));
        let copy = ns.resolve_path_ino("doc.sync-conflict").unwrap();
        assert_ne!(ns.record_id_for(copy), Some(original_record));
        assert!(ns.record_id_for(copy).is_some());
    }
    {
        let mut ns = Namespace::new("a").unwrap();
        ns.create_file(ROOT_INO, "x", Some(&h(1)), 1, 1).unwrap();
        let mut copy = ns.clone_doc();
        assert!(copy.record_ids_complete);
        let copied = copy.lookup(ROOT_INO, "x").unwrap().ino.0;
        let original = ns.lookup(ROOT_INO, "x").unwrap().ino.0;
        assert_eq!(copy.record_id_for(copied), ns.record_id_for(original));
    }
    {
        let mut ns = Namespace::new("owner").unwrap();
        let a = ns.create_dir(ROOT_INO, "a", 1).unwrap();
        let b = ns.create_dir(ROOT_INO, "b", 1).unwrap();
        let a_id = ns.id_for_ino(a).unwrap().to_string();
        let b_id = ns.id_for_ino(b).unwrap().to_string();
        ns.set_field(&a_id, "parent", b_id.clone());
        ns.set_field(&b_id, "parent", a_id);

        ns.rebuild_record_index();
        assert_eq!(
            ns.indexed_record_count(),
            0,
            "unreachable cyclic nodes must be withheld rather than making a rebuild non-terminating"
        );
    }
    {
        let mut ns = Namespace::new("a").unwrap();
        let first = ns
            .create_file(ROOT_INO, "first", Some(&h(1)), 4, 1)
            .unwrap();
        let second = ns
            .create_file(ROOT_INO, "second", Some(&h(9)), 4, 1)
            .unwrap();
        let first_record = ns.record_id_for(first).unwrap();
        let second_record = ns.record_id_for(second).unwrap();
        assert!(ns.set_thumb(second, &h(1)));
        assert_eq!(
            ns.live_record_ids_for_direct_blob(h(1), 8).unwrap(),
            vec![first_record, second_record]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        );
        assert_eq!(
            ns.live_record_ids_for_direct_blob(h(1), 1),
            Err(LiveRecordBlobScanError::LimitExceeded { limit: 1 })
        );

        assert!(ns.remove(first));
        assert_eq!(
            ns.live_record_ids_for_direct_blob(h(1), 8).unwrap(),
            vec![second_record]
        );
        assert!(
            ns.live_record_ids_for_direct_blob(h(77), 8)
                .unwrap()
                .is_empty()
        );
    }
    {
        let mut ns = Namespace::new("a").unwrap();
        let docs = ns.create_dir(ROOT_INO, "docs", 1).unwrap();
        let ino = ns.create_file(docs, "old.png", Some(&h(1)), 4, 1).unwrap();
        assert!(ns.set_thumb(ino, &h(2)));

        assert_eq!(ns.live_paths_for_blob(h(1)), vec!["/docs/old.png"]);
        assert_eq!(ns.live_paths_for_blob(h(2)), vec!["/docs/old.png"]);

        assert!(ns.rename(docs, "old.png", ROOT_INO, "new.png", 2));
        assert_eq!(ns.live_paths_for_blob(h(1)), vec!["/new.png"]);
        assert_eq!(ns.live_paths_for_blob(h(2)), vec!["/new.png"]);

        assert!(ns.remove(ino));
        assert!(ns.live_paths_for_blob(h(1)).is_empty());
        assert!(ns.live_paths_for_blob(h(2)).is_empty());
    }
}

#[test]
fn content_revisions_are_never_torn_and_legacy_writes_supersede_explicitly() {
    {
        let mut a = Namespace::new("a").unwrap();
        let ino = a
            .create_file(ROOT_INO, "atomic.bin", Some(&h(1)), 1, 1)
            .unwrap();
        let record_id = a.record_id_for(ino).unwrap();
        let bytes = a.save();
        let mut b = Namespace::from_bytes("b", &bytes).unwrap();
        let bino = b.lookup(ROOT_INO, "atomic.bin").unwrap().ino.0;
        let revision_a = ContentRevisionV1 {
            schema_version: fs_protocol::CONTENT_REVISION_VERSION,
            record_id,
            version_id: fs_protocol::FileVersionId::new(),
            content_hash: Some(fs_protocol::ContentHash([10; 32])),
            byte_len: 10,
            metadata_hash: fs_protocol::ContentHash([11; 32]),
        };
        let revision_b = ContentRevisionV1 {
            schema_version: fs_protocol::CONTENT_REVISION_VERSION,
            record_id,
            version_id: fs_protocol::FileVersionId::new(),
            content_hash: Some(fs_protocol::ContentHash([20; 32])),
            byte_len: 20,
            metadata_hash: fs_protocol::ContentHash([21; 32]),
        };
        assert!(a.set_content_revision(ino, &revision_a, 2));
        assert!(b.set_content_revision(bino, &revision_b, 2));
        a.merge_doc(&mut b).unwrap();

        let winner = a.content_revision_for(ino).unwrap();
        assert!(winner == revision_a || winner == revision_b);
        let entry = a.entry_at("atomic.bin").unwrap();
        assert_eq!(
            entry.hash,
            winner.content_hash.map(|hash| Hash::from(hash.0))
        );
        assert_eq!(entry.size, winner.byte_len);
        let id = a.id_for_ino(ino).unwrap();
        let conflicts = a.content_revision_conflicts(id);
        assert_eq!(conflicts.len(), 2);
        assert!(conflicts.iter().any(|(_, value)| value == &revision_a));
        assert!(conflicts.iter().any(|(_, value)| value == &revision_b));
    }
    {
        let mut ns = Namespace::new("a").unwrap();
        let ino = ns
            .create_file(ROOT_INO, "compat.bin", Some(&h(1)), 1, 1)
            .unwrap();
        let revision = ContentRevisionV1 {
            schema_version: fs_protocol::CONTENT_REVISION_VERSION,
            record_id: ns.record_id_for(ino).unwrap(),
            version_id: fs_protocol::FileVersionId::new(),
            content_hash: None,
            byte_len: 0,
            metadata_hash: fs_protocol::ContentHash([7; 32]),
        };
        assert!(ns.set_content_revision(ino, &revision, 2));
        assert_eq!(ns.hash_for(ino), None, "None must not resurrect the mirror");
        assert!(ns.set_content(ino, Some(&h(3)), 3, 3));
        assert_eq!(ns.content_revision_for(ino), None);
        assert_eq!(ns.hash_for(ino), Some(h(3)));
        assert_eq!(ns.getattr(ino).unwrap().size, 3);
    }
}

#[test]
fn byte_names_survive_load_save_rename_and_replication_exactly() {
    {
        let mut legacy = Namespace::new("legacy-name").unwrap();
        let ino = legacy
            .create_file(ROOT_INO, "placeholder", None, 0, 1)
            .unwrap();
        let id = legacy.id_for_ino(ino).unwrap().to_string();
        let node = legacy.node(&id).unwrap();
        legacy
            .doc
            .put(&node, "name", ScalarValue::Str("café.txt".into()))
            .unwrap();

        let mut migrated = Namespace::from_bytes("migrated", &legacy.doc.save()).unwrap();
        assert!(migrated.lookup(ROOT_INO, "café.txt").is_some());
        assert!(migrated.rename(ROOT_INO, "café.txt", ROOT_INO, b"caf\xc3\xa9-final.txt", 2,));

        let node = migrated.node(&id).unwrap();
        assert!(matches!(
            migrated.doc.get(&node, "name").unwrap(),
            Some((Value::Scalar(value), _))
                if matches!(value.as_ref(), ScalarValue::Bytes(bytes) if bytes.as_slice() == b"caf\xc3\xa9-final.txt")
        ));
        let mut reopened = Namespace::from_bytes("reopened", &migrated.save()).unwrap();
        assert!(
            reopened
                .lookup(ROOT_INO, b"caf\xc3\xa9-final.txt")
                .is_some(),
            "legacy UTF-8 identity must survive byte-scalar migration and restart"
        );
    }
    {
        let mut ns = Namespace::new("bytes").unwrap();
        let original = b"photo-\xFF.png";
        let renamed = b"photo-\xFE.png";
        let ino = ns.create_file(ROOT_INO, original, None, 0, 1).unwrap();
        assert_eq!(ns.lookup(ROOT_INO, original).unwrap().ino.0, ino);
        assert!(
            ns.readdir(ROOT_INO)
                .unwrap()
                .iter()
                .any(|entry| entry.2.as_bytes() == original)
        );

        let snapshot = ns.save();
        let mut reopened = Namespace::from_bytes("reopened", &snapshot).unwrap();
        let reopened_ino = reopened.lookup(ROOT_INO, original).unwrap().ino.0;
        let record_id = reopened.record_id_for(reopened_ino).unwrap();
        assert!(reopened.rename(ROOT_INO, original, ROOT_INO, renamed, 2));
        assert!(reopened.lookup(ROOT_INO, original).is_none());
        assert_eq!(
            reopened.lookup(ROOT_INO, renamed).unwrap().ino.0,
            reopened_ino
        );

        let renamed_snapshot = reopened.save();
        let mut final_open = Namespace::from_bytes("final", &renamed_snapshot).unwrap();
        assert!(final_open.lookup(ROOT_INO, original).is_none());
        let final_ino = final_open.lookup(ROOT_INO, renamed).unwrap().ino.0;
        assert_eq!(final_open.record_id_for(final_ino), Some(record_id));
    }
    {
        let name = b"replicated-\xFF.bin";
        let mut source = Namespace::new("source").unwrap();
        source.create_file(ROOT_INO, name, None, 0, 1).unwrap();
        let snapshot = source.save();
        let mut peer = Namespace::from_bytes("peer", &snapshot).unwrap();
        assert!(peer.lookup(ROOT_INO, name).is_some());
        assert!(
            peer.readdir(ROOT_INO)
                .unwrap()
                .iter()
                .any(|entry| entry.2.as_bytes() == name)
        );

        let renamed = b"replicated-\xFE.bin";
        assert!(source.rename(ROOT_INO, name, ROOT_INO, renamed, 2));
        let delta = source.save_incremental();
        peer.apply_remote_and_resolve(&delta).unwrap();
        assert!(peer.lookup(ROOT_INO, name).is_none());
        assert!(peer.lookup(ROOT_INO, renamed).is_some());
    }
}

#[test]
fn a_batch_of_changes_is_one_delta_even_across_a_full_save() {
    let mut source = Namespace::new("source").unwrap();
    source.create_file(ROOT_INO, "before", None, 0, 1).unwrap();
    let mut peer = Namespace::from_bytes("peer", &source.save()).unwrap();
    source.create_file(ROOT_INO, "pending", None, 0, 1).unwrap();
    source.begin_incremental_batch();
    source.create_file(ROOT_INO, "one", None, 0, 2).unwrap();
    assert!(
        source.save_incremental().is_empty(),
        "no delta inside a batch"
    );
    let _ = source.save();
    source.create_file(ROOT_INO, "two", None, 0, 3).unwrap();
    let delta = source.end_incremental_batch();
    peer.apply_remote_and_resolve(&delta).unwrap();
    for name in ["pending", "one", "two"] {
        assert!(peer.lookup(ROOT_INO, name).is_some(), "{name} arrived");
    }
    assert!(source.save_incremental().is_empty(), "the batch was taken");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn concurrent_namespace_changes_merge_and_converge() {
    {
        let mut a = Namespace::new("a").unwrap();
        let bytes = a.save();
        let mut b = Namespace::from_bytes("b", &bytes).unwrap();
        a.create_file(ROOT_INO, "from_a", Some(&h(1)), 1, 1)
            .unwrap();
        b.create_file(ROOT_INO, "from_b", Some(&h(2)), 1, 1)
            .unwrap();
        a.merge_doc(&mut b).unwrap();
        assert!(a.lookup(ROOT_INO, "from_a").is_some());
        assert!(a.lookup(ROOT_INO, "from_b").is_some());
    }
    {
        let mut a = Namespace::new("a").unwrap();
        let xino = a.create_dir(ROOT_INO, "X", 1).unwrap();
        let yino = a.create_dir(ROOT_INO, "Y", 1).unwrap();
        let snap = a.save();
        let mut b = Namespace::from_bytes("b", &snap).unwrap();

        // A moves X under Y; B moves Y under X (concurrently).
        let a_y = a.lookup(ROOT_INO, "Y").unwrap().ino.0;
        a.rename(ROOT_INO, "X", a_y, "X", 5);
        let b_x = b.lookup(ROOT_INO, "X").unwrap().ino.0;
        b.rename(ROOT_INO, "Y", b_x, "Y", 5);
        let _ = (xino, yino);

        let mut a2 = Namespace::from_bytes("a", &a.save()).unwrap();
        let mut b2 = Namespace::from_bytes("b", &b.save()).unwrap();
        let _ = a2.merge_doc(&mut b2.clone_doc());
        let _ = b2.merge_doc(&mut a.clone_doc());
        a2.resolve();
        b2.resolve();

        assert!(
            a2.find_cycle().is_none(),
            "a side must be acyclic after resolve"
        );
        assert!(
            b2.find_cycle().is_none(),
            "b side must be acyclic after resolve"
        );

        let xid = a2
            .live_node_ids()
            .into_iter()
            .find(|i| a2.name_of(i).as_deref() == Some(b"X"));
        let yid = a2
            .live_node_ids()
            .into_iter()
            .find(|i| a2.name_of(i).as_deref() == Some(b"Y"));
        assert!(
            xid.is_some() && yid.is_some(),
            "both dirs survive resolution"
        );

        for id in a2.live_node_ids() {
            assert_eq!(
                a2.parent_of(&id),
                b2.parent_of(&id),
                "peers must converge on parent of {id}"
            );
        }
    }
    {
        let mut a = Namespace::new("a").unwrap();
        let dino = a.create_dir(ROOT_INO, "d", 1).unwrap();
        let snap = a.save();
        let mut b = Namespace::from_bytes("b", &snap).unwrap();

        a.remove(dino);
        let b_d = b.lookup(ROOT_INO, "d").unwrap().ino.0;
        b.create_file(b_d, "new.txt", Some(&h(3)), 3, 1).unwrap();

        let _ = a.merge_doc(&mut b);
        a.resolve();

        assert!(a.lookup(ROOT_INO, "d").is_none(), "d stays deleted");
        let new_id = a
            .doc
            .keys(&a.nodes)
            .find(|i| a.name_of(i).as_deref() == Some(b"new.txt"));
        assert!(
            new_id.is_some(),
            "the node still exists in the doc (not lost)"
        );
        assert!(
            !a.reaches_root(&new_id.unwrap()),
            "but it is not reachable from root (consistently trashed, not orphaned)"
        );
    }
    {
        let mut a = Namespace::new("a").unwrap();
        let snap = a.save();
        let mut b = Namespace::from_bytes("b", &snap).unwrap();
        let _ = a.save_incremental();
        a.create_file(ROOT_INO, "delta.txt", Some(&h(4)), 7, 1)
            .unwrap();
        let delta = a.save_incremental();
        b.apply_remote_and_resolve(&delta).unwrap();
        assert_eq!(b.lookup(ROOT_INO, "delta.txt").unwrap().size, 7);
    }
    {
        // Diverge from a shared base doc; the incremental sync ping-pong must converge them.
        let mut a = Namespace::new("a").unwrap();
        a.create_file(ROOT_INO, "base", Some(&h(1)), 1, 1).unwrap();
        let mut b = Namespace::from_bytes("b", &a.save()).unwrap();

        a.create_file(ROOT_INO, "from_a", Some(&h(2)), 1, 1)
            .unwrap();
        b.create_file(ROOT_INO, "from_b", Some(&h(3)), 1, 1)
            .unwrap();

        let mut sa = automerge::sync::State::new();
        let mut sb = automerge::sync::State::new();
        for _ in 0..20 {
            let ma = a.sync_generate(&mut sa);
            if let Some(m) = &ma {
                b.sync_receive(&mut sb, m).unwrap();
            }
            let mb = b.sync_generate(&mut sb);
            if let Some(m) = &mb {
                a.sync_receive(&mut sa, m).unwrap();
            }
            if ma.is_none() && mb.is_none() {
                break;
            }
        }
        assert!(a.lookup(ROOT_INO, "from_b").is_some(), "a got b's change");
        assert!(b.lookup(ROOT_INO, "from_a").is_some(), "b got a's change");
    }
}

#[test]
fn documents_import_export_and_merge_semantically() {
    {
        let mut ns = Namespace::new("a").unwrap();
        let ino = ns.create_document(ROOT_INO, "note.md", "md", 1).unwrap();
        assert_eq!(ns.document_codec(ino).as_deref(), Some("md"));

        let md = b"# Title\n\nHello world\n";
        assert!(ns.import_document(ino, md, 2, &tilo_content::ContentStore::memory()));
        let out = ns
            .export_document(ino, &tilo_content::NoContent)
            .expect("export");
        let s = String::from_utf8_lossy(&out);
        assert!(
            s.contains("Title") && s.contains("Hello world"),
            "round-trip lost content: {s}"
        );
    }
    {
        let mut ns = Namespace::new("a").unwrap();
        let ino = ns.create_document(ROOT_INO, "doc.md", "md", 1).unwrap();
        assert!(ns.import_document(
            ino,
            b"# Keep\n\nimportant\n",
            2,
            &tilo_content::ContentStore::memory()
        ));
        let before = ns.export_document(ino, &tilo_content::NoContent).unwrap();

        // invalid UTF-8 is rejected by the markdown codec
        assert!(
            !ns.import_document(
                ino,
                &[0xff, 0xfe, 0x00],
                3,
                &tilo_content::ContentStore::memory()
            ),
            "malformed import must be rejected"
        );
        let after = ns.export_document(ino, &tilo_content::NoContent).unwrap();
        assert_eq!(
            before, after,
            "existing document must survive a rejected import"
        );
    }
    {
        // The point of model B: concurrent edits to the same document merge at the text level, not a conflict copy.
        let mut a = Namespace::new("a").unwrap();
        let ino = a.create_document(ROOT_INO, "doc.md", "md", 1).unwrap();
        let bid = a.document_insert_block(ino, None, "paragraph").unwrap();
        a.document_splice(ino, &bid, 0, 0, "Hello world");

        let snap = a.save();
        let mut b = Namespace::from_bytes("b", &snap).unwrap();
        let bino = b.lookup(ROOT_INO, "doc.md").unwrap().ino.0;

        // concurrent in-place edits to the same paragraph
        a.document_splice(ino, &bid, 5, 0, " dear");
        b.document_splice(bino, &bid, 11, 0, " (B)");

        a.merge_doc(&mut b).unwrap();
        let text = a.document_block_text(ino, &bid).unwrap();
        assert!(text.contains("dear"), "A's edit must survive: {text}");
        assert!(
            text.contains("(B)"),
            "B's edit must survive (text-level merge): {text}"
        );
    }
}

#[test]
fn saving_a_document_keeps_its_block_ids() {
    let mut ns = Namespace::new("a").unwrap();
    let ino = ns.create_document(ROOT_INO, "note.md", "md", 1).unwrap();
    assert!(ns.import_document(
        ino,
        b"# Title\n\nHello world\n",
        2,
        &tilo_content::ContentStore::memory()
    ));
    let ids = |ns: &Namespace| -> Vec<String> {
        ns.document_snapshot(ino)
            .map(|snap| snap.blocks.iter().map(|block| block.id.clone()).collect())
            .unwrap_or_default()
    };
    let before = ids(&ns);
    assert!(!before.is_empty(), "the save must have written blocks");

    // A second save of the same file: nothing changed, so nothing may be
    // rewritten — every block keeps the id it had.
    assert!(ns.import_document(
        ino,
        b"# Title\n\nHello world\n",
        3,
        &tilo_content::ContentStore::memory()
    ));
    let after = ids(&ns);
    assert_eq!(before, after, "a save must not re-mint block ids");
}

#[test]
fn saving_a_document_keeps_a_peer_concurrent_edit() {
    let mut a = Namespace::new("a").unwrap();
    let ino = a.create_document(ROOT_INO, "doc.md", "md", 1).unwrap();
    assert!(a.import_document(
        ino,
        b"first\n\nsecond\n",
        2,
        &tilo_content::ContentStore::memory()
    ));
    let second = a
        .document_snapshot(ino)
        .and_then(|snap| snap.blocks.get(1).map(|block| block.id.clone()))
        .unwrap_or_default();
    assert!(!second.is_empty(), "the file must have written two blocks");

    let snap = a.save();
    let mut b = Namespace::from_bytes("b", &snap).unwrap();
    let bino = b.lookup(ROOT_INO, "doc.md").unwrap().ino.0;
    // B edits the second paragraph in place while A is away.
    b.document_splice(bino, &second, 6, 0, " paragraph");

    // A saves the file again, from its own stale copy: the whole-document
    // write must merge into B's edit rather than replace the document.
    assert!(a.import_document(
        ino,
        b"first\n\nsecond\n",
        3,
        &tilo_content::ContentStore::memory()
    ));
    a.merge_doc(&mut b).unwrap();
    let text = a.document_block_text(ino, &second).unwrap_or_default();
    assert!(
        text.contains("paragraph"),
        "a save must not lose a peer's concurrent edit: {text}"
    );
}

#[test]
fn record_indexes_track_structure_and_page_tails_directly() {
    {
        let mut ns = Namespace::new("a").unwrap();
        ns.record_index.clear();
        ns.record_index_order.clear();
        for value in 1..=100_001_u128 {
            let record_id = FileRecordId(Uuid::from_u128(value));
            ns.record_index.insert(
                record_id,
                IndexedRecord {
                    record_id,
                    node_id: format!("synthetic/{value}"),
                    parent_id: ROOT_ID.into(),
                    path: format!("/synthetic-{value}"),
                    is_dir: false,
                },
            );
            ns.record_index_order.push(record_id);
        }
        let before = ns.record_index_rebuild_count();
        let page = ns.indexed_record_page(100_000, 128);
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].path, "/synthetic-100001");
        assert_eq!(ns.record_index_rebuild_count(), before);
    }
    {
        let mut a = Namespace::new("a").unwrap();
        let dir = a.create_dir(ROOT_INO, "dir", 1).unwrap();
        for i in 0..50_u64 {
            let parent = if i % 3 == 0 { dir } else { ROOT_INO };
            a.create_file(parent, format!("f{i}"), Some(&h(1)), 1, i)
                .unwrap();
        }
        assert_indexes_match_rebuild(&a);
        assert!(a.rename(ROOT_INO, "f1", dir, "moved", 2));
        assert!(
            a.rename(dir, "f3", dir, "f6", 3),
            "rename over an existing name"
        );
        let gone = a.lookup(ROOT_INO, "f2").unwrap().ino.0;
        assert!(a.remove(gone));
        assert_indexes_match_rebuild(&a);
        assert!(a.lookup(ROOT_INO, "f1").is_none());
        assert!(a.lookup(dir, "moved").is_some());
        assert!(a.lookup(ROOT_INO, "f2").is_none());

        // Concurrent same-name creates on two replicas: the lowest id wins
        // the name, exactly as the unindexed scan chose.
        let mut b = a.clone_doc();
        b.author = "b".into();
        a.create_file(ROOT_INO, "same", Some(&h(2)), 1, 9).unwrap();
        b.create_file(ROOT_INO, "same", Some(&h(3)), 1, 9).unwrap();
        let delta = b.save_incremental();
        a.apply_remote_and_resolve(&delta).unwrap();
        assert_indexes_match_rebuild(&a);
        let winner = a.lookup(ROOT_INO, "same").unwrap().ino.0;
        let mut same: Vec<String> = a
            .doc
            .keys(&a.nodes)
            .filter(|id| a.name_of(id).is_some_and(|name| name.as_bytes() == b"same"))
            .collect();
        same.sort();
        assert_eq!(a.id_for_ino(winner), same.first().map(String::as_str));
        let names: Vec<_> = a
            .readdir(ROOT_INO)
            .unwrap()
            .into_iter()
            .map(|(_, _, name)| name)
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }
}

/// The incrementally maintained structural index and record digest must
/// equal a from-scratch rebuild after every kind of local and merged
/// placement change.
fn assert_indexes_match_rebuild(ns: &Namespace) {
    let mut rebuilt = ns.clone();
    rebuilt.rebuild_record_index();
    assert_eq!(ns.tree.placement, rebuilt.tree.placement);
    assert_eq!(ns.tree.children, rebuilt.tree.children);
    assert_eq!(ns.record_index_digest, rebuilt.record_index_digest);
    assert_eq!(ns.record_index_revision, rebuilt.record_index_revision);
}

/// `inode_view` is the one-lock read FUSE `read` and `getattr` are served
/// from. It must answer exactly what the separate accessors it replaced
/// answer, through remote renames, moves, content changes and deletes
/// applied by the production merge path.
#[test]
fn inode_view_matches_the_separate_accessors_after_remote_apply() {
    let mut source = Namespace::new("source").unwrap();
    let dir = source.create_dir(ROOT_INO, "dir", 1).unwrap();
    source
        .create_file(dir, "a.txt", Some(&h(1)), 10, 1)
        .unwrap();
    let b = source
        .create_file(ROOT_INO, "b.txt", Some(&h(2)), 20, 1)
        .unwrap();
    let gone = source
        .create_file(ROOT_INO, "gone.txt", Some(&h(3)), 30, 1)
        .unwrap();
    source
        .create_document(ROOT_INO, "note.md", "md", 1)
        .unwrap();
    let mut peer = Namespace::from_bytes("peer", &source.save()).unwrap();
    let peer_dir = peer.lookup(ROOT_INO, "dir").unwrap().ino.0;
    let peer_a = peer.lookup(peer_dir, "a.txt").unwrap().ino.0;
    let peer_b = peer.lookup(ROOT_INO, "b.txt").unwrap().ino.0;
    let peer_gone = peer.lookup(ROOT_INO, "gone.txt").unwrap().ino.0;
    let peer_note = peer.lookup(ROOT_INO, "note.md").unwrap().ino.0;
    let inos = [
        ROOT_INO, peer_dir, peer_a, peer_b, peer_gone, peer_note, 9_999,
    ];
    for ino in inos {
        assert_view_matches_accessors(&peer, ino);
    }

    assert!(source.rename(dir, "a.txt", ROOT_INO, "moved.txt", 2));
    assert!(source.set_content(b, Some(&h(4)), 40, 2));
    assert!(source.remove(gone));
    peer.apply_remote_and_resolve(&source.save_incremental())
        .unwrap();
    for ino in inos {
        assert_view_matches_accessors(&peer, ino);
    }

    let live = InodeQuery {
        unlinked: false,
        path: true,
    };
    let moved = peer.inode_view(peer_a, live).unwrap();
    assert_eq!(moved.path.as_deref(), Some("/moved.txt"));
    let changed = peer.inode_view(peer_b, live).unwrap();
    assert_eq!((changed.hash, changed.attr.size), (Some(h(4)), 40));
    assert!(peer.inode_view(peer_gone, live).is_none());
    let retained = peer
        .inode_view(
            peer_gone,
            InodeQuery {
                unlinked: true,
                path: true,
            },
        )
        .unwrap();
    assert!(!retained.live);
    assert_eq!(retained.path, None, "a trashed inode has no live path");
    let note = peer.inode_view(peer_note, live).unwrap();
    assert!(note.document);
}

fn assert_view_matches_accessors(ns: &Namespace, ino: u64) {
    for unlinked in [false, true] {
        for path in [false, true] {
            let view = ns.inode_view(ino, InodeQuery { unlinked, path });
            let attr = if unlinked {
                ns.getattr_including_unlinked(ino)
            } else {
                ns.getattr(ino)
            };
            assert_eq!(view.as_ref().map(|v| v.attr), attr, "attr of {ino}");
            let Some(view) = view else {
                continue;
            };
            assert_eq!(view.live, ns.getattr(ino).is_some(), "liveness of {ino}");
            assert_eq!(view.hash, ns.hash_for(ino), "hash of {ino}");
            let expected_path = if path { ns.path_for(ino) } else { None };
            assert_eq!(view.path, expected_path, "path of {ino}");
            assert_eq!(
                view.document,
                ns.document_codec(ino).is_some()
                    && ns.content_authority() == ContentAuthority::LegacyNamespace,
                "document of {ino}"
            );
        }
    }
}

const CLIENT: [u8; 32] = [1; 32];
const SERVER: [u8; 32] = [2; 32];

/// One exchange as a session runs it (the client writes, the server
/// answers, until neither has anything to send), each side beginning
/// where its last completed exchange with the other left off. Returns
/// the sync bytes both sides generated.
fn exchange(client: &mut Namespace, server: &mut Namespace) -> usize {
    let before = client
        .sync_bytes_generated()
        .saturating_add(server.sync_bytes_generated());
    let mut client_state = client.begin_sync_with(&SERVER);
    let mut server_state = server.begin_sync_with(&CLIENT);
    let mut completed = false;
    for _ in 0..64 {
        let message = client.sync_generate(&mut client_state);
        if let Some(message) = &message {
            server.sync_receive(&mut server_state, message).unwrap();
        }
        let reply = server.sync_generate(&mut server_state);
        if let Some(reply) = &reply {
            client.sync_receive(&mut client_state, reply).unwrap();
        }
        if message.is_none() && reply.is_none() {
            completed = true;
            break;
        }
    }
    assert!(completed, "the exchange completes");
    client.finish_sync_with(SERVER, &client_state);
    server.finish_sync_with(CLIENT, &server_state);
    client
        .sync_bytes_generated()
        .saturating_add(server.sync_bytes_generated())
        .saturating_sub(before)
}

fn converged(a: &mut Namespace, b: &mut Namespace) -> bool {
    a.heads() == b.heads()
}

/// Resuming from the agreed heads never costs convergence: heads this
/// document does not hold are not resumed from, and a peer that lost
/// what it agreed to (restarted from an older copy, without its own
/// record of the exchange) still receives everything.
#[test]
fn unknown_or_lost_agreed_heads_fall_back_to_a_full_sync() {
    let mut host = Namespace::new("host").unwrap();
    host.create_file(ROOT_INO, "base", Some(&h(1)), 1, 1)
        .unwrap();
    let mut device = Namespace::from_bytes("device", &host.save()).unwrap();
    exchange(&mut device, &mut host);
    assert_eq!(
        device.begin_sync_with(&SERVER).shared_heads,
        host.heads(),
        "a completed exchange is resumed from"
    );

    let mut unknown = automerge::sync::State::new();
    unknown.shared_heads = vec![automerge::ChangeHash([7; 32])];
    device.finish_sync_with(SERVER, &unknown);
    assert!(
        device.begin_sync_with(&SERVER).shared_heads.is_empty(),
        "unknown agreed heads start a full sync"
    );
    host.create_file(ROOT_INO, "after-unknown", Some(&h(2)), 1, 2)
        .unwrap();
    exchange(&mut device, &mut host);
    assert!(device.lookup(ROOT_INO, "after-unknown").is_some());
    assert!(converged(&mut device, &mut host));

    let older = host.save();
    host.create_file(ROOT_INO, "lost-by-host", Some(&h(3)), 1, 3)
        .unwrap();
    exchange(&mut device, &mut host);
    assert!(device.lookup(ROOT_INO, "lost-by-host").is_some());
    let mut host = Namespace::from_bytes("host", &older).unwrap();
    device
        .create_file(ROOT_INO, "from-device", Some(&h(4)), 1, 4)
        .unwrap();
    exchange(&mut device, &mut host);
    assert!(
        host.lookup(ROOT_INO, "lost-by-host").is_some(),
        "what the peer lost comes back"
    );
    assert!(host.lookup(ROOT_INO, "from-device").is_some());
    assert!(converged(&mut device, &mut host));

    let mut host = Namespace::from_bytes("host", &host.save()).unwrap();
    host.create_file(ROOT_INO, "after-restart", Some(&h(5)), 1, 5)
        .unwrap();
    device
        .create_file(ROOT_INO, "while-host-restarted", Some(&h(6)), 1, 5)
        .unwrap();
    exchange(&mut device, &mut host);
    assert!(device.lookup(ROOT_INO, "after-restart").is_some());
    assert!(host.lookup(ROOT_INO, "while-host-restarted").is_some());
    assert!(converged(&mut device, &mut host));

    let idle = exchange(&mut device, &mut host);
    assert!(idle < 512, "an idle exchange resumes once more ({idle} B)");
}

/// The peers a namespace remembers are bounded; one beyond the bound
/// displaces another, which simply syncs in full next time.
#[test]
fn remembered_sync_progress_is_bounded() {
    let mut ns = Namespace::new("bounded").unwrap();
    let state = automerge::sync::State::new();
    for peer in 0..=MAX_PEER_SYNC_STATES {
        let mut key = [0_u8; 32];
        key[..8].copy_from_slice(&u64::try_from(peer).unwrap().to_le_bytes());
        ns.finish_sync_with(key, &state);
    }
    assert_eq!(ns.peer_sync_states.len(), MAX_PEER_SYNC_STATES);
}

/// A namespace of `changes` changes, one file per hundred.
fn with_history(author: &str, changes: u64) -> Namespace {
    let mut ns = Namespace::new(author).unwrap();
    let mut file = ROOT_INO;
    for change in 0..changes {
        if change.is_multiple_of(100) {
            file = ns
                .create_file(ROOT_INO, format!("file-{change}"), None, 0, change)
                .unwrap();
        } else {
            let content = Hash::new(change.to_le_bytes());
            assert!(ns.set_content(file, Some(&content), change, change));
        }
        ns.heads();
    }
    ns
}

/// #4113. Sync progress persisted beside the document survives a
/// restart of either side: the first exchange after it summarises only
/// the heads, not the whole history, and still carries every change.
#[test]
fn persisted_sync_progress_resumes_after_a_restart() {
    let mut host = with_history("host", 2_000);
    let mut device = Namespace::from_bytes("device", &host.save()).unwrap();
    let full = exchange(&mut device, &mut host);
    assert!(full > 2_000, "first contact is a full sync ({full} B)");
    assert!(device.in_sync_with(&SERVER) && host.in_sync_with(&CLIENT));

    // An idle exchange leaves the progress, and so the persisted bytes, alone.
    let generation = (device.peer_sync_generation(), host.peer_sync_generation());
    exchange(&mut device, &mut host);
    assert_eq!(
        (device.peer_sync_generation(), host.peer_sync_generation()),
        generation,
        "an idle exchange rewrote the progress"
    );

    let (device_bytes, device_progress) = (device.save(), device.encode_peer_sync_states());
    let (host_bytes, host_progress) = (host.save(), host.encode_peer_sync_states());
    let mut device = Namespace::from_bytes("device", &device_bytes).unwrap();
    let mut host = Namespace::from_bytes("host", &host_bytes).unwrap();
    assert!(
        !device.in_sync_with(&SERVER),
        "nothing remembered before restoring"
    );
    assert_eq!(
        device.restore_peer_sync_states(&device_progress).unwrap(),
        1
    );
    assert_eq!(host.restore_peer_sync_states(&host_progress).unwrap(), 1);
    assert!(device.in_sync_with(&SERVER) && host.in_sync_with(&CLIENT));
    let resumed = exchange(&mut device, &mut host);
    assert!(
        resumed < 512,
        "a restarted exchange resumed ({resumed} B, full {full} B)"
    );

    host.create_file(ROOT_INO, "from-host", Some(&h(1)), 1, 1)
        .unwrap();
    assert!(!host.in_sync_with(&CLIENT), "a change is new to the peer");
    device
        .create_file(ROOT_INO, "from-device", Some(&h(2)), 1, 1)
        .unwrap();
    exchange(&mut device, &mut host);
    assert!(device.lookup(ROOT_INO, "from-host").is_some());
    assert!(host.lookup(ROOT_INO, "from-device").is_some());
    assert!(converged(&mut device, &mut host));
}

/// #4113. Persisted progress is never trusted: damaged bytes are
/// refused whole and leave what is held alone; progress naming heads the
/// document does not hold (another workspace's, or written after the
/// copy the document was restored from) falls back to a full sync; older
/// progress resumes from further back. No change is lost either way.
#[test]
fn corrupt_or_stale_persisted_sync_progress_falls_back_to_a_full_sync() {
    let mut host = with_history("host", 1_000);
    let mut device = Namespace::from_bytes("device", &host.save()).unwrap();
    exchange(&mut device, &mut host);
    let older_device = device.save();
    let older_progress = device.encode_peer_sync_states();
    host.create_file(ROOT_INO, "second", Some(&h(1)), 1, 1)
        .unwrap();
    exchange(&mut device, &mut host);
    let progress = device.encode_peer_sync_states();

    let mut flipped = progress.clone();
    flipped[PEER_SYNC_STATES_TAG.len() + 10] ^= 1;
    let mut other_version = progress.clone();
    other_version[7] = b'2';
    let mut trailing = progress[..progress.len() - 32].to_vec();
    trailing.push(0);
    trailing.extend_from_slice(blake3::hash(&trailing).as_bytes());
    let mut too_many = PEER_SYNC_STATES_TAG.to_vec();
    too_many.extend_from_slice(&u32::MAX.to_le_bytes());
    too_many.extend_from_slice(blake3::hash(&too_many.clone()).as_bytes());
    let mut twice = PEER_SYNC_STATES_TAG.to_vec();
    twice.extend_from_slice(&2_u32.to_le_bytes());
    let state = automerge::sync::State::new().encode();
    for _ in 0..2 {
        twice.extend_from_slice(&SERVER);
        twice.extend_from_slice(&u32::try_from(state.len()).unwrap().to_le_bytes());
        twice.extend_from_slice(&state);
    }
    twice.extend_from_slice(blake3::hash(&twice.clone()).as_bytes());
    for (what, bytes) in [
        ("empty", Vec::new()),
        ("truncated", progress[..progress.len() - 1].to_vec()),
        ("bit-flipped", flipped),
        ("another version", other_version),
        ("trailing bytes", trailing),
        ("beyond the peer bound", too_many),
        ("a peer twice", twice),
    ] {
        let mut copy = Namespace::from_bytes("device", &device.save()).unwrap();
        copy.restore_peer_sync_states(&progress).unwrap();
        assert!(
            copy.restore_peer_sync_states(&bytes).is_err(),
            "{what} progress was accepted"
        );
        assert!(
            copy.in_sync_with(&SERVER),
            "{what} progress replaced what was held"
        );
    }

    let mut restored = Namespace::from_bytes("device", &older_device).unwrap();
    assert_eq!(
        restored.restore_peer_sync_states(&progress).unwrap(),
        0,
        "heads the restored document lacks were resumed from"
    );
    let full = exchange(&mut restored, &mut host);
    assert!(full > 1_000, "the fallback is a full sync ({full} B)");
    assert!(restored.lookup(ROOT_INO, "second").is_some());
    assert!(converged(&mut restored, &mut host));

    let mut stranger = with_history("stranger", 10);
    assert_eq!(stranger.restore_peer_sync_states(&progress).unwrap(), 0);
    assert!(!stranger.in_sync_with(&SERVER));

    let mut device = Namespace::from_bytes("device", &device.save()).unwrap();
    assert_eq!(device.restore_peer_sync_states(&older_progress).unwrap(), 1);
    assert!(
        !device.in_sync_with(&SERVER),
        "older progress is not in sync"
    );
    host.create_file(ROOT_INO, "third", Some(&h(2)), 1, 2)
        .unwrap();
    device
        .create_file(ROOT_INO, "from-device", Some(&h(3)), 1, 2)
        .unwrap();
    exchange(&mut device, &mut host);
    assert!(device.lookup(ROOT_INO, "third").is_some());
    assert!(host.lookup(ROOT_INO, "from-device").is_some());
    assert!(converged(&mut device, &mut host));
}

// test-only helper to merge using a fresh loaded copy
impl Namespace {
    fn clone_doc(&mut self) -> Namespace {
        Namespace::from_bytes(self.author.clone(), &self.save()).unwrap()
    }
}

#[test]
fn signed_create_keeps_stable_identity_and_never_renames_unsigned_nodes() {
    let actor = FileActor {
        person: collab_types::Ed25519PublicKey([21; 32]),
        device: collab_types::Ed25519PublicKey([22; 32]),
    };
    let origin = Namespace::new("origin").unwrap();
    let root_record = origin.record_id_for(ROOT_INO).unwrap();
    let root_binding = origin.signed_binding_for(ROOT_INO, actor, 1).unwrap();

    // A log-only rebuild starts from an empty namespace whose provisional
    // root identity is not the workspace's. The signed root binding adopts
    // the established identity instead of refusing every child as an
    // unknown parent.
    let mut rebuilt = Namespace::new("rebuilt").unwrap();
    assert_ne!(rebuilt.record_id_for(ROOT_INO), Some(root_record));
    assert_eq!(rebuilt.apply_signed_binding(&root_binding), Ok(true));
    assert_eq!(rebuilt.record_id_for(ROOT_INO), Some(root_record));

    let record = FileRecordId::new();
    let binding = FileBindingV1 {
        schema_version: FILE_BINDING_VERSION,
        record_id: record,
        parent: Some(root_record),
        name: b"created.txt".to_vec().into(),
        kind: FileNodeKind::File,
        tombstoned: false,
        actor,
        observed_at_ms: 2,
    };
    assert_eq!(rebuilt.apply_signed_binding(&binding), Ok(true));
    assert_eq!(
        rebuilt
            .ensure_ino_for_record_id(record)
            .and_then(|ino| rebuilt.id_for_ino(ino).map(ToString::to_string)),
        Some(format!("record:{}", record.0))
    );
    assert_eq!(rebuilt.apply_signed_binding(&binding), Ok(false));

    // A node with no stable record id is unsigned state. It keeps its name;
    // the signed record that competes for it is the one that moves.
    let unsigned = rebuilt
        .create_file(ROOT_INO, "contested.txt", Some(&h(7)), 1, 3)
        .unwrap();
    let unsigned_id = rebuilt.id_for_ino(unsigned).unwrap().to_string();
    let unsigned_obj = rebuilt.node(&unsigned_id).unwrap();
    rebuilt.doc.delete(&unsigned_obj, RECORD_ID_KEY).unwrap();
    rebuilt.rebuild_record_index();
    assert!(rebuilt.record_id_for(unsigned).is_none());

    let signed_record = FileRecordId::new();
    let contested = FileBindingV1 {
        schema_version: FILE_BINDING_VERSION,
        record_id: signed_record,
        parent: Some(root_record),
        name: b"contested.txt".to_vec().into(),
        kind: FileNodeKind::File,
        tombstoned: false,
        actor,
        observed_at_ms: 4,
    };
    assert_eq!(rebuilt.apply_signed_binding(&contested), Ok(true));
    let entries = rebuilt
        .readdir(ROOT_INO)
        .unwrap()
        .into_iter()
        .filter(|(_, _, name)| name != "." && name != "..")
        .collect::<Vec<_>>();
    // An unsigned node is not addressable by record identity, so it is
    // observed through the tree listing. It keeps the contested name.
    let (contested_ino, _, _) = entries
        .iter()
        .find(|(_, _, name)| name.as_bytes() == b"contested.txt")
        .expect("unsigned node keeps its name");
    assert_eq!(*contested_ino, unsigned);
    let signed_ino = rebuilt.ensure_ino_for_record_id(signed_record).unwrap();
    let conflict = structural_conflict_name(b"contested.txt", signed_record);
    let (conflict_ino, _, _) = entries
        .iter()
        .find(|(_, _, name)| name == &conflict)
        .expect("signed record keeps a conflict sibling");
    assert_eq!(*conflict_ino, signed_ino);

    // The same record log creates the same node key on a second empty
    // namespace, so the two projections merge rather than duplicate.
    let mut again = Namespace::new("again").unwrap();
    assert_eq!(again.apply_signed_binding(&root_binding), Ok(true));
    assert_eq!(again.apply_signed_binding(&binding), Ok(true));
    assert_eq!(
        again
            .ensure_ino_for_record_id(record)
            .and_then(|ino| again.id_for_ino(ino).map(ToString::to_string)),
        Some(format!("record:{}", record.0))
    );
}
