//! Collaborative-files performance gate: the work each scenario does.
//!
//! Bytes sent, bytes read, document lookups and nodes examined per lock hold
//! do not depend on the machine or its load, so every ceiling here is a hard
//! assertion on every run. The same scenarios' latency is measured nightly by
//! `performance_timing.rs`. The fixtures are generated from fixed data, use
//! local stores only, and contain no polling, sleeps, peer discovery, or
//! remote requests.

use std::path::Path;
use std::sync::{Arc, Barrier, Mutex};

use fs::document_media::DocumentMedia;
use fs::{ContentStore, Heuristics, Hydrator, Namespace, QuickFS, ROOT_INO};
use iroh::{Endpoint, endpoint::presets, protocol::Router};
use runtime_fs::blobs::{BlobScope, MAX_BLOB_BYTES, MAX_READ_CHUNK, ReadOutcome};
use tilo_lock::MutexRecover as _;

const DIRECTORY_ENTRIES: usize = 1_000;
const LARGE_FILE_BYTES: usize = 64 * 1024 * 1024;
const RANGE_BYTES: u64 = 256 * 1024;
const RANGE_READS: usize = 64;
const RANGES_BYTES: u64 = RANGE_BYTES * RANGE_READS as u64;
const MAX_RANGE_OFFSET: u64 = LARGE_FILE_BYTES as u64 - RANGE_BYTES;
/// Remote renames a sync peer applies while the mount serves ranged reads.
const REMOTE_APPLY_DELTAS: usize = 512;
const WARM_DIRECTORY_PASSES: usize = 8;

/// Namespace changes behind the two writers of the idle anti-entropy pass.
const IDLE_SYNC_HISTORY: u64 = 50_000;
/// One idle pass between two writers in sync, both directions together:
/// heads only, at any history length. A pass resumes where the last one left
/// off; one starting afresh sends a Bloom filter over the whole history.
const IDLE_SYNC_BYTES: usize = 148;
/// The workspace behind the conflict-watch scenario: 50,000
/// files in 500 directories.
const CONFLICT_DIRECTORIES: usize = 500;
const CONFLICT_FILES_PER_DIRECTORY: usize = 100;
/// Nodes a conflict scan may examine under one hold of the namespace lock,
/// whatever `fs::conflict::SCAN_STEP_NODES` says: a FUSE request waits for
/// one hold, so a longer one is a latency regression on every mount.
const CONFLICT_NODES_PER_HOLD: usize = 64;
/// Files in the namespace behind the restart-resync scenario.
const RESTART_RESYNC_FILES: u64 = 50_000;
/// The device keys the two writers know each other by.
const IDLE_SYNC_HOST: [u8; 32] = [1; 32];
const IDLE_SYNC_DEVICE: [u8; 32] = [2; 32];
/// Document lookups to reopen a persisted namespace and list a directory of
/// it, per entry, and to list it again once open: each listing reads every
/// child's name and kind, never the whole document.
const COLD_LISTING_LOOKUPS_PER_ENTRY: u64 = 7;
const WARM_LISTING_LOOKUPS_PER_ENTRY: u64 = 2;

fn deterministic_bytes(len: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes().first().copied().unwrap_or_default()
        })
        .collect()
}

/// The offset of the `index`th of the scenarios' ranged reads.
fn range_offset(index: usize) -> u64 {
    (index as u64)
        .wrapping_mul(1_000_003)
        .checked_rem(MAX_RANGE_OFFSET)
        .unwrap_or_default()
        & !4095
}

/// The bytes of `file` the read at `offset` returns.
fn expected_range(file: &[u8], offset: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    let end = usize::try_from(offset.saturating_add(RANGE_BYTES)).ok()?;
    file.get(start..end)
}

/// The ranged reads read their ranges and the file's manifest, once: never
/// the whole file, nor the manifest again per read.
fn assert_ranged_read_work(name: &str, read: u64, manifest_len: u64) {
    eprintln!(
        "WORK {name}: {read} B read for {RANGES_BYTES} B of ranges ({manifest_len} B manifest)"
    );
    assert!(
        (RANGES_BYTES..=RANGES_BYTES.saturating_add(manifest_len)).contains(&read),
        "{name} read {read} B for {RANGE_READS} ranges of {RANGE_BYTES} B"
    );
}

/// One exchange of sync messages as a session runs it (the client writes,
/// the server answers, until neither has anything to send) between two
/// namespaces in sync. Returns the bytes both sent, or `None` if the
/// exchange failed.
fn idle_namespace_pass(
    (client, client_state): (&mut Namespace, &mut automerge::sync::State),
    (server, server_state): (&mut Namespace, &mut automerge::sync::State),
) -> Option<usize> {
    let mut bytes = 0_usize;
    for _ in 0..64 {
        let message = client.sync_generate(client_state);
        if let Some(message) = &message {
            bytes = bytes.saturating_add(message.len());
            server.sync_receive(server_state, message).ok()?;
        }
        let reply = server.sync_generate(server_state);
        if let Some(reply) = &reply {
            bytes = bytes.saturating_add(reply.len());
            client.sync_receive(client_state, reply).ok()?;
        }
        if message.is_none() && reply.is_none() {
            return Some(bytes);
        }
    }
    None
}

/// #3858: the anti-entropy pass every writer runs with every peer every
/// few seconds, with nothing changed. Starting each pass from a fresh sync
/// state sent a Bloom filter over the namespace's whole history; resuming
/// from the last completed pass sends heads, at any history length.
#[test]
fn idle_namespace_anti_entropy_performance_gate() {
    let mut host = Namespace::new("perf-idle-host").unwrap();
    let mut file = ROOT_INO;
    for change in 0..IDLE_SYNC_HISTORY {
        if change.is_multiple_of(100) {
            file = host
                .create_file(ROOT_INO, format!("file-{change}"), None, 0, change)
                .expect("create history fixture entry");
        } else {
            let content = iroh_blobs::Hash::new(change.to_le_bytes());
            assert!(host.set_content(file, Some(&content), change, change));
        }
        // Reading the heads commits the edit as one change.
        host.heads();
    }
    let mut device =
        Namespace::from_bytes("perf-idle-device", &host.save()).expect("open in-sync device");

    let fresh = idle_namespace_pass(
        (&mut device, &mut automerge::sync::State::new()),
        (&mut host, &mut automerge::sync::State::new()),
    )
    .expect("fresh idle pass");
    eprintln!("WORK idle-namespace-pass-fresh-state-50k: {fresh} B (before #3858)");

    // The production path: each side begins where its last completed
    // exchange with the other left off. The first is a full exchange.
    let mut resumed = None;
    for _ in 0..3 {
        let mut device_state = device.begin_sync_with(&IDLE_SYNC_HOST);
        let mut host_state = host.begin_sync_with(&IDLE_SYNC_DEVICE);
        let pass = idle_namespace_pass(
            (&mut device, &mut device_state),
            (&mut host, &mut host_state),
        )
        .expect("resumed idle pass");
        device.finish_sync_with(IDLE_SYNC_HOST, &device_state);
        host.finish_sync_with(IDLE_SYNC_DEVICE, &host_state);
        resumed = Some(pass);
    }
    let bytes = resumed.expect("a resumed pass ran");
    eprintln!("WORK idle-namespace-pass-resumed-50k: {bytes} B (budget {IDLE_SYNC_BYTES} B)");
    assert!(
        bytes <= IDLE_SYNC_BYTES,
        "an idle pass sent {bytes} B over {IDLE_SYNC_HISTORY} changes ({fresh} B from a fresh state)"
    );
}

/// #4113: a device restarts (an update, a reboot) with 50,000 files in
/// its workspace and resumes its exchange with a peer. Without the sync
/// progress persisted beside the document, the restarted side started from
/// a fresh sync state and summarised the whole history again; with it, the
/// exchange carries heads only.
#[test]
fn restart_resync_performance_gate() {
    let mut host = Namespace::new("perf-restart-host").unwrap();
    for file in 0..RESTART_RESYNC_FILES {
        let content = iroh_blobs::Hash::new(file.to_le_bytes());
        host.create_file(ROOT_INO, format!("file-{file}"), Some(&content), 1, file)
            .expect("create restart fixture file");
        // Reading the heads commits each file as one change.
        host.heads();
    }
    let mut device =
        Namespace::from_bytes("perf-restart-device", &host.save()).expect("open in-sync device");
    let mut device_state = device.begin_sync_with(&IDLE_SYNC_HOST);
    let mut host_state = host.begin_sync_with(&IDLE_SYNC_DEVICE);
    idle_namespace_pass(
        (&mut device, &mut device_state),
        (&mut host, &mut host_state),
    )
    .expect("first contact");
    device.finish_sync_with(IDLE_SYNC_HOST, &device_state);
    host.finish_sync_with(IDLE_SYNC_DEVICE, &host_state);
    let saved = device.save();
    let progress = device.encode_peer_sync_states();

    // The restart as it was: the device remembers nothing, the host does.
    let mut forgetful = Namespace::from_bytes("perf-restart-device", &saved).unwrap();
    let mut host_state = host.begin_sync_with(&IDLE_SYNC_DEVICE);
    let full_bytes = idle_namespace_pass(
        (&mut forgetful, &mut automerge::sync::State::new()),
        (&mut host, &mut host_state),
    )
    .expect("restart without progress");
    eprintln!("WORK restart-resync-without-progress-50k-files: {full_bytes} B (before #4113)");

    let mut restarted = Namespace::from_bytes("perf-restart-device", &saved).unwrap();
    assert_eq!(restarted.restore_peer_sync_states(&progress).ok(), Some(1));
    let mut device_state = restarted.begin_sync_with(&IDLE_SYNC_HOST);
    let mut host_state = host.begin_sync_with(&IDLE_SYNC_DEVICE);
    let bytes = idle_namespace_pass(
        (&mut restarted, &mut device_state),
        (&mut host, &mut host_state),
    )
    .expect("restart with progress");
    eprintln!(
        "WORK restart-resync-with-progress-50k-files: {bytes} B (budget {IDLE_SYNC_BYTES} B)"
    );
    assert!(
        bytes <= IDLE_SYNC_BYTES,
        "a restarted device sent {bytes} B over {RESTART_RESYNC_FILES} files ({full_bytes} B without its progress)"
    );
}

/// #3985: the conflict step every writer's reconciliation loop runs every
/// few seconds. It used to walk every node under one hold of the namespace
/// lock on every pass. Now a pass with nothing merged since the last scan
/// reads the namespace's merge generation and examines no node, and a scan
/// (after a merge, a retry, or the periodic sweep) walks the nodes a step
/// at a time, releasing the lock between steps.
#[test]
fn conflict_watch_idle_pass_and_lock_hold_performance_gate() {
    let mut namespace = Namespace::new("perf-conflict").unwrap();
    for directory in 0..CONFLICT_DIRECTORIES {
        let parent = namespace
            .create_dir(ROOT_INO, format!("dir-{directory}"), 1)
            .expect("create conflict fixture directory");
        for file in 0..CONFLICT_FILES_PER_DIRECTORY {
            let content = iroh_blobs::Hash::new(format!("{directory}/{file}"));
            namespace
                .create_file(parent, format!("file-{file}"), Some(&content), 1, 1)
                .expect("create conflict fixture file");
        }
    }
    let nodes = CONFLICT_DIRECTORIES * (1 + CONFLICT_FILES_PER_DIRECTORY);
    // Commit the fixture's edits, as a mounted namespace commits each
    // change when it saves its delta.
    namespace.heads();

    let namespace = Mutex::new(namespace);
    let mut watch = fs::conflict::NamespaceScan::new();
    // The first pass, and any after a merge, scans every node.
    assert!(watch.due(&namespace).is_empty());
    let scan = watch.last_pass();
    eprintln!(
        "WORK conflict-scan-50k: {} nodes in {} lock holds, at most {} per hold",
        scan.nodes, scan.lock_holds, scan.max_nodes_per_hold
    );
    assert_eq!(scan.nodes, nodes, "the scan examined every node once");
    assert!(
        scan.max_nodes_per_hold <= CONFLICT_NODES_PER_HOLD,
        "{} nodes examined under one namespace lock hold (at most {CONFLICT_NODES_PER_HOLD})",
        scan.max_nodes_per_hold
    );

    // What every pass of an idle writer now does.
    for _ in 0..3 {
        assert!(watch.due(&namespace).is_empty());
        let idle = watch.last_pass();
        assert_eq!(idle.nodes, 0, "an idle pass examined nodes");
        assert_eq!(
            idle.lock_holds, 1,
            "an idle pass took the lock more than once"
        );
    }
    assert_eq!(watch.scans(), 1, "an idle pass scanned");
}

/// The ranged reads of the local-residency scenario, through the mount's
/// read path, while a sync peer's remote namespace changes are applied under
/// the namespace lock that read path shares.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mount_ranged_read_under_remote_apply_performance_gate() {
    let store = Arc::new(ContentStore::memory());
    let large_file = deterministic_bytes(LARGE_FILE_BYTES);
    let manifest = store
        .write_content(&large_file)
        .await
        .expect("write chunked fixture")
        .expect("non-empty fixture has a manifest");
    let manifest_len = store.local(manifest).await.expect("local manifest").len() as u64;
    let endpoint = Endpoint::bind(presets::Minimal)
        .await
        .expect("bind local hydration endpoint");
    let router = Router::builder(endpoint.clone()).spawn();
    let hydrator = Arc::new(Hydrator::new(endpoint, Arc::clone(&store)));

    let mut source = Namespace::new("perf-source").unwrap();
    source
        .create_file(
            ROOT_INO,
            "large.dat",
            Some(&manifest),
            LARGE_FILE_BYTES as u64,
            1_700_000_000,
        )
        .expect("create large file entry");
    source
        .create_file(ROOT_INO, "churn-0", None, 0, 1_700_000_000)
        .expect("create churn entry");
    let namespace = Arc::new(Mutex::new(
        Namespace::from_bytes("perf-mount", &source.save()).expect("open mount namespace"),
    ));
    let deltas: Vec<Vec<u8>> = (0..REMOTE_APPLY_DELTAS)
        .map(|index| {
            let (from, to) = (format!("churn-{index}"), format!("churn-{}", index + 1));
            assert!(source.rename(ROOT_INO, from, ROOT_INO, to, 1_700_000_001));
            source.save_incremental()
        })
        .collect();
    let mount = QuickFS::new(
        Arc::clone(&namespace),
        Arc::clone(&hydrator),
        Arc::new(Heuristics::new(Arc::clone(&hydrator))),
    );
    // Each read shares a round with its share of the remote changes, and
    // neither side runs ahead: every run interleaves the same work.
    let round = Arc::new(Barrier::new(2));
    let applier = {
        let (namespace, round) = (Arc::clone(&namespace), Arc::clone(&round));
        std::thread::spawn(move || {
            let mut applied = 0_usize;
            for batch in deltas.chunks(REMOTE_APPLY_DELTAS / RANGE_READS) {
                round.wait();
                for delta in batch {
                    namespace
                        .lock_recover()
                        .apply_remote_and_resolve(delta)
                        .expect("apply remote namespace delta");
                    applied += 1;
                }
                round.wait();
            }
            applied
        })
    };
    let before = store.read_bytes();
    for index in 0..RANGE_READS {
        let offset = range_offset(index);
        round.wait();
        let bytes = mount
            .read_path_range(Path::new("large.dat"), offset, RANGE_BYTES)
            .expect("read locally resident range through the mount");
        round.wait();
        assert_eq!(Some(bytes.as_slice()), expected_range(&large_file, offset));
    }
    let read = store.read_bytes() - before;
    assert_eq!(
        applier.join().expect("remote apply thread"),
        REMOTE_APPLY_DELTAS,
        "every remote change was applied during the reads"
    );
    assert_ranged_read_work(
        "mount-64x256k-ranged-read-under-remote-apply",
        read,
        manifest_len,
    );
    drop(mount);
    router.shutdown().await.expect("stop local endpoint");
}

/// Document media an app reads after a daemon restart: the status check its
/// draw path makes and a range near the end of the largest attachment. Each
/// read verifies only the chunk groups it returns; before, each loaded and
/// verified the whole attachment.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn document_media_cold_range_performance_gate() {
    let dir = tempfile::tempdir().expect("media store tempdir");
    let photos = BlobScope::new("room-1", "so.tilo.photos").expect("media scope");
    let len = usize::try_from(MAX_BLOB_BYTES).expect("media limit fits usize");
    let chunk = MAX_READ_CHUNK as usize;
    let data = deterministic_bytes(len);
    // Near the end, and not on a 16 KiB chunk-group boundary.
    let offset = len - chunk - 12_345;
    let tail = data[offset..offset + chunk].to_vec();
    let hash = {
        let store = Arc::new(
            ContentStore::open(dir.path())
                .await
                .expect("open media store"),
        );
        let media = DocumentMedia::new(Arc::clone(&store));
        let hash = media
            .store(&photos, data)
            .await
            .expect("store media fixture");
        store.store().shutdown().await.expect("close media store");
        hash
    };
    let store = Arc::new(
        ContentStore::open(dir.path())
            .await
            .expect("reopen media store"),
    );
    let media = DocumentMedia::new(Arc::clone(&store));

    let before = store.read_bytes();
    let status = media.read_range(photos.clone(), hash, 0, 0).await;
    let status_read = store.read_bytes() - before;
    assert_eq!(
        status,
        ReadOutcome::Ready {
            bytes: Vec::new(),
            total_len: MAX_BLOB_BYTES
        }
    );
    eprintln!("WORK document-media-cold-64m-status: {status_read} B read");
    assert_eq!(status_read, 0, "a status check read attachment bytes");

    let before = store.read_bytes();
    let read = media
        .read_range(photos, hash, offset as u64, MAX_READ_CHUNK)
        .await;
    let tail_read = store.read_bytes() - before;
    assert_eq!(
        read,
        ReadOutcome::Ready {
            bytes: tail,
            total_len: MAX_BLOB_BYTES
        }
    );
    // Verification reads whole 1 KiB leaves: an unaligned range pays for at
    // most one more at each end.
    let ceiling = u64::from(MAX_READ_CHUNK) + 2 * blake3::CHUNK_LEN as u64;
    eprintln!(
        "WORK document-media-cold-64m-tail-4m-read: {tail_read} B read (at most {ceiling} B)"
    );
    assert!(
        (u64::from(MAX_READ_CHUNK)..=ceiling).contains(&tail_read),
        "a {MAX_READ_CHUNK} B read of a {MAX_BLOB_BYTES} B attachment read {tail_read} B"
    );

    // The same blob loaded whole, as every read was answered before ranges
    // were verified on their own.
    let before = store.read_bytes();
    let whole = store.local(hash).await.expect("whole media blob");
    assert_eq!(whole.len(), len);
    assert_eq!(store.read_bytes() - before, MAX_BLOB_BYTES);
    store.store().shutdown().await.expect("close media store");
}

/// Reopening persisted metadata and listing it, then the local ranged reads
/// of a resident chunked file through `Hydrator::read`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collaborative_fs_release_performance_gate() {
    let mut original = Namespace::new("perf-fixture").unwrap();
    for index in 0..DIRECTORY_ENTRIES {
        original
            .create_file(
                ROOT_INO,
                format!("entry-{index:05}.dat"),
                None,
                index as u64,
                1_700_000_000,
            )
            .expect("create namespace fixture entry");
    }
    let persisted = original.save();
    let entries = DIRECTORY_ENTRIES as u64;

    let mut reopened = Namespace::from_bytes("perf-reader", &persisted)
        .expect("reopen persisted namespace fixture");
    let first_listing = reopened.readdir(ROOT_INO).expect("list root directory");
    assert_eq!(first_listing.len(), DIRECTORY_ENTRIES + 2);
    let cold = reopened.node_lookup_count();
    eprintln!("WORK metadata-cold-open-and-1000-list: {cold} lookups");
    assert!(
        cold <= COLD_LISTING_LOOKUPS_PER_ENTRY * entries,
        "reopening and listing {DIRECTORY_ENTRIES} entries took {cold} lookups"
    );

    for _ in 0..WARM_DIRECTORY_PASSES {
        let listing = reopened.readdir(ROOT_INO).expect("repeat root listing");
        assert_eq!(listing.len(), DIRECTORY_ENTRIES + 2);
    }
    let warm = reopened.node_lookup_count() - cold;
    eprintln!("WORK metadata-warm-8x-1000-list: {warm} lookups");
    assert!(
        warm <= WARM_LISTING_LOOKUPS_PER_ENTRY * entries * WARM_DIRECTORY_PASSES as u64,
        "{WARM_DIRECTORY_PASSES} listings of {DIRECTORY_ENTRIES} entries took {warm} lookups"
    );

    // `Hydrator` owns an Endpoint, but this local-residency scenario installs
    // no peers and makes no protocol requests. The router merely keeps the
    // loopback-bound endpoint alive while production `Hydrator::read` is used.
    let store = Arc::new(ContentStore::memory());
    let large_file = deterministic_bytes(LARGE_FILE_BYTES);
    let manifest = store
        .write_content(&large_file)
        .await
        .expect("write chunked fixture")
        .expect("non-empty fixture has a manifest");
    let manifest_len = store.local(manifest).await.expect("local manifest").len() as u64;
    let endpoint = Endpoint::bind(presets::Minimal)
        .await
        .expect("bind local hydration endpoint");
    let router = Router::builder(endpoint.clone()).spawn();
    let hydrator = Hydrator::new(endpoint, Arc::clone(&store));

    let before = store.read_bytes();
    for index in 0..RANGE_READS {
        let offset = range_offset(index);
        let bytes = hydrator
            .read(manifest, offset, RANGE_BYTES)
            .await
            .expect("read locally resident chunked range");
        assert_eq!(Some(bytes.as_slice()), expected_range(&large_file, offset));
    }
    let read = store.read_bytes() - before;
    assert_ranged_read_work("local-chunked-64x256k-ranged-read", read, manifest_len);
    router.shutdown().await.expect("stop local endpoint");
}
