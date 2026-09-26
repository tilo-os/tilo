//! Genuine SessionStore directory/sentinel regressions; no owner schema or sender fixtures.
use super::*;
use std::cell::RefCell;
use std::os::unix::fs::{PermissionsExt, symlink};

thread_local! {
    static AFTER_OPEN: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        AFTER_OPEN.with(|hook| hook.borrow_mut().take());
    }
}
fn after_open<T>(hook: impl FnOnce() + 'static, action: impl FnOnce() -> T) -> T {
    AFTER_OPEN.with(|slot| assert!(slot.borrow_mut().replace(Box::new(hook)).is_none()));
    let _reset = Reset;
    action()
}
pub(super) fn directory_opened() {
    let hook = AFTER_OPEN.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}
fn private_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn seed() -> (tempfile::TempDir, PathBuf, SessionStore) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("owner");
    let mut store = SessionStore::create(&root).unwrap();
    store.commit(b"original signed aggregate bytes").unwrap();
    (temporary, root, store)
}
fn replace_root(root: &Path, retained: &Path) {
    fs::rename(root, retained).unwrap();
    private_dir(root).unwrap();
    private_file(&root.join("session.bin"), b"replacement aggregate");
}
#[test]
fn store_fd_read_uses_original_after_root_rename_and_replacement() {
    let (temporary, root, store) = seed();
    replace_root(&root, &temporary.path().join("retained"));
    assert_eq!(store.read().unwrap(), b"original signed aggregate bytes");
    assert_eq!(
        fs::read(root.join("session.bin")).unwrap(),
        b"replacement aggregate"
    );
}
#[test]
fn store_fd_commit_and_barrier_use_original_after_root_rename() {
    let (temporary, root, mut store) = seed();
    let retained = temporary.path().join("retained");
    replace_root(&root, &retained);
    store.commit(b"next exact signed bytes").unwrap();
    assert_eq!(
        fs::read(retained.join("session.bin")).unwrap(),
        b"next exact signed bytes"
    );
    assert_eq!(
        fs::read(root.join("session.bin")).unwrap(),
        b"replacement aggregate"
    );
    assert_eq!(
        store.acknowledged_digest().unwrap(),
        *blake3::hash(b"next exact signed bytes").as_bytes()
    );
    drop(store);
    assert_eq!(
        SessionStore::open(&retained).unwrap().read().unwrap(),
        b"next exact signed bytes"
    );
}
#[test]
fn store_fd_open_binds_lock_to_the_original_opened_directory() {
    let (temporary, root, store) = seed();
    drop(store);
    let retained = temporary.path().join("retained");
    let moved_root = root.clone();
    let moved_retained = retained.clone();
    let store = after_open(
        move || replace_root(&moved_root, &moved_retained),
        || SessionStore::open(&root),
    )
    .unwrap();
    let original_lock = File::open(retained.join("owner.lock")).unwrap();
    assert_eq!(
        atomic_file::FileIdentity::of(&store._lock.file).unwrap(),
        atomic_file::FileIdentity::of(&original_lock).unwrap(),
        "held directory must not be paired with a replacement-root sentinel"
    );
    assert!(!root.join("owner.lock").exists());
    assert_eq!(store.read().unwrap(), b"original signed aggregate bytes");
}
#[test]
fn store_fd_open_rechecks_the_actual_directory_privacy() {
    let (_temporary, root, store) = seed();
    drop(store);
    let changed = root.clone();
    let opened = after_open(
        move || fs::set_permissions(changed, fs::Permissions::from_mode(0o777)).unwrap(),
        || SessionStore::open(&root),
    );
    assert!(
        opened.is_err(),
        "opened directory lost private custody before admission"
    );
}
#[test]
fn store_fd_replaced_named_lock_refuses_read_and_commit() {
    let (_temporary, root, mut store) = seed();
    fs::rename(root.join("owner.lock"), root.join("previous.lock")).unwrap();
    private_file(&root.join("owner.lock"), b"");
    assert!(
        store.read().is_err(),
        "detached sentinel cannot authorize a read"
    );
    assert!(store.commit(b"must not publish").is_err());
    assert!(store.is_poisoned());
    assert!(store.acknowledged_digest().is_err());
    assert_eq!(
        fs::read(root.join("session.bin")).unwrap(),
        b"original signed aggregate bytes"
    );
}
#[test]
fn store_fd_owner_lock_rejects_public_and_multiply_linked_files() {
    for hardlink in [false, true] {
        let (temporary, root, store) = seed();
        drop(store);
        if hardlink {
            fs::hard_link(root.join("owner.lock"), temporary.path().join("alias.lock")).unwrap();
        } else {
            fs::set_permissions(root.join("owner.lock"), fs::Permissions::from_mode(0o644))
                .unwrap();
        }
        assert!(
            SessionStore::open(&root).is_err(),
            "invalid named owner lock admitted"
        );
    }
}
#[test]
fn store_fd_aggregate_child_refuses_symlink_nonregular_and_multiple_names() {
    for kind in 0..3 {
        let (temporary, root, store) = seed();
        fs::remove_file(root.join("session.bin")).unwrap();
        match kind {
            0 => {
                let outside = temporary.path().join("outside.bin");
                private_file(&outside, b"outside");
                symlink(outside, root.join("session.bin")).unwrap();
            }
            1 => fs::create_dir(root.join("session.bin")).unwrap(),
            _ => {
                private_file(&root.join("session.bin"), b"linked");
                fs::hard_link(root.join("session.bin"), temporary.path().join("alias.bin"))
                    .unwrap();
            }
        }
        assert!(
            store.read().is_err(),
            "invalid actual aggregate child admitted"
        );
    }
}
#[test]
fn store_fd_actual_faults_preserve_poison_and_acknowledgement_boundary() {
    for point in [
        test_fault::Point::BeforeWrite,
        test_fault::Point::AfterAtomicWrite,
        test_fault::Point::DirectoryBarrier,
    ] {
        let (_temporary, root, mut store) = seed();
        let original = store.acknowledged_digest().unwrap();
        let error =
            test_fault::with_fault(&root, point, || store.commit(b"candidate")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected session barrier failure")
        );
        assert!(store.is_poisoned());
        assert!(store.acknowledged_digest().is_err());
        assert_eq!(store.acknowledged_digest, Some(original));
        assert!(store.commit(b"must not retry as healthy").is_err());
        drop(store);
        let mut reopened = SessionStore::open(&root).unwrap();
        let actual = reopened.read().unwrap();
        assert_eq!(
            actual,
            if point == test_fault::Point::BeforeWrite {
                b"original signed aggregate bytes".as_slice()
            } else {
                b"candidate".as_slice()
            }
        );
        assert!(reopened.acknowledged_digest().is_err());
        reopened.commit(&actual).unwrap();
        assert_eq!(
            reopened.acknowledged_digest().unwrap(),
            *blake3::hash(&actual).as_bytes()
        );
    }
}

fn actual_publication_error(error: anyhow::Error) -> atomic_file::PublicationError {
    match error.downcast::<atomic_file::PublicationError>() {
        Ok(error) => error,
        Err(error) => *error
            .downcast::<std::io::Error>()
            .expect("actual native IO error")
            .into_inner()
            .expect("retained typed publication error")
            .downcast::<atomic_file::PublicationError>()
            .expect("original native publication custody"),
    }
}
#[test]
fn store_fd_actual_atomic_failures_retain_original_commit_custody() {
    use atomic_file::test_support::{Checkpoint, with_failure};
    for point in [Checkpoint::BeforePublish, Checkpoint::BeforeCommit] {
        let (temporary, root, mut store) = seed();
        let original_digest = store.acknowledged_digest().unwrap();
        let error = with_failure(point, || store.commit(b"candidate")).unwrap_err();
        let publication = actual_publication_error(error);
        assert!(publication.io_error().to_string().contains("injected"));
        assert_eq!(
            publication.state(),
            if point == Checkpoint::BeforePublish {
                atomic_file::PublicationState::NotPublished
            } else {
                atomic_file::PublicationState::PublishedDurabilityUnknown
            }
        );
        assert!(store.is_poisoned());
        assert_eq!(store.acknowledged_digest, Some(original_digest));
        assert!(store.acknowledged_digest().is_err());
        let retained = temporary.path().join("retained");
        replace_root(&root, &retained);
        if point == Checkpoint::BeforeCommit {
            publication.retry_commit().unwrap();
            assert_eq!(
                fs::read(retained.join("session.bin")).unwrap(),
                b"candidate"
            );
            assert!(
                store.acknowledged_digest().is_err(),
                "physical retry does not revive the poisoned owner"
            );
        } else {
            assert!(publication.retry_commit().is_err());
            assert_eq!(
                fs::read(retained.join("session.bin")).unwrap(),
                b"original signed aggregate bytes"
            );
        }
        assert_eq!(
            fs::read(root.join("session.bin")).unwrap(),
            b"replacement aggregate"
        );
    }
}
#[test]
fn store_fd_lock_replacement_during_publication_cannot_be_acknowledged() {
    use atomic_file::test_support::{Checkpoint, with_observer};
    let (_temporary, root, mut store) = seed();
    let changed = root.clone();
    let original_digest = store.acknowledged_digest().unwrap();
    let result = with_observer(
        move |event| {
            if event.checkpoint == Checkpoint::BeforeCommit {
                fs::rename(changed.join("owner.lock"), changed.join("previous.lock")).unwrap();
                private_file(&changed.join("owner.lock"), b"");
            }
        },
        || store.commit(b"published before final custody check"),
    );
    assert!(
        result.is_err(),
        "a detached owner lock cannot acknowledge publication"
    );
    assert!(store.is_poisoned());
    assert!(store.acknowledged_digest().is_err());
    assert_eq!(store.acknowledged_digest, Some(original_digest));
}

#[test]
fn store_fd_validation_handles_cannot_release_original_exclusive_custody() {
    let (_temporary, root, mut store) = seed();
    for _ in 0..3 {
        assert_eq!(store.read().unwrap(), b"original signed aggregate bytes");
        store.commit(b"original signed aggregate bytes").unwrap();
        assert!(SessionStore::open(&root).is_err());
        assert!(custody(&root.join("owner.lock")).is_err());
    }
    drop(store);
    assert!(SessionStore::open(&root).is_ok());
}

#[test]
fn store_fd_sentinel_creation_preserves_real_publication_failures() {
    use atomic_file::test_support::{Checkpoint, with_failure};
    for point in [Checkpoint::BeforePublish, Checkpoint::BeforeCommit] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("owner");
        private_dir(&root).unwrap();
        let error = match with_failure(point, || SessionStore::open(&root)) {
            Ok(_) => panic!("uncertain sentinel creation cannot admit an owner"),
            Err(error) => error,
        };
        let publication = actual_publication_error(error);
        assert_eq!(
            publication.state(),
            if point == Checkpoint::BeforePublish {
                atomic_file::PublicationState::NotPublished
            } else {
                atomic_file::PublicationState::PublishedDurabilityUnknown
            }
        );
        if point == Checkpoint::BeforeCommit {
            publication.retry_commit().unwrap();
        }
        let reopened = SessionStore::open(&root).unwrap();
        assert!(
            reopened.read().is_err(),
            "missing aggregate is never initialized by open"
        );
        assert!(reopened.acknowledged_digest().is_err());
    }
}

#[test]
fn store_fd_sentinel_if_absent_race_reopens_only_actual_winner() {
    use atomic_file::test_support::{Checkpoint, with_observer};
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("owner");
    private_dir(&root).unwrap();
    let changed = root.clone();
    let store = with_observer(
        move |event| {
            if event.checkpoint == Checkpoint::BeforePublish {
                private_file(&changed.join("owner.lock"), b"actual competing sentinel");
            }
        },
        || SessionStore::open(&root),
    )
    .unwrap();
    let winner = File::open(root.join("owner.lock")).unwrap();
    assert_eq!(
        atomic_file::FileIdentity::of(&winner).unwrap(),
        atomic_file::FileIdentity::of(&store._lock.file).unwrap()
    );
    assert_eq!(
        fs::read(root.join("owner.lock")).unwrap(),
        b"actual competing sentinel"
    );
    assert!(SessionStore::open(&root).is_err());
}
