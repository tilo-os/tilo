//! Handing the heap a closed window freed back to the kernel.
//!
//! glibc keeps memory it has been given back. A whole-frame buffer freed
//! after the allocator's dynamic mmap threshold rose past its size stays
//! inside the arena it came from, and an arena returns only the free space
//! at its very top. So after a window closed, its frames, its atlas space
//! and the converted copies of its pixels stayed resident: the #3971
//! cycle test measured 61–75 MiB free inside glibc's arenas and still
//! counted in the compositor's anonymous RSS.
//!
//! Closing a window is when a lot is freed at once, so a close asks for one
//! `malloc_trim(0)`, which walks every arena and releases each whole free
//! page. The trim runs on a thread of its own, never the compositor's: it
//! takes each arena's lock while it walks it. It waits until the closes have
//! settled (Vello evicts the window's last frame two paints later, and a
//! client often destroys several surfaces in a row) and runs at most once per
//! `MIN_INTERVAL`. Between requests the thread sleeps on a condition
//! variable, so an idle session never wakes for it.
//!
//! The same trim returns what a software GPU freed. Lavapipe keeps
//! every page of device memory a closed window's textures touched in a memfd
//! until something punches it out; a renderer on lavapipe registers its
//! device with [`register_device`], and each trim hands the device's free
//! ranges back to the kernel (`crate::shell::render::software_gpu_memory`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use tilo_lock::MutexRecover as _;
use vello::wgpu;

/// Quiet time after the last request before trimming.
const SETTLE: Duration = Duration::from_secs(1);
/// Longest a request waits for quiet: a client that destroys surfaces
/// continuously must not postpone the trim forever.
const MAX_DELAY: Duration = Duration::from_secs(5);
/// Shortest time between two trims.
const MIN_INTERVAL: Duration = Duration::from_secs(10);
/// How long after the compositor starts its startup work (renderer warm-up,
/// identity, network, search, font warm-up) has freed its scratch, for the
/// one trim that returns it.
pub const STARTUP_SETTLED: Duration = Duration::from_secs(30);
/// How long the compositor stays quiet after work before a quiet trim
/// ([`request_when_quiet`]).
const QUIET_SETTLE: Duration = Duration::from_secs(2);

/// `instant` plus `duration`; an instant past the clock's range stays put.
fn after(instant: Instant, duration: Duration) -> Instant {
    instant.checked_add(duration).unwrap_or(instant)
}

/// Requests not yet served, and when the last trim ran.
#[derive(Debug, Default)]
struct Schedule {
    /// First and latest request since the last trim.
    pending: Option<(Instant, Instant)>,
    /// The earliest trim asked for at a set time ([`request_after`]).
    later: Option<Instant>,
    /// When the compositor last went quiet after work ([`request_when_quiet`]).
    quiet: Option<Instant>,
    last_trim: Option<Instant>,
    /// A trim taken off the schedule is running.
    trimming: bool,
    /// The running trim serves a set-time request ([`request_after`]).
    serving_set_time: bool,
}

impl Schedule {
    fn request(&mut self, now: Instant) {
        let first = self.pending.map_or(now, |(first, _)| first);
        self.pending = Some((first, now));
    }

    fn request_at(&mut self, at: Instant) {
        self.later = Some(self.later.map_or(at, |later| later.min(at)));
    }

    fn request_quiet(&mut self, now: Instant) {
        self.quiet = Some(now);
    }

    /// When the pending requests are served; `None` with nothing pending.
    fn due(&self) -> Option<Instant> {
        let settled = self
            .pending
            .map(|(first, last)| after(last, SETTLE).min(after(first, MAX_DELAY)));
        let quiet = self.quiet.map(|quiet| after(quiet, QUIET_SETTLE));
        let soonest = [settled, self.later, quiet].into_iter().flatten().min()?;
        Some(
            self.last_trim
                .map_or(soonest, |trimmed| soonest.max(after(trimmed, MIN_INTERVAL))),
        )
    }

    /// Whether to trim at `now`; if so, the requests due by then count as
    /// served.
    fn take(&mut self, now: Instant) -> bool {
        if self.due().is_some_and(|due| due <= now) {
            self.pending = None;
            self.serving_set_time = self.later.is_some_and(|later| later <= now);
            if self.serving_set_time {
                self.later = None;
            }
            self.quiet = None;
            self.last_trim = Some(now);
            self.trimming = true;
            return true;
        }
        false
    }
}

struct Trimmer {
    schedule: Mutex<Schedule>,
    wake: Condvar,
}

static TRIMMER: OnceLock<Option<&'static Trimmer>> = OnceLock::new();
static TRIMS: AtomicU64 = AtomicU64::new(0);
static STARTUP_TRIMMED: AtomicBool = AtomicBool::new(false);

/// Software-GPU devices whose freed memory each trim returns, by registration.
static DEVICES: Mutex<Vec<(u64, wgpu::Device)>> = Mutex::new(Vec::new());
static NEXT_DEVICE: AtomicU64 = AtomicU64::new(0);

/// A device's place among those each trim returns freed GPU memory for;
/// dropping it takes the device off the list.
#[derive(Debug)]
pub struct DeviceRegistration(u64);

impl Drop for DeviceRegistration {
    fn drop(&mut self) {
        devices().retain(|(id, _)| *id != self.0);
    }
}

fn devices() -> MutexGuard<'static, Vec<(u64, wgpu::Device)>> {
    DEVICES.lock_recover()
}

/// Have every trim return what `device` freed, while the registration lives.
/// Only a software rasteriser keeps freed device memory, so a device on any
/// other adapter is not registered.
pub fn register_device(
    adapter: &wgpu::AdapterInfo,
    device: &wgpu::Device,
) -> Option<DeviceRegistration> {
    if adapter.device_type != wgpu::DeviceType::Cpu || !supported() {
        return None;
    }
    let id = NEXT_DEVICE.fetch_add(1, Ordering::Relaxed);
    devices().push((id, device.clone()));
    Some(DeviceRegistration(id))
}

/// Ask for freed heap to be returned soon: a window closed, or something
/// else released a lot at once. Cheap and non-blocking on the caller's
/// thread; any number of requests within a few seconds make one trim.
pub fn request() {
    let Some(trimmer) = trimmer() else {
        return;
    };
    trimmer.schedule.lock_recover().request(Instant::now());
    trimmer.wake.notify_one();
}

/// Windows closed so far in this process; see [`window_closed`].
static WINDOWS_CLOSED: AtomicU64 = AtomicU64::new(0);

/// A window closed: its frames are gone for good. Renderers bring their
/// image atlas down within their next few frames
/// ([`windows_closed`]), and a trim returns what they and the window freed.
pub fn window_closed() {
    WINDOWS_CLOSED.fetch_add(1, Ordering::AcqRel);
    request();
}

/// Windows closed so far, for a renderer to tell whether one closed since
/// its last frame.
pub fn windows_closed() -> u64 {
    WINDOWS_CLOSED.load(Ordering::Acquire)
}

/// Ask for one trim `delay` from now, whatever is requested meanwhile: work
/// that frees its scratch at no single moment (a session's startup) is
/// trimmed once it is over. The trim thread sleeps until then, one wakeup.
pub fn request_after(delay: Duration) {
    let Some(trimmer) = trimmer() else {
        return;
    };
    trimmer
        .schedule
        .lock_recover()
        .request_at(after(Instant::now(), delay));
    trimmer.wake.notify_one();
}

/// The compositor goes quiet after work (its startup, a burst of input):
/// trim once it has stayed quiet a moment. A later call replaces an earlier
/// one not yet served, so work that keeps going never trims mid-way.
pub fn request_when_quiet() {
    let Some(trimmer) = trimmer() else {
        return;
    };
    trimmer
        .schedule
        .lock_recover()
        .request_quiet(Instant::now());
    trimmer.wake.notify_one();
}

/// Whether the trim asked for at startup ([`request_after`]) has run, so
/// startup's scratch is back with the kernel.
pub fn startup_trimmed() -> bool {
    STARTUP_TRIMMED.load(Ordering::Acquire)
}

/// Trims run so far in this process.
pub fn trims_completed() -> u64 {
    TRIMS.load(Ordering::Acquire)
}

/// Wait until every request so far has been served and no trim is running;
/// `false` if that takes longer than `timeout`.
#[cfg(test)]
pub(crate) fn settle(timeout: Duration) -> bool {
    let deadline = after(Instant::now(), timeout);
    loop {
        let served = trimmer().is_none_or(|trimmer| {
            let schedule = trimmer.schedule.lock_recover();
            schedule.pending.is_none() && !schedule.trimming
        });
        if served {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn trimmer() -> Option<&'static Trimmer> {
    *TRIMMER.get_or_init(|| {
        if !supported() {
            return None;
        }
        let trimmer: &'static Trimmer = Box::leak(Box::new(Trimmer {
            schedule: Mutex::new(Schedule::default()),
            wake: Condvar::new(),
        }));
        match std::thread::Builder::new()
            .name("tilo-heap-trim".into())
            .spawn(move || serve(trimmer))
        {
            Ok(_) => Some(trimmer),
            Err(error) => {
                tracing::warn!(%error, "heap trim thread did not start; freed heap stays in glibc");
                None
            }
        }
    })
}

fn serve(trimmer: &'static Trimmer) {
    let mut schedule = trimmer.schedule.lock_recover();
    loop {
        let now = Instant::now();
        if schedule.take(now) {
            drop(schedule);
            trim();
            schedule = trimmer.schedule.lock_recover();
            schedule.trimming = false;
            if schedule.serving_set_time {
                STARTUP_TRIMMED.store(true, Ordering::Release);
            }
            TRIMS.fetch_add(1, Ordering::AcqRel);
            continue;
        }
        schedule = match schedule.due() {
            None => trimmer
                .wake
                .wait(schedule)
                .unwrap_or_else(PoisonError::into_inner),
            Some(due) => {
                trimmer
                    .wake
                    .wait_timeout(schedule, due.saturating_duration_since(now))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
        };
    }
}

const fn supported() -> bool {
    cfg!(all(target_os = "linux", target_env = "gnu"))
}

/// One `HEAP-MILESTONE` line for the session log: glibc's heap in use and free
/// and the process's anonymous RSS, at `what` (the compositor's start, its
/// renderer's first frame, each trim). The vm-acceptance memory checks quote
/// these lines, so an over-budget compositor says when its heap grew: at the
/// renderer's first frame (a software GPU compiling its shaders) or later.
pub fn log_milestone(what: &str) {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: mallinfo2 only reads allocator statistics; it takes no
        // arguments and returns a plain struct by value.
        let info = unsafe { libc::mallinfo2() };
        let mib = |bytes: usize| bytes >> 20;
        // A measurement line for the session log, like RENDERER-MEMORY.
        eprintln!(
            "HEAP-MILESTONE what={what} in_use_mib={} free_mib={} rss_anon_mib={}",
            mib(info.uordblks),
            mib(info.fordblks),
            rss_anon_kib().map_or_else(|| "-".to_owned(), |kib| (kib >> 10).to_string()),
        );
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    let _ = what;
}

/// Return every whole free page in every arena to the kernel.
fn trim() {
    let start = Instant::now();
    let before = rss_anon_kib();
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim only releases memory the allocator holds free; it
    // takes each arena's own lock, so it is safe beside other threads.
    unsafe {
        libc::malloc_trim(0);
    }
    log_milestone("trim");
    tracing::debug!(
        elapsed = ?start.elapsed(),
        rss_anon_before_kib = ?before,
        rss_anon_after_kib = ?rss_anon_kib(),
        "returned freed heap to the kernel"
    );
    release_device_memory();
}

/// Return what each registered software-GPU device freed.
fn release_device_memory() {
    const MIB: u64 = 1 << 20;
    let registered: Vec<wgpu::Device> =
        devices().iter().map(|(_, device)| device.clone()).collect();
    for device in registered {
        let start = Instant::now();
        #[cfg(target_os = "linux")]
        if let Some(released) =
            crate::shell::render::software_gpu_memory::release_freed_memory(&device)
        {
            // A measurement line for the session log, like WORKSPACE-SWITCH.
            eprintln!(
                "DEVICE-MEMORY released_mib={} before_mib={} after_mib={} allocations={} elapsed_ms={}",
                released.bytes() / MIB,
                released.before_bytes / MIB,
                released.after_bytes / MIB,
                released.allocations,
                start.elapsed().as_millis(),
            );
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (device, start);
    }
}

/// This process's anonymous resident memory, in KiB, for the log line.
fn rss_anon_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("RssAnon:")?
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse()
                .ok()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, millis: u64) -> Instant {
        after(start, Duration::from_millis(millis))
    }

    #[test]
    fn a_burst_of_closes_is_served_by_one_trim_once_it_settles() {
        let start = Instant::now();
        let mut schedule = Schedule::default();
        assert_eq!(schedule.due(), None);
        assert!(!schedule.take(start), "nothing was asked for");
        for millis in [0, 200, 400] {
            schedule.request(at(start, millis));
        }
        assert_eq!(schedule.due(), Some(at(start, 1_400)));
        assert!(!schedule.take(at(start, 1_399)));
        assert!(schedule.take(at(start, 1_400)));
        assert!(schedule.trimming);
        assert_eq!(schedule.due(), None, "the burst was served");
        assert!(!schedule.take(at(start, 9_000)));
    }

    #[test]
    fn requests_that_never_stop_still_trim_within_the_longest_delay() {
        let start = Instant::now();
        let mut schedule = Schedule::default();
        for millis in (0..=6_000).step_by(500) {
            schedule.request(at(start, millis));
        }
        assert_eq!(schedule.due(), Some(at(start, 5_000)));
    }

    #[test]
    fn trims_are_at_least_the_shortest_interval_apart() {
        let start = Instant::now();
        let mut schedule = Schedule::default();
        schedule.request(start);
        assert!(schedule.take(at(start, 1_000)));
        schedule.request(at(start, 2_000));
        assert_eq!(schedule.due(), Some(at(start, 11_000)));
        assert!(!schedule.take(at(start, 3_000)));
        assert!(schedule.take(at(start, 11_000)));
    }

    #[test]
    fn a_trim_asked_for_later_survives_the_trims_before_it() {
        let start = Instant::now();
        let mut schedule = Schedule::default();
        // The first frame asks for one now and one when startup is over.
        schedule.request(start);
        schedule.request_at(at(start, 30_000));
        assert_eq!(schedule.due(), Some(at(start, 1_000)));
        assert!(schedule.take(at(start, 1_000)));
        assert_eq!(
            schedule.due(),
            Some(at(start, 30_000)),
            "the later trim is still owed"
        );
        assert!(!schedule.take(at(start, 29_999)));
        assert!(schedule.take(at(start, 30_000)));
        assert_eq!(schedule.due(), None, "both were served");
    }

    #[test]
    fn a_quiet_trim_waits_for_the_last_quiet_moment_and_leaves_a_later_one_owed() {
        let start = Instant::now();
        let mut schedule = Schedule::default();
        schedule.request_at(at(start, 30_000));
        schedule.request_quiet(at(start, 4_000));
        schedule.request_quiet(at(start, 5_000));
        assert_eq!(
            schedule.due(),
            Some(at(start, 7_000)),
            "the last quiet moment counts"
        );
        assert!(schedule.take(at(start, 7_000)));
        assert_eq!(
            schedule.due(),
            Some(at(start, 30_000)),
            "startup's trim is still owed"
        );
        assert!(schedule.take(at(start, 30_000)));
        assert_eq!(schedule.due(), None);
    }

    #[test]
    fn a_trim_asked_for_later_keeps_the_shortest_interval() {
        let start = Instant::now();
        let mut schedule = Schedule::default();
        schedule.request(start);
        assert!(schedule.take(at(start, 1_000)));
        schedule.request_at(at(start, 2_000));
        assert_eq!(schedule.due(), Some(at(start, 11_000)));
    }
}
