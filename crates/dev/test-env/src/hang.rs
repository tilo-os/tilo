//! Waiting in tests without racing the runner.
//!
//! A test that waits for a worker, daemon, socket or event should finish as
//! soon as the thing it waits for happens, and fail only when it never does.
//! A short fixed deadline instead turns a busy CI runner into a failure. Every
//! helper here waits against one workspace-wide [`hang_bound`], polls or
//! blocks until its condition holds, and panics with a message naming what it
//! was waiting for once the bound runs out.
//!
//! Timing the product itself implements or promises (a timeout under test, a
//! latency budget) is not a hang and does not belong here.
//!
//! - [`wait_until`] polls a condition; [`wait_for`] polls until a probe yields
//!   a value.
//! - [`recv`] receives from a [`std::sync::mpsc::Receiver`].
//! - [`within`] awaits a future on any executor.
//! - [`hang_bound`] and [`hang_deadline`] serve waits the helpers do not fit,
//!   such as a socket's read timeout.

#![allow(
    clippy::panic,
    clippy::expect_used,
    reason = "test support: failing the calling test is the point"
)]

use std::collections::VecDeque;
use std::fmt::Display;
use std::future::{Future, IntoFuture};
use std::panic::Location;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, Weak};
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

/// Overrides [`hang_bound`] with a whole number of seconds, for example to
/// wait longer under a debugger or to fail faster while bisecting a hang.
pub const HANG_BOUND_ENV: &str = "TILO_TEST_HANG_BOUND_SECS";

/// The bound when [`HANG_BOUND_ENV`] is unset. Nextest reports a test as
/// slow after a minute and kills it after four, so a hang still fails with
/// the helper's message rather than the runner's.
pub const DEFAULT_HANG_BOUND: Duration = Duration::from_secs(120);

const POLL_MIN: Duration = Duration::from_millis(1);
const POLL_MAX: Duration = Duration::from_millis(20);

/// How long any test waits for something before calling it hung.
///
/// # Panics
///
/// If [`HANG_BOUND_ENV`] is set to anything but a whole number of seconds.
pub fn hang_bound() -> Duration {
    static BOUND: OnceLock<Duration> = OnceLock::new();
    *BOUND.get_or_init(|| match std::env::var(HANG_BOUND_ENV) {
        Ok(secs) => Duration::from_secs(secs.trim().parse().unwrap_or_else(|_| {
            panic!("{HANG_BOUND_ENV}={secs:?} is not a whole number of seconds")
        })),
        Err(_) => DEFAULT_HANG_BOUND,
    })
}

/// The instant [`hang_bound`] from now.
///
/// # Panics
///
/// As [`hang_bound`] does, or if that bound does not fit in an [`Instant`].
pub fn hang_deadline() -> Instant {
    Instant::now()
        .checked_add(hang_bound())
        .expect("the hang bound fits in an Instant")
}

#[track_caller]
fn hung(what: &dyn Display) -> ! {
    panic!(
        "gave up waiting for {what} after {:?}; it looks hung (set {HANG_BOUND_ENV} to wait longer)",
        hang_bound()
    )
}

/// Poll `condition` until it holds.
///
/// # Panics
///
/// Naming `what`, if it still does not hold after [`hang_bound`].
#[track_caller]
pub fn wait_until(what: impl Display, mut condition: impl FnMut() -> bool) {
    wait_for(what, || condition().then_some(()));
}

/// Poll `probe` until it returns a value, and return that value.
///
/// # Panics
///
/// Naming `what`, if it still returns `None` after [`hang_bound`].
#[track_caller]
pub fn wait_for<T>(what: impl Display, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = hang_deadline();
    let mut pause = POLL_MIN;
    loop {
        if let Some(value) = probe() {
            return value;
        }
        let now = Instant::now();
        if now >= deadline {
            hung(&what);
        }
        thread::sleep(pause.min(deadline.saturating_duration_since(now)));
        pause = pause.saturating_mul(2).min(POLL_MAX);
    }
}

/// Receive the next message from `receiver`.
///
/// # Panics
///
/// Naming `what`, if nothing arrives within [`hang_bound`] or every sender
/// is dropped first.
#[track_caller]
pub fn recv<T>(what: impl Display, receiver: &Receiver<T>) -> T {
    match receiver.recv_timeout(hang_bound()) {
        Ok(value) => value,
        Err(RecvTimeoutError::Timeout) => hung(&what),
        Err(RecvTimeoutError::Disconnected) => {
            panic!("every sender was dropped while waiting for {what}")
        }
    }
}

/// Await `future` to completion on whatever executor polls it.
///
/// The bound starts at the first poll and is kept by a wall-clock timer
/// thread, so it holds under a paused or mocked runtime clock too.
///
/// # Panics
///
/// Naming `what`, if `future` is still pending after [`hang_bound`].
#[track_caller]
pub fn within<F: IntoFuture>(what: impl Display, future: F) -> Within<F::IntoFuture> {
    Within {
        future: Box::pin(future.into_future()),
        what: what.to_string(),
        caller: Location::caller(),
        alarm: None,
    }
}

/// The future [`within`] returns.
#[must_use = "futures do nothing unless awaited"]
pub struct Within<F> {
    future: Pin<Box<F>>,
    what: String,
    caller: &'static Location<'static>,
    alarm: Option<Arc<Alarm>>,
}

impl<F: Future> Future for Within<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        if let Poll::Ready(value) = self.future.as_mut().poll(cx) {
            return Poll::Ready(value);
        }
        let alarm = self
            .alarm
            .get_or_insert_with(|| Alarm::set(hang_deadline()));
        assert!(
            !alarm.rung(cx.waker()),
            "gave up waiting for {} (awaited at {}) after {:?}; it looks hung (set {HANG_BOUND_ENV} to wait longer)",
            self.what,
            self.caller,
            hang_bound()
        );
        Poll::Pending
    }
}

/// A wake-up at a wall-clock instant, rung by the one timer thread.
struct Alarm {
    rung: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl Alarm {
    fn set(at: Instant) -> Arc<Self> {
        let alarm = Arc::new(Self {
            rung: AtomicBool::new(false),
            waker: Mutex::new(None),
        });
        let timer = timer();
        timer
            .alarms
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back((at, Arc::downgrade(&alarm)));
        timer.changed.notify_one();
        alarm
    }

    /// Whether the alarm has rung; if not, `waker` is woken when it does.
    fn rung(&self, waker: &Waker) -> bool {
        let mut slot = self.waker.lock().unwrap_or_else(PoisonError::into_inner);
        match slot.as_mut() {
            Some(current) if current.will_wake(waker) => {}
            _ => *slot = Some(waker.clone()),
        }
        drop(slot);
        self.rung.load(Ordering::Acquire)
    }

    fn ring(&self) {
        self.rung.store(true, Ordering::Release);
        let waker = self
            .waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

struct Timer {
    alarms: Mutex<VecDeque<(Instant, Weak<Alarm>)>>,
    changed: Condvar,
}

fn timer() -> &'static Timer {
    static TIMER: OnceLock<&'static Timer> = OnceLock::new();
    TIMER.get_or_init(|| {
        let timer: &'static Timer = Box::leak(Box::new(Timer {
            alarms: Mutex::new(VecDeque::new()),
            changed: Condvar::new(),
        }));
        thread::Builder::new()
            .name("tilo-test-env hang timer".into())
            .spawn(move || timer.run())
            .expect("spawn the hang timer thread");
        timer
    })
}

impl Timer {
    fn run(&self) {
        let mut alarms = self.alarms.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let now = Instant::now();
            // A finished wait drops its alarm; forget it here.
            alarms.retain(|(_, alarm)| alarm.strong_count() > 0);
            let mut next = None::<Instant>;
            alarms.retain(|(at, alarm)| {
                if *at > now {
                    next = Some(next.map_or(*at, |next| next.min(*at)));
                    return true;
                }
                if let Some(alarm) = alarm.upgrade() {
                    alarm.ring();
                }
                false
            });
            alarms = match next {
                Some(at) => {
                    self.changed
                        .wait_timeout(alarms, at.saturating_duration_since(now))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self
                    .changed
                    .wait(alarms)
                    .unwrap_or_else(PoisonError::into_inner),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::pin;
    use std::sync::mpsc;
    use std::task::Wake;

    struct Flag(AtomicBool);

    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::Release);
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        struct Unpark(thread::Thread);
        impl Wake for Unpark {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = Waker::from(Arc::new(Unpark(thread::current())));
        let mut cx = Context::from_waker(&waker);
        let mut future = pin!(future);
        loop {
            if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return value;
            }
            thread::park();
        }
    }

    fn panic_message(result: thread::Result<()>) -> String {
        let payload = result.expect_err("the wait should have panicked");
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_default()
    }

    /// Each helper returns as soon as its condition holds, and fails naming
    /// what it waited for when the other side is gone.
    #[test]
    fn helpers_return_on_the_event_and_name_what_they_waited_for() {
        let mut polls = 0;
        wait_until("the third poll", || {
            polls += 1;
            polls == 3
        });
        assert_eq!(polls, 3);

        let (tx, rx) = mpsc::channel();
        let sender = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            tx.send(7).unwrap();
        });
        assert_eq!(recv("the worker's answer", &rx), 7);
        sender.join().unwrap();
        let message = panic_message(std::panic::catch_unwind(|| {
            recv("the worker's second answer", &rx);
        }));
        assert!(message.contains("the worker's second answer"), "{message}");

        let found = wait_for("a value", {
            let started = Instant::now();
            move || (started.elapsed() > Duration::from_millis(5)).then_some("ready")
        });
        assert_eq!(found, "ready");

        let (tx, rx) = mpsc::channel::<()>();
        let answer = block_on(within("the ready future", async {
            tx.send(()).unwrap();
            42
        }));
        assert_eq!(answer, 42);
        rx.recv().unwrap();
    }

    /// An alarm rings its waker once its instant passes, and an alarm whose
    /// wait finished first is forgotten without ringing.
    #[test]
    fn alarms_ring_at_their_instant_and_dropped_ones_are_forgotten() {
        let flag = Arc::new(Flag(AtomicBool::new(false)));
        let waker = Waker::from(flag.clone());
        let dropped = Alarm::set(Instant::now());
        drop(dropped);
        let alarm = Alarm::set(Instant::now() + Duration::from_millis(10));
        assert!(!alarm.rung(&waker));
        wait_until("the alarm", || flag.0.load(Ordering::Acquire));
        assert!(alarm.rung(&waker));
    }
}
