//! The times a `moq-lite` driver is handed.
//!
//! A driver's clock is its caller's: every poll hands it the time, and it
//! stops the whole program ("driver time moved backwards") when one is earlier
//! than the one before. `moq_net`'s own loop hands a driver each reading of
//! the machine's clock as it comes, and the first time of a session's clock is
//! read by whoever opens the session, from a clock of its own choosing. In a
//! page every one of those readings is the browser's `performance.now()`, and
//! a web view answered less than it had answered before: the page stopped and
//! started over.
//!
//! So a driver is driven here. A [`Pace`] reads one clock for a driver's first
//! time and for every poll after it, and keeps the largest reading it handed
//! out: a clock that steps back holds the driver's time still until it has
//! caught up, which makes a timer late by the step and costs nothing else.

use std::pin::Pin;
use std::task::Poll;

use moq_net::time::{Driver, Instant};

/// The clock every driver's time is read from. On a device it is Tokio's: the
/// one the sleeps below wait on, and the one a test holds still.
#[cfg(not(target_family = "wasm"))]
fn reading() -> Instant {
    n0_future::time::Instant::now().into_std()
}

/// The clock every driver's time is read from. In a page it is the browser's
/// `performance.now()`.
#[cfg(target_family = "wasm")]
fn reading() -> Instant {
    Instant::now()
}

/// The times one driver is handed: each read from one clock, none earlier
/// than the one before.
pub(crate) struct Pace<C = fn() -> Instant> {
    clock: C,
    latest: Option<Instant>,
}

impl Pace {
    /// A pace on the machine's clock.
    pub(crate) fn new() -> Self {
        Self::on(reading)
    }
}

impl<C: FnMut() -> Instant> Pace<C> {
    /// A pace on `clock`.
    const fn on(clock: C) -> Self {
        Self {
            clock,
            latest: None,
        }
    }

    /// The time now: the clock's reading, or the largest time handed out
    /// before it where the clock answers less.
    pub(crate) fn now(&mut self) -> Instant {
        let read = (self.clock)();
        let now = self.latest.map_or(read, |latest| latest.max(read));
        self.latest = Some(now);
        now
    }

    /// Drive `driver` until it ends, sleeping until each deadline it names.
    /// Answers the error it ended on, [`moq_net::Error::Closed`] for a clean
    /// end.
    pub(crate) async fn drive<D: Driver>(mut self, mut driver: D) -> moq_net::Error {
        // The deadline the driver named last, and the sleep that ends at it.
        let mut wake: Option<(Instant, Pin<Box<n0_future::time::Sleep>>)> = None;
        moq_net::kio::wait(|waiter| {
            loop {
                let now = self.now();
                let at = match driver.poll(now, waiter) {
                    Ok(Some(at)) => at,
                    Ok(None) => {
                        wake = None;
                        return Poll::Pending;
                    }
                    Err(error) => return Poll::Ready(error),
                };
                // A poll that names the deadline it named before keeps its sleep.
                if wake.as_ref().is_some_and(|(due, _)| *due != at) {
                    wake = None;
                }
                let (_, sleep) = wake.get_or_insert_with(|| {
                    let rest = at.saturating_duration_since(now);
                    (at, Box::pin(n0_future::time::sleep(rest)))
                });
                if waiter.poll_future(sleep.as_mut()).is_pending() {
                    return Poll::Pending;
                }
                // The sleep is over: the driver is polled at the time it is
                // now, and where that is still short of the deadline (the
                // clock stepped back meanwhile) it sleeps for the rest.
                wake = None;
            }
        })
        .await
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion: its own lock, and a time a few seconds on"
)]
mod tests {
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::{Pin, pin};
    use std::sync::{Arc, Mutex};
    use std::task::Poll;
    use std::time::Duration;

    use moq_net::time::{Driver, Instant};

    use super::Pace;

    /// A clock that answers `base` plus each of `steps` (milliseconds) in
    /// turn, and the last of them from then on.
    fn clock(base: Instant, steps: &[u64]) -> impl FnMut() -> Instant + use<> {
        let mut steps: VecDeque<Duration> = steps
            .iter()
            .map(|step| Duration::from_millis(*step))
            .collect();
        let mut last = Duration::ZERO;
        move || {
            if let Some(step) = steps.pop_front() {
                last = step;
            }
            base.checked_add(last).unwrap()
        }
    }

    /// Poll `future` once: whether it is still going.
    async fn going(mut future: Pin<&mut impl Future>) -> bool {
        std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await
    }

    /// A driver that keeps every time it is handed and names a deadline
    /// `again` after each, or none.
    struct Kept {
        handed: Arc<Mutex<Vec<Instant>>>,
        again: Option<Duration>,
    }

    impl Driver for Kept {
        fn poll(
            &mut self,
            now: Instant,
            _waiter: &moq_net::kio::Waiter,
        ) -> Result<Option<Instant>, moq_net::Error> {
            self.handed.lock().unwrap().push(now);
            Ok(self.again.and_then(|again| now.checked_add(again)))
        }
    }

    /// The fault as a page met it: the clock answers less than it answered
    /// before, and an origin's own driver, whose clock stops the program on an
    /// earlier time, is polled across the step.
    #[tokio::test(start_paused = true)]
    async fn a_clock_that_steps_back_does_not_stop_an_origin() {
        let (origin, driver) = moq_net::origin::Producer::new(crate::admit::origin_config(
            1024 * 1024,
            Duration::from_secs(2),
        ));
        let pace = Pace::on(clock(super::reading(), &[2_000, 1_000, 3_000]));
        let mut driving = pin!(pace.drive(driver));
        for poll in 1..=3 {
            assert!(
                going(driving.as_mut()).await,
                "the origin's driver ended at poll {poll}"
            );
        }
        drop(origin);
    }

    /// A session's clock is made with a pace's first time and polled with its
    /// later ones: none of them is earlier than the one before, whatever the
    /// clock answers.
    #[tokio::test(start_paused = true)]
    async fn a_driver_is_never_handed_an_earlier_time() {
        let base = super::reading();
        let at = |step| base.checked_add(Duration::from_millis(step)).unwrap();
        let handed = Arc::new(Mutex::new(Vec::new()));
        let mut pace = Pace::on(clock(base, &[5_000, 4_000, 4_500, 6_000]));
        let first = pace.now();
        let mut driving = pin!(pace.drive(Kept {
            handed: handed.clone(),
            again: None,
        }));
        for _ in 0..3 {
            assert!(going(driving.as_mut()).await);
        }
        assert_eq!(first, at(5_000));
        assert_eq!(
            *handed.lock().unwrap(),
            [at(5_000), at(5_000), at(6_000)],
            "a time earlier than the session's first, or than the poll before"
        );
    }

    /// The deadline a driver names is kept: it is polled again when the clock
    /// reaches it, not before, and with a time that has reached it.
    #[tokio::test(start_paused = true)]
    async fn a_driver_is_polled_again_at_its_deadline() {
        let handed = Arc::new(Mutex::new(Vec::new()));
        let driving = tokio::spawn(Pace::new().drive(Kept {
            handed: handed.clone(),
            again: Some(Duration::from_secs(3)),
        }));
        tokio::task::yield_now().await;
        assert_eq!(handed.lock().unwrap().len(), 1);
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            handed.lock().unwrap().len(),
            1,
            "polled before its deadline"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        let handed = handed.lock().unwrap().clone();
        let late = Duration::from_secs(3);
        assert!(
            matches!(handed.as_slice(), [first, second] if second.duration_since(*first) >= late),
            "not polled once more, at its deadline: {handed:?}"
        );
        driving.abort();
    }
}
