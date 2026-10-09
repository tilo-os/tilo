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

struct LoopWaker(Wake);

impl std::task::Wake for LoopWaker {
    fn wake(self: Arc<Self>) {
        (self.0)();
    }

    fn wake_by_ref(self: &Arc<Self>) {
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
}
