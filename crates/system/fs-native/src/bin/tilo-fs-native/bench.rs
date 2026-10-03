//! Local benchmarks for the exit criteria that do not need a network
//! (`docs/architecture/storage.md` §19.1): resident I/O against raw (B1,
//! B2), metadata storms (B3), capture cost and hashing throughput (K5, B7
//! locally), placeholder time to first byte (B9 with a local source) and
//! `git status` under capture (B8 locally).
//!
//! "Raw" is the same operation on the same store volume with no fanotify
//! group attached. "Tier 1" runs with the daemon's two groups live: the
//! filesystem-wide journal mark (pumped by a thread, as the daemon would)
//! and a pre-content group serving placeholders. Files in the Tier 1 runs
//! were created as placeholders and hydrated before being measured.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fs_content::ChunkTier;
use fs_native::capture::{CaptureConfig, EpochCapture};
use fs_native::hydrate::{Fetcher, Hydrator, HydratorConfig, PlaceholderSpec};
use fs_native::journal::{ChangeJournal, JournalBatch, JournalConfig, JournalEntry, JournalOp};
use fs_native::probe::{self, K5Params};
use fs_native::{btrfs, sys};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;

/// `SplitMix64`: incompressible, content-defined-chunkable test data whose
/// every byte depends only on its absolute offset.
fn word(index: u64) -> u64 {
    let mut z = index.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn byte(offset: u64) -> u8 {
    let bytes = word(offset >> 3).to_le_bytes();
    bytes
        .get(usize::try_from(offset & 7).unwrap_or(0))
        .copied()
        .unwrap_or(0)
}

fn fill(buf: &mut [u8], offset: u64) {
    let mut at = offset;
    let mut rest = buf;
    // Unaligned head, then whole words, then the tail.
    while at & 7 != 0 {
        let Some((b, tail)) = rest.split_first_mut() else {
            return;
        };
        *b = byte(at);
        rest = tail;
        at = at.wrapping_add(1);
    }
    let mut words = rest.chunks_exact_mut(8);
    for chunk in &mut words {
        chunk.copy_from_slice(&word(at >> 3).to_le_bytes());
        at = at.wrapping_add(8);
    }
    for b in words.into_remainder() {
        *b = byte(at);
        at = at.wrapping_add(1);
    }
}

/// Median of the samples.
fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

fn median_d(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v.get(v.len() / 2).copied().unwrap_or_default()
}

/// Serves deterministic bytes after a fixed delay, like a peer one RTT away.
struct LocalFetcher {
    delay: Duration,
}

impl Fetcher for LocalFetcher {
    fn fetch(&self, _content: &[u8; 32], offset: u64, len: u64) -> io::Result<Vec<u8>> {
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        let mut v =
            vec![0u8; usize::try_from(len).map_err(|_| io::Error::other("range too large"))?];
        fill(&mut v, offset);
        Ok(v)
    }
}

/// The daemon's fanotify side: a journal pumped on its own thread and a
/// hydrator.
struct Daemon {
    stop: Arc<AtomicBool>,
    entries: Arc<AtomicU64>,
    journal: Option<JoinHandle<()>>,
    hydrator: Option<Hydrator>,
}

impl Daemon {
    fn start(store: &Path, watch: &Path, delay: Duration) -> io::Result<Self> {
        Self::start_with(store, watch, delay, MIB)
    }

    fn start_with(store: &Path, watch: &Path, delay: Duration, granule: u64) -> io::Result<Self> {
        let mut journal = ChangeJournal::open(store, JournalConfig::new(watch))?;
        let stop = Arc::new(AtomicBool::new(false));
        let entries = Arc::new(AtomicU64::new(0));
        let (s, e) = (Arc::clone(&stop), Arc::clone(&entries));
        let thread = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                if journal.pump(Some(Duration::from_millis(50))).is_err() {
                    return;
                }
                let n = journal.take().entries.len() as u64;
                e.fetch_add(n, Ordering::Relaxed);
            }
        });
        let hydrator = Hydrator::start(
            Arc::new(LocalFetcher { delay }),
            HydratorConfig {
                granule,
                workers: 8,
                ..HydratorConfig::default()
            },
        )?;
        Ok(Self {
            stop,
            entries,
            journal: Some(thread),
            hydrator: Some(hydrator),
        })
    }

    fn hydrator(&self) -> &Hydrator {
        // Present until drop.
        self.hydrator
            .as_ref()
            .unwrap_or_else(|| unreachable_hydrator())
    }
}

#[allow(clippy::panic, reason = "the hydrator is only taken in Drop")]
fn unreachable_hydrator() -> &'static Hydrator {
    panic!("hydrator already stopped")
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.journal.take() {
            let _ = t.join();
        }
        if let Some(h) = self.hydrator.take() {
            h.stop();
        }
    }
}

fn spec(size: u64) -> PlaceholderSpec {
    PlaceholderSpec {
        content: [7u8; 32],
        size,
        mode: 0o644,
        mtime: std::time::SystemTime::now(),
        owner: None,
    }
}

fn drop_caches() {
    // SAFETY: plain syscall.
    unsafe { libc::sync() };
    let _ = fs::write("/proc/sys/vm/drop_caches", "1");
}

#[allow(
    clippy::cast_precision_loss,
    reason = "a throughput figure, not an exact count"
)]
fn mbps(bytes: u64, d: Duration) -> f64 {
    bytes as f64 / d.as_secs_f64().max(1e-9) / 1e6
}

fn human(size: u64) -> String {
    if size >= GIB {
        format!("{} GiB", size / GIB)
    } else if size >= MIB {
        format!("{} MiB", size / MIB)
    } else {
        format!("{} KiB", size / KIB)
    }
}

/// Writes `size` bytes of test data in 1 MiB blocks and syncs.
fn write_pattern(path: &Path, size: u64) -> io::Result<()> {
    let f = File::create(path)?;
    let mut buf = vec![0u8; usize::try_from(size.min(MIB)).unwrap_or(0)];
    let mut at = 0;
    while at < size {
        fill(&mut buf, at);
        f.write_all_at(&buf, at)?;
        at = at.saturating_add(buf.len() as u64);
    }
    f.sync_all()
}

/// Reads a whole file in 1 MiB blocks (hydrating it if it is a placeholder).
fn read_through(path: &Path, size: u64) -> io::Result<()> {
    let f = File::open(path)?;
    let mut buf = vec![0u8; usize::try_from(size.min(MIB)).unwrap_or(0)];
    let mut at = 0;
    while at < size {
        f.read_exact_at(&mut buf, at)?;
        at = at.saturating_add(buf.len() as u64);
    }
    Ok(())
}

struct IoResult {
    write: f64,
    read_warm: f64,
    read_cold: f64,
    mmap: f64,
}

/// Sequential write (+fdatasync), warm and cold read, and mmap read of an
/// existing file of `size` bytes, repeated to cover at least `budget` bytes.
fn io_pass(path: &Path, size: u64, budget: u64) -> io::Result<IoResult> {
    let block = size.min(MIB);
    let mut buf = vec![0u8; usize::try_from(block).unwrap_or(0)];
    fill(&mut buf, 0);
    let reps = budget.checked_div(size).unwrap_or(1).clamp(1, 20_000);
    // Every write pass ends in fdatasync, so small files are bound by the
    // device's flush latency; a few hundred passes measure that well.
    let write_reps = reps.min(200);
    let f = OpenOptions::new().read(true).write(true).open(path)?;
    let t = Instant::now();
    for _ in 0..write_reps {
        let mut at = 0;
        while at < size {
            f.write_all_at(&buf, at)?;
            at = at.saturating_add(block);
        }
        f.sync_data()?;
    }
    let write = mbps(size.saturating_mul(write_reps), t.elapsed());
    let t = Instant::now();
    for _ in 0..reps {
        let mut at = 0;
        while at < size {
            f.read_exact_at(&mut buf, at)?;
            at = at.saturating_add(block);
        }
    }
    let read_warm = mbps(size.saturating_mul(reps), t.elapsed());
    drop(f);
    drop_caches();
    let f = File::open(path)?;
    let t = Instant::now();
    let mut at = 0;
    while at < size {
        f.read_exact_at(&mut buf, at)?;
        at = at.saturating_add(block);
    }
    let read_cold = mbps(size, t.elapsed());
    let t = Instant::now();
    let mut sum = 0u64;
    for _ in 0..reps.min(1000) {
        let map = sys::Mmap::map(&f, size)?;
        for page in map.as_slice().iter().step_by(4096) {
            sum = sum.wrapping_add(u64::from(*page));
        }
    }
    let mmap = mbps(size.saturating_mul(reps.min(1000)), t.elapsed());
    std::hint::black_box(sum);
    Ok(IoResult {
        write,
        read_warm,
        read_cold,
        mmap,
    })
}

fn bench_io(dir: &Path, store: &Path, quick: bool) -> io::Result<()> {
    let sizes: &[u64] = if quick {
        &[4 * KIB, 64 * KIB, MIB, 64 * MIB]
    } else {
        &[4 * KIB, 64 * KIB, MIB, 64 * MIB, GIB]
    };
    let budget = if quick { 128 * MIB } else { 512 * MIB };
    let rounds = if quick { 3 } else { 5 };
    // Rounds alternate raw and Tier 1 so load drift hits both alike.
    println!(
        "B1/B2 resident I/O, MB/s, median of {rounds} alternating rounds (raw | Tier 1 hydrated placeholder, same volume)"
    );
    println!(
        "{:>7} | {:>16} {:>5} | {:>16} {:>5} | {:>16} {:>5} | {:>16} {:>5}",
        "size", "write+fdatasync", "", "read (cached)", "", "read (cold)", "", "mmap read", ""
    );
    for &size in sizes {
        let mut raw = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        let mut t1 = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for _ in 0..rounds {
            let path = dir.join(format!("raw-{size}"));
            write_pattern(&path, size)?;
            let r = io_pass(&path, size, budget)?;
            fs::remove_file(&path)?;
            for (v, x) in raw
                .iter_mut()
                .zip([r.write, r.read_warm, r.read_cold, r.mmap])
            {
                v.push(x);
            }

            let daemon = Daemon::start(store, dir, Duration::ZERO)?;
            let path = dir.join(format!("hydrated-{size}"));
            daemon.hydrator().create_placeholder(&path, &spec(size))?;
            read_through(&path, size)?;
            let r = io_pass(&path, size, budget)?;
            drop(daemon);
            fs::remove_file(&path)?;
            for (v, x) in t1
                .iter_mut()
                .zip([r.write, r.read_warm, r.read_cold, r.mmap])
            {
                v.push(x);
            }
        }
        let mut line = format!("{:>7} |", human(size));
        for (a, b) in raw.into_iter().zip(t1) {
            let (a, b) = (median(a), median(b));
            let _ = write!(
                line,
                " {a:>7.0} {b:>8.0} {:>4.0}% |",
                100.0 * b / a.max(1e-9)
            );
        }
        println!("{}", line.trim_end_matches('|'));
    }
    Ok(())
}

struct MetaTimes {
    create: Duration,
    stat: Duration,
    readdir: Duration,
    delete: Duration,
}

fn meta_pass(root: &Path, dirs: usize, per_dir: usize) -> io::Result<MetaTimes> {
    let data = [3u8; 4096];
    let t = Instant::now();
    for d in 0..dirs {
        let dir = root.join(format!("d{d:04}"));
        fs::create_dir(&dir)?;
        for f in 0..per_dir {
            fs::write(dir.join(format!("f{f:04}")), data)?;
        }
    }
    let create = t.elapsed();
    let t = Instant::now();
    for d in 0..dirs {
        let dir = root.join(format!("d{d:04}"));
        for f in 0..per_dir {
            std::hint::black_box(fs::symlink_metadata(dir.join(format!("f{f:04}")))?);
        }
    }
    let stat = t.elapsed();
    let t = Instant::now();
    let mut n = 0usize;
    for d in 0..dirs {
        n = n.saturating_add(fs::read_dir(root.join(format!("d{d:04}")))?.count());
    }
    std::hint::black_box(n);
    let readdir = t.elapsed();
    let t = Instant::now();
    for d in 0..dirs {
        fs::remove_dir_all(root.join(format!("d{d:04}")))?;
    }
    let delete = t.elapsed();
    Ok(MetaTimes {
        create,
        stat,
        readdir,
        delete,
    })
}

fn bench_meta(dir: &Path, store: &Path, quick: bool) -> io::Result<()> {
    let (dirs, per) = if quick { (100, 100) } else { (1000, 100) };
    let rounds = 3;
    let mut raw = Vec::new();
    let mut t1 = Vec::new();
    let mut journaled = 0;
    for _ in 0..rounds {
        let raw_dir = dir.join("meta-raw");
        fs::create_dir(&raw_dir)?;
        raw.push(meta_pass(&raw_dir, dirs, per)?);
        fs::remove_dir(&raw_dir)?;
        let t1_dir = dir.join("meta-t1");
        fs::create_dir(&t1_dir)?;
        let daemon = Daemon::start(store, &t1_dir, Duration::ZERO)?;
        t1.push(meta_pass(&t1_dir, dirs, per)?);
        std::thread::sleep(Duration::from_millis(200));
        journaled = daemon.entries.load(Ordering::Relaxed);
        drop(daemon);
        fs::remove_dir(&t1_dir)?;
    }
    println!(
        "B3 {} files (4 KiB) in {dirs} directories, median of {rounds}, raw vs Tier 1 (journal live: {journaled} entries per round)",
        dirs.saturating_mul(per)
    );
    let pick = |v: &[MetaTimes], f: fn(&MetaTimes) -> Duration| median_d(v.iter().map(f).collect());
    for (name, f) in [
        (
            "create",
            (|m: &MetaTimes| m.create) as fn(&MetaTimes) -> Duration,
        ),
        ("stat", |m| m.stat),
        ("readdir", |m| m.readdir),
        ("delete", |m| m.delete),
    ] {
        let (a, b) = (pick(&raw, f), pick(&t1, f));
        println!(
            "  {name:<8} raw {:>8.1} ms   tier1 {:>8.1} ms   {:.2}x",
            a.as_secs_f64() * 1e3,
            b.as_secs_f64() * 1e3,
            b.as_secs_f64() / a.as_secs_f64().max(1e-9)
        );
    }
    Ok(())
}

fn bench_hash(quick: bool) {
    let len = if quick { 64 * MIB } else { 512 * MIB };
    let mut data = vec![0u8; usize::try_from(len).unwrap_or(0)];
    fill(&mut data, 0);
    println!(
        "Hashing throughput (FastCDC + BLAKE3, one thread, {} in memory)",
        human(len)
    );
    for tier in [ChunkTier::Small, ChunkTier::Medium, ChunkTier::Large] {
        let t = Instant::now();
        let (m, _) = fs_content::build_in_tier(&data, tier);
        let d = t.elapsed();
        println!(
            "  {tier:?}: {} chunks, avg {} KiB, {:.0} MB/s",
            m.chunks().len(),
            len.checked_div(m.chunks().len() as u64).unwrap_or(0) / KIB,
            mbps(len, d)
        );
    }
    let t = Instant::now();
    std::hint::black_box(blake3::hash(&data));
    println!("  BLAKE3 alone: {:.0} MB/s", mbps(len, t.elapsed()));
}

fn entries(ops: &[(JournalOp, String, bool)]) -> JournalBatch {
    let entries: Vec<JournalEntry> = ops
        .iter()
        .zip(1u64..)
        .map(|((op, p, d), seq)| JournalEntry {
            seq,
            op: op.clone(),
            path: Some(PathBuf::from(p)),
            is_dir: *d,
            pid: 0,
        })
        .collect();
    JournalBatch {
        through: entries.last().map_or(0, |e| e.seq),
        entries,
        rescan: None,
    }
}

fn bench_capture(dir: &Path, quick: bool) -> io::Result<()> {
    let live = dir.join("room");
    btrfs::create_subvolume(&live)?;
    let epochs = dir.join("epochs");
    fs::create_dir(&epochs)?;
    let (dirs, per, big): (usize, usize, u64) = if quick {
        (100, 100, 256 * MIB)
    } else {
        (1000, 100, GIB)
    };
    let data = [5u8; 4096];
    for d in 0..dirs {
        let p = live.join(format!("d{d:04}"));
        fs::create_dir(&p)?;
        for f in 0..per {
            fs::write(p.join(format!("f{f:04}")), data)?;
        }
    }
    let img = live.join("disk.img");
    {
        let f = File::create(&img)?;
        let mut buf = vec![0u8; usize::try_from(MIB).unwrap_or(0)];
        let mut at = 0;
        while at < big {
            fill(&mut buf, at);
            f.write_all_at(&buf, at)?;
            at = at.saturating_add(MIB);
        }
        f.sync_all()?;
    }
    let mut cap = EpochCapture::new(CaptureConfig::new(&live, &epochs))?;
    let cs = cap.capture(&JournalBatch::default())?;
    let files = dirs.saturating_mul(per);
    println!(
        "Capture: first epoch of {files} files + {} image: snapshot {:.1} ms, scan {:.0} ms, hash {:.0} ms ({:.0} MB/s over {} files, {} threads)",
        human(big),
        cs.stats.snapshot.as_secs_f64() * 1e3,
        cs.stats.scan.as_secs_f64() * 1e3,
        cs.stats.hash.as_secs_f64() * 1e3,
        mbps(cs.stats.chunks.bytes_hashed, cs.stats.hash),
        cs.stats.files_hashed,
        CaptureConfig::new(&live, &epochs).hash_threads,
    );

    let f = OpenOptions::new().write(true).open(&img)?;
    f.write_all_at(&[0xEE; 4096], big / 2)?;
    drop(f);
    let cs = cap.capture(&entries(&[(JournalOp::Modify, "disk.img".into(), false)]))?;
    println!(
        "  4 KiB edit inside the image: total {:.1} ms (snapshot {:.1} ms), {} chunk(s) / {} KiB re-hashed, {} reused",
        cs.stats.total.as_secs_f64() * 1e3,
        cs.stats.snapshot.as_secs_f64() * 1e3,
        cs.stats.chunks.chunks_hashed,
        cs.stats.chunks.bytes_hashed / KIB,
        cs.stats.chunks.chunks_reused
    );

    let mut ops = Vec::new();
    for i in 0..1000usize {
        let p = format!(
            "d{:04}/f{:04}",
            i.checked_rem(dirs).unwrap_or(0),
            i.checked_div(dirs).unwrap_or(0)
        );
        fs::write(live.join(&p), [9u8; 4096])?;
        ops.push((JournalOp::CloseWrite, p, false));
    }
    let cs = cap.capture(&entries(&ops))?;
    println!(
        "  1000 small-file edits: total {:.1} ms (snapshot {:.1} ms, scan {:.1} ms, hash {:.1} ms), {} ops",
        cs.stats.total.as_secs_f64() * 1e3,
        cs.stats.snapshot.as_secs_f64() * 1e3,
        cs.stats.scan.as_secs_f64() * 1e3,
        cs.stats.hash.as_secs_f64() * 1e3,
        cs.ops.len()
    );
    let cs = cap.capture(&JournalBatch::default())?;
    println!(
        "  idle epoch: total {:.1} ms (snapshot {:.1} ms)",
        cs.stats.total.as_secs_f64() * 1e3,
        cs.stats.snapshot.as_secs_f64() * 1e3
    );
    cap.release()?;
    btrfs::delete_subvolume(&live)?;
    fs::remove_dir(&epochs)?;

    bench_k5(dir, quick)
}

/// K5: snapshot latency of a large tree with increasing dirty data.
fn bench_k5(dir: &Path, quick: bool) -> io::Result<()> {
    let k5_files = if quick { 10_000 } else { 100_000 };
    for dirty in [0, 64 * MIB, if quick { 256 * MIB } else { GIB }] {
        let cost = probe::probe_k5(
            dir,
            K5Params {
                files: k5_files,
                dirty_bytes: dirty,
                samples: if dirty >= GIB { 3 } else { 7 },
            },
        )?;
        println!(
            "  K5 snapshot, {k5_files} files, {} dirty: p50 {:.1} ms, p99 {:.1} ms (flush rate {:.0} MB/s)",
            human(dirty.max(KIB)),
            cost.percentile(50).as_secs_f64() * 1e3,
            cost.percentile(99).as_secs_f64() * 1e3,
            mbps(dirty, cost.percentile(50))
        );
    }
    Ok(())
}

fn bench_ttfb(dir: &Path, store: &Path, quick: bool) -> io::Result<()> {
    let samples = if quick { 20 } else { 100 };
    println!(
        "B9 placeholder time to first byte (64 KiB read at a random offset of a 1 GiB placeholder)"
    );
    for (delay_ms, granule) in [(0u64, 64 * KIB), (0, MIB), (1, 64 * KIB), (10, 64 * KIB)] {
        let delay = Duration::from_millis(delay_ms);
        let daemon = Daemon::start_with(store, dir, delay, granule)?;
        let mut times = Vec::with_capacity(samples);
        for i in 0..samples {
            let path = dir.join(format!("cold-{delay_ms}-{i}"));
            daemon.hydrator().create_placeholder(&path, &spec(GIB))?;
            let off = u64::from(byte(i as u64)).saturating_mul(4 * MIB);
            let mut buf = vec![0u8; 64 * 1024];
            let t = Instant::now();
            File::open(&path)?.read_exact_at(&mut buf, off)?;
            times.push(t.elapsed());
            fs::remove_file(&path)?;
        }
        times.sort();
        let pick = |p: usize| {
            times
                .get(times.len().saturating_sub(1).saturating_mul(p) / 100)
                .copied()
                .unwrap_or_default()
        };
        println!(
            "  source delay {delay_ms:>2} ms, fill granule {:>4} KiB: p50 {:.2} ms, p99 {:.2} ms (overhead over the source: p50 {:.2} ms)",
            granule / KIB,
            pick(50).as_secs_f64() * 1e3,
            pick(99).as_secs_f64() * 1e3,
            pick(50).saturating_sub(delay).as_secs_f64() * 1e3
        );
        drop(daemon);
    }
    let daemon = Daemon::start(store, dir, Duration::ZERO)?;
    let size = if quick { 256 * MIB } else { GIB };
    let path = dir.join("stream");
    daemon.hydrator().create_placeholder(&path, &spec(size))?;
    let t = Instant::now();
    let f = File::open(&path)?;
    let mut buf = vec![0u8; usize::try_from(MIB).unwrap_or(0)];
    let mut at = 0;
    while at < size {
        f.read_exact_at(&mut buf, at)?;
        at = at.saturating_add(MIB);
    }
    println!(
        "  sequential hydration of {} from a local source: {:.0} MB/s",
        human(size),
        mbps(size, t.elapsed())
    );
    drop(daemon);
    fs::remove_file(&path)?;
    Ok(())
}

fn git(dir: &Path, args: &[&str]) -> io::Result<()> {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "gc.auto=0"])
        .args(args)
        .env("GIT_AUTHOR_NAME", "Bench")
        .env("GIT_AUTHOR_EMAIL", "bench@example.invalid")
        .env("GIT_COMMITTER_NAME", "Bench")
        .env("GIT_COMMITTER_EMAIL", "bench@example.invalid")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .stdout(std::process::Stdio::null())
        .status()?
        .success();
    if ok {
        Ok(())
    } else {
        Err(io::Error::other(format!("git {args:?} failed")))
    }
}

fn bench_git(dir: &Path, store: &Path, quick: bool) -> io::Result<()> {
    let files = if quick { 5_000 } else { 50_000 };
    let repo = dir.join("repo");
    fs::create_dir(&repo)?;
    git(&repo, &["init", "-q", "-b", "main"])?;
    for i in 0..files {
        let d = repo.join(format!("src/{:03}", i % 500));
        if i < 500 {
            fs::create_dir_all(&d)?;
        }
        fs::write(d.join(format!("f{i}.rs")), format!("fn f{i}() {{}}\n"))?;
    }
    git(&repo, &["add", "-A"])?;
    git(&repo, &["commit", "-q", "-m", "import"])?;
    let time_status = |n: usize| -> io::Result<Duration> {
        git(&repo, &["status", "--porcelain"])?;
        let t = Instant::now();
        for _ in 0..n {
            git(&repo, &["status", "--porcelain"])?;
        }
        Ok(t.elapsed()
            .checked_div(u32::try_from(n).unwrap_or(1))
            .unwrap_or_default())
    };
    let raw = time_status(10)?;
    let daemon = Daemon::start(store, dir, Duration::ZERO)?;
    let t1 = time_status(10)?;
    drop(daemon);
    println!(
        "B8 git status on {files} files (warm): raw {:.1} ms, tier1 {:.1} ms, {:.2}x",
        raw.as_secs_f64() * 1e3,
        t1.as_secs_f64() * 1e3,
        t1.as_secs_f64() / raw.as_secs_f64().max(1e-9)
    );
    fs::remove_dir_all(&repo)?;
    Ok(())
}

pub fn run(what: &str, store: &Path, quick: bool) -> ExitCode {
    let dir = store.join(format!("bench-{}", std::process::id()));
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("cannot create {}: {e}", dir.display());
        return ExitCode::FAILURE;
    }
    let all = what == "all";
    let mut result = Ok(());
    if all || what == "hash" {
        bench_hash(quick);
    }
    for (name, f) in [
        ("io", bench_io as fn(&Path, &Path, bool) -> io::Result<()>),
        ("meta", bench_meta),
        ("ttfb", bench_ttfb),
        ("git", bench_git),
    ] {
        if result.is_ok() && (all || what == name) {
            result = f(&dir, store, quick);
        }
    }
    if result.is_ok() && (all || what == "capture") {
        result = bench_capture(&dir, quick);
    }
    let _ = fs::remove_dir_all(&dir);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("benchmark failed: {e}");
            ExitCode::FAILURE
        }
    }
}
