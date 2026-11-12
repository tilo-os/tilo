//! Work the session's loop starts whose answer comes later: a signature the device's signer makes,
//! a commit into a workspace's record. The loop never waits for it, so a signer that answers
//! through the loop (or through a page) cannot stall it: the work runs as a task of its own
//! ([`off_loop`]), and whoever asked hears its answer when it lands. An asker off the loop (an
//! app's broker, an agent's step) takes the [`Pending`] answer itself; what the loop does with an
//! answer runs on the pass after it landed ([`Landings::then`]).

use std::cell::RefCell;
use std::future::Future;

use tilo_actions::remote::{Pending, PendingPoll, pending};

/// Run `work` as a task of its own, never on the loop that asks: its answer, once it lands. A
/// session with no task to run it on abandons the answer, which its asker hears.
pub(crate) fn off_loop<T: Send + 'static>(
    work: impl Future<Output = T> + Send + 'static,
) -> Pending<T> {
    let (done, answer) = pending();
    match tilo_rt::try_app() {
        Ok(runtime) => {
            runtime.spawn(async move { done.complete(work.await) });
        }
        Err(error) => tracing::warn!(%error, "no task runs the work: its asker hears no answer"),
    }
    answer
}

/// What the loop does with answers still to land, each on the first pass after its answer did.
pub(crate) struct Landings {
    waiting: RefCell<Vec<Box<dyn FnMut() -> bool>>>,
    wake: tilo_service::Wake,
}

impl Landings {
    /// Landings that wake the loop with `wake` as an answer lands.
    pub(crate) fn new(wake: tilo_service::Wake) -> Self {
        Self {
            waiting: RefCell::default(),
            wake,
        }
    }

    /// Run `then` on the loop with `answer` once it landed, `None` when its producer gave it up.
    pub(crate) fn then<T: Send + 'static>(
        &self,
        answer: Pending<T>,
        then: impl FnOnce(Option<T>) + 'static,
    ) {
        let (landed, mut here) = pending();
        let wake = self.wake.clone();
        answer.then(move |answer| {
            if let Some(answer) = answer {
                landed.complete(answer);
            } else {
                drop(landed);
            }
            wake();
        });
        let mut then = Some(then);
        self.waiting.borrow_mut().push(Box::new(move || {
            let answer = match here.poll_ready() {
                PendingPoll::Waiting => return false,
                PendingPoll::Ready(answer) => Some(answer),
                PendingPoll::Abandoned => None,
            };
            if let Some(then) = then.take() {
                then(answer);
            }
            true
        }));
    }

    /// Run what waits on an answer that landed since the last pass: called once per pass.
    pub(crate) fn land(&self) {
        let mut waiting = std::mem::take(&mut *self.waiting.borrow_mut());
        // What runs here may wait on another answer, which joins the list it is taken from.
        waiting.retain_mut(|landed| !landed());
        let mut held = self.waiting.borrow_mut();
        waiting.append(&mut held);
        *held = waiting;
    }
}
