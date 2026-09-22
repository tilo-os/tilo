use super::*;
use sha2::Digest;

// Catches absence of a durable initial namespace and successful reopen.
#[test]
fn empty_store_is_durable_and_exclusive() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap().join("connections");
    let store = ConnectionStore::open_at(&root).expect("initialize real private store");
    assert!(root.join("store-header").is_file());
    assert!(root.join("CURRENT").is_file());
    assert!(matches!(
        ConnectionStore::open_at(&root),
        Err(StoreError::Busy)
    ));
    drop(store);
    assert!(ConnectionStore::open_at(&root).is_ok());
}

fn bindings() -> (EndpointBinding, EndpointBinding) {
    (
        EndpointBinding {
            identity: EndpointIdentity {
                volume: [1; 32],
                root: [2; 32],
            },
            locator: EndpointLocator::new(Path::new("/source")).unwrap(),
        },
        EndpointBinding {
            identity: EndpointIdentity {
                volume: [3; 32],
                root: [4; 32],
            },
            locator: EndpointLocator::new(Path::new("/destination")).unwrap(),
        },
    )
}
fn create(store: &mut ConnectionStore, id: u32) -> ConnectionRecord {
    let mut bytes = [0; 16];
    bytes[12..].copy_from_slice(&id.to_be_bytes());
    let (source, destination) = bindings();
    store
        .create_connection(
            ConnectionId(bytes),
            source,
            destination,
            ConnectionPolicy {
                deletion_retention_seconds: 2_592_000,
            },
        )
        .unwrap()
        .value
}
fn intent(record: &ConnectionRecord) -> OperationIntent {
    OperationIntent {
        connection: record.id,
        expected_revision: record.revision,
        expected_baseline: None,
        path: RelativePath::unix(b"file").unwrap(),
        source_before: Some(EntryState::File {
            sha256: [7; 32],
            bytes: 3,
            executable: false,
        }),
        destination_before: None,
        decision: Decision::Propagate { from: Side::Source },
    }
}
fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap().join("connections");
    (directory, root)
}
// Catches silently losing durable connection settings or inventing a baseline.
#[test]
fn connection_and_absent_baseline_survive_reopen() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 7);
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(reopened.load_connection(record.id).unwrap(), record);
    assert!(
        reopened
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .is_none()
    );
}
// Catches lost fences across checkpoint/reopen and reuse of old live tokens.
#[test]
fn fence_blocks_restart_and_stale_ack() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    let fence = store
        .request_disconnect(record.id, record.revision)
        .unwrap();
    store.checkpoint().unwrap();
    drop(store);
    let mut reopened = ConnectionStore::open_at(&path).unwrap();
    assert!(matches!(
        reopened.begin_intent(intent(&record)),
        Err(StoreError::Disconnected)
    ));
    assert!(matches!(
        reopened.finish_disconnect(&fence),
        Err(StoreError::StaleRevision)
    ));
    let current = reopened.load_connection(record.id).unwrap();
    let fresh = reopened
        .request_disconnect(record.id, current.revision)
        .unwrap();
    assert_eq!(
        reopened.finish_disconnect(&fresh).unwrap().value.lifecycle,
        Lifecycle::Disconnected
    );
    drop(reopened);
    assert_eq!(
        ConnectionStore::open_at(&path)
            .unwrap()
            .load_connection(record.id)
            .unwrap()
            .lifecycle,
        Lifecycle::Disconnected
    );
}
// Catches treating an unproved intent as safely aborted when disconnecting.
#[test]
fn pending_intent_prevents_disconnect_completion() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    store.begin_intent(intent(&record)).unwrap();
    let current = store.load_connection(record.id).unwrap();
    let fence = store
        .request_disconnect(record.id, current.revision)
        .unwrap();
    assert!(matches!(
        store.finish_disconnect(&fence),
        Err(StoreError::RecoveryRequired)
    ));
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        reopened.load_connection(record.id).unwrap().lifecycle,
        Lifecycle::Disconnecting
    );
}
// Catches whole-inventory collection and dropped keys during paged checkpoint merge.
#[test]
fn large_inventory_uses_bounded_pages() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    for id in 0..4097 {
        create(&mut store, id);
        assert!(store.overlay.len() <= 1024);
        assert!(store.overlay_bytes <= 8 * 1024 * 1024);
    }
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    let mut cursor = None;
    let mut count = 0;
    loop {
        let page = store.connection_page(cursor, 128).unwrap();
        assert!(page.entries.len() <= 128);
        for record in page.entries {
            assert_eq!(
                u32::from_be_bytes(record.id.0[12..].try_into().unwrap()),
                count
            );
            count += 1;
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(count, 4097);
}

// Catches metadata-only recovery, truncation, wrong offsets, and loss on reopen.
#[test]
fn recovery_contains_independently_verified_bytes() {
    for size in [0, 786_439] {
        let (_temp, path) = fixture();
        let mut store = ConnectionStore::open_at(&path).unwrap();
        let record = create(&mut store, 1);
        let intent = store.begin_intent(intent(&record)).unwrap();
        let reservation = store
            .reserve_recovery(
                &intent,
                Side::Source,
                RelativePath::unix(b"file").unwrap(),
                RecoveryReason::DisplacedVersion,
            )
            .unwrap();
        let data: Vec<u8> = (0..size)
            .map(|n| ((n * 71 + n / 251) % 256) as u8)
            .collect();
        let original = path.parent().unwrap().join("original");
        std::fs::write(&original, &data).unwrap();
        let preserved = store
            .preserve_file(
                reservation,
                &mut File::open(&original).unwrap(),
                &AtomicBool::new(false),
            )
            .unwrap();
        let mut bytes = Vec::new();
        preserved
            .value
            .file
            .try_clone()
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, data);
        drop(preserved);
        drop(store);
        let reopened = ConnectionStore::open_at(&path).unwrap();
        let entries = reopened
            .recovery_page(record.id, None, 128)
            .unwrap()
            .entries;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].state, RecoveryState::BytesVerified);
        let mut persisted = File::open(
            path.join("recovery")
                .join(format::hex(&record.id.0))
                .join("1-0"),
        )
        .unwrap();
        let mut bytes = Vec::new();
        persisted.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, data);
        assert_eq!(std::fs::read(original).unwrap(), data);
    }
}
// Catches issuing a durable byte proof after cancellation, and losing its intent.
#[test]
fn recovery_cancellation_keeps_pending() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    let intent = store.begin_intent(intent(&record)).unwrap();
    let reservation = store
        .reserve_recovery(
            &intent,
            Side::Source,
            RelativePath::unix(b"file").unwrap(),
            RecoveryReason::DisplacedVersion,
        )
        .unwrap();
    let original = path.parent().unwrap().join("original");
    std::fs::write(&original, b"keep me").unwrap();
    assert!(matches!(
        store.preserve_file(
            reservation,
            &mut File::open(&original).unwrap(),
            &AtomicBool::new(true)
        ),
        Err(StoreError::Cancelled)
    ));
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        reopened
            .pending_page(record.id, None, 128)
            .unwrap()
            .entries
            .len(),
        1
    );
    assert_eq!(
        reopened
            .recovery_page(record.id, None, 128)
            .unwrap()
            .entries[0]
            .state,
        RecoveryState::Reserved
    );
    assert_eq!(std::fs::read(original).unwrap(), b"keep me");
}

fn log_path(path: &Path) -> std::path::PathBuf {
    path.join("log-0")
}
fn event_offsets(path: &Path) -> Vec<u64> {
    let mut file = File::open(log_path(path)).unwrap();
    file.seek(SeekFrom::Start(8)).unwrap();
    let _: format::LogHeader = framing::read_record(&mut file, 0).unwrap().unwrap();
    let mut offsets = vec![file.stream_position().unwrap()];
    let mut sequence = 1;
    while framing::read_record::<format::Event>(&mut file, sequence)
        .unwrap()
        .is_some()
    {
        offsets.push(file.stream_position().unwrap());
        sequence += 1;
    }
    offsets
}
// Catches truncating a full corrupt frame as though it were a torn tail.
#[test]
fn complete_bad_frame_is_never_tail_repair() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    for id in 0..3 {
        create(&mut store, id);
    }
    drop(store);
    let original = std::fs::read(log_path(&path)).unwrap();
    let offsets = event_offsets(&path);
    for offset in offsets.iter().take(3) {
        for relative in [4, 12] {
            let mut bytes = original.clone();
            bytes[*offset as usize + relative] ^= 1;
            std::fs::write(log_path(&path), &bytes).unwrap();
            assert!(matches!(
                ConnectionStore::open_at(&path),
                Err(StoreError::Corrupt)
            ));
            assert_eq!(std::fs::read(log_path(&path)).unwrap(), bytes);
        }
    }
}
// Catches discarding earlier acknowledgements or repairing before root validation.
#[test]
fn torn_tail_repairs_only_after_identity_and_prefix_validation() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    create(&mut store, 2);
    drop(store);
    let original = std::fs::read(log_path(&path)).unwrap();
    let offsets = event_offsets(&path);
    let start = offsets[1] as usize;
    for cut in start..original.len() {
        std::fs::write(log_path(&path), &original[..cut]).unwrap();
        let reopened = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(reopened.load_connection(record.id).unwrap(), record);
        assert_eq!(reopened.connections, 1);
        drop(reopened);
        assert_eq!(
            std::fs::metadata(log_path(&path)).unwrap().len(),
            start as u64
        );
    }
    std::fs::write(log_path(&path), &original[..original.len() - 1]).unwrap();
    let moved = path.with_extension("old");
    std::fs::rename(&path, &moved).unwrap();
    std::fs::create_dir(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    for name in ["ownership.lock", "store-header", "CURRENT", "log-0"] {
        std::fs::copy(moved.join(name), path.join(name)).unwrap();
    }
    std::fs::create_dir(path.join("pages")).unwrap();
    std::fs::create_dir(path.join("recovery")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["pages", "recovery"] {
            std::fs::set_permissions(path.join(name), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
    }
    let before = std::fs::read(log_path(&path)).unwrap();
    assert!(ConnectionStore::open_at(&path).is_err());
    assert_eq!(std::fs::read(log_path(&path)).unwrap(), before);
}
// Catches oversized allocation admission and accepting impossible index depth.
#[test]
fn oversized_frame_and_deep_index_refuse_before_allocation() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    create(&mut store, 1);
    drop(store);
    let offsets = event_offsets(&path);
    let mut bytes = std::fs::read(log_path(&path)).unwrap();
    bytes[offsets[0] as usize..offsets[0] as usize + 4].copy_from_slice(&131_073_u32.to_le_bytes());
    std::fs::write(log_path(&path), &bytes).unwrap();
    assert!(matches!(
        ConnectionStore::open_at(&path),
        Err(StoreError::Corrupt)
    ));
    assert_eq!(std::fs::read(log_path(&path)).unwrap(), bytes);
    let child = format::Child {
        hash: [1; 32],
        first: vec![0; 25],
        last: vec![0; 25],
        count: 1,
        level: 8,
    };
    assert!(
        format::PageNode {
            schema: 1,
            node: format::Node::Index(vec![child])
        }
        .reference([1; 32])
        .is_err()
    );
    assert!(
        format::NativePath::Unix(vec![b'a'; 32769])
            .to_locator()
            .is_err()
    );
    assert!(
        format::NativePath::Unix(vec![b'a'; 4097])
            .to_relative()
            .is_err()
    );
}
// Catches locking a replaced pathname or poisoning only the current call.
#[test]
fn replaced_lock_poisons_current_owner() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    std::fs::rename(path.join("ownership.lock"), path.join("old-lock")).unwrap();
    std::fs::write(path.join("ownership.lock"), b"").unwrap();
    assert!(
        store
            .request_disconnect(record.id, record.revision)
            .is_err()
    );
    std::fs::remove_file(path.join("ownership.lock")).unwrap();
    std::fs::rename(path.join("old-lock"), path.join("ownership.lock")).unwrap();
    assert!(matches!(
        store.request_disconnect(record.id, record.revision),
        Err(StoreError::RecoveryRequired)
    ));
}
// The separate process must contend on the OS lock, not an in-process mutex.
#[test]
fn two_processes_cannot_own_store() {
    if let Ok(path) = std::env::var("QOS_CONNECTED_LOCK_TEST_PATH") {
        let result = ConnectionStore::open_at(Path::new(&path));
        if std::env::var_os("QOS_CONNECTED_EXPECT_BUSY").is_some() {
            assert!(matches!(result, Err(StoreError::Busy)));
        } else {
            assert!(result.is_ok());
        }
        return;
    }
    let (_temp, path) = fixture();
    let store = ConnectionStore::open_at(&path).unwrap();
    let child = |busy: bool| {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "connection::store::tests::two_processes_cannot_own_store",
                "--nocapture",
            ])
            .env("QOS_CONNECTED_LOCK_TEST_PATH", &path);
        if busy {
            command.env("QOS_CONNECTED_EXPECT_BUSY", "1");
        }
        assert!(command.output().unwrap().status.success());
    };
    child(true);
    drop(store);
    child(false);
}
// Catches adopting an empty directory or missing store identity as a fresh store.
#[test]
fn missing_header_is_not_new_store() {
    let (_temp, path) = fixture();
    let store = ConnectionStore::open_at(&path).unwrap();
    drop(store);
    std::fs::remove_file(path.join("store-header")).unwrap();
    let before = std::fs::read(path.join("CURRENT")).unwrap();
    assert!(ConnectionStore::open_at(&path).is_err());
    assert_eq!(std::fs::read(path.join("CURRENT")).unwrap(), before);
}

// Private reducer fixture only: these editable wire facts are NOT executor evidence.
#[test]
fn baseline_commit_has_one_durable_visibility_point() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    let mut operation = intent(&record);
    operation.destination_before = operation.source_before.clone();
    operation.decision = Decision::VerifyEqual;
    let ack = store.begin_intent(operation).unwrap();
    let current = store.load_connection(record.id).unwrap();
    store
        .append(Change::Verified {
            connection: record.id.0,
            serial: ack.value.serial,
            expected: current.revision,
            source: record.source.identity.into(),
            destination: record.destination.identity.into(),
        })
        .unwrap();
    let current = store.load_connection(record.id).unwrap();
    let revision = current.revision + 1;
    let row = Row::Baseline(format::BaselineEntry {
        connection: record.id.0,
        revision,
        ordinal: 0,
        path: format::NativePath::Unix(b"file".to_vec()),
        state: format::State::File {
            sha256: [7; 32],
            bytes: 3,
            executable: false,
        },
    });
    let root = storage::build_pages(&store.storage, std::iter::once(Ok(row))).unwrap();
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
    store
        .append(Change::BaselineCommit { directory_chain: None,
            directory_ancestors: None,
            existing_ancestors: None,
            connection: record.id.0,
            expected: current.revision,
            root,
            count: 1,
            source: record.source.identity.into(),
            destination: record.destination.identity.into(),
            initial_review: true,
        })
        .unwrap();
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    let baseline = reopened.load_baseline(record.id).unwrap();
    assert!(
        reopened
            .pending_page(record.id, None, 128)
            .unwrap()
            .entries
            .is_empty()
    );
    assert_eq!(baseline.entries().unwrap().len(), 1);
    assert_eq!(
        baseline
            .entries()
            .unwrap()
            .get(&RelativePath::unix(b"file").unwrap()),
        Some(&EntryState::File {
            sha256: [7; 32],
            bytes: 3,
            executable: false
        })
    );
}
// Catches a wall-clock policy, early threshold, and failed pin/fence checks.
#[test]
fn retention_never_credits_wall_jumps_or_downtime() {
    let id = RecoveryId {
        operation: OperationId {
            connection: ConnectionId([1; 16]),
            serial: 1,
        },
        slot: 0,
    };
    let mut entry = RecoveryEntry {
        id,
        original_side: Side::Source,
        original_path: RelativePath::unix(b"file").unwrap(),
        state: RecoveryState::Attached,
        reason: RecoveryReason::NormalDeletion,
        content: Some(EntryState::File {
            sha256: [0; 32],
            bytes: 0,
            executable: false,
        }),
        retained_seconds: 2_591_999,
    };
    assert!(!recovery::eligible(&entry, Lifecycle::Active, false));
    let now = std::time::Instant::now();
    let before = now.checked_sub(std::time::Duration::from_secs(1)).unwrap();
    recovery::credit(&mut entry, before, now, Lifecycle::Active, false);
    assert!(recovery::eligible(&entry, Lifecycle::Active, false));
    for lifecycle in [Lifecycle::Disconnecting, Lifecycle::Disconnected] {
        assert!(!recovery::eligible(&entry, lifecycle, false));
    }
    assert!(!recovery::eligible(&entry, Lifecycle::Active, true));
    let saved = entry.retained_seconds;
    recovery::credit(&mut entry, now, before, Lifecycle::Active, false);
    assert_eq!(entry.retained_seconds, saved);
    // A new process sample starts at now; no wall time/downtime enters this interface.
    recovery::credit(&mut entry, now, now, Lifecycle::Active, false);
    assert_eq!(entry.retained_seconds, saved);
    entry.reason = RecoveryReason::Conflict(ConflictId {
        connection: id.operation.connection,
        serial: 1,
    });
    assert!(!recovery::eligible(&entry, Lifecycle::Active, true));
}
// Catches losing either side/tombstone of an unresolved conflict on restart.
#[test]
fn conflict_records_both_observed_versions() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    let mut operation = intent(&record);
    operation.decision = Decision::Conflict(Conflict::EditDelete);
    let ack = store.begin_intent(operation).unwrap();
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    let Some(Row::Conflict(conflict)) = reopened
        .row(&format::key(3, record.id.0, ack.value.serial))
        .unwrap()
    else {
        panic!("durable conflict missing")
    };
    assert!(conflict.source.is_some());
    assert!(conflict.destination.is_none());
    assert!(!conflict.resolved);
    assert!(conflict.preserved.is_empty());
}

thread_local! {pub(super) static FAULT:std::cell::Cell<Option<(usize,bool)>>=const {std::cell::Cell::new(None)};}
pub(super) fn fault() -> Result<(), StoreError> {
    FAULT.with(|fault| match fault.get() {
        Some((0, space)) => {
            fault.set(None);
            if space {
                Err(StoreError::RecoverySpace)
            } else {
                Err(StoreError::Persistence(std::io::Error::other(
                    "injected durability boundary",
                )))
            }
        }
        Some((remaining, space)) => {
            fault.set(Some((remaining - 1, space)));
            Ok(())
        }
        None => Ok(()),
    })
}
// Catches a fence disappearing on either side of the actual CURRENT switch.
#[test]
fn checkpoint_switch_retains_all_fences() {
    let mut failures = 0;
    for point in 0..12 {
        let (_temp, path) = fixture();
        let mut store = ConnectionStore::open_at(&path).unwrap();
        let record = create(&mut store, 1);
        store
            .request_disconnect(record.id, record.revision)
            .unwrap();
        FAULT.with(|fault| fault.set(Some((point, false))));
        let result = store.checkpoint();
        FAULT.with(|fault| fault.set(None));
        if result.is_err() {
            failures += 1;
            assert!(matches!(
                store.begin_intent(intent(&record)),
                Err(StoreError::RecoveryRequired)
            ));
        }
        drop(store);
        let reopened = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(
            reopened.load_connection(record.id).unwrap().lifecycle,
            Lifecycle::Disconnecting
        );
    }
    assert!(
        failures >= 6,
        "exercise each real file/directory/CURRENT durability boundary"
    );
}
// Catches acknowledging bytes after a failed durability boundary/space exhaustion.
#[test]
fn recovery_space_exhaustion_keeps_original_and_pending() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    let intent = store.begin_intent(intent(&record)).unwrap();
    let reservation = store
        .reserve_recovery(
            &intent,
            Side::Source,
            RelativePath::unix(b"file").unwrap(),
            RecoveryReason::DisplacedVersion,
        )
        .unwrap();
    let original = path.parent().unwrap().join("original");
    std::fs::write(&original, b"must survive").unwrap();
    FAULT.with(|fault| fault.set(Some((0, true))));
    let result = store.preserve_file(
        reservation,
        &mut File::open(&original).unwrap(),
        &AtomicBool::new(false),
    );
    FAULT.with(|fault| fault.set(None));
    assert!(matches!(result, Err(StoreError::RecoverySpace)));
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        reopened
            .pending_page(record.id, None, 128)
            .unwrap()
            .entries
            .len(),
        1
    );
    assert_eq!(
        reopened
            .recovery_page(record.id, None, 128)
            .unwrap()
            .entries[0]
            .state,
        RecoveryState::Reserved
    );
    assert_eq!(std::fs::read(original).unwrap(), b"must survive");
}

// Catches overwriting an externally replaced committed pointer during a live session.
#[test]
fn replaced_current_is_not_overwritten() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    let original = std::fs::read(path.join("CURRENT")).unwrap();
    std::fs::rename(path.join("CURRENT"), path.join("previous-current")).unwrap();
    std::fs::copy(path.join("previous-current"), path.join("CURRENT")).unwrap();
    assert!(matches!(
        store.request_disconnect(record.id, record.revision),
        Err(StoreError::IdentityChanged)
    ));
    assert_eq!(std::fs::read(path.join("CURRENT")).unwrap(), original);
}

// Catches unbounded old log/page accumulation while protecting current/recovery objects.
#[test]
fn garbage_collection_is_bounded_and_reachability_checked() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    create(&mut store, 1);
    store.checkpoint().unwrap();
    let old = store.current.root.unwrap();
    create(&mut store, 2);
    store.checkpoint().unwrap();
    let current = store.current.root.unwrap();
    std::fs::write(path.join("recovery").join("keep"), b"recovery bytes").unwrap();
    std::fs::write(path.join("pages").join("unknown"), b"foreign").unwrap();
    let mut removed = 0;
    for _ in 0..5 {
        let count = store.gc_step().unwrap();
        assert!(count <= 32);
        removed += count;
    }
    assert!(removed <= 160);
    assert!(!path.join("pages").join(format::hex(&old)).exists());
    assert!(!path.join("log-0").exists());
    assert!(path.join("pages").join(format::hex(&current)).is_file());
    assert_eq!(
        std::fs::read(path.join("recovery").join("keep")).unwrap(),
        b"recovery bytes"
    );
    assert_eq!(
        std::fs::read(path.join("pages").join("unknown")).unwrap(),
        b"foreign"
    );
    drop(store);
    assert!(ConnectionStore::open_at(&path).is_ok());
}

thread_local! {static CAPTURE_SUBSTITUTE:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};}
pub(super) fn before_capture(
    parent: &std::sync::Arc<storage::Directory>,
    name: &str,
) -> Result<(), StoreError> {
    if CAPTURE_SUBSTITUTE.with(|flag| flag.replace(false)) {
        cap_primitives::fs::rename(
            &parent.file,
            Path::new(name),
            &parent.file,
            Path::new("retained-original"),
        )?;
        storage::open_file(parent, name, true)?.write_all(b"unexpected user replacement")?;
    }
    Ok(())
}
// Catches unlinking a raced-in source object after pathname-only validation.
#[test]
fn garbage_capture_preserves_unexpected_replacement() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    create(&mut store, 1);
    store.checkpoint().unwrap();
    create(&mut store, 2);
    CAPTURE_SUBSTITUTE.with(|flag| flag.set(true));
    let result = store.checkpoint();
    CAPTURE_SUBSTITUTE.with(|flag| flag.set(false));
    assert!(matches!(result, Err(StoreError::RecoveryRequired)));
    assert_eq!(
        std::fs::read(path.join("quarantine").join("captured")).unwrap(),
        b"unexpected user replacement"
    );
    assert!(
        path.join("retained-original").is_file()
            || path.join("pages").join("retained-original").is_file()
    );
    drop(store);
    assert!(matches!(
        ConnectionStore::open_at(&path),
        Err(StoreError::RecoveryRequired)
    ));
    assert_eq!(
        std::fs::read(path.join("quarantine").join("captured")).unwrap(),
        b"unexpected user replacement"
    );
}

// Catches leaving reclamation as an unused private helper.
#[test]
fn checkpoints_make_bounded_reclamation_progress() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    for id in 0..40 {
        create(&mut store, id);
        store.checkpoint().unwrap();
    }
    let logs = std::fs::read_dir(&path)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("log-"))
        .count();
    assert!(logs <= 33, "retired logs must be collected incrementally");
}

#[test]
fn garbage_capture_never_clobbers_quarantine() {
    let (_temp, path) = fixture();
    let store = ConnectionStore::open_at(&path).unwrap();
    store
        .storage
        .create_bytes(&store.storage.pages, "candidate", b"source")
        .unwrap();
    store
        .storage
        .create_bytes(&store.storage.quarantine, "captured", b"destination")
        .unwrap();
    assert!(
        storage::capture_no_replace(&store.storage.pages, "candidate", &store.storage.quarantine)
            .is_err()
    );
    assert_eq!(
        std::fs::read(path.join("pages").join("candidate")).unwrap(),
        b"source"
    );
    assert_eq!(
        std::fs::read(path.join("quarantine").join("captured")).unwrap(),
        b"destination"
    );
}
fn commit_test_baseline(store: &mut ConnectionStore, id: ConnectionId) -> [u8; 32] {
    let record = store.load_connection(id).unwrap();
    let mut operation = intent(&record);
    operation.expected_baseline = record.baseline_revision;
    operation.destination_before = operation.source_before.clone();
    operation.decision = Decision::VerifyEqual;
    let ack = store.begin_intent(operation).unwrap();
    let current = store.load_connection(id).unwrap();
    store
        .append(Change::Verified {
            connection: id.0,
            serial: ack.value.serial,
            expected: current.revision,
            source: record.source.identity.into(),
            destination: record.destination.identity.into(),
        })
        .unwrap();
    let current = store.load_connection(id).unwrap();
    let root = storage::build_pages(
        &store.storage,
        std::iter::once(Ok(Row::Baseline(format::BaselineEntry {
            connection: id.0,
            revision: current.revision + 1,
            ordinal: 0,
            path: format::NativePath::Unix(b"file".to_vec()),
            state: format::State::File {
                sha256: [7; 32],
                bytes: 3,
                executable: false,
            },
        }))),
    )
    .unwrap();
    store
        .append(Change::BaselineCommit { directory_chain: None,
            directory_ancestors: None,
            existing_ancestors: None,
            connection: id.0,
            expected: current.revision,
            root,
            count: 1,
            source: record.source.identity.into(),
            destination: record.destination.identity.into(),
            initial_review: true,
        })
        .unwrap();
    root.unwrap()
}
#[test]
fn garbage_collection_keeps_active_log_baseline_dependencies() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    let first = commit_test_baseline(&mut store, record.id);
    let second = commit_test_baseline(&mut store, record.id);
    store.gc_step().unwrap();
    for hash in [first, second] {
        assert!(path.join("pages").join(format::hex(&hash)).is_file());
    }
    drop(store);
    let mut reopened = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        reopened
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .unwrap()
            .len(),
        1
    );
    reopened.checkpoint().unwrap();
    for _ in 0..3 {
        reopened.gc_step().unwrap();
    }
    assert!(!path.join("pages").join(format::hex(&first)).exists());
    assert!(path.join("pages").join(format::hex(&second)).is_file());
}
// Catches accepting a cursor against a different immutable checkpoint.
#[test]
fn checkpoint_invalidates_inventory_cursor() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    create(&mut store, 1);
    create(&mut store, 2);
    let cursor = store.connection_page(None, 1).unwrap().next;
    store.checkpoint().unwrap();
    assert!(matches!(
        store.connection_page(cursor, 1),
        Err(StoreError::StaleRevision)
    ));
}

thread_local! {static SOURCE_CHANGE:std::cell::RefCell<Option<std::path::PathBuf>>=const {std::cell::RefCell::new(None)};}
pub(super) fn after_copy_chunk() -> Result<(), StoreError> {
    if let Some(path) = SOURCE_CHANGE.with(|value| value.borrow_mut().take()) {
        std::fs::write(path, b"editor changed this source")?;
    }
    Ok(())
}
#[test]
fn source_instability_never_acknowledges_recovery() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    let ack = store.begin_intent(intent(&record)).unwrap();
    let reservation = store
        .reserve_recovery(
            &ack,
            Side::Source,
            RelativePath::unix(b"file").unwrap(),
            RecoveryReason::DisplacedVersion,
        )
        .unwrap();
    let source = path.parent().unwrap().join("source");
    std::fs::write(&source, vec![9; 786_439]).unwrap();
    SOURCE_CHANGE.with(|value| *value.borrow_mut() = Some(source.clone()));
    assert!(matches!(
        store.preserve_file(
            reservation,
            &mut File::open(&source).unwrap(),
            &AtomicBool::new(false)
        ),
        Err(StoreError::RecoveryRequired)
    ));
    assert_eq!(
        store.recovery_page(record.id, None, 128).unwrap().entries[0].state,
        RecoveryState::Reserved
    );
    assert_eq!(
        std::fs::read(source).unwrap(),
        b"editor changed this source"
    );
}

thread_local! {static NO_IDENTITY:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};}
pub(super) fn identity_unavailable() -> bool {
    NO_IDENTITY.with(std::cell::Cell::get)
}
#[test]
fn boot_bound_identity_is_not_adopted() {
    let (_temp, path) = fixture();
    NO_IDENTITY.with(|flag| flag.set(true));
    let result = ConnectionStore::open_at(&path);
    NO_IDENTITY.with(|flag| flag.set(false));
    assert!(matches!(result, Err(StoreError::UnsupportedDurability)));
    assert!(!path.exists());
    let mut store = ConnectionStore::open_at(&path).unwrap();
    create(&mut store, 1);
    drop(store);
    let before = std::fs::read(log_path(&path)).unwrap();
    NO_IDENTITY.with(|flag| flag.set(true));
    let result = ConnectionStore::open_at(&path);
    NO_IDENTITY.with(|flag| flag.set(false));
    assert!(matches!(result, Err(StoreError::UnsupportedDurability)));
    assert_eq!(std::fs::read(log_path(&path)).unwrap(), before);
}
#[cfg(windows)]
#[test]
fn windows_store_refuses_before_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("refused");
    assert!(matches!(
        ConnectionStore::open_at(&path),
        Err(StoreError::UnsupportedDurability)
    ));
    assert!(!path.exists());
}
#[test]
fn stale_revision_and_endpoint_rebind_refuse() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    let before = std::fs::read(log_path(&path)).unwrap();
    let mut stale = intent(&record);
    stale.expected_revision += 1;
    assert!(matches!(
        store.begin_intent(stale),
        Err(StoreError::StaleRevision)
    ));
    let mut operation =
        format::Operation::new(intent(&record), 1, &store.connection(record.id).unwrap()).unwrap();
    operation.source_binding.root = [5; 32];
    assert!(matches!(
        store.append(Change::Intent { operation }),
        Err(StoreError::IdentityChanged)
    ));
    assert_eq!(std::fs::read(log_path(&path)).unwrap(), before);
}
#[test]
fn intent_or_preserved_record_is_not_publication_authority() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    let ack = store.begin_intent(intent(&record)).unwrap();
    let current = store.load_connection(record.id).unwrap();
    let before = std::fs::read(log_path(&path)).unwrap();
    assert!(matches!(
        store.append(Change::Published {
            connection: record.id.0,
            serial: ack.value.serial,
            expected: current.revision,
            source: record.source.identity.into(),
            destination: record.destination.identity.into()
        }),
        Err(StoreError::RecoveryRequired)
    ));
    assert!(matches!(
        store.append(Change::Verified {
            connection: record.id.0,
            serial: ack.value.serial,
            expected: current.revision,
            source: record.source.identity.into(),
            destination: record.destination.identity.into()
        }),
        Err(StoreError::RecoveryRequired)
    ));
    assert!(
        store
            .append(Change::BaselineCommit { directory_chain: None,
                directory_ancestors: None,
                existing_ancestors: None,
                connection: record.id.0,
                expected: current.revision,
                root: None,
                count: 0,
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
                initial_review: true
            })
            .is_err()
    );
    assert_eq!(std::fs::read(log_path(&path)).unwrap(), before);
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
}
#[test]
fn source_model_limit_is_not_store_paging_success() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    assert!(matches!(
        store.read_baseline(&store.connection(record.id).unwrap(), None, 4097, 1),
        Err(StoreError::LimitExceeded)
    ));
    let entries = (0..4097).map(|index| {
        (
            RelativePath::unix(format!("file-{index}").as_bytes()).unwrap(),
            EntryState::Directory,
        )
    });
    assert!(
        VerifiedBaseline::from_verified(
            record.id,
            record.source.identity,
            record.destination.identity,
            entries
        )
        .is_err()
    );
}
#[cfg(unix)]
#[test]
fn recovery_links_and_changed_content_are_never_reused() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    let ack = store.begin_intent(intent(&record)).unwrap();
    let reservation = store
        .reserve_recovery(
            &ack,
            Side::Source,
            RelativePath::unix(b"file").unwrap(),
            RecoveryReason::DisplacedVersion,
        )
        .unwrap();
    let source = path.parent().unwrap().join("source");
    std::fs::write(&source, b"actual bytes").unwrap();
    let preserved = store
        .preserve_file(
            reservation,
            &mut File::open(&source).unwrap(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let id = preserved.value.id;
    drop(preserved);
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    let mut key = format::key(2, record.id.0, id.operation.serial);
    key.extend(id.slot.to_be_bytes());
    let Some(Row::Recovery(entry)) = reopened.row(&key).unwrap() else {
        panic!("missing recovery")
    };
    assert!(reopened.verify_recovery(&entry).is_ok());
    let object = path
        .join("recovery")
        .join(format::hex(&record.id.0))
        .join("1-0");
    let saved = object.with_extension("saved");
    std::fs::rename(&object, &saved).unwrap();
    std::os::unix::fs::symlink(&source, &object).unwrap();
    assert!(reopened.verify_recovery(&entry).is_err());
    std::fs::remove_file(&object).unwrap();
    std::fs::hard_link(&saved, &object).unwrap();
    assert!(reopened.verify_recovery(&entry).is_err());
    std::fs::remove_file(&object).unwrap();
    std::fs::rename(&saved, &object).unwrap();
    std::fs::write(&object, b"altered data").unwrap();
    assert!(reopened.verify_recovery(&entry).is_err());
    assert_eq!(std::fs::read(source).unwrap(), b"actual bytes");
}
#[test]
fn baseline_fault_boundaries_never_expose_mixed_pages() {
    let mut failures = 0;
    for point in 0..5 {
        let (_temp, path) = fixture();
        let mut store = ConnectionStore::open_at(&path).unwrap();
        let record = create(&mut store, 1);
        commit_test_baseline(&mut store, record.id);
        let current = store.load_connection(record.id).unwrap();
        let mut operation = intent(&current);
        operation.expected_baseline = current.baseline_revision;
        operation.destination_before = operation.source_before.clone();
        operation.decision = Decision::VerifyEqual;
        let ack = store.begin_intent(operation).unwrap();
        let current = store.load_connection(record.id).unwrap();
        store
            .append(Change::Verified {
                connection: record.id.0,
                serial: ack.value.serial,
                expected: current.revision,
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
            })
            .unwrap();
        let current = store.load_connection(record.id).unwrap();
        let rows = [b"file".as_slice(), b"file2".as_slice()]
            .into_iter()
            .enumerate()
            .map(|(ordinal, path)| {
                Ok(Row::Baseline(format::BaselineEntry {
                    connection: record.id.0,
                    revision: current.revision + 1,
                    ordinal: ordinal as u32,
                    path: format::NativePath::Unix(path.to_vec()),
                    state: format::State::File {
                        sha256: [8; 32],
                        bytes: 8,
                        executable: false,
                    },
                }))
            });
        FAULT.with(|fault| fault.set(Some((point, false))));
        let result = (|| {
            let root = storage::build_pages(&store.storage, rows)?;
            store.append(Change::BaselineCommit { directory_chain: None,
                directory_ancestors: None,
                existing_ancestors: None,
                connection: record.id.0,
                expected: current.revision,
                root,
                count: 2,
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
                initial_review: false,
            })
        })();
        FAULT.with(|fault| fault.set(None));
        if result.is_err() {
            failures += 1;
        }
        drop(store);
        let reopened = ConnectionStore::open_at(&path).unwrap();
        let baseline = reopened.load_baseline(record.id).unwrap();
        let entries = baseline.entries().unwrap();
        assert!([1, 2].contains(&entries.len()));
        if entries.len() == 2 {
            assert_eq!(
                entries.get(&RelativePath::unix(b"file2").unwrap()),
                Some(&EntryState::File {
                    sha256: [8; 32],
                    bytes: 8,
                    executable: false
                })
            );
        }
    }
    assert!(failures >= 3);
}
#[test]
fn killed_owner_leaves_durable_pending_intent() {
    if let Ok(path) = std::env::var("QOS_CONNECTED_CRASH_PATH") {
        let mut store = ConnectionStore::open_at(Path::new(&path)).unwrap();
        let record = create(&mut store, 9);
        store.begin_intent(intent(&record)).unwrap();
        println!("CONNECTED_DURABLE_READY");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }
    let (_temp, path) = fixture();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "connection::store::tests::killed_owner_leaves_durable_pending_intent",
            "--nocapture",
        ])
        .env("QOS_CONNECTED_CRASH_PATH", &path)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::BufRead;
    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut ready = false;
    for _ in 0..16 {
        let mut line = String::new();
        if output.read_line(&mut line).unwrap() == 0 {
            break;
        }
        if line.contains("CONNECTED_DURABLE_READY") {
            ready = true;
            break;
        }
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(ready);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    let record = reopened
        .connection_page(None, 128)
        .unwrap()
        .entries
        .remove(0);
    assert_eq!(
        reopened
            .pending_page(record.id, None, 128)
            .unwrap()
            .entries
            .len(),
        1
    );
    assert!(
        reopened
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .is_none()
    );
}

// Catches GC deleting a durable candidate while BaselineCommit makes log room.
#[test]
fn baseline_candidate_survives_automatic_checkpoint_before_commit() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    let mut operation = intent(&record);
    operation.destination_before = operation.source_before.clone();
    operation.decision = Decision::VerifyEqual;
    let ack = store.begin_intent(operation).unwrap();
    let current = store.load_connection(record.id).unwrap();
    store
        .append(Change::Verified {
            connection: record.id.0,
            serial: ack.value.serial,
            expected: current.revision,
            source: record.source.identity.into(),
            destination: record.destination.identity.into(),
        })
        .unwrap();
    for id in 2..=1023 {
        create(&mut store, id);
    }
    let current = store.load_connection(record.id).unwrap();
    let root = storage::build_pages(
        &store.storage,
        std::iter::once(Ok(Row::Baseline(format::BaselineEntry {
            connection: record.id.0,
            revision: current.revision + 1,
            ordinal: 0,
            path: format::NativePath::Unix(b"file".to_vec()),
            state: format::State::File {
                sha256: [7; 32],
                bytes: 3,
                executable: false,
            },
        }))),
    )
    .unwrap();
    // Test-only pressure seam: exact replacement accounting no longer
    // checkpoints just because the overlay already has 1,024 keys. The long-
    // locator regression covers real byte pressure; here only the counter is
    // raised to trigger the candidate's real implicit checkpoint/GC path.
    store.overlay_bytes = 8 * 1024 * 1024;
    let generation = store.current.generation;
    store
        .append(Change::BaselineCommit { directory_chain: None,
            directory_ancestors: None,
            existing_ancestors: None,
            connection: record.id.0,
            expected: current.revision,
            root,
            count: 1,
            source: record.source.identity.into(),
            destination: record.destination.identity.into(),
            initial_review: true,
        })
        .unwrap();
    assert!(store.current.generation > generation);
    assert!(store.load_baseline(record.id).is_ok());
    drop(store);
    assert!(
        ConnectionStore::open_at(&path)
            .unwrap()
            .load_baseline(record.id)
            .is_ok()
    );
}

#[test]
fn changed_live_log_is_not_overwritten_or_checkpointed() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    std::fs::OpenOptions::new()
        .append(true)
        .open(log_path(&path))
        .unwrap()
        .write_all(b"unexpected tail")
        .unwrap();
    let bytes = std::fs::read(log_path(&path)).unwrap();
    assert!(matches!(
        store.request_disconnect(record.id, record.revision),
        Err(StoreError::IdentityChanged)
    ));
    assert_eq!(std::fs::read(log_path(&path)).unwrap(), bytes);
    assert!(matches!(
        store.checkpoint(),
        Err(StoreError::RecoveryRequired)
    ));
}

// A failed prior CURRENT publication may leave a newer log: never adopt or replace it.
#[test]
fn orphan_next_generation_is_preserved_for_recovery() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    let fence = store
        .request_disconnect(record.id, record.revision)
        .unwrap();
    let orphan = path.join("log-1");
    std::fs::write(&orphan, b"uncertain prior generation").unwrap();
    assert!(matches!(
        store.checkpoint(),
        Err(StoreError::RecoveryRequired)
    ));
    assert_eq!(
        std::fs::read(&orphan).unwrap(),
        b"uncertain prior generation"
    );
    drop(fence);
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        reopened.load_connection(record.id).unwrap().lifecycle,
        Lifecycle::Disconnecting
    );
}

// A tiny fence reconstructs a large checkpointed row; every ack must replay.
#[test]
fn admission_large_checkpointed_fences_remain_replayable() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let locator = "/".to_owned() + &"a".repeat(30_000);
    let mut records = Vec::new();
    for id in 1_u32..=93 {
        let mut bytes = [0; 16];
        bytes[12..].copy_from_slice(&id.to_be_bytes());
        let (mut source, destination) = bindings();
        source.locator = EndpointLocator::new(Path::new(&locator)).unwrap();
        records.push(
            store
                .create_connection(
                    ConnectionId(bytes),
                    source,
                    destination,
                    ConnectionPolicy {
                        deletion_retention_seconds: 2_592_000,
                    },
                )
                .unwrap()
                .value,
        );
    }
    store.checkpoint().unwrap();
    let generation = store.current.generation;
    for record in &records {
        store
            .request_disconnect(record.id, record.revision)
            .expect("fence must checkpoint its actual row before reaching overlay capacity");
        assert!(store.overlay_bytes <= 8 * 1024 * 1024);
    }
    assert!(store.current.generation > generation);
    drop(store);
    let reopened = ConnectionStore::open_at(&path).expect("all acknowledged fences replay");
    for record in records {
        assert_eq!(
            reopened.load_connection(record.id).unwrap().lifecycle,
            Lifecycle::Disconnecting
        );
    }
}

// Counter seam avoids a million-row fixture; real append must refuse before writing.
#[test]
fn admission_control_capacity_rejects_without_changing_log() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    let original = std::fs::read(log_path(&path)).unwrap();
    store.live_keys = store.connections + 1_048_576;
    assert!(matches!(
        store.begin_intent(intent(&record)),
        Err(StoreError::LimitExceeded)
    ));
    assert_eq!(
        std::fs::read(log_path(&path)).unwrap(),
        original,
        "hard capacity rejection must not append an unreplayable intent"
    );
    assert!(!store.poisoned);
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    assert!(
        reopened
            .pending_page(record.id, None, 128)
            .unwrap()
            .entries
            .is_empty()
    );
}

// Replacing an overlay key must not consume another key or force a checkpoint.
#[test]
fn admission_full_overlay_replaces_existing_key_without_checkpoint() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    for id in 2..=1024 {
        create(&mut store, id);
    }
    let generation = store.current.generation;
    let bytes = store.overlay_bytes;
    let fence = store
        .request_disconnect(record.id, record.revision)
        .unwrap();
    store.finish_disconnect(&fence).unwrap();
    assert_eq!(
        store.current.generation, generation,
        "replacement-aware key accounting should keep this bounded overlay"
    );
    assert_eq!(store.overlay.len(), 1024);
    // Revision, fence, and lifecycle remain one-digit fields; replacing this
    // row twice must subtract its old encoded bytes each time.
    assert_eq!(store.overlay_bytes, bytes);
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        reopened.load_connection(record.id).unwrap().lifecycle,
        Lifecycle::Disconnected
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CheckpointBoundary {
    Prepared,
    Published,
}
type CheckpointHook = (
    CheckpointBoundary,
    Box<dyn FnOnce() -> Result<(), StoreError>>,
);
thread_local! {
    static CHECKPOINT_HOOK: std::cell::RefCell<Option<CheckpointHook>> = const { std::cell::RefCell::new(None) };
}
pub(super) fn checkpoint_boundary(point: CheckpointBoundary) -> Result<(), StoreError> {
    let hook = CHECKPOINT_HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|(wanted, _)| *wanted == point) {
            slot.take()
        } else {
            None
        }
    });
    if let Some((_, hook)) = hook {
        hook()?;
    }
    Ok(())
}

// Mutations happen after real pages, next log, and CURRENT temporary have synced.
#[test]
fn checkpoint_boundary_preserves_changed_current() {
    for boundary in [CheckpointBoundary::Prepared, CheckpointBoundary::Published] {
        for replace in [false, true] {
            let (_temp, path) = fixture();
            let mut store = ConnectionStore::open_at(&path).unwrap();
            let record = create(&mut store, 1);
            store
                .request_disconnect(record.id, record.revision)
                .unwrap();
            let old_log = log_path(&path);
            let log_bytes = std::fs::read(&old_log).unwrap();
            let target = path.join("CURRENT");
            let changed = path.clone();
            CHECKPOINT_HOOK.with(|slot| {
                *slot.borrow_mut() = Some((
                    boundary,
                    Box::new(move || {
                        assert!(changed.join("log-1").exists());
                        if replace {
                            // Identical bytes isolate retained inode identity validation.
                            std::fs::rename(
                                changed.join("CURRENT"),
                                changed.join("saved-current"),
                            )?;
                            std::fs::copy(changed.join("saved-current"), changed.join("CURRENT"))?;
                        } else {
                            std::fs::write(changed.join("CURRENT"), b"unexpected CURRENT bytes")?;
                        }
                        Ok(())
                    }),
                ));
            });
            let result = store.checkpoint();
            assert!(
                matches!(result, Err(StoreError::IdentityChanged)),
                "{:?}",
                result.err()
            );
            let expected = if replace {
                std::fs::read(path.join("saved-current")).unwrap()
            } else {
                b"unexpected CURRENT bytes".to_vec()
            };
            assert_eq!(std::fs::read(&target).unwrap(), expected);
            assert_eq!(std::fs::read(&old_log).unwrap(), log_bytes);
            assert!(store.poisoned);
            assert!(matches!(
                store.checkpoint(),
                Err(StoreError::RecoveryRequired)
            ));
            drop(store);
            if replace {
                let reopened = ConnectionStore::open_at(&path).unwrap();
                assert_eq!(
                    reopened.load_connection(record.id).unwrap().lifecycle,
                    Lifecycle::Disconnecting
                );
            } else {
                assert!(ConnectionStore::open_at(&path).is_err());
            }
            assert_eq!(std::fs::read(target).unwrap(), expected);
        }
    }
}

// Changes during both prepublication preparation and final directory sync must
// remain evidence; adopting a new snapshot must not hide the changed old log.
#[test]
fn checkpoint_boundary_preserves_changed_old_log() {
    for boundary in [CheckpointBoundary::Prepared, CheckpointBoundary::Published] {
        let (_temp, path) = fixture();
        let mut store = ConnectionStore::open_at(&path).unwrap();
        let record = create(&mut store, 1);
        store
            .request_disconnect(record.id, record.revision)
            .unwrap();
        let old_log = log_path(&path);
        let old_current = std::fs::read(path.join("CURRENT")).unwrap();
        let mut unexpected = std::fs::read(&old_log).unwrap();
        unexpected.extend_from_slice(b"unexpected old log tail");
        let target = old_log.clone();
        CHECKPOINT_HOOK.with(|slot| {
            *slot.borrow_mut() = Some((
                boundary,
                Box::new(move || {
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(target)?
                        .write_all(b"unexpected old log tail")?;
                    Ok(())
                }),
            ));
        });
        let result = store.checkpoint();
        assert!(
            matches!(result, Err(StoreError::IdentityChanged)),
            "{:?}",
            result.err()
        );
        assert_eq!(std::fs::read(&old_log).unwrap(), unexpected);
        if boundary == CheckpointBoundary::Prepared {
            assert_eq!(std::fs::read(path.join("CURRENT")).unwrap(), old_current);
        }
        assert!(store.poisoned);
        assert!(matches!(
            store.checkpoint(),
            Err(StoreError::RecoveryRequired)
        ));
        assert_eq!(std::fs::read(&old_log).unwrap(), unexpected);
        if boundary == CheckpointBoundary::Published {
            drop(store);
            let mut reopened = ConnectionStore::open_at(&path).unwrap();
            assert!(matches!(
                reopened.gc_step(),
                Err(StoreError::RecoveryRequired)
            ));
            assert_eq!(std::fs::read(old_log).unwrap(), unexpected);
            assert!(matches!(
                reopened.load_connection(record.id),
                Err(StoreError::RecoveryRequired)
            ));
        }
    }
}

// Existing active-intent admission must keep working alongside exact row admission.
#[test]
fn admission_active_intent_capacity_preserves_log_and_fence_reserve() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 1);
    let original = std::fs::read(log_path(&path)).unwrap();
    store.active_intents = 1024;
    assert!(matches!(
        store.begin_intent(intent(&record)),
        Err(StoreError::RecoveryRequired)
    ));
    assert_eq!(std::fs::read(log_path(&path)).unwrap(), original);
    store
        .request_disconnect(record.id, record.revision)
        .unwrap();
    drop(store);
    assert_eq!(
        ConnectionStore::open_at(&path)
            .unwrap()
            .load_connection(record.id)
            .unwrap()
            .lifecycle,
        Lifecycle::Disconnecting
    );
}

#[cfg(windows)]
pub(super) fn native_fault_after(point: Option<usize>) {
    FAULT.with(|fault| fault.set(point.map(|point| (point, false))));
}
#[cfg(windows)]
pub(super) fn native_checkpoint_hook(
    point: CheckpointBoundary,
    hook: Box<dyn FnOnce() -> Result<(), StoreError>>,
) {
    CHECKPOINT_HOOK.with(|slot| *slot.borrow_mut() = Some((point, hook)));
}

// A stale progress token, durable fence or new store session must close native admission.
#[test]
fn native_live_admission_rejects_stale_revision_fence_and_reopened_session() {
    use super::super::transaction::StepKind;
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 701);
    let session = store.transaction_session(record.id).unwrap();
    let live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let step = session
        .admit_step(&store, &live, StepKind::Namespace, &cancel)
        .unwrap();
    assert!(matches!(
        session.admit_step(&store, &live, StepKind::Namespace, &cancel),
        Err(StoreError::Busy)
    ));
    drop(step);
    assert!(
        session
            .admit_step(&store, &live, StepKind::Namespace, &cancel)
            .is_ok()
    );
    session.request_disconnect();
    assert!(matches!(
        session.admit_step(&store, &live, StepKind::Namespace, &cancel),
        Err(StoreError::Cancelled)
    ));
    assert_eq!(
        store.load_connection(record.id).unwrap().lifecycle,
        Lifecycle::Active
    );
    store
        .request_disconnect(record.id, live.revision())
        .unwrap();
    assert!(matches!(
        store.check_native(&live),
        Err(StoreError::Disconnected)
    ));
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    assert!(matches!(
        reopened.check_native(&live),
        Err(StoreError::StaleRevision)
    ));
    assert_eq!(live.lifetime().strong_count(), 0);
}

// Catches private namespace creation lacking a durable exact registration, stale
// progress reuse, and attempt exhaustion incorrectly releasing retained names.
#[test]
fn native_namespace_reservation_is_exact_durable_and_bounded() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 702);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let reservation = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [7; 16])
        .unwrap();
    assert_eq!(
        reservation.name(),
        ".qos-operation-000000000000000000000000000002be-0000000000000001-07070707070707070707070707070707"
    );
    assert_eq!(live.revision(), 3);
    assert_eq!(store.connection(record.id).unwrap().revision, 3);
    assert!(matches!(
        store.reserve_native_namespace_with_nonce(&mut live, Side::Destination, [7; 16]),
        Err(StoreError::StaleRevision)
    ));
    for nonce in 8..=14 {
        store
            .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [nonce; 16])
            .unwrap();
    }
    assert!(matches!(
        store.reserve_native_namespace_with_nonce(&mut live, Side::Destination, [15; 16]),
        Err(StoreError::LimitExceeded)
    ));
    store.checkpoint().unwrap();
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    let rows = reopened
        .rows()
        .unwrap()
        .map(Result::unwrap)
        .filter(|row| matches!(row, Row::NativeRegistration(_)))
        .count();
    assert_eq!(rows, 8);
    assert_eq!(live.lifetime().strong_count(), 0);
    assert_eq!(reservation.lifetime.strong_count(), 0);
}

// Reducer fixtures are historical facts only; these never construct live native proofs.
fn native_prepared_facts(
    live: &LiveOperation,
    reservation: &NamespaceReservation,
) -> format::NativeRegistration {
    use super::super::transaction::native::descriptor as d;
    let object = |kind, generation| d::Object {
        dev: 1,
        ino: u64::from(generation),
        kind,
        generation: [generation; 32],
    };
    let root = object(0o040_000, 10);
    let content = d::Content {
        digest: [7; 32],
        length: 3,
        executable: false,
    };
    let stamp = d::Stamp {
        length: 3,
        modified: (0, 0),
        changed: (0, 0),
        mode: 0o100_600,
        uid: 0,
        gid: 0,
        links: 1,
    };
    let source = object(0o100_000, 11);
    let identity = |id: EndpointIdentity| {
        let mut value = [0; 64];
        value[..32].copy_from_slice(&id.volume);
        value[32..].copy_from_slice(&id.root);
        value
    };
    let header = d::Header {
        connection: live.id.connection.0,
        operation: live.id.serial,
        attempt: 1,
        nonce: reservation.registration.nonce,
        epoch: [20; 16],
        session: [21; 16],
        admission: live.admission,
        baseline: live.baseline,
        progress: live.revision,
        fence: live.fence,
    };
    let intent = d::Body::Intent(d::Intent {
        endpoints: [identity(live.endpoints[0]), identity(live.endpoints[1])],
        direction: 0,
        decision: 1,
        namespace: object(0o040_000, 12),
        path: live.intent.path.clone(),
        chains: [vec![root.clone()], vec![root.clone()]],
        expected: [
            d::State::Regular {
                object: source.clone(),
                stamp: stamp.clone(),
                content: content.clone(),
            },
            d::State::Absent,
        ],
        selection: d::Selection::None,
    });
    let bytes = d::encode(&header, &intent).unwrap();
    let stage = d::Body::Stage(d::Stage {
        intent: d::decode(&bytes).unwrap().digest,
        object: object(0o100_000, 13),
        content,
        source: d::Source::Endpoint {
            side: 0,
            parent: root,
            object: source,
            stamp,
        },
    });
    let mut registration = reservation.registration.clone();
    registration.intent = Some(bytes);
    registration.stage = Some(d::encode(&header, &stage).unwrap());
    registration
}
#[test]
fn reopened_prepared_addition_remains_eligible_for_fresh_inspection() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 729);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let reservation = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [7; 16])
        .unwrap();
    let registration = native_prepared_facts(&live, &reservation);
    store
        .append(Change::NativePrepared {
            registration,
            expected: live.revision,
        })
        .unwrap();
    store.checkpoint().unwrap();
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let current = store.load_connection(record.id).unwrap();
    let mut context = store
        .open_recovery_context(live.id(), current.revision)
        .unwrap();
    assert!(
        !context.recovery_only,
        "PreparedNoDisplacement is pending work, not a completed operation"
    );
    assert!(context.reads.charge(1).is_err());
    let cancel = AtomicBool::new(false);
    store
        .reserve_recovery_reads(
            &mut context,
            1,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &cancel,
        )
        .unwrap();
    assert!(context.reads.charge(1).is_ok());
    assert_eq!(
        store.pending_page(record.id, None, 8).unwrap().entries[0].stage,
        ReceiptStage::PreparedNoDisplacement
    );
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
    let session = store.transaction_session(record.id).unwrap();
    session.request_disconnect();
    let current = store.load_connection(record.id).unwrap();
    let stopped = store
        .open_recovery_context(live.id(), current.revision)
        .unwrap();
    assert!(stopped.recovery_only);
}
#[test]
fn native_prepared_addition_can_publish_without_dummy_recovery() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 703);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    store
        .reserve_native_reads(
            &mut live,
            &session,
            16 * 1024 * 1024,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &AtomicBool::new(false),
        )
        .unwrap();
    let reservation = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [7; 16])
        .unwrap();
    let registration = native_prepared_facts(&live, &reservation);
    store
        .append(Change::NativePrepared {
            registration,
            expected: live.revision,
        })
        .unwrap();
    let connection = store.connection(record.id).unwrap();
    let current = store.resume_native_for_test(live.id(), &session).unwrap();
    let cancel = AtomicBool::new(false);
    let context = store
        .native_read_context(
            &current,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &cancel,
        )
        .unwrap();
    store
        .append_native(
            Change::Published {
                connection: record.id.0,
                serial: live.id.serial,
                expected: connection.revision,
                source: connection.source.identity,
                destination: connection.destination.identity,
            },
            &context,
        )
        .unwrap();
    let operation = store.operation(live.id).unwrap();
    assert_eq!(operation.decode().unwrap().stage, ReceiptStage::Published);
    assert!(operation.recovery.is_empty());
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        reopened.operation(live.id).unwrap().decode().unwrap().stage,
        ReceiptStage::Published
    );
    assert!(
        reopened
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .is_none()
    );
}

// A closed control latch must reject a new intent even before fence fsync.
#[test]
fn native_closed_session_cannot_begin_an_intent() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 704);
    let session = store.transaction_session(record.id).unwrap();
    session.request_disconnect();
    assert!(matches!(
        store.begin_native_for_test(intent(&record), &session),
        Err(StoreError::Cancelled)
    ));
    assert_eq!(
        store.load_connection(record.id).unwrap().revision,
        record.revision
    );
}

#[test]
fn native_dropped_closed_session_cannot_begin_an_intent() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 708);
    let session = store.transaction_session(record.id).unwrap();
    session.request_disconnect();
    drop(session);
    let reacquired = store.transaction_session(record.id).unwrap();
    assert!(matches!(
        store.begin_native_for_test(intent(&record), &reacquired),
        Err(StoreError::Cancelled)
    ));
    assert_eq!(store.load_connection(record.id).unwrap(), record);
}

#[test]
fn native_dropped_closed_session_cannot_admit_existing_operation() {
    use crate::connection::transaction::StepKind;
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 709);
    let session = store.transaction_session(record.id).unwrap();
    let live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let step = session
        .admit_step(&store, &live, StepKind::Namespace, &cancel)
        .unwrap();
    session.request_disconnect();
    drop(session);
    drop(step);
    let reacquired = store.transaction_session(record.id).unwrap();
    assert!(matches!(
        reacquired.admit_step(&store, &live, StepKind::Namespace, &cancel),
        Err(StoreError::Cancelled)
    ));
    let current = store.load_connection(record.id).unwrap();
    assert_eq!(current.lifecycle, Lifecycle::Active);
    assert_eq!(current.fence_generation, 0);
    assert_eq!(current.revision, live.revision());
}

// The exact durable reservation can produce context only at its current revision.
#[test]
fn native_namespace_context_rejects_stale_attempt_and_session() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 705);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let reservation = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [4; 16])
        .unwrap();
    let header = reservation.header(&live, [9; 16]).unwrap();
    assert_eq!(header.operation, 1);
    assert_eq!(
        header.session,
        reservation.header(&live, [10; 16]).unwrap().session
    );
    assert_eq!(header.namespace_name(), reservation.name());
    let next = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [5; 16])
        .unwrap();
    assert!(matches!(
        reservation.header(&live, [9; 16]),
        Err(StoreError::StaleRevision)
    ));
    assert!(next.header(&live, [9; 16]).is_ok());
    drop(store);
    assert!(matches!(
        next.header(&live, [9; 16]),
        Err(StoreError::StaleRevision)
    ));
}

// Historical facts must bind the actual displaced generation AND the freshly
// copied private artifact. A recovery row on the same side is insufficient.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One real recovery artifact is checked against each substituted fact and then checkpointed and reopened."
)]
fn native_recovery_fact_requires_actual_version_and_full_artifact() {
    use super::super::transaction::native::descriptor as d;
    use sha2::Digest;
    let (temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 706);
    let session = store.transaction_session(record.id).unwrap();
    let mut proposed = intent(&record);
    let original_digest: [u8; 32] = sha2::Sha256::digest(b"old").into();
    proposed.destination_before = Some(EntryState::File {
        sha256: original_digest,
        bytes: 3,
        executable: false,
    });
    let mut live = store.begin_native_for_test(proposed, &session).unwrap();
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    store
        .reserve_native_reads(&mut live, &session, 16 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    let namespace = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [7; 16])
        .unwrap();
    let mut registration = native_prepared_facts(&live, &namespace);
    let mut intent = d::decode(registration.intent.as_ref().unwrap()).unwrap();
    let d::Body::Intent(body) = &mut intent.body else {
        unreachable!()
    };
    let d::State::Regular { object, stamp, .. } = body.expected[0].clone() else {
        unreachable!()
    };
    let mut original = object;
    original.ino = 41;
    original.generation = [41; 32];
    let original_content = d::Content {
        digest: original_digest,
        length: 3,
        executable: false,
    };
    body.expected[1] = d::State::Regular {
        object: original.clone(),
        stamp: stamp.clone(),
        content: original_content.clone(),
    };
    let parent = body.chains[1][0].clone();
    registration.intent = Some(d::encode(&intent.header, &intent.body).unwrap());
    let intent = d::decode(registration.intent.as_ref().unwrap()).unwrap();
    let mut staged = d::decode(registration.stage.as_ref().unwrap()).unwrap();
    let d::Body::Stage(stage) = &mut staged.body else {
        unreachable!()
    };
    stage.intent = intent.digest;
    registration.stage = Some(d::encode(&staged.header, &staged.body).unwrap());
    let staged = d::decode(registration.stage.as_ref().unwrap()).unwrap();
    store
        .append(Change::NativePrepared {
            registration,
            expected: live.revision,
        })
        .unwrap();
    let displaced = d::encode(
        &intent.header,
        &d::Body::Displaced(d::Displaced {
            intent: intent.digest,
            stage: Some(staged.digest),
            object: original.clone(),
            stamp,
            matches_expected: true,
            side: 1,
            parent,
            leaf: super::super::model::NativeName::Unix(b"file".to_vec()),
            barriers: 3,
        }),
    )
    .unwrap();
    let ack = store.ack(live.id);
    let reservation = store
        .reserve_recovery(
            &ack,
            Side::Destination,
            live.intent.path.clone(),
            RecoveryReason::DisplacedVersion,
        )
        .unwrap();
    let version_path = temp.path().join("version");
    std::fs::write(&version_path, b"old").unwrap();
    let current = store.resume_native_for_test(live.id(), &session).unwrap();
    let context = store
        .native_read_context(&current, deadline, &cancel)
        .unwrap();
    let preserved = store
        .preserve_file_with_context(
            reservation,
            &mut File::open(&version_path).unwrap(),
            &cancel,
            Some(&context),
        )
        .unwrap();
    let id = preserved.value.id;
    let mut key = format::key(2, id.operation.connection.0, id.operation.serial);
    key.extend(id.slot.to_be_bytes());
    let Some(Row::Recovery(entry)) = store.row(&key).unwrap() else {
        unreachable!()
    };
    let artifact = entry.object.as_ref().unwrap();
    let mut endpoint = [0; 64];
    endpoint[..32].copy_from_slice(&artifact.root.volume);
    endpoint[32..].copy_from_slice(&artifact.root.root);
    let mut identity = [0; 24];
    let platform = match artifact.file_id {
        format::FileIdentity::Unix(inode) => {
            identity[..8].copy_from_slice(&inode.to_le_bytes());
            0
        }
        format::FileIdentity::Windows(id) => {
            identity[..8].copy_from_slice(&id.volume_serial.to_le_bytes());
            identity[8..].copy_from_slice(&id.file_id);
            1
        }
    };
    let mut origin = [0; 64];
    origin[..32].copy_from_slice(&record.destination.identity.volume);
    origin[32..].copy_from_slice(&record.destination.identity.root);
    let fact = d::RecoveryFact {
        intent: intent.digest,
        recovery: d::Recovery {
            connection: id.operation.connection.0,
            operation: id.operation.serial,
            slot: id.slot,
        },
        side: 1,
        role: 0,
        endpoint: origin,
        object: original,
        content: original_content,
        artifact: d::Artifact {
            endpoint,
            platform,
            identity,
        },
    };
    let row = format::NativeRecoveryFact {
        captured: None,
        connection: id.operation.connection.0,
        serial: id.operation.serial,
        slot: id.slot,
        attempt: 1,
        fact: fact.encode().unwrap(),
        displaced,
    };
    let expected = store.connection(record.id).unwrap().revision;
    let connection = store.connection(record.id).unwrap();
    let premature = Event {
        schema: 1,
        predecessor: store.digest,
        change: Change::Published {
            connection: record.id.0,
            serial: live.id.serial,
            expected,
            source: connection.source.identity.clone(),
            destination: connection.destination.identity.clone(),
        },
    };
    assert!(matches!(
        store.reduce(
            &premature,
            &mut RecoveryChecks {
                mode: VerificationMode::NativeLive(&context),
                ..RecoveryChecks::default()
            }
        ),
        Err(StoreError::RecoveryRequired)
    ));
    store
        .append_native(
            Change::NativeBound {
                fact: row.clone(),
                expected,
                fence: 0,
            },
            &context,
        )
        .unwrap();
    assert!(matches!(
        store.row(&row.key()).unwrap(),
        Some(Row::NativeRecoveryFact(_))
    ));
    let accounted = Event {
        schema: 1,
        predecessor: store.digest,
        change: Change::Published {
            connection: record.id.0,
            serial: live.id.serial,
            expected: expected + 1,
            source: connection.source.identity,
            destination: connection.destination.identity,
        },
    };
    assert!(
        store
            .reduce(
                &accounted,
                &mut RecoveryChecks {
                    mode: VerificationMode::NativeLive(&context),
                    ..RecoveryChecks::default()
                }
            )
            .is_ok()
    );

    store.checkpoint().unwrap();
    drop(preserved);
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        store.row(&row.key()).unwrap(),
        Some(Row::NativeRecoveryFact(row.clone()))
    );
    for field in 0..4 {
        let mut changed = fact.clone();
        match field {
            0 => changed.object.generation[0] ^= 1,
            1 => changed.artifact.identity[0] ^= 1,
            2 => changed.content.digest[0] ^= 1,
            _ => changed.endpoint[0] ^= 1,
        }
        let changed = format::NativeRecoveryFact {
            captured: None,
            fact: changed.encode().unwrap(),
            ..row.clone()
        };
        assert!(
            matches!(
                store.reduce_native_bound(&changed, expected + 1, 0),
                Err(StoreError::Corrupt)
            ),
            "field {field}"
        );
    }
}

// Admission is for one exact store progress revision, even if a caller retains
// the old token and its original revision after another durable store change.
#[test]
fn native_step_is_invalid_after_intervening_durable_progress() {
    use super::super::transaction::StepKind;
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 707);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let revision = live.revision();
    let step = session
        .admit_step(&store, &live, StepKind::Namespace, &AtomicBool::new(false))
        .unwrap();
    assert!(
        step.validate(live.id(), live.lifetime(), revision, StepKind::Namespace)
            .is_ok()
    );
    assert!(matches!(
        step.validate(live.id(), live.lifetime(), revision, StepKind::Publish),
        Err(StoreError::StaleRevision)
    ));
    store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [22; 16])
        .unwrap();
    assert!(matches!(
        step.validate(live.id(), live.lifetime(), revision, StepKind::Namespace),
        Err(StoreError::StaleRevision)
    ));
    drop(step);
    let fresh = session
        .admit_step(&store, &live, StepKind::Namespace, &AtomicBool::new(false))
        .unwrap();
    assert!(
        fresh
            .validate(
                live.id(),
                live.lifetime(),
                live.revision(),
                StepKind::Namespace
            )
            .is_ok()
    );
    drop(store);
    assert!(matches!(
        fresh.validate(
            live.id(),
            live.lifetime(),
            live.revision(),
            StepKind::Namespace
        ),
        Err(StoreError::StaleRevision)
    ));
}

// An uncertain durable store invalidates live native context before a retained
// admitted token could be used after the failure.
#[test]
fn native_store_failure_invalidates_admitted_tokens() {
    use super::super::transaction::StepKind;
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 708);
    let session = store.transaction_session(record.id).unwrap();
    let live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let step = session
        .admit_step(&store, &live, StepKind::Namespace, &AtomicBool::new(false))
        .unwrap();
    store.log.set_len(0).unwrap();
    assert!(store.checkpoint().is_err());
    assert!(matches!(
        step.validate(
            live.id(),
            live.lifetime(),
            live.revision(),
            StepKind::Namespace
        ),
        Err(StoreError::StaleRevision)
    ));
}

#[test]
fn native_intent_atomically_records_budget_marker_while_legacy_stays_unknown() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 710);
    let session = store.transaction_session(record.id).unwrap();
    let live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let encoded = serde_json::to_value(store.operation(live.id()).unwrap()).unwrap();
    assert_eq!(encoded["native_budget"]["version"], 1);
    assert_eq!(encoded["native_budget"]["reserved"], 0);
    assert_eq!(encoded["native_budget"]["sequence"], 0);
    let id = live.id();
    store.checkpoint().unwrap();
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        serde_json::to_value(store.operation(id).unwrap()).unwrap(),
        encoded
    );
    let legacy = create(&mut store, 711);
    let operation = store.begin_intent(intent(&legacy)).unwrap().value;
    let encoded = serde_json::to_value(store.operation(operation).unwrap()).unwrap();
    assert!(
        encoded
            .get("native_budget")
            .is_none_or(serde_json::Value::is_null)
    );
}

#[test]
fn native_read_reservations_are_durable_shared_and_burn_on_reopen() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 712);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let cancel = AtomicBool::new(false);
    assert!(live.operation_reads().charge(1).is_err());
    store
        .reserve_native_reads(&mut live, &session, 1, deadline, &cancel)
        .unwrap();
    let budget = store.operation(live.id()).unwrap().native_budget.unwrap();
    assert_eq!(budget.reserved, format::READ_QUANTUM);
    assert_eq!(budget.sequence, 1);
    assert_eq!(
        store.load_connection(record.id).unwrap().revision,
        live.revision()
    );
    let same = store.resume_native_for_test(live.id(), &session).unwrap();
    assert!(Arc::ptr_eq(live.operation_reads(), same.operation_reads()));
    assert!(
        same.operation_reads()
            .charge(format::READ_QUANTUM - 1)
            .is_ok()
    );
    assert!(live.operation_reads().charge(1).is_ok());
    assert!(same.operation_reads().charge(1).is_err());
    let id = live.id();
    store
        .reserve_native_reads(&mut live, &session, 1, deadline, &cancel)
        .unwrap();
    store.checkpoint().unwrap();
    drop(same);
    drop(live);
    drop(session);
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store.resume_native_for_test(id, &session).unwrap();
    assert!(live.operation_reads().charge(1).is_err());
    assert_eq!(
        store.operation(id).unwrap().native_budget.unwrap().reserved,
        2 * format::READ_QUANTUM
    );
    store
        .reserve_native_reads(&mut live, &session, 1, deadline, &cancel)
        .unwrap();
    assert!(live.operation_reads().charge(format::READ_QUANTUM).is_ok());
    assert!(live.operation_reads().charge(1).is_err());
    assert_eq!(
        store.operation(id).unwrap().native_budget.unwrap().sequence,
        3
    );
}

#[test]
fn native_read_reservations_refuse_busy_stale_cancelled_and_over_limit() {
    use crate::connection::transaction::StepKind;
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 713);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let mut stale = store.resume_native_for_test(live.id(), &session).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let cancel = AtomicBool::new(false);
    let step = session
        .admit_step(&store, &live, StepKind::Namespace, &cancel)
        .unwrap();
    assert!(matches!(
        store.reserve_native_reads(&mut live, &session, 1, deadline, &cancel),
        Err(StoreError::Busy)
    ));
    drop(step);
    assert!(matches!(
        store.reserve_native_reads(&mut live, &session, u64::MAX, deadline, &cancel),
        Err(StoreError::LimitExceeded)
    ));
    cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        store.reserve_native_reads(&mut live, &session, 1, deadline, &cancel),
        Err(StoreError::Cancelled)
    ));
    cancel.store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        store
            .operation(live.id())
            .unwrap()
            .native_budget
            .unwrap()
            .reserved,
        0
    );
    store
        .reserve_native_reads(&mut live, &session, format::READ_LIMIT, deadline, &cancel)
        .unwrap();
    assert!(matches!(
        store.reserve_native_reads(&mut stale, &session, 1, deadline, &cancel),
        Err(StoreError::StaleRevision)
    ));
    assert!(matches!(
        store.reserve_native_reads(&mut live, &session, 1, deadline, &cancel),
        Err(StoreError::LimitExceeded)
    ));
    assert!(live.operation_reads().charge(format::READ_LIMIT).is_ok());
    assert!(live.operation_reads().charge(1).is_err());
    session.request_disconnect();
    assert!(matches!(
        store.reserve_native_reads(&mut live, &session, 1, deadline, &cancel),
        Err(StoreError::Cancelled)
    ));
    let legacy = create(&mut store, 714);
    let id = store.begin_intent(intent(&legacy)).unwrap().value;
    let session = store.transaction_session(legacy.id).unwrap();
    assert!(matches!(
        store.resume_native_for_test(id, &session),
        Err(StoreError::RecoveryRequired)
    ));
}

#[test]
fn native_historical_replay_defers_current_artifact_verification() {
    use sha2::Digest;
    let (temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 715);
    let session = store.transaction_session(record.id).unwrap();
    let mut proposed = intent(&record);
    proposed.destination_before = Some(EntryState::File {
        sha256: sha2::Sha256::digest(b"old").into(),
        bytes: 3,
        executable: false,
    });
    let mut live = store.begin_native_for_test(proposed, &session).unwrap();
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    store
        .reserve_native_reads(&mut live, &session, 16 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    let reservation = store
        .reserve_recovery(
            &store.ack(live.id()),
            Side::Destination,
            live.intent().path.clone(),
            RecoveryReason::DisplacedVersion,
        )
        .unwrap();
    let source = temp.path().join("old");
    std::fs::write(&source, b"old").unwrap();
    let current = store.resume_native_for_test(live.id(), &session).unwrap();
    let context = store
        .native_read_context(&current, deadline, &cancel)
        .unwrap();
    let preserved = store
        .preserve_file_with_context(
            reservation,
            &mut File::open(&source).unwrap(),
            &cancel,
            Some(&context),
        )
        .unwrap();
    let connection = store.connection(record.id).unwrap();
    store
        .append_native(
            Change::Published {
                connection: record.id.0,
                serial: live.id().serial,
                expected: connection.revision,
                source: connection.source.identity,
                destination: connection.destination.identity,
            },
            &context,
        )
        .unwrap();
    let object = path
        .join("recovery")
        .join(format::hex(&record.id.0))
        .join("1-0");
    drop(preserved);
    drop(live);
    drop(session);
    drop(store);
    std::fs::write(&object, b"changed").unwrap();
    let mut store = ConnectionStore::open_at(&path)
        .expect("historical marked facts can replay without minting current proof");
    let mut key = format::key(2, record.id.0, 1);
    key.extend(0_u32.to_be_bytes());
    let Some(Row::Recovery(entry)) = store.row(&key).unwrap() else {
        panic!("retained history");
    };
    assert!(store.verify_recovery(&entry).is_err()); // No native legacy bypass.
    let session = store.transaction_session(record.id).unwrap();
    let mut current = store
        .resume_native_for_test(entry.id().operation, &session)
        .unwrap();
    store
        .reserve_native_reads(&mut current, &session, 1024 * 1024, deadline, &cancel)
        .unwrap();
    let context = store
        .native_read_context(&current, deadline, &cancel)
        .unwrap();
    assert!(
        store
            .verify_recovery_with_context(&entry, Some(&context))
            .is_err()
    );
    assert_eq!(std::fs::read(&object).unwrap(), b"changed");
}

#[test]
fn native_paid_private_copy_and_reread_charge_short_reads_and_eof() {
    let (temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 716);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    store
        .reserve_native_reads(&mut live, &session, 1, deadline, &cancel)
        .unwrap();
    let reservation = store
        .reserve_recovery(
            &store.ack(live.id()),
            Side::Source,
            live.intent().path.clone(),
            RecoveryReason::DisplacedVersion,
        )
        .unwrap();
    let current = store.resume_native_for_test(live.id(), &session).unwrap();
    let context = store
        .native_read_context(&current, deadline, &cancel)
        .unwrap();
    let source = temp.path().join("short-source");
    std::fs::write(&source, b"abc").unwrap();
    let physical_trace = storage::physical_trace::begin();
    let preserved = store
        .preserve_file_with_context(
            reservation,
            &mut File::open(&source).unwrap(),
            &cancel,
            Some(&context),
        )
        .unwrap();
    let mut key = format::key(2, record.id.0, live.id().serial);
    key.extend(preserved.value.id.slot.to_be_bytes());
    let Some(Row::Recovery(entry)) = store.row(&key).unwrap() else {
        panic!("bytes row");
    };
    assert!(store.verify_recovery(&entry).is_err());
    drop(
        store
            .verify_recovery_with_context(&entry, Some(&context))
            .unwrap(),
    );
    let trace = physical_trace.finish();
    let payload: Vec<_> = trace
        .reads
        .iter()
        .filter(|read| read.name.starts_with("payload-"))
        .collect();
    assert_eq!(payload.len(), 6);
    for (index, read) in payload.iter().enumerate() {
        assert_eq!(
            read.name,
            if index < 2 {
                "payload-copy"
            } else {
                "payload-hash"
            }
        );
        assert_eq!(read.requested, 256 * 1024);
        assert_eq!(read.returned, Some(if index % 2 == 0 { 3 } else { 0 }));
    }
    let currents: Vec<_> = trace
        .reads
        .iter()
        .filter(|read| read.name == "CURRENT")
        .collect();
    assert_eq!(currents.len(), 15 * 2);
    for pair in currents.chunks_exact(2) {
        let length = usize::try_from(pair[0].length.unwrap()).unwrap();
        assert!(length < 4096);
        assert_eq!(pair[1].length, Some(length as u64));
        assert_eq!((pair[0].requested, pair[0].returned), (4096, Some(length)));
        assert_eq!(
            (pair[1].requested, pair[1].returned),
            (4097 - length, Some(0))
        );
    }
    assert_eq!(trace.reads.len(), payload.len() + currents.len());
    let requested: u64 = trace.reads.iter().map(|read| read.requested as u64).sum();
    eprintln!(
        "private-copy actual_reads={} current_passes={} payload_requests={} requested={requested} per_cut_lengths={:?}",
        trace.reads.len(),
        currents.len() / 2,
        payload.len(),
        currents
            .chunks_exact(2)
            .map(|p| p[0].length.unwrap())
            .collect::<Vec<_>>()
    );
    // Three real payload passes retain six full requests; every metadata read
    // is independently observed at Read::read, including its actual EOF cut.
    assert!(
        live.operation_reads()
            .charge(format::READ_QUANTUM - requested)
            .is_ok()
    );
    assert!(live.operation_reads().charge(1).is_err());
    assert!(matches!(
        store.verify_recovery_with_context(&entry, Some(&context)),
        Err(StoreError::LimitExceeded)
    ));
    cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        store.verify_recovery_with_context(&entry, Some(&context)),
        Err(StoreError::Cancelled)
    ));
    assert_eq!(std::fs::read(source).unwrap(), b"abc");
}

#[test]
fn native_failed_payload_read_is_charged_and_partial_object_is_retained() {
    let (temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 717);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    store
        .reserve_native_reads(&mut live, &session, 1, deadline, &cancel)
        .unwrap();
    let reservation = store
        .reserve_recovery(
            &store.ack(live.id()),
            Side::Source,
            live.intent().path.clone(),
            RecoveryReason::DisplacedVersion,
        )
        .unwrap();
    let current = store.resume_native_for_test(live.id(), &session).unwrap();
    let context = store
        .native_read_context(&current, deadline, &cancel)
        .unwrap();
    let source = temp.path().join("write-only-source");
    std::fs::write(&source, b"abc").unwrap();
    let mut source = std::fs::OpenOptions::new()
        .write(true)
        .open(source)
        .unwrap();
    let physical_trace = storage::physical_trace::begin();
    assert!(
        store
            .preserve_file_with_context(reservation, &mut source, &cancel, Some(&context))
            .is_err()
    );
    let trace = physical_trace.finish();
    assert_eq!(trace.reads.len(), 7);
    let payload: Vec<_> = trace
        .reads
        .iter()
        .filter(|read| read.name == "payload-copy")
        .collect();
    assert_eq!(payload.len(), 1);
    assert_eq!(
        (payload[0].requested, payload[0].returned),
        (256 * 1024, None)
    );
    let currents: Vec<_> = trace
        .reads
        .iter()
        .filter(|read| read.name == "CURRENT")
        .collect();
    assert_eq!(currents.len(), 6);
    for pair in currents.chunks_exact(2) {
        let length = usize::try_from(pair[0].length.unwrap()).unwrap();
        assert!(length < 4096);
        assert_eq!(pair[1].length, Some(length as u64));
        assert_eq!((pair[0].requested, pair[0].returned), (4096, Some(length)));
        assert_eq!(
            (pair[1].requested, pair[1].returned),
            (4097 - length, Some(0))
        );
    }
    let requested: u64 = trace.reads.iter().map(|read| read.requested as u64).sum();
    eprintln!(
        "failed-copy actual_reads=7 current_passes=3 failed_payload_requests=1 requested={requested} per_cut_lengths={:?}",
        currents
            .chunks_exact(2)
            .map(|p| p[0].length.unwrap())
            .collect::<Vec<_>>()
    );
    assert!(
        live.operation_reads()
            .charge(format::READ_QUANTUM - requested)
            .is_ok()
    );
    assert!(live.operation_reads().charge(1).is_err());
    assert!(
        path.join("recovery")
            .join(format::hex(&record.id.0))
            .join("1-0")
            .exists()
    );
}

// A failed object is retained permanently in its old slot; retry uses a new slot.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn native_incomplete_slot_survives_reopen_and_does_not_block_fresh_capture() {
    let (temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 718);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    store
        .reserve_native_reads(&mut live, &session, 8 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    let reservation = store
        .reserve_recovery(
            &store.ack(live.id()),
            Side::Source,
            live.intent().path.clone(),
            RecoveryReason::DisplacedVersion,
        )
        .unwrap();
    let incomplete = reservation.id;
    let old_path = path
        .join("recovery")
        .join(format::hex(&record.id.0))
        .join("1-0");
    let current = store.resume_native_for_test(live.id(), &session).unwrap();
    let context = store
        .native_read_context(&current, deadline, &cancel)
        .unwrap();
    let unreadable = temp.path().join("write-only-incomplete-source");
    std::fs::write(&unreadable, b"abc").unwrap();
    let mut unreadable = std::fs::OpenOptions::new()
        .write(true)
        .open(unreadable)
        .unwrap();
    assert!(
        store
            .preserve_file_with_context(reservation, &mut unreadable, &cancel, Some(&context))
            .is_err()
    );
    std::fs::write(&old_path, b"partial bytes must remain").unwrap();
    // A read error poisons the old owner; recovery starts from a fresh lock/session.
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store.resume_native_for_test(live.id(), &session).unwrap();
    store
        .reserve_native_reads(&mut live, &session, 8 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    let current = store.load_connection(record.id).unwrap();
    store
        .retain_incomplete_recovery(incomplete, current.revision, current.fence_generation)
        .unwrap();
    assert!(
        store
            .retain_incomplete_recovery(incomplete, current.revision, current.fence_generation)
            .is_err()
    );
    let reservation = store
        .reserve_recovery(
            &store.ack(live.id()),
            Side::Source,
            live.intent().path.clone(),
            RecoveryReason::DisplacedVersion,
        )
        .unwrap();
    assert_eq!(reservation.id.slot, 1);
    let current = store.resume_native_for_test(live.id(), &session).unwrap();
    let context = store
        .native_read_context(&current, deadline, &cancel)
        .unwrap();
    let source = temp.path().join("complete-source");
    std::fs::write(&source, b"abc").unwrap();
    store
        .preserve_file_with_context(
            reservation,
            &mut File::open(&source).unwrap(),
            &cancel,
            Some(&context),
        )
        .unwrap();
    assert_eq!(store.operation(live.id()).unwrap().stage, 1);
    store.checkpoint().unwrap();
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    let entries = reopened.recovery_page(record.id, None, 10).unwrap().entries;
    assert_eq!(entries.len(), 2);
    assert_eq!(format!("{:?}", entries[0].state), "RetainedIncomplete");
    assert_eq!(entries[0].content, None);
    assert_eq!(entries[0].retained_seconds, 0);
    assert_eq!(entries[1].state, RecoveryState::BytesVerified);
    assert_eq!(
        std::fs::read(old_path).unwrap(),
        b"partial bytes must remain"
    );
    assert_eq!(
        std::fs::read(
            path.join("recovery")
                .join(format::hex(&record.id.0))
                .join("1-1")
        )
        .unwrap(),
        b"abc"
    );
}

#[test]
fn native_recovery_only_budget_keeps_stop_latch_closed_across_reopen() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 719);
    let session = store.transaction_session(record.id).unwrap();
    let live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    session.request_disconnect();
    let current = store.load_connection(record.id).unwrap();
    let fence = store
        .request_disconnect(record.id, current.revision)
        .unwrap();
    let current = store.load_connection(record.id).unwrap();
    let mut context = store
        .open_recovery_context(live.id(), current.revision)
        .unwrap();
    assert!(context.recovery_only);
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    store
        .reserve_recovery_reads(&mut context, 1, deadline, &cancel)
        .unwrap();
    assert!(context.reads.charge(1).is_ok());
    assert!(session.require_idle().is_err());
    assert!(store.transaction_session(record.id).is_err());
    assert!(
        store
            .begin_native_for_test(intent(&current), &session)
            .is_err()
    );
    assert!(store.finish_disconnect(&fence).is_err());
    store.checkpoint().unwrap();
    drop(store);
    let mut reopened = ConnectionStore::open_at(&path).unwrap();
    assert!(
        reopened
            .reserve_recovery_reads(&mut context, 1, deadline, &cancel)
            .is_err()
    );
    let current = reopened.load_connection(record.id).unwrap();
    let mut fresh = reopened
        .open_recovery_context(live.id(), current.revision)
        .unwrap();
    assert!(fresh.recovery_only);
    assert!(fresh.reads.charge(1).is_err());
    reopened
        .reserve_recovery_reads(&mut fresh, 1, deadline, &cancel)
        .unwrap();
    assert!(fresh.reads.charge(format::READ_QUANTUM).is_ok());
    assert!(fresh.reads.charge(1).is_err());
    let budget = reopened
        .operation(live.id())
        .unwrap()
        .native_budget
        .unwrap();
    assert_eq!(budget.reserved, 2 * format::READ_QUANTUM);
    assert_eq!(budget.sequence, 2);
    assert!(reopened.transaction_session(record.id).is_err());
    assert!(reopened.begin_intent(intent(&current)).is_err());
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn native_actual_outcome_is_durable_and_binds_prepared_stage() {
    use super::super::transaction::native::descriptor as d;
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 720);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    store
        .reserve_native_reads(
            &mut live,
            &session,
            16 * 1024 * 1024,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &AtomicBool::new(false),
        )
        .unwrap();
    let reservation = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [7; 16])
        .unwrap();
    let mut registration = native_prepared_facts(&live, &reservation);
    store
        .append(Change::NativePrepared {
            registration: registration.clone(),
            expected: live.revision,
        })
        .unwrap();
    let current = store.resume_native_for_test(live.id(), &session).unwrap();
    let cancel = AtomicBool::new(false);
    let context = store
        .native_read_context(
            &current,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &cancel,
        )
        .unwrap();
    let intent = d::decode(registration.intent.as_ref().unwrap()).unwrap();
    let stage = d::decode(registration.stage.as_ref().unwrap()).unwrap();
    let d::Body::Intent(intent_body) = &intent.body else {
        panic!("intent");
    };
    let d::Body::Stage(stage_body) = &stage.body else {
        panic!("stage");
    };
    let d::State::Regular { stamp, .. } = &intent_body.expected[0] else {
        panic!("source");
    };
    let mut header = intent.header.clone();
    header.progress = current.revision;
    let outcome = d::Body::Outcome(d::Outcome {
        intent: intent.digest,
        stage: Some(stage.digest),
        displaced: None,
        states: [
            intent_body.expected[0].clone(),
            d::State::Regular {
                object: stage_body.object.clone(),
                stamp: stamp.clone(),
                content: stage_body.content.clone(),
            },
        ],
        kind: 0,
        barriers: 3,
        recovery: Vec::new(),
        sequence: 1,
    });
    registration.outcome = Some(d::encode(&header, &outcome).unwrap());
    store
        .append_native(
            Change::NativeOutcome {
                reconstructed: false,
                registration: registration.clone(),
                expected: current.revision,
                verified: false,
            },
            &context,
        )
        .unwrap();
    assert_eq!(store.operation(live.id()).unwrap().stage, 2);
    let published = store.resume_native_for_test(live.id(), &session).unwrap();
    let mut wrong = registration.clone();
    let mut wrong_header = header;
    wrong_header.nonce = [8; 16];
    wrong.outcome = Some(d::encode(&wrong_header, &outcome).unwrap());
    assert!(
        store
            .append_native(
                Change::NativeOutcome {
                    reconstructed: false,
                    registration: wrong,
                    expected: published.revision,
                    verified: true
                },
                &context
            )
            .is_err()
    );
    store
        .append_native(
            Change::NativeOutcome {
                reconstructed: false,
                registration: registration.clone(),
                expected: published.revision,
                verified: true,
            },
            &context,
        )
        .unwrap();
    assert_eq!(store.operation(live.id()).unwrap().stage, 3);
    store.checkpoint().unwrap();
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    let Some(Row::NativeRegistration(retained)) = reopened.row(&registration.key()).unwrap() else {
        panic!("retained outcome");
    };
    assert_eq!(retained, registration);
    assert_eq!(reopened.operation(live.id()).unwrap().stage, 3);
    assert!(
        reopened
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .is_none()
    );
}

#[test]
fn native_deletion_requires_matching_committed_baseline() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 721);
    let session = store.transaction_session(record.id).unwrap();
    let mut deletion = intent(&record);
    deletion.decision = Decision::RecoverDeletion { at: Side::Source };
    assert!(matches!(
        store.begin_native_for_test(deletion, &session),
        Err(StoreError::RecoveryRequired)
    ));
    assert!(
        store
            .pending_page(record.id, None, 8)
            .unwrap()
            .entries
            .is_empty()
    );
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
}

fn native_verified_addition_facts(
    store: &mut ConnectionStore,
    record: &ConnectionRecord,
    path: &[u8],
    cancel: &AtomicBool,
) -> LiveOperation {
    use super::super::transaction::native::descriptor as d;
    let session = store.transaction_session(record.id).unwrap();
    let mut operation = intent(record);
    operation.path = RelativePath::unix(path).unwrap();
    operation.expected_baseline = record.baseline_revision;
    let mut live = store.begin_native_for_test(operation, &session).unwrap();
    store
        .reserve_native_reads(
            &mut live,
            &session,
            64 * 1024 * 1024,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            cancel,
        )
        .unwrap();
    let reservation = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [7; 16])
        .unwrap();
    let mut registration = native_prepared_facts(&live, &reservation);
    store
        .append(Change::NativePrepared {
            registration: registration.clone(),
            expected: live.revision,
        })
        .unwrap();
    live = store.resume_native_for_test(live.id(), &session).unwrap();
    let context = store
        .native_read_context(
            &live,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            cancel,
        )
        .unwrap();
    let intent = d::decode(registration.intent.as_ref().unwrap()).unwrap();
    let stage = d::decode(registration.stage.as_ref().unwrap()).unwrap();
    let d::Body::Intent(before) = &intent.body else {
        panic!("intent");
    };
    let d::Body::Stage(staged) = &stage.body else {
        panic!("stage");
    };
    let d::State::Regular { stamp, .. } = &before.expected[0] else {
        panic!("source");
    };
    let mut header = intent.header.clone();
    header.progress = live.revision;
    registration.outcome = Some(
        d::encode(
            &header,
            &d::Body::Outcome(d::Outcome {
                intent: intent.digest,
                stage: Some(stage.digest),
                displaced: None,
                states: [
                    before.expected[0].clone(),
                    d::State::Regular {
                        object: staged.object.clone(),
                        stamp: stamp.clone(),
                        content: staged.content.clone(),
                    },
                ],
                kind: 0,
                barriers: 3,
                recovery: Vec::new(),
                sequence: 1,
            }),
        )
        .unwrap(),
    );
    store
        .append_native(
            Change::NativeOutcome {
                reconstructed: false,
                registration: registration.clone(),
                expected: live.revision,
                verified: false,
            },
            &context,
        )
        .unwrap();
    live = store.resume_native_for_test(live.id(), &session).unwrap();
    store
        .append_native(
            Change::NativeOutcome {
                reconstructed: false,
                registration,
                expected: live.revision,
                verified: true,
            },
            &context,
        )
        .unwrap();
    store.resume_native_for_test(live.id(), &session).unwrap()
}
#[test]
fn native_baseline_patch_cannot_drop_an_unprocessed_path() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 722);
    commit_test_baseline(&mut store, record.id);
    let record = store.load_connection(record.id).unwrap();
    let cancel = AtomicBool::new(false);
    let live = native_verified_addition_facts(&mut store, &record, b"second", &cancel);
    let context = store
        .native_read_context(
            &live,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &cancel,
        )
        .unwrap();
    let row = Row::Baseline(format::BaselineEntry {
        connection: record.id.0,
        revision: live.revision + 1,
        ordinal: 0,
        path: format::NativePath::Unix(b"second".to_vec()),
        state: format::State::File {
            sha256: [7; 32],
            bytes: 3,
            executable: false,
        },
    });
    let root = storage::build_pages(&store.storage, std::iter::once(Ok(row))).unwrap();
    assert!(
        store
            .append_native(
                Change::BaselineCommit { directory_chain: None,
                    directory_ancestors: None,
                    existing_ancestors: None,
                    connection: record.id.0,
                    expected: live.revision,
                    root,
                    count: 1,
                    source: record.source.identity.into(),
                    destination: record.destination.identity.into(),
                    initial_review: false
                },
                &context
            )
            .is_err()
    );
    let baseline = store.load_baseline(record.id).unwrap();
    assert_eq!(baseline.entries().unwrap().len(), 1);
    assert!(
        baseline
            .entries()
            .unwrap()
            .contains_key(&RelativePath::unix(b"file").unwrap())
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn native_baseline_patches_advance_two_paths_separately_and_reopen() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 723);
    commit_test_baseline(&mut store, record.id);
    let cancel = AtomicBool::new(false);
    for (index, name) in [b"second".as_slice(), b"third".as_slice()]
        .into_iter()
        .enumerate()
    {
        let record = store.load_connection(record.id).unwrap();
        let live = native_verified_addition_facts(&mut store, &record, name, &cancel);
        let context = store
            .native_read_context(
                &live,
                std::time::Instant::now() + std::time::Duration::from_secs(30),
                &cancel,
            )
            .unwrap();
        let mut entries = store
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .unwrap()
            .clone();
        assert_eq!(entries.len(), index + 1);
        assert!(!entries.contains_key(&RelativePath::unix(name).unwrap()));
        entries.insert(
            RelativePath::unix(name).unwrap(),
            EntryState::File {
                sha256: [7; 32],
                bytes: 3,
                executable: false,
            },
        );
        let rows = entries
            .into_iter()
            .enumerate()
            .map(|(ordinal, (path, state))| {
                Ok(Row::Baseline(format::BaselineEntry {
                    connection: record.id.0,
                    revision: live.revision + 1,
                    ordinal: u32::try_from(ordinal).unwrap(),
                    path: path.into(),
                    state: state.into(),
                }))
            });
        let root = storage::build_pages(&store.storage, rows).unwrap();
        store
            .append_native(
                Change::BaselineCommit { directory_chain: None,
                    directory_ancestors: None,
                    existing_ancestors: None,
                    connection: record.id.0,
                    expected: live.revision,
                    root,
                    count: u32::try_from(index + 2).unwrap(),
                    source: record.source.identity.into(),
                    destination: record.destination.identity.into(),
                    initial_review: false,
                },
                &context,
            )
            .unwrap();
        assert!(
            store
                .pending_page(record.id, None, 8)
                .unwrap()
                .entries
                .is_empty()
        );
        assert!(
            !store.legacy_native_pending(live.id()).unwrap(),
            "completed legacy native history remains readable with a committed baseline"
        );
        store.checkpoint().unwrap();
        drop(store);
        store = ConnectionStore::open_at(&path).unwrap();
        assert!(!store.legacy_native_pending(live.id()).unwrap());
        let baseline = store.load_baseline(record.id).unwrap();
        let entries = baseline.entries().unwrap();
        assert_eq!(entries.len(), index + 2);
        assert_eq!(
            entries.get(&RelativePath::unix(b"file").unwrap()),
            Some(&EntryState::File {
                sha256: [7; 32],
                bytes: 3,
                executable: false
            })
        );
        assert!(entries.contains_key(&RelativePath::unix(name).unwrap()));
        if index == 0 {
            assert!(!entries.contains_key(&RelativePath::unix(b"third").unwrap()));
        }
    }
    let current = store.load_connection(record.id).unwrap();
    let id = OperationId {
        connection: record.id,
        serial: 3,
    };
    let mut context = store.open_recovery_context(id, current.revision).unwrap();
    assert!(context.recovery_only);
    assert!(context.reads.charge(1).is_err()); // last checkpoint/reopen burned old credit
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    store
        .reserve_recovery_reads(&mut context, 1, deadline, &cancel)
        .unwrap();
    store.check_final_context(&context).unwrap();
    assert_eq!(
        store
            .load_recovery_baseline(&context, deadline, &cancel)
            .unwrap()
            .entries()
            .unwrap()
            .len(),
        3
    );
    assert!(context.reads.charge(format::READ_QUANTUM - 16384).is_ok());
    assert!(context.reads.charge(1).is_err());
    let inspection = store
        .recovery_read_context(&context, deadline, &cancel)
        .unwrap();
    let connection = store.connection(record.id).unwrap();
    assert!(matches!(
        store.append_native(
            Change::Verified {
                connection: record.id.0,
                serial: 3,
                expected: connection.revision,
                source: connection.source.identity,
                destination: connection.destination.identity
            },
            &inspection
        ),
        Err(StoreError::RecoveryRequired)
    ));
}

#[test]
fn native_baseline_page_charges_requested_reads_including_eof() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 724);
    let root = commit_test_baseline(&mut store, record.id);
    let charged = std::cell::Cell::new(0_usize);
    let requests = std::cell::Cell::new(0_usize);
    let meter = |count| {
        charged.set(charged.get() + count);
        requests.set(requests.get() + 1);
        Ok(())
    };
    let page = store.storage.page_metered(root, &meter).unwrap();
    assert_eq!(page.reference(root).unwrap().count, 1);
    assert_eq!(charged.get(), 8192);
    assert_eq!(requests.get(), 2);
    let denied = std::cell::Cell::new(0);
    assert!(matches!(
        store.storage.page_metered(root, &|_| {
            denied.set(denied.get() + 1);
            Err(StoreError::Cancelled)
        }),
        Err(StoreError::Cancelled)
    ));
    assert_eq!(denied.get(), 1);
    assert!(store.storage.page(root).is_ok());
}

#[test]
fn native_page_meter_short_interrupted_error_and_local_cap_are_paid() {
    struct ShortFile {
        file: File,
        max: usize,
        interrupt: bool,
    }
    impl std::io::Read for ShortFile {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if self.interrupt {
                self.interrupt = false;
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            let count = bytes.len().min(self.max);
            std::io::Read::read(&mut self.file, &mut bytes[..count])
        }
    }
    let (temp, _) = fixture();
    let source = temp.path().join("meter-source");
    std::fs::write(&source, b"abcdef").unwrap();
    let requested = std::cell::Cell::new(0_usize);
    let meter = |count| {
        requested.set(requested.get() + count);
        Ok(())
    };
    let mut reader = ShortFile {
        file: File::open(&source).unwrap(),
        max: 2,
        interrupt: true,
    };
    assert_eq!(
        storage::read_metered(&mut reader, format::MAX_BYTES as u64, &meter).unwrap(),
        b"abcdef"
    );
    assert_eq!(requested.get(), 5 * 4096); // interruption, three short reads, EOF
    requested.set(0);
    let mut unreadable = std::fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .unwrap();
    assert!(storage::read_metered(&mut unreadable, format::MAX_BYTES as u64, &meter).is_err());
    assert_eq!(requested.get(), 4096);
    std::fs::write(&source, [9_u8; 64]).unwrap();
    requested.set(0);
    let mut reader = ShortFile {
        file: File::open(&source).unwrap(),
        max: 1,
        interrupt: false,
    };
    assert!(matches!(
        storage::read_metered(&mut reader, format::MAX_BYTES as u64, &meter),
        Err(StoreError::LimitExceeded)
    ));
    assert_eq!(requested.get(), 33 * 4096);
    assert_eq!(std::fs::read(&source).unwrap(), [9_u8; 64]);
}
#[test]
fn native_page_iterator_checks_count_and_candidate_collision_is_metered() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 725);
    let root = commit_test_baseline(&mut store, record.id);
    let requested = std::cell::Cell::new(0_usize);
    let meter = |count| {
        requested.set(requested.get() + count);
        Ok(())
    };
    assert!(matches!(
        storage::TreeIter::new_metered_baseline(&store.storage, Some(root), 2, &meter),
        Err(StoreError::Corrupt)
    ));
    assert_eq!(requested.get(), 8192); // rejected after the first validated root
    requested.set(0);
    let rows = storage::TreeIter::new_metered_baseline(&store.storage, Some(root), 1, &meter)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(requested.get(), 16384); // constructor root and traversal root both pay
    requested.set(0);
    let same =
        storage::build_pages_with(&store.storage, rows.into_iter().map(Ok), Some(&meter)).unwrap();
    assert_eq!(same, Some(root));
    assert_eq!(requested.get(), 8192); // AlreadyExists comparison is a separate reread
    requested.set(0);
    assert!(
        storage::TreeIter::new_metered_baseline(&store.storage, None, 0, &meter)
            .unwrap()
            .next()
            .is_none()
    );
    assert_eq!(requested.get(), 0);
    assert!(
        storage::TreeIter::new_metered_baseline(&store.storage, Some(root), 4097, &meter).is_err()
    );
    assert_eq!(requested.get(), 0);
}

#[test]
fn native_baseline_checkpoint_second_reduction_pays_again_and_pins_candidate() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 726);
    commit_test_baseline(&mut store, record.id);
    let record = store.load_connection(record.id).unwrap();
    let cancel = AtomicBool::new(false);
    let live = native_verified_addition_facts(&mut store, &record, b"second", &cancel);
    let context = store
        .native_read_context(
            &live,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &cancel,
        )
        .unwrap();
    let rows = [b"file".as_slice(), b"second".as_slice()]
        .into_iter()
        .enumerate()
        .map(|(ordinal, path)| {
            Ok(Row::Baseline(format::BaselineEntry {
                connection: record.id.0,
                revision: live.revision + 1,
                ordinal: u32::try_from(ordinal).unwrap(),
                path: format::NativePath::Unix(path.to_vec()),
                state: format::State::File {
                    sha256: [7; 32],
                    bytes: 3,
                    executable: false,
                },
            }))
        });
    let root = storage::build_pages_with(
        &store.storage,
        rows,
        Some(&|count| context.charge(count as u64)),
    )
    .unwrap();
    let before = store.current.generation;
    let old_root = store.connection(record.id).unwrap().baseline_root;
    let (mut credit, mut upper) = (0, 64 * 1024 * 1024 + 1);
    while credit + 1 < upper {
        let mid = credit + (upper - credit) / 2;
        if live.reads.require_credit(mid).is_ok() {
            credit = mid;
        } else {
            upper = mid;
        }
    }
    let physical_trace = storage::physical_trace::begin();
    store.overlay_bytes = 8 * 1024 * 1024; // existing pressure seam, real checkpoint/GC follows
    store
        .append_native(
            Change::BaselineCommit {
                directory_chain: None,
                directory_ancestors: None,
                existing_ancestors: None,
                connection: record.id.0,
                expected: live.revision,
                root,
                count: 2,
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
                initial_review: false,
            },
            &context,
        )
        .unwrap();
    assert!(store.current.generation > before);
    let trace = physical_trace.finish();
    assert_eq!(trace.reductions, vec![(root, old_root), (root, old_root)]);
    assert_ne!(root, old_root);
    // Independently preserve exactly two reductions, each with candidate and
    // old-root constructor + traversal reads, including both EOF probes.
    for reduction in 0..2 {
        for root in [root.unwrap(), old_root.unwrap()] {
            let reads: Vec<_> = trace
                .reads
                .iter()
                .filter(|read| read.reduction == Some(reduction) && read.name == format::hex(&root))
                .collect();
            assert_eq!(reads.len(), 4);
            for pair in reads.chunks_exact(2) {
                let length = usize::try_from(pair[0].length.unwrap()).unwrap();
                assert!(length < 4096);
                assert_eq!(pair[1].length, Some(length as u64));
                assert_eq!((pair[0].requested, pair[0].returned), (4096, Some(length)));
                assert_eq!((pair[1].requested, pair[1].returned), (4096, Some(0)));
            }
        }
    }
    assert!(
        trace
            .reads
            .iter()
            .all(|read| !read.name.is_empty() && read.returned.is_some())
    );
    let requested: u64 = trace.reads.iter().map(|read| read.requested as u64).sum();
    let mut inventory = std::collections::BTreeMap::<String, (usize, u64)>::new();
    for read in &trace.reads {
        let name = if read.name.len() == 64 {
            "page"
        } else {
            &read.name
        };
        let entry = inventory.entry(name.to_owned()).or_default();
        entry.0 += 1;
        entry.1 += read.requested as u64;
    }
    eprintln!(
        "baseline actual_reads={} reductions=2 exact_baseline_requests=16 exact_baseline_bytes=65536 requested={requested} credit_before={credit} inventory={inventory:?}",
        trace.reads.len()
    );
    assert!(live.reads.charge(credit - requested).is_ok());
    assert!(live.reads.charge(1).is_err());
    assert_eq!(
        store
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .unwrap()
            .len(),
        2
    );
    drop(store);
    assert_eq!(
        ConnectionStore::open_at(&path)
            .unwrap()
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn native_initial_intent_requires_explicit_reviewed_preview() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 727);
    let session = store.transaction_session(record.id).unwrap();
    assert!(matches!(
        store.begin_native_without_identity_for_test(intent(&record), &session),
        Err(StoreError::RecoveryRequired)
    ));
    assert!(
        store
            .pending_page(record.id, None, 8)
            .unwrap()
            .entries
            .is_empty()
    );
}

fn initial_preview_fixture(
    store: &ConnectionStore,
    record: &ConnectionRecord,
) -> super::super::transaction::InitialPreview {
    use super::super::model::{Endpoint, NamePolicy, Observation, ObservedTree};
    let endpoint = |identity| Endpoint {
        identity,
        encoding: Encoding::Unix,
        names: NamePolicy::Exact,
    };
    let operation = intent(record);
    let source = ObservedTree::new(
        endpoint(record.source.identity),
        [(operation.path.clone(), operation.source_before.unwrap())],
        Observation::Complete,
    )
    .unwrap();
    let destination = ObservedTree::new(
        endpoint(record.destination.identity),
        [],
        Observation::Complete,
    )
    .unwrap();
    super::super::transaction::initial_preview_for_test(store, record.id, source, destination)
        .unwrap()
}
#[test]
fn native_initial_review_is_exact_consumed_and_durable() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 728);
    let session = store.transaction_session(record.id).unwrap();
    let reviewed = initial_preview_fixture(&store, &record)
        .accept(intent(&record))
        .unwrap();
    let duplicate_preview = initial_preview_fixture(&store, &record)
        .accept(intent(&record))
        .unwrap();
    let live = store
        .begin_native_reviewed_without_identity_for_test(intent(&record), &session, reviewed)
        .unwrap();
    let fact = store.operation(live.id()).unwrap().initial_review.unwrap();
    assert_ne!(fact.preview, [0; 32]);
    assert_ne!(fact.selection, [0; 32]);
    assert_eq!(fact.revision, record.revision);
    assert!(matches!(
        store.begin_native_reviewed_without_identity_for_test(
            intent(&record),
            &session,
            duplicate_preview
        ),
        Err(StoreError::StaleRevision)
    ));
    store.checkpoint().unwrap();
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        reopened.operation(live.id()).unwrap().initial_review,
        Some(fact)
    );
    assert!(
        reopened
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .is_none()
    );
}
#[test]
fn native_initial_review_rejects_changed_selection_fence_and_reopen() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 729);
    let session = store.transaction_session(record.id).unwrap();
    let mut changed = intent(&record);
    changed.source_before = Some(EntryState::File {
        sha256: [8; 32],
        bytes: 3,
        executable: false,
    });
    assert!(
        initial_preview_fixture(&store, &record)
            .accept(changed.clone())
            .is_err()
    );
    let reviewed = initial_preview_fixture(&store, &record)
        .accept(intent(&record))
        .unwrap();
    assert!(
        store
            .begin_native_reviewed_without_identity_for_test(changed, &session, reviewed)
            .is_err()
    );
    let stale = initial_preview_fixture(&store, &record)
        .accept(intent(&record))
        .unwrap();
    let fenced = initial_preview_fixture(&store, &record)
        .accept(intent(&record))
        .unwrap();
    store
        .request_disconnect(record.id, record.revision)
        .unwrap();
    assert!(
        store
            .begin_native_reviewed_without_identity_for_test(intent(&record), &session, fenced)
            .is_err()
    );
    drop(store);
    let mut reopened = ConnectionStore::open_at(&path).unwrap();
    assert!(
        reopened
            .begin_native_reviewed_without_identity_for_test(intent(&record), &session, stale)
            .is_err()
    );
    assert!(
        reopened
            .pending_page(record.id, None, 8)
            .unwrap()
            .entries
            .is_empty()
    );
}

#[test]
fn native_initial_baseline_boolean_cannot_replace_review_fact() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 730);
    let cancel = AtomicBool::new(false);
    let live = native_verified_addition_facts(&mut store, &record, b"file", &cancel);
    let context = store
        .native_read_context(
            &live,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &cancel,
        )
        .unwrap();
    let row = Row::Baseline(format::BaselineEntry {
        connection: record.id.0,
        revision: live.revision + 1,
        ordinal: 0,
        path: format::NativePath::Unix(b"file".to_vec()),
        state: format::State::File {
            sha256: [7; 32],
            bytes: 3,
            executable: false,
        },
    });
    let root = storage::build_pages_with(
        &store.storage,
        std::iter::once(Ok(row)),
        Some(&|count| context.charge(count as u64)),
    )
    .unwrap();
    assert!(matches!(
        store.append_native(
            Change::BaselineCommit { directory_chain: None,
                directory_ancestors: None,
                existing_ancestors: None,
                connection: record.id.0,
                expected: live.revision,
                root,
                count: 1,
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
                initial_review: true
            },
            &context
        ),
        Err(StoreError::RecoveryRequired)
    ));
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
}

#[test]
fn native_deletion_paid_preflight_refuses_a_changed_survivor() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 731);
    commit_test_baseline(&mut store, record.id);
    let record = store.load_connection(record.id).unwrap();
    let session = store.transaction_session(record.id).unwrap();
    let mut deletion = intent(&record);
    deletion.expected_baseline = record.baseline_revision;
    deletion.source_before = Some(EntryState::File {
        sha256: [8; 32],
        bytes: 3,
        executable: false,
    });
    deletion.decision = Decision::RecoverDeletion { at: Side::Source };
    // Intent itself is metadata only; paid preflight must block all native steps.
    let admission = store.begin_native_without_identity_for_test(deletion, &session);
    if let Ok(mut live) = admission {
        let cancel = AtomicBool::new(false);
        for kind in [
            super::super::transaction::StepKind::Namespace,
            super::super::transaction::StepKind::Stage,
            super::super::transaction::StepKind::Relocate,
            super::super::transaction::StepKind::Preserve,
            super::super::transaction::StepKind::Publish,
        ] {
            assert!(
                session.admit_step(&store, &live, kind, &cancel).is_err(),
                "unchecked deletion admitted a native step"
            );
        }
        assert!(matches!(
            store.reserve_native_work(
                &mut live,
                &session,
                16 * 1024 * 1024,
                std::time::Instant::now() + std::time::Duration::from_secs(30),
                &AtomicBool::new(false)
            ),
            Err(StoreError::RecoveryRequired)
        ));
        for kind in [
            super::super::transaction::StepKind::Namespace,
            super::super::transaction::StepKind::Stage,
            super::super::transaction::StepKind::Relocate,
            super::super::transaction::StepKind::Preserve,
            super::super::transaction::StepKind::Publish,
        ] {
            assert!(session.admit_step(&store, &live, kind, &cancel).is_err());
        }
    } else {
        panic!("baseline payload was consulted before the canonical read reservation");
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn reconstructed_outcome_accepts_historical_progress_only_through_explicit_recovery_event() {
    use super::super::transaction::native::descriptor as d;
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 720);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let reservation = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [7; 16])
        .unwrap();
    let mut registration = native_prepared_facts(&live, &reservation);
    store
        .append(Change::NativePrepared {
            registration: registration.clone(),
            expected: live.revision,
        })
        .unwrap();
    let current = store.resume_native_for_test(live.id(), &session).unwrap();
    let cancel = AtomicBool::new(false);
    let intent = d::decode(registration.intent.as_ref().unwrap()).unwrap();
    let stage = d::decode(registration.stage.as_ref().unwrap()).unwrap();
    let d::Body::Intent(intent_body) = &intent.body else {
        panic!("intent");
    };
    let d::Body::Stage(stage_body) = &stage.body else {
        panic!("stage");
    };
    let d::State::Regular { stamp, .. } = &intent_body.expected[0] else {
        panic!("source");
    };
    let mut header = intent.header.clone();
    header.progress = current.revision;
    let outcome = d::Body::Outcome(d::Outcome {
        intent: intent.digest,
        stage: Some(stage.digest),
        displaced: None,
        states: [
            intent_body.expected[0].clone(),
            d::State::Regular {
                object: stage_body.object.clone(),
                stamp: stamp.clone(),
                content: stage_body.content.clone(),
            },
        ],
        kind: 0,
        barriers: 3,
        recovery: Vec::new(),
        sequence: 1,
    });
    registration.outcome = Some(d::encode(&header, &outcome).unwrap());
    let id = live.id();
    store.checkpoint().unwrap();
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let current = store.load_connection(id.connection).unwrap();
    let mut recovery = store.open_recovery_context(id, current.revision).unwrap();
    store
        .reserve_recovery_reads(
            &mut recovery,
            1,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &cancel,
        )
        .unwrap();
    let session = store.transaction_session(id.connection).unwrap();
    let live = store.resume_native_for_test(id, &session).unwrap();
    let context = store
        .native_read_context(
            &live,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &cancel,
        )
        .unwrap();
    assert!(
        store
            .append_native(
                Change::NativeOutcome {
                    registration: registration.clone(),
                    expected: live.revision,
                    verified: false,
                    reconstructed: false
                },
                &context
            )
            .is_err()
    );
    let mut future = registration.clone();
    let mut future_header = header;
    future_header.progress = live.revision + 1;
    future.outcome = Some(d::encode(&future_header, &outcome).unwrap());
    assert!(
        store
            .append_native(
                Change::NativeOutcome {
                    registration: future,
                    expected: live.revision,
                    verified: false,
                    reconstructed: true
                },
                &context
            )
            .is_err()
    );
    store
        .append_native(
            Change::NativeOutcome {
                registration,
                expected: live.revision,
                verified: false,
                reconstructed: true,
            },
            &context,
        )
        .unwrap();
    assert_eq!(
        store.operation(id).unwrap().decode().unwrap().stage,
        ReceiptStage::Published
    );
    assert!(
        store
            .load_baseline(id.connection)
            .unwrap()
            .entries()
            .is_none()
    );
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        store.operation(id).unwrap().decode().unwrap().stage,
        ReceiptStage::Published
    );
}

#[test]
fn native_equal_operation_cannot_admit_namespace_mutation() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 732);
    let session = store.transaction_session(record.id).unwrap();
    let mut intent = intent(&record);
    intent.destination_before = intent.source_before.clone();
    intent.decision = Decision::VerifyEqual;
    let live = store.begin_native_for_test(intent, &session).unwrap();
    let cancel = AtomicBool::new(false);
    for kind in [
        super::super::transaction::StepKind::Namespace,
        super::super::transaction::StepKind::Stage,
        super::super::transaction::StepKind::Relocate,
        super::super::transaction::StepKind::Preserve,
        super::super::transaction::StepKind::Publish,
    ] {
        assert!(
            session.admit_step(&store, &live, kind, &cancel).is_err(),
            "equality admitted namespace mutation"
        );
    }
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
    assert_eq!(
        store.pending_page(record.id, None, 8).unwrap().entries[0].stage,
        ReceiptStage::Intent
    );
}

fn native_admission_facts(
    operation: &format::Operation,
    fence: u64,
) -> super::super::transaction::native::descriptor::Admission {
    use super::super::transaction::native::descriptor as d;
    let object = |kind, side: u64| d::Object {
        dev: 1,
        ino: 20 + side,
        kind,
        generation: [7; 32],
    };
    let state = |value: &Option<format::State>, side| match value {
        Some(format::State::File {
            sha256,
            bytes,
            executable,
        }) => d::State::Regular {
            object: object(0o100_000, side),
            stamp: d::Stamp {
                length: *bytes,
                modified: (1, 0),
                changed: (1, 0),
                mode: if *executable { 0o100_700 } else { 0o100_600 },
                uid: 0,
                gid: 0,
                links: 1,
            },
            content: d::Content {
                digest: *sha256,
                length: *bytes,
                executable: *executable,
            },
        },
        None => d::State::Absent,
        _ => panic!("regular metadata fixture"),
    };
    let endpoint = |value: &storage::Identity| {
        let mut bytes = [0; 64];
        bytes[..32].copy_from_slice(&value.volume);
        bytes[32..].copy_from_slice(&value.root);
        bytes
    };
    d::Admission {
        connection: operation.connection,
        operation: operation.serial,
        revision: operation.expected,
        fence,
        baseline: operation.baseline,
        endpoints: [
            endpoint(&operation.source_binding),
            endpoint(&operation.destination_binding),
        ],
        direction: u8::from(matches!(operation.decision, 2 | 3)),
        decision: operation.decision,
        path: operation.path.to_relative().unwrap(),
        chains: [vec![object(0o040_000, 0)], vec![object(0o040_000, 1)]],
        states: [
            state(&operation.source, 0),
            state(&operation.destination, 1),
        ],
    }
}
#[test]
fn native_admission_identity_is_atomic_exact_and_retained_on_reopen() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 733);
    let connection = store.connection(record.id).unwrap();
    let mut operation =
        format::Operation::new(intent(&record), connection.serial + 1, &connection).unwrap();
    operation.native_budget = Some(format::NativeBudget::initial());
    let fact = native_admission_facts(&operation, connection.fence);
    for field in 0..5 {
        let mut wrong = fact.clone();
        match field {
            0 => wrong.operation += 1,
            1 => wrong.revision += 1,
            2 => wrong.fence += 1,
            3 => wrong.endpoints[0][0] ^= 1,
            _ => wrong.path = RelativePath::unix(b"other").unwrap(),
        }
        let mut bad = operation.clone();
        bad.native_identity = Some(wrong.encode().unwrap());
        assert!(
            store.append(Change::Intent { operation: bad }).is_err(),
            "unjoined native admission metadata accepted"
        );
        assert!(
            store
                .pending_page(record.id, None, 8)
                .unwrap()
                .entries
                .is_empty()
        );
    }
    operation.native_identity = Some(fact.encode().unwrap());
    let id = operation.decode().unwrap().id;
    store
        .append(Change::Intent {
            operation: operation.clone(),
        })
        .unwrap();
    assert_eq!(
        store.operation(id).unwrap().native_identity,
        operation.native_identity
    );
    store.checkpoint().unwrap();
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        store.operation(id).unwrap().native_identity,
        operation.native_identity
    );
    let record = create(&mut store, 734);
    let session = store.transaction_session(record.id).unwrap();
    let legacy = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    assert!(
        store
            .operation(legacy.id())
            .unwrap()
            .native_identity
            .is_none()
    );
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert!(
        store
            .operation(legacy.id())
            .unwrap()
            .native_identity
            .is_none()
    );
}

#[test]
fn native_never_begun_boundary_accounts_pending_without_verification_or_baseline() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 735);
    let connection = store.connection(record.id).unwrap();
    let mut operation =
        format::Operation::new(intent(&record), connection.serial + 1, &connection).unwrap();
    operation.native_budget = Some(format::NativeBudget::initial());
    operation.native_identity = Some(
        native_admission_facts(&operation, connection.fence)
            .encode()
            .unwrap(),
    );
    let id = operation.decode().unwrap().id;
    store.append(Change::Intent { operation }).unwrap();
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store.resume_native_for_test(id, &session).unwrap();
    store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [8; 16])
        .unwrap();
    let fence = store.request_disconnect(record.id, live.revision).unwrap();
    assert!(store.finish_disconnect(&fence).is_err());
    let current = store.connection(record.id).unwrap();
    let operation = store.operation(id).unwrap();
    let event = Change::NativeBoundary {
        connection: record.id.0,
        serial: id.serial,
        expected: current.revision,
        operation_expected: operation.revision,
        fence: current.fence,
        kind: 0,
        evidence: None,
    };
    for field in 0..5 {
        let mut bad = event.clone();
        let Change::NativeBoundary {
            serial,
            expected,
            operation_expected,
            fence: stale_fence,
            kind,
            ..
        } = &mut bad
        else {
            unreachable!()
        };
        match field {
            0 => *serial += 1,
            1 => *expected += 1,
            2 => *operation_expected += 1,
            3 => *stale_fence += 1,
            _ => *kind = 4,
        }
        assert!(store.append(bad).is_err());
        assert_eq!(
            store.connection(record.id).unwrap().pending,
            Some(id.serial)
        );
        assert!(store.finish_disconnect(&fence).is_err());
    }
    store
        .append(event.clone())
        .expect("fresh native NeverBegun facts must durably account stopped pending work");
    assert!(
        store.append(event).is_err(),
        "accounting replay must not update progress twice"
    );
    assert!(store.connection(record.id).unwrap().pending.is_none());
    assert_eq!(store.operation(id).unwrap().stage, 7);
    assert_eq!(
        store.operation(id).unwrap().native_budget,
        operation.native_budget
    );
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
    assert_eq!(
        store
            .table_page(record.id, 5, None, 8)
            .unwrap()
            .entries
            .len(),
        1
    );
    assert!(session.require_idle().is_err());
    store.finish_disconnect(&fence).unwrap();
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        store.load_connection(record.id).unwrap().lifecycle,
        Lifecycle::Disconnected
    );
    assert_eq!(store.operation(id).unwrap().stage, 7);
    assert_eq!(
        store
            .table_page(record.id, 5, None, 8)
            .unwrap()
            .entries
            .len(),
        1
    );
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
}

#[test]
fn native_never_begun_refuses_missing_history_and_prepared_work() {
    for prepared in [false, true] {
        let (_temp, path) = fixture();
        let mut store = ConnectionStore::open_at(&path).unwrap();
        let record = create(&mut store, 736);
        let session = store.transaction_session(record.id).unwrap();
        let mut live = if prepared {
            let connection = store.connection(record.id).unwrap();
            let mut operation =
                format::Operation::new(intent(&record), connection.serial + 1, &connection)
                    .unwrap();
            operation.native_budget = Some(format::NativeBudget::initial());
            operation.native_identity = Some(
                native_admission_facts(&operation, connection.fence)
                    .encode()
                    .unwrap(),
            );
            let id = operation.decode().unwrap().id;
            store.append(Change::Intent { operation }).unwrap();
            store.resume_native_for_test(id, &session).unwrap()
        } else {
            store
                .begin_native_for_test(intent(&record), &session)
                .unwrap()
        };
        if prepared {
            let reservation = store
                .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [8; 16])
                .unwrap();
            let registration = native_prepared_facts(&live, &reservation);
            store
                .append(Change::NativePrepared {
                    registration,
                    expected: live.revision,
                })
                .unwrap();
        }
        let current = store.connection(record.id).unwrap();
        let fence = store
            .request_disconnect(record.id, current.revision)
            .unwrap();
        let current = store.connection(record.id).unwrap();
        let operation = store.operation(live.id()).unwrap();
        assert_eq!(operation.native_identity.is_some(), prepared);
        assert_eq!(operation.stage, if prepared { 6 } else { 0 });
        assert!(
            store
                .append(Change::NativeBoundary {
                    connection: record.id.0,
                    serial: live.id().serial,
                    expected: current.revision,
                    operation_expected: operation.revision,
                    fence: current.fence,
                    kind: 0,
                    evidence: None
                })
                .is_err()
        );
        assert!(store.finish_disconnect(&fence).is_err());
        store.checkpoint().unwrap();
        drop(store);
        let store = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(
            store.load_connection(record.id).unwrap().lifecycle,
            Lifecycle::Disconnecting
        );
        assert_eq!(store.operation(live.id()).unwrap(), operation);
        assert!(store.load_baseline(record.id).unwrap().entries().is_none());
    }
}

#[test]
fn native_staged_boundary_retains_prepared_namespace_across_disconnect_and_reopen() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 737);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let reservation = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [9; 16])
        .unwrap();
    let registration = native_prepared_facts(&live, &reservation);
    store
        .append(Change::NativePrepared {
            registration: registration.clone(),
            expected: live.revision,
        })
        .unwrap();
    let current = store.connection(record.id).unwrap();
    let fence = store
        .request_disconnect(record.id, current.revision)
        .unwrap();
    let current = store.connection(record.id).unwrap();
    let operation = store.operation(live.id()).unwrap();
    store
        .append(Change::NativeBoundary {
            connection: record.id.0,
            serial: live.id().serial,
            expected: current.revision,
            operation_expected: operation.revision,
            fence: current.fence,
            kind: 1,
            evidence: Some(format::BoundaryEvidence {
                attempt: 1,
                displaced: None,
                outcome: None,
            }),
        })
        .expect("actual staged-only accounting retains prepared work");
    assert_eq!(store.operation(live.id()).unwrap().stage, 7);
    assert_eq!(
        store.row(&registration.key()).unwrap(),
        Some(Row::NativeRegistration(registration.clone()))
    );
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
    store.finish_disconnect(&fence).unwrap();
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        store.pending_page(record.id, None, 8).unwrap().entries[0].stage,
        ReceiptStage::AccountedPending
    );
    assert_eq!(
        store.row(&registration.key()).unwrap(),
        Some(Row::NativeRegistration(registration))
    );
    assert_eq!(
        store.load_connection(record.id).unwrap().lifecycle,
        Lifecycle::Disconnected
    );
}

#[test]
fn native_displaced_and_published_boundaries_pin_unverified_quarantine_without_baseline() {
    for kind in [2, 3] {
        let (_temp, path) = fixture();
        let mut store = ConnectionStore::open_at(&path).unwrap();
        let record = create(&mut store, 738);
        let session = store.transaction_session(record.id).unwrap();
        let mut proposal = intent(&record);
        proposal.destination_before = proposal.source_before.clone();
        let mut live = store.begin_native_for_test(proposal, &session).unwrap();
        let reservation = store
            .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [10; 16])
            .unwrap();
        let (registration, displaced) = native_replacement_facts(&live, &reservation);
        store
            .append(Change::NativePrepared {
                registration: registration.clone(),
                expected: live.revision,
            })
            .unwrap();
        let current = store.connection(record.id).unwrap();
        let fence = store
            .request_disconnect(record.id, current.revision)
            .unwrap();
        let current = store.connection(record.id).unwrap();
        let operation = store.operation(live.id()).unwrap();
        let evidence = format::BoundaryEvidence {
            attempt: 1,
            displaced: Some(displaced),
            outcome: None,
        };
        store
            .append(Change::NativeBoundary {
                connection: record.id.0,
                serial: live.id().serial,
                expected: current.revision,
                operation_expected: operation.revision,
                fence: current.fence,
                kind,
                evidence: Some(evidence.clone()),
            })
            .expect("durable actual quarantine is an accounted boundary, not verified recovery");
        assert!(
            store
                .recovery_page(record.id, None, 8)
                .unwrap()
                .entries
                .is_empty()
        );
        assert!(store.load_baseline(record.id).unwrap().entries().is_none());
        assert_eq!(
            store.operation(live.id()).unwrap().native_boundary_evidence,
            Some(evidence.clone())
        );
        store.finish_disconnect(&fence).unwrap();
        store.checkpoint().unwrap();
        drop(store);
        let store = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(
            store.operation(live.id()).unwrap().native_boundary_evidence,
            Some(evidence)
        );
        assert_eq!(
            store.pending_page(record.id, None, 8).unwrap().entries[0].stage,
            ReceiptStage::AccountedPending
        );
        assert_eq!(
            store.row(&registration.key()).unwrap(),
            Some(Row::NativeRegistration(registration))
        );
        assert!(store.load_baseline(record.id).unwrap().entries().is_none());
    }
}

fn native_replacement_facts(
    live: &LiveOperation,
    reservation: &NamespaceReservation,
) -> (format::NativeRegistration, Vec<u8>) {
    use super::super::transaction::native::descriptor as d;
    let mut registration = native_prepared_facts(live, reservation);
    let mut original = d::decode(registration.intent.as_ref().unwrap()).unwrap();
    let d::Body::Intent(body) = &mut original.body else {
        unreachable!()
    };
    let Some(EntryState::File {
        sha256,
        bytes,
        executable,
    }) = &live.intent().source_before
    else {
        unreachable!()
    };
    if let d::State::Regular { content, stamp, .. } = &mut body.expected[0] {
        *content = d::Content {
            digest: *sha256,
            length: *bytes,
            executable: *executable,
        };
        stamp.length = *bytes;
    }
    body.expected[1] = body.expected[0].clone();
    let before = body.clone();
    registration.intent = Some(d::encode(&original.header, &original.body).unwrap());
    let original = d::decode(registration.intent.as_ref().unwrap()).unwrap();
    let mut stage = d::decode(registration.stage.as_ref().unwrap()).unwrap();
    let d::Body::Stage(body) = &mut stage.body else {
        unreachable!()
    };
    body.intent = original.digest;
    if let d::State::Regular { content, stamp, .. } = &before.expected[0] {
        body.content = content.clone();
        if let d::Source::Endpoint {
            stamp: source_stamp,
            ..
        } = &mut body.source
        {
            *source_stamp = stamp.clone();
        }
    }
    registration.stage = Some(d::encode(&stage.header, &stage.body).unwrap());
    let stage = d::decode(registration.stage.as_ref().unwrap()).unwrap();
    let d::State::Regular { object, stamp, .. } = &before.expected[1] else {
        unreachable!()
    };
    let displaced = d::encode(
        &original.header,
        &d::Body::Displaced(d::Displaced {
            intent: original.digest,
            stage: Some(stage.digest),
            object: object.clone(),
            stamp: stamp.clone(),
            matches_expected: true,
            side: 1,
            parent: before.chains[1][0].clone(),
            leaf: before.path.components()[0].clone(),
            barriers: 3,
        }),
    )
    .unwrap();
    (registration, displaced)
}

#[test]
fn native_recovery_only_drain_reserves_and_retains_partial_without_reopening_admission() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 739);
    let session = store.transaction_session(record.id).unwrap();
    let mut proposal = intent(&record);
    proposal.destination_before = proposal.source_before.clone();
    let mut live = store.begin_native_for_test(proposal, &session).unwrap();
    let reservation = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [11; 16])
        .unwrap();
    let (registration, displaced) = native_replacement_facts(&live, &reservation);
    store
        .append(Change::NativePrepared {
            registration,
            expected: live.revision,
        })
        .unwrap();
    let current = store.connection(record.id).unwrap();
    store
        .request_disconnect(record.id, current.revision)
        .unwrap();
    let current = store.connection(record.id).unwrap();
    let operation = store.operation(live.id()).unwrap();
    let entry = format::Recovery {
        captured: None,
        connection: record.id.0,
        serial: live.id().serial,
        slot: 0,
        side: 1,
        path: operation.path.clone(),
        reason: 1,
        state: 0,
        content: None,
        object: None,
        retained: 0,
    };
    let source = format::BoundaryEvidence {
        attempt: 1,
        displaced: Some(displaced),
        outcome: None,
    };
    assert!(
        store
            .append(Change::Reserve {
                entry: entry.clone(),
                expected: current.revision,
                fence: current.fence
            })
            .is_err()
    );
    store
        .append(Change::NativeDrain {
            entry: entry.clone(),
            expected: current.revision,
            operation_expected: operation.revision,
            fence: current.fence,
            source: source.clone(),
            fact: None,
        })
        .expect("retained actual pre-fence quarantine may reserve a narrow drain slot");
    assert!(session.require_idle().is_err());
    let current = store.connection(record.id).unwrap();
    let operation = store.operation(live.id()).unwrap();
    let mut incomplete = entry;
    incomplete.state = 5;
    store
        .append(Change::NativeDrain {
            entry: incomplete,
            expected: current.revision,
            operation_expected: operation.revision,
            fence: current.fence,
            source,
            fact: None,
        })
        .unwrap();
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        store.load_connection(record.id).unwrap().lifecycle,
        Lifecycle::Disconnecting
    );
    assert_eq!(
        store.recovery_page(record.id, None, 8).unwrap().entries[0].state,
        RecoveryState::RetainedIncomplete
    );
    assert_eq!(
        store.connection(record.id).unwrap().pending,
        Some(live.id().serial)
    );
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn native_recovery_only_bytes_requires_paid_private_reread_and_exact_origin_facts() {
    use super::super::transaction::native::descriptor as d;
    use sha2::Digest;
    use std::time::{Duration, Instant};
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 740);
    let session = store.transaction_session(record.id).unwrap();
    let mut proposal = intent(&record);
    proposal.source_before = Some(EntryState::File {
        sha256: sha2::Sha256::digest(b"abc").into(),
        bytes: 3,
        executable: false,
    });
    proposal.destination_before = proposal.source_before.clone();
    let mut live = store.begin_native_for_test(proposal, &session).unwrap();
    let reservation = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [12; 16])
        .unwrap();
    let (registration, displaced) = native_replacement_facts(&live, &reservation);
    store
        .append(Change::NativePrepared {
            registration: registration.clone(),
            expected: live.revision,
        })
        .unwrap();
    let current = store.connection(record.id).unwrap();
    store
        .request_disconnect(record.id, current.revision)
        .unwrap();
    let current = store.connection(record.id).unwrap();
    let mut context = store
        .open_recovery_context(live.id(), current.revision)
        .unwrap();
    let cancel = AtomicBool::new(false);
    store
        .reserve_recovery_reads(
            &mut context,
            4 * 1024 * 1024,
            Instant::now() + Duration::from_secs(30),
            &cancel,
        )
        .unwrap();
    let mut entry = format::Recovery {
        captured: None,
        connection: record.id.0,
        serial: live.id().serial,
        slot: 0,
        side: 1,
        path: live.intent().path.clone().into(),
        reason: 1,
        state: 0,
        content: None,
        object: None,
        retained: 0,
    };
    let source = format::BoundaryEvidence {
        attempt: 1,
        displaced: Some(displaced.clone()),
        outcome: None,
    };
    store
        .append(Change::NativeDrain {
            entry: entry.clone(),
            expected: context.revision,
            operation_expected: context.operation_revision,
            fence: context.fence,
            source: source.clone(),
            fact: None,
        })
        .unwrap();
    context.revision += 1;
    context.operation_revision += 1;
    let directory = store
        .storage
        .recovery
        .create_child(std::ffi::OsStr::new(&format::hex(&record.id.0)))
        .unwrap();
    let name = format!("{}-0", live.id().serial);
    let mut writer = storage::open_file(&directory, &name, true).unwrap();
    std::io::Write::write_all(&mut writer, b"abc").unwrap();
    storage::sync_file(&writer).unwrap();
    let metadata = storage::regular(&writer).unwrap();
    let object = format::ObjectIdentity {
        root: store.storage.identity.clone(),
        file_id: storage::file_identity(&metadata),
    };
    let original = d::decode(registration.intent.as_ref().unwrap()).unwrap();
    let d::Body::Intent(body) = &original.body else {
        unreachable!()
    };
    let d::State::Regular {
        object: origin,
        content,
        ..
    } = &body.expected[1]
    else {
        unreachable!()
    };
    let fact = d::RecoveryFact {
        intent: original.digest,
        recovery: d::Recovery {
            connection: record.id.0,
            operation: live.id().serial,
            slot: 0,
        },
        side: 1,
        role: 0,
        endpoint: body.endpoints[1],
        object: origin.clone(),
        content: content.clone(),
        artifact: recovery::artifact_view(&object),
    };
    entry.state = 1;
    entry.content = Some(format::State::File {
        sha256: content.digest,
        bytes: content.length,
        executable: false,
    });
    entry.object = Some(object);
    let fact = format::NativeRecoveryFact {
        captured: None,
        connection: record.id.0,
        serial: live.id().serial,
        slot: 0,
        attempt: 1,
        fact: fact.encode().unwrap(),
        displaced,
    };
    let change = Change::NativeDrain {
        entry: entry.clone(),
        expected: context.revision,
        operation_expected: context.operation_revision,
        fence: context.fence,
        source,
        fact: Some(fact.clone()),
    };
    assert!(
        store.append(change.clone()).is_err(),
        "unmetered Bytes bypass"
    );
    let reads = store
        .recovery_read_context(&context, Instant::now() + Duration::from_secs(30), &cancel)
        .unwrap();
    let mut wrong_origin = change.clone();
    let Change::NativeDrain {
        fact: Some(wrong), ..
    } = &mut wrong_origin
    else {
        unreachable!()
    };
    let mut bound = d::RecoveryFact::decode(&wrong.fact).unwrap();
    bound.object.generation[0] ^= 1;
    wrong.fact = bound.encode().unwrap();
    assert!(store.append_native(wrong_origin, &reads).is_err());
    std::io::Seek::seek(&mut writer, std::io::SeekFrom::Start(0)).unwrap();
    std::io::Write::write_all(&mut writer, b"abd").unwrap();
    storage::sync_file(&writer).unwrap();
    assert!(
        store.append_native(change.clone(), &reads).is_err(),
        "paid read must reject changed private bytes"
    );
    assert_eq!(
        std::fs::read(
            path.join("recovery")
                .join(format::hex(&record.id.0))
                .join(&name)
        )
        .unwrap(),
        b"abd"
    );
    assert_eq!(
        store.recovery_page(record.id, None, 8).unwrap().entries[0].state,
        RecoveryState::Reserved
    );
    std::io::Seek::seek(&mut writer, std::io::SeekFrom::Start(0)).unwrap();
    std::io::Write::write_all(&mut writer, b"abc").unwrap();
    storage::sync_file(&writer).unwrap();
    store
        .append_native(change, &reads)
        .expect("paid actual private bytes and joined native origin can drain after fence");
    assert_eq!(
        store.row(&fact.key()).unwrap(),
        Some(Row::NativeRecoveryFact(fact))
    );
    assert_eq!(
        store.recovery_page(record.id, None, 8).unwrap().entries[0].state,
        RecoveryState::BytesVerified
    );
    assert_eq!(
        store.load_connection(record.id).unwrap().lifecycle,
        Lifecycle::Disconnecting
    );
    assert!(session.require_idle().is_err());
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
    // Reopen the event log without checkpointing. Historical replay accepts the
    // marked fact while a subsequent live paid read must reject later drift.
    drop(store);
    std::io::Seek::seek(&mut writer, std::io::SeekFrom::Start(0)).unwrap();
    std::io::Write::write_all(&mut writer, b"bad").unwrap();
    storage::sync_file(&writer).unwrap();
    drop(writer);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        store.recovery_page(record.id, None, 8).unwrap().entries[0].state,
        RecoveryState::BytesVerified
    );
    let current = store.load_connection(record.id).unwrap();
    let mut reopened = store
        .open_recovery_context(live.id(), current.revision)
        .unwrap();
    store
        .reserve_recovery_reads(
            &mut reopened,
            4 * 1024 * 1024,
            Instant::now() + Duration::from_secs(30),
            &cancel,
        )
        .unwrap();
    let paid = store
        .recovery_read_context(&reopened, Instant::now() + Duration::from_secs(30), &cancel)
        .unwrap();
    assert!(
        store
            .verify_recovery_with_context(&entry, Some(&paid))
            .is_err()
    );
    assert_eq!(
        std::fs::read(
            path.join("recovery")
                .join(format::hex(&record.id.0))
                .join(&name)
        )
        .unwrap(),
        b"bad"
    );
    assert_eq!(
        store
            .operation(live.id())
            .unwrap()
            .native_budget
            .unwrap()
            .reserved,
        8 * 1024 * 1024
    );
    assert_eq!(
        store.load_connection(record.id).unwrap().lifecycle,
        Lifecycle::Disconnecting
    );
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
    store.checkpoint().unwrap();
}

// A serialized selection is historical intent, never ordinary propagation authority.
#[test]
fn native_selected_action_roundtrips_without_rewriting_observed_decision() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 911);
    let connection = store.connection(record.id).unwrap();
    let mut operation = format::Operation::new(intent(&record), 2, &connection).unwrap();
    operation.native_budget = Some(format::NativeBudget::initial());
    let mut wire = serde_json::to_value(&operation).unwrap();
    let action = serde_json::json!({
        "kind": "restore", "version": 1, "target": 1, "replace": false,
        "revision": operation.expected, "fence": connection.fence,
        "selected": {"kind": "recovery", "operation": 1, "slot": 0,
            "content": null},
        "conflict": null
    });
    // Use the existing State wire shape, without assuming its serde representation.
    let mut action = action;
    action["selected"]["content"] =
        serde_json::to_value(operation.source.clone().unwrap()).unwrap();
    wire["action"] = action.clone();
    let decoded = serde_json::from_value::<format::Operation>(wire.clone());
    assert!(
        decoded.is_ok(),
        "new native selected-action history must be understood: {decoded:?}"
    );
    let decoded = decoded.unwrap();
    assert_eq!(
        decoded.decode().unwrap().intent.decision,
        Decision::Propagate { from: Side::Source }
    );
    assert_eq!(serde_json::to_value(&decoded).unwrap()["action"], action);
    let mut initial = decoded;
    initial.decision = 5;
    assert_eq!(
        initial
            .decode()
            .expect("selected restore must retain a truthful initial-preview observation")
            .intent
            .decision,
        Decision::InitialPreview
    );
    initial.action = None;
    assert!(
        initial.decode().is_err(),
        "initial-preview history cannot become ordinary execution"
    );
    for (field, value) in [
        ("version", serde_json::json!(2)),
        ("target", serde_json::json!(2)),
        ("revision", serde_json::json!(operation.expected + 1)),
        ("kind", serde_json::json!("unknown")),
    ] {
        let mut bad = wire.clone();
        bad["action"][field] = value;
        assert!(
            serde_json::from_value::<format::Operation>(bad)
                .map_or(true, |row| row.decode().is_err()),
            "accepted invalid {field}"
        );
    }
    wire.as_object_mut().unwrap().remove("action");
    assert!(
        serde_json::from_value::<format::Operation>(wire)
            .unwrap()
            .decode()
            .is_ok(),
        "legacy absence remains ordinary history"
    );
}

#[test]
fn native_selected_action_requires_an_exact_retained_version_at_atomic_intent() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 912);
    commit_test_baseline(&mut store, record.id);
    let current = store.load_connection(record.id).unwrap();
    let connection = store.connection(record.id).unwrap();
    let mut proposal = intent(&current);
    proposal.expected_baseline = current.baseline_revision;
    let mut operation =
        format::Operation::new(proposal, connection.serial + 1, &connection).unwrap();
    operation.native_budget = Some(format::NativeBudget::initial());
    operation.action = Some(format::SelectedAction::Restore {
        version: 1,
        target: 1,
        replace: false,
        revision: current.revision,
        fence: current.fence_generation,
        selected: format::SelectedVersion::Recovery {
            operation: 1,
            slot: 0,
            content: operation.source.clone().unwrap(),
        },
        conflict: None,
    });
    let before = std::fs::read(log_path(&path)).unwrap();
    assert!(
        store.append(Change::Intent { operation }).is_err(),
        "a selected version must name an actual retained private row"
    );
    assert_eq!(std::fs::read(log_path(&path)).unwrap(), before);
    assert_eq!(store.load_connection(record.id).unwrap(), current);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn native_selected_action_retains_exact_history_and_rejects_ordinary_resume_after_reopen() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 913);
    let mut prior = intent(&record);
    prior.destination_before = prior.source_before.clone();
    let ack = store.begin_intent(prior).unwrap();
    let reservation = store
        .reserve_recovery(
            &ack,
            Side::Destination,
            RelativePath::unix(b"file").unwrap(),
            RecoveryReason::DisplacedVersion,
        )
        .unwrap();
    let original = path.parent().unwrap().join("selected-original");
    std::fs::write(&original, b"old").unwrap();
    let preserved = store
        .preserve_file(
            reservation,
            &mut File::open(&original).unwrap(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let selected = store
        .recovery_page(record.id, None, 8)
        .unwrap()
        .entries
        .remove(0);
    assert_eq!(selected.id, preserved.value.id);
    for verified in [false, true] {
        let expected = store.load_connection(record.id).unwrap().revision;
        let change = if verified {
            Change::Verified {
                connection: record.id.0,
                serial: ack.value.serial,
                expected,
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
            }
        } else {
            Change::Published {
                connection: record.id.0,
                serial: ack.value.serial,
                expected,
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
            }
        };
        store.append(change).unwrap();
    }
    let expected = store.load_connection(record.id).unwrap().revision;
    let root = storage::build_pages(
        &store.storage,
        std::iter::once(Ok(Row::Baseline(format::BaselineEntry {
            connection: record.id.0,
            revision: expected + 1,
            ordinal: 0,
            path: format::NativePath::Unix(b"file".to_vec()),
            state: intent(&record).source_before.unwrap().into(),
        }))),
    )
    .unwrap();
    store
        .append(Change::BaselineCommit { directory_chain: None,
            directory_ancestors: None,
            existing_ancestors: None,
            connection: record.id.0,
            expected,
            root,
            count: 1,
            source: record.source.identity.into(),
            destination: record.destination.identity.into(),
            initial_review: true,
        })
        .unwrap();
    let current = store.load_connection(record.id).unwrap();
    let connection = store.connection(record.id).unwrap();
    let mut proposal = intent(&current);
    proposal.expected_baseline = current.baseline_revision;
    // Keep an actual equal observation. It must not become VerifyEqual execution authority.
    proposal.destination_before = proposal.source_before.clone();
    proposal.decision = Decision::VerifyEqual;
    let mut operation =
        format::Operation::new(proposal, connection.serial + 1, &connection).unwrap();
    operation.native_budget = Some(format::NativeBudget::initial());
    operation.action = Some(format::SelectedAction::Restore {
        version: 1,
        target: 1,
        replace: true,
        revision: current.revision,
        fence: current.fence_generation,
        selected: format::SelectedVersion::Recovery {
            operation: selected.id.operation.serial,
            slot: selected.id.slot,
            content: selected.content.unwrap().into(),
        },
        conflict: None,
    });
    for field in 0..6 {
        let mut bad = operation.clone();
        let format::SelectedAction::Restore {
            fence,
            selected,
            conflict,
            ..
        } = bad.action.as_mut().unwrap();
        let format::SelectedVersion::Recovery { slot, content, .. } = selected else {
            panic!("recovery fixture")
        };
        match field {
            0 => *slot = 1,
            1 => {
                *content = format::State::File {
                    sha256: [9; 32],
                    bytes: 3,
                    executable: false,
                }
            }
            2 => *fence += 1,
            3 => bad.path = format::NativePath::Unix(b"other".to_vec()),
            4 => {
                *conflict = Some(format::SelectedConflict {
                    serial: 1,
                    revision: 1,
                });
            }
            _ => bad.source_binding.root = [8; 32],
        }
        assert!(
            store.reduce_intent(&bad).is_err(),
            "accepted mismatched selection {field}"
        );
    }
    let operation_id = OperationId {
        connection: record.id,
        serial: operation.serial,
    };
    store
        .append(Change::Intent {
            operation: operation.clone(),
        })
        .unwrap();
    assert!(matches!(
        operation.decode().unwrap().action,
        ExecutionAction::Selected(_)
    ));
    let current = store.load_connection(record.id).unwrap();
    assert!(
        store
            .reduce_outcome(
                record.id.0,
                operation_id.serial,
                current.revision,
                &record.source.identity.into(),
                &record.destination.identity.into(),
                true,
                &mut RecoveryChecks::default()
            )
            .is_err(),
        "observed equality cannot dispatch selected restore even in the historical reducer"
    );
    store.checkpoint().unwrap();
    drop(preserved);
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(store.operation(operation_id).unwrap(), operation);
    let current = store.load_connection(record.id).unwrap();
    let context = store
        .open_recovery_context(operation_id, current.revision)
        .unwrap();
    assert!(!context.action().is_reconciliation());
    let session = store.transaction_session(record.id).unwrap();
    assert!(
        store
            .resume_native_for_test(operation_id, &session)
            .is_err(),
        "ordinary reacquisition must not dispatch selected restore"
    );
    assert_eq!(
        std::fs::read(
            path.join("recovery")
                .join(format::hex(&record.id.0))
                .join("1-0")
        )
        .unwrap(),
        b"old"
    );
    assert_eq!(std::fs::read(original).unwrap(), b"old");
}

#[test]
fn native_conflict_snapshot_pins_additional_versions_on_an_originally_absent_side() {
    use super::super::transaction::native::descriptor as d;
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 914);
    let session = store.transaction_session(record.id).unwrap();
    let live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let operation = store.operation(live.id()).unwrap();
    let metadata = native_admission_facts(&operation, live.fence());
    let mut competitor = metadata.states[0].clone();
    if let d::State::Regular {
        object, content, ..
    } = &mut competitor
    {
        object.generation = [8; 32];
        content.digest = [9; 32];
    }
    let snapshot = d::ConflictSnapshot {
        connection: record.id.0,
        operation: live.id().serial,
        admission: live.admission(),
        revision: live.revision(),
        fence: live.fence(),
        baseline: operation.baseline,
        endpoints: metadata.endpoints,
        path: metadata.path,
        chains: metadata.chains,
        versions: vec![
            d::CapturedVersion {
                side: 0,
                role: 0,
                state: metadata.states[0].clone(),
                displaced: None,
            },
            d::CapturedVersion {
                side: 1,
                role: 0,
                state: competitor,
                displaced: None,
            },
        ],
    };
    let bytes = snapshot.encode().unwrap();
    store
        .append(Change::NativeCapture {
            snapshot: bytes.clone(),
            displaced: Vec::new(),
            expected: live.revision(),
            kind: 0,
        })
        .expect(
            "actual conflict capture must pin a new occupant even if its planned side was absent",
        );
    let operation = store.operation(live.id()).unwrap();
    assert_eq!(
        operation.decode().unwrap().intent.destination_before,
        None,
        "original observation remains truthful history"
    );
    let Some(Row::Conflict(conflict)) = store
        .row(&format::key(3, record.id.0, live.id().serial))
        .unwrap()
    else {
        panic!("conflict");
    };
    assert_eq!(
        conflict.destination, None,
        "original planned absence remains distinct from the captured competitor"
    );
    assert_eq!(conflict.decode().unwrap().captured.unwrap(), snapshot);
    assert_eq!(operation.native_capture.as_deref(), Some(bytes.as_slice()));
    assert!(!conflict.resolved);
    assert!(conflict.preserved.is_empty());
    assert_eq!(
        store.connection(record.id).unwrap().pending,
        Some(live.id().serial),
        "unpreserved capture cannot clear pending"
    );
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(store.operation(live.id()).unwrap(), operation);
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn native_conflict_quarantine_clue_survives_checkpoint_before_private_copy() {
    use super::super::transaction::native::descriptor as d;
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 915);
    let session = store.transaction_session(record.id).unwrap();
    let mut proposal = intent(&record);
    proposal.destination_before = proposal.source_before.clone();
    let mut live = store.begin_native_for_test(proposal, &session).unwrap();
    let reservation = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [6; 16])
        .unwrap();
    let (registration, displaced) = native_replacement_facts(&live, &reservation);
    store
        .append(Change::NativePrepared {
            registration: registration.clone(),
            expected: live.revision,
        })
        .unwrap();
    live.revision += 1;
    let operation = store.operation(live.id()).unwrap();
    let metadata = native_admission_facts(&operation, live.fence());
    let clue = d::decode(&displaced).unwrap();
    let d::Body::Intent(prepared) = d::decode(registration.intent.as_ref().unwrap())
        .unwrap()
        .body
    else {
        panic!("intent");
    };
    let d::Body::Displaced(quarantine) = &clue.body else {
        panic!("displaced");
    };
    let d::State::Regular { content, .. } = &prepared.expected[1] else {
        panic!("regular");
    };
    let mut snapshot = d::ConflictSnapshot {
        connection: record.id.0,
        operation: live.id().serial,
        admission: live.admission(),
        revision: live.revision(),
        fence: live.fence(),
        baseline: operation.baseline,
        endpoints: metadata.endpoints,
        path: metadata.path,
        chains: metadata.chains,
        versions: vec![
            d::CapturedVersion {
                side: 0,
                role: 0,
                state: metadata.states[0].clone(),
                displaced: None,
            },
            d::CapturedVersion {
                side: 1,
                role: 0,
                state: metadata.states[1].clone(),
                displaced: None,
            },
            d::CapturedVersion {
                side: 1,
                role: 1,
                state: d::State::Regular {
                    object: quarantine.object.clone(),
                    stamp: quarantine.stamp.clone(),
                    content: content.clone(),
                },
                displaced: Some(clue.digest),
            },
        ],
    };
    let d::Body::Stage(staged) = d::decode(registration.stage.as_ref().unwrap())
        .unwrap()
        .body
    else {
        panic!("stage");
    };
    snapshot.versions.push(d::CapturedVersion {
        side: 1,
        role: 2,
        state: d::State::Regular {
            object: staged.object.clone(),
            stamp: quarantine.stamp.clone(),
            content: staged.content,
        },
        displaced: None,
    });
    let mut wrong_stage = snapshot.clone();
    if let d::State::Regular { object, .. } = &mut wrong_stage.versions[3].state {
        object.generation = [77; 32];
    }
    assert!(
        store
            .reduce_native_capture(
                &wrong_stage.encode().unwrap(),
                &[displaced.clone()],
                live.revision(),
                0
            )
            .is_err(),
        "an additional stage version must join the exact registered stage object"
    );
    let bytes = snapshot.encode().unwrap();
    let before = std::fs::read(log_path(&path)).unwrap();
    assert!(
        store
            .append(Change::NativeCapture {
                snapshot: bytes.clone(),
                displaced: Vec::new(),
                expected: live.revision(),
                kind: 0
            })
            .is_err()
    );
    assert_eq!(std::fs::read(log_path(&path)).unwrap(), before);
    store
        .append(Change::NativeCapture {
            snapshot: bytes,
            displaced: vec![displaced.clone()],
            expected: live.revision(),
            kind: 0,
        })
        .unwrap();
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    let row = serde_json::to_value(store.operation(live.id()).unwrap()).unwrap();
    assert_eq!(
        row["native_capture_displaced"],
        serde_json::json!([displaced]),
        "capture must retain exact pre-copy quarantine clue across compaction"
    );
    assert!(
        store
            .recovery_page(record.id, None, 8)
            .unwrap()
            .entries
            .is_empty()
    );
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn native_conflict_capture_requires_exact_version_fact_not_any_bytes_on_the_side() {
    use super::super::transaction::native::descriptor as d;
    use sha2::{Digest, Sha256};
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 916);
    let session = store.transaction_session(record.id).unwrap();
    let mut proposal = intent(&record);
    proposal.source_before = Some(EntryState::File {
        sha256: Sha256::digest(b"abc").into(),
        bytes: 3,
        executable: false,
    });
    let mut live = store.begin_native_for_test(proposal, &session).unwrap();
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    store
        .reserve_native_reads(&mut live, &session, 16 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    let operation = store.operation(live.id()).unwrap();
    let metadata = native_admission_facts(&operation, live.fence());
    let mut competitor = metadata.states[0].clone();
    if let d::State::Regular { object, .. } = &mut competitor {
        object.generation = [18; 32];
    }
    let snapshot = d::ConflictSnapshot {
        connection: record.id.0,
        operation: live.id().serial,
        admission: live.admission(),
        revision: live.revision(),
        fence: live.fence(),
        baseline: operation.baseline,
        endpoints: metadata.endpoints,
        path: metadata.path,
        chains: metadata.chains,
        versions: vec![
            d::CapturedVersion {
                side: 0,
                role: 0,
                state: metadata.states[0].clone(),
                displaced: None,
            },
            d::CapturedVersion {
                side: 1,
                role: 0,
                state: competitor,
                displaced: None,
            },
        ],
    };
    let snapshot_bytes = snapshot.encode().unwrap();
    store
        .append(Change::NativeCapture {
            snapshot: snapshot_bytes.clone(),
            displaced: Vec::new(),
            expected: live.revision(),
            kind: 0,
        })
        .unwrap();
    live.revision += 1;
    for kind in [
        super::super::transaction::StepKind::Namespace,
        super::super::transaction::StepKind::Stage,
        super::super::transaction::StepKind::Relocate,
        super::super::transaction::StepKind::Preserve,
        super::super::transaction::StepKind::Publish,
    ] {
        assert!(
            session.admit_step(&store, &live, kind, &cancel).is_err(),
            "captured conflict cannot retain ordinary step admission"
        );
    }
    let recovery_context = store
        .open_recovery_context(live.id(), live.revision())
        .unwrap();
    assert!(matches!(
        recovery_context.action(),
        ExecutionAction::Captured
    ));
    assert_eq!(
        store.native_capture_snapshot(&recovery_context).unwrap(),
        snapshot
    );
    assert!(
        store.resume_native_for_test(live.id(), &session).is_err(),
        "ordinary ID/session reacquisition cannot resume captured work"
    );
    // The raw historical event above deliberately leaves the old token stale.
    // The production owning capture consumer performs this canonical action update.
    live.action = ExecutionAction::Captured;
    for kind in [
        super::super::transaction::StepKind::Namespace,
        super::super::transaction::StepKind::Stage,
        super::super::transaction::StepKind::Relocate,
        super::super::transaction::StepKind::Preserve,
        super::super::transaction::StepKind::Publish,
    ] {
        assert!(session.admit_step(&store, &live, kind, &cancel).is_err());
    }
    let reservation = store.reserve_capture_slot(&mut live, &snapshot, 1).expect(
        "a captured-version reservation must bind its exact origin before any bytes are copied",
    );
    let Some(Row::Recovery(reserved)) = store.row(&recovery::recovery_key(reservation.id)).unwrap()
    else {
        panic!("reservation");
    };
    assert_eq!(
        reserved.captured,
        Some(format::CapturedSlot {
            snapshot: snapshot.encode().unwrap(),
            version: 1
        })
    );
    assert_eq!(reserved.state, 0);
    let mut wrong_origin = reserved;
    wrong_origin.captured.as_mut().unwrap().version = 0;
    assert!(
        wrong_origin.decode().is_err(),
        "different-side origin cannot relabel the reservation"
    );
    let original = path.parent().unwrap().join("competitor-bytes");
    std::fs::write(&original, b"abc").unwrap();
    let context = store.native_read_context(&live, deadline, &cancel).unwrap();
    let preserved = store
        .preserve_file_with_context(
            reservation,
            &mut File::open(&original).unwrap(),
            &cancel,
            Some(&context),
        )
        .unwrap();
    live.revision += 1;
    assert_eq!(
        store.operation(live.id()).unwrap().stage,
        0,
        "same-side Bytes without native object binding cannot satisfy capture"
    );
    assert_eq!(
        store.connection(record.id).unwrap().pending,
        Some(live.id().serial)
    );
    let Some(Row::Recovery(entry)) = store
        .row(&recovery::recovery_key(preserved.value.id))
        .unwrap()
    else {
        panic!("private row");
    };
    let d::State::Regular {
        object, content, ..
    } = &snapshot.versions[1].state
    else {
        panic!("captured file");
    };
    let exact = d::RecoveryFact {
        intent: snapshot.binding_digest().unwrap(),
        recovery: d::Recovery {
            connection: record.id.0,
            operation: live.id().serial,
            slot: entry.slot,
        },
        side: 1,
        role: 1,
        endpoint: snapshot.endpoints[1],
        object: object.clone(),
        content: content.clone(),
        artifact: recovery::artifact_view(entry.object.as_ref().unwrap()),
    };
    let bound = format::NativeRecoveryFact {
        captured: Some(snapshot_bytes),
        connection: record.id.0,
        serial: live.id().serial,
        slot: entry.slot,
        attempt: 0,
        fact: exact.encode().unwrap(),
        displaced: Vec::new(),
    };
    let mut wrong = exact;
    wrong.object.generation = [33; 32];
    let bad = format::NativeRecoveryFact {
        fact: wrong.encode().unwrap(),
        ..bound.clone()
    };
    assert!(
        store
            .append_native(
                Change::NativeBound {
                    fact: bad,
                    expected: live.revision(),
                    fence: live.fence()
                },
                &context
            )
            .is_err()
    );
    store
        .append_native(
            Change::NativeBound {
                fact: bound,
                expected: live.revision(),
                fence: live.fence(),
            },
            &context,
        )
        .expect("exact captured native origin must join independently preserved private bytes");
    live.revision += 1;
    assert_eq!(
        store.operation(live.id()).unwrap().stage,
        0,
        "one verified side cannot complete the other required version"
    );
    let reservation = store.reserve_capture_slot(&mut live, &snapshot, 0).unwrap();
    let second = store
        .preserve_file_with_context(
            reservation,
            &mut File::open(&original).unwrap(),
            &cancel,
            Some(&context),
        )
        .unwrap();
    live.revision += 1;
    assert_eq!(
        store.operation(live.id()).unwrap().stage,
        0,
        "even two sides of Bytes require two exact origin facts"
    );
    let Some(Row::Recovery(entry)) = store.row(&recovery::recovery_key(second.value.id)).unwrap()
    else {
        panic!("private row");
    };
    let d::State::Regular {
        object, content, ..
    } = &snapshot.versions[0].state
    else {
        panic!("captured file");
    };
    let exact = d::RecoveryFact {
        intent: snapshot.binding_digest().unwrap(),
        recovery: d::Recovery {
            connection: record.id.0,
            operation: live.id().serial,
            slot: entry.slot,
        },
        side: 0,
        role: 1,
        endpoint: snapshot.endpoints[0],
        object: object.clone(),
        content: content.clone(),
        artifact: recovery::artifact_view(entry.object.as_ref().unwrap()),
    };
    let bound = format::NativeRecoveryFact {
        captured: Some(snapshot.encode().unwrap()),
        connection: record.id.0,
        serial: live.id().serial,
        slot: entry.slot,
        attempt: 0,
        fact: exact.encode().unwrap(),
        displaced: Vec::new(),
    };
    store
        .append_native(
            Change::NativeBound {
                fact: bound,
                expected: live.revision(),
                fence: live.fence(),
            },
            &context,
        )
        .unwrap();
    live.revision += 1;
    assert_eq!(store.operation(live.id()).unwrap().stage, 1);
    assert!(
        store
            .reduce_outcome(
                record.id.0,
                live.id().serial,
                live.revision(),
                &record.source.identity.into(),
                &record.destination.identity.into(),
                false,
                &mut RecoveryChecks::default()
            )
            .is_err(),
        "captured conflict is not ordinary publication"
    );
    assert_eq!(std::fs::read(&original).unwrap(), b"abc");
    let Some(Row::Conflict(conflict)) = store
        .row(&format::key(3, record.id.0, live.id().serial))
        .unwrap()
    else {
        panic!("conflict");
    };
    store
        .append_native(
            Change::NativeConflictComplete {
                connection: record.id.0,
                serial: live.id().serial,
                expected: live.revision(),
                fence: live.fence(),
                conflict_expected: conflict.revision,
                capture: snapshot.binding_digest().unwrap(),
            },
            &context,
        )
        .expect(
            "fully preserved actual conflict must account pending without publication or baseline",
        );
    assert_eq!(store.connection(record.id).unwrap().pending, None);
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
    store.checkpoint().unwrap();
    drop(preserved);
    drop(second);
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(store.operation(live.id()).unwrap().stage, 8);
    assert_eq!(store.connection(record.id).unwrap().pending, None);
    assert_eq!(
        store
            .recovery_page(record.id, None, 8)
            .unwrap()
            .entries
            .len(),
        2
    );
    assert_eq!(std::fs::read(original).unwrap(), b"abc");
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn native_captured_drain_retains_pre_fence_origin_without_clearing_pending() {
    use super::super::transaction::native::descriptor as d;
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 920);
    let session = store.transaction_session(record.id).unwrap();
    let live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let operation = store.operation(live.id()).unwrap();
    let metadata = native_admission_facts(&operation, live.fence());
    let snapshot = d::ConflictSnapshot {
        connection: record.id.0,
        operation: live.id().serial,
        admission: live.admission(),
        revision: live.revision(),
        fence: live.fence(),
        baseline: operation.baseline,
        endpoints: metadata.endpoints,
        path: metadata.path,
        chains: metadata.chains,
        versions: vec![
            d::CapturedVersion {
                side: 0,
                role: 0,
                state: metadata.states[0].clone(),
                displaced: None,
            },
            d::CapturedVersion {
                side: 1,
                role: 0,
                state: d::State::Absent,
                displaced: None,
            },
        ],
    };
    let bytes = snapshot.encode().unwrap();
    store
        .append(Change::NativeCapture {
            snapshot: bytes.clone(),
            displaced: Vec::new(),
            expected: live.revision(),
            kind: 1,
        })
        .unwrap();
    let current = store.connection(record.id).unwrap();
    store
        .request_disconnect(record.id, current.revision)
        .unwrap();
    let current = store.connection(record.id).unwrap();
    let operation = store.operation(live.id()).unwrap();
    let entry = format::Recovery {
        captured: Some(format::CapturedSlot {
            snapshot: bytes.clone(),
            version: 0,
        }),
        connection: record.id.0,
        serial: live.id().serial,
        slot: 0,
        side: 0,
        path: operation.path.clone(),
        reason: 2,
        state: 0,
        content: None,
        object: None,
        retained: 0,
    };
    for bad in 0..5 {
        let mut wrong = entry.clone();
        match bad {
            0 => wrong.path = RelativePath::unix(b"other").unwrap().into(),
            1 => wrong.captured.as_mut().unwrap().version = 1,
            2 => wrong.serial += 1,
            3 => {
                let mut changed = snapshot.clone();
                changed.fence = current.fence;
                wrong.captured.as_mut().unwrap().snapshot = changed.encode().unwrap();
            }
            _ => {
                let mut changed = snapshot.clone();
                if let d::State::Regular { object, .. } = &mut changed.versions[0].state {
                    object.generation[0] ^= 1;
                }
                wrong.captured.as_mut().unwrap().snapshot = changed.encode().unwrap();
            }
        }
        assert!(
            store
                .append(Change::NativeCaptureDrain {
                    entry: wrong,
                    expected: current.revision,
                    operation_expected: operation.revision,
                    fence: current.fence,
                    fact: None
                })
                .is_err()
        );
    }
    assert!(
        store
            .append(Change::Reserve {
                entry: entry.clone(),
                expected: current.revision,
                fence: current.fence
            })
            .is_err()
    );
    store
        .append(Change::NativeCaptureDrain {
            entry: entry.clone(),
            expected: current.revision,
            operation_expected: operation.revision,
            fence: current.fence,
            fact: None,
        })
        .expect("exact pre-fence captured version may reserve a private accounting slot");
    assert!(session.require_idle().is_err());
    for kind in [
        super::super::transaction::StepKind::Namespace,
        super::super::transaction::StepKind::Stage,
        super::super::transaction::StepKind::Relocate,
        super::super::transaction::StepKind::Preserve,
        super::super::transaction::StepKind::Publish,
    ] {
        assert!(
            session
                .admit_step(&store, &live, kind, &AtomicBool::new(false))
                .is_err()
        );
    }
    let current = store.connection(record.id).unwrap();
    let operation = store.operation(live.id()).unwrap();
    let mut incomplete = entry;
    incomplete.state = 5;
    store
        .append(Change::NativeCaptureDrain {
            entry: incomplete,
            expected: current.revision,
            operation_expected: operation.revision,
            fence: current.fence,
            fact: None,
        })
        .unwrap();
    let current = store.connection(record.id).unwrap();
    let fence = store
        .request_disconnect(record.id, current.revision)
        .unwrap();
    assert!(store.finish_disconnect(&fence).is_err());
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        store.recovery_page(record.id, None, 8).unwrap().entries[0].state,
        RecoveryState::RetainedIncomplete
    );
    assert_eq!(
        store.connection(record.id).unwrap().pending,
        Some(live.id().serial)
    );
    assert_eq!(
        store.load_connection(record.id).unwrap().lifecycle,
        Lifecycle::Disconnecting
    );
    assert_eq!(
        store.operation(live.id()).unwrap().native_capture,
        Some(bytes)
    );
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn native_captured_drain_bytes_require_exact_origin_and_paid_private_reread() {
    use super::super::transaction::native::descriptor as d;
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 921);
    let session = store.transaction_session(record.id).unwrap();
    use sha2::Digest;
    let mut proposal = intent(&record);
    proposal.source_before = Some(EntryState::File {
        sha256: sha2::Sha256::digest(b"abc").into(),
        bytes: 3,
        executable: false,
    });
    let live = store.begin_native_for_test(proposal, &session).unwrap();
    let operation = store.operation(live.id()).unwrap();
    let metadata = native_admission_facts(&operation, live.fence());
    let snapshot = d::ConflictSnapshot {
        connection: record.id.0,
        operation: live.id().serial,
        admission: live.admission(),
        revision: live.revision(),
        fence: live.fence(),
        baseline: operation.baseline,
        endpoints: metadata.endpoints,
        path: metadata.path,
        chains: metadata.chains,
        versions: vec![
            d::CapturedVersion {
                side: 0,
                role: 0,
                state: metadata.states[0].clone(),
                displaced: None,
            },
            d::CapturedVersion {
                side: 1,
                role: 0,
                state: d::State::Absent,
                displaced: None,
            },
        ],
    };
    let bytes = snapshot.encode().unwrap();
    store
        .append(Change::NativeCapture {
            snapshot: bytes.clone(),
            displaced: Vec::new(),
            expected: live.revision(),
            kind: 1,
        })
        .unwrap();
    let current = store.connection(record.id).unwrap();
    store
        .request_disconnect(record.id, current.revision)
        .unwrap();
    let current = store.connection(record.id).unwrap();
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut context = store
        .open_recovery_context(live.id(), current.revision)
        .unwrap();
    store
        .reserve_recovery_reads(&mut context, 4 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    let operation = store.operation(live.id()).unwrap();
    let mut entry = format::Recovery {
        captured: Some(format::CapturedSlot {
            snapshot: bytes.clone(),
            version: 0,
        }),
        connection: record.id.0,
        serial: live.id().serial,
        slot: 0,
        side: 0,
        path: operation.path,
        reason: 2,
        state: 0,
        content: None,
        object: None,
        retained: 0,
    };
    store
        .append(Change::NativeCaptureDrain {
            entry: entry.clone(),
            expected: context.revision,
            operation_expected: context.operation_revision,
            fence: context.fence,
            fact: None,
        })
        .unwrap();
    context.revision += 1;
    context.operation_revision = context.revision;
    let directory = store
        .storage
        .recovery
        .create_child(std::ffi::OsStr::new(&format::hex(&record.id.0)))
        .unwrap();
    let name = format!("{}-0", live.id().serial);
    let mut writer = storage::open_file(&directory, &name, true).unwrap();
    std::io::Write::write_all(&mut writer, b"abc").unwrap();
    storage::sync_file(&writer).unwrap();
    let identity = storage::regular(&writer).unwrap();
    let object = format::ObjectIdentity {
        root: store.storage.identity.clone(),
        file_id: storage::file_identity(&identity),
    };
    let d::State::Regular {
        object: origin,
        content,
        ..
    } = &snapshot.versions[0].state
    else {
        panic!("file");
    };
    let bound = d::RecoveryFact {
        intent: snapshot.binding_digest().unwrap(),
        recovery: d::Recovery {
            connection: record.id.0,
            operation: live.id().serial,
            slot: 0,
        },
        side: 0,
        role: 1,
        endpoint: snapshot.endpoints[0],
        object: origin.clone(),
        content: content.clone(),
        artifact: recovery::artifact_view(&object),
    };
    let fact = format::NativeRecoveryFact {
        captured: Some(bytes),
        connection: record.id.0,
        serial: live.id().serial,
        slot: 0,
        attempt: 0,
        fact: bound.encode().unwrap(),
        displaced: Vec::new(),
    };
    entry.state = 1;
    entry.object = Some(object);
    entry.content = Some(format::State::File {
        sha256: content.digest,
        bytes: content.length,
        executable: content.executable,
    });
    let event = Change::NativeCaptureDrain {
        entry: entry.clone(),
        expected: context.revision,
        operation_expected: context.operation_revision,
        fence: context.fence,
        fact: Some(fact.clone()),
    };
    assert!(
        store.append(event.clone()).is_err(),
        "legacy hashing cannot bypass canonical paid drain reads"
    );
    let paid = store
        .recovery_read_context(&context, deadline, &cancel)
        .unwrap();
    let mut wrong = event.clone();
    let Change::NativeCaptureDrain {
        fact: Some(wrong), ..
    } = &mut wrong
    else {
        panic!("fact");
    };
    let mut wrong_bound = bound;
    wrong_bound.object.generation[0] ^= 1;
    wrong.fact = wrong_bound.encode().unwrap();
    let mut wrong_event = event.clone();
    if let Change::NativeCaptureDrain { fact, .. } = &mut wrong_event {
        *fact = Some(wrong.clone());
    }
    assert!(store.append_native(wrong_event, &paid).is_err());
    std::io::Seek::seek(&mut writer, std::io::SeekFrom::Start(0)).unwrap();
    std::io::Write::write_all(&mut writer, b"bad").unwrap();
    storage::sync_file(&writer).unwrap();
    assert!(store.append_native(event.clone(), &paid).is_err());
    assert_eq!(
        std::fs::read(
            path.join("recovery")
                .join(format::hex(&record.id.0))
                .join(&name)
        )
        .unwrap(),
        b"bad"
    );
    std::io::Seek::seek(&mut writer, std::io::SeekFrom::Start(0)).unwrap();
    std::io::Write::write_all(&mut writer, b"abc").unwrap();
    storage::sync_file(&writer).unwrap();
    store
        .append_native(event, &paid)
        .expect("fresh paid exact captured bytes may bind after fence without completing work");
    assert_eq!(
        store.row(&fact.key()).unwrap(),
        Some(Row::NativeRecoveryFact(fact))
    );
    assert_eq!(store.operation(live.id()).unwrap().stage, 0);
    assert_eq!(
        store.connection(record.id).unwrap().pending,
        Some(live.id().serial)
    );
    assert!(session.require_idle().is_err());
    assert_eq!(
        store.load_connection(record.id).unwrap().lifecycle,
        Lifecycle::Disconnecting
    );
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
    drop(store); // exercise native-only historical event replay, without checkpoint
    std::io::Seek::seek(&mut writer, std::io::SeekFrom::Start(0)).unwrap();
    std::io::Write::write_all(&mut writer, b"new").unwrap();
    storage::sync_file(&writer).unwrap();
    drop(writer);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let current = store.connection(record.id).unwrap();
    let mut context = store
        .open_recovery_context(live.id(), current.revision)
        .unwrap();
    store
        .reserve_recovery_reads(&mut context, 4 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    let paid = store
        .recovery_read_context(&context, deadline, &cancel)
        .unwrap();
    assert!(
        store
            .verify_recovery_with_context(&entry, Some(&paid))
            .is_err()
    );
    assert_eq!(
        std::fs::read(
            path.join("recovery")
                .join(format::hex(&record.id.0))
                .join(&name)
        )
        .unwrap(),
        b"new"
    );
    assert_eq!(
        store.connection(record.id).unwrap().pending,
        Some(live.id().serial)
    );
    assert_eq!(
        store
            .operation(live.id())
            .unwrap()
            .native_budget
            .unwrap()
            .reserved,
        8 * 1024 * 1024
    );
    store.checkpoint().unwrap();
}

#[test]
fn native_reviewed_restore_selection_is_consumed_atomically_with_scoped_consent() {
    reviewed_restore_case(0);
}

#[test]
fn native_selected_stopped_boundaries_account_without_application_or_baseline() {
    for kind in 1..=3 {
        reviewed_restore_case(kind);
    }
}

#[test]
fn native_selected_stopped_drain_reserves_only_existing_displacement() {
    reviewed_restore_case(4);
}

#[test]
fn native_selected_untouched_accounts_only_atomic_history_with_paid_choice() {
    for boundary in 5..=6 {
        reviewed_restore_case(boundary);
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn reviewed_restore_case(boundary: u8) {
    for (replace, target_absent) in [(false, false), (true, false), (false, true)] {
        if boundary > 1 && !replace && !target_absent {
            continue;
        }
        if matches!(boundary, 2 | 4) && !replace {
            continue;
        }
        let (_temp, path) = fixture();
        let mut store = ConnectionStore::open_at(&path).unwrap();
        let record = create(&mut store, 923);
        let mut prior = intent(&record);
        prior.destination_before = prior.source_before.clone();
        let ack = store.begin_intent(prior).unwrap();
        let reservation = store
            .reserve_recovery(
                &ack,
                Side::Destination,
                RelativePath::unix(b"file").unwrap(),
                RecoveryReason::DisplacedVersion,
            )
            .unwrap();
        let original = path.parent().unwrap().join("selected-original");
        std::fs::write(&original, b"old").unwrap();
        let preserved = store
            .preserve_file(
                reservation,
                &mut File::open(&original).unwrap(),
                &AtomicBool::new(false),
            )
            .unwrap();
        let selected = store
            .recovery_page(record.id, None, 8)
            .unwrap()
            .entries
            .remove(0);
        assert_eq!(selected.id, preserved.value.id);
        for verified in [false, true] {
            let expected = store.load_connection(record.id).unwrap().revision;
            let change = if verified {
                Change::Verified {
                    connection: record.id.0,
                    serial: ack.value.serial,
                    expected,
                    source: record.source.identity.into(),
                    destination: record.destination.identity.into(),
                }
            } else {
                Change::Published {
                    connection: record.id.0,
                    serial: ack.value.serial,
                    expected,
                    source: record.source.identity.into(),
                    destination: record.destination.identity.into(),
                }
            };
            store.append(change).unwrap();
        }
        let expected = store.load_connection(record.id).unwrap().revision;
        let root = storage::build_pages(
            &store.storage,
            std::iter::once(Ok(Row::Baseline(format::BaselineEntry {
                connection: record.id.0,
                revision: expected + 1,
                ordinal: 0,
                path: format::NativePath::Unix(b"file".to_vec()),
                state: intent(&record).source_before.unwrap().into(),
            }))),
        )
        .unwrap();
        store
            .append(Change::BaselineCommit { directory_chain: None,
                directory_ancestors: None,
                existing_ancestors: None,
                connection: record.id.0,
                expected,
                root,
                count: 1,
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
                initial_review: true,
            })
            .unwrap();
        // Isolate native-only replay from the intentionally strict earlier legacy Bytes event.
        if boundary != 0 {
            store.checkpoint().unwrap();
        }
        let current = store.load_connection(record.id).unwrap();
        let mut proposal = intent(&current);
        proposal.expected_baseline = current.baseline_revision;
        // Keep an actual equal observation. It must not become VerifyEqual execution authority.
        proposal.destination_before = proposal.source_before.clone();
        proposal.decision = Decision::VerifyEqual;
        if target_absent {
            proposal.destination_before = None;
            proposal.decision = Decision::RecoverDeletion { at: Side::Source };
        }
        if replace {
            proposal.destination_before = Some(EntryState::File {
                sha256: sha2::Sha256::digest(b"new").into(),
                bytes: 3,
                executable: false,
            });
            proposal.decision = Decision::Propagate {
                from: Side::Destination,
            };
        }
        use super::super::model::{Endpoint, NamePolicy, Observation, ObservedTree};
        let observe = |identity, state: Option<EntryState>| {
            ObservedTree::new(
                Endpoint {
                    identity,
                    encoding: Encoding::Unix,
                    names: NamePolicy::Exact,
                },
                state
                    .into_iter()
                    .map(|state| (proposal.path.clone(), state)),
                Observation::Complete,
            )
            .unwrap()
        };
        let source_observation = observe(current.source.identity, proposal.source_before.clone());
        let destination_observation = observe(
            current.destination.identity,
            proposal.destination_before.clone(),
        );
        let log_before_preview =
            std::fs::read(path.join(format!("log-{}", store.current.log))).unwrap();
        for bad in 0..3 {
            let mut endpoint = source_observation.endpoint().clone();
            if bad == 2 {
                endpoint.identity.root[0] ^= 1;
            }
            let observation = match bad {
                0 => Observation::Unavailable(super::super::model::PendingReason::Offline),
                1 => Observation::Incomplete(vec![super::super::model::ScopeIssue {
                    path: None,
                    reason: super::super::model::PendingReason::Unstable,
                }]),
                _ => Observation::Complete,
            };
            let source = ObservedTree::new(endpoint, std::iter::empty(), observation).unwrap();
            assert!(
                store
                    .restore_preview(
                        selected.id,
                        Side::Destination,
                        None,
                        source,
                        destination_observation.clone()
                    )
                    .is_err()
            );
        }
        assert_eq!(
            std::fs::read(path.join(format!("log-{}", store.current.log))).unwrap(),
            log_before_preview
        );
        let preview = store
            .restore_preview(
                selected.id,
                Side::Destination,
                None,
                source_observation,
                destination_observation,
            )
            .expect("complete actual planning observations must produce a scoped restore preview");
        assert_eq!(preview.intent(), &proposal);
        let reviewed = if replace {
            preview.accept_replacement()
        } else {
            preview.accept()
        };
        let older = store
            .restore_review_basis(proposal.clone(), selected.id, Side::Destination, None)
            .unwrap()
            .accept_replacement();
        assert_eq!(reviewed.intent(), &proposal);
        let session = store.transaction_session(record.id).unwrap();
        let mut live = if boundary >= 5 {
            let (intent, action) = store.consume_reviewed_selection(reviewed).unwrap();
            let connection = store.connection(record.id).unwrap();
            let operation =
                format::Operation::new(intent.clone(), connection.serial + 1, &connection).unwrap();
            let identity = native_admission_facts(&operation, connection.fence)
                .encode()
                .unwrap();
            store
                .begin_native_operation(intent, &session, None, Some(identity), Some(action), None)
                .unwrap()
        } else {
            store
                .begin_native_selected_for_test(reviewed, &session)
                .expect(
                    "explicit reviewed selection must bind atomically to its canonical operation",
                )
        };
        let log = std::fs::read(path.join(format!("log-{}", store.current.log))).unwrap();
        assert!(
            store
                .begin_native_selected_for_test(older, &session)
                .is_err(),
            "another token for the old revision cannot replay consent"
        );
        assert_eq!(
            std::fs::read(path.join(format!("log-{}", store.current.log))).unwrap(),
            log
        );
        let operation = store.operation(live.id()).unwrap();
        let Some(format::SelectedAction::Restore {
            replace: actual,
            selected: version,
            target,
            revision,
            fence,
            ..
        }) = &operation.action
        else {
            panic!("selected action");
        };
        assert_eq!(*actual, replace);
        assert_eq!(*target, 1);
        assert_eq!(*revision, current.revision);
        assert_eq!(*fence, current.fence_generation);
        assert_eq!(
            version,
            &format::SelectedVersion::Recovery {
                operation: selected.id.operation.serial,
                slot: selected.id.slot,
                content: selected.content.clone().unwrap().into()
            }
        );
        assert_eq!(operation.decode().unwrap().intent, proposal);
        assert!(!live.action().is_reconciliation());
        assert_eq!(
            store
                .operation(live.id())
                .unwrap()
                .native_budget
                .unwrap()
                .reserved,
            0,
            "review minted no read credit or byte proof"
        );
        assert_eq!(
            std::fs::read(
                path.join("recovery")
                    .join(format::hex(&record.id.0))
                    .join("1-0")
            )
            .unwrap(),
            b"old"
        );
        for kind in [
            super::super::transaction::StepKind::Namespace,
            super::super::transaction::StepKind::Stage,
            super::super::transaction::StepKind::Relocate,
            super::super::transaction::StepKind::Preserve,
            super::super::transaction::StepKind::Publish,
        ] {
            assert!(
                session
                    .admit_step(&store, &live, kind, &AtomicBool::new(false))
                    .is_err()
            );
        }
        let cancel = AtomicBool::new(false);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        assert!(
            store
                .open_verified_recovery(&live, deadline, &cancel)
                .is_err(),
            "selection alone supplies no read credit"
        );
        store
            .reserve_native_reads(&mut live, &session, 16 * 1024 * 1024, deadline, &cancel)
            .unwrap();
        let mut reader = store
            .open_verified_recovery(&live, deadline, &cancel)
            .expect("fresh selected private bytes are charged to the new canonical operation");
        let mut chunk = [0; 4];
        assert_eq!(reader.read_chunk(&store, &live, 0, &mut chunk).unwrap(), 3);
        assert_eq!(&chunk[..3], b"old");
        assert_eq!(reader.read_chunk(&store, &live, 3, &mut chunk).unwrap(), 0);
        assert_eq!(reader.binding().unwrap().0, selected.id);
        reader.validate(&store, &live).unwrap();
        cancel.store(true, std::sync::atomic::Ordering::Release);
        assert!(reader.read_chunk(&store, &live, 0, &mut chunk).is_err());
        cancel.store(false, std::sync::atomic::Ordering::Release);
        assert!(
            reader.read_chunk(&store, &live, 0, &mut chunk).is_err(),
            "cancelled private proof must remain invalid even if the flag is cleared"
        );
        if boundary >= 5 {
            if boundary == 5 {
                store
                    .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [75; 16])
                    .unwrap();
            }
            let baseline = store.load_baseline(record.id).unwrap();
            let fence = store
                .request_disconnect(record.id, live.revision())
                .unwrap();
            let current = store.connection(record.id).unwrap();
            let mut context = store
                .open_recovery_context(live.id(), current.revision)
                .unwrap();
            store
                .reserve_recovery_reads(&mut context, 4 * 1024 * 1024, deadline, &cancel)
                .unwrap();
            let event: Change = serde_json::from_value(serde_json::json!({ "event": "SelectedUntouched", "connection": record.id.0,
            "serial": live.id().serial, "expected": context.revision(), "operation_expected": context.operation_revision,
            "fence": context.fence() })).expect("untouched selected work requires dedicated stopped accounting");
            assert!(
                store.append(event.clone()).is_err(),
                "metadata alone cannot account selected work"
            );
            let paid = store
                .recovery_read_context(&context, deadline, &cancel)
                .unwrap();
            let chosen = path
                .join("recovery")
                .join(format::hex(&record.id.0))
                .join("1-0");
            std::fs::write(&chosen, b"bad").unwrap();
            assert!(store.append_native(event.clone(), &paid).is_err());
            std::fs::write(&chosen, b"old").unwrap();
            let original = store.operation(live.id()).unwrap();
            let key = format::key(1, record.id.0, live.id().serial);
            for damage in 0..2 {
                let mut changed = original.clone();
                if damage == 0 {
                    changed.native_identity = None;
                } else {
                    changed.stage = 6;
                }
                let previous = store.overlay.insert(key.clone(), Row::Operation(changed));
                assert!(
                    store.append_native(event.clone(), &paid).is_err(),
                    "missing historical identity or already prepared work is not untouched"
                );
                if let Some(row) = previous {
                    store.overlay.insert(key.clone(), row);
                } else {
                    store.overlay.remove(&key);
                }
            }
            for field in 0..4 {
                let mut changed = event.clone();
                let Change::SelectedUntouched {
                    serial,
                    expected,
                    operation_expected,
                    fence,
                    ..
                } = &mut changed
                else {
                    panic!("untouched");
                };
                match field {
                    0 => *serial += 1,
                    1 => *expected += 1,
                    2 => *operation_expected += 1,
                    _ => *fence += 1,
                }
                assert!(store.append_native(changed, &paid).is_err());
            }
            assert!(
                store
                    .append(Change::NativeBoundary {
                        connection: record.id.0,
                        serial: live.id().serial,
                        expected: context.revision(),
                        operation_expected: context.operation_revision,
                        fence: context.fence(),
                        kind: 0,
                        evidence: None
                    })
                    .is_err()
            );
            store
                .append_native(event.clone(), &paid)
                .expect("atomic unchanged history and paid selected bytes account untouched work");
            assert!(
                store.append_native(event, &paid).is_err(),
                "a consumed context cannot append twice"
            );
            let operation = store.operation(live.id()).unwrap();
            assert_eq!(operation.native_boundary, Some(0));
            assert_eq!(operation.stage, 7);
            assert!(operation.selected_applied.is_none());
            assert!(operation.recovery.is_empty());
            assert!(store.connection(record.id).unwrap().pending.is_none());
            assert_eq!(store.load_baseline(record.id).unwrap(), baseline);
            store.finish_disconnect(&fence).unwrap();
            if boundary == 5 {
                store.checkpoint().unwrap();
            }
            drop(store);
            // The other branch replays native-only accounting after a later byte change.
            if boundary == 6 {
                std::fs::write(&chosen, b"bad").unwrap();
            }
            let mut store = ConnectionStore::open_at(&path).unwrap();
            if boundary == 6 {
                let current = store.connection(record.id).unwrap();
                let mut context = store
                    .open_recovery_context(live.id(), current.revision)
                    .unwrap();
                store
                    .reserve_recovery_reads(&mut context, 4 * 1024 * 1024, deadline, &cancel)
                    .unwrap();
                let paid = store
                    .recovery_read_context(&context, deadline, &cancel)
                    .unwrap();
                let mut checks = RecoveryChecks {
                    mode: VerificationMode::NativeLive(&paid),
                    ..RecoveryChecks::default()
                };
                assert!(
                    checks
                        .verify_stopped_selected(&store, &store.operation(live.id()).unwrap())
                        .is_err()
                );
            }
            assert_eq!(
                store.load_connection(record.id).unwrap().lifecycle,
                Lifecycle::Disconnected
            );
            assert_eq!(store.operation(live.id()).unwrap().native_boundary, Some(0));
            assert_eq!(store.load_baseline(record.id).unwrap(), baseline);
            assert_eq!(
                std::fs::read(chosen).unwrap(),
                if boundary == 5 { b"old" } else { b"bad" }
            );
            continue;
        }
        if replace || target_absent {
            use super::super::transaction::native::descriptor as d;
            let namespace = store
                .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [72; 16])
                .expect("canonical selected target must reserve its own native attempt");
            let mut registration = native_prepared_facts(&live, &namespace);
            let mut descriptor = d::decode(registration.intent.as_ref().unwrap()).unwrap();
            let d::Body::Intent(body) = &mut descriptor.body else {
                panic!("intent");
            };
            body.decision = if target_absent { 3 } else { 2 };
            body.expected[1] = if target_absent {
                d::State::Absent
            } else {
                body.expected[0].clone()
            };
            if let d::State::Regular { content, .. } = &mut body.expected[1] {
                content.digest = sha2::Sha256::digest(b"new").into();
            }
            let reader = store
                .open_verified_recovery(&live, deadline, &cancel)
                .unwrap();
            let (id, artifact, content) = reader.binding().unwrap();
            let recovery = d::Recovery {
                connection: id.operation.connection.0,
                operation: id.operation.serial,
                slot: id.slot,
            };
            body.selection = d::Selection::Restore {
                recovery: recovery.clone(),
                artifact: artifact.clone(),
                content: content.clone(),
            };
            registration.intent = Some(d::encode(&descriptor.header, &descriptor.body).unwrap());
            let mut stage = d::decode(registration.stage.as_ref().unwrap()).unwrap();
            let d::Body::Stage(staged) = &mut stage.body else {
                panic!("stage");
            };
            staged.intent = d::decode(registration.intent.as_ref().unwrap())
                .unwrap()
                .digest;
            staged.content = content.clone();
            staged.source = d::Source::Recovery {
                recovery,
                artifact,
                content,
            };
            registration.stage = Some(d::encode(&stage.header, &stage.body).unwrap());
            for bad in 0..3 {
                let mut wrong = registration.clone();
                let mut changed = d::decode(wrong.intent.as_ref().unwrap()).unwrap();
                let d::Body::Intent(body) = &mut changed.body else {
                    panic!("intent");
                };
                match bad {
                    0 => {
                        body.selection = d::Selection::None;
                        body.decision = 0;
                    }
                    1 => {
                        let d::Selection::Restore { recovery, .. } = &mut body.selection else {
                            panic!("selection");
                        };
                        recovery.slot += 1;
                    }
                    _ => body.direction = 1,
                }
                wrong.intent = Some(d::encode(&changed.header, &changed.body).unwrap());
                let mut stage = d::decode(wrong.stage.as_ref().unwrap()).unwrap();
                let d::Body::Stage(staged) = &mut stage.body else {
                    panic!("stage");
                };
                staged.intent = d::decode(wrong.intent.as_ref().unwrap()).unwrap().digest;
                if bad == 1 {
                    let d::Source::Recovery { recovery, .. } = &mut staged.source else {
                        panic!("private source");
                    };
                    recovery.slot += 1;
                }
                wrong.stage = Some(d::encode(&stage.header, &stage.body).unwrap());
                if bad != 0 {
                    wrong.validate().unwrap();
                }
                assert!(
                    store
                        .reduce_native_registration(&wrong, live.revision(), true)
                        .is_err()
                );
            }
            store.append(Change::NativePrepared { registration: registration.clone(), expected: live.revision() })
            .expect("selected prepared facts join exact pinned private version independently of observed equality");
            live.revision += 1;
            assert_eq!(
                d::decode(registration.stage.as_ref().unwrap())
                    .unwrap()
                    .profile,
                d::Profile::LegacyStage
            );
            assert!(
                !store.legacy_native_pending(live.id()).unwrap(),
                "selected QNS2 stays on its existing selected route"
            );
            assert_eq!(
                store.operation(live.id()).unwrap().stage,
                if target_absent { 6 } else { 0 },
                "selected absence is prepared, occupied target still awaits preservation"
            );
            if boundary == 1 {
                account_selected_boundary_case(store, &live, &registration, boundary, &path);
                continue;
            }
            let mut displaced_outcome = None;
            let mut selected_references = Vec::new();
            if replace {
                let reservation = store.reserve_recovery(&store.ack(live.id()), Side::Destination, proposal.path.clone(), RecoveryReason::DisplacedVersion)
                .expect("selected replacement retains the current target independently of the observed propagation source");
                live.revision += 1;
                let Some(Row::Recovery(mut candidate)) =
                    store.row(&recovery::recovery_key(reservation.id)).unwrap()
                else {
                    panic!("reservation");
                };
                candidate.slot += 1;
                assert!(
                    store
                        .reduce_recovery(&candidate, live.revision(), live.fence(), false)
                        .is_ok()
                );
                for wrong_side_or_reason in 0..2 {
                    let mut wrong = candidate.clone();
                    if wrong_side_or_reason == 0 {
                        wrong.side = 0;
                    } else {
                        wrong.reason = 2;
                    }
                    assert!(
                        store
                            .reduce_recovery(&wrong, live.revision(), live.fence(), false)
                            .is_err()
                    );
                }
                let entries = store.recovery_page(record.id, None, 8).unwrap().entries;
                assert!(
                    entries.iter().any(|entry| entry.id == reservation.id
                        && entry.state == RecoveryState::Reserved)
                );
                assert!(
                    entries
                        .iter()
                        .any(|entry| entry.id == selected.id && entry.content == selected.content)
                );
                let current_bytes = path.parent().unwrap().join("current-target");
                std::fs::write(&current_bytes, b"new").unwrap();
                let paid = store.native_read_context(&live, deadline, &cancel).unwrap();
                let preserved = store
                    .preserve_file_with_context(
                        reservation,
                        &mut File::open(&current_bytes).unwrap(),
                        &cancel,
                        Some(&paid),
                    )
                    .unwrap();
                live.revision = store.load_connection(record.id).unwrap().revision;
                assert_eq!(
                    store.operation(live.id()).unwrap().stage,
                    0,
                    "Bytes alone are not exact native displacement"
                );
                let id = preserved.value.id;
                let Some(Row::Recovery(entry)) = store.row(&recovery::recovery_key(id)).unwrap()
                else {
                    panic!("preserved target");
                };
                let original = d::decode(registration.intent.as_ref().unwrap()).unwrap();
                let stage = d::decode(registration.stage.as_ref().unwrap()).unwrap();
                let d::Body::Intent(body) = &original.body else {
                    panic!("intent");
                };
                let d::State::Regular {
                    object,
                    stamp,
                    content,
                } = &body.expected[1]
                else {
                    panic!("target");
                };
                let displaced = d::encode(
                    &original.header,
                    &d::Body::Displaced(d::Displaced {
                        intent: original.digest,
                        stage: Some(stage.digest),
                        object: object.clone(),
                        stamp: stamp.clone(),
                        matches_expected: true,
                        side: 1,
                        parent: body.chains[1][0].clone(),
                        leaf: body.path.components()[0].clone(),
                        barriers: 3,
                    }),
                )
                .unwrap();
                let fact = d::RecoveryFact {
                    intent: original.digest,
                    recovery: d::Recovery {
                        connection: id.operation.connection.0,
                        operation: id.operation.serial,
                        slot: id.slot,
                    },
                    side: 1,
                    role: 0,
                    endpoint: body.endpoints[1],
                    object: object.clone(),
                    content: content.clone(),
                    artifact: recovery::artifact_view(entry.object.as_ref().unwrap()),
                };
                let bound = format::NativeRecoveryFact {
                    captured: None,
                    connection: id.operation.connection.0,
                    serial: id.operation.serial,
                    slot: id.slot,
                    attempt: 1,
                    fact: fact.encode().unwrap(),
                    displaced,
                };
                let mut wrong = fact.clone();
                wrong.object.generation[0] ^= 1;
                assert!(
                    store
                        .reduce_native_bound(
                            &format::NativeRecoveryFact {
                                fact: wrong.encode().unwrap(),
                                ..bound.clone()
                            },
                            live.revision(),
                            live.fence()
                        )
                        .is_err()
                );
                selected_references.push(d::Reference {
                    slot: id.slot,
                    side: 1,
                    role: 0,
                    binding: fact.binding_digest().unwrap(),
                });
                displaced_outcome = Some(bound.displaced.clone());
                store
                    .append_native(
                        Change::NativeBound {
                            fact: bound,
                            expected: live.revision(),
                            fence: live.fence(),
                        },
                        &paid,
                    )
                    .unwrap();
                live.revision += 1;
                assert_eq!(
                    store.operation(live.id()).unwrap().stage,
                    1,
                    "only exact selected target displacement enables the next native phase"
                );
                assert_eq!(
                    std::fs::read(
                        path.join("recovery")
                            .join(format::hex(&record.id.0))
                            .join(format!("{}-{}", id.operation.serial, id.slot))
                    )
                    .unwrap(),
                    b"new"
                );
                assert_eq!(
                    std::fs::read(
                        path.join("recovery")
                            .join(format::hex(&record.id.0))
                            .join("1-0")
                    )
                    .unwrap(),
                    b"old"
                );
            }
            if matches!(boundary, 2 | 4) {
                registration.outcome_displaced = displaced_outcome;
                account_selected_boundary_case(store, &live, &registration, boundary, &path);
                continue;
            }
            if target_absent || replace {
                let baseline = store.load_baseline(record.id).unwrap();
                let parsed = d::decode(registration.intent.as_ref().unwrap()).unwrap();
                let d::Body::Intent(body) = &parsed.body else {
                    panic!("intent");
                };
                let stage = d::decode(registration.stage.as_ref().unwrap()).unwrap();
                let d::Body::Stage(staged) = &stage.body else {
                    panic!("stage");
                };
                let d::State::Regular { stamp, .. } = &body.expected[0] else {
                    panic!("current source");
                };
                let mut header = parsed.header.clone();
                header.progress = live.revision();
                let outcome = d::Outcome {
                    intent: parsed.digest,
                    stage: Some(stage.digest),
                    displaced: displaced_outcome
                        .as_ref()
                        .map(|bytes| d::decode(bytes).unwrap().digest),
                    states: [
                        body.expected[0].clone(),
                        d::State::Regular {
                            object: staged.object.clone(),
                            stamp: stamp.clone(),
                            content: staged.content.clone(),
                        },
                    ],
                    kind: u8::from(replace),
                    barriers: 3,
                    recovery: selected_references,
                    sequence: 1,
                };
                registration.outcome_displaced = displaced_outcome;
                registration.outcome =
                    Some(d::encode(&header, &d::Body::Outcome(outcome)).unwrap());
                registration.validate().unwrap();
                for verified in [false, true] {
                    let paid = store.native_read_context(&live, deadline, &cancel).unwrap();
                    let mut checks = RecoveryChecks {
                        mode: super::VerificationMode::NativeLive(&paid),
                        ..RecoveryChecks::default()
                    };
                    let chosen_path = path
                        .join("recovery")
                        .join(format::hex(&record.id.0))
                        .join("1-0");
                    std::fs::write(&chosen_path, b"bad").unwrap();
                    assert!(
                        store
                            .reduce_native_outcome(
                                &registration,
                                live.revision(),
                                verified,
                                false,
                                &mut checks
                            )
                            .is_err(),
                        "current selected bytes must be independently paid and checked"
                    );
                    std::fs::write(&chosen_path, b"old").unwrap();
                    store.append_native(Change::NativeOutcome { registration: registration.clone(), expected: live.revision(), verified, reconstructed: false }, &paid)
                    .expect("native selected outcome accepts exact target success without an equality assertion");
                    live.revision += 1;
                }
                assert_eq!(store.operation(live.id()).unwrap().stage, 3);
                assert_eq!(
                    store.load_baseline(record.id).unwrap(),
                    baseline,
                    "selected target verification cannot advance baseline"
                );
                assert!(store.connection(record.id).unwrap().pending.is_some());
                assert_eq!(
                    std::fs::read(
                        path.join("recovery")
                            .join(format::hex(&record.id.0))
                            .join("1-0")
                    )
                    .unwrap(),
                    b"old"
                );
                if boundary == 3 {
                    account_selected_boundary_case(store, &live, &registration, boundary, &path);
                    continue;
                }
                let completion = serde_json::json!({ "event": "SelectedApplied", "connection": record.id.0, "serial": live.id().serial,
                "expected": live.revision(), "fence": live.fence(), "attempt": registration.attempt,
                "outcome": d::decode(registration.outcome.as_ref().unwrap()).unwrap().digest });
                let completion: Change = serde_json::from_value(completion)
                    .expect("selected target accounting must have a distinct durable completion");
                let paid = store.native_read_context(&live, deadline, &cancel).unwrap();
                store.append_native(completion, &paid).expect("accounted selected application releases executable pending without baseline advancement");
                assert!(store.connection(record.id).unwrap().pending.is_none());
                assert_eq!(store.operation(live.id()).unwrap().stage, 9);
                assert_eq!(
                    serde_json::to_value(store.operation(live.id()).unwrap()).unwrap()["selected_applied"]
                        ["attention"],
                    true
                );
                assert_eq!(store.load_baseline(record.id).unwrap(), baseline);
                let applied_revision = store.operation(live.id()).unwrap().revision;
                let mut inspection = store
                    .open_recovery_context(live.id(), applied_revision)
                    .unwrap();
                assert!(inspection.recovery_only());
                store
                    .reserve_recovery_reads(&mut inspection, 4 * 1024 * 1024, deadline, &cancel)
                    .unwrap();
                assert_eq!(store.selected_application(live.id()).expect("paid inspection after accounting must not corrupt historical SelectedApplied").unwrap().stage, ReceiptStage::SelectedApplied);
                assert_eq!(
                    store
                        .operation(live.id())
                        .unwrap()
                        .selected_applied
                        .unwrap()
                        .revision,
                    applied_revision,
                    "historical application revision does not become a new application"
                );
                let mut future_fact = store.operation(live.id()).unwrap();
                future_fact.selected_applied.as_mut().unwrap().revision = future_fact.revision + 1;
                assert!(
                    future_fact.decode().is_err(),
                    "a future application is not historical completion"
                );
                let current = store.connection(record.id).unwrap();
                let final_context = store
                    .open_recovery_context(live.id(), current.revision)
                    .unwrap();
                assert!(
                    store.check_final_context(&final_context).is_err(),
                    "selected attention without a conflict row must block Up to date"
                );
                assert!(
                    !store.final_context_ready(&final_context).unwrap(),
                    "durable attention must remain distinct from broken current admission"
                );
                let mut unsafe_plan = proposal.clone();
                unsafe_plan.expected_revision = current.revision;
                unsafe_plan.decision = Decision::Propagate { from: Side::Source };
                let mut undo =
                    format::Operation::new(unsafe_plan.clone(), current.serial + 1, &current)
                        .unwrap();
                undo.native_budget = Some(format::NativeBudget::initial());
                assert!(
                    store.reduce_intent(&undo).is_err(),
                    "old baseline cannot silently overwrite the applied selected choice"
                );
                unsafe_plan.path = RelativePath::unix(b"independent").unwrap();
                let mut independent =
                    format::Operation::new(unsafe_plan, current.serial + 1, &current).unwrap();
                independent.native_budget = Some(format::NativeBudget::initial());
                assert!(
                    store.reduce_intent(&independent).is_ok(),
                    "per-path attention must not block independent work"
                );
                let sequence = store.sequence;
                let receipt = store.selected_application(live.id()).unwrap().unwrap();
                assert_eq!(receipt.stage, ReceiptStage::SelectedApplied);
                assert_eq!(
                    store.selected_application(live.id()).unwrap(),
                    Some(receipt)
                );
                assert_eq!(
                    store.sequence, sequence,
                    "lost-ack accounting query appends nothing"
                );
                store.retention_sample = std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(60))
                    .unwrap();
                store.checkpoint().unwrap();
                let retained = store.recovery_page(record.id, None, 8).unwrap().entries;
                assert_eq!(
                    retained
                        .iter()
                        .find(|entry| entry.id == selected.id)
                        .unwrap()
                        .retained_seconds,
                    0,
                    "selected attention pins the chosen version after pending clears"
                );
                if replace {
                    let displaced = retained
                        .iter()
                        .find(|entry| entry.id.operation == live.id())
                        .unwrap();
                    assert_eq!(displaced.state, RecoveryState::Attached);
                    assert_eq!(displaced.retained_seconds, 0);
                }
                // An independently verified second file can commit while the selected
                // path retains its old baseline and actionable attention record.
                let current = store.load_connection(record.id).unwrap();
                let other =
                    native_verified_addition_facts(&mut store, &current, b"independent", &cancel);
                let payer = store
                    .native_read_context(&other, deadline, &cancel)
                    .unwrap();
                let root = storage::build_pages(
                    &store.storage,
                    [b"file".as_slice(), b"independent"]
                        .into_iter()
                        .enumerate()
                        .map(|(ordinal, path)| {
                            Ok(Row::Baseline(format::BaselineEntry {
                                connection: record.id.0,
                                revision: other.revision() + 1,
                                ordinal: ordinal as u32,
                                path: format::NativePath::Unix(path.to_vec()),
                                state: format::State::File {
                                    sha256: [7; 32],
                                    bytes: 3,
                                    executable: false,
                                },
                            }))
                        }),
                )
                .unwrap();
                store
                    .append_native(
                        Change::BaselineCommit { directory_chain: None,
                            directory_ancestors: None,
                            existing_ancestors: None,
                            connection: record.id.0,
                            expected: other.revision(),
                            root,
                            count: 2,
                            source: record.source.identity.into(),
                            destination: record.destination.identity.into(),
                            initial_review: false,
                        },
                        &payer,
                    )
                    .unwrap();
                assert_eq!(
                    store
                        .load_baseline(record.id)
                        .unwrap()
                        .entries()
                        .unwrap()
                        .get(&proposal.path),
                    baseline.entries().unwrap().get(&proposal.path)
                );
                assert_eq!(
                    store
                        .selected_attention(
                            record.id,
                            &format::NativePath::from(proposal.path.clone())
                        )
                        .unwrap()
                        .len(),
                    1
                );
                assert!(
                    store.final_context_ready(&final_context).is_err(),
                    "stale admission cannot become harmless attention"
                );
                let complete = store
                    .open_recovery_context(
                        other.id(),
                        store.load_connection(record.id).unwrap().revision,
                    )
                    .unwrap();
                assert!(
                    !store.final_context_ready(&complete).unwrap(),
                    "independent committed file succeeds while attention prevents Up to date"
                );
                let current = store.load_connection(record.id).unwrap();
                let mut matching = proposal.clone();
                matching.expected_revision = current.revision;
                matching.expected_baseline = current.baseline_revision;
                matching.source_before = selected.content.clone();
                matching.destination_before = selected.content.clone();
                matching.decision = Decision::VerifyEqual;
                let mut equality = store.begin_native_for_test(matching, &session).unwrap();
                store
                    .reserve_native_reads(
                        &mut equality,
                        &session,
                        64 * 1024 * 1024,
                        deadline,
                        &cancel,
                    )
                    .unwrap();
                let payer = store
                    .native_read_context(&equality, deadline, &cancel)
                    .unwrap();
                store
                    .append_native(
                        Change::Verified {
                            connection: record.id.0,
                            serial: equality.id().serial,
                            expected: equality.revision(),
                            source: record.source.identity.into(),
                            destination: record.destination.identity.into(),
                        },
                        &payer,
                    )
                    .unwrap();
                equality.revision += 1;
                let root = storage::build_pages(
                    &store.storage,
                    [b"file".as_slice(), b"independent"]
                        .into_iter()
                        .enumerate()
                        .map(|(ordinal, path)| {
                            Ok(Row::Baseline(format::BaselineEntry {
                                connection: record.id.0,
                                revision: equality.revision() + 1,
                                ordinal: ordinal as u32,
                                path: format::NativePath::Unix(path.to_vec()),
                                state: if ordinal == 0 {
                                    selected.content.clone().unwrap().into()
                                } else {
                                    format::State::File {
                                        sha256: [7; 32],
                                        bytes: 3,
                                        executable: false,
                                    }
                                },
                            }))
                        }),
                )
                .unwrap();
                let prior = store.load_baseline(record.id).unwrap();
                let prior_attention = store
                    .selected_attention(record.id, &format::NativePath::from(proposal.path.clone()))
                    .unwrap();
                let probe = BaselineAppendProbe {
                    generation: store.current.generation,
                    checkpoint: true,
                    reject_call: 1,
                    calls: std::cell::Cell::new(0),
                };
                store.overlay_bytes = 9 * 1024 * 1024;
                assert!(matches!(
                    store.append_checked_with_baseline(
                        Change::BaselineCommit { directory_chain: None,
                            directory_ancestors: None,
                            existing_ancestors: None,
                            connection: record.id.0,
                            expected: equality.revision(),
                            root,
                            count: 2,
                            source: record.source.identity.into(),
                            destination: record.destination.identity.into(),
                            initial_review: false,
                        },
                        RecoveryChecks {
                            mode: VerificationMode::NativeLive(&payer),
                            ..RecoveryChecks::default()
                        },
                        BaselineAppendCheck::Host(&probe)
                    ),
                    Err(StoreError::RecoveryRequired)
                ));
                assert_eq!(probe.calls.get(), 1);
                assert_eq!(store.load_baseline(record.id).unwrap(), prior);
                assert_eq!(
                    store
                        .selected_attention(
                            record.id,
                            &format::NativePath::from(proposal.path.clone())
                        )
                        .unwrap(),
                    prior_attention
                );
                store
                    .append_native(
                        Change::BaselineCommit { directory_chain: None,
                            directory_ancestors: None,
                            existing_ancestors: None,
                            connection: record.id.0,
                            expected: equality.revision(),
                            root,
                            count: 2,
                            source: record.source.identity.into(),
                            destination: record.destination.identity.into(),
                            initial_review: false,
                        },
                        &payer,
                    )
                    .unwrap();
                assert!(
                    store
                        .selected_attention(
                            record.id,
                            &format::NativePath::from(proposal.path.clone())
                        )
                        .unwrap()
                        .is_empty(),
                    "fresh selected equality must clear only its path barrier with the baseline patch"
                );
            }
        }
        let operation = store.operation(live.id()).unwrap();
        store.checkpoint().unwrap();
        drop(store);
        let store = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(store.operation(live.id()).unwrap(), operation);
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn native_reviewed_restore_selection_refuses_same_revision_reopen_and_fence() {
    for fence in [false, true] {
        let (_temp, path) = fixture();
        let mut store = ConnectionStore::open_at(&path).unwrap();
        let record = create(&mut store, 924);
        let mut prior = intent(&record);
        prior.destination_before = prior.source_before.clone();
        let ack = store.begin_intent(prior).unwrap();
        let reservation = store
            .reserve_recovery(
                &ack,
                Side::Destination,
                RelativePath::unix(b"file").unwrap(),
                RecoveryReason::DisplacedVersion,
            )
            .unwrap();
        let original = path.parent().unwrap().join("selected-original");
        std::fs::write(&original, b"old").unwrap();
        let preserved = store
            .preserve_file(
                reservation,
                &mut File::open(&original).unwrap(),
                &AtomicBool::new(false),
            )
            .unwrap();
        let selected = store
            .recovery_page(record.id, None, 8)
            .unwrap()
            .entries
            .remove(0);
        assert_eq!(selected.id, preserved.value.id);
        for verified in [false, true] {
            let expected = store.load_connection(record.id).unwrap().revision;
            let change = if verified {
                Change::Verified {
                    connection: record.id.0,
                    serial: ack.value.serial,
                    expected,
                    source: record.source.identity.into(),
                    destination: record.destination.identity.into(),
                }
            } else {
                Change::Published {
                    connection: record.id.0,
                    serial: ack.value.serial,
                    expected,
                    source: record.source.identity.into(),
                    destination: record.destination.identity.into(),
                }
            };
            store.append(change).unwrap();
        }
        let expected = store.load_connection(record.id).unwrap().revision;
        let root = storage::build_pages(
            &store.storage,
            std::iter::once(Ok(Row::Baseline(format::BaselineEntry {
                connection: record.id.0,
                revision: expected + 1,
                ordinal: 0,
                path: format::NativePath::Unix(b"file".to_vec()),
                state: intent(&record).source_before.unwrap().into(),
            }))),
        )
        .unwrap();
        store
            .append(Change::BaselineCommit { directory_chain: None,
                directory_ancestors: None,
                existing_ancestors: None,
                connection: record.id.0,
                expected,
                root,
                count: 1,
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
                initial_review: true,
            })
            .unwrap();
        let current = store.load_connection(record.id).unwrap();
        let mut proposal = intent(&current);
        proposal.expected_baseline = current.baseline_revision;
        // Keep an actual equal observation. It must not become VerifyEqual execution authority.
        proposal.destination_before = proposal.source_before.clone();
        proposal.decision = Decision::VerifyEqual;
        let reviewed = store
            .restore_review_basis(proposal, selected.id, Side::Destination, None)
            .unwrap()
            .accept_replacement();
        let before = store.load_connection(record.id).unwrap();
        if fence {
            let session = store.transaction_session(record.id).unwrap();
            store
                .request_disconnect(record.id, before.revision)
                .unwrap();
            let log = std::fs::read(log_path(&path)).unwrap();
            assert!(
                store
                    .begin_native_selected_for_test(reviewed, &session)
                    .is_err()
            );
            assert_eq!(std::fs::read(log_path(&path)).unwrap(), log);
            assert!(session.require_idle().is_err());
        } else {
            store.checkpoint().unwrap();
            drop(preserved);
            drop(store);
            let mut reopened = ConnectionStore::open_at(&path).unwrap();
            assert_eq!(
                reopened.load_connection(record.id).unwrap(),
                before,
                "only the owning session changed"
            );
            let session = reopened.transaction_session(record.id).unwrap();
            let current_log = path.join(format!("log-{}", reopened.current.log));
            let log = std::fs::read(&current_log).unwrap();
            assert!(
                reopened
                    .begin_native_selected_for_test(reviewed, &session)
                    .is_err()
            );
            assert_eq!(std::fs::read(current_log).unwrap(), log);
        }
        assert_eq!(
            std::fs::read(
                path.join("recovery")
                    .join(format::hex(&record.id.0))
                    .join("1-0")
            )
            .unwrap(),
            b"old"
        );
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn native_selected_reader_reopens_with_paid_inspection_only() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 924);
    let mut prior = intent(&record);
    prior.destination_before = prior.source_before.clone();
    let ack = store.begin_intent(prior).unwrap();
    let reservation = store
        .reserve_recovery(
            &ack,
            Side::Destination,
            RelativePath::unix(b"file").unwrap(),
            RecoveryReason::DisplacedVersion,
        )
        .unwrap();
    let original = path.parent().unwrap().join("selected-original");
    std::fs::write(&original, b"old").unwrap();
    let preserved = store
        .preserve_file(
            reservation,
            &mut File::open(&original).unwrap(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let selected = store
        .recovery_page(record.id, None, 8)
        .unwrap()
        .entries
        .remove(0);
    assert_eq!(selected.id, preserved.value.id);
    for verified in [false, true] {
        let expected = store.load_connection(record.id).unwrap().revision;
        let change = if verified {
            Change::Verified {
                connection: record.id.0,
                serial: ack.value.serial,
                expected,
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
            }
        } else {
            Change::Published {
                connection: record.id.0,
                serial: ack.value.serial,
                expected,
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
            }
        };
        store.append(change).unwrap();
    }
    let expected = store.load_connection(record.id).unwrap().revision;
    let root = storage::build_pages(
        &store.storage,
        std::iter::once(Ok(Row::Baseline(format::BaselineEntry {
            connection: record.id.0,
            revision: expected + 1,
            ordinal: 0,
            path: format::NativePath::Unix(b"file".to_vec()),
            state: intent(&record).source_before.unwrap().into(),
        }))),
    )
    .unwrap();
    store
        .append(Change::BaselineCommit { directory_chain: None,
            directory_ancestors: None,
            existing_ancestors: None,
            connection: record.id.0,
            expected,
            root,
            count: 1,
            source: record.source.identity.into(),
            destination: record.destination.identity.into(),
            initial_review: true,
        })
        .unwrap();
    let current = store.load_connection(record.id).unwrap();
    let mut proposal = intent(&current);
    proposal.expected_baseline = current.baseline_revision;
    // Keep an actual equal observation. It must not become VerifyEqual execution authority.
    proposal.destination_before = proposal.source_before.clone();
    proposal.decision = Decision::VerifyEqual;
    let reviewed = store
        .restore_review_basis(proposal, selected.id, Side::Destination, None)
        .unwrap()
        .accept_replacement();

    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_selected_for_test(reviewed, &session)
        .unwrap();
    store
        .reserve_native_reads(&mut live, &session, 4 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    let reader = store
        .open_verified_recovery(&live, deadline, &cancel)
        .unwrap();
    let binding = reader.binding().unwrap();
    drop(reader);
    drop(preserved);
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let current = store.load_connection(record.id).unwrap();
    let mut context = store
        .open_recovery_context(live.id(), current.revision)
        .unwrap();
    assert!(
        store
            .open_selected_recovery_reader(&context, deadline, &cancel)
            .is_err(),
        "reopening burns unspent old credit"
    );
    store
        .reserve_recovery_reads(&mut context, 4 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    let mut reader = store
        .open_selected_recovery_reader(&context, deadline, &cancel)
        .expect("fresh paid selected private bytes can be inspected without minting LiveOperation");
    assert_eq!(reader.binding().unwrap(), binding);
    assert_eq!(
        store
            .operation(live.id())
            .unwrap()
            .native_budget
            .unwrap()
            .reserved,
        8 * 1024 * 1024
    );
    assert!(store.check_native(&live).is_err());
    assert_eq!(
        store.connection(record.id).unwrap().pending,
        Some(live.id().serial)
    );
    store
        .reserve_recovery_reads(&mut context, 4 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    assert!(
        reader.validate_recovery(&store, &context).is_err(),
        "new durable revision invalidates the retained old inspection"
    );
    drop(reader);
    let mut reader = store
        .open_selected_recovery_reader(&context, deadline, &cancel)
        .unwrap();
    cancel.store(true, std::sync::atomic::Ordering::Release);
    assert!(reader.validate_recovery(&store, &context).is_err());
    cancel.store(false, std::sync::atomic::Ordering::Release);
    assert!(
        reader.validate_recovery(&store, &context).is_err(),
        "cancelled inspection never reopens its failed latch"
    );
    drop(reader);
    std::fs::write(
        path.join("recovery")
            .join(format::hex(&record.id.0))
            .join("1-0"),
        b"bad",
    )
    .unwrap();
    assert!(
        store
            .open_selected_recovery_reader(&context, deadline, &cancel)
            .is_err()
    );
}

#[test]
fn native_selected_collision_snapshot_keeps_truthful_history_and_private_choice() {
    selected_collision_case(false);
}

#[test]
fn native_selected_collision_stopped_drain_retains_choice_and_pending() {
    selected_collision_case(true);
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn selected_collision_case(stopped: bool) {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 924);
    let mut prior = intent(&record);
    prior.destination_before = prior.source_before.clone();
    let ack = store.begin_intent(prior).unwrap();
    let reservation = store
        .reserve_recovery(
            &ack,
            Side::Destination,
            RelativePath::unix(b"file").unwrap(),
            RecoveryReason::DisplacedVersion,
        )
        .unwrap();
    let original = path.parent().unwrap().join("selected-original");
    std::fs::write(&original, b"old").unwrap();
    let preserved = store
        .preserve_file(
            reservation,
            &mut File::open(&original).unwrap(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let selected = store
        .recovery_page(record.id, None, 8)
        .unwrap()
        .entries
        .remove(0);
    assert_eq!(selected.id, preserved.value.id);
    for verified in [false, true] {
        let expected = store.load_connection(record.id).unwrap().revision;
        let change = if verified {
            Change::Verified {
                connection: record.id.0,
                serial: ack.value.serial,
                expected,
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
            }
        } else {
            Change::Published {
                connection: record.id.0,
                serial: ack.value.serial,
                expected,
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
            }
        };
        store.append(change).unwrap();
    }
    let expected = store.load_connection(record.id).unwrap().revision;
    let root = storage::build_pages(
        &store.storage,
        std::iter::once(Ok(Row::Baseline(format::BaselineEntry {
            connection: record.id.0,
            revision: expected + 1,
            ordinal: 0,
            path: format::NativePath::Unix(b"file".to_vec()),
            state: intent(&record).source_before.unwrap().into(),
        }))),
    )
    .unwrap();
    store
        .append(Change::BaselineCommit { directory_chain: None,
            directory_ancestors: None,
            existing_ancestors: None,
            connection: record.id.0,
            expected,
            root,
            count: 1,
            source: record.source.identity.into(),
            destination: record.destination.identity.into(),
            initial_review: true,
        })
        .unwrap();
    if stopped {
        store.checkpoint().unwrap();
    }
    let current = store.load_connection(record.id).unwrap();
    let mut proposal = intent(&current);
    proposal.expected_baseline = current.baseline_revision;
    // Keep an actual equal observation. It must not become VerifyEqual execution authority.
    proposal.destination_before = proposal.source_before.clone();
    proposal.decision = Decision::VerifyEqual;
    let reviewed = store
        .restore_review_basis(proposal, selected.id, Side::Destination, None)
        .unwrap()
        .accept();

    use super::super::transaction::native::descriptor as d;
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_selected_for_test(reviewed, &session)
        .unwrap();
    store
        .reserve_native_reads(&mut live, &session, 16 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    let operation = store.operation(live.id()).unwrap();
    let mut facts = native_admission_facts(&operation, live.fence());
    for state in &mut facts.states {
        if let d::State::Regular { content, .. } = state {
            content.digest = sha2::Sha256::digest(b"abc").into();
        }
    }
    let snapshot = d::ConflictSnapshot {
        connection: record.id.0,
        operation: live.id().serial,
        admission: live.admission(),
        revision: live.revision(),
        fence: live.fence(),
        baseline: operation.baseline,
        endpoints: facts.endpoints,
        path: facts.path,
        chains: facts.chains,
        versions: (0..2)
            .map(|side| d::CapturedVersion {
                side,
                role: 0,
                state: facts.states[usize::from(side)].clone(),
                displaced: None,
            })
            .collect(),
    };
    let snapshot = snapshot.encode().unwrap();
    let event: Change = serde_json::from_value(serde_json::json!({ "event": "NativeSelectedCollision", "snapshot": snapshot, "expected": live.revision() }))
        .expect("default occupied restore requires distinct selected collision accounting");
    let paid = store.native_read_context(&live, deadline, &cancel).unwrap();
    let selected_path = path
        .join("recovery")
        .join(format::hex(&record.id.0))
        .join("1-0");
    std::fs::write(&selected_path, b"bad").unwrap();
    assert!(
        store.append_native(event.clone(), &paid).is_err(),
        "collision capture must freshly check selected private bytes"
    );
    std::fs::write(&selected_path, b"old").unwrap();
    store
        .append_native(event, &paid)
        .expect("fresh selected collision retains history and actual at-name facts");
    let collision = store.operation(live.id()).unwrap();
    assert_eq!(collision.decision, operation.decision);
    assert_eq!(collision.action, operation.action);
    assert_eq!(collision.stage, 0);
    assert!(format!("{:?}", collision.decode().unwrap().action).starts_with("SelectedCollision"));
    let Some(Row::Conflict(conflict)) = store
        .row(&format::key(3, record.id.0, live.id().serial))
        .unwrap()
    else {
        panic!("collision");
    };
    assert_eq!(conflict.kind, 3);
    assert!(!conflict.resolved);
    assert_eq!(
        store.connection(record.id).unwrap().pending,
        Some(live.id().serial)
    );
    let snapshot = d::ConflictSnapshot::decode(collision.native_capture.as_ref().unwrap()).unwrap();
    live.revision = collision.revision;
    live.action = collision.decode().unwrap().action;
    if stopped {
        selected_collision_drain_case(store, &live, &snapshot, &path);
        return;
    }
    let actual = path.parent().unwrap().join("actual-collision-version");
    std::fs::write(&actual, b"abc").unwrap();
    for version in 0..2 {
        let reservation = store
            .reserve_capture_slot(&mut live, &snapshot, version)
            .expect("dedicated collision action must preserve its exact captured current versions");
        let paid = store.native_read_context(&live, deadline, &cancel).unwrap();
        let copy = store
            .preserve_file_with_context(
                reservation,
                &mut File::open(&actual).unwrap(),
                &cancel,
                Some(&paid),
            )
            .unwrap();
        live.revision += 1;
        let Some(Row::Recovery(entry)) = store.row(&recovery::recovery_key(copy.value.id)).unwrap()
        else {
            panic!("copy");
        };
        let captured = &snapshot.versions[usize::from(version)];
        let d::State::Regular {
            object, content, ..
        } = &captured.state
        else {
            panic!("file");
        };
        let fact = d::RecoveryFact {
            intent: snapshot.binding_digest().unwrap(),
            recovery: d::Recovery {
                connection: record.id.0,
                operation: live.id().serial,
                slot: entry.slot,
            },
            side: captured.side,
            role: 1,
            endpoint: snapshot.endpoints[usize::from(captured.side)],
            object: object.clone(),
            content: content.clone(),
            artifact: recovery::artifact_view(entry.object.as_ref().unwrap()),
        };
        let bound = format::NativeRecoveryFact {
            captured: Some(snapshot.encode().unwrap()),
            connection: record.id.0,
            serial: live.id().serial,
            slot: entry.slot,
            attempt: 0,
            fact: fact.encode().unwrap(),
            displaced: Vec::new(),
        };
        store.append_native(Change::NativeBound { fact: bound, expected: live.revision(), fence: live.fence() }, &paid)
            .expect("collision current version must join exact native origin and independently verified private bytes");
        live.revision += 1;
    }
    let collision = store.operation(live.id()).unwrap();
    assert_eq!(collision.stage, 1);
    let baseline = store.load_baseline(record.id).unwrap();
    let Some(Row::Conflict(conflict)) = store
        .row(&format::key(3, record.id.0, live.id().serial))
        .unwrap()
    else {
        panic!("collision");
    };
    let complete: Change = serde_json::from_value(serde_json::json!({ "event": "SelectedCollisionComplete", "connection": record.id.0,
        "serial": live.id().serial, "expected": live.revision(), "fence": live.fence(), "conflict_expected": conflict.revision,
        "capture": snapshot.binding_digest().unwrap() })).expect("selected collision needs separate durable completion");
    let paid = store.native_read_context(&live, deadline, &cancel).unwrap();
    store
        .append_native(complete, &paid)
        .expect("accounted selected collision releases pending with all choices pinned");
    assert_eq!(store.connection(record.id).unwrap().pending, None);
    assert_eq!(store.load_baseline(record.id).unwrap(), baseline);
    let collision = store.operation(live.id()).unwrap();
    assert_eq!(collision.stage, 10);
    let choices = store
        .conflict_record(ConflictId {
            connection: record.id,
            serial: live.id().serial,
        })
        .unwrap();
    assert!(
        choices.preserved.contains(&selected.id),
        "collision review must expose the exact historical selected choice"
    );
    assert_eq!(choices.preserved.len(), 3);
    let accounted_revision = store.load_connection(record.id).unwrap().revision;
    let accounted_entries = store.recovery_page(record.id, None, 8).unwrap().entries;
    for _ in 0..2 {
        assert_eq!(
            store.selected_collision_accounting(live.id()).unwrap(),
            Some(choices.clone()),
            "lost completion acknowledgement replays historical accounting without a new operation"
        );
        assert_eq!(
            store.load_connection(record.id).unwrap().revision,
            accounted_revision
        );
        assert_eq!(
            store.recovery_page(record.id, None, 8).unwrap().entries,
            accounted_entries
        );
    }
    let current = store.load_connection(record.id).unwrap();
    let mut ordinary = intent(&current);
    ordinary.expected_baseline = current.baseline_revision;
    ordinary.source_before = Some(EntryState::File {
        sha256: sha2::Sha256::digest(b"abc").into(),
        bytes: 3,
        executable: false,
    });
    ordinary.destination_before = ordinary.source_before.clone();
    ordinary.decision = Decision::VerifyEqual;
    let mut proposed = format::Operation::new(
        ordinary.clone(),
        current.revision,
        &store.connection(record.id).unwrap(),
    )
    .unwrap();
    proposed.serial = store.connection(record.id).unwrap().serial + 1;
    proposed.native_budget = Some(format::NativeBudget::initial());
    assert!(
        store.reduce_intent(&proposed).is_err(),
        "current at-name equality must not silently discard an unresolved chosen historical version"
    );
    let conflict_id = choices.id;
    let reviewed = store.restore_review_basis(ordinary.clone(), selected.id, Side::Destination, Some((conflict_id, choices.revision)))
        .expect("a new explicit review may choose only the collision's exactly joined historical version").accept_replacement();
    let (reviewed_intent, action) = store.consume_reviewed_selection(reviewed).unwrap();
    let mut resolved = format::Operation::new(
        reviewed_intent,
        proposed.serial,
        &store.connection(record.id).unwrap(),
    )
    .unwrap();
    resolved.native_budget = Some(format::NativeBudget::initial());
    resolved.action = Some(action);
    assert!(
        store.reduce_intent(&resolved).is_ok(),
        "durable Intent must enforce the same exact selected collision join as preview"
    );
    assert!(
        store
            .restore_review_basis(
                ordinary.clone(),
                selected.id,
                Side::Destination,
                Some((conflict_id, choices.revision - 1))
            )
            .is_err()
    );
    let Some(Row::Conflict(current_conflict)) = store
        .row(&format::key(3, record.id.0, conflict_id.serial))
        .unwrap()
    else {
        panic!("conflict");
    };
    for changed in 0..3 {
        let mut wrong = current_conflict.clone();
        match changed {
            0 => wrong.revision += 1,
            1 => wrong.path = format::NativePath::Unix(b"other".to_vec()),
            _ => wrong.native_capture = None,
        }
        assert!(!matches!(
            store.conflict_contains_selection(&wrong, selected.id),
            Ok(true)
        ));
    }
    let Some(Row::Recovery(mut same_slot)) =
        store.row(&recovery::recovery_key(selected.id)).unwrap()
    else {
        panic!("selected");
    };
    same_slot.serial = proposed.serial;
    let unrelated = same_slot.id();
    let unrelated_key = same_slot.key();
    store
        .overlay
        .insert(unrelated_key.clone(), Row::Recovery(same_slot));
    assert!(
        store
            .restore_review_basis(
                ordinary.clone(),
                unrelated,
                Side::Destination,
                Some((conflict_id, choices.revision))
            )
            .is_err(),
        "matching slot/path/content from another operation is not the canonical selected choice"
    );
    store.overlay.remove(&unrelated_key);
    let collision_key = format::key(1, collision.connection, collision.serial);
    let mut stale_action = collision.clone();
    let Some(format::SelectedAction::Restore { replace, .. }) = &mut stale_action.action else {
        panic!("selected action");
    };
    *replace = true;
    let previous_row = store
        .overlay
        .insert(collision_key.clone(), Row::Operation(stale_action));
    assert!(
        store.conflict_record(conflict_id).is_err(),
        "changed action cannot project a selected collision choice"
    );
    if let Some(row) = previous_row {
        store.overlay.insert(collision_key, row);
    } else {
        store.overlay.remove(&collision_key);
    }
    ordinary.path = RelativePath::unix(b"independent").unwrap();
    let mut independent = format::Operation::new(
        ordinary,
        proposed.serial,
        &store.connection(record.id).unwrap(),
    )
    .unwrap();
    independent.native_budget = Some(format::NativeBudget::initial());
    assert!(
        store.reduce_intent(&independent).is_ok(),
        "selected collision attention is scoped to its path"
    );
    let final_context = store
        .open_recovery_context(live.id(), current.revision)
        .unwrap();
    assert!(store.check_final_context(&final_context).is_err());
    store.retention_sample = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(60))
        .unwrap();
    store.checkpoint().unwrap();
    assert_eq!(
        store
            .recovery_page(record.id, None, 8)
            .unwrap()
            .entries
            .iter()
            .find(|entry| entry.id == selected.id)
            .unwrap()
            .retained_seconds,
        0,
        "historical selected choice stays pinned after pending clears"
    );
    drop(preserved);
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(store.operation(live.id()).unwrap(), collision);
    assert_eq!(store.conflict_record(conflict_id).unwrap(), choices);
    assert_eq!(
        store.selected_collision_accounting(live.id()).unwrap(),
        Some(choices)
    );
    assert_eq!(std::fs::read(selected_path).unwrap(), b"old");
}

#[test]
fn selected_equality_conflict_exception_requires_exact_choice_and_revision() {
    // Historical metadata predicate only, never a comparison/native proof.
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 926);
    let connection = store.connection(record.id).unwrap();
    let mut applied = format::Operation::new(intent(&record), 2, &connection).unwrap();
    applied.native_budget = Some(format::NativeBudget::initial());
    let content = applied.source.clone().unwrap();
    applied.action = Some(format::SelectedAction::Restore {
        version: 1,
        target: 1,
        replace: false,
        revision: applied.expected,
        fence: connection.fence,
        selected: format::SelectedVersion::Recovery {
            operation: 1,
            slot: 0,
            content: content.clone(),
        },
        conflict: Some(format::SelectedConflict {
            serial: 1,
            revision: 2,
        }),
    });
    applied.stage = 9;
    applied.revision += 1;
    applied.selected_applied = Some(format::SelectedAppliedFact {
        revision: applied.revision,
        fence: connection.fence,
        outcome: [1; 32],
        attention: true,
    });
    let mut comparison = applied.clone();
    comparison.serial = 3;
    comparison.expected = applied.revision;
    comparison.revision = applied.revision + 1;
    comparison.action = None;
    comparison.selected_applied = None;
    comparison.stage = 3;
    comparison.decision = 0;
    comparison.source = Some(content.clone());
    comparison.destination = Some(content);
    let conflict = format::ConflictData {
        connection: record.id.0,
        serial: 1,
        revision: 2,
        kind: 0,
        path: applied.path.clone(),
        source: applied.source.clone(),
        destination: applied.destination.clone(),
        native_capture: None,
        preserved: vec![0],
        resolved: false,
    };
    applied.decode().unwrap();
    comparison.decode().unwrap();
    conflict.decode().unwrap();
    assert!(
        selected_equality_conflict_matches(&comparison, &applied, &conflict),
        "exact chosen equality must admit the bound conflict to the later paid candidate reduction"
    );
    for changed in 0..7 {
        let mut comparison = comparison.clone();
        let mut applied = applied.clone();
        let mut conflict = conflict.clone();
        match changed {
            0 => conflict.serial += 1,
            1 => conflict.revision += 1,
            2 => conflict.path = format::NativePath::Unix(b"other".to_vec()),
            3 => applied.selected_applied.as_mut().unwrap().attention = false,
            4 => comparison.destination = None,
            5 => {
                comparison.source = Some(format::State::File {
                    sha256: [9; 32],
                    bytes: 3,
                    executable: false,
                });
                comparison.destination = comparison.source.clone();
            }
            _ => comparison.action = applied.action.clone(),
        }
        assert!(!selected_equality_conflict_matches(
            &comparison,
            &applied,
            &conflict
        ));
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn account_selected_boundary_case(
    mut store: ConnectionStore,
    live: &LiveOperation,
    registration: &format::NativeRegistration,
    kind: u8,
    path: &std::path::Path,
) {
    let baseline = store.load_baseline(live.id().connection).unwrap();
    let current = store.load_connection(live.id().connection).unwrap();
    let fence = store
        .request_disconnect(current.id, current.revision)
        .unwrap();
    let current = store.load_connection(current.id).unwrap();
    let mut context = store
        .open_recovery_context(live.id(), current.revision)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    store
        .reserve_recovery_reads(&mut context, 4 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    let kind = if kind == 4 {
        let operation = store.operation(live.id()).unwrap();
        let entry = format::Recovery {
            captured: None,
            connection: current.id.0,
            serial: live.id().serial,
            slot: u32::try_from(operation.recovery.len()).unwrap(),
            side: registration.side,
            path: operation.path.clone(),
            reason: 1,
            state: 0,
            content: None,
            object: None,
            retained: 0,
        };
        let source = format::BoundaryEvidence {
            attempt: registration.attempt,
            displaced: registration.outcome_displaced.clone(),
            outcome: None,
        };
        assert!(
            store
                .append(Change::NativeDrain {
                    entry: entry.clone(),
                    source: source.clone(),
                    fact: None,
                    expected: context.revision(),
                    operation_expected: context.operation_revision,
                    fence: context.fence()
                })
                .is_err(),
            "ordinary drain cannot reclassify selected history"
        );
        let event: Change = serde_json::from_value(serde_json::json!({ "event": "SelectedDrain", "entry": entry,
            "source": source, "fact": null, "expected": context.revision(), "operation_expected": context.operation_revision,
            "fence": context.fence() })).expect("selected pre-fence displacement requires a dedicated drain transition");
        let paid = store
            .recovery_read_context(&context, deadline, &cancel)
            .unwrap();
        assert!(
            store.append(event.clone()).is_err(),
            "selected drain reservation cannot bypass canonical paid verification"
        );
        store
            .append_native(event, &paid)
            .expect("selected old displacement may reserve private preservation after stop");
        context.revision += 1;
        context.operation_revision = context.revision;
        assert_eq!(
            store.row(&entry.key()).unwrap(),
            Some(Row::Recovery(entry.clone()))
        );
        use super::super::transaction::native::descriptor as d;
        let directory_name = format::hex(&current.id.0);
        let metadata = super::super::super::nofollow::metadata_nofollow(
            &store.storage.recovery.file,
            std::path::Path::new(&directory_name),
        )
        .unwrap();
        let directory = store
            .storage
            .recovery
            .open_child(std::ffi::OsStr::new(&directory_name), &metadata)
            .unwrap();
        let name = format!("{}-{}", entry.serial, entry.slot);
        let mut writer = storage::open_file(&directory, &name, true).unwrap();
        std::io::Write::write_all(&mut writer, b"new").unwrap();
        storage::sync_file(&writer).unwrap();
        let metadata = storage::regular(&writer).unwrap();
        let artifact = format::ObjectIdentity {
            root: store.storage.identity.clone(),
            file_id: storage::file_identity(&metadata),
        };
        let original = d::decode(registration.intent.as_ref().unwrap()).unwrap();
        let d::Body::Intent(body) = &original.body else {
            panic!("intent");
        };
        let d::State::Regular {
            object, content, ..
        } = &body.expected[usize::from(entry.side)]
        else {
            panic!("displacement");
        };
        let fact = d::RecoveryFact {
            intent: original.digest,
            recovery: d::Recovery {
                connection: entry.connection,
                operation: entry.serial,
                slot: entry.slot,
            },
            side: entry.side,
            role: 0,
            endpoint: body.endpoints[usize::from(entry.side)],
            object: object.clone(),
            content: content.clone(),
            artifact: recovery::artifact_view(&artifact),
        };
        let mut entry = entry;
        entry.state = 1;
        entry.object = Some(artifact);
        entry.content = Some(format::State::File {
            sha256: content.digest,
            bytes: content.length,
            executable: content.executable,
        });
        let bound = format::NativeRecoveryFact {
            captured: None,
            connection: entry.connection,
            serial: entry.serial,
            slot: entry.slot,
            attempt: registration.attempt,
            fact: fact.encode().unwrap(),
            displaced: source.displaced.clone().unwrap(),
        };
        let event = Change::SelectedDrain {
            entry: entry.clone(),
            source,
            fact: Some(bound.clone()),
            expected: context.revision(),
            operation_expected: context.operation_revision,
            fence: context.fence(),
        };
        let paid = store
            .recovery_read_context(&context, deadline, &cancel)
            .unwrap();
        let mut wrong = event.clone();
        let Change::SelectedDrain {
            fact: Some(wrong), ..
        } = &mut wrong
        else {
            panic!("bound");
        };
        let mut origin = fact;
        origin.object.generation[0] ^= 1;
        wrong.fact = origin.encode().unwrap();
        let mut wrong_event = event.clone();
        let Change::SelectedDrain { fact, .. } = &mut wrong_event else {
            panic!("drain");
        };
        *fact = Some(wrong.clone());
        assert!(store.append_native(wrong_event, &paid).is_err());
        std::io::Seek::seek(&mut writer, std::io::SeekFrom::Start(0)).unwrap();
        std::io::Write::write_all(&mut writer, b"bad").unwrap();
        storage::sync_file(&writer).unwrap();
        assert!(store.append_native(event.clone(), &paid).is_err());
        assert_eq!(
            store
                .recovery_page(current.id, None, 16)
                .unwrap()
                .entries
                .iter()
                .find(|value| value.id == entry.id())
                .unwrap()
                .state,
            RecoveryState::Reserved
        );
        assert_eq!(
            std::fs::read(
                path.join("recovery")
                    .join(format::hex(&current.id.0))
                    .join(&name)
            )
            .unwrap(),
            b"bad"
        );
        std::io::Seek::seek(&mut writer, std::io::SeekFrom::Start(0)).unwrap();
        std::io::Write::write_all(&mut writer, b"new").unwrap();
        storage::sync_file(&writer).unwrap();
        store
            .append_native(event, &paid)
            .expect("fresh paid private bytes must bind the exact stopped selected displacement");
        context.revision += 1;
        context.operation_revision = context.revision;
        assert_eq!(
            store.row(&bound.key()).unwrap(),
            Some(Row::NativeRecoveryFact(bound))
        );
        assert_eq!(
            std::fs::read(
                path.join("recovery")
                    .join(format::hex(&current.id.0))
                    .join(&name)
            )
            .unwrap(),
            b"new"
        );
        assert_eq!(store.operation(live.id()).unwrap().stage, operation.stage);
        assert!(store.connection(current.id).unwrap().pending.is_some());
        assert!(store.check_native(live).is_err());
        2
    } else {
        kind
    };
    let event: Change = serde_json::from_value(serde_json::json!({ "event": "SelectedBoundary", "connection": current.id.0,
        "serial": live.id().serial, "expected": context.revision(), "operation_expected": context.operation_revision,
        "fence": context.fence(), "kind": kind, "evidence": { "attempt": registration.attempt,
            "displaced": registration.outcome_displaced, "outcome": registration.outcome } }))
        .expect("selected stopped work requires distinct safe-boundary accounting");
    let paid = store
        .recovery_read_context(&context, deadline, &cancel)
        .unwrap();
    let Change::SelectedBoundary { evidence, .. } = &event else {
        panic!("selected boundary");
    };
    assert!(
        store
            .append(Change::NativeBoundary {
                connection: current.id.0,
                serial: live.id().serial,
                expected: context.revision(),
                operation_expected: context.operation_revision,
                fence: context.fence(),
                kind,
                evidence: Some(evidence.clone())
            })
            .is_err(),
        "ordinary accounting cannot consume selected history"
    );
    let chosen = path
        .join("recovery")
        .join(format::hex(&current.id.0))
        .join("1-0");
    std::fs::write(&chosen, b"bad").unwrap();
    assert!(
        store.append_native(event.clone(), &paid).is_err(),
        "stopped selected accounting still requires current paid chosen bytes"
    );
    std::fs::write(&chosen, b"old").unwrap();
    store
        .append_native(event, &paid)
        .expect("owning stopped facts may account the exact reached boundary without publication");
    assert_eq!(store.operation(live.id()).unwrap().stage, 7);
    assert_eq!(
        store.operation(live.id()).unwrap().native_boundary,
        Some(kind)
    );
    assert!(
        store
            .operation(live.id())
            .unwrap()
            .selected_applied
            .is_none()
    );
    assert!(store.connection(current.id).unwrap().pending.is_none());
    assert!(store.check_native(live).is_err());
    assert_eq!(store.load_baseline(current.id).unwrap(), baseline);
    let current = store.load_connection(current.id).unwrap();
    store.finish_disconnect(&fence).unwrap();
    if kind == 2 {
        drop(store);
        std::fs::write(&chosen, b"bad").unwrap();
        let mut store = ConnectionStore::open_at(path)
            .expect("native-only replay records history without reauthorizing changed bytes");
        let current = store.load_connection(current.id).unwrap();
        let mut context = store
            .open_recovery_context(live.id(), current.revision)
            .unwrap();
        store
            .reserve_recovery_reads(&mut context, 4 * 1024 * 1024, deadline, &cancel)
            .unwrap();
        let paid = store
            .recovery_read_context(&context, deadline, &cancel)
            .unwrap();
        let mut checks = RecoveryChecks {
            mode: VerificationMode::NativeLive(&paid),
            ..RecoveryChecks::default()
        };
        assert!(
            checks
                .verify_stopped_selected(&store, &store.operation(live.id()).unwrap())
                .is_err()
        );
        assert_eq!(std::fs::read(chosen).unwrap(), b"bad");
        assert_eq!(store.load_baseline(current.id).unwrap(), baseline);
        assert_eq!(store.operation(live.id()).unwrap().stage, 7);
        assert_eq!(current.lifecycle, Lifecycle::Disconnected);
        return;
    }
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(path).unwrap();
    assert_eq!(
        store.load_connection(current.id).unwrap().lifecycle,
        Lifecycle::Disconnected
    );
    assert_eq!(store.operation(live.id()).unwrap().stage, 7);
    assert_eq!(store.load_baseline(current.id).unwrap(), baseline);
    assert_eq!(std::fs::read(chosen).unwrap(), b"old");
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn selected_collision_drain_case(
    mut store: ConnectionStore,
    live: &LiveOperation,
    snapshot: &super::super::transaction::native::descriptor::ConflictSnapshot,
    path: &std::path::Path,
) {
    use super::super::transaction::native::descriptor as d;
    let id = live.id();
    let current = store.load_connection(id.connection).unwrap();
    let baseline = store.load_baseline(id.connection).unwrap();
    let fence = store
        .request_disconnect(id.connection, current.revision)
        .unwrap();
    let current = store.load_connection(id.connection).unwrap();
    let mut context = store.open_recovery_context(id, current.revision).unwrap();
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    store
        .reserve_recovery_reads(&mut context, 16 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    let encoded = snapshot.encode().unwrap();
    for index in 0..2 {
        let captured = &snapshot.versions[index];
        let entry = format::Recovery {
            captured: Some(format::CapturedSlot {
                snapshot: encoded.clone(),
                version: index as u8,
            }),
            connection: id.connection.0,
            serial: id.serial,
            slot: index as u32,
            side: captured.side,
            path: snapshot.path.clone().into(),
            reason: 2,
            state: 0,
            content: None,
            object: None,
            retained: 0,
        };
        assert!(
            store
                .append(Change::NativeCaptureDrain {
                    entry: entry.clone(),
                    expected: context.revision(),
                    operation_expected: context.operation_revision,
                    fence: context.fence(),
                    fact: None
                })
                .is_err()
        );
        let event: Change = serde_json::from_value(serde_json::json!({ "event": "SelectedCaptureDrain", "entry": entry,
            "expected": context.revision(), "operation_expected": context.operation_revision, "fence": context.fence(), "fact": null }))
            .expect("pre-fence selected collision requires dedicated paid stopped accounting");
        assert!(store.append(event.clone()).is_err());
        let paid = store
            .recovery_read_context(&context, deadline, &cancel)
            .unwrap();
        let chosen = path
            .join("recovery")
            .join(format::hex(&id.connection.0))
            .join("1-0");
        std::fs::write(&chosen, b"bad").unwrap();
        assert!(store.append_native(event.clone(), &paid).is_err());
        std::fs::write(&chosen, b"old").unwrap();
        for field in 0..4 {
            let mut changed = event.clone();
            let Change::SelectedCaptureDrain {
                entry,
                expected,
                operation_expected,
                fence,
                ..
            } = &mut changed
            else {
                panic!("drain");
            };
            match field {
                0 => entry.serial += 1,
                1 => *expected += 1,
                2 => *operation_expected += 1,
                _ => *fence += 1,
            }
            assert!(store.append_native(changed, &paid).is_err());
        }
        let key = format::key(1, id.connection.0, id.serial);
        let mut changed = store.operation(id).unwrap();
        changed.action = None;
        let previous = store.overlay.insert(key.clone(), Row::Operation(changed));
        assert!(
            store.append_native(event.clone(), &paid).is_err(),
            "ordinary capture cannot consume a selected drain event"
        );
        if let Some(row) = previous {
            store.overlay.insert(key.clone(), row);
        } else {
            store.overlay.remove(&key);
        }
        store
            .append_native(event, &paid)
            .expect("existing captured selected versions may reserve after the fence");
        context.revision += 1;
        context.operation_revision = context.revision;
        let directory_name = format::hex(&id.connection.0);
        let metadata = super::super::super::nofollow::metadata_nofollow(
            &store.storage.recovery.file,
            std::path::Path::new(&directory_name),
        )
        .unwrap();
        let directory = store
            .storage
            .recovery
            .open_child(std::ffi::OsStr::new(&directory_name), &metadata)
            .unwrap();
        let name = format!("{}-{}", id.serial, entry.slot);
        let mut file = storage::open_file(&directory, &name, true).unwrap();
        std::io::Write::write_all(&mut file, b"abc").unwrap();
        storage::sync_file(&file).unwrap();
        let artifact = format::ObjectIdentity {
            root: store.storage.identity.clone(),
            file_id: storage::file_identity(&storage::regular(&file).unwrap()),
        };
        let d::State::Regular {
            object, content, ..
        } = &captured.state
        else {
            panic!("version");
        };
        let bound = d::RecoveryFact {
            intent: snapshot.binding_digest().unwrap(),
            recovery: d::Recovery {
                connection: id.connection.0,
                operation: id.serial,
                slot: entry.slot,
            },
            side: captured.side,
            role: 1,
            endpoint: snapshot.endpoints[index],
            object: object.clone(),
            content: content.clone(),
            artifact: recovery::artifact_view(&artifact),
        };
        let fact = format::NativeRecoveryFact {
            captured: Some(encoded.clone()),
            connection: id.connection.0,
            serial: id.serial,
            slot: entry.slot,
            attempt: 0,
            displaced: Vec::new(),
            fact: bound.encode().unwrap(),
        };
        let mut entry = entry;
        entry.state = 1;
        entry.object = Some(artifact);
        entry.content = Some(format::State::File {
            sha256: content.digest,
            bytes: content.length,
            executable: content.executable,
        });
        let event: Change = serde_json::from_value(serde_json::json!({ "event": "SelectedCaptureDrain", "entry": entry,
            "expected": context.revision(), "operation_expected": context.operation_revision, "fence": context.fence(), "fact": fact })).unwrap();
        let paid = store
            .recovery_read_context(&context, deadline, &cancel)
            .unwrap();
        let mut wrong = event.clone();
        let Change::SelectedCaptureDrain {
            fact: Some(wrong_fact),
            ..
        } = &mut wrong
        else {
            panic!("bound");
        };
        let mut origin = bound.clone();
        origin.object.generation[0] ^= 1;
        wrong_fact.fact = origin.encode().unwrap();
        assert!(store.append_native(wrong, &paid).is_err());
        std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(0)).unwrap();
        std::io::Write::write_all(&mut file, b"bad").unwrap();
        storage::sync_file(&file).unwrap();
        assert!(store.append_native(event.clone(), &paid).is_err());
        std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(0)).unwrap();
        std::io::Write::write_all(&mut file, b"abc").unwrap();
        storage::sync_file(&file).unwrap();
        store.append_native(event, &paid).unwrap();
        context.revision += 1;
        context.operation_revision = context.revision;
        assert_eq!(
            store.row(&fact.key()).unwrap(),
            Some(Row::NativeRecoveryFact(fact))
        );
        assert_eq!(
            std::fs::read(path.join("recovery").join(&directory_name).join(name)).unwrap(),
            b"abc"
        );
    }
    assert!(store.finish_disconnect(&fence).is_err());
    assert_eq!(
        store.connection(id.connection).unwrap().pending,
        Some(id.serial)
    );
    assert_eq!(store.operation(id).unwrap().stage, 0);
    assert_eq!(store.load_baseline(id.connection).unwrap(), baseline);
    assert!(store.check_native(live).is_err());
    drop(store);
    let store = ConnectionStore::open_at(path).unwrap();
    assert_eq!(
        store.load_connection(id.connection).unwrap().lifecycle,
        Lifecycle::Disconnecting
    );
    assert_eq!(
        store.connection(id.connection).unwrap().pending,
        Some(id.serial)
    );
    assert_eq!(store.load_baseline(id.connection).unwrap(), baseline);
    assert_eq!(
        std::fs::read(
            path.join("recovery")
                .join(format::hex(&id.connection.0))
                .join("1-0")
        )
        .unwrap(),
        b"old"
    );
}

#[test]
fn native_selected_tombstone_schema_requires_positive_absence_binding() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 926);
    let connection = store.connection(record.id).unwrap();
    let mut operation = format::Operation::new(intent(&record), 2, &connection).unwrap();
    operation.native_budget = Some(format::NativeBudget::initial());
    let mut wire = serde_json::to_value(&operation).unwrap();
    wire["action"] = serde_json::json!({ "kind": "restore", "version": 1, "target": 1, "replace": true,
        "revision": operation.expected, "fence": connection.fence,
        "selected": { "kind": "tombstone", "operation": 1, "revision": 2, "side": 0, "snapshot": vec![7; 32] },
        "conflict": { "serial": 1, "revision": 2 } });
    let decoded = serde_json::from_value::<format::Operation>(wire.clone())
        .expect("a positively recorded tombstone is a typed historical choice");
    assert_eq!(
        decoded.decode().unwrap().intent.decision,
        operation.decode().unwrap().intent.decision
    );
    assert_eq!(serde_json::to_value(&decoded).unwrap(), wire);
    for field in 0..8 {
        let mut bad = wire.clone();
        match field {
            0 => bad["action"]["selected"]["side"] = serde_json::json!(2),
            1 => bad["action"]["selected"]["operation"] = serde_json::json!(2),
            2 => bad["action"]["selected"]["snapshot"] = serde_json::json!(vec![0; 32]),
            3 => bad["action"]["conflict"] = serde_json::Value::Null,
            4 => bad["action"]["conflict"]["serial"] = serde_json::json!(2),
            5 => bad["action"]["selected"]["kind"] = serde_json::json!("unavailable"),
            6 => bad["action"]["selected"]["revision"] = serde_json::json!(0),
            _ => bad["action"]["selected"]["revision"] = serde_json::json!(3),
        }
        assert!(
            serde_json::from_value::<format::Operation>(bad)
                .map_or(true, |value| value.decode().is_err())
        );
    }
    assert!(
        store.reduce_intent(&decoded).is_err(),
        "schema does not prove a matching recorded absence exists"
    );
}

#[test]
fn native_selected_tombstone_intent_requires_exact_retained_qnc_absence() {
    selected_tombstone_case(0);
}
#[test]
fn native_selected_tombstone_stopped_boundaries_keep_real_versions_without_application() {
    for kind in 1..=3 {
        selected_tombstone_case(kind);
    }
}
#[test]
fn native_selected_tombstone_stopped_drain_pays_for_real_displacement() {
    selected_tombstone_case(4);
}
#[test]
fn native_selected_tombstone_collision_retains_original_choice_for_later_review() {
    selected_tombstone_case(5);
}
#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn selected_tombstone_case(stopped: u8) {
    use super::super::transaction::native::descriptor as d;
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 927);
    let session = store.transaction_session(record.id).unwrap();
    let mut proposal = intent(&record);
    proposal.source_before = Some(EntryState::File {
        sha256: sha2::Sha256::digest(b"abc").into(),
        bytes: 3,
        executable: false,
    });
    let mut live = store.begin_native_for_test(proposal, &session).unwrap();
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    store
        .reserve_native_reads(&mut live, &session, 16 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    let operation = store.operation(live.id()).unwrap();
    let metadata = native_admission_facts(&operation, live.fence());
    let snapshot = d::ConflictSnapshot {
        connection: record.id.0,
        operation: live.id().serial,
        admission: live.admission(),
        revision: live.revision(),
        fence: live.fence(),
        baseline: operation.baseline,
        endpoints: metadata.endpoints,
        path: metadata.path,
        chains: metadata.chains,
        versions: vec![
            d::CapturedVersion {
                side: 0,
                role: 0,
                state: metadata.states[0].clone(),
                displaced: None,
            },
            d::CapturedVersion {
                side: 1,
                role: 0,
                state: d::State::Absent,
                displaced: None,
            },
        ],
    };
    let bytes = snapshot.encode().unwrap();
    store
        .append(Change::NativeCapture {
            snapshot: bytes.clone(),
            displaced: Vec::new(),
            expected: live.revision(),
            kind: 1,
        })
        .unwrap();
    live.revision += 1;
    live.action = ExecutionAction::Captured;
    let reservation = store.reserve_capture_slot(&mut live, &snapshot, 0).unwrap();
    let actual = path.parent().unwrap().join("surviving-version");
    std::fs::write(&actual, b"abc").unwrap();
    let paid = store.native_read_context(&live, deadline, &cancel).unwrap();
    let preserved = store
        .preserve_file_with_context(
            reservation,
            &mut File::open(&actual).unwrap(),
            &cancel,
            Some(&paid),
        )
        .unwrap();
    live.revision += 1;
    let Some(Row::Recovery(entry)) = store
        .row(&recovery::recovery_key(preserved.value.id))
        .unwrap()
    else {
        panic!("copy");
    };
    let d::State::Regular {
        object, content, ..
    } = &snapshot.versions[0].state
    else {
        panic!("regular");
    };
    let bound = d::RecoveryFact {
        intent: snapshot.binding_digest().unwrap(),
        recovery: d::Recovery {
            connection: record.id.0,
            operation: live.id().serial,
            slot: entry.slot,
        },
        side: 0,
        role: 1,
        endpoint: snapshot.endpoints[0],
        object: object.clone(),
        content: content.clone(),
        artifact: recovery::artifact_view(entry.object.as_ref().unwrap()),
    };
    let fact = format::NativeRecoveryFact {
        captured: Some(bytes),
        connection: record.id.0,
        serial: live.id().serial,
        slot: entry.slot,
        attempt: 0,
        fact: bound.encode().unwrap(),
        displaced: Vec::new(),
    };
    store
        .append_native(
            Change::NativeBound {
                fact,
                expected: live.revision(),
                fence: live.fence(),
            },
            &paid,
        )
        .unwrap();
    live.revision += 1;
    let conflict = store
        .conflict_record(ConflictId {
            connection: record.id,
            serial: live.id().serial,
        })
        .unwrap();
    let paid = store.native_read_context(&live, deadline, &cancel).unwrap();
    store
        .append_native(
            Change::NativeConflictComplete {
                connection: record.id.0,
                serial: live.id().serial,
                expected: live.revision(),
                fence: live.fence(),
                conflict_expected: conflict.revision,
                capture: snapshot.binding_digest().unwrap(),
            },
            &paid,
        )
        .unwrap();
    let conflict = store.conflict_record(conflict.id).unwrap();
    let current = store.load_connection(record.id).unwrap();
    let connection = store.connection(record.id).unwrap();
    let mut next_intent = intent(&current);
    next_intent.source_before = Some(EntryState::File {
        sha256: sha2::Sha256::digest(b"abc").into(),
        bytes: 3,
        executable: false,
    });
    let mut next = format::Operation::new(next_intent, connection.serial + 1, &connection).unwrap();
    next.native_budget = Some(format::NativeBudget::initial());
    next.action = Some(format::SelectedAction::Restore {
        version: 1,
        target: 0,
        replace: stopped != 5,
        revision: current.revision,
        fence: current.fence_generation,
        selected: format::SelectedVersion::Tombstone {
            operation: live.id().serial,
            revision: conflict.revision,
            side: 1,
            snapshot: snapshot.binding_digest().unwrap(),
        },
        conflict: Some(format::SelectedConflict {
            serial: conflict.id.serial,
            revision: conflict.revision,
        }),
    });
    for field in 0..6 {
        let mut wrong = next.clone();
        let Some(format::SelectedAction::Restore {
            selected,
            conflict: Some(conflict),
            ..
        }) = &mut wrong.action
        else {
            panic!("tombstone");
        };
        let format::SelectedVersion::Tombstone {
            side,
            snapshot,
            operation,
            ..
        } = selected
        else {
            panic!("tombstone");
        };
        match field {
            0 => *side = 0,
            1 => snapshot[0] ^= 1,
            2 => *operation += 1,
            3 => conflict.revision += 1,
            4 => wrong.path = format::NativePath::Unix(b"elsewhere".to_vec()),
            _ => wrong.destination_binding.root[0] ^= 1,
        }
        assert!(store.reduce_intent(&wrong).is_err());
    }
    let choices = store
        .tombstone_choices(conflict.id)
        .expect("a recorded absent side must be visible as a typed selectable version");
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].side(), Side::Destination);
    let preview = store
        .tombstone_review_basis(
            next.decode().unwrap().intent,
            choices[0].clone(),
            Side::Source,
            (conflict.id, conflict.revision),
        )
        .unwrap();
    let reviewed = if stopped == 5 {
        preview.accept()
    } else {
        preview.accept_replacement()
    };
    let old = store
        .tombstone_review_basis(
            next.decode().unwrap().intent,
            choices[0].clone(),
            Side::Source,
            (conflict.id, conflict.revision),
        )
        .unwrap()
        .accept_replacement();
    let (reviewed_intent, reviewed_action) = store.consume_reviewed_selection(reviewed).unwrap();
    assert_eq!(reviewed_intent, next.decode().unwrap().intent);
    assert_eq!(Some(reviewed_action.clone()), next.action);
    let mut selected_live = store
        .begin_native_operation(
            reviewed_intent,
            &session,
            None,
            None,
            Some(reviewed_action),
            None,
        )
        .expect("exact recorded absence must admit as a selected historical version");
    assert_eq!(store.operation(selected_live.id()).unwrap(), next);
    store
        .reserve_native_reads(
            &mut selected_live,
            &session,
            4 * 1024 * 1024,
            deadline,
            &cancel,
        )
        .unwrap();
    assert!(
        store
            .open_verified_recovery(&selected_live, deadline, &cancel)
            .is_err(),
        "recorded absence never supplies a fake recovery file"
    );
    let mut proof = store
        .open_verified_tombstone(&selected_live, deadline, &cancel)
        .expect("typed selected metadata must join the exact live operation");
    proof.validate(&store, &selected_live).unwrap();
    cancel.store(true, std::sync::atomic::Ordering::Release);
    assert!(proof.validate(&store, &selected_live).is_err());
    cancel.store(false, std::sync::atomic::Ordering::Release);
    assert!(
        proof.validate(&store, &selected_live).is_err(),
        "clearing cancellation cannot reactivate the selection proof"
    );
    drop(proof);
    if stopped == 5 {
        account_tombstone_collision(store, selected_live, &path, &actual, &choices[0], &conflict);
        return;
    }
    let namespace = store
        .reserve_native_namespace_with_nonce(&mut selected_live, Side::Source, [79; 16])
        .unwrap();
    let mut prepared = native_prepared_facts(&selected_live, &namespace);
    let mut descriptor = d::decode(prepared.intent.as_ref().unwrap()).unwrap();
    let d::Body::Intent(body) = &mut descriptor.body else {
        panic!("intent");
    };
    body.direction = 1;
    if let d::State::Regular { content, .. } = &mut body.expected[0] {
        content.digest = sha2::Sha256::digest(b"abc").into();
    }
    body.selection = store
        .selected_descriptor(
            &store.operation(selected_live.id()).unwrap(),
            next.action.as_ref().unwrap(),
        )
        .unwrap();
    prepared.intent = Some(d::encode(&descriptor.header, &descriptor.body).unwrap());
    prepared.stage = None;
    store
        .append(Change::NativePrepared {
            registration: prepared.clone(),
            expected: selected_live.revision(),
        })
        .expect(
            "selected tombstone prepares a recoverable namespace without inventing a payload stage",
        );
    selected_live.revision += 1;
    assert_eq!(store.operation(selected_live.id()).unwrap().stage, 0);
    assert!(
        store
            .operation(selected_live.id())
            .unwrap()
            .recovery
            .is_empty()
    );
    if stopped == 1 {
        account_tombstone_boundary(store, &selected_live, &prepared, stopped, &path);
        return;
    }
    let recovery = store
        .reserve_recovery(
            &store.ack(selected_live.id()),
            Side::Source,
            selected_live.intent().path.clone(),
            RecoveryReason::DisplacedVersion,
        )
        .expect(
            "a selected tombstone must retain its real displaced target without a payload stage",
        );
    selected_live.revision += 1;
    assert_eq!(
        store
            .recovery_page(record.id, None, 8)
            .unwrap()
            .entries
            .iter()
            .find(|entry| entry.id == recovery.id)
            .unwrap()
            .state,
        RecoveryState::Reserved
    );
    let original = d::decode(prepared.intent.as_ref().unwrap()).unwrap();
    let d::Body::Intent(body) = &original.body else {
        panic!("intent");
    };
    let d::State::Regular {
        object,
        stamp,
        content,
    } = &body.expected[0]
    else {
        panic!("target");
    };
    let displaced = d::encode(
        &original.header,
        &d::Body::Displaced(d::Displaced {
            intent: original.digest,
            stage: None,
            object: object.clone(),
            stamp: stamp.clone(),
            matches_expected: true,
            side: 0,
            parent: body.chains[0][0].clone(),
            leaf: NativeName::Unix(b"file".to_vec()),
            barriers: 3,
        }),
    )
    .unwrap();
    if stopped == 4 {
        prepared.outcome_displaced = Some(displaced);
        account_tombstone_boundary(store, &selected_live, &prepared, stopped, &path);
        return;
    }
    let paid = store
        .native_read_context(&selected_live, deadline, &cancel)
        .unwrap();
    let copy = store
        .preserve_file_with_context(
            recovery,
            &mut File::open(&actual).unwrap(),
            &cancel,
            Some(&paid),
        )
        .unwrap();
    selected_live.revision += 1;
    let Some(Row::Recovery(entry)) = store.row(&recovery::recovery_key(copy.value.id)).unwrap()
    else {
        panic!("copy");
    };
    let fact = d::RecoveryFact {
        intent: original.digest,
        recovery: d::Recovery {
            connection: record.id.0,
            operation: selected_live.id().serial,
            slot: entry.slot,
        },
        side: 0,
        role: 0,
        endpoint: body.endpoints[0],
        object: object.clone(),
        content: content.clone(),
        artifact: recovery::artifact_view(entry.object.as_ref().unwrap()),
    };
    let bound = format::NativeRecoveryFact {
        captured: None,
        connection: record.id.0,
        serial: selected_live.id().serial,
        slot: entry.slot,
        attempt: prepared.attempt,
        fact: fact.encode().unwrap(),
        displaced: displaced.clone(),
    };
    store
        .append_native(
            Change::NativeBound {
                fact: bound,
                expected: selected_live.revision(),
                fence: selected_live.fence(),
            },
            &paid,
        )
        .unwrap();
    selected_live.revision += 1;
    assert_eq!(store.operation(selected_live.id()).unwrap().stage, 1);
    if stopped == 2 {
        prepared.outcome_displaced = Some(displaced);
        account_tombstone_boundary(store, &selected_live, &prepared, stopped, &path);
        return;
    }
    let mut header = original.header.clone();
    header.progress = selected_live.revision();
    let outcome = d::encode(
        &header,
        &d::Body::Outcome(d::Outcome {
            intent: original.digest,
            stage: None,
            displaced: Some(d::decode(&displaced).unwrap().digest),
            states: [d::State::Absent, d::State::Absent],
            kind: 2,
            barriers: 3,
            recovery: vec![d::Reference {
                slot: entry.slot,
                side: 0,
                role: 0,
                binding: fact.binding_digest().unwrap(),
            }],
            sequence: 1,
        }),
    )
    .unwrap();
    prepared.outcome = Some(outcome);
    prepared.outcome_displaced = Some(displaced);
    for verified in [false, true] {
        let paid = store
            .native_read_context(&selected_live, deadline, &cancel)
            .unwrap();
        store.append_native(Change::NativeOutcome { registration: prepared.clone(), expected: selected_live.revision(), verified, reconstructed: false }, &paid)
            .expect("selected absence outcome joins the exact real retained displacement and historical choice");
        selected_live.revision += 1;
    }
    if stopped == 3 {
        account_tombstone_boundary(store, &selected_live, &prepared, stopped, &path);
        return;
    }
    let outcome_digest = d::decode(prepared.outcome.as_ref().unwrap())
        .unwrap()
        .digest;
    let paid = store
        .native_read_context(&selected_live, deadline, &cancel)
        .unwrap();
    store.append_native(Change::SelectedApplied { connection: record.id.0, serial: selected_live.id().serial,
        expected: selected_live.revision(), fence: selected_live.fence(), attempt: prepared.attempt, outcome: outcome_digest }, &paid)
        .expect("accounted tombstone clears executable pending while retaining its attention barrier and real displaced bytes");
    selected_live.revision += 1;
    let next = store.operation(selected_live.id()).unwrap();
    assert_eq!(next.stage, 9);
    assert!(next.selected_applied.as_ref().unwrap().attention);
    assert!(store.connection(record.id).unwrap().pending.is_none());
    assert_eq!(
        store
            .recovery_page(record.id, None, 8)
            .unwrap()
            .entries
            .iter()
            .find(|entry| entry.id == copy.value.id)
            .unwrap()
            .state,
        RecoveryState::Attached
    );
    assert!(
        store.consume_reviewed_selection(old).is_err(),
        "the exact consumed revision cannot approve another operation"
    );
    assert!(
        store
            .resume_native_for_test(next.decode().unwrap().id, &session)
            .is_err()
    );
    assert_eq!(std::fs::read(&actual).unwrap(), b"abc");
    assert_eq!(
        std::fs::read(
            path.join("recovery")
                .join(format::hex(&record.id.0))
                .join("1-0")
        )
        .unwrap(),
        b"abc"
    );
    store.checkpoint().unwrap();
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(store.operation(next.decode().unwrap().id).unwrap(), next);
    assert!(!store.conflict_record(conflict.id).unwrap().resolved);
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
    let current = store.load_connection(record.id).unwrap();
    let equality = OperationIntent {
        connection: record.id,
        expected_revision: current.revision,
        expected_baseline: current.baseline_revision,
        path: next.path.to_relative().unwrap(),
        source_before: None,
        destination_before: None,
        decision: Decision::VerifyEqual,
    };
    let connection = store.connection(record.id).unwrap();
    let mut candidate_operation =
        format::Operation::new(equality.clone(), connection.serial + 1, &connection).unwrap();
    candidate_operation.native_budget = Some(format::NativeBudget::initial());
    for altered in 0..3 {
        let mut changed = candidate_operation.clone();
        match altered {
            0 => {
                changed.source = Some(format::State::File {
                    sha256: [7; 32],
                    bytes: 3,
                    executable: false,
                });
                changed.destination = changed.source.clone();
            }
            1 => {
                changed.decision = 1;
                changed.source = Some(format::State::File {
                    sha256: [7; 32],
                    bytes: 3,
                    executable: false,
                });
            }
            _ => changed.native_budget = None,
        }
        assert!(
            store.reduce_intent(&changed).is_err(),
            "an applied tombstone cannot be silently replaced by old-baseline work or unrelated equality"
        );
    }
    assert!(
        store.reduce_intent(&candidate_operation).is_ok(),
        "fresh exact two-sided absence may verify the chosen tombstone"
    );
    use super::super::model::{Encoding, Endpoint, NamePolicy, Observation, ObservedTree};
    let observation = |identity| {
        ObservedTree::new(
            Endpoint {
                identity,
                encoding: Encoding::Unix,
                names: NamePolicy::Exact,
            },
            [],
            Observation::Complete,
        )
        .unwrap()
    };
    let preview = super::super::transaction::initial_preview_for_test(
        &store,
        record.id,
        observation(record.source.identity),
        observation(record.destination.identity),
    )
    .unwrap()
    .accept(equality.clone())
    .unwrap();
    let session = store.transaction_session(record.id).unwrap();
    let mut equality = store
        .begin_native_reviewed_without_identity_for_test(equality, &session, preview)
        .unwrap();
    store
        .reserve_native_reads(&mut equality, &session, 16 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    let paid = store
        .native_read_context(&equality, deadline, &cancel)
        .unwrap();
    store
        .append_native(
            Change::Verified {
                connection: record.id.0,
                serial: equality.id().serial,
                expected: equality.revision(),
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
            },
            &paid,
        )
        .unwrap();
    equality.revision += 1;
    assert!(
        selected_equality_conflict_matches(
            &store.operation(equality.id()).unwrap(),
            &next,
            &match store
                .row(&format::key(3, record.id.0, conflict.id.serial))
                .unwrap()
                .unwrap()
            {
                Row::Conflict(value) => value,
                _ => panic!("conflict"),
            }
        ),
        "only exact chosen absence and the bound current conflict revision authorize candidate consideration"
    );
    store.append_native(Change::BaselineCommit { directory_chain: None, directory_ancestors: None, existing_ancestors: None, connection: record.id.0, expected: equality.revision(), root: None, count: 0,
        source: record.source.identity.into(), destination: record.destination.identity.into(), initial_review: true }, &paid)
        .expect("fresh absence comparison clears the attention barrier only after paid retained-version verification");
    assert!(
        store
            .selected_attention(record.id, &next.path)
            .unwrap()
            .is_empty()
    );
    assert!(store.conflict_record(conflict.id).unwrap().resolved);
    assert!(
        store
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .unwrap()
            .is_empty()
    );
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert!(store.conflict_record(conflict.id).unwrap().resolved);
    assert_eq!(
        std::fs::read(
            path.join("recovery")
                .join(format::hex(&record.id.0))
                .join(format!(
                    "{}-{}",
                    copy.value.id.operation.serial, copy.value.id.slot
                ))
        )
        .unwrap(),
        b"abc"
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn account_tombstone_boundary(
    mut store: ConnectionStore,
    live: &LiveOperation,
    registration: &format::NativeRegistration,
    kind: u8,
    path: &std::path::Path,
) {
    let current = store.load_connection(live.id().connection).unwrap();
    let fence = store
        .request_disconnect(current.id, current.revision)
        .unwrap();
    let current = store.load_connection(current.id).unwrap();
    let mut context = store
        .open_recovery_context(live.id(), current.revision)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    store
        .reserve_recovery_reads(&mut context, 4 * 1024 * 1024, deadline, &cancel)
        .unwrap();
    let kind = if kind == 4 {
        use super::super::transaction::native::descriptor as d;
        let operation = store.operation(live.id()).unwrap();
        let id = RecoveryId {
            operation: live.id(),
            slot: operation.recovery[0],
        };
        let Some(Row::Recovery(mut entry)) = store.row(&recovery::recovery_key(id)).unwrap() else {
            panic!("reserved");
        };
        assert_eq!(entry.state, 0);
        let directory_name = format::hex(&current.id.0);
        let metadata = super::super::super::nofollow::metadata_nofollow(
            &store.storage.recovery.file,
            std::path::Path::new(&directory_name),
        )
        .unwrap();
        let directory = store
            .storage
            .recovery
            .open_child(std::ffi::OsStr::new(&directory_name), &metadata)
            .unwrap();
        let name = format!("{}-{}", entry.serial, entry.slot);
        let mut writer = storage::open_file(&directory, &name, true).unwrap();
        std::io::Write::write_all(&mut writer, b"bad").unwrap();
        storage::sync_file(&writer).unwrap();
        let artifact = format::ObjectIdentity {
            root: store.storage.identity.clone(),
            file_id: storage::file_identity(&storage::regular(&writer).unwrap()),
        };
        let original = d::decode(registration.intent.as_ref().unwrap()).unwrap();
        let d::Body::Intent(before) = &original.body else {
            panic!("intent");
        };
        let d::State::Regular {
            object, content, ..
        } = &before.expected[usize::from(entry.side)]
        else {
            panic!("displaced");
        };
        let fact = d::RecoveryFact {
            intent: original.digest,
            recovery: d::Recovery {
                connection: entry.connection,
                operation: entry.serial,
                slot: entry.slot,
            },
            side: entry.side,
            role: 0,
            endpoint: before.endpoints[usize::from(entry.side)],
            object: object.clone(),
            content: content.clone(),
            artifact: recovery::artifact_view(&artifact),
        };
        entry.state = 1;
        entry.object = Some(artifact);
        entry.content = Some(format::State::File {
            sha256: content.digest,
            bytes: content.length,
            executable: content.executable,
        });
        let fact = format::NativeRecoveryFact {
            captured: None,
            connection: entry.connection,
            serial: entry.serial,
            slot: entry.slot,
            attempt: registration.attempt,
            fact: fact.encode().unwrap(),
            displaced: registration.outcome_displaced.clone().unwrap(),
        };
        let event = Change::SelectedDrain {
            entry: entry,
            source: format::BoundaryEvidence {
                attempt: registration.attempt,
                displaced: registration.outcome_displaced.clone(),
                outcome: None,
            },
            fact: Some(fact),
            expected: context.revision(),
            operation_expected: context.operation_revision,
            fence: context.fence(),
        };
        let paid = store
            .recovery_read_context(&context, deadline, &cancel)
            .unwrap();
        assert!(
            store.append_native(event.clone(), &paid).is_err(),
            "changed private bytes cannot account a tombstone displacement"
        );
        std::io::Seek::seek(&mut writer, std::io::SeekFrom::Start(0)).unwrap();
        std::io::Write::write_all(&mut writer, b"abc").unwrap();
        storage::sync_file(&writer).unwrap();
        assert!(store.append(event.clone()).is_err());
        store
            .append_native(event.clone(), &paid)
            .expect("tombstone stopped drain accepts only exact paid real displaced bytes");
        context.revision += 1;
        context.operation_revision = context.revision;
        assert!(store.append_native(event, &paid).is_err());
        assert_eq!(store.operation(live.id()).unwrap().stage, 0);
        assert!(store.connection(current.id).unwrap().pending.is_some());
        assert_eq!(
            std::fs::read(path.join("recovery").join(&directory_name).join(&name)).unwrap(),
            b"abc"
        );
        2
    } else {
        kind
    };
    let evidence = format::BoundaryEvidence {
        attempt: registration.attempt,
        displaced: registration.outcome_displaced.clone(),
        outcome: registration.outcome.clone(),
    };
    let event = Change::SelectedBoundary {
        connection: current.id.0,
        serial: live.id().serial,
        expected: context.revision(),
        operation_expected: context.operation_revision,
        fence: context.fence(),
        kind,
        evidence: evidence.clone(),
    };
    let paid = store
        .recovery_read_context(&context, deadline, &cancel)
        .unwrap();
    assert!(store.append(event.clone()).is_err());
    assert!(
        store
            .append_native(
                Change::NativeBoundary {
                    connection: current.id.0,
                    serial: live.id().serial,
                    expected: context.revision(),
                    operation_expected: context.operation_revision,
                    fence: context.fence(),
                    kind,
                    evidence: Some(evidence)
                },
                &paid
            )
            .is_err()
    );
    for changed in 0..4 {
        let mut stale = event.clone();
        let Change::SelectedBoundary {
            expected,
            operation_expected,
            fence,
            serial,
            ..
        } = &mut stale
        else {
            panic!("boundary");
        };
        match changed {
            0 => *expected += 1,
            1 => *operation_expected += 1,
            2 => *fence += 1,
            _ => *serial += 1,
        }
        assert!(store.append_native(stale, &paid).is_err());
    }
    store.append_native(event.clone(), &paid).expect("recorded tombstone boundary accounts only already reached facts under stopped paid context");
    assert!(store.append_native(event, &paid).is_err());
    assert!(store.check_native(live).is_err());
    let operation = store.operation(live.id()).unwrap();
    assert_eq!(operation.stage, 7);
    assert_eq!(operation.native_boundary, Some(kind));
    assert!(operation.selected_applied.is_none());
    assert!(store.connection(current.id).unwrap().pending.is_none());
    assert!(store.load_baseline(current.id).unwrap().entries().is_none());
    assert!(
        !store
            .conflict_record(ConflictId {
                connection: current.id,
                serial: 1
            })
            .unwrap()
            .resolved
    );
    for entry in store.recovery_page(current.id, None, 16).unwrap().entries {
        assert_eq!(
            std::fs::read(
                path.join("recovery")
                    .join(format::hex(&current.id.0))
                    .join(format!("{}-{}", entry.id.operation.serial, entry.id.slot))
            )
            .unwrap(),
            b"abc"
        );
    }
    store.finish_disconnect(&fence).unwrap();
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(path).unwrap();
    assert_eq!(store.operation(live.id()).unwrap().stage, 7);
    assert_eq!(
        store.load_connection(current.id).unwrap().lifecycle,
        Lifecycle::Disconnected
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn account_tombstone_collision(
    mut store: ConnectionStore,
    mut live: LiveOperation,
    path: &std::path::Path,
    actual: &std::path::Path,
    chosen: &TombstoneChoice,
    original: &ConflictRecord,
) {
    use super::super::transaction::native::descriptor as d;
    let cancel = AtomicBool::new(false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let operation = store.operation(live.id()).unwrap();
    let mut facts = native_admission_facts(&operation, live.fence());
    if let d::State::Regular { content, .. } = &mut facts.states[0] {
        content.digest = sha2::Sha256::digest(b"abc").into();
    }
    let snapshot = d::ConflictSnapshot {
        connection: live.id().connection.0,
        operation: live.id().serial,
        admission: live.admission(),
        revision: live.revision(),
        fence: live.fence(),
        baseline: operation.baseline,
        endpoints: facts.endpoints,
        path: facts.path,
        chains: facts.chains,
        versions: (0..2)
            .map(|side| d::CapturedVersion {
                side,
                role: 0,
                state: facts.states[usize::from(side)].clone(),
                displaced: None,
            })
            .collect(),
    };
    let paid = store.native_read_context(&live, deadline, &cancel).unwrap();
    store
        .append_native(
            Change::NativeSelectedCollision {
                snapshot: snapshot.encode().unwrap(),
                expected: live.revision(),
            },
            &paid,
        )
        .unwrap();
    let captured = store.operation(live.id()).unwrap();
    live.revision = captured.revision;
    live.action = captured.decode().unwrap().action;
    let reservation = store.reserve_capture_slot(&mut live, &snapshot, 0).unwrap();
    let paid = store.native_read_context(&live, deadline, &cancel).unwrap();
    let copy = store
        .preserve_file_with_context(
            reservation,
            &mut File::open(actual).unwrap(),
            &cancel,
            Some(&paid),
        )
        .unwrap();
    live.revision += 1;
    let Some(Row::Recovery(entry)) = store.row(&recovery::recovery_key(copy.value.id)).unwrap()
    else {
        panic!("copy");
    };
    let d::State::Regular {
        object, content, ..
    } = &snapshot.versions[0].state
    else {
        panic!("actual");
    };
    let fact = d::RecoveryFact {
        intent: snapshot.binding_digest().unwrap(),
        recovery: d::Recovery {
            connection: live.id().connection.0,
            operation: live.id().serial,
            slot: entry.slot,
        },
        side: 0,
        role: 1,
        endpoint: snapshot.endpoints[0],
        object: object.clone(),
        content: content.clone(),
        artifact: recovery::artifact_view(entry.object.as_ref().unwrap()),
    };
    let bound = format::NativeRecoveryFact {
        captured: Some(snapshot.encode().unwrap()),
        connection: live.id().connection.0,
        serial: live.id().serial,
        slot: entry.slot,
        attempt: 0,
        fact: fact.encode().unwrap(),
        displaced: Vec::new(),
    };
    store
        .append_native(
            Change::NativeBound {
                fact: bound,
                expected: live.revision(),
                fence: live.fence(),
            },
            &paid,
        )
        .unwrap();
    live.revision += 1;
    let id = ConflictId {
        connection: live.id().connection,
        serial: live.id().serial,
    };
    let Some(Row::Conflict(conflict)) = store
        .row(&format::key(3, id.connection.0, id.serial))
        .unwrap()
    else {
        panic!("collision");
    };
    let paid = store.native_read_context(&live, deadline, &cancel).unwrap();
    store.append_native(Change::SelectedCollisionComplete { connection: id.connection.0, serial: id.serial, expected: live.revision(),
        fence: live.fence(), conflict_expected: conflict.revision, capture: snapshot.binding_digest().unwrap() }, &paid)
        .expect("occupied selected absence completes only collision accounting and preserves its exact historical choice");
    let collision = store.conflict_record(id).unwrap();
    assert!(store.connection(id.connection).unwrap().pending.is_none());
    assert_eq!(&store.conflict_record(original.id).unwrap(), original);
    let choices = store.tombstone_choices(id).unwrap();
    assert!(
        choices.contains(&chosen),
        "later collision exposes the original absence even when its revision differs"
    );
    let sequence = store.sequence;
    for _ in 0..2 {
        assert_eq!(
            store.selected_collision_accounting(live.id()).unwrap(),
            Some(collision.clone())
        );
    }
    assert_eq!(store.sequence, sequence);
    store.retention_sample = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(60))
        .unwrap();
    store.checkpoint().unwrap();
    drop(store);
    let mut store = ConnectionStore::open_at(path).unwrap();
    assert_eq!(&store.conflict_record(original.id).unwrap(), original);
    assert!(store.tombstone_choices(id).unwrap().contains(&chosen));
    let retained = store
        .recovery_page(id.connection, None, 16)
        .unwrap()
        .entries;
    assert!(retained.iter().all(|entry| entry.retained_seconds == 0));
    for entry in retained {
        assert_eq!(
            std::fs::read(
                path.join("recovery")
                    .join(format::hex(&id.connection.0))
                    .join(format!("{}-{}", entry.id.operation.serial, entry.id.slot))
            )
            .unwrap(),
            b"abc"
        );
    }
    let current = store.load_connection(id.connection).unwrap();
    let mut proposed = live.intent().clone();
    proposed.expected_revision = current.revision;
    for altered in 0..6 {
        let mut changed = chosen.clone();
        match altered {
            0 => changed.revision += 1,
            1 => changed.snapshot[0] ^= 1,
            2 => changed.side = 0,
            3 => changed.origin.serial = id.serial,
            4 => changed.path = RelativePath::unix(b"other").unwrap(),
            _ => changed.origin.connection = ConnectionId([44; 16]),
        }
        assert!(
            store
                .tombstone_review_basis(
                    proposed.clone(),
                    changed,
                    Side::Source,
                    (id, collision.revision)
                )
                .is_err()
        );
    }
    assert!(
        store
            .tombstone_review_basis(
                proposed.clone(),
                chosen.clone(),
                Side::Source,
                (id, collision.revision + 1)
            )
            .is_err()
    );
    let canonical = store.operation(live.id()).unwrap();
    let key = format::key(1, canonical.connection, canonical.serial);
    let mut wrong = canonical;
    let Some(format::SelectedAction::Restore {
        selected: format::SelectedVersion::Tombstone { revision, .. },
        ..
    }) = &mut wrong.action
    else {
        panic!("choice");
    };
    *revision += 1;
    let prior = store.overlay.insert(key.clone(), Row::Operation(wrong));
    assert!(
        store
            .tombstone_review_basis(
                proposed.clone(),
                chosen.clone(),
                Side::Source,
                (id, collision.revision)
            )
            .is_err()
    );
    if let Some(row) = prior {
        store.overlay.insert(key, row);
    } else {
        store.overlay.remove(&key);
    }
    let review = store
        .tombstone_review_basis(
            proposed,
            chosen.clone(),
            Side::Source,
            (id, collision.revision),
        )
        .expect("current collision review must retain the exact historical origin")
        .accept_replacement();
    let (proposal, action) = store.consume_reviewed_selection(review).unwrap();
    let format::SelectedAction::Restore {
        selected, conflict, ..
    } = &action;
    assert_eq!(conflict.as_ref().unwrap().serial, id.serial);
    assert_eq!(conflict.as_ref().unwrap().revision, collision.revision);
    assert_eq!(
        selected,
        &format::SelectedVersion::Tombstone {
            operation: chosen.origin.serial,
            revision: chosen.revision,
            side: chosen.side,
            snapshot: chosen.snapshot
        }
    );
    let connection = store.connection(id.connection).unwrap();
    let mut proposal_row =
        format::Operation::new(proposal.clone(), connection.serial + 1, &connection).unwrap();
    proposal_row.native_budget = Some(format::NativeBudget::initial());
    proposal_row.action = Some(action.clone());
    assert!(store.reduce_intent(&proposal_row).is_ok());
    for altered in 0..5 {
        let mut changed = proposal_row.clone();
        let Some(format::SelectedAction::Restore {
            selected:
                format::SelectedVersion::Tombstone {
                    revision, snapshot, ..
                },
            conflict: Some(bound),
            ..
        }) = &mut changed.action
        else {
            panic!("selection");
        };
        match altered {
            0 => *revision += 1,
            1 => snapshot[0] ^= 1,
            2 => bound.revision += 1,
            3 => changed.path = format::NativePath::Unix(b"other".to_vec()),
            _ => changed.source_binding.root[0] ^= 1,
        }
        assert!(
            store.reduce_intent(&changed).is_err(),
            "same-path origin data cannot substitute for exact collision-action membership"
        );
    }
    let session = store.transaction_session(id.connection).unwrap();
    let continued = store
        .begin_native_operation(proposal, &session, None, None, Some(action), None)
        .unwrap();
    let operation = store.operation(continued.id()).unwrap();
    assert_eq!(
        store
            .selected_descriptor(&operation, operation.action.as_ref().unwrap())
            .unwrap(),
        d::Selection::Tombstone {
            conflict: (chosen.origin.connection.0, chosen.origin.serial),
            revision: chosen.revision
        }
    );
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(path).unwrap();
    assert_eq!(&store.conflict_record(original.id).unwrap(), original);
    assert_eq!(std::fs::read(actual).unwrap(), b"abc");
    assert!(store.tombstone_choices(id).unwrap().contains(&chosen));
}

#[test]
fn native_executor_planning_requests_initial_review_without_creating_intent() {
    use super::super::{
        model::{Encoding, Endpoint, NamePolicy, Observation, ObservedTree},
        transaction::{RegularPlan, plan_regular},
    };
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 929);
    let endpoint = |identity| Endpoint {
        identity,
        encoding: Encoding::Unix,
        names: NamePolicy::Exact,
    };
    let proposal = intent(&record);
    let source = ObservedTree::new(
        endpoint(record.source.identity),
        [(
            proposal.path.clone(),
            proposal.source_before.clone().unwrap(),
        )],
        Observation::Complete,
    )
    .unwrap();
    let destination = ObservedTree::new(
        endpoint(record.destination.identity),
        [],
        Observation::Complete,
    )
    .unwrap();
    let sequence = store.sequence;
    let baseline = store.load_baseline(record.id).unwrap();
    let mut wrong = baseline.clone();
    wrong.connection.0[0] ^= 1;
    assert!(
        plan_regular(
            &store,
            record.id,
            &wrong,
            source.clone(),
            destination.clone()
        )
        .is_err()
    );
    let plan = plan_regular(&store, record.id, &baseline, source, destination).unwrap();
    assert_eq!(store.sequence, sequence);
    assert_eq!(store.load_connection(record.id).unwrap(), record);
    assert!(store.connection(record.id).unwrap().pending.is_none());
    assert!(
        store
            .pending_page(record.id, None, 8)
            .unwrap()
            .entries
            .is_empty()
    );
    let RegularPlan::InitialReview(preview) = plan else {
        panic!("routine planning may not auto-accept initial consent");
    };
    let reviewed = preview.accept(proposal.clone()).unwrap();
    let session = store.transaction_session(record.id).unwrap();
    let live = store
        .begin_native_reviewed_without_identity_for_test(proposal, &session, reviewed)
        .unwrap();
    assert!(store.operation(live.id()).unwrap().initial_review.is_some());
}

#[test]
fn native_executor_reuses_only_the_exact_unprepared_reservation() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 930);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let first = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [3; 16])
        .unwrap();
    let sequence = store.sequence;
    let revision = live.revision();
    let reused = store
        .reusable_native_namespace(&live, Side::Destination)
        .unwrap()
        .unwrap();
    assert_eq!(reused.name(), first.name());
    assert_eq!(store.sequence, sequence);
    assert_eq!(live.revision(), revision);
    assert_eq!(
        store
            .rows()
            .unwrap()
            .filter(|row| matches!(row, Ok(Row::NativeRegistration(_))))
            .count(),
        1
    );
    assert!(
        store
            .reusable_native_namespace(&mut live, Side::Source)
            .is_err()
    );
    let prepared = native_prepared_facts(&live, &reused);
    store
        .append(Change::NativePrepared {
            registration: prepared,
            expected: live.revision(),
        })
        .unwrap();
    live.revision += 1;
    assert!(
        store
            .reusable_native_namespace(&mut live, Side::Destination)
            .is_err(),
        "prepared evidence must resume through inspection, never new preparation"
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
)]
fn native_verified_equality_is_untouched_for_stopped_accounting() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 820);
    let connection = store.connection(record.id).unwrap();
    let mut proposal = intent(&record);
    proposal.destination_before = proposal.source_before.clone();
    proposal.decision = Decision::VerifyEqual;
    let mut operation =
        format::Operation::new(proposal, connection.serial + 1, &connection).unwrap();
    operation.native_budget = Some(format::NativeBudget::initial());
    operation.native_identity = Some(
        native_admission_facts(&operation, connection.fence)
            .encode()
            .unwrap(),
    );
    let id = operation.decode().unwrap().id;
    store.append(Change::Intent { operation }).unwrap();
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store.resume_native_for_test(id, &session).unwrap();
    let cancel = AtomicBool::new(false);
    store
        .reserve_native_reads(
            &mut live,
            &session,
            4 * 1024 * 1024,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &cancel,
        )
        .unwrap();
    let payer = store
        .native_read_context(
            &live,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &cancel,
        )
        .unwrap();
    store
        .append_native(
            Change::Verified {
                connection: record.id.0,
                serial: id.serial,
                expected: live.revision(),
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
            },
            &payer,
        )
        .unwrap();
    store.checkpoint().unwrap();
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(store.operation(id).unwrap().stage, 3);
    let original = store.operation(id).unwrap();
    assert!(store.verified_equality_is_untouched(&original).unwrap());
    for field in 0..9 {
        let mut bad = original.clone();
        match field {
            0 => bad.stage = 2,
            1 => bad.recovery.push(0),
            2 => bad.native_identity = None,
            3 => bad.native_capture = Some(vec![0]),
            4 => bad.native_boundary = Some(0),
            5 => bad.native_capture_displaced.push(vec![0]),
            6 => bad.destination = None,
            7 => bad.decision = 1,
            _ => {
                bad.action = Some(format::SelectedAction::Restore {
                    version: 1,
                    target: 1,
                    replace: false,
                    revision: bad.expected,
                    fence: 0,
                    selected: format::SelectedVersion::Recovery {
                        operation: 1,
                        slot: 0,
                        content: bad.source.clone().unwrap(),
                    },
                    conflict: None,
                });
            }
        }
        assert!(
            !matches!(store.verified_equality_is_untouched(&bad), Ok(true)),
            "wrong untouched equality shape {field}"
        );
    }
    let current = store.load_connection(record.id).unwrap();
    let fence = store
        .request_disconnect(record.id, current.revision)
        .unwrap();
    let current = store.connection(record.id).unwrap();
    let operation = store.operation(id).unwrap();
    let event = Change::NativeBoundary {
        connection: record.id.0,
        serial: id.serial,
        expected: current.revision,
        operation_expected: operation.revision,
        fence: current.fence,
        kind: 0,
        evidence: None,
    };
    for field in 0..4 {
        let mut bad = event.clone();
        let Change::NativeBoundary {
            serial,
            expected,
            operation_expected,
            fence,
            ..
        } = &mut bad
        else {
            unreachable!()
        };
        match field {
            0 => *serial += 1,
            1 => *expected += 1,
            2 => *operation_expected += 1,
            _ => *fence += 1,
        }
        assert!(store.append(bad).is_err());
        assert_eq!(store.operation(id).unwrap().stage, 3);
    }
    store.append(event)
        .expect("Verified equality has not changed either endpoint and can be accounted with fresh untouched evidence");
    assert_eq!(store.operation(id).unwrap().stage, 7);
    assert!(store.connection(record.id).unwrap().pending.is_none());
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
    store.finish_disconnect(&fence).unwrap();
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        store.load_connection(record.id).unwrap().lifecycle,
        Lifecycle::Disconnected
    );
    assert_eq!(store.operation(id).unwrap().stage, 7);
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
}

#[test]
fn native_verified_equality_refuses_even_an_uncreated_namespace() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 821);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [8; 16])
        .unwrap();
    // A historical shape probe, not a production transition to Verified. Even a
    // wholly uncreated reservation disqualifies the stage-3 equality exception.
    let mut operation = store.operation(live.id()).unwrap();
    operation.stage = 3;
    operation.destination = operation.source.clone();
    operation.decision = 0;
    operation.native_identity = Some(
        native_admission_facts(&operation, live.fence())
            .encode()
            .unwrap(),
    );
    assert_eq!(
        operation.decode().unwrap().intent.decision,
        Decision::VerifyEqual
    );
    assert!(!store.verified_equality_is_untouched(&operation).unwrap());
    assert_eq!(store.operation(live.id()).unwrap().stage, 0);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep exact existing-parent fixture setup, durable boundaries and retained-byte assertions together."
)]
fn native_executor_plans_existing_missing_baseline_ancestors() {
    use super::super::{
        model::{Encoding, Endpoint, NamePolicy, Observation, ObservedTree},
        transaction::{RegularPlan, plan_regular},
    };
    for baselined_parent in [false, true] {
        let (_temp, path) = fixture();
        let mut store = ConnectionStore::open_at(&path).unwrap();
        let record = create(&mut store, 822);
        let mut seed = intent(&record);
        seed.destination_before = seed.source_before.clone();
        seed.decision = Decision::VerifyEqual;
        let id = *store.begin_intent(seed).unwrap().value();
        let current = store.load_connection(record.id).unwrap();
        store
            .append(Change::Verified {
                connection: record.id.0,
                serial: id.serial,
                expected: current.revision,
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
            })
            .unwrap();
        let current = store.load_connection(record.id).unwrap();
        let entries = if baselined_parent {
            vec![(
                RelativePath::unix(b"folder").unwrap(),
                EntryState::Directory,
            )]
        } else {
            vec![]
        };
        let root = storage::build_pages(
            &store.storage,
            entries.iter().enumerate().map(|(ordinal, (path, state))| {
                Ok(Row::Baseline(format::BaselineEntry {
                    connection: record.id.0,
                    revision: current.revision + 1,
                    ordinal: ordinal as u32,
                    path: path.clone().into(),
                    state: state.clone().into(),
                }))
            }),
        )
        .unwrap();
        store
            .append(Change::BaselineCommit { directory_chain: None,
                directory_ancestors: None,
                existing_ancestors: None,
                connection: record.id.0,
                expected: current.revision,
                root,
                count: entries.len() as u32,
                source: record.source.identity.into(),
                destination: record.destination.identity.into(),
                initial_review: true,
            })
            .unwrap();
        let current = store.load_connection(record.id).unwrap();
        let sequence = store.sequence;
        let endpoint = |identity| Endpoint {
            identity,
            encoding: Encoding::Unix,
            names: NamePolicy::Exact,
        };
        let path = RelativePath::unix(b"folder/file").unwrap();
        let parent = RelativePath::unix(b"folder").unwrap();
        let value = EntryState::File {
            sha256: [7; 32],
            bytes: 3,
            executable: false,
        };
        let source = ObservedTree::new(
            endpoint(record.source.identity),
            [
                (parent.clone(), EntryState::Directory),
                (path.clone(), value),
            ],
            Observation::Complete,
        )
        .unwrap();
        let destination = ObservedTree::new(
            endpoint(record.destination.identity),
            [(parent, EntryState::Directory)],
            Observation::Complete,
        )
        .unwrap();
        let baseline = store.load_baseline(record.id).unwrap();
        let plan = plan_regular(&store, record.id, &baseline, source, destination).unwrap();
        assert!(
            matches!(plan, RegularPlan::Ready(ref intent) if intent.path == path),
            "safe existing parents must permit the regular leaf proposal"
        );
        assert_eq!(store.sequence, sequence);
        assert_eq!(store.load_connection(record.id).unwrap(), current);
        assert!(store.connection(record.id).unwrap().pending.is_none());
        assert_eq!(
            store
                .table_page(record.id, 5, None, 16)
                .unwrap()
                .entries
                .len(),
            0
        );
    }
}

#[test]
fn native_executor_reviewed_nested_initial_accepts_existing_ancestors_before_intent() {
    use super::super::{
        model::{Encoding, Endpoint, NamePolicy, Observation, ObservedTree},
        transaction::initial_preview_for_test,
    };
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 823);
    let sequence = store.sequence;
    let endpoint = |identity| Endpoint {
        identity,
        encoding: Encoding::Unix,
        names: NamePolicy::Exact,
    };
    let parent = RelativePath::unix(b"folder").unwrap();
    let file = RelativePath::unix(b"folder/file").unwrap();
    let value = EntryState::File {
        sha256: [7; 32],
        bytes: 3,
        executable: false,
    };
    let source = ObservedTree::new(
        endpoint(record.source.identity),
        [
            (parent.clone(), EntryState::Directory),
            (file.clone(), value.clone()),
        ],
        Observation::Complete,
    )
    .unwrap();
    let destination = ObservedTree::new(
        endpoint(record.destination.identity),
        [(parent, EntryState::Directory)],
        Observation::Complete,
    )
    .unwrap();
    let preview = initial_preview_for_test(&store, record.id, source, destination).unwrap();
    let mut proposal = intent(&record);
    proposal.path = file;
    proposal.source_before = Some(value);
    let reviewed = preview.accept(proposal).unwrap();
    assert!(
        reviewed.regular_prerequisite(&store).unwrap().is_none(),
        "reviewed leaf already has Directory ancestors on both endpoints"
    );
    assert_eq!(store.sequence, sequence);
    assert_eq!(store.load_connection(record.id).unwrap(), record);
    assert!(store.connection(record.id).unwrap().pending.is_none());
    assert!(
        store
            .table_page(record.id, 5, None, 8)
            .unwrap()
            .entries
            .is_empty()
    );
}

// Store-ordering instrumentation only. This cannot construct native evidence.
pub(super) struct BaselineAppendProbe {
    generation: u64,
    checkpoint: bool,
    reject_call: usize,
    calls: std::cell::Cell<usize>,
}
impl BaselineAppendProbe {
    pub(super) fn validate(&self, store: &ConnectionStore) -> Result<(), StoreError> {
        self.calls.set(self.calls.get() + 1);
        self.assert_preparation_finished(store);
        if self.calls.get() == self.reject_call {
            Err(StoreError::RecoveryRequired)
        } else {
            Ok(())
        }
    }
    fn assert_preparation_finished(&self, store: &ConnectionStore) {
        assert_eq!(
            store.current.generation > self.generation,
            self.checkpoint,
            "final proof must run after any actual checkpoint and second reduction"
        );
    }
}
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep pre-write refusal, healthy control and post-fsync durable-error assertions under the same actual checkpoint and second-reduction setup."
)]
fn native_baseline_final_validation_refuses_before_event_after_checkpoint_preparation() {
    for checkpoint in [false, true] {
        for reject_call in [1, 0, 2] {
            let (_temp, path) = fixture();
            let mut store = ConnectionStore::open_at(&path).unwrap();
            let record = create(&mut store, 790);
            commit_test_baseline(&mut store, record.id);
            let baseline = store.load_baseline(record.id).unwrap();
            let record = store.load_connection(record.id).unwrap();
            let cancel = AtomicBool::new(false);
            let live = native_verified_addition_facts(&mut store, &record, b"second", &cancel);
            let payer = store
                .native_read_context(
                    &live,
                    std::time::Instant::now() + std::time::Duration::from_secs(30),
                    &cancel,
                )
                .unwrap();
            let root = storage::build_pages(
                &store.storage,
                [b"file".as_slice(), b"second".as_slice()]
                    .into_iter()
                    .enumerate()
                    .map(|(ordinal, path)| {
                        Ok(Row::Baseline(format::BaselineEntry {
                            connection: record.id.0,
                            revision: live.revision + 1,
                            ordinal: u32::try_from(ordinal).unwrap(),
                            path: format::NativePath::Unix(path.to_vec()),
                            state: format::State::File {
                                sha256: [7; 32],
                                bytes: 3,
                                executable: false,
                            },
                        }))
                    }),
            )
            .unwrap();
            let before = store.load_connection(record.id).unwrap();
            let operation = store.operation(live.id()).unwrap();
            let probe = BaselineAppendProbe {
                generation: store.current.generation,
                checkpoint,
                reject_call,
                calls: std::cell::Cell::new(0),
            };
            if checkpoint {
                store.overlay_bytes = 9 * 1024 * 1024;
            }
            let result = store.append_checked_with_baseline(
                Change::BaselineCommit { directory_chain: None,
                    directory_ancestors: None,
                    existing_ancestors: None,
                    connection: record.id.0,
                    expected: live.revision,
                    root,
                    count: 2,
                    source: record.source.identity.into(),
                    destination: record.destination.identity.into(),
                    initial_review: false,
                },
                RecoveryChecks {
                    mode: VerificationMode::NativeLive(&payer),
                    ..RecoveryChecks::default()
                },
                BaselineAppendCheck::Host(&probe),
            );
            if reject_call == 2 {
                assert!(matches!(result, Err(StoreError::RecoveryRequired)));
                assert_eq!(probe.calls.get(), 2);
                assert!(
                    store.validate().is_err(),
                    "post-write failure must poison this owner"
                );
            } else if reject_call == 1 {
                assert!(
                    matches!(result, Err(StoreError::RecoveryRequired)),
                    "a final proof refusal must prevent the BaselineCommit event"
                );
                assert_eq!(store.load_connection(record.id).unwrap(), before);
                assert_eq!(store.operation(live.id()).unwrap(), operation);
                assert_eq!(store.load_baseline(record.id).unwrap(), baseline);
                assert_eq!(probe.calls.get(), 1);
            } else {
                result.unwrap();
                assert_eq!(
                    store
                        .load_baseline(record.id)
                        .unwrap()
                        .entries()
                        .unwrap()
                        .len(),
                    2
                );
                assert_eq!(
                    probe.calls.get(),
                    2,
                    "recheck while proof owners remain held through fsync"
                );
            }
            drop(store);
            let reopened = ConnectionStore::open_at(&path).unwrap();
            if reject_call == 1 {
                assert_eq!(reopened.load_connection(record.id).unwrap(), before);
                assert_eq!(reopened.load_baseline(record.id).unwrap(), baseline);
                assert_eq!(reopened.operation(live.id()).unwrap(), operation);
            } else {
                assert_eq!(
                    reopened
                        .load_baseline(record.id)
                        .unwrap()
                        .entries()
                        .unwrap()
                        .len(),
                    2
                );
            }
        }
    }
}

// A present, exact receipt authorizes only the two missing strict prefixes.
// Removing the reducer receipt join must make this successful commit fail.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep exact existing-parent fixture setup, durable boundaries and retained-byte assertions together."
)]
fn existing_ancestor_receipt_commits_exact_closure_and_replays() {
    for initial in [false, true] {
        for commit in [false, true] {
            let (_temp, path) = fixture();
            let mut store = ConnectionStore::open_at(&path).unwrap();
            let record = create(&mut store, 830);
            if !initial {
                commit_test_baseline(&mut store, record.id);
            }
            let record = store.load_connection(record.id).unwrap();
            let connection = store.connection(record.id).unwrap();
            let mut proposal = intent(&record);
            proposal.path = RelativePath::unix(b"a/b/file").unwrap();
            proposal.expected_baseline = record.baseline_revision;
            proposal.destination_before = proposal.source_before.clone();
            proposal.decision = Decision::VerifyEqual;
            let mut operation =
                format::Operation::new(proposal, connection.serial + 1, &connection).unwrap();
            operation.native_budget = Some(format::NativeBudget::initial());
            let mut admission = native_admission_facts(&operation, connection.fence);
            for chain in &mut admission.chains {
                let root = chain[0].clone();
                let mut first = root.clone();
                first.ino += 10;
                let mut second = root;
                second.ino += 20;
                chain.extend([first, second]);
            }
            let admission = admission.encode().unwrap();
            operation.native_identity = Some(admission.clone());
            if initial {
                operation.initial_review = Some(format::InitialReviewFact {
                    preview: [9; 32],
                    selection: format::digest(&format::encode(&operation).unwrap()),
                    revision: operation.expected,
                });
            }
            let id = operation.decode().unwrap().id;
            store.append(Change::Intent { operation }).unwrap();
            let session = store.transaction_session(record.id).unwrap();
            let mut live = store.resume_native_for_test(id, &session).unwrap();
            let cancel = AtomicBool::new(false);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            store
                .reserve_native_reads(&mut live, &session, 64 * 1024 * 1024, deadline, &cancel)
                .unwrap();
            let context = store.native_read_context(&live, deadline, &cancel).unwrap();
            let current = store.load_connection(record.id).unwrap();
            store
                .append_native(
                    Change::Verified {
                        connection: record.id.0,
                        serial: id.serial,
                        expected: current.revision,
                        source: record.source.identity.into(),
                        destination: record.destination.identity.into(),
                    },
                    &context,
                )
                .unwrap();
            let current = store.load_connection(record.id).unwrap();
            let mut entries = store
                .load_baseline(record.id)
                .unwrap()
                .entries()
                .cloned()
                .unwrap_or_default();
            entries.insert(RelativePath::unix(b"a").unwrap(), EntryState::Directory);
            entries.insert(RelativePath::unix(b"a/b").unwrap(), EntryState::Directory);
            entries.insert(
                RelativePath::unix(b"a/b/file").unwrap(),
                intent(&record).source_before.unwrap(),
            );
            let root = storage::build_pages(
                &store.storage,
                entries.iter().enumerate().map(|(ordinal, (path, state))| {
                    Ok(Row::Baseline(format::BaselineEntry {
                        connection: record.id.0,
                        revision: current.revision + 1,
                        ordinal: ordinal as u32,
                        path: path.clone().into(),
                        state: state.clone().into(),
                    }))
                }),
            )
            .unwrap();
            let wire = serde_json::json!({"event": "BaselineCommit",
                "connection": record.id.0, "expected": current.revision, "root": root, "count": entries.len(),
                "source": storage::Identity::from(record.source.identity),
                "destination": storage::Identity::from(record.destination.identity), "initial_review": initial,
                "existing_ancestors": {"version": 1, "admission": format::digest(&admission), "prefixes": 6_u64}
            });
            let live = store.resume_native_for_test(id, &session).unwrap();
            let context = store.native_read_context(&live, deadline, &cancel).unwrap();
            let before = store.load_connection(record.id).unwrap();
            let old = store.load_baseline(record.id).unwrap();
            let mut bad_receipts = Vec::new();
            for bitmap in [0_u64, 1, 2, 4, 8, 14, u64::MAX] {
                let mut bad = wire.clone();
                bad["existing_ancestors"]["prefixes"] = bitmap.into();
                bad_receipts.push(bad);
            }
            let mut bad = wire.clone();
            bad["existing_ancestors"]["version"] = 2.into();
            bad_receipts.push(bad);
            let mut bad = wire.clone();
            bad["existing_ancestors"]["admission"] = serde_json::json!([0_u8; 32].to_vec());
            bad_receipts.push(bad);
            let mut bad = wire.clone();
            bad.as_object_mut().unwrap().remove("existing_ancestors");
            bad_receipts.push(bad);
            let mut bad = wire.clone();
            bad["existing_ancestors"] = serde_json::Value::Null;
            bad_receipts.push(bad);
            let mut bad = wire.clone();
            bad["existing_ancestors"]["extra"] = true.into();
            bad_receipts.push(bad);
            for bad in bad_receipts {
                if let Ok(change) = serde_json::from_value::<Change>(bad) {
                    assert!(store.append_native(change, &context).is_err());
                }
                assert_eq!(store.load_connection(record.id).unwrap(), before);
                assert_eq!(
                    store.load_baseline(record.id).unwrap().entries(),
                    old.entries()
                );
            }
            let encoded = serde_json::to_string(&wire).unwrap();
            let duplicate = encoded.replace("\"prefixes\":6", "\"prefixes\":6,\"prefixes\":6");
            assert_ne!(encoded, duplicate);
            assert!(serde_json::from_str::<Change>(&duplicate).is_err());
            let receipt_size = serde_json::to_vec(&wire["existing_ancestors"])
                .unwrap()
                .len();
            let change: Change =
                serde_json::from_value(wire.clone()).expect("versioned existing-ancestor receipt");
            let event_size = format::encode(&Event {
                schema: 1,
                predecessor: [0; 32],
                change: change.clone(),
            })
            .unwrap()
            .len();
            eprintln!(
                "existing ancestor receipt JSON={receipt_size} bytes; representative event JSON={event_size} bytes"
            );
            assert!(event_size < format::MAX_BYTES);
            let mut extra = entries.clone();
            extra.insert(
                RelativePath::unix(b"unrelated").unwrap(),
                EntryState::Directory,
            );
            let extra_root = storage::build_pages(
                &store.storage,
                extra.iter().enumerate().map(|(ordinal, (path, state))| {
                    Ok(Row::Baseline(format::BaselineEntry {
                        connection: record.id.0,
                        revision: current.revision + 1,
                        ordinal: ordinal as u32,
                        path: path.clone().into(),
                        state: state.clone().into(),
                    }))
                }),
            )
            .unwrap();
            let mut wrong_candidate = wire;
            wrong_candidate["root"] = serde_json::json!(extra_root);
            wrong_candidate["count"] = serde_json::json!(extra.len());
            assert!(
                store
                    .append_native(serde_json::from_value(wrong_candidate).unwrap(), &context)
                    .is_err()
            );
            assert_eq!(
                store.load_baseline(record.id).unwrap().entries(),
                old.entries()
            );

            if !commit {
                drop(store);
                let store = ConnectionStore::open_at(&path).unwrap();
                assert_eq!(
                    store.load_baseline(record.id).unwrap().entries(),
                    old.entries()
                );
                assert_eq!(store.load_connection(record.id).unwrap(), before);
                continue;
            }

            let live = store.resume_native_for_test(id, &session).unwrap();
            let context = store.native_read_context(&live, deadline, &cancel).unwrap();
            store.append_native(change, &context).unwrap();
            assert_eq!(
                store.load_baseline(record.id).unwrap().entries(),
                Some(&entries)
            );
            drop(store);
            let store = ConnectionStore::open_at(&path).unwrap();
            assert_eq!(
                store.load_baseline(record.id).unwrap().entries(),
                Some(&entries)
            );
        }
    }
}

#[test]
fn existing_ancestor_preflight_capacity_refuses_without_durable_changes() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 831);
    let mut proposal = intent(&record);
    proposal.path = RelativePath::unix(b"a/b/file").unwrap();
    let baseline = VerifiedBaseline::absent(
        record.id,
        record.source.identity,
        record.destination.identity,
    );
    assert_eq!(
        store
            .preflight_regular_baseline(&proposal, &baseline)
            .unwrap(),
        3
    );
    let rows = (0..4096).map(|index| {
        (
            RelativePath::unix(format!("old-{index:04}").as_bytes()).unwrap(),
            EntryState::Directory,
        )
    });
    let baseline = VerifiedBaseline::from_verified(
        record.id,
        record.source.identity,
        record.destination.identity,
        rows,
    )
    .unwrap();
    assert!(matches!(
        store.preflight_regular_baseline(&proposal, &baseline),
        Err(StoreError::LimitExceeded)
    ));
    assert_eq!(store.load_connection(record.id).unwrap(), record);
    assert_eq!(store.executor_position(record.id).unwrap(), (None, None));
    assert!(
        store
            .recovery_page(record.id, None, 128)
            .unwrap()
            .entries
            .is_empty()
    );
}

#[test]
fn native_legacy_pending_classification_preserves_registered_history_and_baseline() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 991);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    assert!(!store.legacy_native_pending(live.id()).unwrap());
    let reservation = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [7; 16])
        .unwrap();
    let registration = native_prepared_facts(&live, &reservation);
    let canonical = registration.clone();
    store
        .append(Change::NativePrepared {
            registration,
            expected: live.revision,
        })
        .unwrap();
    let id = live.id();
    let before = store.load_baseline(record.id).unwrap().entries().cloned();
    let revision = store.connection(record.id).unwrap().revision;
    assert!(store.legacy_native_pending(id).unwrap());
    assert_eq!(store.connection(record.id).unwrap().revision, revision);
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    assert!(reopened.legacy_native_pending(id).unwrap());
    assert_eq!(
        reopened.connection(record.id).unwrap().pending,
        Some(id.serial)
    );
    assert_eq!(
        reopened
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .cloned(),
        before
    );
    let Some(Row::NativeRegistration(retained)) = reopened.row(&canonical.key()).unwrap() else {
        panic!("retained registration");
    };
    assert_eq!(retained, canonical);
}

#[test]
fn native_created_profile_registration_is_distinct_from_legacy_pending() {
    use super::super::transaction::native::descriptor as d;
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 992);
    let session = store.transaction_session(record.id).unwrap();
    let mut live = store
        .begin_native_for_test(intent(&record), &session)
        .unwrap();
    let reservation = store
        .reserve_native_namespace_with_nonce(&mut live, Side::Destination, [8; 16])
        .unwrap();
    let mut registration = native_prepared_facts(&live, &reservation);
    let old = d::decode(registration.stage.as_ref().unwrap()).unwrap();
    let mut bytes = registration.stage.take().unwrap();
    bytes[..4].copy_from_slice(b"QNS3");
    let split = bytes.len() - 32;
    let digest = sha2::Sha256::digest(&bytes[..split]);
    bytes[split..].copy_from_slice(&digest);
    let stage = d::decode(&bytes).unwrap();
    assert_eq!(stage.profile, d::Profile::CreatedRegular);
    assert_ne!(old.digest, stage.digest);
    registration.stage = Some(bytes);
    registration.validate().unwrap();
    store
        .append(Change::NativePrepared {
            registration: registration.clone(),
            expected: live.revision,
        })
        .unwrap();
    assert!(!store.legacy_native_pending(live.id()).unwrap());
    drop(store);
    let reopened = ConnectionStore::open_at(&path).unwrap();
    assert!(!reopened.legacy_native_pending(live.id()).unwrap());
    assert_eq!(
        reopened.row(&registration.key()).unwrap(),
        Some(Row::NativeRegistration(registration))
    );
}

#[path = "directory_tests.rs"]
mod directory_registration_wire_tests;

// The version boundary must preserve old durable bytes while recognizing only a
// well-formed explicit carry event; this supplies metadata, never native proof.
#[test]
fn selected_attention_wire_preserves_legacy_and_accepts_versioned_event() {
    let legacy = r#"{"event":"SelectedApplied","connection":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"serial":4,"expected":20,"fence":0,"attempt":1,"outcome":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1]}"#;
    let old: Change = serde_json::from_str(legacy).unwrap();
    assert_eq!(format::encode(&old).unwrap(), legacy.as_bytes());
    let mut next: serde_json::Value = serde_json::from_str(legacy).unwrap();
    next["event"] = "SelectedAppliedCarryV1".into();
    next["carry"] = serde_json::json!({"version":1,"prior":[{
        "operation":2,"operation_revision":12,"applied_revision":11,
        "fence":0,"outcome":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],
        "conflict":1,"conflict_revision":4
    }]});
    let event: Change = serde_json::from_value(next.clone())
        .expect("a separate versioned carry event must decode without reinterpreting legacy SelectedApplied");
    assert_eq!(serde_json::to_value(&event).unwrap(), next);
    for invalid in 0..8 {
        let mut value = next.clone();
        match invalid {
            0 => value["carry"]["version"] = 2.into(),
            1 => value["carry"] = serde_json::Value::Null,
            2 => value["carry"]["prior"] = serde_json::json!([]),
            3 => value["carry"]["unexpected"] = true.into(),
            4 => value["carry"]["prior"][0]["operation"] = 0.into(),
            5 => value["carry"]["prior"][0]["outcome"] = serde_json::json!(([0u8; 32])),
            6 => {
                let first = value["carry"]["prior"][0].clone();
                value["carry"]["prior"] = serde_json::json!([first.clone(), first]);
            }
            _ => {
                value.as_object_mut().unwrap().remove("carry");
            }
        }
        assert!(
            serde_json::from_value::<Change>(value).is_err(),
            "malformed carry case {invalid}"
        );
    }
}

// Individually decodable historical metadata is not sufficient when the conflict
// and its originating operation disagree; executor admission must join them.
#[test]
fn selected_attention_executor_refuses_cross_row_conflict_after_checkpoint() {
    let (_temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 977);
    let mut connection = store.connection(record.id).unwrap();
    let mut origin = format::Operation::new(intent(&record), 1, &connection).unwrap();
    origin.decision = 10;
    origin.path = format::NativePath::Unix(b"different-origin-path".to_vec());
    let mut applied = format::Operation::new(intent(&record), 2, &connection).unwrap();
    applied.native_budget = Some(format::NativeBudget::initial());
    let content = applied.source.clone().unwrap();
    applied.action = Some(format::SelectedAction::Restore {
        version: 1,
        target: 1,
        replace: true,
        revision: applied.expected,
        fence: connection.fence,
        selected: format::SelectedVersion::Recovery {
            operation: 1,
            slot: 0,
            content: content.clone(),
        },
        conflict: Some(format::SelectedConflict {
            serial: 1,
            revision: 2,
        }),
    });
    applied.stage = 9;
    applied.revision = 3;
    applied.selected_applied = Some(format::SelectedAppliedFact {
        revision: 3,
        fence: connection.fence,
        outcome: [1; 32],
        attention: true,
    });
    let conflict = format::ConflictData {
        connection: record.id.0,
        serial: 1,
        revision: 2,
        kind: 0,
        path: applied.path.clone(),
        source: applied.source.clone(),
        destination: applied.destination.clone(),
        native_capture: None,
        preserved: vec![],
        resolved: false,
    };
    origin.decode().unwrap();
    applied.decode().unwrap();
    conflict.decode().unwrap();
    connection.serial = 2;
    connection.revision = 3;
    let rows = store
        .prepare_rows(vec![
            Row::Connection(connection),
            Row::Operation(origin),
            Row::Operation(applied),
            Row::Conflict(conflict),
        ])
        .unwrap();
    store.commit_rows(rows);
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    let current = store.load_connection(record.id).unwrap();
    let mut equality = intent(&current);
    equality.source_before = Some(content.clone().into());
    equality.destination_before = Some(content.into());
    equality.decision = Decision::VerifyEqual;
    assert!(
        matches!(
            store.executor_candidate_allowed(&equality),
            Err(StoreError::Corrupt)
        ),
        "cross-row path mismatch must refuse after checkpoint, not satisfy explicit consent"
    );
}

// Portable historical metadata only. No fixture row is ever submitted as a
// native receipt or used to claim actual bytes or current endpoint authority.
#[cfg(unix)]
fn selected_attention_metadata_fixture() -> (tempfile::TempDir, ConnectionStore, format::Operation)
{
    let (temp, path) = fixture();
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let record = create(&mut store, 978);
    let connection = store.connection(record.id).unwrap();
    let mut rows = Vec::new();
    for serial in [1, 2] {
        let mut origin = format::Operation::new(intent(&record), serial, &connection).unwrap();
        origin.decision = 10;
        origin.recovery = vec![0];
        origin.stage = 1;
        rows.push(Row::Conflict(format::ConflictData {
            connection: record.id.0,
            serial,
            revision: 2,
            kind: 0,
            path: origin.path.clone(),
            source: origin.source.clone(),
            destination: origin.destination.clone(),
            native_capture: None,
            preserved: vec![0],
            resolved: false,
        }));
        rows.push(Row::Recovery(format::Recovery {
            captured: None,
            connection: record.id.0,
            serial,
            slot: 0,
            side: 0,
            path: origin.path.clone(),
            reason: 2,
            state: 1,
            content: origin.source.clone(),
            object: Some(format::ObjectIdentity {
                root: store.storage.identity.clone(),
                file_id: format::FileIdentity::Unix(7),
            }),
            retained: 0,
        }));
        rows.push(Row::Operation(origin));
    }
    let mut new = None;
    for (serial, conflict, revision) in [(3, 1, 6), (4, 2, 8), (5, 2, 11)] {
        let mut applied = format::Operation::new(intent(&record), serial, &connection).unwrap();
        applied.native_budget = Some(format::NativeBudget::initial());
        applied.action = Some(format::SelectedAction::Restore {
            version: 1,
            target: 1,
            replace: true,
            revision: applied.expected,
            fence: connection.fence,
            selected: format::SelectedVersion::Recovery {
                operation: conflict,
                slot: 0,
                content: applied.source.clone().unwrap(),
            },
            conflict: Some(format::SelectedConflict {
                serial: conflict,
                revision: 2,
            }),
        });
        applied.revision = revision;
        if serial == 5 {
            applied.stage = 3;
            new = Some(applied);
        } else {
            applied.stage = 9;
            applied.selected_applied = Some(format::SelectedAppliedFact {
                revision,
                fence: connection.fence,
                outcome: [serial as u8; 32],
                attention: true,
            });
            rows.push(Row::Operation(applied));
        }
    }
    let prepared = store.prepare_rows(rows).unwrap();
    store.commit_rows(prepared);
    (temp, store, new.unwrap())
}

#[cfg(unix)]
#[test]
fn selected_attention_carry_keeps_only_current_distinct_compatible_consent() {
    let (_temp, mut store, next) = selected_attention_metadata_fixture();
    let connection = ConnectionId(next.connection);
    let expected = vec![format::SelectedAttentionRefV1 {
        operation: 3,
        operation_revision: 6,
        applied_revision: 6,
        fence: 0,
        outcome: [3; 32],
        conflict: 1,
        conflict_revision: 2,
    }];
    assert_eq!(store.selected_attention_carry(&next, 10).unwrap(), expected);
    let old = store
        .operation(OperationId {
            connection,
            serial: 3,
        })
        .unwrap();
    let conflict_key = format::key(3, next.connection, 1);
    let Some(Row::Conflict(original)) = store.row(&conflict_key).unwrap() else {
        panic!("conflict");
    };
    // Legitimate revised/resolved history supersedes; corruption still refuses.
    for resolved in [false, true] {
        let mut revised = original.clone();
        revised.revision += 1;
        revised.resolved = resolved;
        store
            .overlay
            .insert(conflict_key.clone(), Row::Conflict(revised));
        assert!(
            store
                .selected_attention_carry(&next, 10)
                .unwrap()
                .is_empty()
        );
    }
    store
        .overlay
        .insert(conflict_key.clone(), Row::Conflict(original.clone()));
    for altered in 0..4 {
        let mut previous = old.clone();
        match altered {
            0 => previous.selected_applied.as_mut().unwrap().attention = false,
            1 => previous.revision = 12,
            2 => previous.selected_applied.as_mut().unwrap().fence += 1,
            _ => {
                let Some(format::SelectedAction::Restore {
                    selected: format::SelectedVersion::Recovery { content, .. },
                    ..
                }) = &mut previous.action
                else {
                    panic!("selected");
                };
                *content = format::State::File {
                    sha256: [8; 32],
                    bytes: 3,
                    executable: false,
                };
            }
        }
        store
            .overlay
            .insert(format::key(1, next.connection, 3), Row::Operation(previous));
        let result = store.selected_attention_carry(&next, 10);
        if altered == 0 {
            assert!(result.unwrap().is_empty());
        } else {
            assert!(result.is_err(), "invalid recorded selection {altered}");
        }
    }
    store
        .overlay
        .insert(format::key(1, next.connection, 3), Row::Operation(old));
    let origin = store
        .operation(OperationId {
            connection,
            serial: 1,
        })
        .unwrap();
    for altered in 0..4 {
        let mut broken = original.clone();
        let mut operation = origin.clone();
        match altered {
            0 => operation.path = format::NativePath::Unix(b"wrong-path".to_vec()),
            1 => broken.source = None,
            2 => operation.destination_binding.root[0] ^= 1,
            _ => broken.preserved = vec![1],
        }
        broken.resolved = true; // Cannot hide structural corruption behind supersession.
        broken.revision += 1;
        operation.decode().unwrap();
        broken.decode().unwrap();
        store.overlay.insert(
            format::key(1, next.connection, 1),
            Row::Operation(operation),
        );
        store
            .overlay
            .insert(conflict_key.clone(), Row::Conflict(broken));
        assert!(
            store.selected_attention_carry(&next, 10).is_err(),
            "broken cross-row pair {altered}"
        );
    }
}

#[cfg(unix)]
#[test]
fn selected_attention_ordinary_join_keeps_earlier_capture_and_revision_history() {
    use super::super::transaction::native::descriptor as d;
    let (_temp, mut store, next) = selected_attention_metadata_fixture();
    let id = OperationId {
        connection: ConnectionId(next.connection),
        serial: 1,
    };
    let mut operation = store.operation(id).unwrap();
    operation.native_budget = Some(format::NativeBudget::initial());
    operation.decision = 1; // Captured propagation may have an independently supplied conflict kind.
    operation.stage = 8;
    operation.revision = 7; // Completion followed by paid inspection.
    let metadata = native_admission_facts(&operation, 0);
    let mut snapshot = d::ConflictSnapshot {
        connection: next.connection,
        operation: 1,
        admission: 2,
        revision: 2,
        fence: 0,
        baseline: None,
        endpoints: metadata.endpoints,
        path: metadata.path,
        chains: metadata.chains,
        versions: vec![
            d::CapturedVersion {
                side: 0,
                role: 0,
                state: metadata.states[0].clone(),
                displaced: None,
            },
            d::CapturedVersion {
                side: 1,
                role: 0,
                state: metadata.states[0].clone(),
                displaced: None,
            },
        ],
    };
    let earlier = snapshot.encode().unwrap();
    snapshot.revision = 3;
    let latest = snapshot.encode().unwrap();
    operation.native_capture = Some(latest.clone());
    let key = format::key(3, next.connection, 1);
    let Some(Row::Conflict(mut conflict)) = store.row(&key).unwrap() else {
        panic!("conflict");
    };
    conflict.kind = 3;
    conflict.revision = 5;
    conflict.native_capture = Some(latest);
    let recovery_id = RecoveryId {
        operation: id,
        slot: 0,
    };
    let Some(Row::Recovery(mut entry)) = store.row(&recovery::recovery_key(recovery_id)).unwrap()
    else {
        panic!("recovery");
    };
    entry.captured = Some(format::CapturedSlot {
        snapshot: earlier,
        version: 0,
    });
    store.overlay.insert(
        recovery::recovery_key(recovery_id),
        Row::Recovery(entry.clone()),
    );
    store.overlay.insert(
        format::key(1, next.connection, 1),
        Row::Operation(operation.clone()),
    );
    assert!(
        store.ordinary_conflict_join(&conflict).is_ok(),
        "older preserved capture, changed capture kind and actual occupied destination are valid history"
    );
    // The earlier snapshot must still belong to this origin and its endpoints.
    snapshot.revision = 2;
    snapshot.endpoints[0][0] ^= 1;
    entry.captured.as_mut().unwrap().snapshot = snapshot.encode().unwrap();
    store
        .overlay
        .insert(recovery::recovery_key(recovery_id), Row::Recovery(entry));
    assert!(store.ordinary_conflict_join(&conflict).is_err());
}

#[cfg(unix)]
#[test]
fn selected_attention_metadata_helper_requires_exact_complete_refs() {
    let (_temp, mut store, mut next) = selected_attention_metadata_fixture();
    // Bind a new third conflict so both older independently accepted consents
    // must be carried, in deterministic operation order.
    let original_key = format::key(3, next.connection, 2);
    let Some(Row::Conflict(mut third)) = store.row(&original_key).unwrap() else {
        panic!("conflict");
    };
    let mut origin = store
        .operation(OperationId {
            connection: ConnectionId(next.connection),
            serial: 2,
        })
        .unwrap();
    let Some(Row::Recovery(mut entry)) = store
        .row(&recovery::recovery_key(RecoveryId {
            operation: OperationId {
                connection: ConnectionId(next.connection),
                serial: 2,
            },
            slot: 0,
        }))
        .unwrap()
    else {
        panic!("recovery");
    };
    third.serial = 5;
    origin.serial = 5;
    entry.serial = 5;
    let rows = store
        .prepare_rows(vec![
            Row::Conflict(third),
            Row::Operation(origin),
            Row::Recovery(entry),
        ])
        .unwrap();
    store.commit_rows(rows);
    next.serial = 6;
    let Some(format::SelectedAction::Restore {
        selected: format::SelectedVersion::Recovery { operation, .. },
        conflict: Some(bound),
        ..
    }) = &mut next.action
    else {
        panic!("selection");
    };
    *operation = 5;
    bound.serial = 5;
    let valid = format::SelectedAttentionCarryV1 {
        version: 1,
        prior: vec![
            format::SelectedAttentionRefV1 {
                operation: 3,
                operation_revision: 6,
                applied_revision: 6,
                fence: 0,
                outcome: [3; 32],
                conflict: 1,
                conflict_revision: 2,
            },
            format::SelectedAttentionRefV1 {
                operation: 4,
                operation_revision: 8,
                applied_revision: 8,
                fence: 0,
                outcome: [4; 32],
                conflict: 2,
                conflict_revision: 2,
            },
        ],
    };
    store
        .validate_selected_attention_carry(&next, 10, &valid)
        .unwrap();
    let before = (
        store.sequence,
        store.digest,
        store.overlay.clone(),
        std::fs::read(
            store
                .storage
                .path
                .join(format!("log-{}", store.current.log)),
        )
        .unwrap(),
    );
    for changed in 0..11 {
        let mut wrong = valid.clone();
        match changed {
            0 => {
                wrong.prior.pop();
            }
            1 => wrong.prior.swap(0, 1),
            2 => wrong.prior[0].operation = 2,
            3 => wrong.prior[0].operation_revision += 1,
            4 => wrong.prior[0].applied_revision += 1,
            5 => wrong.prior[0].fence += 1,
            6 => wrong.prior[0].outcome[0] ^= 1,
            7 => wrong.prior[0].conflict_revision += 1,
            8 => wrong.prior[0].conflict = 2,
            9 => wrong.version = 2,
            _ => wrong.prior = vec![valid.prior[0].clone(); 128],
        }
        assert!(
            store
                .validate_selected_attention_carry(&next, 10, &wrong)
                .is_err(),
            "substituted carry {changed}"
        );
    }
    assert_eq!(
        (
            store.sequence,
            store.digest,
            store.overlay.clone(),
            std::fs::read(
                store
                    .storage
                    .path
                    .join(format!("log-{}", store.current.log))
            )
            .unwrap()
        ),
        before
    );
    // Different content is valid history but incompatible consent, not corruption.
    let different = format::State::File {
        sha256: [9; 32],
        bytes: 7,
        executable: true,
    };
    let id = OperationId {
        connection: ConnectionId(next.connection),
        serial: 1,
    };
    let mut origin = store.operation(id).unwrap();
    let mut applied = store
        .operation(OperationId {
            connection: id.connection,
            serial: 3,
        })
        .unwrap();
    let Some(Row::Conflict(mut conflict)) = store.row(&format::key(3, next.connection, 1)).unwrap()
    else {
        panic!("conflict");
    };
    let Some(Row::Recovery(mut entry)) = store
        .row(&recovery::recovery_key(RecoveryId {
            operation: id,
            slot: 0,
        }))
        .unwrap()
    else {
        panic!("entry");
    };
    origin.source = Some(different.clone());
    conflict.source = Some(different.clone());
    entry.content = Some(different.clone());
    let Some(format::SelectedAction::Restore {
        selected: format::SelectedVersion::Recovery { content, .. },
        ..
    }) = &mut applied.action
    else {
        panic!("selection");
    };
    *content = different;
    let rows = store
        .prepare_rows(vec![
            Row::Operation(origin),
            Row::Operation(applied.clone()),
            Row::Conflict(conflict),
            Row::Recovery(entry),
        ])
        .unwrap();
    store.commit_rows(rows);
    assert_eq!(
        store.selected_attention_carry(&next, 10).unwrap(),
        vec![valid.prior[1].clone()]
    );
    // A cleared fact stays cleared even if the new choice returns to its content.
    applied.selected_applied.as_mut().unwrap().attention = false;
    store.overlay.insert(
        format::key(1, next.connection, 3),
        Row::Operation(applied.clone()),
    );
    let Some(format::SelectedAction::Restore { selected, .. }) = applied.action else {
        panic!("selection");
    };
    let Some(format::SelectedAction::Restore {
        selected: current,
        conflict,
        ..
    }) = &mut next.action
    else {
        panic!("selection");
    };
    *current = selected;
    *conflict = Some(format::SelectedConflict {
        serial: 1,
        revision: 2,
    });
    assert!(
        store
            .selected_attention_carry(&next, 10)
            .unwrap()
            .is_empty()
    );
}

// Unit coverage of row projection only: this does not dispatch a completion
// event or prove native history. The native event-history test covers that seam.
#[cfg(unix)]
#[test]
fn selected_attention_supersession_helper_projects_rows_across_checkpoint() {
    for carry in [false, true] {
        let (_temp, mut store, next) = selected_attention_metadata_fixture();
        let path = store.storage.path.clone();
        let id = ConnectionId(next.connection);
        let selected = format::SelectedAttentionCarryV1 {
            version: 1,
            prior: vec![format::SelectedAttentionRefV1 {
                operation: 3,
                operation_revision: 6,
                applied_revision: 6,
                fence: 0,
                outcome: [3; 32],
                conflict: 1,
                conflict_revision: 2,
            }],
        };
        if carry {
            store
                .validate_selected_attention_carry(&next, 10, &selected)
                .unwrap();
        }
        let changes = store
            .superseded_attention_rows(id, &next.path, carry.then_some(&selected))
            .unwrap();
        assert_eq!(changes.len(), if carry { 1 } else { 2 });
        let prepared = store.prepare_rows(changes).unwrap();
        store.commit_rows(prepared);
        let expected = if carry { vec![3] } else { vec![] };
        assert_eq!(
            store
                .selected_attention(id, &next.path)
                .unwrap()
                .iter()
                .map(|op| op.serial)
                .collect::<Vec<_>>(),
            expected
        );
        store.checkpoint().unwrap();
        drop(store);
        let store = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(
            store
                .selected_attention(id, &next.path)
                .unwrap()
                .iter()
                .map(|op| op.serial)
                .collect::<Vec<_>>(),
            expected
        );
    }
}

#[cfg(unix)]
#[test]
fn selected_attention_equality_requires_every_conflict_after_checkpoint() {
    let (_temp, mut store, next) = selected_attention_metadata_fixture();
    let id = ConnectionId(next.connection);
    let mut connection = store.connection(id).unwrap();
    connection.serial = 4;
    connection.revision = 10;
    store.overlay.insert(
        format::key(0, next.connection, 0),
        Row::Connection(connection.clone()),
    );
    let mut equality = intent(&store.load_connection(id).unwrap());
    equality.destination_before = equality.source_before.clone();
    equality.decision = Decision::VerifyEqual;
    assert!(
        store.executor_candidate_allowed(&equality).unwrap(),
        "both explicitly bound metadata consents can propose comparison, never commit it"
    );
    let mut origin = store
        .operation(OperationId {
            connection: id,
            serial: 1,
        })
        .unwrap();
    origin.serial = 5;
    origin.recovery.clear();
    origin.stage = 0;
    let Some(Row::Conflict(mut unaccepted)) =
        store.row(&format::key(3, next.connection, 1)).unwrap()
    else {
        panic!("conflict");
    };
    unaccepted.serial = 5;
    unaccepted.preserved.clear();
    connection.serial = 5;
    let prepared = store
        .prepare_rows(vec![
            Row::Connection(connection),
            Row::Operation(origin),
            Row::Conflict(unaccepted),
        ])
        .unwrap();
    store.commit_rows(prepared);
    store.checkpoint().unwrap();
    let path = store.storage.path.clone();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert!(
        !store.executor_candidate_allowed(&equality).unwrap(),
        "a third unaccepted conflict still blocks equality"
    );
}

// This test utility consumes genuine completed native history from the worker
// fixture. It never returns a proof/context or makes replay available to a live
// caller. Compile it on Unix hosts as well; execution belongs to the Linux test.
#[cfg(unix)]
impl ConnectionStore {
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            dead_code,
            reason = "The genuine fixture runs only on Linux; host compilation still checks this historical test utility."
        )
    )]
    pub(in crate::connection) fn assert_selected_attention_event_history(self, id: ConnectionId) {
        let path = self.storage.path.clone();
        let log_path = path.join(format!("log-{}", self.current.log));
        let original_log = std::fs::read(&log_path).unwrap();
        let original_rows = self.rows().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        let original_sequence = self.sequence;
        let original_digest = self.digest;
        let mut log = File::open(&log_path).unwrap();
        log.seek(SeekFrom::Start(8)).unwrap();
        let header: format::LogHeader = framing::read_record(&mut log, 0).unwrap().unwrap();
        let mut sequence = header.first_sequence;
        let mut last = None;
        loop {
            let offset = log.stream_position().unwrap();
            let event: Option<Event> = framing::read_record(&mut log, sequence).unwrap();
            let Some(event) = event else {
                break;
            };
            last = Some((offset, sequence, event));
            sequence += 1;
        }
        let (offset, sequence, event) = last.expect("genuine final completion event");
        assert_eq!(sequence, original_sequence);
        let Change::SelectedAppliedCarryV1 {
            connection,
            serial,
            expected,
            fence,
            attempt,
            outcome,
            carry,
        } = &event.change
        else {
            panic!("genuine final event must carry two earlier consents");
        };
        assert_eq!(*connection, id.0);
        assert_eq!(carry.prior.len(), 2);
        let serial = *serial;
        let legacy = Change::SelectedApplied {
            connection: *connection,
            serial,
            expected: *expected,
            fence: *fence,
            attempt: *attempt,
            outcome: *outcome,
        };
        let prefix = original_log[..usize::try_from(offset).unwrap()].to_vec();
        let cut = || {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&log_path)
                .unwrap();
            file.set_len(offset).unwrap();
            file.sync_all().unwrap();
            assert_eq!(std::fs::read(&log_path).unwrap(), prefix);
        };
        drop(log);
        drop(self);
        cut();
        let mut store = ConnectionStore::open_at(&path).unwrap();
        let snapshot = |store: &ConnectionStore| {
            (
                store.sequence,
                store.digest,
                store.load_connection(id).unwrap(),
                store.overlay.clone(),
                store.overlay_bytes,
                std::fs::read(&log_path).unwrap(),
                std::fs::read(path.join("CURRENT")).unwrap(),
            )
        };
        let before = snapshot(&store);
        let historical_append = |store: &mut ConnectionStore, change: Change| {
            let checks = store.replay_checks(&change)?;
            let probe = super::chain_tests::HistoricalAppendProbe::default();
            store.append_checked_with_baseline(
                change,
                checks,
                BaselineAppendCheck::Historical(&probe),
            )
        };
        // Identical production-event control proves the real registration,
        // outcome, revision, fence and replay mode are otherwise admissible.
        historical_append(&mut store, event.change.clone()).unwrap();
        assert_eq!(store.sequence, original_sequence);
        assert_eq!(store.digest, original_digest);
        assert_eq!(
            store
                .rows()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            original_rows
        );
        assert_eq!(std::fs::read(&log_path).unwrap(), original_log);
        drop(store);
        let store = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(
            store
                .rows()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            original_rows
        );
        drop(store);
        cut();
        let mut store = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(snapshot(&store), before);
        for changed in 0..14 {
            let mut wrong = event.change.clone();
            let Change::SelectedAppliedCarryV1 { carry, .. } = &mut wrong else {
                unreachable!()
            };
            match changed {
                0 => {
                    carry.prior.pop();
                } // Nonempty but incomplete list.
                1 => carry.prior.swap(0, 1),
                2 => carry.prior[0].operation += 1,
                3 => carry.prior[0].operation_revision += 1,
                4 => carry.prior[0].applied_revision += 1,
                5 => carry.prior[0].fence += 1,
                6 => carry.prior[0].outcome[0] ^= 1,
                7 => carry.prior[0].conflict_revision += 1,
                8 => carry.prior[0].conflict = carry.prior[1].conflict,
                9 => carry.version = 2,
                10 => carry.prior = vec![carry.prior[0].clone(); 128],
                11 => carry.prior.clear(),
                12 => carry.prior.push(carry.prior[1].clone()),
                _ => carry.prior[0].conflict_revision -= 1,
            }
            assert!(
                historical_append(&mut store, wrong).is_err(),
                "carry substitution {changed} reached completion"
            );
            assert_eq!(
                snapshot(&store),
                before,
                "carry substitution {changed} mutated admission"
            );
        }
        // Historical acceptance never grants live append permission.
        assert!(matches!(
            store.append(event.change.clone()),
            Err(StoreError::RecoveryRequired)
        ));
        assert_eq!(snapshot(&store), before);
        let checks = store.replay_checks(&event.change).unwrap();
        assert!(matches!(
            store.append_checked(event.change.clone(), checks),
            Err(StoreError::RecoveryRequired)
        ));
        assert_eq!(snapshot(&store), before);
        // Removing the native mark is deliberately invalid metadata, not a
        // fixture that claims native authority. Actual event admission refuses.
        let key = format::key(1, id.0, serial);
        let prior_overlay = store.overlay.get(&key).cloned();
        let mut unmarked = store
            .operation(OperationId {
                connection: id,
                serial,
            })
            .unwrap();
        unmarked.native_budget = None;
        store.overlay.insert(key.clone(), Row::Operation(unmarked));
        let unmarked_before = snapshot(&store);
        assert!(matches!(
            store.append(event.change.clone()),
            Err(StoreError::RecoveryRequired)
        ));
        assert_eq!(snapshot(&store), unmarked_before);
        match prior_overlay {
            Some(row) => {
                store.overlay.insert(key, row);
            }
            None => {
                store.overlay.remove(&key);
            }
        }
        assert_eq!(snapshot(&store), before);
        // Keep every genuine completion field; only the historical event type
        // changes. Legacy must clear both old markers even though their content
        // matches and each conflict remains independently unresolved.
        let mut legacy_rows = original_rows;
        for row in &mut legacy_rows {
            if let Row::Operation(operation) = row
                && carry.prior.iter().any(|prior| {
                    prior.operation == operation.serial && operation.connection == id.0
                })
            {
                let fact = operation.selected_applied.as_mut().unwrap();
                assert!(fact.attention);
                fact.attention = false;
            }
        }
        historical_append(&mut store, legacy).unwrap();
        assert_eq!(
            store
                .rows()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            legacy_rows
        );
        let applied = store
            .operation(OperationId {
                connection: id,
                serial,
            })
            .unwrap();
        assert_eq!(
            store
                .selected_attention(id, &applied.path)
                .unwrap()
                .iter()
                .map(|operation| operation.serial)
                .collect::<Vec<_>>(),
            vec![serial]
        );
        let legacy_log = std::fs::read(&log_path).unwrap();
        drop(store);
        let mut store = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(
            store
                .rows()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            legacy_rows
        );
        assert_eq!(std::fs::read(&log_path).unwrap(), legacy_log);
        store.checkpoint().unwrap();
        drop(store);
        let store = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(
            store
                .rows()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            legacy_rows
        );
        assert_eq!(
            store
                .selected_attention(id, &applied.path)
                .unwrap()
                .iter()
                .map(|operation| operation.serial)
                .collect::<Vec<_>>(),
            vec![serial]
        );
    }
}
