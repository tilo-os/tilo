//! The startup-time benchmark: the median spawn-to-exec of `/bin/true` must
//! stay under 100 ms. The design measures about 3 ms; this guards the order
//! of magnitude, not the exact number, so a slow CI machine still passes.
//!
//! A worker's heap can be tens of GiB, so a second test starts jobs from a
//! process holding a large one: the job's init must hold none of it.

use std::time::{Duration, Instant};

use tilo_sandbox::job::{JobSpec, Launch, Root};

use crate::needs_sandbox;

/// The median and 95th percentile time from spawn to the end of `/bin/true`.
fn startup(runs: usize) -> (Duration, Duration) {
    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs {
        let spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/true"]);
        let t0 = Instant::now();
        let mut job = Launch::new(&spec).spawn().expect("spawn /bin/true");
        // spawn() already returned after execve; waiting adds only the exit.
        let _ = job.wait();
        times.push(t0.elapsed());
    }
    times.sort_unstable();
    (times[times.len() / 2], times[times.len() * 95 / 100])
}

#[test]
fn startup_stays_under_100ms() {
    needs_sandbox!();
    let runs = 60;
    let (median, p95) = startup(runs);
    eprintln!("startup: median {median:?}, p95 {p95:?} over {runs} spawns");
    assert!(
        median < Duration::from_millis(100),
        "median startup {median:?} is over 100 ms"
    );
}

/// Memory in 4 KiB pages, as a long-running worker's heap mostly is, written
/// so every page is resident.
struct Heap {
    at: *mut u8,
    len: usize,
}

impl Heap {
    fn new(len: usize) -> Self {
        // SAFETY: a new private anonymous mapping.
        let at = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        assert_ne!(at, libc::MAP_FAILED, "map the heap");
        // SAFETY: the mapping just made.
        unsafe { libc::madvise(at, len, libc::MADV_NOHUGEPAGE) };
        for off in (0..len).step_by(4096) {
            // SAFETY: inside the mapping.
            unsafe { at.cast::<u8>().add(off).write_volatile(1) };
        }
        Self { at: at.cast(), len }
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        // SAFETY: the mapping `new` made.
        unsafe { libc::munmap(self.at.cast(), self.len) };
    }
}

/// A process's resident memory, in KiB.
fn rss_kib(pid: i32) -> u64 {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap_or(0)
}

#[test]
fn the_init_holds_none_of_the_callers_heap() {
    needs_sandbox!();
    let runs = 20;
    let (small, _) = startup(runs);
    let gib = 1;
    let heap = Heap::new(gib << 30);
    let (large, _) = startup(runs);
    eprintln!(
        "startup: median {small:?} with a small heap, {large:?} with {gib} GiB, \
         over {runs} spawns each"
    );
    let spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/sleep", "30"]);
    let job = Launch::new(&spec).spawn().expect("spawn sleep");
    let init_kib = rss_kib(job.pid());
    drop(job);
    drop(heap);
    eprintln!("startup: the job's init holds {} MiB", init_kib >> 10);
    assert!(
        init_kib < 64 << 10,
        "the job's init holds {init_kib} KiB of the caller's {gib} GiB heap"
    );
}
