use super::*;
use crate::portable_storage::{PortableStorage, StorageKey};
use automerge::resource_limits::NativeAllowance;
fn cache() -> (PortableStorage, StorageKey) {
    let cache = PortableStorage::new("profile", "device").unwrap();
    let key = StorageKey::PrivateActions {
        workspace: "room".into(),
    };
    cache.put(key.clone(), b"original".to_vec(), None).unwrap();
    (cache, key)
}
fn allowance() -> NativeAllowance {
    NativeAllowance::new(32 * 1024 * 1024).unwrap()
}
fn capture(cache: &PortableStorage, budget: &NativeAllowance) -> PrivateActionsCapture {
    let n = PrivateActionsCapture::required_bytes(cache, "room")
        .unwrap()
        .unwrap();
    PrivateActionsCapture::capture(cache, "room", budget.reserve(n).unwrap())
        .unwrap()
        .unwrap()
}
#[test]
fn private_actions_storage_capture_exact_and_paid() {
    let (cache, key) = cache();
    let budget = allowance();
    let baseline = budget.retained_bytes();
    let (_, stats) =
        crate::semantic_document_read::retirement::measure_test_allocations(&budget, || {
            let saved = capture(&cache, &budget);
            assert_eq!(saved.bytes(), b"original");
            assert!(saved.is_current());
            drop(saved);
        });
    assert!(!stats.underfunded);
    assert_eq!(stats.live, 0);
    assert_eq!(budget.retained_bytes(), baseline);
    let saved = capture(&cache, &budget);
    cache.put(key, b"newer".to_vec(), None).unwrap();
    assert!(!saved.is_current());
}
#[test]
fn private_actions_storage_capture_recounts_and_actor_refuses() {
    let (cache, key) = cache();
    let budget = allowance();
    let n = PrivateActionsCapture::required_bytes(&cache, "room")
        .unwrap()
        .unwrap();
    cache.put(key, vec![1; 4096], None).unwrap();
    assert!(matches!(
        PrivateActionsCapture::capture(&cache, "room", budget.reserve(n).unwrap()),
        Err(SenderStorageCode::Capacity)
    ));
    let other = PortableStorage::new("profile", "other").unwrap();
    other
        .put(
            StorageKey::PrivateActions {
                workspace: "room".into(),
            },
            vec![1],
            Some(vec![2]),
        )
        .unwrap();
    assert_eq!(
        PrivateActionsCapture::required_bytes(&other, "room"),
        Err(SenderStorageCode::InvalidScope)
    );
}

#[test]
fn private_actions_storage_finish_moves_paid_backing_and_preserves_generic_put() {
    let (cache, key) = cache();
    let budget = allowance();
    let baseline = budget.retained_bytes();
    let mut held = capture(&cache, &budget);
    let paid = budget.retained_bytes();
    assert_eq!(held.finish().unwrap(), held.revision());
    assert!(cache.flush(&key).is_ok());
    drop(held);
    assert_eq!(budget.retained_bytes(), paid);
    cache.put(key.clone(), b"newer".to_vec(), None).unwrap();
    assert_eq!(budget.retained_bytes(), paid);
    assert!(cache.flush(&key).is_err());
    // Status keeps its ordinary independently owned public copy semantics.
    assert_eq!(
        cache.status(&key).unwrap().committed.unwrap().1.bytes,
        b"original"
    );
    let old = capture(&cache, &budget);
    cache.put(key.clone(), b"latest".to_vec(), None).unwrap();
    let mut stale = old;
    assert_eq!(stale.finish(), Err(SenderStorageCode::StaleCapture));
    assert!(cache.flush(&key).is_err());
    drop(stale);
    drop(cache);
    assert_eq!(budget.retained_bytes(), baseline);
}
fn scope() -> SenderScope {
    SenderScope::native(uuid::Uuid::from_bytes([1; 16]), [2; 32], [3; 32]).unwrap()
}
#[test]
fn private_actions_storage_envelope_exact_bounded_scope_and_corruption() {
    let scope = scope();
    let budget = allowance();
    let payload = b"opaque private outbox";
    let (_, stats) =
        crate::semantic_document_read::retirement::measure_test_allocations(&budget, || {
            let credit = budget
                .reserve(codec::required_bytes(&scope, payload.len()).unwrap())
                .unwrap();
            let mut bytes = codec::encode(&scope, [4; 16], 1, 9, payload).unwrap();
            let parsed = codec::decode(&scope, &bytes).unwrap();
            assert_eq!(parsed.payload, payload);
            assert_eq!(parsed.sequence, 1);
            assert_eq!(parsed.revision, 9);
            assert_eq!(parsed.instance, [4; 16]);
            let foreign =
                SenderScope::native(uuid::Uuid::from_bytes([8; 16]), [2; 32], [3; 32]).unwrap();
            assert!(codec::decode(&foreign, &bytes).is_err());
            for i in 0..bytes.len() {
                bytes[i] ^= 1;
                assert!(codec::decode(&scope, &bytes).is_err());
                bytes[i] ^= 1;
            }
            drop(bytes);
            drop(credit);
        });
    assert!(!stats.underfunded);
    assert_eq!(stats.live, 0);
    assert!(codec::required_bytes(&scope, codec::MAX_PAYLOAD + 1).is_err());
    assert!(codec::encode(&scope, [0; 16], 1, 1, payload).is_err());
    assert!(codec::encode(&scope, [4; 16], 0, 1, payload).is_err());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(in crate::host_worker::private_actions) mod consuming {
    use super::*;
    use atomic_file::{Privacy, PublishMode, StorageDirectory};
    use std::os::unix::fs::PermissionsExt;
    use std::{ffi::OsStr, fs};
    const CHILD: &str = "private-actions-v1";
    const ROOM: &str = "01010101-0101-0101-0101-010101010101";
    fn directory(path: &std::path::Path) -> StorageDirectory {
        StorageDirectory::open(path).unwrap()
    }
    fn fixture() -> tempfile::TempDir {
        fixture_payload(b"initial")
    }
    // The qualification runner supplies an explicit persistent XFS parent.
    // Ordinary Linux unit tests may still use their normal temporary filesystem;
    // successful default-temp tests make no XFS or reboot durability claim.
    fn fixture_container() -> tempfile::TempDir {
        #[cfg(target_os = "linux")]
        if let Some(parent) = std::env::var_os("QOS_PRIVATE_ACTIONS_TEST_ROOT") {
            use std::os::fd::AsRawFd;
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
            let parent = std::path::PathBuf::from(parent);
            assert!(
                parent.is_absolute(),
                "explicit fixture root must be absolute"
            );
            let open_directory = |path: &std::path::Path| {
                fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(path)
                    .expect("existing actual fixture directory")
            };
            let _parent = open_directory(&parent);
            let root = tempfile::tempdir_in(&parent).expect("create fixture inside explicit root");
            let directory = open_directory(root.path());
            let mut statistics = std::mem::MaybeUninit::<libc::statfs>::uninit();
            assert_eq!(
                unsafe { libc::fstatfs(directory.as_raw_fd(), statistics.as_mut_ptr()) },
                0
            );
            let statistics = unsafe { statistics.assume_init() };
            assert_eq!(
                statistics.f_type as u64, 0x5846_5342,
                "explicit fixture must be actual XFS"
            );
            let metadata = directory.metadata().unwrap();
            eprintln!(
                "private-actions XFS fixture={} device={} inode={} magic={:#x}",
                root.path().display(),
                metadata.dev(),
                metadata.ino(),
                statistics.f_type
            );
            return root;
        }
        tempfile::tempdir().unwrap()
    }
    fn fixture_payload(payload: &[u8]) -> tempfile::TempDir {
        let root = fixture_container();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let parent = directory(root.path());
        parent
            .publish(
                OsStr::new(LOCK_NAME),
                b"",
                PublishMode::IfAbsent,
                Privacy::OwnerOnly,
            )
            .unwrap();
        fs::create_dir(root.path().join(CHILD)).unwrap();
        fs::set_permissions(root.path().join(CHILD), fs::Permissions::from_mode(0o700)).unwrap();
        let bytes = codec::encode(&scope(), [4; 16], 1, 17, payload).unwrap();
        directory(&root.path().join(CHILD))
            .publish(
                OsStr::new(FILE_NAME),
                &bytes,
                PublishMode::IfAbsent,
                Privacy::OwnerOnly,
            )
            .unwrap();
        root
    }
    #[test]
    fn private_actions_storage_consuming_real_existing_read() {
        let root = fixture();
        let budget = allowance();
        let cache = PortableStorage::new("profile", "device").unwrap();
        let file = PrivateActionsFile::from_held(
            directory(root.path()),
            scope(),
            &cache,
            budget.reserve(4096).unwrap(),
        )
        .unwrap_or_else(|e| panic!("actual existing owner must open: {:?}", e.code()));
        let read_credit = budget.reserve(file.read_required_bytes().unwrap()).unwrap();
        let read = match file.read_existing(read_credit) {
            PrivateReadOutcome::Committed(read) => read,
            PrivateReadOutcome::Failed(e) => {
                panic!("actual existing commit failed: {:?}", e.code())
            }
        };
        assert_eq!(read.payload(), b"initial");
        assert_eq!(read.sequence(), 1);
    }

    fn opened(
        root: &std::path::Path,
        cache: &PortableStorage,
        budget: &NativeAllowance,
    ) -> PrivateActionsFile {
        PrivateActionsFile::from_held(
            directory(root),
            scope(),
            cache,
            budget.reserve(4096).unwrap(),
        )
        .unwrap_or_else(|e| panic!("open {:?}", e.code()))
    }
    fn read(file: PrivateActionsFile, budget: &NativeAllowance) -> CommittedPrivateRead {
        let credit = budget.reserve(file.read_required_bytes().unwrap()).unwrap();
        match file.read_existing(credit) {
            PrivateReadOutcome::Committed(v) => v,
            PrivateReadOutcome::Failed(e) => panic!("read {:?}", e.code()),
        }
    }
    fn hydrate(read: CommittedPrivateRead, budget: &NativeAllowance) -> CurrentPrivateState {
        let credit = budget
            .reserve(read.hydrate_required_bytes().unwrap())
            .unwrap();
        read.hydrate_current(credit)
            .unwrap_or_else(|e| panic!("hydrate {:?}", e.code()))
    }
    #[test]
    fn private_actions_storage_consuming_hydrates_one_backing_and_commits_paid_queue() {
        let root = fixture();
        let budget = allowance();
        let cache = PortableStorage::new("profile", "device").unwrap();
        let initial = read(opened(root.path(), &cache, &budget), &budget);
        let backing = initial.bytes.as_ptr();
        let state = hydrate(initial, &budget);
        {
            let view = state.borrow_current().unwrap();
            assert_eq!(view.payload(), b"initial");
            assert_eq!(view.payload().as_ptr(), backing);
            assert_eq!(view.instance(), &[4; 16]);
            assert_eq!(view.sequence(), 1);
        }
        // An unrelated key advances the global counter, never this entry's stamp.
        cache
            .put(
                StorageKey::PrivateActions {
                    workspace: "unrelated".into(),
                },
                b"other".to_vec(),
                None,
            )
            .unwrap();
        assert_eq!(state.borrow_current().unwrap().payload(), b"initial");
        let credit = budget.reserve(10).unwrap();
        let payload = b"new intent".to_vec();
        let file = match state
            .queue_owned(payload, credit)
            .unwrap_or_else(|e| panic!("queue {:?}", e.code()))
        {
            PrivateQueueOutcome::Pending(f) => f,
            _ => panic!("changed bytes must be pending"),
        };
        let key = StorageKey::PrivateActions {
            workspace: ROOM.into(),
        };
        assert!(cache.flush(&key).is_err());
        let n = PrivateActionsCapture::required_bytes(file.cache(), file.workspace())
            .unwrap()
            .unwrap();
        let captured = PrivateActionsCapture::capture(
            file.cache(),
            file.workspace(),
            budget.reserve(n).unwrap(),
        )
        .unwrap()
        .unwrap();
        let pointer = captured.bytes().as_ptr();
        let n = file.prepare_required_bytes(&captured).unwrap();
        let prepared = file
            .prepare(captured, budget.reserve(n).unwrap())
            .unwrap_or_else(|e| panic!("prepare {:?}", e.code()));
        let committed = match prepared.publish() {
            PrivateWriteOutcome::Committed(v) => v,
            PrivateWriteOutcome::Failed(e) => panic!("physical {:?}", e.publication_error()),
            PrivateWriteOutcome::Refused(e) => panic!("refused {:?}", e.code()),
        };
        assert!(cache.flush(&key).is_err());
        let state = committed
            .finish_current()
            .unwrap_or_else(|e| panic!("finish {:?}", e.code()));
        assert!(cache.flush(&key).is_ok());
        {
            let view = state.borrow_current().unwrap();
            assert_eq!(view.payload(), b"new intent");
            assert_eq!(view.payload().as_ptr(), pointer);
            assert_eq!(view.sequence(), 2);
        }
        drop(state);
        drop(cache);
        let reopened = PortableStorage::new("profile", "device").unwrap();
        let state = hydrate(
            read(opened(root.path(), &reopened, &budget), &budget),
            &budget,
        );
        assert_eq!(state.borrow_current().unwrap().payload(), b"new intent");
        assert_eq!(state.borrow_current().unwrap().sequence(), 2);
    }
    fn state(
        root: &std::path::Path,
        cache: &PortableStorage,
        budget: &NativeAllowance,
    ) -> CurrentPrivateState {
        hydrate(read(opened(root, cache, budget), budget), budget)
    }
    fn queue(
        state: CurrentPrivateState,
        payload: &[u8],
        budget: &NativeAllowance,
    ) -> PrivateActionsFile {
        let credit = budget.reserve(payload.len()).unwrap();
        match state
            .queue_owned(payload.to_vec(), credit)
            .unwrap_or_else(|e| panic!("queue {:?}", e.code()))
        {
            PrivateQueueOutcome::Pending(file) => file,
            _ => panic!("expected changed payload"),
        }
    }
    fn prepared(file: PrivateActionsFile, budget: &NativeAllowance) -> PreparedPrivateWrite {
        let n = PrivateActionsCapture::required_bytes(file.cache(), file.workspace())
            .unwrap()
            .unwrap();
        let capture = PrivateActionsCapture::capture(
            file.cache(),
            file.workspace(),
            budget.reserve(n).unwrap(),
        )
        .unwrap()
        .unwrap();
        let n = file.prepare_required_bytes(&capture).unwrap();
        file.prepare(capture, budget.reserve(n).unwrap())
            .unwrap_or_else(|e| panic!("prepare {:?}", e.code()))
    }
    fn committed(write: PrivateWriteOutcome) -> CommittedPrivateWrite {
        match write {
            PrivateWriteOutcome::Committed(v) => v,
            PrivateWriteOutcome::Failed(e) => panic!("publish {:?}", e.publication_error()),
            PrivateWriteOutcome::Refused(e) => panic!("refused {:?}", e.code()),
        }
    }
    fn key() -> StorageKey {
        StorageKey::PrivateActions {
            workspace: ROOM.into(),
        }
    }
    #[test]
    fn private_actions_storage_consuming_delayed_capture_cannot_switch_queued_revision() {
        let root = fixture();
        let budget = allowance();
        let cache = PortableStorage::new("profile", "device").unwrap();
        let file = queue(
            state(root.path(), &cache, &budget),
            b"first command",
            &budget,
        );
        cache
            .put(key(), b"replacement command".to_vec(), None)
            .unwrap();
        let n = PrivateActionsCapture::required_bytes(file.cache(), file.workspace())
            .unwrap()
            .unwrap();
        let newer = PrivateActionsCapture::capture(
            file.cache(),
            file.workspace(),
            budget.reserve(n).unwrap(),
        )
        .unwrap()
        .unwrap();
        let n = file.prepare_required_bytes(&newer).unwrap();
        assert!(
            file.prepare(newer, budget.reserve(n).unwrap()).is_err(),
            "a delayed queue must not commit a newer command"
        );
    }
    #[test]
    fn private_actions_storage_consuming_stale_before_io_and_after_barrier() {
        let root = fixture();
        let budget = allowance();
        let cache = PortableStorage::new("profile", "device").unwrap();
        let write = prepared(
            queue(
                state(root.path(), &cache, &budget),
                b"first command",
                &budget,
            ),
            &budget,
        );
        let before = fs::read(root.path().join(CHILD).join(FILE_NAME)).unwrap();
        cache.put(key(), b"newer".to_vec(), None).unwrap();
        let refused = match write.publish() {
            PrivateWriteOutcome::Refused(v) => v,
            _ => panic!("stale publication"),
        };
        assert_eq!(refused.code(), SenderStorageCode::StaleCapture);
        assert_eq!(
            fs::read(root.path().join(CHILD).join(FILE_NAME)).unwrap(),
            before
        );
        drop(refused);
        let cache2 = PortableStorage::new("profile", "device").unwrap();
        let done = committed(
            prepared(
                queue(state(root.path(), &cache2, &budget), b"committed", &budget),
                &budget,
            )
            .publish(),
        );
        cache2.put(key(), b"latest".to_vec(), None).unwrap();
        let refusal = match done.finish_current() {
            Err(v) => v,
            Ok(_) => panic!("stale finish"),
        };
        assert_eq!(refusal.code(), SenderStorageCode::StaleCapture);
        assert!(cache2.flush(&key()).is_err());
        let e = PrivateActionsFile::from_held(
            directory(root.path()),
            scope(),
            &cache2,
            budget.reserve(0).unwrap(),
        )
        .err()
        .expect("lease must stay held");
        assert_eq!(e.code(), SenderStorageCode::Busy);
    }
    #[test]
    fn private_actions_storage_consuming_actual_publication_failure_and_original_retry() {
        use atomic_file::test_support::{Checkpoint, with_failure};
        for point in [
            Checkpoint::BeforeDataFlush,
            Checkpoint::BeforePublish,
            Checkpoint::BeforeCommit,
        ] {
            let root = fixture();
            let budget = allowance();
            let cache = PortableStorage::new("profile", "device").unwrap();
            let write = prepared(
                queue(
                    state(root.path(), &cache, &budget),
                    b"private command",
                    &budget,
                ),
                &budget,
            );
            let failed = match with_failure(point, || write.publish()) {
                PrivateWriteOutcome::Failed(v) => v,
                _ => panic!("actual failure must remain physical"),
            };
            assert!(
                failed
                    .publication_error()
                    .io_error()
                    .to_string()
                    .contains("injected")
            );
            assert!(cache.flush(&key()).is_err());
            let e = PrivateActionsFile::from_held(
                directory(root.path()),
                scope(),
                &cache,
                budget.reserve(0).unwrap(),
            )
            .err()
            .expect("failed phase retains lease");
            assert_eq!(e.code(), SenderStorageCode::Busy);
            drop(e);
            if point == Checkpoint::BeforeCommit {
                assert_eq!(failed.state(), PublicationState::PublishedDurabilityUnknown);
                let done = committed(failed.retry_commit());
                assert!(cache.flush(&key()).is_err());
                let state = done
                    .finish_current()
                    .unwrap_or_else(|e| panic!("retry finish {:?}", e.code()));
                assert_eq!(
                    state.borrow_current().unwrap().payload(),
                    b"private command"
                );
            } else {
                assert_eq!(failed.state(), PublicationState::NotPublished);
                assert!(matches!(
                    failed.retry_commit(),
                    PrivateWriteOutcome::Failed(_)
                ));
            }
        }
    }
    #[test]
    fn private_actions_storage_consuming_existing_retry_refuses_replaced_identity() {
        use atomic_file::test_support::{ExistingCheckpoint, with_existing_hooks};
        let root = fixture();
        let budget = allowance();
        let cache = PortableStorage::new("profile", "device").unwrap();
        let file = opened(root.path(), &cache, &budget);
        let credit = budget.reserve(file.read_required_bytes().unwrap()).unwrap();
        let failed = match with_existing_hooks(
            Some(ExistingCheckpoint::BeforeNamespaceFlush),
            |_| {},
            || file.read_existing(credit),
        ) {
            PrivateReadOutcome::Failed(v) => v,
            _ => panic!("expected real existing commit failure"),
        };
        assert!(failed.existing_commit_error().is_some());
        let changed = codec::encode(&scope(), [9; 16], 1, 0, b"foreign").unwrap();
        directory(&root.path().join(CHILD))
            .publish(
                OsStr::new(FILE_NAME),
                &changed,
                PublishMode::Replace,
                Privacy::OwnerOnly,
            )
            .unwrap();
        let failed = match failed.retry_commit() {
            PrivateReadOutcome::Failed(v) => v,
            _ => panic!("replacement disclosed"),
        };
        assert!(failed.existing_commit_error().unwrap().source_changed());
    }
    #[test]
    fn private_actions_storage_consuming_hydrate_rechecks_bound_key_and_disk() {
        let root = fixture();
        let budget = allowance();
        let cache = PortableStorage::new("profile", "device").unwrap();
        let initial = read(opened(root.path(), &cache, &budget), &budget);
        let required = initial.hydrate_required_bytes().unwrap();
        let pointer = initial.bytes.as_ptr();
        let refused = initial
            .hydrate_current(budget.reserve(required - 1).unwrap())
            .err()
            .expect("undersized metadata");
        assert_eq!(refused.code(), SenderStorageCode::Capacity);
        let initial = refused.into_read();
        assert_eq!(initial.bytes.as_ptr(), pointer);
        assert_eq!(initial.payload(), b"initial");
        cache.put(key(), b"local pending".to_vec(), None).unwrap();
        let refused = initial
            .hydrate_current(budget.reserve(required).unwrap())
            .err()
            .expect("occupied key");
        assert_eq!(refused.code(), SenderStorageCode::StaleCapture);
        drop(refused);
        let other = PortableStorage::new("profile", "device").unwrap();
        let initial = read(opened(root.path(), &other, &budget), &budget);
        let credit = budget
            .reserve(initial.hydrate_required_bytes().unwrap())
            .unwrap();
        let changed = codec::encode(&scope(), [4; 16], 2, 0, b"changed").unwrap();
        directory(&root.path().join(CHILD))
            .publish(
                OsStr::new(FILE_NAME),
                &changed,
                PublishMode::Replace,
                Privacy::OwnerOnly,
            )
            .unwrap();
        let refused = initial
            .hydrate_current(credit)
            .err()
            .expect("disk changed after read");
        assert!(refused.io_error().is_some());
        assert!(other.get(&key()).is_none());
    }
    #[test]
    fn private_actions_storage_consuming_original_child_sentinel_and_cursor_fences() {
        for change in 0..3 {
            let root = fixture();
            let budget = allowance();
            let cache = PortableStorage::new("profile", "device").unwrap();
            let write = prepared(
                queue(state(root.path(), &cache, &budget), b"command", &budget),
                &budget,
            );
            match change {
                0 => {
                    fs::rename(root.path().join(CHILD), root.path().join("old-child")).unwrap();
                    fs::create_dir(root.path().join(CHILD)).unwrap();
                    fs::set_permissions(root.path().join(CHILD), fs::Permissions::from_mode(0o700))
                        .unwrap();
                }
                1 => {
                    directory(root.path())
                        .publish(
                            OsStr::new(LOCK_NAME),
                            b"",
                            PublishMode::Replace,
                            Privacy::OwnerOnly,
                        )
                        .unwrap();
                }
                _ => {
                    let changed = codec::encode(&scope(), [4; 16], 2, 0, b"changed").unwrap();
                    directory(&root.path().join(CHILD))
                        .publish(
                            OsStr::new(FILE_NAME),
                            &changed,
                            PublishMode::Replace,
                            Privacy::OwnerOnly,
                        )
                        .unwrap();
                }
            }
            let refused = match write.publish() {
                PrivateWriteOutcome::Refused(v) => v,
                _ => panic!("changed binding published"),
            };
            assert!(refused.io_error().is_some());
            assert!(cache.flush(&key()).is_err());
        }
    }
    #[test]
    fn private_actions_storage_consuming_exclusive_and_existing_only() {
        use atomic_file::lease::{FileLease, LeaseMode};
        let root = fixture();
        let budget = allowance();
        let cache = PortableStorage::new("profile", "device").unwrap();
        let lease = FileLease::try_acquire(
            directory(root.path())
                .open_read(OsStr::new(LOCK_NAME), Privacy::OwnerOnly)
                .unwrap(),
            LeaseMode::Shared,
        )
        .unwrap();
        let refused = PrivateActionsFile::from_held(
            directory(root.path()),
            scope(),
            &cache,
            budget.reserve(0).unwrap(),
        )
        .err()
        .expect("shared excludes actual exclusive");
        assert_eq!(refused.code(), SenderStorageCode::Busy);
        drop(refused);
        drop(lease);
        fs::remove_file(root.path().join(LOCK_NAME)).unwrap();
        let refused = PrivateActionsFile::from_held(
            directory(root.path()),
            scope(),
            &cache,
            budget.reserve(0).unwrap(),
        )
        .err()
        .expect("no missing sentinel creation");
        assert!(refused.io_error().is_some());
        assert!(!root.path().join(LOCK_NAME).exists());
    }

    pub(in crate::host_worker::private_actions) fn actual_state_fixture(
        payload: &[u8],
        budget: &NativeAllowance,
    ) -> (tempfile::TempDir, CurrentPrivateState, PortableStorage) {
        let root = fixture_payload(payload);
        let cache = PortableStorage::new("profile", "device").unwrap();
        let held = state(root.path(), &cache, budget);
        (root, held, cache)
    }
    #[test]
    fn private_actions_storage_consuming_paid_lifetime_and_current_aliases() {
        let root = fixture();
        let budget = allowance();
        let baseline = budget.retained_bytes();
        let cache = PortableStorage::new("profile", "device").unwrap();
        // Force nontrivial actual BTree insertion shape; initial ordinary entries are unqualified.
        for index in 0..80 {
            cache
                .put(
                    StorageKey::PrivateActions {
                        workspace: format!("ordinary-{index:03}"),
                    },
                    vec![index],
                    None,
                )
                .unwrap();
        }
        let original = directory(root.path());
        let (_, stats) =
            crate::semantic_document_read::retirement::measure_test_allocations(&budget, || {
                let file = PrivateActionsFile::from_held(
                    original,
                    scope(),
                    &cache,
                    budget.reserve(0).unwrap(),
                )
                .unwrap_or_else(|e| panic!("open {:?}", e.code()));
                let current = hydrate(read(file, &budget), &budget);
                assert_eq!(current.workspace(), ROOM);
                assert_eq!(current.borrow_current().unwrap().payload(), b"initial");
                let input_credit = budget.reserve(128 * 1024).unwrap();
                let input = vec![7; 128 * 1024];
                let file = match current
                    .queue_owned(input, input_credit)
                    .unwrap_or_else(|e| panic!("queue {:?}", e.code()))
                {
                    PrivateQueueOutcome::Pending(v) => v,
                    _ => panic!("pending"),
                };
                let capture_credit = budget
                    .reserve(file.capture_required_bytes().unwrap())
                    .unwrap();
                let capture = file.capture(capture_credit).unwrap();
                let encoded_credit = budget
                    .reserve(file.prepare_required_bytes(&capture).unwrap())
                    .unwrap();
                let done = committed(
                    file.prepare(capture, encoded_credit)
                        .unwrap_or_else(|e| panic!("prepare {:?}", e.code()))
                        .publish(),
                );
                let current = done
                    .finish_current()
                    .unwrap_or_else(|e| panic!("finish {:?}", e.code()));
                let view = current.borrow_current().unwrap();
                assert_eq!(view.payload().len(), 128 * 1024);
                drop(view);
                // Input/current/capture/envelope credits survive to their actual last backing owner.
                drop(current);
                drop(cache);
            });
        assert!(!stats.underfunded, "{stats:?}");
        assert_eq!(stats.live, 0, "{stats:?}");
        assert_eq!(budget.retained_bytes(), baseline);
    }
    #[test]
    fn private_actions_storage_consuming_pending_credit_survives_sender_drop_and_generic_ack_is_not_receipt()
     {
        let budget = allowance();
        let baseline = budget.retained_bytes();
        let (root, current, cache) = actual_state_fixture(b"initial", &budget);
        let file = queue(current, b"new private command", &budget);
        drop(file);
        assert!(budget.retained_bytes() > baseline);
        // Ordinary ACK may mark cache durable but has no physical provenance.
        let pending = cache
            .pending_writes()
            .into_iter()
            .find(|w| w.key == key())
            .unwrap();
        cache.acknowledge(&pending, Ok(())).unwrap();
        assert_eq!(
            codec::decode(
                &scope(),
                &fs::read(root.path().join(CHILD).join(FILE_NAME)).unwrap()
            )
            .unwrap()
            .payload,
            b"initial"
        );
        drop(pending);
        drop(cache);
        assert_eq!(budget.retained_bytes(), baseline);
    }
    #[test]
    fn private_actions_storage_consuming_real_fixture_borrow_and_foreign_cache_refusal() {
        let budget = allowance();
        let (root, current, cache) = actual_state_fixture(b"opaque", &budget);
        assert_eq!(current.borrow_current().unwrap().payload(), b"opaque");
        let file = queue(current, b"queued", &budget);
        let other = PortableStorage::new("profile", "device").unwrap();
        other.put(key(), b"queued".to_vec(), None).unwrap();
        let n = PrivateActionsCapture::required_bytes(&other, ROOM)
            .unwrap()
            .unwrap();
        let foreign = PrivateActionsCapture::capture(&other, ROOM, budget.reserve(n).unwrap())
            .unwrap()
            .unwrap();
        let n = file.prepare_required_bytes(&foreign).unwrap();
        let refusal = file
            .prepare(foreign, budget.reserve(n).unwrap())
            .err()
            .expect("foreign same-byte cache");
        assert_eq!(refusal.code(), SenderStorageCode::InvalidScope);
        drop(refusal);
        drop(cache);
        drop(root);
    }

    #[test]
    fn private_actions_storage_consuming_final_fence_and_stamp_refuse_changed_identity() {
        for change in 0..3 {
            let root = fixture();
            let budget = allowance();
            let cache = PortableStorage::new("profile", "device").unwrap();
            let done = committed(
                prepared(
                    queue(state(root.path(), &cache, &budget), b"command", &budget),
                    &budget,
                )
                .publish(),
            );
            match change {
                0 => {
                    fs::rename(root.path().join(CHILD), root.path().join("old-child")).unwrap();
                    fs::create_dir(root.path().join(CHILD)).unwrap();
                    fs::set_permissions(root.path().join(CHILD), fs::Permissions::from_mode(0o700))
                        .unwrap();
                }
                1 => {
                    directory(root.path())
                        .publish(
                            OsStr::new(LOCK_NAME),
                            b"",
                            PublishMode::Replace,
                            Privacy::OwnerOnly,
                        )
                        .unwrap();
                }
                _ => {
                    let changed = codec::encode(&scope(), [9; 16], 2, 0, b"changed").unwrap();
                    directory(&root.path().join(CHILD))
                        .publish(
                            OsStr::new(FILE_NAME),
                            &changed,
                            PublishMode::Replace,
                            Privacy::OwnerOnly,
                        )
                        .unwrap();
                }
            }
            let failed = done
                .finish_current()
                .err()
                .expect("changed post-barrier identity");
            assert!(failed.io_error().is_some());
            assert!(cache.flush(&key()).is_err());
        }
        let budget = allowance();
        let (_root, mut state, _cache) = actual_state_fixture(b"exact bytes", &budget);
        state.stamp.payload_digest[0] ^= 1;
        assert!(matches!(
            state.borrow_current(),
            Err(SenderStorageCode::StaleCapture)
        ));
    }
    #[test]
    fn private_actions_storage_consuming_root_fd_and_queue_refusal_preserve_originals() {
        let budget = allowance();
        let (root, current, cache) = actual_state_fixture(b"initial", &budget);
        let refused = current
            .queue_owned(b"next".to_vec(), budget.reserve(3).unwrap())
            .err()
            .expect("actual capacity check");
        assert_eq!(refused.code(), SenderStorageCode::Capacity);
        let (current, bytes, credit) = refused.into_parts();
        assert_eq!(bytes, b"next");
        drop(bytes);
        drop(credit);
        let current = match current
            .queue_owned(b"initial".to_vec(), budget.reserve(7).unwrap())
            .unwrap_or_else(|e| panic!("same bytes {:?}", e.code()))
        {
            PrivateQueueOutcome::Unchanged(v) => v,
            _ => panic!("same bytes must not queue"),
        };
        let file = queue(current, b"retained root", &budget);
        let container = fixture_container();
        let retained = container.path().join("retained");
        fs::rename(root.path(), &retained).unwrap();
        fs::create_dir(root.path()).unwrap();
        let current = committed(prepared(file, &budget).publish())
            .finish_current()
            .unwrap_or_else(|e| panic!("original FD {:?}", e.code()));
        assert_eq!(
            current.borrow_current().unwrap().payload(),
            b"retained root"
        );
        assert!(!root.path().join(CHILD).exists());
        assert!(cache.flush(&key()).is_ok());
        assert_eq!(
            codec::decode(
                &scope(),
                &fs::read(retained.join(CHILD).join(FILE_NAME)).unwrap()
            )
            .unwrap()
            .payload,
            b"retained root"
        );
    }
}
