//! What wakes the compositor loop when an invocation it waits on moves.
//!
//! The loop polls a [`PendingInvocation`](super::PendingInvocation), but the
//! answer is produced elsewhere: an application admitting the action, the
//! broker recording a job's progress. Polled only when something unrelated
//! woke the loop, an idle desktop left a control client waiting without
//! bound. The loop installs its wake here for the thread it runs on, and a
//! ticket made on that thread wakes it when its answer arrives.

use std::cell::RefCell;
use std::future::Future as _;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, Waker};

use tilo_actions::remote::{Pending, PendingPoll};

use crate::session_runtime::Wake;

thread_local! {
    static WAKE: RefCell<Option<Wake>> = const { RefCell::new(None) };
}

/// The loop's wake stays installed on its thread while this lives.
#[must_use = "the wake is uninstalled when this is dropped"]
pub struct Installed(());

impl Drop for Installed {
    fn drop(&mut self) {
        // A thread already tearing down its locals has no loop left to wake.
        let _ = WAKE.try_with(|slot| slot.borrow_mut().take());
    }
}

/// Have every invocation made on this thread wake the loop with `wake` when
/// its answer arrives.
pub fn install(wake: Wake) -> Installed {
    WAKE.with(|slot| *slot.borrow_mut() = Some(wake));
    Installed(())
}

/// How many times a [`waker`] has woken the loop, from any thread.
static PROGRESS: AtomicU64 = AtomicU64::new(0);

/// How many answers have woken the loop so far: admissions, index builds,
/// directory pages, broker changes. A waiter that kept the value it saw
/// knows whether anything arrived since, whoever it was for.
pub fn progress() -> u64 {
    PROGRESS.load(Ordering::Acquire)
}

struct LoopWaker(Wake);

impl std::task::Wake for LoopWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        // Counted before the wake, so the woken loop sees it.
        PROGRESS.fetch_add(1, Ordering::AcqRel);
        (self.0)();
    }
}

/// The wake installed on this thread, as a [`Waker`]: a no-op one where no
/// loop installed one.
pub fn waker() -> Waker {
    WAKE.with(|slot| slot.borrow().clone()).map_or_else(
        || Waker::noop().clone(),
        |wake| Waker::from(Arc::new(LoopWaker(wake))),
    )
}

/// Take `pending`'s answer if it has arrived; otherwise `waker` is woken
/// once it does, or once its producer gives up on it.
pub fn poll<T>(pending: &mut Pending<T>, waker: &Waker) -> PendingPoll<T> {
    let mut context = Context::from_waker(waker);
    match Pin::new(pending).poll(&mut context) {
        Poll::Ready(Some(value)) => PendingPoll::Ready(value),
        Poll::Ready(None) => PendingPoll::Abandoned,
        Poll::Pending => PendingPoll::Waiting,
    }
}

/// `pending`, made on the loop's thread, with this thread's [`waker`]
/// registered from the start: its answer wakes the loop even if it arrives
/// before anyone polls it. An answer already in stays in, for the holder's
/// first poll to take.
pub fn watch<T>(mut pending: Pending<T>) -> Pending<T> {
    match poll(&mut pending, &waker()) {
        PendingPoll::Ready(value) => Pending::ready(value),
        PendingPoll::Waiting | PendingPoll::Abandoned => pending,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn an_answer_wakes_the_installed_loop_once_it_arrives() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let installed = install(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }));
        let (completer, mut answer) = tilo_actions::remote::pending::<u8>();
        let waker = waker();
        assert_eq!(poll(&mut answer, &waker), PendingPoll::Waiting);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "nothing arrived yet");
        completer.complete(7);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the answer itself wakes the loop"
        );
        assert_eq!(poll(&mut answer, &waker), PendingPoll::Ready(7));

        let (abandoned, mut never) = tilo_actions::remote::pending::<u8>();
        assert_eq!(poll(&mut never, &waker), PendingPoll::Waiting);
        drop(abandoned);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a producer giving up wakes the loop to report it"
        );
        assert_eq!(poll(&mut never, &waker), PendingPoll::Abandoned);

        drop(installed);
        let (completer, mut unwatched) = tilo_actions::remote::pending::<u8>();
        assert_eq!(poll(&mut unwatched, &super::waker()), PendingPoll::Waiting);
        completer.complete(1);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "an uninstalled wake is not called"
        );
    }

    #[test]
    fn a_watched_answer_wakes_before_its_first_poll_and_counts_as_progress() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let _installed = install(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }));
        let (completer, answer) = tilo_actions::remote::pending::<u8>();
        let mut watched = watch(answer);
        let before = progress();
        completer.complete(3);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "nobody polled it yet");
        assert!(progress() > before, "a wake is counted as progress");
        assert_eq!(poll(&mut watched, &waker()), PendingPoll::Ready(3));

        let mut early = watch(Pending::ready(5_u8));
        assert_eq!(
            poll(&mut early, &waker()),
            PendingPoll::Ready(5),
            "an answer already in is kept for the first poll"
        );
    }
}
