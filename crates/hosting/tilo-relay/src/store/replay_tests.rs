use super::*;
use crate::{coedit::CoeditPayload, transport};
fn frame(value: u8, size: usize, timestamp: u64) -> Vec<u8> {
    transport::encode(&Envelope::Delta {
        workspace_id: 7,
        author_device_public_key: [1; 32],
        authored_at_ns: timestamp,
        signature: [0; 64],
        delta: CoeditPayload::RetainedDelta {
            channel: [9; 32],
            body: vec![value; size],
        }
        .encode()
        .unwrap(),
    })
    .unwrap()
}
/// Active generation of a retained lineage whose files the test writes
/// directly, so it can inject records that retention itself would reject or
/// collapse.
fn raw_generation(store: &WorkspaceStore, author: u8, channel: u8) -> PathBuf {
    let lineage = store.root.join("retained/v2").join(format!(
        "{}-{}",
        hex::encode([author; 32]),
        hex::encode([channel; 32])
    ));
    let name = "00000000000000000000000000000000";
    fs::create_dir_all(lineage.join(name)).unwrap();
    fs::write(lineage.join("CURRENT"), name).unwrap();
    lineage.join(name)
}
/// A pending entry at the exact path `enqueue_pending` names, written without
/// its per-entry durability barrier: backlog fixtures of thousands of entries
/// test replay bounds, not the fsync cost of building them.
fn raw_pending(store: &WorkspaceStore, device: &[u8; 32], seq: u64, bytes: &[u8]) -> PathBuf {
    let directory = store.pending_dir_for(device);
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("{seq:016x}-{}.delta", blake3::hash(bytes).to_hex()));
    fs::write(&path, bytes).unwrap();
    path
}
fn snapshot(store: &WorkspaceStore) -> ReplaySnapshot {
    let mut build = store
        .start_replay_capture(ReplayId(rand::random()), [2; 32])
        .unwrap();
    loop {
        if let Step::Done(snapshot) = store
            .capture_replay_step(&mut build, WorkLimit::default())
            .unwrap()
        {
            return snapshot;
        }
    }
}
fn prepare(store: &WorkspaceStore, snapshot: &mut ReplaySnapshot) -> ReplaySummary {
    loop {
        if let Step::Done(summary) = store
            .prepare_replay_step(snapshot, WorkLimit::default())
            .unwrap()
        {
            return summary;
        }
        assert!(snapshot.stats().last_step_work <= 128);
    }
}
fn one_entry() -> WorkLimit {
    WorkLimit {
        entries: 1,
        ..WorkLimit::default()
    }
}
#[test]
fn bounded_snapshot_pins_orders_counts_and_hydrates_only_its_batch() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    let retained = raw_generation(&store, 0, 9);
    for i in (0..300).rev() {
        fs::write(retained.join(format!("{i:016x}.delta")), frame(1, 2048, i)).unwrap();
    }
    for i in 0..1500 {
        raw_pending(&store, &[2; 32], i, &[0]);
    }
    let pending = store
        .enqueue_pending(&[2; 32], 2000, &frame(2, 2048, 2000))
        .unwrap();
    let mut snapshot = snapshot(&store);
    // Its preparation pins what the capture listed (until then retention
    // collects nothing: `retention_collects_what_a_capture_listed_only_once_
    // it_is_pinned`); from then on the replay needs no source.
    assert_eq!(prepare(&store, &mut snapshot).total_frames, 301);
    fs::remove_file(retained.join("0000000000000000.delta")).unwrap();
    assert_eq!(snapshot.stats().hydration_reads, 0);
    let budget = ReplayBudget::new(256 * 1024 * 1024);
    let batch = store
        .read_replay_batch(
            &mut snapshot,
            ReplayLimits {
                max_frames: 8,
                ..ReplayLimits::default()
            },
            &budget,
        )
        .unwrap();
    assert_eq!(batch.frames.len(), 8);
    assert_eq!(snapshot.stats().hydration_reads, 8);
    for (i, env) in batch.frames.iter().enumerate() {
        assert!(matches!(env, Envelope::Delta {authored_at_ns, ..} if *authored_at_ns == i as u64));
    }
    assert!(budget.used() > 8 * 2048);
    drop(batch);
    assert_eq!(budget.used(), 0);
    assert!(pending.exists());
    assert!(snapshot.stats().peak_metadata_entries <= 128);
    assert!(snapshot.stats().peak_frame_bytes < 40_000);
    let mut discard = snapshot.into_discard().unwrap();
    while matches!(discard.step(WorkLimit::default()).unwrap(), Step::More) {}
    assert!(pending.exists());
}
#[test]
fn collisions_malformed_twins_and_replaced_pending_never_authorize_retirement() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    let first = frame(1, 10, 1);
    let second = frame(2, 10, 2);
    let same_body_new_publication = frame(1, 10, 2);
    for (name, bytes) in [
        ("a", first.as_slice()),
        ("b", second.as_slice()),
        ("c", first.as_slice()),
        ("d", b"bad".as_slice()),
        ("e", same_body_new_publication.as_slice()),
    ] {
        fs::write(
            raw_generation(&store, 0, 9).join(format!("{name}.delta")),
            bytes,
        )
        .unwrap();
    }
    let duplicate = store.enqueue_pending(&[2; 32], 1, &first).unwrap();
    let replaced = store.enqueue_pending(&[2; 32], 2, &second).unwrap();
    let bad = store.enqueue_pending(&[2; 32], 3, b"bad").unwrap();
    let mut snapshot = snapshot(&store);
    snapshot.digest = |_| [0; 32];
    assert_eq!(prepare(&store, &mut snapshot).total_frames, 3);
    // Same bytes at a new inode are a new identity and must survive old cleanup.
    fs::remove_file(&replaced).unwrap();
    fs::write(&replaced, &second).unwrap();
    assert!(duplicate.exists());
    assert!(
        store
            .cleanup_replay_step(&mut snapshot, one_entry())
            .is_ok()
    );
    while matches!(
        store
            .cleanup_replay_step(&mut snapshot, one_entry())
            .unwrap(),
        Step::More
    ) {}
    assert!(!duplicate.exists());
    assert!(replaced.exists());
    assert!(bad.exists());
}
#[test]
fn oversized_batch_budget_and_overlimit_files_are_bounded() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    for (i, size) in [(1, 10), (2, 2 * 1024 * 1024), (3, 10)] {
        store
            .enqueue_pending(&[2; 32], i, &frame(1, size, i))
            .unwrap();
    }
    let huge = store
        .pending_dir_for(&[2; 32])
        .join("ffffffffffffffff.delta");
    File::create(&huge)
        .unwrap()
        .set_len(transport::MAX_FRAME_BYTES as u64 + 100)
        .unwrap();
    let mut snapshot = snapshot(&store);
    assert_eq!(prepare(&store, &mut snapshot).total_frames, 3);
    let budget = ReplayBudget::new(256 * 1024 * 1024);
    let small_budget = ReplayBudget::new(1);
    assert!(
        store
            .read_replay_batch(&mut snapshot, ReplayLimits::default(), &small_budget)
            .is_err()
    );
    for expected in [1, 2, 3] {
        let batch = store
            .read_replay_batch(&mut snapshot, ReplayLimits::default(), &budget)
            .unwrap();
        assert_eq!(batch.frames.len(), 1);
        assert!(
            matches!(&batch.frames[0],Envelope::Delta{authored_at_ns,..} if *authored_at_ns == expected)
        );
        assert_eq!(batch.eof, expected == 3);
    }
    while matches!(
        store
            .cleanup_replay_step(&mut snapshot, WorkLimit::default())
            .unwrap(),
        Step::More
    ) {}
    assert!(huge.exists());
}
#[test]
fn replay_history_payload_and_metadata_regression() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    let retained = raw_generation(&store, 0, 9);
    for i in 0..600 {
        fs::write(retained.join(format!("{i:016x}.delta")), frame(1, 8192, i)).unwrap();
    }
    let mut replay = snapshot(&store);
    assert_eq!(prepare(&store, &mut replay).total_frames, 600);
    let live_payload = replay.stats().peak_frame_bytes;
    let resident_metadata = replay.stats().peak_metadata_entries;
    assert!(
        live_payload <= 4 * 8400,
        "live replay bytes {live_payload}; resident entries {resident_metadata}"
    );
    assert!(
        resident_metadata <= 128,
        "resident replay entries {resident_metadata}"
    );
}
#[test]
fn pinned_generation_survives_gc_and_restart_reclaims_only_owned_work() {
    use automerge::transaction::Transactable;
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    let mut document = automerge::AutoCommit::new();
    document.put(automerge::ROOT, "first", 1).unwrap();
    let make_checkpoint = |body| {
        transport::encode(&Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: [1; 32],
            authored_at_ns: 0,
            signature: [0; 64],
            delta: CoeditPayload::Checkpoint {
                channel: [9; 32],
                body,
            }
            .encode()
            .unwrap(),
        })
        .unwrap()
    };
    let old = make_checkpoint(document.save());
    store.retain_checkpoint(&[9; 32], &old).unwrap();
    let mut pinned = snapshot(&store);
    document.put(automerge::ROOT, "second", 2).unwrap();
    store
        .retain_checkpoint(&[9; 32], &make_checkpoint(document.save()))
        .unwrap();
    assert_eq!(prepare(&store, &mut pinned).total_frames, 1);
    let batch = store
        .read_replay_batch(
            &mut pinned,
            ReplayLimits::default(),
            &ReplayBudget::new(DEFAULT_OUTPUT_BYTES),
        )
        .unwrap();
    assert_eq!(transport::encode(&batch.frames[0]).unwrap(), old);
    let pending = store.enqueue_pending(&[2; 32], 8, &frame(8, 8, 8)).unwrap();
    let interrupted = store
        .start_replay_capture(ReplayId([5; 16]), [2; 32])
        .unwrap();
    drop(interrupted);
    drop(pinned);
    let unknown = store.root.join("store-work-v1/unknown");
    fs::create_dir(&unknown).unwrap();
    fs::write(unknown.join("keep"), b"unknown").unwrap();
    let reopened = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    let mut gc = reopened.start_work_reclamation().unwrap();
    while matches!(gc.step(one_entry()).unwrap(), Step::More) {}
    assert!(pending.exists());
    assert!(unknown.join("keep").exists());
    assert_eq!(
        fs::read_dir(store.root.join("store-work-v1"))
            .unwrap()
            .count(),
        1
    );
}
#[test]
fn retention_rejects_oversized_proof_allocation_and_preserves_unknown_record() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    store.retain_delta(&[9; 32], 1, &frame(1, 8, 1)).unwrap();
    let lineage = store.root.join("retained/v2").join(format!(
        "{}-{}",
        hex::encode([1; 32]),
        hex::encode([9; 32])
    ));
    let active = retained::current(&lineage).unwrap().unwrap();
    let large = active.join("0000000000000002.delta");
    File::create(&large)
        .unwrap()
        .set_len(transport::MAX_FRAME_BYTES as u64 + 10)
        .unwrap();
    let mut build = store
        .start_retain(&[9; 32], Some(3), frame(3, 8, 3))
        .unwrap();
    while matches!(build.step(WorkLimit::default()).unwrap(), Step::More) {}
    assert!(large.exists());
    assert!(
        build.stats().peak_frame_bytes < 4096,
        "proof hydrated {} bytes",
        build.stats().peak_frame_bytes
    );
}

#[test]
fn replay_budget_includes_transport_encoder_allocation_capacity() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    let bytes = frame(42, 100_000, 1);
    store.enqueue_pending(&[2; 32], 1, &bytes).unwrap();
    let mut snapshot = snapshot(&store);
    prepare(&store, &mut snapshot);
    let budget = ReplayBudget::new(DEFAULT_OUTPUT_BYTES);
    let batch = store
        .read_replay_batch(&mut snapshot, ReplayLimits::default(), &budget)
        .unwrap();
    let envelope = batch.front().unwrap();
    let encoded = transport::encode(envelope).unwrap();
    let decoded = match envelope {
        Envelope::Delta { delta, .. } => delta.capacity(),
        _ => unreachable!(),
    };
    // The encoder serializes straight into one growable framed Vec; at its
    // last growth the outgrown buffer (fewer than `len` bytes) coexists with
    // the final allocation.
    let overlap = decoded + std::mem::size_of::<Envelope>() + encoded.len() + encoded.capacity();
    assert!(
        batch.charged_bytes >= overlap,
        "reserved {}, actual encode overlap {overlap}",
        batch.charged_bytes
    );
}

#[test]
fn partial_cleanup_restart_preserves_remaining_pending_and_unknown_snapshot_files() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    let mut paths = Vec::new();
    for i in 0..3 {
        paths.push(
            store
                .enqueue_pending(&[2; 32], i, &frame(1, 16, i))
                .unwrap(),
        );
    }
    let mut snapshot = snapshot(&store);
    prepare(&store, &mut snapshot);
    assert!(matches!(
        store
            .cleanup_replay_step(&mut snapshot, one_entry())
            .unwrap(),
        Step::More
    ));
    assert!(!paths[0].exists());
    assert!(paths[1].exists());
    assert!(paths[2].exists());
    let unknown = snapshot.root.join("pins/unknown");
    fs::create_dir(&unknown).unwrap();
    fs::write(unknown.join("keep"), b"unknown").unwrap();
    drop(snapshot);
    let mut restart = store.start_work_reclamation().unwrap();
    while matches!(restart.step(one_entry()).unwrap(), Step::More) {}
    assert!(paths[1].exists());
    assert!(paths[2].exists());
    assert!(unknown.join("keep").exists());
    let mut reconnect = snapshot_for_device(&store);
    assert_eq!(prepare(&store, &mut reconnect).total_frames, 2);
}
fn snapshot_for_device(store: &WorkspaceStore) -> ReplaySnapshot {
    snapshot(store)
}

#[test]
fn noncanonical_prefix_and_generic_envelope_do_not_prove_pending_retirement() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    let mut malformed = frame(1, 16, 1);
    malformed[0] ^= 1;
    let generic = transport::encode(&Envelope::Ping).unwrap();
    let retained = raw_generation(&store, 0, 9);
    fs::write(retained.join("a.delta"), &malformed).unwrap();
    fs::write(retained.join("b.delta"), &generic).unwrap();
    let bad = store.enqueue_pending(&[2; 32], 1, &malformed).unwrap();
    let valid = store.enqueue_pending(&[2; 32], 2, &generic).unwrap();
    let mut snapshot = snapshot(&store);
    snapshot.digest = |_| [0; 32];
    assert_eq!(
        prepare(&store, &mut snapshot).total_frames,
        2,
        "generic retained data cannot omit matching pending replay"
    );
    while matches!(
        store
            .cleanup_replay_step(&mut snapshot, WorkLimit::default())
            .unwrap(),
        Step::More
    ) {}
    assert!(bad.exists());
    assert!(!valid.exists());
}

#[test]
fn capture_preserves_lineage_checkpoint_tail_and_filename_order() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    for (channel, stamp) in [(0x0b, 20), (0x0a, 10)] {
        let generation = raw_generation(&store, 0, channel);
        fs::write(generation.join("checkpoint.delta"), frame(1, 8, stamp)).unwrap();
        fs::write(generation.join("2.delta"), frame(1, 8, stamp + 2)).unwrap();
        fs::write(generation.join("1.delta"), frame(1, 8, stamp + 1)).unwrap();
    }
    store.retain_delta(&[9; 32], 999, &frame(1, 8, 30)).unwrap();
    store.retain_delta(&[9; 32], 1, &frame(1, 8, 31)).unwrap();
    store
        .enqueue_pending(&[2; 32], 1, &frame(1, 8, 40))
        .unwrap();
    let mut snapshot = snapshot(&store);
    assert_eq!(prepare(&store, &mut snapshot).total_frames, 9);
    let budget = ReplayBudget::new(DEFAULT_OUTPUT_BYTES);
    let mut stamps = Vec::new();
    loop {
        let batch = store
            .read_replay_batch(&mut snapshot, ReplayLimits::default(), &budget)
            .unwrap();
        for frame in &batch.frames {
            if let Envelope::Delta { authored_at_ns, .. } = frame {
                stamps.push(*authored_at_ns);
            }
        }
        if batch.eof {
            break;
        }
    }
    assert_eq!(stamps, vec![10, 11, 12, 20, 21, 22, 30, 31, 40]);
}

/// A shell drops what a device published until it holds that device's
/// certificate, so the lineage of a device's identity channel replays first,
/// whatever its hash orders it among the other channels.
#[test]
fn capture_replays_certificate_lineages_first() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    let edits = raw_generation(&store, 0, 0x0a);
    fs::write(edits.join("checkpoint.delta"), frame(1, 8, 10)).unwrap();
    fs::write(edits.join("1.delta"), frame(1, 8, 11)).unwrap();
    let author = [0xff; 32];
    let mut route = b"identity-hello\0".to_vec();
    route.extend_from_slice(&author);
    let lineage = store.root.join("retained/v2").join(format!(
        "{}-{}",
        hex::encode(author),
        blake3::hash(&route).to_hex()
    ));
    let name = "00000000000000000000000000000000";
    fs::create_dir_all(lineage.join(name)).unwrap();
    fs::write(lineage.join("CURRENT"), name).unwrap();
    fs::write(lineage.join(name).join("checkpoint.delta"), frame(2, 8, 50)).unwrap();
    assert_eq!(
        lineage_rank(&format!("{}-{}", hex::encode(author), hex::encode([9; 32]))),
        "1"
    );

    let mut snapshot = snapshot(&store);
    assert_eq!(prepare(&store, &mut snapshot).total_frames, 3);
    let budget = ReplayBudget::new(DEFAULT_OUTPUT_BYTES);
    let mut stamps = Vec::new();
    loop {
        let batch = store
            .read_replay_batch(&mut snapshot, ReplayLimits::default(), &budget)
            .unwrap();
        for frame in &batch.frames {
            if let Envelope::Delta { authored_at_ns, .. } = frame {
                stamps.push(*authored_at_ns);
            }
        }
        if batch.eof {
            break;
        }
    }
    assert_eq!(stamps, vec![50, 10, 11]);
}

#[test]
fn interrupted_capture_sort_and_bounded_generation_gc_preserve_unknown_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    let retained = raw_generation(&store, 0, 9);
    for i in 0..260 {
        fs::write(retained.join(format!("{i:016x}.delta")), [0]).unwrap();
    }
    // The capture lists the retained frames; its preparation pins them and
    // orders them, spilling past one run.
    let mut capture = snapshot(&store);
    for _ in 0..100 {
        if capture.sorter.as_ref().unwrap().stats.peak_metadata_entries >= 128 {
            break;
        }
        assert!(matches!(
            store
                .prepare_replay_step(&mut capture, WorkLimit::default())
                .unwrap(),
            Step::More
        ));
    }
    let root = capture.root.clone();
    assert!(
        root.join("sort/0000000000000000-0000000000000000.run")
            .exists()
    );
    let mut discard = capture.into_discard().unwrap();
    while matches!(discard.step(one_entry()).unwrap(), Step::More) {}
    assert!(!root.exists());
    assert_eq!(fs::read_dir(&retained).unwrap().count(), 260);
    store.retain_delta(&[9; 32], 1, &frame(1, 8, 1)).unwrap();
    let lineage = store.root.join("retained/v2").join(format!(
        "{}-{}",
        hex::encode([1; 32]),
        hex::encode([9; 32])
    ));
    let obsolete = lineage.join("77777777777777777777777777777777");
    fs::create_dir(&obsolete).unwrap();
    for i in 0..260 {
        fs::write(obsolete.join(format!("{i:016x}.delta")), [0]).unwrap();
    }
    fs::create_dir(obsolete.join("unknown")).unwrap();
    fs::write(obsolete.join("unknown/keep"), b"keep").unwrap();
    let mut gc = retained::GenerationGc::new(lineage).unwrap();
    let mut previous = 260;
    loop {
        let done = matches!(gc.step(one_entry()).unwrap(), Step::Done(()));
        let left = fs::read_dir(&obsolete)
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().file_type().unwrap().is_file())
            .count();
        assert!(
            previous - left <= 1,
            "one-entry quantum deleted more than one record"
        );
        previous = left;
        if done {
            break;
        }
    }
    assert_eq!(previous, 0);
    assert!(obsolete.join("unknown/keep").exists());
}

/// A newcomer's capture runs in its workspace owner's turn, which must not
/// grow with the room: it lists the retained frames, and pins none. Its
/// preparation, off the owner, pins every one of them.
#[test]
fn a_capture_lists_the_rooms_frames_and_its_preparation_pins_them() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    for author in 0..64 {
        let generation = raw_generation(&store, author, 9);
        fs::write(
            generation.join("0000000000000000.delta"),
            frame(author, 8, 1),
        )
        .unwrap();
    }
    let pins =
        |snapshot: &ReplaySnapshot| fs::read_dir(snapshot.root.join("pins")).unwrap().count();
    let mut captured = snapshot(&store);
    assert_eq!(
        pins(&captured),
        0,
        "the owner's turn pinned retained frames"
    );
    assert_eq!(prepare(&store, &mut captured).total_frames, 64);
    assert_eq!(pins(&captured), 64);
}

/// What a capture listed stays where it was listed until it is pinned:
/// retention replacing a lineage meanwhile leaves the replaced generation
/// for later, collects it once the capture pinned it, and the capture
/// replays what it listed.
#[test]
fn retention_collects_what_a_capture_listed_only_once_it_is_pinned() {
    use automerge::transaction::Transactable;
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    let checkpoint = |body| {
        transport::encode(&Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: [1; 32],
            authored_at_ns: 0,
            signature: [0; 64],
            delta: CoeditPayload::Checkpoint {
                channel: [9; 32],
                body,
            }
            .encode()
            .unwrap(),
        })
        .unwrap()
    };
    let mut document = automerge::AutoCommit::new();
    document.put(automerge::ROOT, "first", 1).unwrap();
    let old = checkpoint(document.save());
    store.retain_checkpoint(&[9; 32], &old).unwrap();
    let lineage = store.root.join("retained/v2").join(format!(
        "{}-{}",
        hex::encode([1; 32]),
        hex::encode([9; 32])
    ));
    let listed = retained::current(&lineage).unwrap().unwrap();
    let mut captured = snapshot(&store);
    document.put(automerge::ROOT, "second", 2).unwrap();
    store
        .retain_checkpoint(&[9; 32], &checkpoint(document.save()))
        .unwrap();
    assert_ne!(retained::current(&lineage).unwrap().unwrap(), listed);
    assert!(
        listed.exists(),
        "a listed generation was collected before it was pinned"
    );
    assert!(store.deferred_collections().is_empty());
    assert_eq!(prepare(&store, &mut captured).total_frames, 1);
    let batch = store
        .read_replay_batch(
            &mut captured,
            ReplayLimits::default(),
            &ReplayBudget::new(DEFAULT_OUTPUT_BYTES),
        )
        .unwrap();
    assert_eq!(transport::encode(&batch.frames[0]).unwrap(), old);
    assert_eq!(store.deferred_collections(), vec![lineage.clone()]);
    let mut gc = retained::GenerationGc::new(lineage).unwrap();
    while matches!(gc.step(WorkLimit::default()).unwrap(), Step::More) {}
    assert!(!listed.exists());
}

#[cfg(unix)]
struct RestoreDirectoryPermissions {
    path: PathBuf,
    permissions: fs::Permissions,
}
#[cfg(unix)]
impl Drop for RestoreDirectoryPermissions {
    fn drop(&mut self) {
        fs::set_permissions(&self.path, self.permissions.clone()).unwrap();
    }
}
#[cfg(unix)]
fn deny_directory_mutation(path: &Path) -> Option<RestoreDirectoryPermissions> {
    use std::os::unix::fs::PermissionsExt;
    let restore = RestoreDirectoryPermissions {
        path: path.into(),
        permissions: fs::metadata(path).unwrap().permissions(),
    };
    fs::set_permissions(path, fs::Permissions::from_mode(0o500)).unwrap();
    // Privileged test processes may bypass mode bits. Never claim that such a
    // run exercised PermissionDenied; the normal non-root runner uses real I/O.
    let probe = path.join("permission-probe");
    match File::create(&probe) {
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Some(restore),
        Ok(file) => {
            drop(file);
            fs::remove_file(probe).unwrap();
            eprintln!("skipping mode-bit failure fixture: process bypasses directory permissions");
            None
        }
        Err(error) => panic!("unexpected permission probe error: {error}"),
    }
}
#[cfg(unix)]
fn reach_unlink_failure(mut step: impl FnMut() -> Result<Step<()>, StoreError>) {
    for _ in 0..100 {
        match step() {
            Err(StoreError::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied => return,
            Ok(Step::More) => {}
            outcome => panic!("expected real transient unlink failure, got {outcome:?}"),
        }
    }
    panic!("reclamation did not reach the blocked file");
}
#[cfg(unix)]
fn finish_reclamation(mut step: impl FnMut() -> Result<Step<()>, StoreError>) {
    for _ in 0..100 {
        if matches!(step().unwrap(), Step::Done(())) {
            return;
        }
    }
    panic!("reclamation did not finish after permissions were restored");
}
/// Creates one recognized work entry, either directly in `root` or in its
/// `pins` child, and returns the directory holding it and the entry path.
#[cfg(unix)]
fn owned_work_entry(root: &Path, child: bool) -> (PathBuf, PathBuf) {
    let directory = if child {
        root.join("pins")
    } else {
        root.to_path_buf()
    };
    fs::create_dir_all(&directory).unwrap();
    let owned = directory.join(if child {
        "0000000000000000.frame"
    } else {
        "replay.manifest"
    });
    fs::write(&owned, b"owned snapshot bytes").unwrap();
    (directory, owned)
}
/// Blocks unlinking in `directory`, drives `step` into the real failure, then
/// restores permissions and requires `step` to retire `owned`. Returns false
/// when the process bypasses mode bits and the fixture cannot run.
#[cfg(unix)]
fn retries_blocked_unlink(
    directory: &Path,
    owned: &Path,
    mut step: impl FnMut(WorkLimit) -> Result<Step<()>, StoreError>,
    message: &str,
) -> bool {
    let Some(restore) = deny_directory_mutation(directory) else {
        return false;
    };
    reach_unlink_failure(|| step(one_entry()));
    assert!(owned.exists());
    drop(restore);
    finish_reclamation(|| step(one_entry()));
    assert!(!owned.exists(), "{message}");
    true
}
#[test]
#[cfg(unix)]
fn direct_discard_retries_exact_entry_after_transient_unlink_failure() {
    for child in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp
            .path()
            .join("store-work-v1/replay-11111111111111111111111111111111");
        let (directory, owned) = owned_work_entry(&root, child);
        let mut discard = WorkDiscard::new(root.clone()).unwrap();
        if retries_blocked_unlink(
            &directory,
            &owned,
            |limit| discard.step(limit),
            "Done skipped the previously failed recognized file",
        ) {
            assert!(!root.exists());
        }
    }
}
#[test]
#[cfg(unix)]
fn startup_reclaimer_retries_exact_entry_after_transient_unlink_failure() {
    for child in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let pending = store
            .enqueue_pending(&[2; 32], 1, &frame(1, 16, 1))
            .unwrap();
        let root = store
            .root
            .join("store-work-v1/replay-11111111111111111111111111111111");
        let (directory, owned) = owned_work_entry(&root, child);
        let mut reclaimer = store.start_work_reclamation().unwrap();
        if retries_blocked_unlink(
            &directory,
            &owned,
            |limit| reclaimer.step(limit),
            "startup Done skipped the previously failed recognized file",
        ) {
            assert!(!root.exists());
            assert!(pending.exists());
        }
    }
}
#[test]
#[cfg(unix)]
fn generation_gc_retries_exact_entry_after_transient_unlink_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    store.retain_delta(&[9; 32], 1, &frame(1, 16, 1)).unwrap();
    let lineage = store.root.join("retained/v2").join(format!(
        "{}-{}",
        hex::encode([1; 32]),
        hex::encode([9; 32])
    ));
    let active = retained::current(&lineage).unwrap().unwrap();
    let obsolete = lineage.join("11111111111111111111111111111111");
    fs::create_dir(&obsolete).unwrap();
    let owned = obsolete.join("0000000000000000.delta");
    fs::write(&owned, b"old generation").unwrap();
    fs::write(obsolete.join("unknown"), b"preserve").unwrap();
    let mut gc = retained::GenerationGc::new(lineage).unwrap();
    if retries_blocked_unlink(
        &obsolete,
        &owned,
        |limit| gc.step(limit),
        "generation Done skipped the previously failed record",
    ) {
        assert!(obsolete.join("unknown").exists());
        assert!(active.join("0000000000000001.delta").exists());
    }
}

#[test]
fn replay_budget_includes_v2_wire_encoding_overlap() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    store
        .enqueue_pending(&[2; 32], 1, &frame(42, 100_000, 1))
        .unwrap();
    let mut snapshot = snapshot(&store);
    prepare(&store, &mut snapshot);
    let batch = store
        .read_replay_batch(
            &mut snapshot,
            ReplayLimits::default(),
            &ReplayBudget::new(DEFAULT_OUTPUT_BYTES),
        )
        .unwrap();
    let envelope = batch.front().unwrap();
    let payload = postcard::to_allocvec(&tilo_delivery_protocol::transport_v2::EnvelopeV2::State(
        envelope.clone(),
    ))
    .unwrap();
    let wire = tilo_delivery_protocol::transport_v2::encode_state(envelope).unwrap();
    let overlap = heap_bytes(envelope)
        + std::mem::size_of::<Envelope>()
        + payload.capacity()
        + wire.capacity();
    assert!(
        batch.charged_bytes >= overlap,
        "v2 reservation {} omitted known encode overlap {overlap}",
        batch.charged_bytes
    );
}

#[test]
fn disposable_reclamation_treats_disappeared_recognized_entries_as_completed() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    let work = store.root().join("store-work-v1");
    fs::create_dir_all(&work).unwrap();
    let root = work.join("replay-00000000000000000000000000000001");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("replay.manifest"), b"old metadata").unwrap();
    let mut discard = WorkDiscard::new(root.clone()).unwrap();
    let path = discard
        .outer
        .as_mut()
        .unwrap()
        .peek()
        .unwrap()
        .unwrap()
        .path();
    fs::remove_file(path).unwrap();
    let result = discard.step(WorkLimit::default());
    assert!(
        matches!(result, Ok(Step::Done(()))),
        "already absent recognized file remained a retry error: {result:?}"
    );
    let mut missing = WorkDiscard::new(root).expect("already absent spool root should be disposed");
    assert!(matches!(
        missing.step(WorkLimit::default()).unwrap(),
        Step::Done(())
    ));
}

#[test]
fn startup_reclamation_treats_disappeared_selected_work_root_as_completed() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    let root = store
        .root()
        .join("store-work-v1/replay-00000000000000000000000000000001");
    fs::create_dir_all(root.join("pins")).unwrap();
    fs::write(root.join("pins/0000000000000001.frame"), b"old pin").unwrap();
    let unknown = root.parent().unwrap().join("unknown");
    fs::write(&unknown, b"preserve").unwrap();
    let mut startup = store.start_work_reclamation().unwrap();
    while startup
        .entries
        .as_mut()
        .unwrap()
        .peek()
        .unwrap()
        .unwrap()
        .path()
        != root
    {
        startup.entries.as_mut().unwrap().advance();
    }
    let mut discard = WorkDiscard::new(root).unwrap();
    while matches!(discard.step(WorkLimit::default()).unwrap(), Step::More) {}
    let result = startup.step(WorkLimit::default());
    assert!(
        result.is_ok(),
        "already disposed selected root remained a retry error: {result:?}"
    );
    while matches!(startup.step(WorkLimit::default()).unwrap(), Step::More) {}
    assert!(unknown.exists());
}

#[test]
fn disposable_reclamation_finishes_when_selected_child_and_root_disappear() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    let root = store
        .root()
        .join("store-work-v1/replay-00000000000000000000000000000001");
    fs::create_dir_all(root.join("pins")).unwrap();
    fs::write(root.join("pins/0000000000000001.frame"), b"old pin").unwrap();
    let mut later = WorkDiscard::new(root.clone()).unwrap();
    later.step(one_entry()).unwrap();
    later.child.as_mut().unwrap().1.peek().unwrap();
    let mut earlier = WorkDiscard::new(root).unwrap();
    while matches!(earlier.step(WorkLimit::default()).unwrap(), Step::More) {}
    assert!(matches!(
        later.step(WorkLimit::default()).unwrap(),
        Step::Done(())
    ));
}

#[test]
fn unproved_coedit_wrapper_cannot_supply_replay_duplicate_coverage() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(temp.path(), 7, false).unwrap();
    let bytes = super::super::test_support::wrapper_suffix(&frame(1, 10, 1), false);
    store.retain_delta(&[9; 32], 1, &bytes).unwrap();
    let pending = store.enqueue_pending(&[2; 32], 1, &bytes).unwrap();
    let mut snapshot = snapshot(&store);
    assert_eq!(
        prepare(&store, &mut snapshot).total_frames,
        2,
        "unproved wrappers must be replayed independently"
    );
    assert!(pending.exists());
    assert!(store.load_retained().unwrap().contains(&bytes));
}

/// A frame of `author` on `channel`, as `frame` makes one.
fn authored(author: u8, channel: u8, value: u8, timestamp: u64) -> ([u8; 32], Vec<u8>) {
    let bytes = transport::encode(&Envelope::Delta {
        workspace_id: 7,
        author_device_public_key: [author; 32],
        authored_at_ns: timestamp,
        signature: [0; 64],
        delta: CoeditPayload::RetainedDelta {
            channel: [channel; 32],
            body: vec![value; 10],
        }
        .encode()
        .unwrap(),
    })
    .unwrap();
    ([channel; 32], bytes)
}

/// The lineage directory name of `author` on `channel`.
fn lineage_name(author: u8, channel: u8) -> OsString {
    format!(
        "{}-{}",
        hex::encode([author; 32]),
        hex::encode([channel; 32])
    )
    .into()
}

fn listing_hits() -> u64 {
    LISTING_HITS.with(std::cell::Cell::get)
}

/// A capture lists each lineage once; later captures list it from memory
/// until the owner reports it changed (or reports nothing), and list what
/// changed from its generation again.
#[test]
fn captures_remember_each_lineage_until_it_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    let (first, a1) = authored(1, 9, 1, 1);
    let (second, b1) = authored(2, 8, 2, 2);
    store.retain_delta(&first, 1, &a1).unwrap();
    store.retain_delta(&second, 2, &b1).unwrap();
    let before = listing_hits();
    let mut listed = snapshot(&store);
    assert_eq!(prepare(&store, &mut listed).total_frames, 2);
    assert_eq!(
        listing_hits() - before,
        0,
        "the first capture lists every lineage"
    );
    assert_eq!(
        store.listed_lineage_names(),
        [lineage_name(1, 9), lineage_name(2, 8)]
    );

    let mut remembered = snapshot(&store);
    assert_eq!(prepare(&store, &mut remembered).total_frames, 2);
    assert_eq!(listing_hits() - before, 2, "both lineages come from memory");

    // The owner job retaining to the first lineage reports it.
    let (_, a2) = authored(1, 9, 3, 3);
    store.retain_delta(&first, 3, &a2).unwrap();
    store.forget_listed(Some(&[lineage_name(1, 9)]));
    let mut changed = snapshot(&store);
    assert_eq!(prepare(&store, &mut changed).total_frames, 3);
    assert_eq!(
        listing_hits() - before,
        3,
        "only the unchanged lineage comes from memory"
    );
    let batch = store
        .read_replay_batch(
            &mut changed,
            ReplayLimits::default(),
            &ReplayBudget::new(1024 * 1024),
        )
        .unwrap();
    let times: Vec<u64> = batch
        .frames
        .iter()
        .map(|frame| match frame {
            Envelope::Delta { authored_at_ns, .. } => *authored_at_ns,
            _ => 0,
        })
        .collect();
    assert!(times.contains(&3), "the new frame is replayed: {times:?}");
    drop(batch);

    // A job that says nothing of what it changed forgets everything.
    store.forget_listed(None);
    assert!(store.listed_lineage_names().is_empty());
    let mut relisted = snapshot(&store);
    assert_eq!(prepare(&store, &mut relisted).total_frames, 3);
    assert_eq!(listing_hits() - before, 3);
}

/// A device's access request for a channel, framed as `author` retains it.
fn access_request(author: &ed25519_dalek::SigningKey, timestamp: u64) -> ([u8; 32], Vec<u8>) {
    let message = neighbour_wire::NeighbourMsg::MlsAccessRequest {
        room: "room".into(),
        request: Box::new(
            collab_types::MlsAccessRequestV1::signed(
                collab_types::ChannelId::new(),
                vec![1],
                1,
                author,
            )
            .unwrap(),
        ),
        delivery: 0,
    };
    let (channel, _) = neighbour_wire::retention(&message, false).unwrap();
    let bytes = transport::encode(&Envelope::Delta {
        workspace_id: 7,
        author_device_public_key: author.verifying_key().to_bytes(),
        authored_at_ns: timestamp,
        signature: [0; 64],
        delta: CoeditPayload::Checkpoint {
            channel,
            body: postcard::to_stdvec(&message).unwrap(),
        }
        .encode()
        .unwrap(),
    })
    .unwrap();
    (channel, bytes)
}

/// In a room its host governs (and so stewards), a capture leaves out what
/// only the steward reads, from memory once it read a lineage, and still
/// replays everything else; the requests stay retained. Elsewhere a capture
/// replays them as before.
#[test]
fn a_governed_rooms_capture_leaves_out_what_only_its_steward_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
    let member = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
    let (channel, request) = access_request(&member, 1);
    store.retain_checkpoint(&channel, &request).unwrap();
    store.retain_delta(&[9; 32], 2, &frame(1, 10, 2)).unwrap();
    let mut whole = snapshot(&store);
    assert_eq!(
        prepare(&store, &mut whole).total_frames,
        2,
        "a room this host does not govern replays every request"
    );

    store.replay_without_steward_only();
    let mut governed = snapshot(&store);
    assert_eq!(prepare(&store, &mut governed).total_frames, 1);
    let batch = store
        .read_replay_batch(
            &mut governed,
            ReplayLimits::default(),
            &ReplayBudget::new(1024 * 1024),
        )
        .unwrap();
    assert!(
        batch.frames.len() == 1
            && matches!(
                batch.frames.front(),
                Some(Envelope::Delta {
                    authored_at_ns: 2,
                    ..
                })
            ),
        "only the member's edit is replayed"
    );
    drop(batch);
    assert!(
        store.load_retained().unwrap().contains(&request),
        "the request stays retained for the steward"
    );

    // Classified once: a capture decides the lineage from memory after that,
    // without reading what it holds now.
    let lineage = store.root().join("retained/v2").join(format!(
        "{}-{}",
        hex::encode(member.verifying_key().to_bytes()),
        hex::encode(channel)
    ));
    let active = retained::current(&lineage).unwrap().unwrap();
    fs::write(active.join("checkpoint.delta"), frame(3, 10, 3)).unwrap();
    let mut remembered = snapshot(&store);
    assert_eq!(prepare(&store, &mut remembered).total_frames, 1);
}
