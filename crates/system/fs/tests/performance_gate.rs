//! Deterministic collaborative-files production work gate.
//!
//! Ordinary tests enforce production work counts, independent of scheduling. The
//! fixtures are generated from fixed data, use local stores only, and contain
//! no polling, sleeps, peer discovery, or remote requests.
//! Latency benchmarks live in `performance_timing.rs`, run only by scheduled CI.

use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Barrier, Mutex};

use fs::document_media::DocumentMedia;
use fs::{ContentStore, Heuristics, Hydrator, Namespace, QuickFS, ROOT_INO, ThumbProvider};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use iroh::{Endpoint, endpoint::presets, protocol::Router};
use runtime_fs::blobs::{BlobScope, MAX_BLOB_BYTES, MAX_READ_CHUNK, ReadOutcome};
use thumbnail::FileCategory;

const DIRECTORY_ENTRIES: usize = 1_000;
const LARGE_FILE_BYTES: usize = 64 * 1024 * 1024;
const RANGE_BYTES: u64 = 256 * 1024;
const RANGE_READS: usize = 64;
const TOTAL_RANGE_BYTES: u64 = RANGE_BYTES * RANGE_READS as u64;
/// Remote renames a sync peer applies while the mount serves ranged reads.
const REMOTE_APPLY_DELTAS: usize = 512;
const WARM_DIRECTORY_PASSES: usize = 8;
const WARM_THUMB_LOOKUPS: usize = 1_000;

/// Namespace changes behind the two writers of the idle anti-entropy pass.
const IDLE_SYNC_HISTORY: u64 = 50_000;
/// An idle two-way exchange consists of heads only.
const IDLE_SYNC_BYTES: usize = 148;
/// The workspace behind the conflict-watch scenario: 50,000
/// files in 500 directories.
const CONFLICT_DIRECTORIES: usize = 500;
const CONFLICT_FILES_PER_DIRECTORY: usize = 100;
/// Files in the namespace behind the restart-resync scenario.
const RESTART_RESYNC_FILES: u64 = 50_000;
/// The device keys the two writers know each other by.
const IDLE_SYNC_HOST: [u8; 32] = [1; 32];
const IDLE_SYNC_DEVICE: [u8; 32] = [2; 32];

fn assert_ranged_work(
    name: &str,
    store: &ContentStore,
    before_reads: fs::store::ReadWork,
    manifest_len: u64,
) {
    let after = store.read_work();
    eprintln!("WORK {name}: before={before_reads:?} after={after:?}");
    assert_eq!(
        after.file_bytes.checked_sub(before_reads.file_bytes),
        Some(TOTAL_RANGE_BYTES)
    );
    assert_eq!(
        after.blob_bytes.checked_sub(before_reads.blob_bytes),
        Some(manifest_len)
    );
    assert_eq!(
        after
            .bao_leaf_bytes
            .checked_sub(before_reads.bao_leaf_bytes),
        Some(0)
    );
}

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

fn thumbnail_fixture() -> Vec<u8> {
    let image = RgbaImage::from_fn(1024, 768, |x, y| {
        Rgba([
            (x.wrapping_mul(17) ^ y)
                .to_le_bytes()
                .first()
                .copied()
                .unwrap_or_default(),
            (y.wrapping_mul(29) ^ x)
                .to_le_bytes()
                .first()
                .copied()
                .unwrap_or_default(),
            (x.wrapping_add(y).wrapping_mul(7))
                .to_le_bytes()
                .first()
                .copied()
                .unwrap_or_default(),
            255,
        ])
    });
    let mut encoded = Vec::new();
    assert!(
        DynamicImage::ImageRgba8(image)
            .write_to(&mut Cursor::new(&mut encoded), ImageFormat::Png)
            .is_ok(),
        "encode deterministic image fixture"
    );
    encoded
}

/// One exchange of sync messages as a session runs it (the client writes,
/// the server answers, until neither has anything to send) between two
/// namespaces in sync. Returns the bytes both sent, or `None` if the exchange failed.
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

    let fresh_bytes = idle_namespace_pass(
        (&mut device, &mut automerge::sync::State::new()),
        (&mut host, &mut automerge::sync::State::new()),
    )
    .expect("fresh idle pass");
    eprintln!("WORK idle-fresh: bytes={fresh_bytes}");

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
    assert_eq!(
        bytes, IDLE_SYNC_BYTES,
        "an idle pass sent {bytes} B over {IDLE_SYNC_HISTORY} changes"
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
    eprintln!("WORK restart-forgetful: bytes={full_bytes}");

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
    assert_eq!(
        bytes, IDLE_SYNC_BYTES,
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

    assert!(fs::conflict::detect(&namespace).is_empty());

    let namespace = Mutex::new(namespace);
    let mut watch = fs::conflict::NamespaceScan::new();
    // The first pass, and any after a merge, scans.
    assert!(watch.due(&namespace).is_empty());
    let scan = watch.last_pass();
    assert_eq!(scan.nodes, nodes);
    assert_eq!(
        scan.lock_holds,
        nodes.div_ceil(fs::conflict::SCAN_STEP_NODES) + 1
    );
    assert!(
        scan.nodes >= nodes,
        "the scan examined {} nodes",
        scan.nodes
    );
    // Independent of the runner: the lock is released every step.
    assert!(
        scan.lock_holds > scan.nodes / fs::conflict::SCAN_STEP_NODES,
        "{} nodes in only {} lock holds",
        scan.nodes,
        scan.lock_holds
    );
    eprintln!(
        "WORK conflict-scan: nodes={} holds={}",
        scan.nodes, scan.lock_holds
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
    // Each read shares a round with eight remote changes. Neither participant
    // may run ahead, so every run covers all the same concurrent work.
    let round = Arc::new(Barrier::new(2));
    let applier = {
        let namespace = Arc::clone(&namespace);
        let round = Arc::clone(&round);
        std::thread::spawn(move || {
            let mut applied = 0_usize;
            for batch in deltas.chunks(REMOTE_APPLY_DELTAS / RANGE_READS) {
                round.wait();
                for delta in batch {
                    namespace
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .apply_remote_and_resolve(delta)
                        .expect("apply remote namespace delta");
                    applied += 1;
                }
                round.wait();
            }
            applied
        })
    };
    let manifest_len = store.local(manifest).await.expect("local manifest").len() as u64;
    let before_reads = store.read_work();
    for index in 0..RANGE_READS {
        let max_offset = (LARGE_FILE_BYTES as u64) - RANGE_BYTES;
        let offset = ((index as u64 * 1_000_003) % max_offset) & !4095;
        round.wait();
        let bytes = mount
            .read_path_range(Path::new("large.dat"), offset, RANGE_BYTES)
            .expect("read locally resident range through the mount");
        let start = usize::try_from(offset).expect("fixture offset fits usize");
        let end =
            usize::try_from(offset.saturating_add(RANGE_BYTES)).expect("fixture end fits usize");
        assert_eq!(bytes, large_file[start..end]);
        round.wait();
    }
    let applied_deltas = applier.join().expect("remote apply thread");
    assert_eq!(applied_deltas, REMOTE_APPLY_DELTAS);
    eprintln!("WORK remote-apply: deltas={applied_deltas}");
    assert_ranged_work("mount-ranges", &store, before_reads, manifest_len);
    drop(mount);
    router.shutdown().await.expect("stop local endpoint");
}

/// Document media an app reads after a daemon restart: the status check its
/// draw path makes and a range near the end of the largest attachment.
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

    let before_status = store.read_work();
    let status = media.read_range(photos.clone(), hash, 0, 0).await;
    let after_status = store.read_work();
    eprintln!("WORK media-status: before={before_status:?} after={after_status:?}");
    assert_eq!(after_status, before_status);
    assert_eq!(
        status,
        ReadOutcome::Ready {
            bytes: Vec::new(),
            total_len: MAX_BLOB_BYTES
        }
    );
    assert_eq!(
        after_status.bao_leaf_bytes, before_status.bao_leaf_bytes,
        "an empty range must not verify attachment data"
    );

    let before_tail = store.read_work();
    let read = media
        .read_range(photos, hash, offset as u64, MAX_READ_CHUNK)
        .await;
    let after_tail = store.read_work();
    assert_eq!(
        read,
        ReadOutcome::Ready {
            bytes: tail,
            total_len: MAX_BLOB_BYTES
        }
    );
    // BAO yields 1 KiB leaves. This unaligned request spans
    // one more leaf than its length, not the attachment's whole 64 MiB.
    let verified = after_tail.bao_leaf_bytes - before_tail.bao_leaf_bytes;
    let materialized = after_tail.blob_bytes - before_tail.blob_bytes;
    eprintln!("WORK document-media-cold-tail: verified={verified} blob={materialized}");
    assert_eq!(
        verified,
        u64::from(MAX_READ_CHUNK) + blake3::CHUNK_LEN as u64
    );
    assert_eq!(
        materialized,
        after_status.blob_bytes - before_status.blob_bytes,
        "only the small scope reference may be loaded whole, as for status"
    );

    let before_whole = store.read_work();
    let whole = store.local(hash).await.expect("whole media blob");
    let after_whole = store.read_work();
    assert_eq!(whole.len(), len);
    assert_eq!(
        after_whole.blob_bytes - before_whole.blob_bytes,
        MAX_BLOB_BYTES
    );
    eprintln!(
        "WORK document-media-whole: blob={}",
        after_whole.blob_bytes - before_whole.blob_bytes
    );
    store.store().shutdown().await.expect("close media store");
}

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

    let mut reopened = Namespace::from_bytes("perf-reader", &persisted)
        .expect("reopen persisted namespace fixture");
    let first_listing = reopened.readdir(ROOT_INO).expect("list root directory");
    assert_eq!(first_listing.len(), DIRECTORY_ENTRIES + 2);
    eprintln!(
        "WORK metadata-cold: lookups={}",
        reopened.node_lookup_count()
    );
    let cold_lookups = reopened.node_lookup_count();
    assert_eq!(cold_lookups, 7 * DIRECTORY_ENTRIES as u64);

    for _ in 0..WARM_DIRECTORY_PASSES {
        let listing = reopened.readdir(ROOT_INO).expect("repeat root listing");
        assert_eq!(listing.len(), DIRECTORY_ENTRIES + 2);
    }

    eprintln!(
        "WORK metadata-warm: lookups={}",
        reopened.node_lookup_count() - cold_lookups
    );

    assert_eq!(
        reopened.node_lookup_count() - cold_lookups,
        2 * (DIRECTORY_ENTRIES * WARM_DIRECTORY_PASSES) as u64
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
    let endpoint = Endpoint::bind(presets::Minimal)
        .await
        .expect("bind local hydration endpoint");
    let router = Router::builder(endpoint.clone()).spawn();
    let hydrator = Hydrator::new(endpoint, Arc::clone(&store));

    let manifest_len = store.local(manifest).await.expect("local manifest").len() as u64;
    let before_reads = store.read_work();
    for index in 0..RANGE_READS {
        let max_offset = (LARGE_FILE_BYTES as u64) - RANGE_BYTES;
        let offset = ((index as u64 * 1_000_003) % max_offset) & !4095;
        let bytes = hydrator
            .read(manifest, offset, RANGE_BYTES)
            .await
            .expect("read locally resident chunked range");
        let start = usize::try_from(offset).expect("fixture offset fits usize");
        let end =
            usize::try_from(offset.saturating_add(RANGE_BYTES)).expect("fixture end fits usize");
        assert_eq!(bytes, large_file[start..end]);
    }
    assert_ranged_work("hydration-ranges", &store, before_reads, manifest_len);
    router.shutdown().await.expect("stop local endpoint");

    let thumbnail_dir = tempfile::tempdir().expect("thumbnail cache tempdir");
    let provider = ThumbProvider::open(thumbnail_dir.path()).expect("open thumbnail cache");
    let source = thumbnail_fixture();
    let content_hash = *blake3::hash(&source).as_bytes();

    let generated = provider
        .generate(content_hash, FileCategory::Image, &source)
        .expect("generate deterministic thumbnail");

    assert_eq!(provider.generation_count(), 1);
    for _ in 0..WARM_THUMB_LOOKUPS {
        assert_eq!(
            provider.cached(content_hash).as_deref(),
            Some(generated.as_slice())
        );
    }
    assert_eq!(
        provider.generation_count(),
        1,
        "warm lookups must not decode the source again"
    );
    eprintln!(
        "WORK thumbnails: generations={}",
        provider.generation_count()
    );
}
