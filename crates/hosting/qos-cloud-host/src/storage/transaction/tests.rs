use super::*;
use crate::{StoreSaveOutcome, UncertainStoreCommit};
use atomic_file::{FileIdentity, test_support::Checkpoint};
use qos_action_broker::peer_actions::OwnerPolicyClock;

fn selected() -> (tempfile::TempDir, crate::Store) {
    let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    (dir, store)
}

#[test]
fn store_transaction_keeps_original_lock_and_refuses_missing_sentinel() {
    let (dir, store) = selected();
    let original = FileIdentity::of(&store.directory).unwrap();
    assert!(matches!(
        ExistingStoreLock::acquire(dir.path(), File::open(dir.path()).unwrap(), false),
        Err(LockFailure::Busy)
    ));
    drop(store);
    let locked =
        ExistingStoreLock::acquire(dir.path(), File::open(dir.path()).unwrap(), false).unwrap();
    assert_eq!(FileIdentity::of(locked.directory()).unwrap(), original);
    assert!(matches!(
        crate::delivery::access::try_open(dir.path(), "agent-admission"),
        Err(crate::delivery::access::AccessError::Busy)
    ));
    drop(locked);
    std::fs::remove_file(dir.path().join("state.lock")).unwrap();
    assert!(matches!(
        ExistingStoreLock::acquire(dir.path(), File::open(dir.path()).unwrap(), false),
        Err(LockFailure::Unavailable(_))
    ));
    assert!(!dir.path().join("state.lock").exists());
}

#[test]
fn store_transaction_publication_keeps_original_directory_after_rename() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, store) = selected();
    let generation = store.state.generation;
    let bytes = std::fs::read(dir.path().join("state.json")).unwrap();
    drop(store);
    let locked =
        ExistingStoreLock::acquire(dir.path(), File::open(dir.path()).unwrap(), false).unwrap();
    let holder = tempfile::tempdir().unwrap();
    let moved = holder.path().join("original");
    std::fs::rename(dir.path(), &moved).unwrap();
    std::fs::create_dir(dir.path()).unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(dir.path().join("state.json"), b"ambient replacement").unwrap();
    let failed = Cell::new(None);
    let result = publish_bytes(
        locked.directory(),
        locked.root(),
        &failed,
        None,
        generation,
        &bytes,
    );
    let retained = std::fs::read(moved.join("state.json")).unwrap();
    let replacement = std::fs::read(dir.path().join("state.json")).unwrap();
    drop(locked);
    std::fs::remove_dir_all(dir.path()).unwrap();
    std::fs::rename(&moved, dir.path()).unwrap();
    result.unwrap();
    assert_eq!(retained, bytes);
    assert_eq!(replacement, b"ambient replacement");
    assert!(failed.get().is_none());
}

#[test]
fn store_transaction_publication_preserves_outcomes_and_retired_clock_refusal() {
    for (point, outcome) in [
        (Checkpoint::BeforePublish, StoreSaveOutcome::NotPublished),
        (
            Checkpoint::BeforeCommit,
            StoreSaveOutcome::PublishedDurabilityUnknown,
        ),
    ] {
        let (dir, store) = selected();
        let generation = store.state.generation;
        let bytes = std::fs::read(dir.path().join("state.json")).unwrap();
        drop(store);
        let locked =
            ExistingStoreLock::acquire(dir.path(), File::open(dir.path()).unwrap(), false).unwrap();
        let failed = Cell::new(None);
        let error = crate::storage::test_support::with_save_failure(dir.path(), point, || {
            publish_bytes(
                locked.directory(),
                locked.root(),
                &failed,
                None,
                generation,
                &bytes,
            )
        })
        .unwrap_err();
        assert_eq!(
            failed.get(),
            Some(StoreCommitFailure {
                generation,
                outcome
            })
        );
        assert_eq!(
            error.is::<UncertainStoreCommit>(),
            outcome != StoreSaveOutcome::NotPublished
        );
        assert!(
            publish_bytes(
                locked.directory(),
                locked.root(),
                &failed,
                None,
                generation + 1,
                b"must not publish"
            )
            .is_err()
        );
        assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), bytes);
    }
    let (dir, store) = selected();
    let bytes = std::fs::read(dir.path().join("state.json")).unwrap();
    let generation = store.state.generation;
    drop(store);
    let locked =
        ExistingStoreLock::acquire(dir.path(), File::open(dir.path()).unwrap(), false).unwrap();
    let clock = OwnerPolicyClock::new().unwrap();
    let origin = clock.new_origin();
    let time = origin.capture().unwrap();
    origin.invalidate();
    let failed = Cell::new(None);
    assert!(
        publish_bytes(
            locked.directory(),
            locked.root(),
            &failed,
            Some(&time),
            generation,
            b"must not publish"
        )
        .is_err()
    );
    assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), bytes);
    assert!(failed.get().is_none());
}
