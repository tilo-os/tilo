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
const REMOTE_APPLY_DELTAS: usize = 512;
const WARM_DIRECTORY_PASSES: usize = 8;

const CONFLICT_DIRECTORIES: usize = 500;
const CONFLICT_FILES_PER_DIRECTORY: usize = 100;
const CONFLICT_NODES_PER_HOLD: usize = 64;
const COLD_LISTING_LOOKUPS_PER_ENTRY: u64 = 4;
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

fn range_offset(index: usize) -> u64 {
    (index as u64)
        .wrapping_mul(1_000_003)
        .checked_rem(MAX_RANGE_OFFSET)
        .unwrap_or_default()
        & !4095
}

fn expected_range(file: &[u8], offset: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    let end = usize::try_from(offset.saturating_add(RANGE_BYTES)).ok()?;
    file.get(start..end)
}

fn assert_ranged_read_work(name: &str, read: u64, manifest_len: u64) {
    eprintln!(
        "WORK {name}: {read} B read for {RANGES_BYTES} B of ranges ({manifest_len} B manifest)"
    );
    assert!(
        (RANGES_BYTES..=RANGES_BYTES.saturating_add(manifest_len)).contains(&read),
        "{name} read {read} B for {RANGE_READS} ranges of {RANGE_BYTES} B"
    );
}

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
    namespace.heads();

    let namespace = Mutex::new(namespace);
    let mut watch = fs::conflict::NamespaceScan::new();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn document_media_cold_range_performance_gate() {
    let dir = tempfile::tempdir().expect("media store tempdir");
    let photos = BlobScope::new("workspace-1", "so.tilo.photos").expect("media scope");
    let len = usize::try_from(MAX_BLOB_BYTES).expect("media limit fits usize");
    let chunk = MAX_READ_CHUNK as usize;
    let data = deterministic_bytes(len);
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
    let ceiling = u64::from(MAX_READ_CHUNK) + 2 * blake3::CHUNK_LEN as u64;
    eprintln!(
        "WORK document-media-cold-64m-tail-4m-read: {tail_read} B read (at most {ceiling} B)"
    );
    assert!(
        (u64::from(MAX_READ_CHUNK)..=ceiling).contains(&tail_read),
        "a {MAX_READ_CHUNK} B read of a {MAX_BLOB_BYTES} B attachment read {tail_read} B"
    );

    let before = store.read_bytes();
    let whole = store.local(hash).await.expect("whole media blob");
    assert_eq!(whole.len(), len);
    assert_eq!(store.read_bytes() - before, MAX_BLOB_BYTES);
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
