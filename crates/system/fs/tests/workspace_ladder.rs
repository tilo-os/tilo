//! A workspace on a size ladder: does any cost grow with how big the
//! workspace is, instead of with what the request touches?
//!
//! The same shapes run at every rung — many small files, one big folder, a
//! few huge files — and each rung is measured for putting files in, listing
//! a big folder, persisting one change, a second device joining and being
//! given the tree, reading content it has never fetched, the metadata each
//! device holds, and reopening everything after a restart. The check asserts
//! *work* (bytes, records, node lookups), which does not depend on the
//! machine, so it holds on every run; the times are reported for the
//! scheduled performance run.
//!
//! The test runs on every change with small rungs; the full ladder is the
//! same test with `TILO_FS_LADDER_RUNGS_MIB=10240,102400,256000` (see
//! `tools/ci/fs-workspace-scale.sh`).
//!
//! Tunables (environment):
//!
//! - `TILO_FS_LADDER_RUNGS_MIB`: comma-separated totals (default `64,256`);
//! - `TILO_FS_LADDER_SMALL_FILES`: small files per rung; default four per MiB
//!   of the rung, at 4 KiB each;
//! - `TILO_FS_LADDER_HUGE_FILES`: how many huge files carry the rest
//!   (default 4);
//! - `TILO_FS_LADDER_DIR_ENTRIES`: entries in the big folder (default 1000);
//! - `TILO_FS_LADDER_DIR`: where the stores go (default `$CARGO_TARGET_DIR`
//!   or the temp dir); it must hold the rung's content.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    reason = "in a measurement these are the assertion or fixture arithmetic, not a hazard"
)]

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_fs::kept_documents::KeptDocuments;
use tilo_fs::{ContentStore, Heuristics, Hydrator, Namespace, QuickFS, ROOT_INO};

const SMALL_BYTES: u64 = 4096;
const RANGE_BYTES: u64 = 1024 * 1024;
/// Hydration's request granularity: what one range read may fetch at most.
const FETCH_BOUND: u64 = 64 * 1024 * 1024;
const BLOCK: usize = 8 * 1024 * 1024;

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn bench_dir() -> PathBuf {
    let base = std::env::var_os("TILO_FS_LADDER_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&base).unwrap();
    base.join(format!("fs-ladder-{}", std::process::id()))
}

fn report(rung: &str, metric: &str, value: f64, unit: &str) {
    println!("BENCH {rung} {metric} {value:.3} {unit}");
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Bytes the files under `root` hold, without following links and without reading content.
///
/// Counted in allocated blocks, never in length: a device's content area makes a file-backed
/// file its full size at once (`set_len`) and fills it as content arrives, so its length is the
/// whole video even when a range read fetched one mebibyte of it.
fn disk_bytes(root: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;

    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut total = 0_u64;
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            total = total.saturating_add(disk_bytes(&entry.path()));
        } else if kind.is_file() {
            total = total.saturating_add(entry.metadata().map_or(0, |m| m.blocks() * 512));
        }
    }
    total
}

/// The input and output this process did so far, as the kernel counts it: system calls that read
/// or write, and the bytes they moved. A put's work, whatever the machine's speed or load.
#[derive(Clone, Copy)]
struct Io {
    calls: u64,
    bytes: u64,
}

impl Io {
    #[cfg(target_os = "linux")]
    #[allow(
        clippy::unnecessary_wraps,
        reason = "elsewhere the kernel does not count it"
    )]
    fn now() -> Option<Self> {
        let counted = std::fs::read_to_string("/proc/self/io").unwrap();
        let field = |name: &str| {
            counted
                .lines()
                .find_map(|line| line.strip_prefix(name)?.trim().parse::<u64>().ok())
                .unwrap()
        };
        Some(Self {
            calls: field("syscr:") + field("syscw:"),
            bytes: field("rchar:") + field("wchar:"),
        })
    }

    /// Only Linux counts a process's input and output where the test can read it.
    #[cfg(not(target_os = "linux"))]
    fn now() -> Option<Self> {
        None
    }

    fn since(self, before: Self) -> Self {
        Self {
            calls: self.calls.saturating_sub(before.calls),
            bytes: self.bytes.saturating_sub(before.bytes),
        }
    }
}

fn fill(seed: u64, offset: u64, out: &mut [u8]) {
    for (i, word) in out.chunks_mut(8).enumerate() {
        let mut x = (offset / 8 + i as u64)
            .wrapping_add(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15))
            .wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 31;
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^= x >> 29;
        let bytes = x.to_le_bytes();
        word.copy_from_slice(&bytes[..word.len()]);
    }
}

/// `size` deterministic bytes in a file, written in blocks so the fixture
/// never sits in memory whole.
fn write_fixture(path: &Path, seed: u64, size: u64) {
    let mut buf = vec![0_u8; BLOCK];
    let mut file = std::fs::File::create(path).unwrap();
    let mut off = 0_u64;
    while off < size {
        let n = BLOCK.min(usize::try_from(size - off).unwrap());
        fill(seed, off, &mut buf[..n]);
        file.write_all(&buf[..n]).unwrap();
        off = off.saturating_add(n as u64);
    }
    file.sync_all().unwrap();
}

/// One rung's measurements, kept so the check can compare rungs.
struct Rung {
    total_mib: u64,
    nodes: u64,
    put_small_s: f64,
    /// The input and output putting the small files did, their sync included.
    put_small_io: Option<Io>,
    small_files: u64,
    put_huge_s: f64,
    store_bytes: u64,
    content_bytes: u64,
    small_bytes: u64,
    persist_bytes: u64,
    persist_s: f64,
    keep_first_s: f64,
    flush_bytes: u64,
    flush_s: f64,
    list_entries: u64,
    list_lookups: u64,
    list_cold_s: f64,
    join_records: u64,
    join_bytes: u64,
    join_s: f64,
    fetched_bytes: u64,
    read_s: f64,
    tree_held_bytes: i64,
    restart_s: f64,
    restart_first_read_s: f64,
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "a rung's measurements are one scenario read top to bottom"
)]
async fn measure_rung(
    base: &Path,
    total_mib: u64,
    huge_files: u64,
    dir_entries: u64,
    small_files: u64,
) -> Rung {
    let rung = format!("ladder/{total_mib}mib");
    let dir = base.join(format!("{total_mib}mib"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures = dir.join("fixtures");
    std::fs::create_dir_all(&fixtures).unwrap();
    let store_a = Arc::new(ContentStore::open(dir.join("a")).await.unwrap());
    let store_b = Arc::new(ContentStore::open(dir.join("b")).await.unwrap());
    let mut namespace = Namespace::new("rung-host").unwrap();

    let total = total_mib.saturating_mul(1024 * 1024);
    let small_files = small_files.max(1);
    let small_total = small_files.saturating_mul(SMALL_BYTES).min(total);
    let huge_total = total.saturating_sub(small_total);
    let huge_size = (huge_total / huge_files).max(1024 * 1024);
    let flat_entries = dir_entries.min(small_files);
    report(&rung, "total_mib", total_mib as f64, "MiB");
    report(&rung, "small_files", small_files as f64, "files");
    report(&rung, "flat_entries", flat_entries as f64, "entries");
    eprintln!(
        "ladder: {total_mib} MiB rung: {small_files} small files, {huge_files} huge of {huge_size} B"
    );

    // Put: the bytes of many small files, then of the huge ones, the way a
    // close hands them to the store.
    let flat = namespace
        .create_dir(ROOT_INO, "flat", 1_700_000_000)
        .unwrap();
    let small_dir = namespace
        .create_dir(ROOT_INO, "small", 1_700_000_000)
        .unwrap();
    let mut buf = vec![0_u8; SMALL_BYTES as usize];
    // What a mount does: content commits without its own sync, and one barrier makes the window
    // durable, so a copy of small files shares the syncs.
    store_a.set_sync_writes(false);
    let io_before = Io::now();
    let started = Instant::now();
    let mut small_written = 0_u64;
    let mut small_count = 0_u64;
    let mut folders: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
    let mut store_time = Duration::ZERO;
    let mut tree_time = Duration::ZERO;
    for i in 0..small_files {
        fill(i, 0, &mut buf);
        let store_started = Instant::now();
        let hash = store_a.write_content(&buf).await.unwrap().unwrap();
        store_time += store_started.elapsed();
        let tree_started = Instant::now();
        let parent = if i < flat_entries {
            flat
        } else {
            let folder = i / 1000;
            *folders.entry(folder).or_insert_with(|| {
                namespace
                    .create_dir(small_dir, format!("d{folder:03}"), 1_700_000_000)
                    .unwrap()
            })
        };
        namespace
            .create_file(
                parent,
                format!("f{i:07}"),
                Some(&hash),
                SMALL_BYTES,
                1_700_000_000,
            )
            .unwrap();
        tree_time += tree_started.elapsed();
        small_written = small_written.saturating_add(SMALL_BYTES);
        small_count = small_count.saturating_add(1);
        if small_written >= small_total {
            break;
        }
    }
    let put_small = started.elapsed();
    let sync_started = Instant::now();
    store_a.sync_pending().unwrap();
    let put_small_io = io_before
        .zip(Io::now())
        .map(|(before, after)| after.since(before));
    report(
        &rung,
        "put_small_sync_s",
        sync_started.elapsed().as_secs_f64(),
        "s",
    );
    if let Some(io) = put_small_io {
        let files = small_count.max(1) as f64;
        report(
            &rung,
            "put_small_io_calls_per_file",
            io.calls as f64 / files,
            "calls",
        );
        report(
            &rung,
            "put_small_io_bytes_per_file",
            io.bytes as f64 / files,
            "B",
        );
    }
    report(&rung, "put_small_s", put_small.as_secs_f64(), "s");
    report(&rung, "put_small_store_s", store_time.as_secs_f64(), "s");
    report(&rung, "put_small_tree_s", tree_time.as_secs_f64(), "s");
    report(
        &rung,
        "put_small_files_s",
        small_count as f64 / put_small.as_secs_f64(),
        "files/s",
    );
    report(
        &rung,
        "put_small_mib_s",
        mib(small_written) / put_small.as_secs_f64(),
        "MiB/s",
    );
    eprintln!("ladder: {total_mib} MiB rung: small files put in {put_small:?}");
    // What the same puts cost with a sync per file, the behaviour a batch replaces.
    let synced_sample = 1_000_u64.min(small_count);
    store_a.set_sync_writes(true);
    let synced_started = Instant::now();
    for i in 0..synced_sample {
        fill(i, 0, &mut buf);
        let _ = store_a.write_content(&buf).await.unwrap();
    }
    let synced_s = synced_started.elapsed();
    store_a.set_sync_writes(false);
    report(&rung, "put_small_synced_s", synced_s.as_secs_f64(), "s");
    report(
        &rung,
        "put_small_synced_files_s",
        synced_sample as f64 / synced_s.as_secs_f64(),
        "files/s",
    );
    let started = Instant::now();
    let mut huge_written = 0_u64;
    for i in 0..huge_files {
        if huge_written >= huge_total {
            break;
        }
        let size = huge_size.min(huge_total.saturating_sub(huge_written));
        let fixture = fixtures.join(format!("huge{i:02}.bin"));
        write_fixture(&fixture, 100 + i, size);
        let (written, _) = store_a
            .publish_file(&fixture, None, None, false)
            .await
            .unwrap();
        let (hash, len) = written.expect("a non-empty huge file has content");
        namespace
            .create_file(
                ROOT_INO,
                format!("huge{i:02}.bin"),
                Some(&hash),
                len,
                1_700_000_000,
            )
            .unwrap();
        huge_written = huge_written.saturating_add(size);
        let _ = std::fs::remove_file(&fixture);
    }
    let put_huge = started.elapsed();
    report(&rung, "put_huge_s", put_huge.as_secs_f64(), "s");
    report(
        &rung,
        "put_huge_mib_s",
        mib(huge_written) / put_huge.as_secs_f64(),
        "MiB/s",
    );
    eprintln!("ladder: {total_mib} MiB rung: huge files put in {put_huge:?}");
    let store_bytes = disk_bytes(&dir.join("a"));
    let content_bytes = small_written.saturating_add(huge_written);
    report(&rung, "store_mib", mib(store_bytes), "MiB");
    report(
        &rung,
        "store_per_content_byte",
        store_bytes as f64 / content_bytes.max(1) as f64,
        "x",
    );

    // Persisting: what the daemon writes once changes settle. Whole-tree
    // serialization here would make every flush cost the workspace.
    let nodes = namespace.live_node_ids().len() as u64;
    let started = Instant::now();
    let saved = namespace.save();
    let persist = started.elapsed();
    report(&rung, "persist_bytes", saved.len() as f64, "B");
    report(&rung, "persist_s", persist.as_secs_f64(), "s");
    report(
        &rung,
        "persist_bytes_per_node",
        saved.len() as f64 / nodes.max(1) as f64,
        "B",
    );

    // What one settled change costs when the tree is kept document by document: the first keep
    // saves the whole tree (as a device migrating or joining does), and every later one saves
    // only the documents a change touched.
    let kept_at = dir.join("kept");
    let (kept, _) = KeptDocuments::open(&kept_at).unwrap();
    let mut mirror = Namespace::from_bytes("kept", &saved).unwrap();
    kept.opened(&mut mirror, BTreeMap::new()).unwrap();
    let mirror = std::sync::Mutex::new(mirror);
    let started = Instant::now();
    kept.keep(&mirror).unwrap();
    let keep_first = started.elapsed();
    let before_flush = disk_bytes(&kept_at);
    {
        let mut mirror = mirror.lock().unwrap();
        mirror
            .create_file(ROOT_INO, "one-more.txt", None, 1, 1_700_000_000)
            .unwrap();
        kept.journal(&mut mirror).unwrap();
    }
    let started = Instant::now();
    kept.keep(&mirror).unwrap();
    let flush = started.elapsed();
    let flush_bytes = disk_bytes(&kept_at).saturating_sub(before_flush);
    report(&rung, "keep_first_s", keep_first.as_secs_f64(), "s");
    report(&rung, "flush_s", flush.as_secs_f64(), "s");
    report(&rung, "flush_bytes", flush_bytes as f64, "B");
    report(&rung, "kept_tree_bytes", disk_bytes(&kept_at) as f64, "B");
    drop(mirror);

    // Listing the big folder on a device that has the tree.
    let lookups_before = namespace.node_lookup_count();
    let started = Instant::now();
    let entries = namespace.readdir(flat).unwrap().len();
    let list_cold = started.elapsed();
    let list_lookups = namespace.node_lookup_count().saturating_sub(lookups_before);
    let started = Instant::now();
    for _ in 0..8 {
        assert_eq!(namespace.readdir(flat).unwrap().len(), entries);
    }
    let list_warm = started.elapsed().div_f64(8.0);
    report(&rung, "list_entries", entries as f64, "entries");
    report(&rung, "list_cold_s", list_cold.as_secs_f64(), "s");
    report(&rung, "list_warm_s", list_warm.as_secs_f64(), "s");
    report(&rung, "list_lookups", list_lookups as f64, "lookups");

    // How much metadata one device holds for the tree, and what reopening it
    // costs.
    let tree_held_bytes = tilo_automerge::heap::held_by(|| {
        Namespace::from_bytes("held", &saved).expect("the saved tree loads")
    });
    report(&rung, "tree_held_bytes", tree_held_bytes as f64, "B");
    let started = Instant::now();
    let mut reopened = Namespace::from_bytes("reopened", &saved).unwrap();
    let _ = reopened.readdir(ROOT_INO).unwrap();
    let restart = started.elapsed();
    report(&rung, "restart_s", restart.as_secs_f64(), "s");
    drop(reopened);

    // A second device joins: the record mesh gives a fresh joiner each chunk whole, which is
    // the tree's state and not the history that built it.
    let mut peer = Namespace::new("rung-peer").unwrap();
    let started = Instant::now();
    let chunks = namespace.records();
    let records: Vec<(fs_protocol::TreeChunk, Vec<u8>)> = chunks
        .iter()
        .filter_map(|chunk| namespace.save_document(*chunk).map(|bytes| (*chunk, bytes)))
        .collect();
    let join_records = records.len() as u64;
    let join_bytes: u64 = records.iter().map(|(_, bytes)| bytes.len() as u64).sum();
    peer.take_documents(records).unwrap();
    let join = started.elapsed();
    report(&rung, "join_records", join_records as f64, "records");
    report(&rung, "join_bytes", join_bytes as f64, "B");
    report(&rung, "join_s", join.as_secs_f64(), "s");
    let started = Instant::now();
    let peer_root = peer.readdir(ROOT_INO).unwrap();
    let join_list = started.elapsed();
    assert_eq!(peer_root.len(), namespace.readdir(ROOT_INO).unwrap().len());
    report(&rung, "join_list_s", join_list.as_secs_f64(), "s");
    eprintln!("ladder: {total_mib} MiB rung: peer joined in {join:?}");

    // Reading through a mount on the joined device, content it has never
    // fetched: one small file whole, then a mebibyte in the middle of a huge
    // one, from the device that has it.
    let hydrator = Arc::new(Hydrator::new(Arc::clone(&store_b)).with_source(Arc::new(
        tilo_fs::hydration::StoreSource(Arc::clone(&store_a)),
    )));
    let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
    let peer_namespace = Arc::new(std::sync::Mutex::new(peer));
    let mount = QuickFS::new(
        Arc::clone(&peer_namespace),
        Arc::clone(&hydrator),
        heuristics,
    );
    let before = disk_bytes(&dir.join("b"));
    let started = Instant::now();
    let small = match mount.read_path_range(Path::new("flat/f0000000"), 0, SMALL_BYTES) {
        Ok(bytes) => bytes,
        Err(errno) => {
            let hash = {
                let mut ns = peer_namespace.lock().unwrap();
                let (ino, _) = ns.lookup_kind(flat, "f0000000").unwrap();
                ns.hash_for(ino)
            };
            let hydrated = hash.map(|hash| hydrator.block_on(hydrator.read(hash, 0, SMALL_BYTES)));
            panic!("reading an unfetched small file: errno {errno}, hydrator: {hydrated:?}");
        }
    };
    let read_small = started.elapsed();
    assert_eq!(small.len() as u64, SMALL_BYTES);
    let started = Instant::now();
    let middle = mount
        .read_path_range(Path::new("huge00.bin"), huge_size / 2, RANGE_BYTES)
        .expect("a range of a huge file the device has never fetched");
    let read_huge = started.elapsed();
    assert_eq!(middle.len() as u64, RANGE_BYTES);
    let fetched = disk_bytes(&dir.join("b")).saturating_sub(before);
    report(&rung, "read_small_s", read_small.as_secs_f64(), "s");
    report(&rung, "read_huge_range_s", read_huge.as_secs_f64(), "s");
    report(&rung, "fetched_bytes", fetched as f64, "B");
    report(
        &rung,
        "fetched_per_range_byte",
        fetched as f64 / (SMALL_BYTES + RANGE_BYTES) as f64,
        "x",
    );
    eprintln!(
        "ladder: {total_mib} MiB rung: unfetched reads took {read_small:?} and {read_huge:?}"
    );
    drop(mount);

    Rung {
        total_mib,
        nodes,
        put_small_s: put_small.as_secs_f64(),
        put_small_io,
        small_files: small_count,
        put_huge_s: put_huge.as_secs_f64(),
        store_bytes,
        content_bytes,
        small_bytes: small_written,
        persist_bytes: saved.len() as u64,
        persist_s: persist.as_secs_f64(),
        keep_first_s: keep_first.as_secs_f64(),
        flush_bytes,
        flush_s: flush.as_secs_f64(),
        list_entries: entries as u64,
        list_lookups,
        list_cold_s: list_cold.as_secs_f64(),
        join_records,
        join_bytes,
        join_s: join.as_secs_f64(),
        fetched_bytes: fetched,
        read_s: read_huge.as_secs_f64(),
        tree_held_bytes: i64::try_from(tree_held_bytes).unwrap_or(i64::MAX),
        restart_s: restart.as_secs_f64(),
        restart_first_read_s: read_small.as_secs_f64(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workspace_on_a_size_ladder() {
    let rungs: Vec<u64> = std::env::var("TILO_FS_LADDER_RUNGS_MIB")
        .ok()
        .map(|list| {
            list.split(',')
                .filter_map(|value| value.trim().parse().ok())
                .collect()
        })
        .filter(|rungs: &Vec<u64>| !rungs.is_empty())
        .unwrap_or_else(|| vec![64, 256]);
    let huge_files = env_u64("TILO_FS_LADDER_HUGE_FILES", 4).max(1);
    let dir_entries = env_u64("TILO_FS_LADDER_DIR_ENTRIES", 1_000);
    let base = bench_dir();
    let mut measured = Vec::new();
    for total_mib in rungs {
        let rung = Box::pin(measure_rung(
            &base,
            total_mib,
            huge_files,
            dir_entries,
            env_u64("TILO_FS_LADDER_SMALL_FILES", total_mib.saturating_mul(4)),
        ))
        .await;
        summary(&rung);
        measured.push(rung);
        // The rung's stores are the ladder's biggest files; give them back
        // before the next rung writes more.
        let _ = std::fs::remove_dir_all(base.join(format!("{total_mib}mib")));
    }
    let _ = std::fs::remove_dir_all(&base);
    assert_work_is_flat(&measured);
}

/// The rung's times in one line, for the run's log.
fn summary(rung: &Rung) {
    eprintln!(
        "ladder: {} MiB ({} nodes): put small {:.1} MiB/s, huge {:.1} MiB/s, persist {:.0} B in {:.3} s, \
         list {:.3} s, join {:.3} s ({} records, {} B), read {:.3} s ({} B fetched), \
         tree {} B, first keep {:.3} s, one change saved in {:.3} s ({} B), restart {:.3} s, \
         first read {:.3} s",
        rung.total_mib,
        rung.nodes,
        mib(rung.small_bytes) / rung.put_small_s,
        mib(rung.content_bytes - rung.small_bytes) / rung.put_huge_s,
        rung.persist_bytes,
        rung.persist_s,
        rung.list_cold_s,
        rung.join_s,
        rung.join_records,
        rung.join_bytes,
        rung.read_s,
        rung.fetched_bytes,
        rung.tree_held_bytes,
        rung.keep_first_s,
        rung.flush_s,
        rung.flush_bytes,
        rung.restart_s,
        rung.restart_first_read_s,
    );
}

/// Work per touched item must not grow with the workspace: the first rung is
/// the baseline each later one is compared with.
fn assert_work_is_flat(rungs: &[Rung]) {
    let Some(first) = rungs.first() else { return };
    let chunk_nodes = fs_protocol::tree::CHUNK_NODES as f64;
    for rung in first_one_is_exempt(rungs) {
        // Putting a file in costs the same work whether the workspace is big: the system calls
        // it makes and the bytes they move, never a cost that grows with what the store holds.
        if let (Some(first_io), Some(rung_io)) = (first.put_small_io, rung.put_small_io) {
            let per_file = |io: Io, files: u64| {
                let files = files.max(1) as f64;
                (io.calls as f64 / files, io.bytes as f64 / files)
            };
            let (first_calls, first_bytes) = per_file(first_io, first.small_files);
            let (rung_calls, rung_bytes) = per_file(rung_io, rung.small_files);
            assert!(
                rung_calls <= first_calls * 1.25 + 1.0,
                "a small file's put at {} MiB makes {rung_calls:.1} system calls, at {} MiB {first_calls:.1}",
                rung.total_mib,
                first.total_mib
            );
            assert!(
                rung_bytes <= first_bytes * 1.25 + 4096.0,
                "a small file's put at {} MiB moves {rung_bytes:.0} B, at {} MiB {first_bytes:.0}",
                rung.total_mib,
                first.total_mib
            );
        }
        // One settled change never costs more than the whole tree it is part of.
        assert!(
            rung.flush_bytes <= rung.persist_bytes,
            "saving one change at {} MiB wrote {} B, the whole tree is {} B",
            rung.total_mib,
            rung.flush_bytes,
            rung.persist_bytes
        );
        // The store holds about the content, not a multiple of it.
        let ratio = rung.store_bytes as f64 / rung.content_bytes.max(1) as f64;
        assert!(
            ratio <= 1.5,
            "the store holds {ratio} of the content at {} MiB",
            rung.total_mib
        );
        // Persisting is bounded by the tree's records, not the workspace: one
        // chunk of nodes per 1,000 nodes, never the whole tree per change.
        let per_node = rung.persist_bytes as f64 / rung.nodes.max(1) as f64;
        let first_per_node = first.persist_bytes as f64 / first.nodes.max(1) as f64;
        assert!(
            per_node <= first_per_node * 2.0,
            "the tree holds {per_node} B per node at {} MiB, {first_per_node} at {} MiB",
            rung.total_mib,
            first.total_mib
        );
        // Joining carries the tree, once per chunk of nodes.
        assert!(
            rung.join_records as f64 <= rung.nodes as f64 / chunk_nodes + 2.0,
            "a join at {} MiB sent {} records for {} nodes",
            rung.total_mib,
            rung.join_records,
            rung.nodes
        );
        // Listing a folder costs its entries, not the workspace.
        assert!(
            rung.list_lookups as f64 <= rung.list_entries as f64 * 4.0,
            "listing {} entries at {} MiB took {} node lookups",
            rung.list_entries,
            rung.total_mib,
            rung.list_lookups
        );
        // Reading two ranges fetches at most hydration's request granularity each, whatever
        // the workspace's size.
        assert!(
            rung.fetched_bytes <= FETCH_BOUND,
            "reading {} B of ranges at {} MiB fetched {} B",
            SMALL_BYTES + RANGE_BYTES,
            rung.total_mib,
            rung.fetched_bytes
        );
        // The metadata one device holds grows with the number of files, not
        // with their bytes: 2 MiB per node would be a second copy of content.
        let held_per_node = rung.tree_held_bytes as f64 / rung.nodes.max(1) as f64;
        assert!(
            held_per_node <= 4096.0,
            "the tree holds {held_per_node} B per node at {} MiB",
            rung.total_mib
        );
    }
}

fn first_one_is_exempt(rungs: &[Rung]) -> impl Iterator<Item = &Rung> {
    rungs.iter().skip(1)
}
