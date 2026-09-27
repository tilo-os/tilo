//! A value an [`ActionHost`](super::ActionHost) produces later.
//!
//! Some answers need the owner of an action — an application that must
//! re-resolve a live directory row — and must not block the caller while it
//! does. [`Pending`] is that answer's receiving end, usable three ways
//! without tying the vocabulary to an async runtime: poll it from a frame
//! loop ([`Pending::poll_ready`]), await it (it is a [`Future`]), or attach a
//! continuation ([`Pending::then`]). Answers available immediately are
//! [`Pending::ready`].

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

type Continuation<T> = Box<dyn FnOnce(Option<T>) + Send>;
/// Whether whoever a continuation hands the answer to still wants it.
type Interest = Box<dyn Fn() -> bool + Send>;

enum State<T> {
    Waiting {
        waker: Option<Waker>,
        then: Option<Continuation<T>>,
        /// Set with a continuation that only forwards the answer, so that
        /// abandoning the far end abandons this one too.
        interest: Option<Interest>,
    },
    /// Completed (`Some`) or abandoned by its producer (`None`).
    Done(Option<T>),
    Taken,
}

struct Slot<T> {
    state: Mutex<State<T>>,
    settled: Condvar,
}

impl<T> Slot<T> {
    fn settle(&self, value: Option<T>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match std::mem::replace(&mut *state, State::Taken) {
            State::Waiting {
                then: Some(then), ..
            } => {
                drop(state);
                then(value);
            }
            State::Waiting {
                waker, then: None, ..
            } => {
                *state = State::Done(value);
                drop(state);
                self.settled.notify_all();
                if let Some(waker) = waker {
                    waker.wake();
                }
            }
            already => *state = already,
        }
    }

    /// Whether nobody can observe this answer any more. `own` is how many
    /// strong references the asking side itself holds.
    fn abandoned(slot: &Arc<Self>, own: usize) -> bool {
        if Arc::strong_count(slot) > own {
            return false;
        }
        let state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &*state {
            State::Waiting {
                then: Some(_),
                interest,
                ..
            } => interest.as_ref().is_some_and(|wanted| !wanted()),
            State::Waiting { then: None, .. } | State::Done(_) | State::Taken => true,
        }
    }
}

/// The producing end of a [`Pending`]. Dropping it without completing
/// abandons the answer, which the receiving end observes.
pub struct Completer<T> {
    slot: Option<Arc<Slot<T>>>,
}

/// The receiving end: an answer that may not exist yet.
pub struct Pending<T> {
    slot: Arc<Slot<T>>,
}

/// A connected [`Completer`] and [`Pending`].
#[must_use]
pub fn pending<T>() -> (Completer<T>, Pending<T>) {
    let slot = Arc::new(Slot {
        state: Mutex::new(State::Waiting {
            waker: None,
            then: None,
            interest: None,
        }),
        settled: Condvar::new(),
    });
    (
        Completer {
            slot: Some(Arc::clone(&slot)),
        },
        Pending { slot },
    )
}

impl<T> Completer<T> {
    /// Whether nobody is waiting any more: the [`Pending`] was dropped —
    /// or, through [`Pending::map`], the mapped answer was — so the work
    /// behind this answer may be skipped.
    #[must_use]
    pub fn is_abandoned(&self) -> bool {
        self.slot
            .as_ref()
            .is_none_or(|slot| Slot::abandoned(slot, 1))
    }

    /// Deliver the answer. A continuation attached with
    /// [`Pending::then`] runs here, on the completing thread.
    pub fn complete(mut self, value: T) {
        if let Some(slot) = self.slot.take() {
            slot.settle(Some(value));
        }
    }
}

impl<T> Drop for Completer<T> {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.take() {
            slot.settle(None);
        }
    }
}

/// What [`Pending::poll_ready`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum PendingPoll<T> {
    Ready(T),
    Waiting,
    /// The producer went away without answering.
    Abandoned,
}

impl<T> Pending<T> {
    /// An answer that is already known.
    #[must_use]
    pub fn ready(value: T) -> Self {
        Self {
            slot: Arc::new(Slot {
                state: Mutex::new(State::Done(Some(value))),
                settled: Condvar::new(),
            }),
        }
    }

    /// Take the answer if it has arrived, without waiting.
    pub fn poll_ready(&mut self) -> PendingPoll<T> {
        let mut state = self
            .slot
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match std::mem::replace(&mut *state, State::Taken) {
            State::Done(Some(value)) => PendingPoll::Ready(value),
            State::Done(None) | State::Taken => PendingPoll::Abandoned,
            waiting @ State::Waiting { .. } => {
                *state = waiting;
                PendingPoll::Waiting
            }
        }
    }

    /// Block the calling thread until the answer arrives or `timeout`
    /// passes. For threads that do not drive the answer's producer.
    ///
    /// # Errors
    ///
    /// Returns `Err(self)` on timeout, so the caller may keep waiting, and
    /// `Ok(None)` if the producer abandoned the answer.
    pub fn wait_timeout(mut self, timeout: Duration) -> Result<Option<T>, Self> {
        let deadline = Instant::now().checked_add(timeout);
        let mut state = self
            .slot
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            if !matches!(*state, State::Waiting { .. }) {
                drop(state);
                return Ok(match self.poll_ready() {
                    PendingPoll::Ready(value) => Some(value),
                    PendingPoll::Waiting | PendingPoll::Abandoned => None,
                });
            }
            let remaining = deadline.map_or(timeout, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
            if remaining.is_zero() {
                drop(state);
                return Err(self);
            }
            state = self
                .slot
                .settled
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Run `then` with the answer once it exists — immediately if it
    /// already does. `None` means the producer abandoned it.
    pub fn then(self, then: impl FnOnce(Option<T>) + Send + 'static) {
        self.then_while(then, None);
    }

    fn then_while(self, then: impl FnOnce(Option<T>) + Send + 'static, interest: Option<Interest>) {
        let mut state = self
            .slot
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match std::mem::replace(&mut *state, State::Taken) {
            State::Waiting { waker, .. } => {
                *state = State::Waiting {
                    waker,
                    then: Some(Box::new(then)),
                    interest,
                };
            }
            State::Done(value) => {
                drop(state);
                then(value);
            }
            State::Taken => {
                drop(state);
                then(None);
            }
        }
    }
}

impl<T: Send + 'static> Pending<T> {
    /// The answer, transformed once it arrives.
    #[must_use]
    pub fn map<U: Send + 'static>(self, map: impl FnOnce(T) -> U + Send + 'static) -> Pending<U> {
        let (completer, mapped) = pending();
        let far_end = completer.slot.as_ref().map(Arc::downgrade);
        // Upgrading adds one reference to the completer's own.
        let wanted = move || {
            far_end
                .as_ref()
                .and_then(Weak::upgrade)
                .is_some_and(|slot| !Slot::abandoned(&slot, 2))
        };
        self.then_while(
            move |value| {
                if let Some(value) = value {
                    completer.complete(map(value));
                }
            },
            Some(Box::new(wanted)),
        );
        mapped
    }
}

impl<T> Future for Pending<T> {
    /// `None` if the producer abandoned the answer.
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<T>> {
        let this = self.get_mut();
        let mut state = this
            .slot
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match std::mem::replace(&mut *state, State::Taken) {
            State::Done(value) => Poll::Ready(value),
            State::Taken => Poll::Ready(None),
            State::Waiting { then, interest, .. } => {
                *state = State::Waiting {
                    waker: Some(context.waker().clone()),
                    then,
                    interest,
                };
                Poll::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pending answer is polled, awaited or continued; dropping a mapped answer abandons the
    /// original; waiting across threads sees the answer or times out; a ready answer resolves as a
    /// future without a runtime.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn a_pending_answer_resolves_however_it_is_waited_on() {
        a_pending_answer_is_polled_awaited_or_continued();
        dropping_a_mapped_answer_abandons_the_original();
        waiting_across_threads_sees_the_answer_or_times_out();
        a_ready_answer_resolves_as_a_future_without_a_runtime();
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "test phase: arithmetic on small fixed test values cannot overflow"
    )]
    fn a_pending_answer_is_polled_awaited_or_continued() {
        let (completer, mut answer) = pending::<u32>();
        assert_eq!(answer.poll_ready(), PendingPoll::Waiting);
        completer.complete(7);
        assert_eq!(answer.poll_ready(), PendingPoll::Ready(7));
        assert_eq!(answer.poll_ready(), PendingPoll::Abandoned);

        let (completer, answer) = pending::<u32>();
        let doubled = answer.map(|value| value * 2);
        completer.complete(21);
        assert_eq!(doubled.wait_timeout(Duration::ZERO).ok(), Some(Some(42)));

        let (completer, mut answer) = pending::<u32>();
        drop(completer);
        assert_eq!(answer.poll_ready(), PendingPoll::Abandoned);
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "test phase: arithmetic on small fixed test values cannot overflow"
    )]
    fn dropping_a_mapped_answer_abandons_the_original() {
        let (completer, answer) = pending::<u32>();
        let mapped = answer.map(|value| value + 1);
        assert!(!completer.is_abandoned());
        drop(mapped);
        assert!(completer.is_abandoned());

        let (completer, answer) = pending::<u32>();
        answer.then(|_| {});
        assert!(
            !completer.is_abandoned(),
            "a plain continuation always wants the answer"
        );
    }

    fn waiting_across_threads_sees_the_answer_or_times_out() {
        let (completer, answer) = pending::<&'static str>();
        let answer = answer
            .wait_timeout(Duration::from_millis(10))
            .expect_err("nothing has answered yet");
        let producer = std::thread::spawn(move || completer.complete("done"));
        assert_eq!(
            answer.wait_timeout(tilo_test_env::hang_bound()).ok(),
            Some(Some("done"))
        );
        producer.join().expect("producer thread");
    }

    fn a_ready_answer_resolves_as_a_future_without_a_runtime() {
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut ready = Pending::ready(3_u8);
        assert_eq!(
            Pin::new(&mut ready).poll(&mut context),
            Poll::Ready(Some(3))
        );
        let (completer, mut waiting) = pending::<u8>();
        assert_eq!(Pin::new(&mut waiting).poll(&mut context), Poll::Pending);
        completer.complete(4);
        assert_eq!(
            Pin::new(&mut waiting).poll(&mut context),
            Poll::Ready(Some(4))
        );
    }
}
