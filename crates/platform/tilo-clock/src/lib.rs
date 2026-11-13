//! Reading the time the same way on every target: [`now`] and [`wall`] are the
//! sanctioned monotonic and wall-clock reads, and [`time`] names the clock types
//! instead of `std::time`'s, which panic in a browser page. [`Clock`] is an
//! injectable time source, for deadline behaviour a test reaches without sleeping.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The clock types under `std::time`'s names, for signatures that need one;
/// times are read through [`now`] and [`wall`].
pub mod time {
    pub use crate::imp::{Duration, Instant, SystemTime, SystemTimeError, UNIX_EPOCH};
}

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
        converted.unwrap_or(std::time::UNIX_EPOCH)
    }

    pub fn from_std(time: std::time::SystemTime) -> SystemTime {
        let converted = match time.duration_since(std::time::UNIX_EPOCH) {
            Ok(after) => UNIX_EPOCH.checked_add(after),
            Err(before) => UNIX_EPOCH.checked_sub(before.duration()),
        };
        converted.unwrap_or(UNIX_EPOCH)
    }
}

#[must_use]
#[inline]
pub fn now() -> Instant {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(at) = testing::monotonic_override() {
        return at;
    }
    Instant::now()
}

#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    use super::time::{Instant, SystemTime};
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

    pub fn with_monotonic_time<T>(at: Instant, operation: impl FnOnce() -> T) -> T {
        let _restore = Restore(MONOTONIC.with(|clock| clock.replace(Some(at))));
        operation()
    }

    struct RestoreWall(Option<u64>);

    impl Drop for RestoreWall {
        fn drop(&mut self) {
            super::FROZEN_HERE_MS.with(|here| here.set(self.0));
        }
    }

    /// A test's right to pin its process's wall clock: only
    /// `tilo_test_env::in_own_process` gives one, and `policy/ui-clock-contract`
    /// refuses [`hold_wall_everywhere`] elsewhere.
    #[derive(Clone, Copy)]
    pub struct OwnProcess(());

    impl OwnProcess {
        #[doc(hidden)]
        #[must_use]
        pub const fn of_a_test_in_its_own_process() -> Self {
            Self(())
        }
    }

    /// A process-wide wall-clock pin, back to the real clock when it drops.
    pub struct WallHold {
        _process: OwnProcess,
        pinned: std::sync::Arc<super::AtomicU64>,
        replaced: Option<std::sync::Arc<super::AtomicU64>>,
    }

    impl Drop for WallHold {
        fn drop(&mut self) {
            let mut process = super::PROCESS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if process
                .as_ref()
                .is_some_and(|pin| std::sync::Arc::ptr_eq(pin, &self.pinned))
            {
                *process = self.replaced.take();
            }
        }
    }

    /// Pin the wall clock of `process` to `at` until the test ends; every
    /// thread reads `at`.
    pub fn hold_wall_everywhere(process: OwnProcess, at: SystemTime) -> WallHold {
        let pinned = std::sync::Arc::new(super::AtomicU64::new(super::wall_ms(at)));
        // A second pin takes over from the first.
        let replaced = super::PROCESS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(pinned.clone());
        WallHold {
            _process: process,
            pinned,
            replaced,
        }
    }

    thread_local! {
        static FOLLOWED: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicU64>>> =
            const { std::cell::RefCell::new(None) };
    }

    pub(super) fn followed_ms() -> u64 {
        FOLLOWED.with(|followed| {
            followed
                .borrow()
                .as_ref()
                .map_or(0, |wall| wall.load(std::sync::atomic::Ordering::Relaxed))
        })
    }

    /// A wall clock a test pins for the threads it owns; threads that follow
    /// none keep their own.
    #[derive(Clone, Debug, Default)]
    pub struct SharedWall(std::sync::Arc<std::sync::atomic::AtomicU64>);

    impl SharedWall {
        /// This thread reads the pinned time from now on, until
        /// [`Self::release`].
        pub fn follow(&self) {
            FOLLOWED.with(|followed| *followed.borrow_mut() = Some(self.0.clone()));
        }

        pub fn pin(&self, at: SystemTime) {
            self.0
                .store(super::wall_ms(at), std::sync::atomic::Ordering::Relaxed);
        }

        pub fn release(&self) {
            self.0.store(0, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Pin the wall clock of this thread alone to `at`: the other tests of the
    /// process and a runtime's workers keep their own clock.
    pub fn with_wall<T>(at: SystemTime, operation: impl FnOnce() -> T) -> T {
        let ms = super::wall_ms(at);
        let _restore = RestoreWall(super::FROZEN_HERE_MS.with(|here| here.replace(Some(ms))));
        operation()
    }
}

#[cfg(test)]
mod scoped_monotonic_tests {
    use super::*;

    #[test]
    fn a_shared_wall_pins_the_threads_following_it_until_released() {
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

    #[test]
    fn scoped_wall_time_is_thread_local_nested_and_restored_after_unwind() {
        let at = UNIX_EPOCH + Duration::from_secs(4_000_000_000);
        testing::with_wall(at, || {
            assert_eq!(wall(), at);
            let later = at + Duration::from_secs(3);
            testing::with_wall(later, || assert_eq!(wall(), later));
            assert_eq!(wall(), at);
            assert!(std::thread::spawn(wall).join().unwrap() < at);
            let panicked = std::panic::catch_unwind(|| {
                testing::with_wall(later, || panic!("unwind the clock scope"));
            });
            assert!(panicked.is_err());
            assert_eq!(wall(), at);
        });
        assert!(wall() < at);
    }

    #[test]
    fn a_clock_lent_at_the_epoch_is_a_clock_the_code_reads() {
        // `0` is the epoch, not the absence of a hold: a bare wasm32 has no
        // `std` clock, so falling back to the machine's would trap a module lent
        // this clock.
        let (ms, held, frozen) = with_wall_ms(0, || (unix_now_ms(), wall(), wall_frozen()));
        assert_eq!(ms, 0, "the lent time is the time the code reads");
        assert_eq!(held, UNIX_EPOCH);
        assert!(frozen, "a hold at the epoch is a hold");
        assert!(
            unix_now_ms() > 0,
            "the thread's own clock runs again after the hold"
        );
    }

    #[test]
    fn two_threads_holding_different_wall_times_at_once_each_read_their_own() {
        let first = UNIX_EPOCH + Duration::from_secs(4_000_000_000);
        let second = UNIX_EPOCH + Duration::from_secs(4_100_000_000);
        let together = std::sync::Arc::new(std::sync::Barrier::new(2));
        let hold = |at: SystemTime, other: SystemTime| {
            let together = std::sync::Arc::clone(&together);
            std::thread::spawn(move || {
                testing::with_wall(at, || {
                    together.wait();
                    assert_eq!(wall(), at);
                    assert_ne!(wall(), other);
                    together.wait();
                });
                together.wait();
                assert!(wall() < first, "the hold ended with its scope");
            })
        };
        let threads = [hold(first, second), hold(second, first)];
        for thread in threads {
            thread.join().unwrap();
        }
    }
}

thread_local! {
    /// The wall time this thread holds; `None` while it reads its own clock.
    static FROZEN_HERE_MS: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

fn frozen_ms() -> Option<u64> {
    match FROZEN_HERE_MS.with(std::cell::Cell::get) {
        Some(here) => Some(here),
        None => match followed_ms() {
            0 => match pinned_ms() {
                0 => None,
                pinned => Some(pinned),
            },
            followed => Some(followed),
        },
    }
}

/// Hold this thread's wall clock at `now_ms`; nested holds restore the one
/// before them. This is how a host lends a clock to code that has none.
pub fn with_wall_ms<T>(now_ms: u64, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<u64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FROZEN_HERE_MS.with(|here| here.set(self.0));
        }
    }
    let _restore = Restore(FROZEN_HERE_MS.with(|here| here.replace(Some(now_ms))));
    f()
}

static PROCESS: std::sync::Mutex<Option<std::sync::Arc<AtomicU64>>> = std::sync::Mutex::new(None);

fn pinned_ms() -> u64 {
    PROCESS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .map_or(0, |ms| ms.load(Ordering::Relaxed))
}

#[cfg(any(test, feature = "test-support"))]
fn followed_ms() -> u64 {
    testing::followed_ms()
}

#[cfg(not(any(test, feature = "test-support")))]
const fn followed_ms() -> u64 {
    0
}

#[must_use]
#[inline]
pub fn wall() -> SystemTime {
    match frozen_ms() {
        Some(ms) => UNIX_EPOCH
            .checked_add(Duration::from_millis(ms))
            .unwrap_or(UNIX_EPOCH),
        None => SystemTime::now(),
    }
}

/// Pin the wall clock of the whole process to `at` for a screenshot capture,
/// so every frame shows the same date. The monotonic clock keeps running.
#[cfg(not(test))]
pub fn pin_wall(at: SystemTime) {
    let pinned = std::sync::Arc::new(AtomicU64::new(wall_ms(at)));
    *PROCESS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pinned);
}

/// `at` as milliseconds since the epoch, never `0` (which means the clock runs).
fn wall_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(1, |since| saturating_u64(since.as_millis()).max(1))
}

/// Whether a clock is held here, so a wall read answers the hold.
#[must_use]
pub fn wall_frozen() -> bool {
    frozen_ms().is_some()
}

/// Whether the device's clock is synchronised, when the platform can say;
/// `None` when nothing here knows.
#[must_use]
pub fn is_synchronised() -> Option<bool> {
    #[cfg(all(target_os = "linux", not(target_family = "wasm")))]
    {
        let output = std::process::Command::new("timedatectl")
            .args(["show", "-p", "NTPSynchronized", "--value"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        match String::from_utf8_lossy(&output.stdout).trim() {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        }
    }
    #[cfg(not(all(target_os = "linux", not(target_family = "wasm"))))]
    {
        None
    }
}

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

#[must_use]
#[inline]
pub fn to_std(time: SystemTime) -> std::time::SystemTime {
    imp::to_std(time)
}

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

pub fn try_unix_now() -> Option<u64> {
    since_epoch().map(|duration| duration.as_secs())
}

pub fn try_unix_now_ms() -> Option<u64> {
    since_epoch().map(|duration| saturating_u64(duration.as_millis()))
}

pub fn try_unix_now_ns() -> Option<u64> {
    since_epoch().map(|duration| saturating_u64(duration.as_nanos()))
}

pub fn unix_now() -> u64 {
    try_unix_now().unwrap_or(0)
}

pub fn unix_now_ms() -> u64 {
    try_unix_now_ms().unwrap_or(0)
}

pub fn unix_now_ns() -> u64 {
    try_unix_now_ns().unwrap_or(0)
}

pub fn unix_now_i64() -> i64 {
    i64::try_from(unix_now()).unwrap_or(i64::MAX)
}

pub fn unix_now_ms_i64() -> i64 {
    i64::try_from(unix_now_ms()).unwrap_or(i64::MAX)
}

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        unix_now_ms()
    }
}

/// Milliseconds since this process first read a monotonic clock; never moves
/// backwards.
#[derive(Clone, Copy, Debug, Default)]
pub struct MonotonicClock;

impl Clock for MonotonicClock {
    fn now_ms(&self) -> u64 {
        static START: OnceLock<Instant> = OnceLock::new();
        saturating_u64(START.get_or_init(now).elapsed().as_millis())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FixedClock(pub u64);

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0
    }
}

#[derive(Debug, Default)]
pub struct ManualClock(AtomicU64);

impl ManualClock {
    pub const fn new(now_ms: u64) -> Self {
        Self(AtomicU64::new(now_ms))
    }

    pub fn set(&self, now_ms: u64) {
        self.0.store(now_ms, Ordering::SeqCst);
    }

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
    fn a_threads_own_wall_clock_shadows_the_process_pin_only_on_that_thread() {
        const OTHER: u64 = 4_000_000_000;
        let at = UNIX_EPOCH + Duration::from_secs(OTHER);
        tilo_test_env::in_own_process(
            module_path!(),
            "a_threads_own_wall_clock_shadows_the_process_pin_only_on_that_thread",
            || {
                let process = testing::OwnProcess::of_a_test_in_its_own_process();
                let _held = testing::hold_wall_everywhere(process, at);
                assert_eq!(unix_now(), OTHER);
                assert!(wall_frozen());
                testing::with_wall(at + Duration::from_secs(1), || {
                    assert_eq!(wall(), at + Duration::from_secs(1), "the thread's own");
                    assert_eq!(
                        std::thread::spawn(wall).join().unwrap(),
                        at,
                        "another thread keeps the process's"
                    );
                });
                assert_eq!(wall(), at, "the process's, after the thread's scope");
            },
        );
    }

    #[test]
    fn a_frozen_wall_clock_holds_while_the_monotonic_clock_runs() {
        let at = UNIX_EPOCH + Duration::from_secs(1_782_816_120);
        let before = now();
        testing::with_wall(at, || {
            assert!(wall_frozen());
            assert_eq!(wall(), at);
            assert_eq!(unix_now(), 1_782_816_120);
            assert_eq!(wall(), at, "it stays put");
            assert!(now() >= before, "the monotonic clock is not frozen");
        });
        assert!(!wall_frozen(), "the scope ended");
    }

    #[test]
    fn clocks_agree_on_units_move_only_as_told_and_never_backwards() {
        assert!(!wall_frozen(), "no other test's pinned time is read here");
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
