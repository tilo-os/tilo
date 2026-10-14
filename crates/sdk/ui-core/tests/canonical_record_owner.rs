//! Explicit owners use only fresh per-test roots; no launch environment or user state.
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc};
use tilo_ui::record::{
    self, RecordCheckout, RecordId, RecordKind, RecordOpenError, RecordOpenOutcome, RecordOwners,
    RecordSaveStatus, RecordStore,
};

fn target() -> RecordCheckout {
    RecordCheckout::new(RecordId::minted(), RecordKind::new("note"))
}
fn directory(
    state_root: &std::path::Path,
    room: &str,
    want: &RecordCheckout,
) -> std::path::PathBuf {
    app_state::DocStore::room_record_dir_at(
        state_root,
        "so.tilo.test",
        room,
        &want.id.storage_key(),
    )
    .unwrap()
}
/// The record's save status once the store has settled it. `try_flush` and a
/// repairing open hand durability to the store's thread, which confirms it
/// a moment later; until then the record reads, correctly, as Saving.
fn settled(store: &RecordStore, id: &RecordId) -> RecordSaveStatus {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let status = store.save_status(id).unwrap();
        if status != RecordSaveStatus::Saving || std::time::Instant::now() >= deadline {
            return status;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
fn body(store: &RecordStore, want: &RecordCheckout) -> String {
    store
        .read(&want.id, |doc| {
            doc.get(ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .as_str()
                .unwrap()
                .to_owned()
        })
        .unwrap()
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn one_canonical_owner_shares_history_and_reserves_exactly_once() {
    // Case `corrupt_is_not_a_new_record`.
    {
        let tmp = tempfile::tempdir().unwrap();
        let mut owners = RecordOwners::new(tmp.path().join("state"));
        let store = owners.open("so.tilo.test", "room-a").unwrap();
        let want = target();
        let path =
            directory(&tmp.path().join("state"), "room-a", &want).join(app_state::DOCUMENT_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for original in [b"not an automerge document".to_vec(), vec![]] {
            std::fs::write(&path, &original).unwrap();
            assert!(matches!(
                store.try_open_record(&want),
                Err(RecordOpenError::Store(_))
            ));
            assert!(store.read(&want.id, |_| ()).is_none());
            assert!(store.reserve_record(&want).is_err());
            store.try_flush().unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }

    // Case `one_owner_shares_changes_and_history_but_rooms_do_not_alias`.
    {
        let tmp = tempfile::tempdir().unwrap();
        let mut owners = RecordOwners::new(tmp.path().join("state"));
        let a = owners.open("so.tilo.test", "room-a").unwrap();
        let b = owners.open("so.tilo.test", "room-a").unwrap();
        let other = owners.open("so.tilo.test", "room-b").unwrap();
        let want = target();
        assert!(matches!(
            a.try_open_record(&want),
            Err(RecordOpenError::MissingRecord)
        ));
        let mut token = a.reserve_record(&want).unwrap();
        token
            .initialize(|doc| {
                doc.put(ROOT, "body", "shared").unwrap();
                Ok(())
            })
            .unwrap();
        assert!(matches!(
            b.try_open_record(&want),
            Ok(RecordOpenOutcome::AlreadyOpen)
        ));
        assert_eq!(body(&b, &want), "shared");
        let mut second = other.reserve_record(&want).unwrap();
        second
            .initialize(|doc| {
                doc.put(ROOT, "body", "other").unwrap();
                Ok(())
            })
            .unwrap();
        b.with(&want.id, |doc| doc.put(ROOT, "body", "changed").unwrap())
            .unwrap();
        assert_eq!(body(&a, &want), "changed");
        assert_eq!(body(&other, &want), "other");
        a.try_flush().unwrap();
        assert_eq!(settled(&b, &want.id), RecordSaveStatus::Saved);
        drop((token, second, a, b, other));
        let reopened = owners.open("so.tilo.test", "room-a").unwrap();
        assert!(matches!(
            reopened.try_open_record(&want),
            Ok(RecordOpenOutcome::Loaded)
        ));
        assert_eq!(body(&reopened, &want), "changed");
    }

    // Case `reservation_is_private_retryable_and_exactly_once`.
    {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("state");
        let mut owners = RecordOwners::new(root.clone());
        let store = owners.open("so.tilo.test", "room-a").unwrap();
        let want = target();
        let mut reservation = store.reserve_record(&want).unwrap();
        assert_eq!(reservation.checkout().id, want.id);
        assert!(!store.is_ready(&want.id));
        assert!(store.open_records().is_empty());
        assert!(matches!(
            store.save_status(&want.id),
            Err(RecordOpenError::Busy)
        ));
        let path = directory(&root, "room-a", &want).join(app_state::DOCUMENT_FILE);
        assert!(!path.exists());
        assert!(matches!(
            reservation.initialize(|doc| {
                doc.put(ROOT, "discard", "partial").unwrap();
                assert!(matches!(
                    store.try_open_record(&want),
                    Err(RecordOpenError::Busy)
                ));
                assert!(matches!(
                    store.reserve_record(&want),
                    Err(RecordOpenError::Busy)
                ));
                Err("retry me".into())
            }),
            Err(RecordOpenError::Initialization(_))
        ));
        assert!(!path.exists());
        reservation
            .initialize(|doc| {
                assert!(doc.get(ROOT, "discard").unwrap().is_none());
                Ok(())
            })
            .unwrap();
        assert!(
            reservation
                .initialize(|_| panic!("must not initialize twice"))
                .is_err()
        );
        drop(reservation);
        assert!(store.is_ready(&want.id));
        store.try_flush().unwrap();
        let unused = target();
        let canceled = store.reserve_record(&unused).unwrap();
        drop(canceled);
        assert!(matches!(
            store.try_open_record(&unused),
            Err(RecordOpenError::MissingRecord)
        ));
        assert!(matches!(
            store.save_status(&unused.id),
            Err(RecordOpenError::MissingRecord)
        ));
        assert!(store.reserve_record(&unused).is_ok());
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn validation_refusals_and_repairs_preserve_bytes_and_reservations() {
    // Case `identity_and_schema_refusal_preserves_complete_and_recovered_bytes`.
    {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("state");
        let mut owners = RecordOwners::new(root.clone());
        let store = owners.open("so.tilo.test", "room-a").unwrap();
        for case in ["record", "room", "kind", "home", "schema"] {
            for torn in [false, true] {
                let want = target();
                let mut doc = AutoCommit::new();
                let id = if case == "record" {
                    RecordId::minted()
                } else {
                    want.id.clone()
                };
                let kind = if case == "kind" {
                    RecordKind::new("wrong")
                } else {
                    want.kind.clone()
                };
                record::init_scoped(
                    &mut doc,
                    &id,
                    &kind,
                    if case == "home" { "" } else { "minting-room" },
                    Some(if case == "room" {
                        "other-room"
                    } else {
                        "room-a"
                    }),
                );
                if case == "schema" {
                    doc.put(
                        ROOT,
                        app_state::SCHEMA_VERSION_KEY,
                        u64::from(app_state::SCHEMA_VERSION) + 1,
                    )
                    .unwrap();
                }
                let heads = doc.get_heads();
                let mut original = doc.save();
                if torn {
                    doc.put(ROOT, "torn", true).unwrap();
                    let tail = doc.save_after(&heads);
                    original.extend_from_slice(&tail[..tail.len() / 2]);
                }
                let path = directory(&root, "room-a", &want).join(app_state::DOCUMENT_FILE);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, &original).unwrap();
                assert!(
                    store.try_open_record(&want).is_err(),
                    "case {case}, torn {torn}"
                );
                assert!(!store.is_ready(&want.id));
                store.try_flush().unwrap();
                assert_eq!(std::fs::read(&path).unwrap(), original);
            }
        }
    }

    // Case `validated_prefix_repair_preserves_home_and_survives_edit_flush_reopen`.
    {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("state");
        let mut owners = RecordOwners::new(root.clone());
        let store = owners.open("so.tilo.test", "room-a").unwrap();
        let want = target();
        let mut doc = AutoCommit::new();
        record::init_scoped(
            &mut doc,
            &want.id,
            &want.kind,
            "original-minting-room",
            Some("room-a"),
        );
        doc.put(ROOT, "body", "prefix").unwrap();
        let heads = doc.get_heads();
        let mut bytes = doc.save();
        doc.put(ROOT, "body", "torn-tail").unwrap();
        let tail = doc.save_after(&heads);
        bytes.extend_from_slice(&tail[..tail.len() / 2]);
        let path = directory(&root, "room-a", &want).join(app_state::DOCUMENT_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        store.try_open_record(&want).unwrap();
        assert_eq!(body(&store, &want), "prefix");
        assert_eq!(
            store.read(&want.id, record::home).flatten().as_deref(),
            Some("original-minting-room")
        );
        assert_eq!(settled(&store, &want.id), RecordSaveStatus::Saved);
        store.with(&want.id, |doc| {
            doc.put(ROOT, "body", "after-recovery").unwrap();
        });
        store.try_flush().unwrap();
        drop(store);
        let store = owners.open("so.tilo.test", "room-a").unwrap();
        store.try_open_record(&want).unwrap();
        assert_eq!(body(&store, &want), "after-recovery");
    }

    // Case `new_schema_validation_failure_keeps_unpublished_reservation_for_retry`.
    {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("state");
        let mut owners = RecordOwners::new(root.clone());
        let store = owners.open("so.tilo.test", "room-a").unwrap();
        let want = target();
        let mut token = store.reserve_record(&want).unwrap();
        assert!(matches!(
            token.initialize(|doc| {
                doc.put(ROOT, app_state::SCHEMA_VERSION_KEY, "invalid")
                    .unwrap();
                Ok(())
            }),
            Err(RecordOpenError::Store(
                app_state::StoreError::InvalidSchema { .. }
            ))
        ));
        assert!(!store.is_ready(&want.id));
        assert!(
            !directory(&root, "room-a", &want)
                .join(app_state::DOCUMENT_FILE)
                .exists()
        );
        token.initialize(|_| Ok(())).unwrap();
        store.try_flush().unwrap();
        let path = directory(&root, "room-a", &want).join(app_state::DOCUMENT_FILE);
        std::fs::remove_file(&path).unwrap();
        store.with(&want.id, |doc| {
            doc.put(ROOT, "body", "missing-append-target").unwrap();
        });
        assert!(store.try_flush().is_err());
        assert!(
            !path.exists(),
            "append must not create a missing base document"
        );
        store.try_flush().unwrap();
        assert_eq!(settled(&store, &want.id), RecordSaveStatus::Saved);
        assert!(AutoCommit::load(&std::fs::read(path).unwrap()).is_ok());
    }
}

#[test]
fn status_polling_and_failed_flushes_keep_pending_edits() {
    // Case `status_polling_preserves_pending_typing_and_memory_only`.
    {
        let memory = RecordStore::in_memory();
        let want = target();
        memory.open_record(&want);
        assert_eq!(
            memory.save_status(&want.id).unwrap(),
            RecordSaveStatus::MemoryOnly
        );
        let tmp = tempfile::tempdir().unwrap();
        let mut owners = RecordOwners::new(tmp.path().join("state"));
        let store = owners.open("so.tilo.test", "room-a").unwrap();
        store
            .reserve_record(&want)
            .unwrap()
            .initialize(|_| Ok(()))
            .unwrap();
        store.try_flush().unwrap();
        store.with(&want.id, |doc| {
            doc.put(ROOT, "body", "typing").unwrap();
        });
        let before = store
            .with_history(&want.id, |doc, history| {
                (doc.pending_ops(), doc.base_heads(), history.revision())
            })
            .unwrap();
        assert!(before.0 > 0);
        for _ in 0..100 {
            assert_eq!(
                store.save_status(&want.id).unwrap(),
                RecordSaveStatus::Saving
            );
        }
        let after = store
            .with_history(&want.id, |doc, history| {
                (doc.pending_ops(), doc.base_heads(), history.revision())
            })
            .unwrap();
        assert_eq!(before, after);
        store.try_flush().unwrap();
        assert_eq!(settled(&store, &want.id), RecordSaveStatus::Saved);
    }

    // Case `failed_flush_retries_complete_latest_document_without_new_edit`.
    {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("state");
        let mut owners = RecordOwners::new(root.clone());
        let store = owners.open("so.tilo.test", "room-a").unwrap();
        let want = target();
        store
            .reserve_record(&want)
            .unwrap()
            .initialize(|_| Ok(()))
            .unwrap();
        store.try_flush().unwrap();
        let path = directory(&root, "room-a", &want).join(app_state::DOCUMENT_FILE);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        store.with(&want.id, |doc| doc.put(ROOT, "body", "first").unwrap());
        assert!(store.try_flush().is_err());
        assert!(matches!(
            store.save_status(&want.id).unwrap(),
            RecordSaveStatus::Failed(_)
        ));
        assert!(store.durability_error(&want.id).is_some());
        store.with(&want.id, |doc| doc.put(ROOT, "body", "latest").unwrap());
        store.tick();
        let expected = store
            .with(&want.id, automerge::AutoCommit::get_heads)
            .unwrap();
        std::fs::remove_dir(&path).unwrap();
        store.try_flush().unwrap();
        assert_eq!(settled(&store, &want.id), RecordSaveStatus::Saved);
        assert_eq!(store.durability_error(&want.id), None);
        drop(store);
        let store = owners.open("so.tilo.test", "room-a").unwrap();
        store.try_open_record(&want).unwrap();
        assert_eq!(body(&store, &want), "latest");
        assert_eq!(
            store
                .with(&want.id, automerge::AutoCommit::get_heads)
                .unwrap(),
            expected
        );
    }
}
