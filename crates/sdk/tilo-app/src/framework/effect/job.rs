//! A job's cancellation signals: what stopping a job sets, how a job tracks
//! the invoker's signal alongside the UI thread's own, and which jobs stop
//! before they are polled ([`Running::prepare`](super::Running::prepare)).

use super::{Job, JobCustody, JobEvent, Next, Progress, failure, track_in, track_job};
use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use ui_core::WakeHandle;
use ui_core::invocation_scope::{ResourceEffectScope, ResourceInvocationScope};

/// The signals that stop a job: the UI thread's own, which it sets when the
/// caller stops waiting, and the invoker's, which fires past the caller's
/// wait ([`EffectCx::cancel_by`](super::EffectCx::cancel_by)).
pub(super) struct Signals {
    /// The UI thread's signal: set whenever the job is stopped.
    pub(super) cancelled: Arc<AtomicBool>,
    /// The signal `Entry::cancel` sets against the UI thread's own signal,
    /// for a job whose answer outlives its caller
    /// (see [`EffectCx::cancel_by`](super::EffectCx::cancel_by)).
    pub(super) cancel_by: Option<Arc<AtomicBool>>,
}

impl Signals {
    /// The UI thread's signal, nothing else yet.
    pub(super) fn own() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            cancel_by: None,
        }
    }

    /// Whether a signal other than the UI thread's stopped the job: the
    /// invoker's went off.
    pub(super) fn fired(&self) -> bool {
        self.cancel_by
            .as_ref()
            .is_some_and(|signal| signal.load(Ordering::Acquire))
    }

    /// Stop the job: set both signals, so no further arrival of its answer
    /// is taken.
    pub(super) fn stop(self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(cancel_by) = self.cancel_by {
            cancel_by.store(true, Ordering::Release);
        }
    }
}

/// What a job reports and how it is stopped: the same for every resource,
/// so one copy of it serves them all.
pub(super) struct JobCore {
    events: Receiver<JobEvent>,
    signals: Signals,
    pub(super) task: Option<ui_core::rt::JoinHandle<()>>,
    pub(super) source: Option<super::Source>,
    sequence: u64,
    pub(super) custody: Option<ResourceInvocationScope>,
}

impl JobCore {
    /// The core of the job `event`s arrives on, reporting through
    /// `signals`, under `custody`.
    pub(super) fn of(
        events: Receiver<JobEvent>,
        signals: Signals,
        custody: Option<ResourceInvocationScope>,
    ) -> Self {
        Self {
            events,
            signals,
            task: None,
            source: None,
            sequence: 0,
            custody,
        }
    }

    pub(super) fn stop(self) {
        self.signals.stop();
        if let Some(task) = self.task {
            task.abort();
        }
    }

    /// Whether a signal other than the UI thread's stopped the job.
    pub(super) fn fired(&self) -> bool {
        self.signals.fired()
    }

    /// The next thing the job reports, if it reported anything.
    pub(super) fn next(&mut self) -> Option<Next> {
        if let Some(error) = self
            .custody
            .as_ref()
            .and_then(|custody| custody.check().err())
        {
            return Some(Next::Failed(failure(error)));
        }
        let answered = self.source.as_mut().and_then(|source| source());
        let event = match answered.map_or_else(
            || self.events.try_recv(),
            |value| Ok(JobEvent::Finished(value)),
        ) {
            Ok(event) => event,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                return Some(Next::Failed(failure(
                    "the effect ended without an answer".into(),
                )));
            }
        };
        if let Some(error) = self
            .custody
            .as_ref()
            .and_then(|custody| custody.check().err())
        {
            return Some(Next::Failed(failure(error)));
        }
        Some(match event {
            JobEvent::Failed(error) => Next::Failed(error),
            JobEvent::Progress {
                phase,
                completed,
                total,
            } => {
                self.sequence = self.sequence.saturating_add(1);
                Next::Progress(super::Polled::Progress {
                    sequence: self.sequence,
                    phase,
                    completed,
                    total,
                })
            }
            JobEvent::Finished(value) => Next::Finished(value),
        })
    }
}

/// How a job an effect's handler started is tracked: the context's job
/// slot and what a job's progress needs of it.
pub(super) struct Tracking<'a> {
    pub(super) started: &'a mut Option<Job>,
    pub(super) wake: &'a WakeHandle,
    pub(super) worker: Option<&'a ResourceEffectScope>,
    pub(super) custody: Option<&'a ResourceInvocationScope>,
    pub(super) scope: Option<&'a std::rc::Weak<super::super::surface::SurfaceScope>>,
    /// The signal the invocation's cancellation sets, for a job whose answer
    /// outlives the caller's own wait
    /// ([`EffectCx::cancel_by`](super::EffectCx::cancel_by)).
    pub(super) cancel_by: Option<&'a Arc<AtomicBool>>,
    /// The `Then<R, H>` type of the runner's completions.
    pub(super) owner: std::any::TypeId,
}

/// Start tracking a job in the context `tracking` describes, with the
/// completion `then`, tied to the context's surface: the [`Progress`] its
/// work reports through.
#[inline(never)]
pub(super) fn track_for(tracking: &mut Tracking<'_>, then: Option<Box<dyn Any>>) -> Progress {
    let progress = track_job(
        &mut *tracking.started,
        tracking.wake,
        JobCustody::of(tracking.worker, tracking.custody),
        then,
        tracking.owner,
        tracking.cancel_by.cloned(),
    );
    track_in(tracking.scope, &progress);
    progress
}
