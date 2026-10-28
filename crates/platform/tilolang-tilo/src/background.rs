//! Work that reads a whole room off the frame path: search's first index and publication's first
//! trees are read from a [`crate::store::Snapshot`] on a thread of their own, and the frame that
//! follows their end takes the result in.

use std::sync::{Arc, Mutex, mpsc};

/// What a host passes to have a frame scheduled when background work ends: the window's
/// redraw request. Called from the worker thread.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

type Job<T> = Box<dyn FnOnce() -> T + Send>;

/// Work under way on a thread of its own.
pub(crate) struct Pending<T> {
    result: mpsc::Receiver<T>,
}

/// What [`Pending::poll`] found.
pub(crate) enum Poll<T> {
    Ready(T),
    Running,
    /// The work ended without a result: it panicked. The caller does it on the frame instead.
    Lost,
}

impl<T: Send + 'static> Pending<T> {
    /// Run `work` on a thread of its own, then call `wake`. Without a thread to be had (the
    /// process is at its limit) the work runs here, as it is owed either way.
    pub fn start(work: impl FnOnce() -> T + Send + 'static, wake: Option<Waker>) -> Self {
        let (send, result) = mpsc::sync_channel(1);
        let job: Arc<Mutex<Option<Job<T>>>> = Arc::new(Mutex::new(Some(Box::new(work))));
        let taken = Arc::clone(&job);
        let worker_send = send.clone();
        let spawned = std::thread::Builder::new()
            .name("tilolang-read".to_owned())
            .spawn(move || {
                crate::index::read_off_frame();
                let work = taken.lock().ok().and_then(|mut slot| slot.take());
                if let Some(work) = work {
                    // The receiver is gone only when its owner was dropped: nobody wants it.
                    let _ = worker_send.send(work());
                    if let Some(wake) = wake {
                        wake();
                    }
                }
            });
        if spawned.is_err() {
            let work = job.lock().ok().and_then(|mut slot| slot.take());
            if let Some(work) = work {
                let _ = send.send(work());
            }
        }
        Self { result }
    }

    /// The result, if the work has ended.
    pub fn poll(&self) -> Poll<T> {
        match self.result.try_recv() {
            Ok(value) => Poll::Ready(value),
            Err(mpsc::TryRecvError::Empty) => Poll::Running,
            Err(mpsc::TryRecvError::Disconnected) => Poll::Lost,
        }
    }

    /// Wait for the work to end.
    pub fn wait(&self) -> Poll<T> {
        self.result.recv().map_or(Poll::Lost, Poll::Ready)
    }
}
