//! Process-local owner time ownership, never admission or readiness authority.
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;
#[cfg(any(test, feature = "test-support"))]
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
    /// A reading failed since `observed`: the next good one is taken as the new anchor, with
    /// the time already handed out as its floor.
    reanchor: bool,
}
impl State {
    /// Policy time at `monotonic`, never earlier than a time already handed out: the time
    /// elapsed since the last reading is added to the last answer, and the wall clock
    /// (`None` when it cannot be read) only ever moves it forward.
    ///
    /// A reading behind the last one fails, and so does an answer past `u64` nanoseconds;
    /// neither closes the clock. After a failed reading the next good one answers the last
    /// time handed out (or the wall clock, when later) and measures from there.
    fn advance(
        &mut self,
        monotonic: Instant,
        wall_ns: Option<u64>,
    ) -> Result<u64, PolicyClockError> {
        let floor = if self.reanchor {
            self.unix_ns
        } else {
            let Some(elapsed) = monotonic.checked_duration_since(self.observed) else {
                tracing::warn!(
                    behind_ns = self.observed.duration_since(monotonic).as_nanos(),
                    "the monotonic clock read earlier than its last reading: \
                     owner policy time measures from its next reading"
                );
                self.reanchor = true;
                return Err(PolicyClockError);
            };
            u64::try_from(elapsed.as_nanos())
                .ok()
                .and_then(|elapsed| self.unix_ns.checked_add(elapsed))
                .ok_or(PolicyClockError)?
        };
        let now = wall_ns.map_or(floor, |wall| floor.max(wall));
        self.unix_ns = now;
        self.observed = monotonic;
        self.reanchor = false;
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
    /// Moves the controlled monotonic reading back by `elapsed`: the next reading fails, as a
    /// monotonic clock read behind its last reading does.
    ///
    /// # Panics
    ///
    /// Panics when the controlled monotonic reading would underflow.
    #[expect(
        clippy::expect_used,
        reason = "test control: an underflowing test clock is a broken test, not a runtime state"
    )]
    pub fn rewind_monotonic(&self, elapsed: std::time::Duration) {
        let mut sample = self.sample();
        sample.monotonic = sample
            .monotonic
            .checked_sub(elapsed)
            .expect("test monotonic underflow");
    }
    /// The controlled sample, still usable after a panicking test poisoned it.
    fn sample(&self) -> std::sync::MutexGuard<'_, TestSample> {
        self.0.lock_recover()
    }
}
impl OwnerPolicyClock {
    /// A clock seeded from the current wall time.
    pub fn new() -> Result<Self, PolicyClockError> {
        Ok(Self(Arc::new(Clock {
            state: Mutex::new(State {
                observed: tilo_clock::now(),
                unix_ns: tilo_clock::try_unix_now_ns().ok_or(PolicyClockError)?,
                reanchor: false,
            }),
            #[cfg(any(test, feature = "test-support"))]
            test_sample: None,
        })))
    }
    /// Current policy time in Unix nanoseconds, never running backwards.
    ///
    /// # Errors
    ///
    /// This reading failed: the monotonic clock read behind its last reading, or the time
    /// would pass `u64` nanoseconds. A later reading answers again.
    pub fn now_ns(&self) -> Result<u64, PolicyClockError> {
        let mut state = match self.0.state.lock() {
            Ok(state) => state,
            // A reader panicked while it held the clock. A reading changes the state only in
            // its last, infallible stores, so what it holds is a time already handed out:
            // measure from the next reading, with that time as the floor.
            Err(poisoned) => {
                tracing::warn!("owner policy time recovers from a reader that panicked");
                let mut state = poisoned.into_inner();
                state.reanchor = true;
                self.0.state.clear_poison();
                state
            }
        };
        #[cfg(any(test, feature = "test-support"))]
        if let Some(sample) = &self.0.test_sample {
            let sample = sample.lock_recover();
            return state.advance(sample.monotonic, Some(sample.wall_ns));
        }
        // The wall clock only moves policy time forward: without it, the monotonic clock still
        // answers from the last time handed out.
        let wall = tilo_clock::try_unix_now_ns();
        state.advance(tilo_clock::now(), wall)
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(initial_ns: u64) -> (Self, PolicyClockTestControl) {
        let observed = tilo_clock::now();
        let sample = Arc::new(Mutex::new(TestSample {
            monotonic: observed,
            wall_ns: initial_ns,
        }));
        (
            Self(Arc::new(Clock {
                state: Mutex::new(State {
                    observed,
                    unix_ns: initial_ns,
                    reanchor: false,
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

    tilo_test_env::scenario! {
        fn the_owner_policy_clock_never_runs_backwards_or_outlives_its_origin;

        phase owner_policy_clock_retained_sampling_uses_same_controlled_clock;
        {
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

        phase owner_policy_clock_rollback_and_forward_preserve_elapsed;
        {
            let start = tilo_clock::now();
            let mut state = State {
                observed: start,
                unix_ns: 200,
                reanchor: false,
            };
            let at = |ns| start + Duration::from_nanos(ns);
            assert_eq!(state.advance(at(5), Some(50)), Ok(205));
            assert_eq!(state.advance(at(9), Some(300)), Ok(300));
            assert_eq!(state.advance(at(12), Some(10)), Ok(303));
            assert_eq!(state.advance(at(12), Some(301)), Ok(303));
            assert_eq!(
                state.advance(at(20), None),
                Ok(311),
                "an unreadable wall clock leaves the monotonic clock to answer"
            );
        }

        phase owner_policy_clock_answers_again_after_a_failed_reading;
        {
            let start = tilo_clock::now();
            let at = |ns| start + Duration::from_nanos(ns);
            let mut state = State {
                observed: at(10),
                unix_ns: 500,
                reanchor: false,
            };
            assert!(state.advance(at(4), Some(900)).is_err(), "a reading behind the last fails");
            assert_eq!(
                state.advance(at(6), Some(100)),
                Ok(500),
                "the next good reading answers no earlier than the last time handed out"
            );
            assert_eq!(state.advance(at(9), Some(100)), Ok(503), "and measures from itself");
            assert!(state.advance(at(1), None).is_err());
            assert_eq!(state.advance(at(2), Some(700)), Ok(700), "a later wall clock still wins");
            let mut state = State {
                observed: start,
                unix_ns: u64::MAX,
                reanchor: false,
            };
            assert!(state.advance(at(1), Some(0)).is_err(), "time past u64 fails");
            assert_eq!(state.advance(start, Some(0)), Ok(u64::MAX));
        }

        phase owner_policy_clock_retained_time_recovers_but_its_retired_origin_does_not;
        {
            let (clock, control) = OwnerPolicyClock::for_test(1_000);
            let origin = clock.new_origin();
            let time = origin.capture().unwrap();
            control.advance_monotonic(Duration::from_nanos(50));
            assert_eq!(time.now_ns(), Ok(1_050));
            control.rewind_monotonic(Duration::from_nanos(30));
            control.set_wall_ns(1);
            assert!(time.now_ns().is_err(), "the failed reading refuses");
            assert_eq!(
                time.now_ns(),
                Ok(1_050),
                "the next reading answers again, never earlier than before the failure"
            );
            control.advance_monotonic(Duration::from_nanos(7));
            assert_eq!(time.now_ns(), Ok(1_057));
            let other = clock.new_origin();
            let survivor = other.capture().unwrap();
            origin.invalidate();
            control.advance_monotonic(Duration::from_nanos(1));
            assert!(time.now_ns().is_err(), "a retired origin still refuses");
            assert!(origin.capture().is_err());
            assert_eq!(survivor.now_ns(), Ok(1_058));
        }

        phase owner_policy_clock_origin_drop_invalidation_and_identity;
        {
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

        phase owner_policy_clock_origin_drop_wins_over_sampling_in_progress;
        {
            let clock = OwnerPolicyClock::new().unwrap();
            let origin = clock.new_origin();
            let time = origin.capture().unwrap();
            let held = clock.0.state.lock().unwrap();
            let reader = std::thread::spawn(move || time.now_ns());
            let deadline = tilo_test_env::hang_deadline();
            while Arc::strong_count(&origin.0) != 2 {
                assert!(
                    tilo_clock::now() < deadline,
                    "sampler did not retain origin"
                );
                std::thread::yield_now();
            }
            drop(origin);
            drop(held);
            assert!(
                reader.join().unwrap().is_err(),
                "source retirement must win before sample returns"
            );
        }

        phase owner_policy_clock_recovers_from_a_reader_that_panicked;
        {
            let (clock, control) = OwnerPolicyClock::for_test(400);
            let origin = clock.new_origin();
            let time = origin.capture().unwrap();
            control.advance_monotonic(Duration::from_nanos(20));
            assert_eq!(time.now_ns(), Ok(420));
            let held = clock;
            assert!(
                tilo_rt::panic_boundary::isolate(move || {
                    let _guard = held.0.state.lock().unwrap();
                    panic!("test poison");
                })
                .is_err()
            );
            control.set_wall_ns(5);
            assert_eq!(
                time.now_ns(),
                Ok(420),
                "a poisoned clock answers again from the last time handed out, not the wall"
            );
            control.advance_monotonic(Duration::from_nanos(3));
            assert_eq!(time.now_ns(), Ok(423));
            assert!(origin.capture().is_ok());
        }
    }
}
