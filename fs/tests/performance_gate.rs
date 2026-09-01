//! Deterministic collaborative-files release performance gate.
//!
//! Ordinary `cargo test` keeps this cheap. Release CI opts in with
//! `QOS_FS_PERF_GATE=1`; once enabled every ceiling is a hard assertion. The
//! fixtures are generated from fixed data, use local stores only, and contain
//! no polling, sleeps, peer discovery, or remote requests.

use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fs::{ContentStore, Hydrator, Namespace, ROOT_INO, ThumbProvider};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use iroh::{Endpoint, endpoint::presets, protocol::Router};
use thumbnail::FileCategory;

const ENABLE_ENV: &str = "QOS_FS_PERF_GATE";

const DIRECTORY_ENTRIES: usize = 1_000;
const LARGE_FILE_BYTES: usize = 64 * 1024 * 1024;
const RANGE_BYTES: u64 = 256 * 1024;
const RANGE_READS: usize = 64;
const WARM_DIRECTORY_PASSES: usize = 8;
const WARM_THUMB_LOOKUPS: usize = 1_000;

// These are release ceilings, not claims about typical latency. They are
// deliberately generous enough for the shared, unpinned Forgejo runner while
// still catching order-of-magnitude regressions and accidentally blocking UI
// paths. Keep fixture sizes stable when revising a ceiling.
const COLD_METADATA_SLO: Duration = Duration::from_secs(2);
const WARM_DIRECTORY_SLO: Duration = Duration::from_secs(4);
const RANGED_READ_SLO: Duration = Duration::from_secs(4);
const COLD_THUMBNAIL_SLO: Duration = Duration::from_secs(3);
const WARM_THUMBNAIL_SLO: Duration = Duration::from_millis(500);

fn require_enabled() -> bool {
    if let Ok("1") = std::env::var(ENABLE_ENV).as_deref() {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collaborative_fs_release_performance_gate() {
    if !require_enabled() {
        return;
    }

    // Build outside the measured window: this scenario measures reopening
    // persisted metadata and listing it, not bulk creation/import throughput.
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

    let started = Instant::now();
    let mut reopened = Namespace::from_bytes("perf-reader", &persisted)
        .expect("reopen persisted namespace fixture");
    let first_listing = reopened.readdir(ROOT_INO).expect("list root directory");
    let cold_metadata = started.elapsed();
    assert_eq!(first_listing.len(), DIRECTORY_ENTRIES + 2);
    report(
        "metadata-cold-open-and-1000-list",
        cold_metadata,
        COLD_METADATA_SLO,
    );

    let started = Instant::now();
    for _ in 0..WARM_DIRECTORY_PASSES {
        let listing = reopened.readdir(ROOT_INO).expect("repeat root listing");
        assert_eq!(listing.len(), DIRECTORY_ENTRIES + 2);
    }
    report(
        "metadata-warm-8x-1000-list",
        started.elapsed(),
        WARM_DIRECTORY_SLO,
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
    let endpoint = Endpoint::bind(presets::N0)
        .await
        .expect("bind local hydration endpoint");
    let router = Router::builder(endpoint.clone()).spawn();
    let hydrator = Hydrator::new(endpoint, store);

    let started = Instant::now();
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
    report(
        "local-chunked-64x256k-ranged-read",
        started.elapsed(),
        RANGED_READ_SLO,
    );
    router.shutdown().await.expect("stop local endpoint");

    let thumbnail_dir = tempfile::tempdir().expect("thumbnail cache tempdir");
    let provider = ThumbProvider::open(thumbnail_dir.path()).expect("open thumbnail cache");
    let source = thumbnail_fixture();
    let content_hash = *blake3::hash(&source).as_bytes();

    let started = Instant::now();
    let generated = provider
        .generate(content_hash, FileCategory::Image, &source)
        .expect("generate deterministic thumbnail");
    report(
        "thumbnail-cold-decode-scale-encode-cache",
        started.elapsed(),
        COLD_THUMBNAIL_SLO,
    );

    let started = Instant::now();
    for _ in 0..WARM_THUMB_LOOKUPS {
        assert_eq!(
            provider.cached(content_hash).as_deref(),
            Some(generated.as_slice())
        );
    }
    report(
        "thumbnail-warm-1000-cache-lookups",
        started.elapsed(),
        WARM_THUMBNAIL_SLO,
    );
}
