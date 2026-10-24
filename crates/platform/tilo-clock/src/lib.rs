//! Reading the time, once, the same way on every target.
//!
//! [`now`] and [`wall`] are the sanctioned monotonic and wall-clock reads, and
//! the re-exported clock types are the ones to name instead of `std::time`'s.
//! On Linux and macOS they are `std::time`'s own types, so values cross freely
//! into crates that name `std::time` directly. `std`'s clocks panic in a
//! browser page; there the page's clocks (`performance.now()` and
//! `Date.now()`) back the same API. [`to_std`] and [`from_std`] cross into and
//! out of `std::time::SystemTime` where an API names it.
//!
//! The `unix_now*` functions read the wall clock as integer time since the Unix
//! epoch. The unchecked forms return `0` when the system clock is set before
//! the epoch and saturate at `u64::MAX`; the `try_` forms return `None` for a
//! pre-epoch clock so callers that must fail closed can choose their own
//! fallback.
//!
//! [`Clock`] is the injectable millisecond source for code whose deadline
//! behaviour must be testable without sleeping: production passes
//! [`SystemClock`] (wall time) or [`MonotonicClock`] (never moves backwards),
//! tests pass [`FixedClock`] or [`ManualClock`].

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

pub use imp::{Duration, Instant, SystemTime, SystemTimeError, UNIX_EPOCH};

#[cfg(any(not(target_family = "wasm"), tilo_no_js))]
mod imp {
    pub use std::time::{Duration, Instant, SystemTime, SystemTimeError, UNIX_EPOCH};

    #[inline]
    pub const fn to_std(time: SystemTime) -> std::time::SystemTime {
        time
    }

    #[inline]
    pub const fn from_std(time: std::time::SystemTime) -> SystemTime {
        time
    }
}

#[cfg(all(target_family = "wasm", not(tilo_no_js)))]
mod imp {
    pub use web_time::{Duration, Instant, SystemTime, SystemTimeError, UNIX_EPOCH};

    pub fn to_std(time: SystemTime) -> std::time::SystemTime {
        let converted = match time.duration_since(UNIX_EPOCH) {
            Ok(after) => std::time::UNIX_EPOCH.checked_add(after),
            Err(before) => std::time::UNIX_EPOCH.checked_sub(before.duration()),
        };
        // Both clocks span far more than any real timestamp.
        converted.unwrap_or(std::time::UNIX_EPOCH)
    }

    pub fn from_std(time: std::time::SystemTime) -> SystemTime {
        let converted = match time.duration_since(std::time::UNIX_EPOCH) {
            Ok(after) => UNIX_EPOCH.checked_add(after),
            Err(before) => UNIX_EPOCH.checked_sub(before.duration()),
        };
        // Both clocks span far more than any real timestamp.
        converted.unwrap_or(UNIX_EPOCH)
    }
}

/// The current monotonic instant, for measuring elapsed time and scheduling
/// deadlines.
#[must_use]
#[inline]
pub fn now() -> Instant {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(at) = testing::monotonic_override() {
        return at;
    }
    Instant::now()
}

/// Per-thread clock scopes for tests: the monotonic clock for input work
/// and due deadlines, the wall clock for what a capture pins. Other threads
/// continue to use their ordinary clocks.
#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    use super::{Instant, SystemTime};
    use std::cell::Cell;

    thread_local! {
        static MONOTONIC: Cell<Option<Instant>> = const { Cell::new(None) };
    }

    pub(super) fn monotonic_override() -> Option<Instant> {
        MONOTONIC.with(Cell::get)
    }

    struct Restore(Option<Instant>);

    impl Drop for Restore {
        fn drop(&mut self) {
            MONOTONIC.with(|clock| clock.set(self.0));
        }
    }

    /// Run the real operation at one monotonic instant on this thread.
    /// Nested scopes and unwinding restore the previous reading. Pinning
    /// input cost keeps unrelated idle timers outside the observation; an
    /// explicit later scope can exercise those timers independently.
    pub fn with_monotonic_time<T>(at: Instant, operation: impl FnOnce() -> T) -> T {
        let _restore = Restore(MONOTONIC.with(|clock| clock.replace(Some(at))));
        operation()
    }

    struct RestoreWall(u64);

    impl Drop for RestoreWall {
        fn drop(&mut self) {
            super::FROZEN_HERE_MS.with(|here| here.set(self.0));
        }
    }

    thread_local! {
        static FOLLOWED: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicU64>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// The wall time pinned on the [`SharedWall`] this thread follows, in
    /// milliseconds; `0` while it runs or the thread follows none.
    pub(super) fn followed_ms() -> u64 {
        FOLLOWED.with(|followed| {
            followed
                .borrow()
                .as_ref()
                .map_or(0, |wall| wall.load(std::sync::atomic::Ordering::Relaxed))
        })
    }

    /// A wall clock a test pins and releases for threads it owns, such as a
    /// runtime's workers that answer on its behalf: each follows it from
    /// [`SharedWall::follow`] on. Threads that follow none keep their own
    /// clock, so the other tests of the process are left alone.
    #[derive(Clone, Debug, Default)]
    pub struct SharedWall(std::sync::Arc<std::sync::atomic::AtomicU64>);

    impl SharedWall {
        /// This thread reads this clock from now on: the pinned time while
        /// pinned, its ordinary clock otherwise. A pin of
        /// [`with_wall`] on the thread wins over it.
        pub fn follow(&self) {
            FOLLOWED.with(|followed| *followed.borrow_mut() = Some(self.0.clone()));
        }

        /// Every thread following this clock reads `at` until
        /// [`Self::release`].
        pub fn pin(&self, at: SystemTime) {
            self.0
                .store(super::wall_ms(at), std::sync::atomic::Ordering::Relaxed);
        }

        /// The threads following this clock read their ordinary clock again.
        pub fn release(&self) {
            self.0.store(0, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Run `operation` with the wall clock pinned to `at` on this thread
    /// alone, as [`super::freeze_wall`] pins it for a capture. Other threads
    /// (the other tests of one process) keep their own clock: a test of the
    /// pinned clock must not stop every notice and indicator beside it.
    /// Nested scopes and unwinding restore the previous reading.
    pub fn with_wall<T>(at: SystemTime, operation: impl FnOnce() -> T) -> T {
        let ms = super::wall_ms(at);
        let _restore = RestoreWall(super::FROZEN_HERE_MS.with(|here| here.replace(ms)));
        operation()
    }
}

#[cfg(test)]
mod scoped_monotonic_tests {
    use super::*;

    #[test]
    fn a_shared_wall_pins_the_threads_following_it_until_released() {
        // A time no other test of this crate pins process-wide.
        const PINNED: u64 = 123_456;
        let wall = testing::SharedWall::default();
        let follower = {
            let wall = wall.clone();
            move || {
                wall.follow();
                unix_now()
            }
        };
        assert_ne!(std::thread::spawn(follower.clone()).join().unwrap(), PINNED);
        wall.pin(UNIX_EPOCH + Duration::from_secs(PINNED));
        assert_eq!(std::thread::spawn(follower.clone()).join().unwrap(), PINNED);
        assert_ne!(
            unix_now(),
            PINNED,
            "a thread following none keeps its clock"
        );
        wall.release();
        assert_ne!(std::thread::spawn(follower).join().unwrap(), PINNED);
    }

    #[test]
    fn scoped_time_is_thread_local_nested_and_restored_after_unwind() {
        let at = now().checked_add(Duration::from_secs(60)).unwrap();
        testing::with_monotonic_time(at, || {
            assert_eq!(now(), at);
            let later = at.checked_add(Duration::from_secs(1)).unwrap();
            testing::with_monotonic_time(later, || assert_eq!(now(), later));
            assert_eq!(now(), at);
            assert!(std::thread::spawn(now).join().unwrap() < at);
            let panicked = std::panic::catch_unwind(|| {
                testing::with_monotonic_time(later, || panic!("unwind the clock scope"));
            });
            assert!(panicked.is_err());
            assert_eq!(now(), at);
        });
        assert!(now() < at);
    }
}

/// Wall time pinned by [`freeze_wall`], in milliseconds since the Unix epoch;
/// `0` while the clock runs.
static FROZEN_WALL_MS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The wall time [`freeze_wall`] pinned on this thread, which wins over
    /// the process-wide one here: two tests pinning different times on
    /// their own threads of one process each read their own.
    static FROZEN_HERE_MS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The pinned wall time this thread reads, in milliseconds; `0` while the
/// clock runs.
fn frozen_ms() -> u64 {
    match FROZEN_HERE_MS.with(std::cell::Cell::get) {
        0 => match followed_ms() {
            0 => FROZEN_WALL_MS.load(Ordering::Relaxed),
            followed => followed,
        },
        here => here,
    }
}

/// The wall time of the test clock this thread follows
/// ([`testing::SharedWall`]); `0` when none is pinned.
#[cfg(any(test, feature = "test-support"))]
fn followed_ms() -> u64 {
    testing::followed_ms()
}

#[cfg(not(any(test, feature = "test-support")))]
const fn followed_ms() -> u64 {
    0
}

/// The current wall-clock time, for timestamps shown to people or persisted.
/// After [`freeze_wall`] it is that instant, every time.
#[must_use]
#[inline]
pub fn wall() -> SystemTime {
    match frozen_ms() {
        0 => SystemTime::now(),
        ms => UNIX_EPOCH
            .checked_add(Duration::from_millis(ms))
            .unwrap_or(UNIX_EPOCH),
    }
}

/// Pin the wall clock to `at` for the rest of the process, so a screenshot
/// capture shows the same date and time on every run. The monotonic clock
/// ([`now`]) keeps running: animations and timeouts still advance. Never
/// called in a live session.
///
/// The calling thread reads `at` from then on even when another thread
/// pins another time later, so tests sharing one process each keep the
/// time they chose; other threads read the last time pinned anywhere.
pub fn freeze_wall(at: SystemTime) {
    let ms = wall_ms(at);
    FROZEN_WALL_MS.store(ms, Ordering::Relaxed);
    FROZEN_HERE_MS.with(|here| here.set(ms));
}

/// `at` as a pinned reading: milliseconds since the Unix epoch, never `0`
/// (which means the clock runs).
fn wall_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(1, |since| saturating_u64(since.as_millis()).max(1))
}

/// Whether [`freeze_wall`] pinned the wall clock.
#[must_use]
pub fn wall_frozen() -> bool {
    frozen_ms() != 0
}

/// The IANA name of the local time zone, when the system names one: `$TZ`
/// (without a leading `:`), else the zone `/etc/localtime` links to below
/// `zoneinfo/`. `None` where the system names none, and on the web; local
/// times still follow the system's offsets there. Callers check the name
/// against their zone database.
#[must_use]
pub fn local_zone_name() -> Option<String> {
    #[cfg(all(unix, not(target_family = "wasm")))]
    {
        if let Ok(tz) = std::env::var("TZ") {
            let tz = tz.trim_start_matches(':');
            if !tz.is_empty() && !tz.starts_with('/') {
                return Some(tz.to_owned());
            }
        }
        let link = std::fs::read_link("/etc/localtime").ok()?;
        let (_, zone) = link.to_str()?.split_once("zoneinfo/")?;
        (!zone.is_empty()).then(|| zone.to_owned())
    }
    #[cfg(not(all(unix, not(target_family = "wasm"))))]
    {
        None
    }
}

/// A wall-clock time as `std::time`'s, for crates and APIs that name
/// `std::time::SystemTime` directly. The identity where the clock is native.
#[must_use]
#[inline]
pub fn to_std(time: SystemTime) -> std::time::SystemTime {
    imp::to_std(time)
}

/// A `std::time` wall-clock time, such as a file's modification time, as this
/// crate's. The identity where the clock is native.
#[must_use]
#[inline]
pub fn from_std(time: std::time::SystemTime) -> SystemTime {
    imp::from_std(time)
}

fn since_epoch() -> Option<Duration> {
    wall().duration_since(UNIX_EPOCH).ok()
}

fn saturating_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Whole seconds since the Unix epoch, or `None` if the clock precedes it.
pub fn try_unix_now() -> Option<u64> {
    since_epoch().map(|duration| duration.as_secs())
}

/// Milliseconds since the Unix epoch, or `None` if the clock precedes it.
pub fn try_unix_now_ms() -> Option<u64> {
    since_epoch().map(|duration| saturating_u64(duration.as_millis()))
}

/// Nanoseconds since the Unix epoch, or `None` if the clock precedes it.
pub fn try_unix_now_ns() -> Option<u64> {
    since_epoch().map(|duration| saturating_u64(duration.as_nanos()))
}

/// Whole seconds since the Unix epoch; `0` if the clock precedes it.
pub fn unix_now() -> u64 {
    try_unix_now().unwrap_or(0)
}

/// Milliseconds since the Unix epoch; `0` if the clock precedes it.
pub fn unix_now_ms() -> u64 {
    try_unix_now_ms().unwrap_or(0)
}

/// Nanoseconds since the Unix epoch; `0` if the clock precedes it.
pub fn unix_now_ns() -> u64 {
    try_unix_now_ns().unwrap_or(0)
}

/// Whole seconds since the Unix epoch as a signed timestamp; `0` if the clock
/// precedes it.
pub fn unix_now_i64() -> i64 {
    i64::try_from(unix_now()).unwrap_or(i64::MAX)
}

/// Milliseconds since the Unix epoch as a signed timestamp; `0` if the clock
/// precedes it.
pub fn unix_now_ms_i64() -> i64 {
    i64::try_from(unix_now_ms()).unwrap_or(i64::MAX)
}

/// An injectable millisecond time source.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// Wall-clock milliseconds since the Unix epoch.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        unix_now_ms()
    }
}

/// Milliseconds since this process first read a monotonic clock. Never moves
/// backwards, so it suits process-local lifetimes that must not be extended
/// or cut short by wall-clock adjustments.
#[derive(Clone, Copy, Debug, Default)]
pub struct MonotonicClock;

impl Clock for MonotonicClock {
    fn now_ms(&self) -> u64 {
        static START: OnceLock<Instant> = OnceLock::new();
        saturating_u64(START.get_or_init(now).elapsed().as_millis())
    }
}

/// A clock stopped at one instant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FixedClock(pub u64);

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0
    }
}

/// A clock that moves only when told to.
#[derive(Debug, Default)]
pub struct ManualClock(AtomicU64);

impl ManualClock {
    pub const fn new(now_ms: u64) -> Self {
        Self(AtomicU64::new(now_ms))
    }

    pub fn set(&self, now_ms: u64) {
        self.0.store(now_ms, Ordering::SeqCst);
    }

    /// Move forward by `delta_ms`, saturating at `u64::MAX`.
    pub fn advance(&self, delta_ms: u64) {
        let _ = self
            .0
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |now| {
                Some(now.saturating_add(delta_ms))
            });
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_keeps_the_wall_time_it_pinned_when_another_pins_another() {
        let first = UNIX_EPOCH + Duration::from_secs(1_000);
        let second = UNIX_EPOCH + Duration::from_secs(2_000);
        let pinned = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let early = scope.spawn(|| {
                freeze_wall(first);
                pinned.wait();
                pinned.wait();
                wall()
            });
            let late = scope.spawn(|| {
                pinned.wait();
                freeze_wall(second);
                pinned.wait();
                wall()
            });
            assert_eq!(
                early.join().ok(),
                Some(first),
                "the first thread keeps its own time"
            );
            assert_eq!(late.join().ok(), Some(second));
        });
    }

    #[test]
    fn a_frozen_wall_clock_holds_while_the_monotonic_clock_runs() {
        let at = UNIX_EPOCH + Duration::from_secs(1_782_816_120);
        let before = now();
        freeze_wall(at);
        assert!(wall_frozen());
        assert_eq!(wall(), at);
        assert_eq!(unix_now(), 1_782_816_120);
        assert_eq!(wall(), at, "it stays put");
        assert!(now() >= before, "the monotonic clock is not frozen");
    }

    #[test]
    fn clocks_agree_on_units_move_only_as_told_and_never_backwards() {
        {
            let secs = unix_now();
            let ms = unix_now_ms();
            let ns = unix_now_ns();
            assert!(secs > 1_600_000_000);
            assert!(ms / 1_000 >= secs);
            assert!(ns / 1_000_000 >= ms);
            assert_eq!(unix_now_i64(), i64::try_from(unix_now()).unwrap());
        }
        {
            assert_eq!(FixedClock(7).now_ms(), 7);
            let clock = ManualClock::new(10);
            assert_eq!(clock.now_ms(), 10);
            clock.advance(5);
            assert_eq!(clock.now_ms(), 15);
            clock.set(3);
            assert_eq!(clock.now_ms(), 3);
            clock.set(u64::MAX - 1);
            clock.advance(10);
            assert_eq!(clock.now_ms(), u64::MAX);
        }
        {
            let first = MonotonicClock.now_ms();
            assert!(MonotonicClock.now_ms() >= first);
        }
    }

    #[test]
    fn now_is_monotonic() {
        let earlier = now();
        assert!(now() >= earlier);
    }

    #[test]
    fn wall_is_after_the_epoch() {
        assert!(wall().duration_since(UNIX_EPOCH).is_ok());
    }

    #[test]
    fn std_conversions_round_trip_on_both_sides_of_the_epoch() {
        let offset = Duration::from_millis(1_700_000_000_123);
        for time in [UNIX_EPOCH + offset, UNIX_EPOCH - offset, UNIX_EPOCH] {
            assert_eq!(from_std(to_std(time)), time);
        }
        assert_eq!(to_std(UNIX_EPOCH + offset), std::time::UNIX_EPOCH + offset);
    }
}
