//! Collaborative-files latency benchmarks.
//!
//! Wall-clock time depends on the machine, so these run in the nightly
//! performance suite (`tools/ci/perf-scheduled.sh fs`), which records every
//! `PERF` line; `performance_gate.rs` bounds the same scenarios' work on every
//! change. Ordinary `cargo test` skips them; `TILO_FS_PERF_GATE=1` runs them
//! and makes every ceiling a hard assertion. The fixtures are generated from
//! fixed data, use local stores only, and contain no polling, sleeps, peer
//! discovery, or remote requests.

use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fs::document_media::DocumentMedia;
use fs::{ContentStore, Heuristics, Hydrator, Namespace, QuickFS, ROOT_INO, ThumbProvider};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use iroh::{Endpoint, endpoint::presets, protocol::Router};
use runtime_fs::blobs::{BlobScope, MAX_BLOB_BYTES, MAX_READ_CHUNK, ReadOutcome};
use thumbnail::FileCategory;
use tilo_lock::MutexRecover as _;

const ENABLE_ENV: &str = "TILO_FS_PERF_GATE";

const DIRECTORY_ENTRIES: usize = 1_000;
const LARGE_FILE_BYTES: usize = 64 * 1024 * 1024;
const RANGE_BYTES: u64 = 256 * 1024;
const RANGE_READS: usize = 64;
const REMOTE_APPLY_DELTAS: usize = 512;
const WARM_DIRECTORY_PASSES: usize = 8;
const WARM_THUMB_LOOKUPS: usize = 1_000;

const COLD_METADATA_SLO: Duration = Duration::from_millis(360); // p99 179.5, p50 92.8
const WARM_DIRECTORY_SLO: Duration = Duration::from_millis(470); // p99 233.6, p50 122.9
const RANGED_READ_SLO: Duration = Duration::from_millis(400); // p99 197.7, p50 27.2
const REMOTE_APPLY_RANGED_READ_SLO: Duration = Duration::from_millis(680); // p99 337.2, p50 231.1
const COLD_THUMBNAIL_SLO: Duration = Duration::from_millis(550); // p99 271.3, p50 171.0
const WARM_THUMBNAIL_SLO: Duration = Duration::from_millis(12); // p99 5.76, p50 2.85
const MEDIA_STATUS_SLO: Duration = Duration::from_millis(5); // first run 0.75
const MEDIA_TAIL_READ_SLO: Duration = Duration::from_millis(40); // first run 17.6
const MEDIA_TAIL_READ_SHARE_OF_WHOLE_LOAD: f64 = 1.0 / 8.0;

const CONFLICT_DIRECTORIES: usize = 500;
const CONFLICT_FILES_PER_DIRECTORY: usize = 100;
const IDLE_CONFLICT_SLO: Duration = Duration::from_millis(5);
const CONFLICT_SCAN_MEAN_HOLD_SLO: Duration = Duration::from_millis(10);
const CONFLICT_SCAN_LONGEST_HOLD_SLO: Duration = Duration::from_millis(200);
const ATTEMPTS: usize = 3;
fn require_enabled() -> bool {
    if std::env::var(ENABLE_ENV).as_deref() == Ok("1") {
        true
    } else {
        eprintln!("collaborative FS performance gate disabled; set {ENABLE_ENV}=1 to enforce SLOs");
        false
    }
}

fn report(name: &str, elapsed: Duration, ceiling: Duration) {
    eprintln!(
        "PERF {name}: {:.3} ms (SLO <= {:.3} ms)",
        elapsed.as_secs_f64() * 1_000.0,
        ceiling.as_secs_f64() * 1_000.0
    );
    assert!(
        elapsed <= ceiling,
        "{name} exceeded release SLO: {elapsed:?} > {ceiling:?}"
    );
}

fn report_relative(name: &str, elapsed: Duration, baseline: (&str, Duration), share: f64) {
    let (baseline_name, baseline) = baseline;
    let ceiling = baseline.mul_f64(share);
    eprintln!(
        "RATIO {name}: {:.4} of {baseline_name} (at most {share:.4})",
        elapsed.as_secs_f64() / baseline.as_secs_f64()
    );
    assert!(
        elapsed <= ceiling,
        "{name} exceeded its share of {baseline_name} in the same run: {elapsed:?} > {share} of {baseline:?}"
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

#[test]
fn conflict_watch_idle_pass_and_lock_hold_performance_gate() {
    if !require_enabled() {
        return;
    }
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

    let started = Instant::now();
    assert!(fs::conflict::detect(&namespace).is_empty());
    eprintln!(
        "PERF conflict-detect-50k: {:.3} ms under one lock hold (each pass before #3985)",
        started.elapsed().as_secs_f64() * 1_000.0
    );

    let namespace = Mutex::new(namespace);
    let mut watch = fs::conflict::NamespaceScan::new();
    assert!(watch.due(&namespace).is_empty());
    let scan = watch.last_pass();
    assert!(
        scan.nodes >= nodes,
        "the scan examined {} nodes",
        scan.nodes
    );
    assert!(
        scan.lock_holds > scan.nodes / fs::conflict::SCAN_STEP_NODES,
        "{} nodes in only {} lock holds",
        scan.nodes,
        scan.lock_holds
    );
    eprintln!(
        "PERF conflict-scan-50k: {} nodes in {} lock holds, {:.3} ms held in all",
        scan.nodes,
        scan.lock_holds,
        scan.total_hold.as_secs_f64() * 1_000.0
    );
    let steps = u32::try_from(scan.lock_holds).expect("lock holds fit u32");
    report(
        "conflict-scan-mean-lock-hold-50k",
        scan.total_hold / steps,
        CONFLICT_SCAN_MEAN_HOLD_SLO,
    );
    report(
        "conflict-scan-longest-lock-hold-50k",
        scan.longest_hold,
        CONFLICT_SCAN_LONGEST_HOLD_SLO,
    );

    let mut slowest = Duration::ZERO;
    for _ in 0..3 {
        let started = Instant::now();
        assert!(watch.due(&namespace).is_empty());
        slowest = slowest.max(started.elapsed());
        let idle = watch.last_pass();
        assert_eq!(idle.nodes, 0, "an idle pass examined nodes");
        assert_eq!(
            idle.lock_holds, 1,
            "an idle pass took the lock more than once"
        );
    }
    assert_eq!(watch.scans(), 1, "an idle pass scanned");
    report("conflict-idle-pass-50k", slowest, IDLE_CONFLICT_SLO);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mount_ranged_read_under_remote_apply_performance_gate() {
    if !require_enabled() {
        return;
    }
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
    let hydrator = Arc::new(Hydrator::new(endpoint, store));

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
    let stop = Arc::new(AtomicBool::new(false));
    let applier = {
        let (namespace, stop) = (Arc::clone(&namespace), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut applied = 0_usize;
            for delta in &deltas {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                namespace
                    .lock_recover()
                    .apply_remote_and_resolve(delta)
                    .expect("apply remote namespace delta");
                applied += 1;
            }
            applied
        })
    };
    let started = Instant::now();
    for index in 0..RANGE_READS {
        let max_offset = (LARGE_FILE_BYTES as u64) - RANGE_BYTES;
        let offset = ((index as u64 * 1_000_003) % max_offset) & !4095;
        let bytes = mount
            .read_path_range(Path::new("large.dat"), offset, RANGE_BYTES)
            .expect("read locally resident range through the mount");
        let start = usize::try_from(offset).expect("fixture offset fits usize");
        let end =
            usize::try_from(offset.saturating_add(RANGE_BYTES)).expect("fixture end fits usize");
        assert_eq!(bytes, large_file[start..end]);
    }
    let under_apply = started.elapsed();
    stop.store(true, Ordering::Relaxed);
    let applied_deltas = applier.join().expect("remote apply thread");
    eprintln!("PERF remote deltas applied during the mount reads: {applied_deltas}");
    report(
        "mount-64x256k-ranged-read-under-remote-apply",
        under_apply,
        REMOTE_APPLY_RANGED_READ_SLO,
    );
    drop(mount);
    router.shutdown().await.expect("stop local endpoint");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn document_media_cold_range_performance_gate() {
    if !require_enabled() {
        return;
    }
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
    let mut status_time = Duration::MAX;
    let mut tail_time = Duration::MAX;
    let mut whole_time = Duration::MAX;
    for _ in 0..ATTEMPTS {
        let store = Arc::new(
            ContentStore::open(dir.path())
                .await
                .expect("reopen media store"),
        );
        let media = DocumentMedia::new(Arc::clone(&store));

        let started = Instant::now();
        let status = media.read_range(photos.clone(), hash, 0, 0).await;
        status_time = status_time.min(started.elapsed());
        assert_eq!(
            status,
            ReadOutcome::Ready {
                bytes: Vec::new(),
                total_len: MAX_BLOB_BYTES
            }
        );

        let started = Instant::now();
        let read = media
            .read_range(photos.clone(), hash, offset as u64, MAX_READ_CHUNK)
            .await;
        tail_time = tail_time.min(started.elapsed());
        assert_eq!(
            read,
            ReadOutcome::Ready {
                bytes: tail.clone(),
                total_len: MAX_BLOB_BYTES
            }
        );

        let started = Instant::now();
        let whole = store.local(hash).await.expect("whole media blob");
        whole_time = whole_time.min(started.elapsed());
        assert_eq!(whole.len(), len);
        store.store().shutdown().await.expect("close media store");
    }
    eprintln!(
        "PERF document-media-whole-blob-load-64m: {:.3} ms (before ranged reads)",
        whole_time.as_secs_f64() * 1_000.0
    );

    report(
        "document-media-cold-64m-status",
        status_time,
        MEDIA_STATUS_SLO,
    );
    report(
        "document-media-cold-64m-tail-4m-read",
        tail_time,
        MEDIA_TAIL_READ_SLO,
    );
    report_relative(
        "document-media-cold-64m-tail-4m-read",
        tail_time,
        ("document-media-whole-blob-load-64m", whole_time),
        MEDIA_TAIL_READ_SHARE_OF_WHOLE_LOAD,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collaborative_fs_release_performance_gate() {
    if !require_enabled() {
        return;
    }

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

    let mut cold_metadata = Duration::MAX;
    let mut reopened = None;
    for _ in 0..ATTEMPTS {
        let started = Instant::now();
        let mut namespace = Namespace::from_bytes("perf-reader", &persisted)
            .expect("reopen persisted namespace fixture");
        let first_listing = namespace.readdir(ROOT_INO).expect("list root directory");
        cold_metadata = cold_metadata.min(started.elapsed());
        assert_eq!(first_listing.len(), DIRECTORY_ENTRIES + 2);
        reopened = Some(namespace);
    }
    let mut reopened = reopened.expect("at least one attempt");
    report(
        "metadata-cold-open-and-1000-list",
        cold_metadata,
        COLD_METADATA_SLO,
    );

    let mut warm_metadata = Duration::MAX;
    for _ in 0..ATTEMPTS {
        let started = Instant::now();
        for _ in 0..WARM_DIRECTORY_PASSES {
            let listing = reopened.readdir(ROOT_INO).expect("repeat root listing");
            assert_eq!(listing.len(), DIRECTORY_ENTRIES + 2);
        }
        warm_metadata = warm_metadata.min(started.elapsed());
    }
    report(
        "metadata-warm-8x-1000-list",
        warm_metadata,
        WARM_DIRECTORY_SLO,
    );

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
    let hydrator = Hydrator::new(endpoint, store);

    let mut ranged_reads = Duration::MAX;
    for _ in 0..ATTEMPTS {
        let started = Instant::now();
        for index in 0..RANGE_READS {
            let max_offset = (LARGE_FILE_BYTES as u64) - RANGE_BYTES;
            let offset = ((index as u64 * 1_000_003) % max_offset) & !4095;
            let bytes = hydrator
                .read(manifest, offset, RANGE_BYTES)
                .await
                .expect("read locally resident chunked range");
            let start = usize::try_from(offset).expect("fixture offset fits usize");
            let end = usize::try_from(offset.saturating_add(RANGE_BYTES))
                .expect("fixture end fits usize");
            assert_eq!(bytes, large_file[start..end]);
        }
        ranged_reads = ranged_reads.min(started.elapsed());
    }
    report(
        "local-chunked-64x256k-ranged-read",
        ranged_reads,
        RANGED_READ_SLO,
    );
    router.shutdown().await.expect("stop local endpoint");

    let thumbnail_dir = tempfile::tempdir().expect("thumbnail cache tempdir");
    let provider = ThumbProvider::open(thumbnail_dir.path()).expect("open thumbnail cache");
    thumbnail_scenarios(&provider);
}

fn thumbnail_scenarios(provider: &ThumbProvider) {
    let source = thumbnail_fixture();
    let content_hash = *blake3::hash(&source).as_bytes();

    let mut cold_thumbnail = Duration::MAX;
    let mut generated = Vec::new();
    for _ in 0..ATTEMPTS {
        let started = Instant::now();
        generated = provider
            .generate(content_hash, FileCategory::Image, &source)
            .unwrap_or_default();
        cold_thumbnail = cold_thumbnail.min(started.elapsed());
        assert!(!generated.is_empty(), "generate deterministic thumbnail");
    }
    report(
        "thumbnail-cold-decode-scale-encode-cache",
        cold_thumbnail,
        COLD_THUMBNAIL_SLO,
    );

    let mut warm_thumbnail = Duration::MAX;
    for _ in 0..ATTEMPTS {
        let started = Instant::now();
        for _ in 0..WARM_THUMB_LOOKUPS {
            assert_eq!(
                provider.cached(content_hash).as_deref(),
                Some(generated.as_slice())
            );
        }
        warm_thumbnail = warm_thumbnail.min(started.elapsed());
    }
    report(
        "thumbnail-warm-1000-cache-lookups",
        warm_thumbnail,
        WARM_THUMBNAIL_SLO,
    );
}
