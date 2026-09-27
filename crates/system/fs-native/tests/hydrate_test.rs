//! Placeholders filled through fanotify pre-content events.

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant, SystemTime};

use fs_native::hydrate::{Hydrator, HydratorConfig, PlaceholderSpec, Residency};
use fs_native::sys::Mmap;

use crate::support::{PatternFetcher, content_id, fanotify_scratch, pattern};

const MIB: u64 = 1024 * 1024;

fn spec(seed: u8, size: u64) -> PlaceholderSpec {
    PlaceholderSpec {
        content: content_id(seed),
        size,
        mode: 0o640,
        mtime: SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789),
        owner: None,
    }
}

fn start(fetcher: &Arc<PatternFetcher>, granule: u64) -> Hydrator {
    Hydrator::start(
        Arc::clone(fetcher) as Arc<dyn fs_native::hydrate::Fetcher>,
        HydratorConfig {
            granule,
            workers: 8,
            readahead_max: 0,
            ..HydratorConfig::default()
        },
    )
    .unwrap()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn placeholders_look_exact_and_reads_fill_only_touched_granules() {
    // Phase `a_placeholder_looks_exact_to_stat_and_readdir_without_hydrating`.
    fn a_placeholder_looks_exact_to_stat_and_readdir_without_hydrating() {
        let Some(s) = fanotify_scratch("hydrate_stat") else {
            return;
        };
        let fetcher = PatternFetcher::new(Duration::ZERO);
        let h = start(&fetcher, 256 * 1024);
        let path = s.live.join("video.mkv");
        h.create_placeholder(&path, &spec(1, 10 * MIB)).unwrap();
        let m = fs::metadata(&path).unwrap();
        assert_eq!(m.len(), 10 * MIB);
        assert_eq!(m.mode() & 0o7777, 0o640);
        assert_eq!((m.mtime(), m.mtime_nsec()), (1_700_000_000, 123_456_789));
        assert!(m.blocks() == 0, "a placeholder holds no data blocks");
        let names: Vec<_> = fs::read_dir(&s.live)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["video.mkv"]);
        assert_eq!(h.stats().events.load(Ordering::Relaxed), 0);
        assert_eq!(h.residency(&path).unwrap(), Residency::Placeholder);
        h.stop();
    }
    // Phase `a_partial_read_fills_only_the_granules_it_touches`.
    fn a_partial_read_fills_only_the_granules_it_touches() {
        let Some(s) = fanotify_scratch("hydrate_partial") else {
            return;
        };
        let fetcher = PatternFetcher::new(Duration::from_millis(5));
        let h = start(&fetcher, 256 * 1024);
        let path = s.live.join("big");
        h.create_placeholder(&path, &spec(2, 64 * MIB)).unwrap();
        let f = File::open(&path).unwrap();
        let mut buf = vec![0u8; 4096];
        f.read_exact_at(&mut buf, 10 * MIB + 100).unwrap();
        assert_eq!(buf, pattern(2, 10 * MIB + 100, 4096));
        assert_eq!(fetcher.calls(), [(10 * MIB, 256 * 1024)]);
        assert_eq!(
            h.residency(&path).unwrap(),
            Residency::Partial {
                resident: 256 * 1024,
                size: 64 * MIB
            }
        );
        // Re-reading a resident range costs no fetch.
        f.read_exact_at(&mut buf, 10 * MIB + 8192).unwrap();
        assert_eq!(fetcher.calls().len(), 1);
        h.stop();
    }
    // Phase `a_full_read_makes_the_file_resident_and_unmarked`.
    fn a_full_read_makes_the_file_resident_and_unmarked() {
        let Some(s) = fanotify_scratch("hydrate_full") else {
            return;
        };
        let fetcher = PatternFetcher::new(Duration::ZERO);
        let h = start(&fetcher, 1024 * 1024);
        let path = s.live.join("f");
        let size = 5 * MIB + 12345;
        h.create_placeholder(&path, &spec(3, size)).unwrap();
        let mut all = Vec::new();
        File::open(&path).unwrap().read_to_end(&mut all).unwrap();
        assert_eq!(all.len() as u64, size);
        assert_eq!(all, pattern(3, 0, size as usize));
        assert_eq!(h.residency(&path).unwrap(), Residency::Resident);
        let meta = fs::metadata(&path).unwrap();
        assert_eq!(
            (meta.mtime(), meta.mtime_nsec()),
            (1_700_000_000, 123_456_789),
            "hydration does not move the version's mtime"
        );
        let xattr = fs_native_xattr(&path);
        assert!(xattr.is_none(), "residency xattr left on a resident file");
        // No mark remains: further reads raise no events.
        let before = h.stats().events.load(Ordering::Relaxed);
        let mut again = Vec::new();
        File::open(&path).unwrap().read_to_end(&mut again).unwrap();
        assert_eq!(h.stats().events.load(Ordering::Relaxed), before);
        h.stop();
    }
    // Phase `mmap_of_a_placeholder_sees_the_real_bytes`.
    fn mmap_of_a_placeholder_sees_the_real_bytes() {
        let Some(s) = fanotify_scratch("hydrate_mmap") else {
            return;
        };
        let fetcher = PatternFetcher::new(Duration::from_millis(2));
        let h = start(&fetcher, 256 * 1024);
        let path = s.live.join("pack");
        let size = 16 * MIB;
        h.create_placeholder(&path, &spec(4, size)).unwrap();
        let f = File::open(&path).unwrap();
        let map = Mmap::map(&f, size).unwrap();
        let bytes = map.as_slice();
        for off in [0u64, 4095, 3 * MIB + 17, size - 1] {
            assert_eq!(bytes[off as usize], pattern(4, off, 1)[0], "byte {off}");
        }
        assert_eq!(bytes, &pattern(4, 0, size as usize)[..]);
        h.stop();
    }
    // Phase `a_write_into_a_placeholder_keeps_the_surrounding_bytes`.
    fn a_write_into_a_placeholder_keeps_the_surrounding_bytes() {
        let Some(s) = fanotify_scratch("hydrate_write") else {
            return;
        };
        let fetcher = PatternFetcher::new(Duration::ZERO);
        let h = start(&fetcher, 64 * 1024);
        let path = s.live.join("db");
        h.create_placeholder(&path, &spec(7, 2 * MIB)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        f.write_all_at(b"LOCAL", MIB + 10).unwrap();
        let mut buf = vec![0u8; 64 * 1024];
        f.read_exact_at(&mut buf, MIB).unwrap();
        let mut want = pattern(7, MIB, 64 * 1024);
        want[10..15].copy_from_slice(b"LOCAL");
        assert_eq!(buf, want);
        h.stop();
    }
    // Phase `a_placeholder_looks_exact_to_stat_and_readdir_without_hydrating` (defined above).
    a_placeholder_looks_exact_to_stat_and_readdir_without_hydrating();
    // Phase `a_partial_read_fills_only_the_granules_it_touches` (defined above).
    a_partial_read_fills_only_the_granules_it_touches();
    // Phase `a_full_read_makes_the_file_resident_and_unmarked` (defined above).
    a_full_read_makes_the_file_resident_and_unmarked();
    // Phase `mmap_of_a_placeholder_sees_the_real_bytes` (defined above).
    mmap_of_a_placeholder_sees_the_real_bytes();
    // Phase `a_write_into_a_placeholder_keeps_the_surrounding_bytes` (defined above).
    a_write_into_a_placeholder_keeps_the_surrounding_bytes();
}

fn fs_native_xattr(path: &std::path::Path) -> Option<Vec<u8>> {
    let p = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    let n = c"trusted.tilo.residency";
    let mut buf = vec![0u8; 512];
    let r = unsafe { libc::getxattr(p.as_ptr(), n.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    (r >= 0).then(|| {
        buf.truncate(r.cast_unsigned());
        buf
    })
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn fetches_are_shared_prefetched_and_fail_with_eio_when_unreachable() {
    // Phase `concurrent_readers_share_fills_and_each_range_is_fetched_once`.
    fn concurrent_readers_share_fills_and_each_range_is_fetched_once() {
        let Some(s) = fanotify_scratch("hydrate_concurrent") else {
            return;
        };
        let fetcher = PatternFetcher::new(Duration::from_millis(20));
        let h = start(&fetcher, 256 * 1024);
        let path = s.live.join("shared");
        let size = 8 * MIB;
        h.create_placeholder(&path, &spec(5, size)).unwrap();
        let barrier = Arc::new(Barrier::new(16));
        let threads: Vec<_> = (0..16u64)
            .map(|t| {
                let (path, barrier) = (path.clone(), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    let f = File::open(&path).unwrap();
                    barrier.wait();
                    // Every thread reads the same hot megabyte plus its own range.
                    let mut hot = vec![0u8; MIB as usize];
                    f.read_exact_at(&mut hot, 2 * MIB).unwrap();
                    assert_eq!(hot, pattern(5, 2 * MIB, MIB as usize));
                    let own = (t * 512 * 1024) % size;
                    let mut buf = vec![0u8; 100_000];
                    f.read_exact_at(&mut buf, own).unwrap();
                    assert_eq!(buf, pattern(5, own, 100_000));
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let mut calls = fetcher.calls();
        calls.sort_unstable();
        let before = calls.len();
        calls.dedup();
        assert_eq!(calls.len(), before, "a range was fetched twice: {calls:?}");
        let total: u64 = calls.iter().map(|c| c.1).sum();
        assert!(total <= size);
        h.stop();
    }
    // Phase `an_unreachable_source_fails_the_read_with_eio`.
    fn an_unreachable_source_fails_the_read_with_eio() {
        let Some(s) = fanotify_scratch("hydrate_eio") else {
            return;
        };
        let fetcher = PatternFetcher::new(Duration::ZERO);
        fetcher.fail.store(true, Ordering::Relaxed);
        let h = start(&fetcher, 256 * 1024);
        let path = s.live.join("offline");
        h.create_placeholder(&path, &spec(6, MIB)).unwrap();
        let mut buf = [0u8; 16];
        let err = File::open(&path).unwrap().read_at(&mut buf, 0).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::EIO), "{err}");
        // Metadata is untouched and a later read succeeds once online.
        assert_eq!(fs::metadata(&path).unwrap().len(), MIB);
        fetcher.fail.store(false, Ordering::Relaxed);
        File::open(&path)
            .unwrap()
            .read_exact_at(&mut buf, 0)
            .unwrap();
        assert_eq!(&buf[..], &pattern(6, 0, 16)[..]);
        h.stop();
    }
    // Phase `time_to_first_byte_is_one_fetch`.
    fn time_to_first_byte_is_one_fetch() {
        let Some(s) = fanotify_scratch("hydrate_ttfb") else {
            return;
        };
        let delay = Duration::from_millis(30);
        let fetcher = PatternFetcher::new(delay);
        let h = start(&fetcher, 64 * 1024);
        let path = s.live.join("cold");
        h.create_placeholder(&path, &spec(9, 64 * MIB)).unwrap();
        let t = Instant::now();
        let mut buf = vec![0u8; 64 * 1024];
        File::open(&path)
            .unwrap()
            .read_exact_at(&mut buf, 32 * MIB)
            .unwrap();
        let ttfb = t.elapsed();
        assert_eq!(buf, pattern(9, 32 * MIB, 64 * 1024));
        eprintln!("placeholder TTFB with a {delay:?} source: {ttfb:?}");
        assert!(ttfb < delay + Duration::from_millis(500), "{ttfb:?}");
        h.stop();
    }
    // Phase `a_sequential_reader_is_fetched_ahead_and_nothing_is_fetched_twice`.
    fn a_sequential_reader_is_fetched_ahead_and_nothing_is_fetched_twice() {
        let Some(s) = fanotify_scratch("hydrate_readahead") else {
            return;
        };
        let size = 16 * MIB;
        let delay = Duration::from_millis(10);

        let plain = PatternFetcher::new(delay);
        let h = start_with(&plain, 256 * 1024, 0);
        let path = s.live.join("plain");
        h.create_placeholder(&path, &spec(11, size)).unwrap();
        let (bytes, without) = read_sequentially(&path);
        assert_eq!(bytes, pattern(11, 0, size as usize));
        h.stop();

        let ahead = PatternFetcher::new(delay);
        let h = start_with(&ahead, 256 * 1024, 8 * MIB);
        let path = s.live.join("ahead");
        h.create_placeholder(&path, &spec(12, size)).unwrap();
        let (bytes, with) = read_sequentially(&path);
        assert_eq!(bytes, pattern(12, 0, size as usize));
        let fetched: u64 = ahead.calls().iter().map(|(_, len)| len).sum();
        let readahead = h.stats().readahead_bytes.load(Ordering::Relaxed);
        eprintln!(
            "16 MiB cold sequential read, {delay:?} per fetch: {without:?} in {} fetches without read-ahead, \
             {with:?} in {} fetches with it ({} MiB fetched ahead)",
            plain.calls().len(),
            ahead.calls().len(),
            readahead / MIB
        );
        assert_eq!(fetched, size, "every byte is fetched exactly once");
        assert!(
            readahead > size / 2,
            "most of the file arrives ahead of the reader"
        );
        assert!(
            ahead.calls().len() * 4 < plain.calls().len(),
            "read-ahead batches fetches: {} vs {}",
            ahead.calls().len(),
            plain.calls().len()
        );
        assert_eq!(h.residency(&path).unwrap(), Residency::Resident);
        h.stop();
    }
    // Phase `concurrent_readers_share_fills_and_each_range_is_fetched_once` (defined above).
    concurrent_readers_share_fills_and_each_range_is_fetched_once();
    // Phase `an_unreachable_source_fails_the_read_with_eio` (defined above).
    an_unreachable_source_fails_the_read_with_eio();
    // Phase `time_to_first_byte_is_one_fetch` (defined above).
    time_to_first_byte_is_one_fetch();
    // Phase `a_sequential_reader_is_fetched_ahead_and_nothing_is_fetched_twice` (defined above).
    a_sequential_reader_is_fetched_ahead_and_nothing_is_fetched_twice();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn hydrators_adopt_placeholders_replace_versions_and_gate_writers() {
    // Phase `a_restarted_hydrator_adopts_placeholders_and_keeps_local_writes`.
    fn a_restarted_hydrator_adopts_placeholders_and_keeps_local_writes() {
        let Some(s) = fanotify_scratch("hydrate_adopt") else {
            return;
        };
        let fetcher = PatternFetcher::new(Duration::ZERO);
        let path = s.live.join("resume");
        {
            let h = start(&fetcher, 256 * 1024);
            h.create_placeholder(&path, &spec(8, 4 * MIB)).unwrap();
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            let mut buf = [0u8; 16];
            f.read_exact_at(&mut buf, 0).unwrap();
            f.write_all_at(b"KEEP", 100).unwrap();
            h.stop();
        }
        let h = start(&fetcher, 256 * 1024);
        assert!(h.adopt(&path).unwrap());
        assert_eq!(
            h.residency(&path).unwrap(),
            Residency::Partial {
                resident: 256 * 1024,
                size: 4 * MIB
            }
        );
        let all = fs::read(&path).unwrap();
        let mut want = pattern(8, 0, 4 * MIB as usize);
        want[100..104].copy_from_slice(b"KEEP");
        assert_eq!(all, want);
        h.stop();
    }
    // Phase `without_a_serving_group_a_placeholder_reads_as_holes_until_adopted`.
    // The kernel drops a group's marks with the group. Until a new hydrator
    // adopts them, placeholders read as their holes, so the store must never
    // be exposed to readers without the daemon attached. This pins that
    // behaviour: if a kernel starts failing such reads instead, this fails
    // and the exposure rule can be relaxed.
    fn without_a_serving_group_a_placeholder_reads_as_holes_until_adopted() {
        let Some(s) = fanotify_scratch("hydrate_unserved") else {
            return;
        };
        let fetcher = PatternFetcher::new(Duration::ZERO);
        let path = s.live.join("orphan");
        let h = start(&fetcher, 64 * 1024);
        h.create_placeholder(&path, &spec(10, MIB)).unwrap();
        h.stop();
        let mut buf = vec![0xFFu8; 4096];
        File::open(&path)
            .unwrap()
            .read_exact_at(&mut buf, 0)
            .unwrap();
        assert!(
            buf.iter().all(|b| *b == 0),
            "an unserved placeholder returned data"
        );
        let h = start(&fetcher, 64 * 1024);
        assert!(h.adopt(&path).unwrap());
        File::open(&path)
            .unwrap()
            .read_exact_at(&mut buf, 0)
            .unwrap();
        assert_eq!(buf, pattern(10, 0, 4096));
        h.stop();
    }
    // Phase `replacing_a_file_with_a_placeholder_keeps_open_readers_on_the_old_version`.
    fn replacing_a_file_with_a_placeholder_keeps_open_readers_on_the_old_version() {
        let Some(s) = fanotify_scratch("hydrate_replace") else {
            return;
        };
        let fetcher = PatternFetcher::new(Duration::ZERO);
        let h = start(&fetcher, 64 * 1024);
        let path = s.live.join("doc");
        fs::write(&path, b"old version").unwrap();
        let old = File::open(&path).unwrap();
        h.replace_with_placeholder(&path, &spec(13, 3 * MIB))
            .unwrap();
        let mut before = String::new();
        (&old).read_to_string(&mut before).unwrap();
        assert_eq!(before, "old version", "an open reader keeps its inode");
        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.len(), 3 * MIB);
        assert_eq!(meta.permissions().mode() & 0o777, 0o640);
        let mut buf = vec![0u8; 4096];
        File::open(&path)
            .unwrap()
            .read_exact_at(&mut buf, MIB)
            .unwrap();
        assert_eq!(buf, pattern(13, MIB, 4096));
        h.stop();
    }
    // Phase `a_gated_file_refuses_writers_with_ebusy_and_still_serves_readers`.
    fn a_gated_file_refuses_writers_with_ebusy_and_still_serves_readers() {
        let Some(s) = fanotify_scratch("hydrate_gate") else {
            return;
        };
        let fetcher = PatternFetcher::new(Duration::ZERO);
        let h = start(&fetcher, 64 * 1024);
        let path = s.live.join("db.sqlite");
        fs::write(&path, vec![7u8; 64 * 1024]).unwrap();
        h.gate_writes(&path, libc::EBUSY).unwrap();

        // A reader is unaffected.
        let mut buf = vec![0u8; 4096];
        File::open(&path)
            .unwrap()
            .read_exact_at(&mut buf, 0)
            .unwrap();
        assert_eq!(buf, vec![7u8; 4096]);

        // A writer gets EBUSY from the access, not a torn file.
        let w = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let err = w.write_at(b"x", 0).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::EBUSY), "{err}");
        assert!(h.stats().gated_denials.load(Ordering::Relaxed) >= 1);
        // Each access is judged by the call that makes it: reads by the same
        // process, even through its writable descriptor, are served.
        let mut probe = vec![0u8; 16];
        File::open(&path)
            .unwrap()
            .read_exact_at(&mut probe, 0)
            .unwrap();
        w.read_exact_at(&mut probe, 16).unwrap();
        assert_eq!(probe, vec![7u8; 16]);

        // Once the lease is back, the same descriptor writes.
        h.ungate_writes(&path).unwrap();
        w.write_at(b"x", 0).unwrap();
        let mut want = vec![7u8; 64 * 1024];
        want[0] = b'x';
        assert_eq!(
            fs::read(&path).unwrap(),
            want,
            "the refused write left no trace"
        );
        h.stop();
    }
    // Phase `an_asking_gate_lets_the_write_policy_decide`.
    fn an_asking_gate_lets_the_write_policy_decide() {
        let Some(s) = fanotify_scratch("hydrate_ask") else {
            return;
        };
        let fetcher = PatternFetcher::new(Duration::ZERO);
        let h = start(&fetcher, 64 * 1024);
        let policy = Arc::new(Toggle {
            grant: std::sync::atomic::AtomicBool::new(false),
            asked: std::sync::atomic::AtomicU64::new(0),
        });
        h.set_write_policy(Arc::clone(&policy) as Arc<dyn fs_native::hydrate::WritePolicy>);
        let path = s.live.join("wal");
        fs::write(&path, vec![1u8; 8192]).unwrap();
        let key = h.gate(&path, fs_native::hydrate::WriteGate::Ask).unwrap();
        assert_eq!(key, Hydrator::file_key(&path).unwrap());

        // Readers never ask.
        let mut buf = vec![0u8; 16];
        File::open(&path)
            .unwrap()
            .read_exact_at(&mut buf, 0)
            .unwrap();
        assert_eq!(policy.asked.load(Ordering::Relaxed), 0);

        let w = OpenOptions::new().write(true).open(&path).unwrap();
        let err = w.write_at(b"z", 0).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::EBUSY));
        policy.grant.store(true, Ordering::Relaxed);
        w.write_at(b"z", 0).unwrap();
        assert!(policy.asked.load(Ordering::Relaxed) >= 2);
        h.stop();
    }
    // Phase `a_restarted_hydrator_adopts_placeholders_and_keeps_local_writes` (defined above).
    a_restarted_hydrator_adopts_placeholders_and_keeps_local_writes();
    // Phase `without_a_serving_group_a_placeholder_reads_as_holes_until_adopted` (defined above).
    without_a_serving_group_a_placeholder_reads_as_holes_until_adopted();
    // Phase `replacing_a_file_with_a_placeholder_keeps_open_readers_on_the_old_version` (defined above).
    replacing_a_file_with_a_placeholder_keeps_open_readers_on_the_old_version();
    // Phase `a_gated_file_refuses_writers_with_ebusy_and_still_serves_readers` (defined above).
    a_gated_file_refuses_writers_with_ebusy_and_still_serves_readers();
    // Phase `an_asking_gate_lets_the_write_policy_decide` (defined above).
    an_asking_gate_lets_the_write_policy_decide();
}

fn start_with(fetcher: &Arc<PatternFetcher>, granule: u64, readahead_max: u64) -> Hydrator {
    Hydrator::start(
        Arc::clone(fetcher) as Arc<dyn fs_native::hydrate::Fetcher>,
        HydratorConfig {
            granule,
            workers: 8,
            readahead_max,
            ..HydratorConfig::default()
        },
    )
    .unwrap()
}

/// Reads `path` front to back in 128 KiB `read(2)` calls, like `cat`.
fn read_sequentially(path: &std::path::Path) -> (Vec<u8>, Duration) {
    let t = Instant::now();
    let mut f = File::open(path).unwrap();
    let mut out = Vec::new();
    let mut buf = vec![0u8; 128 * 1024];
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    (out, t.elapsed())
}

struct Toggle {
    grant: std::sync::atomic::AtomicBool,
    asked: std::sync::atomic::AtomicU64,
}

impl fs_native::hydrate::WritePolicy for Toggle {
    fn admit_writer(&self, _file: (u64, u64)) -> Result<(), i32> {
        self.asked.fetch_add(1, Ordering::Relaxed);
        if self.grant.load(Ordering::Relaxed) {
            Ok(())
        } else {
            Err(libc::EBUSY)
        }
    }
}
