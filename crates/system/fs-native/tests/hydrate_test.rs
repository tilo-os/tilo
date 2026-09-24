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
        },
    )
    .unwrap()
}

#[test]
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

#[test]
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

#[test]
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
    let xattr = fs_native_xattr(&path);
    assert!(xattr.is_none(), "residency xattr left on a resident file");
    // No mark remains: further reads raise no events.
    let before = h.stats().events.load(Ordering::Relaxed);
    let mut again = Vec::new();
    File::open(&path).unwrap().read_to_end(&mut again).unwrap();
    assert_eq!(h.stats().events.load(Ordering::Relaxed), before);
    h.stop();
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

#[test]
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

#[test]
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

#[test]
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

#[test]
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

/// The kernel drops a group's marks with the group. Until a new hydrator
/// adopts them, placeholders read as their holes, so the store must never
/// be exposed to readers without the daemon attached. This pins that
/// behaviour: if a kernel starts failing such reads instead, this fails
/// and the exposure rule can be relaxed.
#[test]
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

#[test]
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
