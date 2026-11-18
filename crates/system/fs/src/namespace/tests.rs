use super::*;

fn h(b: u8) -> Hash {
    Hash::from([b; 32])
}

tilo_test_env::scenario! {
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn the_tree_and_its_record_indexes_follow_every_local_and_remote_change;

    phase tree_operations_create_move_trash_and_persist;
    {
        {
            let mut ns = Namespace::new("a").unwrap();
            assert_eq!(ns.id_for_ino(ROOT_INO), Some(ROOT_ID));
            assert!(ns.document_id_for(ROOT_INO).is_some());
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
            let document_id = ns.document_id_for(ino).unwrap();
            let dir = ns.create_dir(ROOT_INO, "sub", 1).unwrap();
            assert!(ns.rename(ROOT_INO, "old", dir, "new", 2));
            assert!(ns.lookup(ROOT_INO, "old").is_none());
            let moved = ns.lookup(dir, "new").unwrap();
            assert_eq!(moved.ino, INodeNo(ino), "inode preserved across move");
            assert_eq!(
                ns.document_id_for(moved.ino.0),
                Some(document_id),
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

    phase document_ids_are_stable_persisted_backfilled_and_resolve_live_blobs;
    {
        {
            let mut original = Namespace::new("a").unwrap();
            let file = original
                .create_file(ROOT_INO, "stable.txt", Some(&h(1)), 1, 1)
                .unwrap();
            let root_document = original.document_id_for(ROOT_INO).unwrap();
            let file_document = original.document_id_for(file).unwrap();
            assert_ne!(root_document, file_document);

            let bytes = original.save();
            let mut reloaded = Namespace::from_bytes("a", &bytes).unwrap();
            let reloaded_file = reloaded.lookup(ROOT_INO, "stable.txt").unwrap().ino.0;
            assert_eq!(reloaded.document_id_for(ROOT_INO), Some(root_document));
            assert_eq!(reloaded.document_id_for(reloaded_file), Some(file_document));
            assert_eq!(
                reloaded.entry_at("stable.txt").unwrap().document_id,
                file_document
            );
        }
        {
            let mut legacy = Namespace::new("legacy").unwrap();
            legacy
                .create_file(ROOT_INO, "old.txt", Some(&h(2)), 2, 1)
                .unwrap();
            let ids: Vec<_> = legacy.doc.sorted_node_ids();
            let _ = legacy.doc.delete(&Obj::index(), ROOT_DOCUMENT_ID_KEY);
            for id in ids {
                let node = legacy.node(&id).unwrap();
                let _ = legacy.doc.delete(&node, DOCUMENT_ID_KEY);
            }
            let legacy_bytes = legacy.save();

            let mut first = Namespace::from_bytes("first", &legacy_bytes).unwrap();
            let mut second = Namespace::from_bytes("second", &legacy_bytes).unwrap();
            let first_ino = first.lookup(ROOT_INO, "old.txt").unwrap().ino.0;
            let second_ino = second.lookup(ROOT_INO, "old.txt").unwrap().ino.0;
            assert_eq!(
                first.document_id_for(ROOT_INO),
                second.document_id_for(ROOT_INO),
                "root migration must not depend on the loading peer"
            );
            assert_eq!(
                first.document_id_for(first_ino),
                second.document_id_for(second_ino),
                "node migration must not depend on the loading peer"
            );

            let expected_root = first.document_id_for(ROOT_INO).unwrap();
            let expected_file = first.document_id_for(first_ino).unwrap();
            let migrated_bytes = first.save();
            let mut persisted = Namespace::from_bytes("third", &migrated_bytes).unwrap();
            let persisted_ino = persisted.lookup(ROOT_INO, "old.txt").unwrap().ino.0;
            assert_eq!(persisted.document_id_for(ROOT_INO), Some(expected_root));
            assert_eq!(persisted.document_id_for(persisted_ino), Some(expected_file));
        }
        {
            let mut ns = Namespace::new("a").unwrap();
            let original = ns.create_file(ROOT_INO, "doc", Some(&h(1)), 1, 1).unwrap();
            let original_document = ns.document_id_for(original).unwrap();
            assert!(ns.create_conflict_copy(ROOT_ID, "doc.sync-conflict", &h(2), 1, 2));
            let copy = ns.resolve_path_ino("doc.sync-conflict").unwrap();
            assert_ne!(ns.document_id_for(copy), Some(original_document));
            assert!(ns.document_id_for(copy).is_some());
        }
        {
            let mut ns = Namespace::new("a").unwrap();
            ns.create_file(ROOT_INO, "x", Some(&h(1)), 1, 1).unwrap();
            let mut copy = ns.clone_doc();
            assert!(copy.document_ids_complete);
            let copied = copy.lookup(ROOT_INO, "x").unwrap().ino.0;
            let original = ns.lookup(ROOT_INO, "x").unwrap().ino.0;
            assert_eq!(copy.document_id_for(copied), ns.document_id_for(original));
        }
        {
            let mut ns = Namespace::new("owner").unwrap();
            let a = ns.create_dir(ROOT_INO, "a", 1).unwrap();
            let b = ns.create_dir(ROOT_INO, "b", 1).unwrap();
            let a_id = ns.id_for_ino(a).unwrap().to_string();
            let b_id = ns.id_for_ino(b).unwrap().to_string();
            ns.set_field(&a_id, "parent", b_id.clone());
            ns.set_field(&b_id, "parent", a_id);

            ns.rebuild_document_index();
            assert_eq!(
                ns.indexed_document_count(),
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
            let first_document = ns.document_id_for(first).unwrap();
            let second_document = ns.document_id_for(second).unwrap();
            assert!(ns.set_thumb(second, &h(1)));
            assert_eq!(
                ns.live_document_ids_for_direct_blob(h(1), 8).unwrap(),
                vec![first_document, second_document]
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                ns.live_document_ids_for_direct_blob(h(1), 1),
                Err(LiveDocumentBlobScanError::LimitExceeded { limit: 1 })
            );

            assert!(ns.remove(first));
            assert_eq!(
                ns.live_document_ids_for_direct_blob(h(1), 8).unwrap(),
                vec![second_document]
            );
            assert!(
                ns.live_document_ids_for_direct_blob(h(77), 8)
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

    phase content_revisions_are_never_torn_and_legacy_writes_supersede_explicitly;
    {
        {
            let mut a = Namespace::new("a").unwrap();
            let ino = a
                .create_file(ROOT_INO, "atomic.bin", Some(&h(1)), 1, 1)
                .unwrap();
            let document_id = a.document_id_for(ino).unwrap();
            let bytes = a.save();
            let mut b = Namespace::from_bytes("b", &bytes).unwrap();
            let bino = b.lookup(ROOT_INO, "atomic.bin").unwrap().ino.0;
            let revision_a = ContentRevisionV1 {
                schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                document_id,
                version_id: fs_protocol::FileVersionId::new(),
                content_hash: Some(fs_protocol::ContentHash([10; 32])),
                byte_len: 10,
                metadata_hash: fs_protocol::ContentHash([11; 32]),
            };
            let revision_b = ContentRevisionV1 {
                schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                document_id,
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
                document_id: ns.document_id_for(ino).unwrap(),
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

    phase changes_since_cover_what_a_full_save_came_between;
    {
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

    phase concurrent_namespace_changes_merge_and_converge;
    {
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
                .sorted_node_ids()
                .into_iter()
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

    phase record_indexes_track_structure_and_page_tails_directly;
    {
        {
            let mut ns = Namespace::new("a").unwrap();
            ns.document_index.clear();
            ns.document_index_order.clear();
            for value in 1..=100_001_u128 {
                let document_id = FileDocumentId(Uuid::from_u128(value));
                ns.document_index.insert(
                    document_id,
                    IndexedDocument {
                        document_id,
                        node_id: format!("synthetic/{value}"),
                        parent_id: ROOT_ID.into(),
                        path: format!("/synthetic-{value}"),
                        is_dir: false,
                    },
                );
                ns.document_index_order.push(document_id);
            }
            let before = ns.document_index_rebuild_count();
            let page = ns.indexed_document_page(100_000, 128);
            assert_eq!(page.len(), 1);
            assert_eq!(page[0].path, "/synthetic-100001");
            assert_eq!(ns.document_index_rebuild_count(), before);
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
                .sorted_node_ids()
                .into_iter()
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

    phase inode_view_matches_the_separate_accessors_after_remote_apply;
    {
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
        let mut peer = Namespace::from_bytes("peer", &source.save()).unwrap();
        let peer_dir = peer.lookup(ROOT_INO, "dir").unwrap().ino.0;
        let peer_a = peer.lookup(peer_dir, "a.txt").unwrap().ino.0;
        let peer_b = peer.lookup(ROOT_INO, "b.txt").unwrap().ino.0;
        let peer_gone = peer.lookup(ROOT_INO, "gone.txt").unwrap().ino.0;
        let inos = [ROOT_INO, peer_dir, peer_a, peer_b, peer_gone, 9_999];
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
    }
}

tilo_test_env::scenario! {
    #[allow(
        clippy::too_many_lines,
        reason = "The signed two-record fixture verifies source replacement, victim tombstoning, emitted namespace delta, and idempotent replay together."
    )]
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "test arithmetic: the lint exempts the body of a test function, not the closure a phase is"
    )]
    fn signed_records_bind_exactly_and_fail_closed;

    phase signed_bindings_and_replacements_are_verified_exact_and_fail_closed;
    {
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
                document_id: ns.document_id_for(file).unwrap(),
                parent: Some(ns.document_id_for(right).unwrap()),
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
            let file_document = ns.document_id_for(file).unwrap();
            let occupied_document = ns.document_id_for(occupied).unwrap();
            assert_eq!(ns.apply_signed_binding(&occupied_binding), Ok(true));
            let (winner, loser_document) = if file_document < occupied_document {
                (file, occupied_document)
            } else {
                (occupied, file_document)
            };
            assert_eq!(ns.lookup(right, "occupied.txt").unwrap().ino.0, winner);
            let conflict = structural_conflict_name(b"occupied.txt", loser_document);
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
            let mut ns = Namespace::new("owner").unwrap();
            let first = ns.create_file(ROOT_INO, "a", None, 0, 1).unwrap();
            let duplicate = ns.create_file(ROOT_INO, "b", None, 0, 1).unwrap();
            let document_id = ns.document_id_for(first).unwrap();
            let duplicate_id = ns.id_for_ino(duplicate).unwrap().to_string();
            let duplicate_obj = ns.node(&duplicate_id).unwrap();
            ns.doc
                .put(&duplicate_obj, DOCUMENT_ID_KEY, document_id.0.to_string())
                .unwrap();
            ns.rebuild_document_index();
            assert!(
                ns.indexed_document(document_id).is_none(),
                "an ambiguous durable record identity must be withheld, not routed to an arbitrary node"
            );
            assert_eq!(ns.hash_for(first), None);
            assert_eq!(ns.hash_for(duplicate), None);
        }
        {
            let mut ns = Namespace::new("recipient").unwrap();
            let record = FileDocumentId::new();
            let ino = ns
                .create_projected_file(ROOT_INO, "shared.png", record, Some(&h(8)), 42, 9)
                .unwrap();
            assert_eq!(ns.document_id_for(ino), Some(record));
            assert_eq!(ns.hash_for(ino), Some(h(8)));
            assert_eq!(ns.getattr(ino).unwrap().size, 42);
            assert!(
                ns.create_projected_file(ROOT_INO, "alias.png", record, None, 0, 0)
                    .is_none()
            );
            assert!(
                ns.create_projected_file(ROOT_INO, "shared.png", FileDocumentId::new(), None, 0, 0)
                    .is_none()
            );
            assert!(
                ns.create_projected_file(ROOT_INO, "../escape", FileDocumentId::new(), None, 0, 0)
                    .is_none()
            );
        }
    }

    phase signed_create_keeps_stable_identity_and_never_renames_unsigned_nodes;
    {
        let actor = FileActor {
            person: collab_types::Ed25519PublicKey([21; 32]),
            device: collab_types::Ed25519PublicKey([22; 32]),
        };
        let origin = Namespace::new("origin").unwrap();
        let root_document = origin.document_id_for(ROOT_INO).unwrap();
        let root_binding = origin.signed_binding_for(ROOT_INO, actor, 1).unwrap();

        let mut rebuilt = Namespace::new("rebuilt").unwrap();
        assert_ne!(rebuilt.document_id_for(ROOT_INO), Some(root_document));
        assert_eq!(rebuilt.apply_signed_binding(&root_binding), Ok(true));
        assert_eq!(rebuilt.document_id_for(ROOT_INO), Some(root_document));

        let record = FileDocumentId::new();
        let binding = FileBindingV1 {
            schema_version: FILE_BINDING_VERSION,
            document_id: record,
            parent: Some(root_document),
            name: b"created.txt".to_vec().into(),
            kind: FileNodeKind::File,
            tombstoned: false,
            actor,
            observed_at_ms: 2,
        };
        assert_eq!(rebuilt.apply_signed_binding(&binding), Ok(true));
        assert_eq!(
            rebuilt
                .ensure_ino_for_document_id(record)
                .and_then(|ino| rebuilt.id_for_ino(ino).map(ToString::to_string)),
            Some(format!("record:{}", record.0))
        );
        assert_eq!(rebuilt.apply_signed_binding(&binding), Ok(false));

        let unsigned = rebuilt
            .create_file(ROOT_INO, "contested.txt", Some(&h(7)), 1, 3)
            .unwrap();
        let unsigned_id = rebuilt.id_for_ino(unsigned).unwrap().to_string();
        let unsigned_obj = rebuilt.node(&unsigned_id).unwrap();
        rebuilt.doc.delete(&unsigned_obj, DOCUMENT_ID_KEY).unwrap();
        rebuilt.rebuild_document_index();
        assert!(rebuilt.document_id_for(unsigned).is_none());

        let signed_document = FileDocumentId::new();
        let contested = FileBindingV1 {
            schema_version: FILE_BINDING_VERSION,
            document_id: signed_document,
            parent: Some(root_document),
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
        let signed_ino = rebuilt.ensure_ino_for_document_id(signed_document).unwrap();
        let conflict = structural_conflict_name(b"contested.txt", signed_document);
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
                .ensure_ino_for_document_id(record)
                .and_then(|ino| again.id_for_ino(ino).map(ToString::to_string)),
            Some(format!("record:{}", record.0))
        );
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

        let mut migrated = Namespace::from_bytes("migrated", &legacy.save()).unwrap();
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
        let document_id = reopened.document_id_for(reopened_ino).unwrap();
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
        assert_eq!(final_open.document_id_for(final_ino), Some(document_id));
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

pub(super) fn assert_indexes_match_rebuild(ns: &Namespace) {
    let mut rebuilt = ns.clone();
    rebuilt.rebuild_document_index();
    assert_eq!(ns.tree.placement, rebuilt.tree.placement, "placement");
    assert_eq!(ns.tree.children, rebuilt.tree.children, "children");
    assert_eq!(ns.tree.records, rebuilt.tree.records, "record bindings");
    assert_eq!(
        ns.tree.resource_of, rebuilt.tree.resource_of,
        "record owners"
    );
    assert_eq!(ns.document_index, rebuilt.document_index, "rows");
    assert_eq!(
        ns.document_index_order, rebuilt.document_index_order,
        "order"
    );
    assert_eq!(
        ns.document_index_digest, rebuilt.document_index_digest,
        "digest"
    );
    assert_eq!(
        ns.document_index_revision, rebuilt.document_index_revision,
        "revision"
    );
    assert_eq!(
        ns.document_index_ambiguous, rebuilt.document_index_ambiguous,
        "ambiguity"
    );
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
        }
    }
}

impl Namespace {
    fn clone_doc(&mut self) -> Namespace {
        Namespace::from_bytes(self.author.clone(), &self.save()).unwrap()
    }
}

#[test]
fn trees_founded_apart_merge_with_every_file_of_each() {
    let names = |ns: &mut Namespace| -> Vec<String> {
        let mut names: Vec<String> = ns
            .live_node_ids()
            .iter()
            .filter_map(|id| ns.entry_for_node(id))
            .map(|entry| String::from_utf8_lossy(entry.name.as_bytes()).into_owned())
            .collect();
        names.sort();
        names
    };
    // Two computers that wrote into their folders before either reached the workspace.
    let mut laptop = Namespace::new("laptop").unwrap();
    laptop
        .create_file(ROOT_INO, "contract.txt", Some(&h(1)), 6, 1)
        .unwrap();
    let mut desktop = Namespace::new("desktop").unwrap();
    desktop
        .create_file(ROOT_INO, "handbook.md", Some(&h(2)), 6, 1)
        .unwrap();
    laptop.merge_remote_and_resolve(&desktop.save()).unwrap();
    assert_eq!(names(&mut laptop), ["contract.txt", "handbook.md"]);

    // A page that put the first file into the workspace's empty record, before any computer
    // reached it: the record holds no tree yet, so the page's placing makes its map.
    let mut record = automerge::AutoCommit::new();
    let mut tree = fs_protocol::tree::Tree::of_document(&record.save()).unwrap();
    fs_protocol::tree::place(
        (&mut record, 1),
        &mut tree,
        ROOT_ID,
        "notice.pdf",
        fs_protocol::tree::Placing::File {
            hash: &h(3).to_hex(),
            size: 3,
        },
        (1, "page"),
    )
    .unwrap();
    let _ = record.commit();
    laptop.take_documents(vec![(1, record.save())]).unwrap();
    assert_eq!(
        names(&mut laptop),
        ["contract.txt", "handbook.md", "notice.pdf"]
    );

    // And the computer's tree read back from what it keeps holds them all.
    let mut kept = Namespace::from_bytes("laptop", &laptop.save()).unwrap();
    assert_eq!(
        names(&mut kept),
        ["contract.txt", "handbook.md", "notice.pdf"]
    );
}

#[test]
fn the_folder_reads_as_changed_with_its_newest_entry() {
    let mut ns = Namespace::new("laptop").unwrap();
    let opened = ns.getattr(ROOT_INO).unwrap().mtime;
    assert!(
        opened > UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        "an empty folder reads as changed when it opened, not in 1970: {opened:?}"
    );
    let newest = 1_790_000_000_000;
    ns.create_file(ROOT_INO, "terms.txt", Some(&h(1)), 6, newest)
        .unwrap();
    ns.create_file(ROOT_INO, "older.txt", Some(&h(2)), 6, newest - 60_000)
        .unwrap();
    assert_eq!(
        ns.getattr(ROOT_INO).unwrap().mtime,
        UNIX_EPOCH + Duration::from_millis(newest)
    );
}

/// A tree's files fill one chunk after another, and a write to one file is a write to the
/// chunk that holds it: what another device takes in costs that chunk's change, whatever the
/// tree holds.
#[test]
fn a_write_is_one_change_of_the_chunk_that_holds_the_file() {
    let files = fs_protocol::tree::CHUNK_NODES * 2 + 10;
    let mut laptop = Namespace::new("laptop").unwrap();
    let mut first = None;
    for n in 0..files {
        let ino = laptop
            .create_file(ROOT_INO, format!("f{n}.txt"), Some(&h(1)), 1, 1)
            .unwrap();
        first.get_or_insert(ino);
    }
    assert_eq!(
        laptop.records(),
        [0, 1, 2, 3],
        "the files filled three chunks"
    );

    // Another device takes the tree in from its records, as they hold it.
    let writes = laptop.document_writes(&BTreeMap::new());
    assert_eq!(writes.len(), 4, "the tree's own record and each chunk");
    let mut desktop = Namespace::new("desktop").unwrap();
    desktop
        .take_documents(writes.into_iter().map(|w| (w.chunk, w.changes)).collect())
        .unwrap();
    assert!(
        desktop
            .lookup(ROOT_INO, format!("f{}.txt", files - 1))
            .is_some()
    );
    assert_eq!(
        desktop.indexed_document_count(),
        laptop.indexed_document_count()
    );
    assert_eq!(
        desktop.document_id_for(ROOT_INO),
        laptop.document_id_for(ROOT_INO)
    );

    // Renaming the first file writes its chunk alone.
    let known = laptop.record_heads();
    let first = first.unwrap();
    let chunk = laptop.document_id_for(first).unwrap().chunk();
    assert!(laptop.rename(ROOT_INO, "f0.txt", ROOT_INO, "renamed.txt", 2));
    let writes = laptop.document_writes(&known);
    assert_eq!(
        writes.iter().map(|w| w.chunk).collect::<Vec<_>>(),
        [chunk],
        "a rename is a change of the file's chunk"
    );
    assert!(
        writes[0].changes.len() < 1024,
        "and carries the rename, not the chunk: {} bytes",
        writes[0].changes.len()
    );
    desktop
        .take_documents(vec![(chunk, writes[0].changes.clone())])
        .unwrap();
    assert!(desktop.lookup(ROOT_INO, "renamed.txt").is_some());
    assert!(desktop.lookup(ROOT_INO, "f0.txt").is_none());

    // What the desktop holds now is what the laptop does, record by record.
    assert_eq!(desktop.record_heads(), laptop.record_heads());
    // And a namespace kept on disk reads back the same.
    let kept = Namespace::from_bytes("laptop", &laptop.save()).unwrap();
    assert_eq!(
        kept.indexed_document_count(),
        laptop.indexed_document_count()
    );
}

/// A record that changed more than one write carries (files written while it could not go out)
/// goes out over several writes, each within the budget and building on the last, until the
/// holder has every change.
#[test]
fn a_record_changed_past_one_write_goes_out_over_several() {
    let mut laptop = Namespace::new("laptop").unwrap();
    for n in 0..200_u32 {
        laptop
            .create_file(ROOT_INO, format!("file-{n}.rs"), Some(&h(1)), 4, 1)
            .unwrap();
        // Each file is its own change, as each push of the folder commits what it made.
        laptop.record_heads();
    }
    let whole = laptop.record_write(1, &[], usize::MAX).unwrap();
    let budget = whole.changes.len() / 5;
    let mut record = Vec::new();
    let mut sent = Vec::new();
    let mut writes = 0;
    while let Some(write) = laptop.record_write(1, &sent, budget) {
        assert!(
            !write.changes.is_empty(),
            "a write carries something until the heads are reached"
        );
        assert!(
            write.changes.len() <= budget,
            "{} bytes over a budget of {budget}",
            write.changes.len()
        );
        record.extend_from_slice(&write.changes);
        sent = write.heads;
        writes += 1;
        assert!(writes < 50, "the writes reach the record's heads");
    }
    assert!(writes > 1, "the record went out in one write");
    let mut record = automerge::AutoCommit::load(&record).unwrap();
    assert_eq!(
        record.get_heads(),
        whole.heads,
        "the writes carry every change"
    );
}

/// The entries of a document's changes, oldest first: each change's file action verbs and who
/// they name.
fn entries_of(document: &[u8]) -> Vec<(Vec<String>, Option<String>)> {
    let mut doc = AutoCommit::load(document).unwrap();
    doc.get_changes(&[])
        .into_iter()
        .filter_map(|change| tilo_actions::ActionContext::from_change_message(change.message()?))
        .filter(|context| {
            context
                .resource
                .as_ref()
                .map(|resource| resource.kind.as_str())
                == Some(fs_protocol::tree::TREE_KIND)
        })
        .map(|context| {
            let verbs = match &context.what {
                Some(tilo_actions::Did::Verb(verb)) => vec![verb.action.action.clone()],
                Some(tilo_actions::Did::Group(group)) => group
                    .distinct
                    .iter()
                    .filter_map(|did| match did {
                        tilo_actions::Did::Verb(verb) => Some(verb.action.action.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let who = context
                .invoker
                .as_ref()
                .and_then(|invoker| invoker.name.clone());
            (verbs, who)
        })
        .collect()
}

/// What an agent writes through the mount is a change of the tree naming the file actions
/// it did and the agent: History shows it under the agent, grants decide its verbs, and what
/// the person writes next is a change of their own.
#[test]
fn a_write_through_the_mount_names_its_file_actions_and_the_agent_that_wrote() {
    let mut ns = Namespace::new("ada").unwrap();
    let agent = tilo_actions::ActionInvoker::admitted(
        &tilo_actions::Acting::agent("claude").in_session("s1"),
    )
    .named(Some("Tidy the notes".into()));
    let (ino, chunk) = {
        let _acting = acting_as(Some(
            tilo_actions::ActionContext::default().with_invoker(agent),
        ));
        let ino = ns
            .create_file(ROOT_INO, "plan.md", Some(&h(1)), 4, 1)
            .unwrap();
        assert!(ns.set_content(ino, Some(&h(2)), 5, 2));
        (ino, ns.document_id_for(ino).unwrap().chunk())
    };
    // The person renames it afterwards, on the same thread.
    assert!(ns.rename(ROOT_INO, "plan.md", ROOT_INO, "final.md", 3));
    let document = ns.doc.doc_mut(chunk).unwrap().save();
    let entries = entries_of(&document);
    assert_eq!(
        entries,
        vec![
            (
                vec!["new-file".to_owned(), "write".to_owned()],
                Some("Tidy the notes".to_owned())
            ),
            (vec!["move".to_owned()], None),
        ],
        "one change by the agent, one by the person"
    );
    assert_eq!(ns.hash_for(ino), Some(h(2)));
}

/// Each chunk's heads, the tree's own document first.
fn chunk_heads(ns: &mut Namespace) -> BTreeMap<fs_protocol::TreeChunk, Vec<automerge::ChangeHash>> {
    ns.note_change();
    ns.doc.heads()
}

/// A device that wrote a change and has not published it yet takes in another device's change of
/// the same chunk: its own change is not counted as one the holders have, so it goes out with
/// its next publication and the holders merge every later change built on it.
#[test]
fn a_change_not_published_yet_goes_out_after_another_devices_change_arrives() {
    let mut laptop = Namespace::new("laptop").unwrap();
    laptop
        .create_file(ROOT_INO, "report.txt", Some(&h(1)), 1, 1)
        .unwrap();
    laptop
        .create_file(ROOT_INO, "build.bin", Some(&h(2)), 1, 1)
        .unwrap();
    let mut desktop = Namespace::new("desktop").unwrap();
    desktop
        .take_documents(
            laptop
                .document_writes(&BTreeMap::new())
                .into_iter()
                .map(|w| (w.chunk, w.changes))
                .collect(),
        )
        .unwrap();
    let (laptop_published, desktop_published) =
        (chunk_heads(&mut laptop), chunk_heads(&mut desktop));

    // The desktop renames one file and has not published it when the laptop's rename arrives.
    assert!(desktop.rename(ROOT_INO, "report.txt", ROOT_INO, "notes.txt", 2));
    assert!(laptop.rename(ROOT_INO, "build.bin", ROOT_INO, "out.bin", 2));
    let from_laptop = laptop.document_writes(&laptop_published);
    let holders = desktop
        .take_published_documents(
            from_laptop
                .into_iter()
                .map(|w| (w.chunk, w.changes))
                .collect(),
            &desktop_published,
        )
        .unwrap();
    assert!(desktop.lookup(ROOT_INO, "out.bin").is_some());

    // The desktop's next publication carries its rename, and the laptop takes it.
    let from_desktop = desktop.document_writes(&holders);
    assert!(!from_desktop.is_empty(), "the desktop's rename goes out");
    laptop
        .take_documents(
            from_desktop
                .into_iter()
                .map(|w| (w.chunk, w.changes))
                .collect(),
        )
        .unwrap();
    assert!(laptop.lookup(ROOT_INO, "notes.txt").is_some());
    assert_eq!(chunk_heads(&mut laptop), chunk_heads(&mut desktop));
}
