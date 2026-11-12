//! Work of this device on its workspaces awaited on the session's loop: polled as it is started,
//! when it is done at once (the desktop's own key signs at once), and again whenever its waker wakes
//! the loop, so a signer that answers later, a write of this device still under way or a branch
//! waiting for its records never holds the loop.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

/// What a write came to: the write as the workspace took it, or why it took none.
pub(crate) type Written = Result<tilo_peer_state::owner::AppWrite, String>;

/// Work under way on the loop, answering `T` once it is done.
pub(crate) struct Polled<T> {
    work: Pin<Box<dyn Future<Output = T>>>,
    waker: Waker,
}

/// One write under way ([`crate::engine::Engine::write_record`]).
pub(crate) type Writing = Polled<Written>;

impl<T> Polled<T> {
    /// `work`, whose waker wakes the loop through `wake` when it moves on.
    pub(crate) fn new(wake: tilo_service::Wake, work: impl Future<Output = T> + 'static) -> Self {
        Self {
            work: Box::pin(work),
            waker: Waker::from(Arc::new(Wakes(wake))),
        }
    }

    /// What the work came to, once it is done; `None` while it is under way. Work that answered
    /// is not asked again.
    pub(crate) fn done(&mut self) -> Option<T> {
        match self
            .work
            .as_mut()
            .poll(&mut Context::from_waker(&self.waker))
        {
            Poll::Ready(done) => Some(done),
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
