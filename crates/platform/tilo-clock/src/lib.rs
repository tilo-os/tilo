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

#[cfg(not(target_family = "wasm"))]
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

#[cfg(target_family = "wasm")]
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
#[allow(
    clippy::disallowed_methods,
    reason = "the one sanctioned monotonic clock read"
)]
pub fn now() -> Instant {
    Instant::now()
}

/// Wall time pinned by [`freeze_wall`], in milliseconds since the Unix epoch;
/// `0` while the clock runs.
static FROZEN_WALL_MS: AtomicU64 = AtomicU64::new(0);

/// The current wall-clock time, for timestamps shown to people or persisted.
/// After [`freeze_wall`] it is that instant, every time.
#[must_use]
#[inline]
#[allow(
    clippy::disallowed_methods,
    reason = "the one sanctioned wall-clock read"
)]
pub fn wall() -> SystemTime {
    match FROZEN_WALL_MS.load(Ordering::Relaxed) {
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
pub fn freeze_wall(at: SystemTime) {
    let ms = at
        .duration_since(UNIX_EPOCH)
        .map_or(1, |since| saturating_u64(since.as_millis()).max(1));
    FROZEN_WALL_MS.store(ms, Ordering::Relaxed);
}

/// Whether [`freeze_wall`] pinned the wall clock.
#[must_use]
pub fn wall_frozen() -> bool {
    FROZEN_WALL_MS.load(Ordering::Relaxed) != 0
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
        // Phase `wall_clock_units_agree`.
        {
            let secs = unix_now();
            let ms = unix_now_ms();
            let ns = unix_now_ns();
            assert!(secs > 1_600_000_000);
            assert!(ms / 1_000 >= secs);
            assert!(ns / 1_000_000 >= ms);
            assert_eq!(unix_now_i64(), i64::try_from(unix_now()).unwrap());
        }
        // Phase `test_clocks_hold_and_move_as_told`.
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
        // Phase `monotonic_clock_never_moves_backwards`.
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
