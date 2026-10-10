//! Handing the heap a closed window freed back to the kernel (TIL-705).
//!
//! glibc keeps memory it has been given back. A whole-frame buffer freed
//! after the allocator's dynamic mmap threshold rose past its size stays
//! inside the arena it came from, and an arena returns only the free space
//! at its very top. So after a window closed, its frames, its atlas space
//! and the converted copies of its pixels stayed resident: the TIL-692
//! cycle test measured 61–75 MiB free inside glibc's arenas and still
//! counted in the compositor's anonymous RSS.
//!
//! Closing a window is when a lot is freed at once, so a close asks for one
//! `malloc_trim(0)`, which walks every arena and releases each whole free
//! page. The trim runs on a thread of its own, never the compositor's: it
//! takes each arena's lock while it walks it. It waits until the closes have
//! settled (Vello evicts the window's last frame two paints later, and a
//! client often destroys several surfaces in a row) and runs at most once per
//! [`MIN_INTERVAL`]. Between requests the thread sleeps on a condition
//! variable, so an idle session never wakes for it.
//!
//! The same trim returns what a software GPU freed (TIL-731). Lavapipe keeps
//! every page of device memory a closed window's textures touched in a memfd
//! until something punches it out; a renderer on lavapipe registers its
//! device with [`register_device`], and each trim hands the device's free
//! ranges back to the kernel (`crate::shell::render::software_gpu_memory`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use vello::wgpu;

/// Quiet time after the last request before trimming.
const SETTLE: Duration = Duration::from_secs(1);
/// Longest a request waits for quiet: a client that destroys surfaces
/// continuously must not postpone the trim forever.
const MAX_DELAY: Duration = Duration::from_secs(5);
/// Shortest time between two trims.
const MIN_INTERVAL: Duration = Duration::from_secs(10);

/// `instant` plus `duration`; an instant past the clock's range stays put.
fn after(instant: Instant, duration: Duration) -> Instant {
    instant.checked_add(duration).unwrap_or(instant)
}

/// Requests not yet served, and when the last trim ran.
#[derive(Debug, Default)]
struct Schedule {
    /// First and latest request since the last trim.
    pending: Option<(Instant, Instant)>,
    last_trim: Option<Instant>,
    /// A trim taken off the schedule is running.
    trimming: bool,
}

impl Schedule {
    fn request(&mut self, now: Instant) {
        let first = self.pending.map_or(now, |(first, _)| first);
        self.pending = Some((first, now));
    }

    /// When the pending requests are served; `None` with nothing pending.
    fn due(&self) -> Option<Instant> {
        let (first, last) = self.pending?;
        let settled = after(last, SETTLE).min(after(first, MAX_DELAY));
        Some(
            self.last_trim
                .map_or(settled, |trimmed| settled.max(after(trimmed, MIN_INTERVAL))),
        )
    }

    /// Whether to trim at `now`; if so, the requests count as served.
    fn take(&mut self, now: Instant) -> bool {
        if self.due().is_some_and(|due| due <= now) {
            self.pending = None;
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
    DEVICES.lock().unwrap_or_else(PoisonError::into_inner)
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
    trimmer
        .schedule
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .request(Instant::now());
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
            let schedule = trimmer
                .schedule
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
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
    let mut schedule = trimmer
        .schedule
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    loop {
        let now = Instant::now();
        if schedule.take(now) {
            drop(schedule);
            trim();
            schedule = trimmer
                .schedule
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            schedule.trimming = false;
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
    tracing::debug!(
        elapsed = ?start.elapsed(),
        rss_anon_before_kib = ?before,
        rss_anon_after_kib = ?rss_anon_kib(),
        "returned freed heap to the kernel"
    );
    release_device_memory();
}

/// Return what each registered software-GPU device freed (TIL-731).
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
}
