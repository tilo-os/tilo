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
/// Remote renames a sync peer applies while the mount serves ranged reads.
const REMOTE_APPLY_DELTAS: usize = 512;
const WARM_DIRECTORY_PASSES: usize = 8;
const WARM_THUMB_LOOKUPS: usize = 1_000;

// Release ceilings, not claims about typical latency. Each is twice the
// nearest-rank p99 of CI's `real-fuse` job (debug test profile, shared
// `runner` class), rounded up to 10 ms (1 ms below 10 ms).
// Measured 2026-10-09 over 56 runs, main pushes and pull requests between
// 3bc23c4b40 and b0aee36a66 plus the eight #3956 cites; at that N the p99
// is the slowest run, so every ceiling is 2x the worst run seen. The trailing
// comment is that p99 and the median. Revise by the same rule, and keep
// fixture sizes stable when you do; tools/perf/kpi-budgets.toml mirrors them.
//
// The local ranged read has a heavy tail (two of 56 runs at 4x and 7x its
// median), so its ceiling stops a regression in the worst case, not a 10x one
// in the typical case; the nightly relative comparison (tools/perf/compare.py)
// covers that.
const COLD_METADATA_SLO: Duration = Duration::from_millis(360); // p99 179.5, p50 92.8
const WARM_DIRECTORY_SLO: Duration = Duration::from_millis(470); // p99 233.6, p50 122.9
const RANGED_READ_SLO: Duration = Duration::from_millis(400); // p99 197.7, p50 27.2
/// The same reads through the mount while a peer's changes are applied.
const REMOTE_APPLY_RANGED_READ_SLO: Duration = Duration::from_millis(680); // p99 337.2, p50 231.1
const COLD_THUMBNAIL_SLO: Duration = Duration::from_millis(550); // p99 271.3, p50 171.0
const WARM_THUMBNAIL_SLO: Duration = Duration::from_millis(12); // p99 5.76, p50 2.85
/// Document media on a cold store (reopened, as after a daemon restart): an
/// app's status check, the empty range `state()` sends, and one 4 MiB range
/// near the end of a 64 MiB attachment, the largest the capability accepts,
/// as a seek to the end of a video asks for. Reads verify only the chunk
/// groups they return; before, each loaded and verified the whole
/// attachment, which the first `real-fuse` run measured at 464.5 ms.
/// Provisional ceilings until CI runs give a p99. The status ceiling stays
/// above one scheduler slice, as for `IDLE_SYNC_SLO`.
const MEDIA_STATUS_SLO: Duration = Duration::from_millis(5); // first run 0.75
const MEDIA_TAIL_READ_SLO: Duration = Duration::from_millis(40); // first run 17.6
/// Also require the tail read to be no more than one eighth of loading the
/// same blob whole in this run. This catches a whole-blob fallback even when
/// both operations are fast, without replacing the absolute release limit.
const MEDIA_TAIL_READ_SHARE_OF_WHOLE_LOAD: f64 = 1.0 / 8.0;

/// Namespace changes behind the two writers of the idle anti-entropy pass.
const IDLE_SYNC_HISTORY: u64 = 50_000;
/// One idle pass between two writers in sync, both directions together.
/// Independent of history: a pass resumes where the last one left off.
/// Twice the 148 B every one of 41 CI runs sent: the byte count does not
/// depend on the runner, so this is the idle pass's tight gate.
const IDLE_SYNC_BYTES: usize = 296;
/// The pass takes 0.015 ms (p99 0.052 ms over 41 runs). A ceiling under one
/// scheduler time slice would fail whenever the runner preempts the pass, so
/// time only bounds a stall here; `IDLE_SYNC_BYTES` catches a regression.
const IDLE_SYNC_SLO: Duration = Duration::from_millis(5);
/// The workspace behind the conflict-watch scenario: 50,000
/// files in 500 directories.
const CONFLICT_DIRECTORIES: usize = 500;
const CONFLICT_FILES_PER_DIRECTORY: usize = 100;
/// An idle pass of the conflict step: the namespace's merge generation,
/// read under one lock hold, and no node examined. The node count is the tight,
/// runner-independent gate; as for `IDLE_SYNC_SLO`, time only bounds a
/// stall and stays above one scheduler slice.
const IDLE_CONFLICT_SLO: Duration = Duration::from_millis(5);
/// A conflict scan over that workspace holds the namespace lock one step
/// of `fs::conflict::SCAN_STEP_NODES` nodes at a time, at any workspace
/// size. Before #3985 the whole walk was one hold: 2.5 s on the CI runner
/// (1.4 s on an M-series laptop), both debug builds. The mean hold per step
/// is the steady gate: 3.3 ms on the runner, 1.8 ms on the laptop. The
/// longest step of the first two CI runs held 50 and 77 ms (3-5 ms on the
/// laptop): the shared runner's tail, so the longest hold only bounds a
/// stall, and its ceiling still fails a walk under one hold 12 times over.
/// Both are provisional until CI runs give a p99 (#3956's rule).
const CONFLICT_SCAN_MEAN_HOLD_SLO: Duration = Duration::from_millis(10);
const CONFLICT_SCAN_LONGEST_HOLD_SLO: Duration = Duration::from_millis(200);
/// Files in the namespace behind the restart-resync scenario.
const RESTART_RESYNC_FILES: u64 = 50_000;
/// A device's first exchange with a peer after a restart, from the sync
/// progress it persisted: heads only, as an idle pass, so
/// `IDLE_SYNC_BYTES` is its tight gate and, as for `IDLE_SYNC_SLO`, time
/// only bounds a stall. Provisional until CI runs give a p99.
const RESTART_RESYNC_SLO: Duration = Duration::from_millis(5);
/// Times each single-shot wall-clock scenario runs; the gate takes the
/// fastest. The nightly lane shares its node with other jobs, and one
/// preemption or page-cache eviction there turned an 85 ms cold metadata open
/// into 1.4 s, and a 1.4 ms media status check into 46 ms, in runs whose every
/// other scenario was typical. A regression slows every attempt, so the
/// fastest of a few still fails it; a neighbour's burst rarely covers them
/// all. Each ceiling still bounds a single attempt.
const ATTEMPTS: usize = 3;
/// The device keys the two writers know each other by.
const IDLE_SYNC_HOST: [u8; 32] = [1; 32];
const IDLE_SYNC_DEVICE: [u8; 32] = [2; 32];

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

/// Additional same-run check after [`report`] enforces the absolute SLO.
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

/// One exchange of sync messages as a session runs it (the client writes,
/// the server answers, until neither has anything to send) between two
/// namespaces in sync. Returns the bytes both sent and the time both spent
/// generating and applying them, or `None` if the exchange failed.
fn idle_namespace_pass(
    (client, client_state): (&mut Namespace, &mut automerge::sync::State),
    (server, server_state): (&mut Namespace, &mut automerge::sync::State),
) -> Option<(usize, Duration)> {
    let started = Instant::now();
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
            return Some((bytes, started.elapsed()));
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
    if !require_enabled() {
        return;
    }
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
        host.heads();
    }
    let mut device =
        Namespace::from_bytes("perf-idle-device", &host.save()).expect("open in-sync device");

    let (fresh_bytes, fresh_time) = idle_namespace_pass(
        (&mut device, &mut automerge::sync::State::new()),
        (&mut host, &mut automerge::sync::State::new()),
    )
    .expect("fresh idle pass");
    eprintln!(
        "PERF idle-namespace-pass-fresh-state-50k: {fresh_bytes} B, {:.3} ms (before #3858)",
        fresh_time.as_secs_f64() * 1_000.0
    );

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
    let (bytes, time) = resumed.expect("a resumed pass ran");
    eprintln!("PERF idle-namespace-pass-resumed-50k: {bytes} B (budget {IDLE_SYNC_BYTES} B)");
    assert!(
        bytes <= IDLE_SYNC_BYTES,
        "an idle pass sent {bytes} B over {IDLE_SYNC_HISTORY} changes"
    );
    report("idle-namespace-pass-resumed-50k", time, IDLE_SYNC_SLO);
}

/// #4113: a device restarts (an update, a reboot) with 50,000 files in
/// its workspace and resumes its exchange with a peer. Without the sync
/// progress persisted beside the document, the restarted side started from
/// a fresh sync state and summarised the whole history again; with it, the
/// exchange carries heads only.
#[test]
fn restart_resync_performance_gate() {
    if !require_enabled() {
        return;
    }
    let mut host = Namespace::new("perf-restart-host").unwrap();
    for file in 0..RESTART_RESYNC_FILES {
        let content = iroh_blobs::Hash::new(file.to_le_bytes());
        host.create_file(ROOT_INO, format!("file-{file}"), Some(&content), 1, file)
            .expect("create restart fixture file");
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
    let (full_bytes, full_time) = idle_namespace_pass(
        (&mut forgetful, &mut automerge::sync::State::new()),
        (&mut host, &mut host_state),
    )
    .expect("restart without progress");
    eprintln!(
        "PERF restart-resync-without-progress-50k-files: {full_bytes} B, {:.3} ms (before #4113)",
        full_time.as_secs_f64() * 1_000.0
    );

    let mut restarted = Namespace::from_bytes("perf-restart-device", &saved).unwrap();
    let started = Instant::now();
    assert_eq!(restarted.restore_peer_sync_states(&progress).ok(), Some(1));
    let mut device_state = restarted.begin_sync_with(&IDLE_SYNC_HOST);
    let mut host_state = host.begin_sync_with(&IDLE_SYNC_DEVICE);
    let (bytes, _) = idle_namespace_pass(
        (&mut restarted, &mut device_state),
        (&mut host, &mut host_state),
    )
    .expect("restart with progress");
    let time = started.elapsed();
    eprintln!(
        "PERF restart-resync-with-progress-50k-files: {bytes} B (budget {IDLE_SYNC_BYTES} B)"
    );
    assert!(
        bytes <= IDLE_SYNC_BYTES,
        "a restarted device sent {bytes} B over {RESTART_RESYNC_FILES} files ({full_bytes} B without its progress)"
    );
    report("restart-resync-50k-files", time, RESTART_RESYNC_SLO);
}

/// #3985: the conflict step every writer's reconciliation loop runs every
/// few seconds. It used to walk every node under one hold of the namespace
/// lock on every pass. Now a pass with nothing merged since the last scan
/// reads the namespace's merge generation and examines no node, and a scan
/// (after a merge, a retry, or the periodic sweep) walks the nodes a step
/// at a time, releasing the lock between steps.
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
    // Commit the fixture's edits, as a mounted namespace commits each
    // change when it saves its delta.
    namespace.heads();

    let started = Instant::now();
    assert!(fs::conflict::detect(&namespace).is_empty());
    eprintln!(
        "PERF conflict-detect-50k: {:.3} ms under one lock hold (each pass before #3985)",
        started.elapsed().as_secs_f64() * 1_000.0
    );

    let namespace = Mutex::new(namespace);
    let mut watch = fs::conflict::NamespaceScan::new();
    // The first pass, and any after a merge, scans.
    assert!(watch.due(&namespace).is_empty());
    let scan = watch.last_pass();
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

    // What every pass of an idle writer now does.
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

/// The ranged reads of the local-residency scenario, through the mount's
/// read path, while a sync peer's remote namespace changes are applied under
/// the namespace lock that read path shares.
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

/// Document media an app reads after a daemon restart: the status check its
/// draw path makes and a range near the end of the largest attachment.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn document_media_cold_range_performance_gate() {
    if !require_enabled() {
        return;
    }
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
    // Each attempt reopens the store, so every one reads it cold.
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

        // The same blob loaded whole, as every read was answered before ranges
        // were verified on their own: the tail read's baseline.
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

/// Generating a thumbnail from a fixed image, then looking it up in the
/// cache it filled.
fn thumbnail_scenarios(provider: &ThumbProvider) {
    let source = thumbnail_fixture();
    let content_hash = *blake3::hash(&source).as_bytes();

    // `generate` decodes, scales and encodes on every call; it does not
    // consult the cache it fills.
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
