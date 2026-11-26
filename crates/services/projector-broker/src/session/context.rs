//! What a running session shares with the service that supervises it: the
//! stop request, readiness, and the events it reports.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tilo_projector::CaptureProfile;
use tilo_projector::control::{ConnectStage, ProjectorEvent};

use super::Helpers;
use super::paths::SessionPaths;
use tilo_lock::MutexRecover as _;

type Waker = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct State {
    stop: bool,
    /// Why the service stopped the session, when that is itself a failure
    /// (it never became ready in time).
    reason: Option<&'static str>,
    ready: bool,
    finished: bool,
    wakers: Vec<Waker>,
}

#[derive(Default)]
pub(crate) struct SessionControl {
    state: Mutex<State>,
    changed: Condvar,
}

impl SessionControl {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock_recover()
    }

    /// Asks the session to stop, waking every waiter and every registered
    /// waker. A later reason does not replace an earlier one.
    pub(crate) fn request_stop(&self, reason: Option<&'static str>) {
        let mut state = self.lock();
        state.stop = true;
        if state.reason.is_none() {
            state.reason = reason;
        }
        let wakers = std::mem::take(&mut state.wakers);
        drop(state);
        self.changed.notify_all();
        for wake in wakers {
            wake();
        }
    }

    pub(crate) fn stop_reason(&self) -> Option<&'static str> {
        self.lock().reason
    }

    /// Runs `wake` once a stop is requested, or now if one already was. A
    /// session blocked on something other than this condvar (a channel of
    /// sender output) registers how to wake itself.
    pub(crate) fn on_stop(&self, wake: impl FnOnce() + Send + 'static) {
        let mut state = self.lock();
        if state.stop {
            drop(state);
            wake();
        } else {
            state.wakers.push(Box::new(wake));
        }
    }

    /// Sleeps for `timeout` unless a stop is requested first. Returns whether
    /// a stop was requested.
    pub(crate) fn wait_stop(&self, timeout: Duration) -> bool {
        self.wait_until(timeout, |state| state.stop).stop
    }

    pub(crate) fn mark_ready(&self) {
        self.lock().ready = true;
        self.changed.notify_all();
    }

    /// Stops the session for `reason` unless it becomes ready, is stopped or
    /// finishes within `timeout`.
    pub(crate) fn expire_unless_ready(&self, timeout: Duration, reason: &'static str) {
        let flags = self.wait_until(timeout, |state| state.ready || state.stop || state.finished);
        if !(flags.ready || flags.stop || flags.finished) {
            self.request_stop(Some(reason));
        }
    }

    pub(crate) fn mark_finished(&self) {
        self.lock().finished = true;
        self.changed.notify_all();
    }

    /// Waits until the session's thread has finished. Returns whether it did.
    pub(crate) fn wait_finished(&self, timeout: Duration) -> bool {
        self.wait_until(timeout, |state| state.finished).finished
    }

    fn wait_until(&self, timeout: Duration, done: impl Fn(&State) -> bool) -> Flags {
        let deadline = Instant::now().checked_add(timeout);
        let mut state = self.lock();
        while !done(&state) {
            let remaining = deadline.map_or(timeout, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
            if remaining.is_zero() {
                break;
            }
            state = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        Flags {
            stop: state.stop,
            ready: state.ready,
            finished: state.finished,
        }
    }
}

struct Flags {
    stop: bool,
    ready: bool,
    finished: bool,
}

pub(crate) type EventSink = Arc<dyn Fn(ProjectorEvent) + Send + Sync>;

#[derive(Clone)]
pub(crate) struct SessionContext {
    pub(crate) paths: SessionPaths,
    pub(crate) helpers: Helpers,
    pub(crate) device_id: String,
    control: Arc<SessionControl>,
    events: EventSink,
}

impl SessionContext {
    pub(crate) fn new(
        paths: SessionPaths,
        helpers: Helpers,
        device_id: String,
        control: Arc<SessionControl>,
        events: EventSink,
    ) -> Self {
        Self {
            paths,
            helpers,
            device_id,
            control,
            events,
        }
    }

    /// A context whose events land in the returned channel, for tests that
    /// drive one session without a service.
    #[cfg(test)]
    pub(crate) fn for_test(
        paths: SessionPaths,
        helpers: Helpers,
    ) -> (Self, std::sync::mpsc::Receiver<ProjectorEvent>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let events: EventSink = Arc::new(move |event| {
            let _ = tx.send(event);
        });
        let context = Self::new(
            paths,
            helpers,
            "test-receiver".to_owned(),
            Arc::default(),
            events,
        );
        (context, rx)
    }

    pub(crate) fn control(&self) -> &Arc<SessionControl> {
        &self.control
    }

    /// The session is over; `error` is why, if it failed.
    pub(crate) fn end(&self, error: Option<String>) {
        (self.events)(ProjectorEvent::Ended {
            device_id: self.device_id.clone(),
            error,
        });
    }

    pub(crate) fn stage(&self, stage: ConnectStage) {
        (self.events)(ProjectorEvent::Connecting {
            device_id: self.device_id.clone(),
            stage,
        });
    }

    pub(crate) fn ready(&self) {
        self.control.mark_ready();
        (self.events)(ProjectorEvent::Connected {
            device_id: self.device_id.clone(),
        });
    }

    pub(crate) fn publish_profile(&self, profile: CaptureProfile) {
        (self.events)(ProjectorEvent::CaptureProfile(profile));
    }

    pub(crate) fn wait_stop(&self, timeout: Duration) -> bool {
        self.control.wait_stop(timeout)
    }
}
