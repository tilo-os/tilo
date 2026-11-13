//! Work of this device on its workspaces that the session's loop waits on without holding it: a
//! write into a workspace's record (signed, sealed and kept by the workspace's store) or a branch
//! being copied runs as a task of its own ([`crate::landing::off_loop`]), and the loop is woken to
//! take its answer once it is done. A write can take a store hundreds of milliseconds on a busy
//! machine; on the loop it held every frame and every window's first frame for that long.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use tilo_actions::remote::Pending;

/// What a write came to: the write as the workspace took it, or why it took none.
pub(crate) type Written = Result<tilo_peer_state::owner::AppWrite, String>;

/// Work under way off the loop, answering `T` once it is done.
pub(crate) struct Polled<T> {
    answer: Pending<T>,
    /// What the work came to when the task running it went away without answering.
    gone: Option<Box<dyn FnOnce() -> T>>,
    waker: Waker,
}

/// One write under way ([`crate::engine::Engine::write_record`]).
pub(crate) type Writing = Polled<Written>;

impl Writing {
    /// `write`, run off the loop, which `wake` wakes once it is done.
    pub(crate) fn write(
        wake: tilo_service::Wake,
        write: impl Future<Output = Written> + Send + 'static,
    ) -> Self {
        Self::new(wake, write, || {
            Err("the write's task stopped before it answered".to_owned())
        })
    }
}

impl<T: Send + 'static> Polled<T> {
    /// Start `work` off the loop; `wake` wakes the loop once it is done, and `gone` is its answer
    /// should the task running it stop first.
    pub(crate) fn new(
        wake: tilo_service::Wake,
        work: impl Future<Output = T> + Send + 'static,
        gone: impl FnOnce() -> T + 'static,
    ) -> Self {
        Self {
            answer: crate::landing::off_loop(work),
            gone: Some(Box::new(gone)),
            waker: Waker::from(Arc::new(Wakes(wake))),
        }
    }

    /// What the work came to, once it is done; `None` while it is under way. Work that answered
    /// is not asked again.
    pub(crate) fn done(&mut self) -> Option<T> {
        match Pin::new(&mut self.answer).poll(&mut Context::from_waker(&self.waker)) {
            Poll::Ready(Some(done)) => Some(done),
            Poll::Ready(None) => self.gone.take().map(|gone| gone()),
            Poll::Pending => None,
        }
    }
}

/// The loop's wake, as a waker.
struct Wakes(tilo_service::Wake);

impl std::task::Wake for Wakes {
    fn wake(self: Arc<Self>) {
        (self.0)();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier, Mutex, mpsc};
    use std::time::Duration;

    use super::Writing;

    /// A write the store is slow to take does not hold the loop that started it: the loop gets
    /// its turn back at once, is woken when the write is done, and takes the answer then.
    #[test]
    fn a_slow_write_runs_off_the_loop_and_wakes_it_when_done() {
        let (rings, heard) = mpsc::channel();
        let rings = Mutex::new(rings);
        let wake: tilo_service::Wake = Arc::new(move || {
            let _ = rings.lock().map(|rings| rings.send(()));
        });
        // The store answers only once the loop has had its turn back: a write run on the loop
        // would never get past this.
        let turn_back = Arc::new(Barrier::new(2));
        let store = Arc::clone(&turn_back);
        let mut writing = Writing::write(wake, async move {
            store.wait();
            Err("refused".to_owned())
        });
        assert!(writing.done().is_none(), "the write is still under way");
        turn_back.wait();
        heard
            .recv_timeout(Duration::from_secs(10))
            .expect("the loop is woken once the write is done");
        assert!(
            matches!(writing.done(), Some(Err(why)) if why == "refused"),
            "the loop takes the write's answer"
        );
    }
}
