use super::*;

fn h(b: u8) -> Hash {
    Hash::from([b; 32])
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
        assert_eq!(ns.lookup(right, "occupied.txt").unwrap().ino.0, occupied);
    }
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
            .apply_replacement_transaction(&transaction.manifest)
            .unwrap()
            .unwrap();
        assert!(!delta.is_empty());
        assert_eq!(
            ns.lookup(ROOT_INO, "destination.txt").unwrap().ino.0,
            source_ino
        );
        assert!(ns.lookup(ROOT_INO, "source.txt").is_none());
        assert!(ns.ino_for_record_id(victim_record).is_none());
        assert_eq!(
            ns.apply_replacement_transaction(&transaction.manifest),
            Ok(None)
        );
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
fn changes_since_cover_what_a_full_save_came_between() {
    let mut source = Namespace::new("source").unwrap();
    source.create_file(ROOT_INO, "before", None, 0, 1).unwrap();
    let mut peer = Namespace::from_bytes("peer", &source.save()).unwrap();
    let heads = source.heads();
    source.create_file(ROOT_INO, "one", None, 0, 2).unwrap();
    let _ = source.save();
    source.create_file(ROOT_INO, "two", None, 0, 3).unwrap();
    let delta = source.changes_since(&heads);
    peer.apply_remote_and_resolve(&delta).unwrap();
    for name in ["one", "two"] {
        assert!(peer.lookup(ROOT_INO, name).is_some(), "{name} arrived");
    }
    let held = source.heads();
    assert!(
        source.changes_since(&held).is_empty(),
        "everything was told"
    );
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
            a2.find_cycle(None).is_none(),
            "a side must be acyclic after resolve"
        );
        assert!(
            b2.find_cycle(None).is_none(),
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
        let mut a = Namespace::new("a").unwrap();
        a.create_file(ROOT_INO, "base", Some(&h(1)), 1, 1).unwrap();
        let mut b = Namespace::from_bytes("b", &a.save()).unwrap();

        a.create_file(ROOT_INO, "from_a", Some(&h(2)), 1, 1)
            .unwrap();
        b.create_file(ROOT_INO, "from_b", Some(&h(3)), 1, 1)
            .unwrap();

        let from_a = a.save_incremental();
        let from_b = b.save_incremental();
        b.apply_remote_and_resolve(&from_a).unwrap();
        a.apply_remote_and_resolve(&from_b).unwrap();
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
        let mut a = Namespace::new("a").unwrap();
        let ino = a.create_document(ROOT_INO, "doc.md", "md", 1).unwrap();
        let bid = a.document_insert_block(ino, None, "paragraph").unwrap();
        a.document_splice(ino, &bid, 0, 0, "Hello world");

        let snap = a.save();
        let mut b = Namespace::from_bytes("b", &snap).unwrap();
        let bino = b.lookup(ROOT_INO, "doc.md").unwrap().ino.0;

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
    b.document_splice(bino, &second, 6, 0, " paragraph");

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

pub(super) fn assert_indexes_match_rebuild(ns: &Namespace) {
    let mut rebuilt = ns.clone();
    rebuilt.rebuild_record_index();
    assert_eq!(ns.tree.placement, rebuilt.tree.placement, "placement");
    assert_eq!(ns.tree.children, rebuilt.tree.children, "children");
    assert_eq!(ns.tree.records, rebuilt.tree.records, "record bindings");
    assert_eq!(ns.tree.record_of, rebuilt.tree.record_of, "record owners");
    assert_eq!(ns.record_index, rebuilt.record_index, "rows");
    assert_eq!(ns.record_index_order, rebuilt.record_index_order, "order");
    assert_eq!(
        ns.record_index_digest, rebuilt.record_index_digest,
        "digest"
    );
    assert_eq!(
        ns.record_index_revision, rebuilt.record_index_revision,
        "revision"
    );
    assert_eq!(
        ns.record_index_ambiguous, rebuilt.record_index_ambiguous,
        "ambiguity"
    );
}

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
                ns.document_codec(ino).is_some(),
                "document of {ino}"
            );
        }
    }
}

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
