use super::*;

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
                "setup::connection::store::tests::two_processes_cannot_own_store",
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
        .append(Change::BaselineCommit {
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

thread_local! {static FAULT:std::cell::Cell<Option<(usize,bool)>>=const {std::cell::Cell::new(None)};}
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
        .append(Change::BaselineCommit {
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
            .append(Change::BaselineCommit {
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
            store.append(Change::BaselineCommit {
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
            "setup::connection::store::tests::killed_owner_leaves_durable_pending_intent",
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
        .append(Change::BaselineCommit {
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
