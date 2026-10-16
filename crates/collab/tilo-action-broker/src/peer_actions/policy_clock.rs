//! Process-local owner time custody, never admission or readiness authority.
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;
use tilo_lock::MutexRecover as _;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolicyClockError;
impl std::fmt::Display for PolicyClockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("owner policy time unavailable")
    }
}
impl std::error::Error for PolicyClockError {}

struct State {
    observed: Instant,
    unix_ns: u64,
    failed: bool,
}
impl State {
    fn advance(&mut self, monotonic: Instant, wall_ns: u64) -> Result<u64, PolicyClockError> {
        if self.failed {
            return Err(PolicyClockError);
        }
        // Any arithmetic/monotonic failure permanently closes this clock.
        self.failed = true;
        let elapsed = u64::try_from(
            monotonic
                .checked_duration_since(self.observed)
                .ok_or(PolicyClockError)?
                .as_nanos(),
        )
        .map_err(|_| PolicyClockError)?;
        let now = self
            .unix_ns
            .checked_add(elapsed)
            .ok_or(PolicyClockError)?
            .max(wall_ns);
        self.unix_ns = now;
        self.observed = monotonic;
        self.failed = false;
        Ok(now)
    }
}
#[derive(Clone)]
pub struct OwnerPolicyClock(Arc<Clock>);
struct Clock {
    state: Mutex<State>,
    #[cfg(any(test, feature = "test-support"))]
    test_sample: Option<Arc<Mutex<TestSample>>>,
}
#[cfg(any(test, feature = "test-support"))]
struct TestSample {
    monotonic: Instant,
    wall_ns: u64,
}
/// Deterministic clock inputs for tests only. Never enabled by a wire request.
#[cfg(any(test, feature = "test-support"))]
pub struct PolicyClockTestControl(Arc<Mutex<TestSample>>);
#[cfg(any(test, feature = "test-support"))]
impl PolicyClockTestControl {
    pub fn set_wall_ns(&self, wall_ns: u64) {
        self.sample().wall_ns = wall_ns;
    }
    /// Advances the controlled monotonic reading by `elapsed`.
    ///
    /// # Panics
    ///
    /// Panics when the controlled monotonic reading would overflow.
    #[expect(
        clippy::expect_used,
        reason = "test control: an overflowing test clock is a broken test, not a runtime state"
    )]
    pub fn advance_monotonic(&self, elapsed: std::time::Duration) {
        let mut sample = self.sample();
        sample.monotonic = sample
            .monotonic
            .checked_add(elapsed)
            .expect("test monotonic overflow");
    }
    /// The controlled sample, still usable after a panicking test poisoned it.
    fn sample(&self) -> std::sync::MutexGuard<'_, TestSample> {
        self.0.lock_recover()
    }
}
impl OwnerPolicyClock {
    /// A clock seeded from the current wall time.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyClockError`] when the wall clock is before the Unix epoch.
    pub fn new() -> Result<Self, PolicyClockError> {
        Ok(Self(Arc::new(Clock {
            state: Mutex::new(State {
                observed: Instant::now(),
                unix_ns: tilo_clock::try_unix_now_ns().ok_or(PolicyClockError)?,
                failed: false,
            }),
            #[cfg(any(test, feature = "test-support"))]
            test_sample: None,
        })))
    }
    /// Current policy time in Unix nanoseconds, never running backwards.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyClockError`] once the clock failed or its state is unavailable.
    pub fn now_ns(&self) -> Result<u64, PolicyClockError> {
        let mut state = self.0.state.lock().map_err(|_| PolicyClockError)?;
        #[cfg(any(test, feature = "test-support"))]
        if let Some(sample) = &self.0.test_sample {
            let sample = sample.lock().map_err(|_| PolicyClockError)?;
            return state.advance(sample.monotonic, sample.wall_ns);
        }
        let wall = match tilo_clock::try_unix_now_ns().ok_or(PolicyClockError) {
            Ok(wall) => wall,
            Err(error) => {
                state.failed = true;
                return Err(error);
            }
        };
        state.advance(Instant::now(), wall)
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(initial_ns: u64) -> (Self, PolicyClockTestControl) {
        let observed = Instant::now();
        let sample = Arc::new(Mutex::new(TestSample {
            monotonic: observed,
            wall_ns: initial_ns,
        }));
        (
            Self(Arc::new(Clock {
                state: Mutex::new(State {
                    observed,
                    unix_ns: initial_ns,
                    failed: false,
                }),
                test_sample: Some(sample.clone()),
            })),
            PolicyClockTestControl(sample),
        )
    }
    pub fn same_clock(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn new_origin(&self) -> PolicyOrigin {
        PolicyOrigin(Arc::new(Origin {
            clock: self.clone(),
            invalid: AtomicBool::new(false),
        }))
    }
}
struct Origin {
    clock: OwnerPolicyClock,
    invalid: AtomicBool,
}
/// The actual source retains this token. Dropping it retires all captured times.
pub struct PolicyOrigin(Arc<Origin>);
impl Drop for PolicyOrigin {
    fn drop(&mut self) {
        self.invalidate();
    }
}
impl PolicyOrigin {
    pub fn invalidate(&self) {
        self.0.invalid.store(true, Ordering::Release);
    }
    /// Captures a retained time of this origin.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyClockError`] when the origin is retired or the clock failed.
    pub fn capture(&self) -> Result<PolicyTime, PolicyClockError> {
        let value = PolicyTime {
            clock: self.0.clock.clone(),
            origin: Arc::downgrade(&self.0),
        };
        value.now_ns()?;
        Ok(value)
    }
    pub fn owns(&self, time: &PolicyTime) -> bool {
        Weak::ptr_eq(&time.origin, &Arc::downgrade(&self.0)) && time.belongs_to(&self.0.clock)
    }
}
/// Opaque retained time, not proof of current policy or permission to execute.
#[derive(Clone)]
pub struct PolicyTime {
    clock: OwnerPolicyClock,
    origin: Weak<Origin>,
}
impl PolicyTime {
    pub fn belongs_to(&self, clock: &OwnerPolicyClock) -> bool {
        self.clock.same_clock(clock)
    }
    pub fn same_origin(&self, other: &Self) -> bool {
        self.clock.same_clock(&other.clock) && Weak::ptr_eq(&self.origin, &other.origin)
    }
    /// Current time of the retained clock while its origin is live.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyClockError`] when the origin is retired or the clock failed.
    pub fn now_ns(&self) -> Result<u64, PolicyClockError> {
        let origin = self.origin.upgrade().ok_or(PolicyClockError)?;
        if origin.invalid.load(Ordering::Acquire) {
            return Err(PolicyClockError);
        }
        let now = self.clock.now_ns()?;
        if origin.invalid.load(Ordering::Acquire) {
            return Err(PolicyClockError);
        }
        Ok(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn owner_policy_clock_retained_sampling_uses_same_controlled_clock() {
        let (clock, control) = OwnerPolicyClock::for_test(200);
        let origin = clock.new_origin();
        let time = origin.capture().unwrap();
        assert_eq!(time.now_ns(), Ok(200));
        control.set_wall_ns(50);
        control.advance_monotonic(Duration::from_nanos(5));
        assert_eq!(time.now_ns(), Ok(205));
        control.set_wall_ns(300);
        assert_eq!(clock.now_ns(), Ok(300));
        control.set_wall_ns(1);
        control.advance_monotonic(Duration::from_nanos(2));
        assert_eq!(time.now_ns(), Ok(302));
    }
    #[test]
    fn owner_policy_clock_rollback_and_forward_preserve_elapsed() {
        let start = Instant::now();
        let mut state = State {
            observed: start,
            unix_ns: 200,
            failed: false,
        };
        assert_eq!(state.advance(start + Duration::from_nanos(5), 50), Ok(205));
        assert_eq!(state.advance(start + Duration::from_nanos(9), 300), Ok(300));
        assert_eq!(state.advance(start + Duration::from_nanos(12), 10), Ok(303));
        assert_eq!(
            state.advance(start + Duration::from_nanos(12), 301),
            Ok(303)
        );
    }
    #[test]
    fn owner_policy_clock_arithmetic_failure_is_terminal() {
        let start = Instant::now();
        let mut state = State {
            observed: start,
            unix_ns: u64::MAX,
            failed: false,
        };
        assert!(state.advance(start + Duration::from_nanos(1), 0).is_err());
        assert!(state.advance(start, 0).is_err());
        let mut state = State {
            observed: start,
            unix_ns: 1,
            failed: false,
        };
        assert!(
            state
                .advance(start.checked_sub(Duration::from_nanos(1)).unwrap(), 0)
                .is_err()
        );
        assert!(state.advance(start + Duration::from_nanos(1), 2).is_err());
    }
    #[test]
    fn owner_policy_clock_origin_drop_invalidation_and_identity() {
        let clock = OwnerPolicyClock::new().unwrap();
        let origin = clock.new_origin();
        let time = origin.capture().unwrap();
        let clone = time.clone();
        assert!(clock.same_clock(&clock.clone()));
        assert!(time.belongs_to(&clock));
        assert!(origin.owns(&clone));
        assert!(time.same_origin(&clone));
        assert!(!clock.same_clock(&OwnerPolicyClock::new().unwrap()));
        let other = clock.new_origin();
        assert!(!other.owns(&time));
        assert!(!other.capture().unwrap().same_origin(&time));
        let before = time.now_ns().unwrap();
        assert!(clone.now_ns().unwrap() >= before);
        origin.invalidate();
        assert!(time.now_ns().is_err());
        assert!(origin.capture().is_err());
        assert!(other.capture().is_ok());
        let live = other.capture().unwrap();
        drop(other);
        assert!(live.now_ns().is_err());
    }
    #[test]
    fn owner_policy_clock_origin_drop_wins_over_sampling_in_progress() {
        let clock = OwnerPolicyClock::new().unwrap();
        let origin = clock.new_origin();
        let time = origin.capture().unwrap();
        let held = clock.0.state.lock().unwrap();
        let reader = std::thread::spawn(move || time.now_ns());
        let deadline = tilo_test_env::hang_deadline();
        while Arc::strong_count(&origin.0) != 2 {
            assert!(Instant::now() < deadline, "sampler did not retain origin");
            std::thread::yield_now();
        }
        drop(origin);
        drop(held);
        assert!(
            reader.join().unwrap().is_err(),
            "source retirement must win before sample returns"
        );
    }
    #[test]
    fn owner_policy_clock_poison_never_falls_back_to_wall() {
        let clock = OwnerPolicyClock::new().unwrap();
        let origin = clock.new_origin();
        let time = origin.capture().unwrap();
        let held = clock.clone();
        assert!(
            std::panic::catch_unwind(move || {
                let _guard = held.0.state.lock().unwrap();
                panic!("test poison");
            })
            .is_err()
        );
        assert!(clock.now_ns().is_err());
        assert!(time.now_ns().is_err());
        assert!(origin.capture().is_err());
    }
}
