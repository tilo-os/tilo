//! A poke that sleeps until the next real event.
//!
//! Idle portal emitters and the collaboration drainer used to wake on a
//! timer with nothing to do. That CPU is on a worker thread, so the
//! compositor's frame loop can stay at zero ticks while the process is
//! still over its idle budget.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;

/// Wakes one waiter. A poke that arrives before [`IdleWake::wait`] is kept,
/// so a signal queued just before the emitter parks is not lost.
#[derive(Clone)]
pub(crate) struct IdleWake(Arc<Notify>);

impl Default for IdleWake {
    fn default() -> Self {
        Self(Arc::new(Notify::new()))
    }
}

impl std::fmt::Debug for IdleWake {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("IdleWake")
    }
}

impl IdleWake {
    pub(crate) fn poke(&self) {
        self.0.notify_one();
    }

    pub(crate) async fn wait(&self) {
        self.0.notified().await;
    }
}

/// Same contract as [`IdleWake`] for a thread that is not on a tokio runtime.
/// `timeout` of `None` parks until the next poke.
#[derive(Debug)]
pub(crate) struct DrainPark {
    pending: Mutex<bool>,
    cv: Condvar,
}

impl Default for DrainPark {
    fn default() -> Self {
        Self {
            pending: Mutex::new(false),
            cv: Condvar::new(),
        }
    }
}

impl DrainPark {
    pub(crate) fn poke(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            *pending = true;
            self.cv.notify_one();
        }
    }

    pub(crate) fn wait(&self, timeout: Option<Duration>) {
        let Ok(pending) = self.pending.lock() else {
            return;
        };
        let guard = match timeout {
            Some(timeout) => {
                self.cv
                    .wait_timeout_while(pending, timeout, |pending| !*pending)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            None => self
                .cv
                .wait_while(pending, |pending| !*pending)
                .unwrap_or_else(PoisonError::into_inner),
        };
        let mut pending = guard;
        *pending = false;
    }
}
