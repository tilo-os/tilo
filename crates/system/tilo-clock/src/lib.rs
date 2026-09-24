//! Reading the time, once.
//!
//! The free functions read the wall clock as integer time since the Unix
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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn since_epoch() -> Option<Duration> {
    SystemTime::now().duration_since(UNIX_EPOCH).ok()
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
        saturating_u64(START.get_or_init(Instant::now).elapsed().as_millis())
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
    fn wall_clock_units_agree() {
        let secs = unix_now();
        let ms = unix_now_ms();
        let ns = unix_now_ns();
        assert!(secs > 1_600_000_000);
        assert!(ms / 1_000 >= secs);
        assert!(ns / 1_000_000 >= ms);
        assert_eq!(unix_now_i64(), i64::try_from(unix_now()).unwrap());
    }

    #[test]
    fn test_clocks_hold_and_move_as_told() {
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

    #[test]
    fn monotonic_clock_never_moves_backwards() {
        let first = MonotonicClock.now_ms();
        assert!(MonotonicClock.now_ms() >= first);
    }
}
