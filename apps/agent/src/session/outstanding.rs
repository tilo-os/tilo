//! What a task asks its host without waiting in line for the answer.
//!
//! Live activity, progress lines and cancellation polls are round trips to
//! the host, and none of them is on the work's critical path: the model call
//! and the steps are. They are sent as detached requests
//! ([`crate::wire::Link::send_detached`]) and their answers are read where
//! they matter.
//!
//! Ordering is not given up by not waiting. The collaboration client
//! dispatches requests to the broker one at a time, in the order they are
//! sent, and answers them in that order — so a progress line still reaches
//! the host before the step after it, and a cancellation poll sent before a
//! step is answered before that step's reply.

use ui::time::{Duration, Instant};

use anyhow::Result;
use ui::collaboration::{CollabEvent, CollabRequest};

use super::{Session, TaskControlChange};
use crate::wire::{Detached, Link};

/// How long a task waits for the broker to answer one cancellation poll.
const CANCELLATION_WAIT: Duration = Duration::from_secs(5);

/// Detached requests of the task in progress.
#[derive(Debug, Default)]
pub(super) struct Outstanding {
    /// The latest live update sent and not yet answered. While it is out,
    /// periodic updates coalesce in the publisher instead of queueing up
    /// behind it.
    live: Option<u64>,
    control: ControlWatch,
}

impl Outstanding {
    /// Record the live update just sent.
    pub(super) fn record_live(&mut self, request_id: u64) {
        self.live = Some(request_id);
    }
}

/// What this task knows about its own cancellation.
#[derive(Debug, Default)]
struct ControlWatch {
    /// Polls not answered yet: request id, and [`Self::settled`] when sent.
    polls: Vec<(u64, u64)>,
    /// The latest [`Self::settled`] at which a poll answered "not cancelled".
    clear_at: Option<u64>,
    cancelled: bool,
    /// Owner-authenticated directions not yet handed to the task loop.
    directions: Vec<runtime_collab::EnrolledTaskRunDirection>,
    /// The broker's refusal to answer, reported at the next check.
    failed: Option<String>,
    /// Steps settled so far in this task.
    settled: u64,
    polled_at: Option<Instant>,
}

/// Send one enrolled task update and return its request id without waiting
/// for the host's answer; a refusal is read later with
/// [`Link::take_refusals`].
pub(super) fn report_enrolled_detached(
    link: &mut Link,
    next_request: &mut u64,
    sequence: &mut u64,
    acceptance: runtime_collab::EnrolledTaskAcceptance,
    update: runtime_collab::TaskRunUpdate,
) -> Result<u64> {
    *sequence = sequence
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("enrolled sequence exhausted"))?;
    let request_id = *next_request;
    *next_request = request_id
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("request sequence exhausted"))?;
    link.send_detached(&CollabRequest::ReportEnrolledTask {
        request_id,
        acceptance,
        sequence: *sequence,
        update,
    })?;
    Ok(request_id)
}

impl Session {
    /// Start a task's watch afresh: an earlier task's polls and refusals say
    /// nothing about this one.
    pub(super) fn begin_task_watch(&mut self) {
        for (request_id, _) in std::mem::take(&mut self.outstanding.control.polls) {
            self.link.forget(request_id);
        }
        self.outstanding = Outstanding::default();
        // Refusals of an earlier task's updates belong to that task.
        for error in self.link.take_refusals() {
            tracing::warn!(%error, "an earlier task's live update was refused");
        }
    }

    /// Whether the latest live update is still unanswered.
    pub(super) fn live_in_flight(&mut self) -> bool {
        self.outstanding
            .live
            .is_some_and(|request_id| self.link.awaiting(request_id))
    }

    /// Apply refusals of detached updates as they arrive: live activity is
    /// best effort, so a refusal turns it off and never stops the task.
    pub(super) fn absorb_refusals(&mut self) {
        for error in self.link.take_refusals() {
            tracing::warn!(%error, "live update refused; live updates stop");
            self.live.stop();
        }
    }

    /// A step of the task settled: a cancellation answer from before it no
    /// longer speaks for the next one.
    pub(super) fn step_settled(&mut self) {
        self.outstanding.control.settled = self.outstanding.control.settled.saturating_add(1);
    }

    /// Ask the host whether `run` was cancelled, without waiting, unless a
    /// poll is already out.
    pub(super) fn poll_cancellation(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
    ) -> Result<()> {
        if self.outstanding.control.polls.is_empty() {
            self.send_control_poll(run)?;
        }
        Ok(())
    }

    /// [`Self::poll_cancellation`] at most once per `every`.
    pub(super) fn poll_cancellation_every(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        every: Duration,
    ) -> Result<()> {
        if self
            .outstanding
            .control
            .polled_at
            .is_none_or(|at| at.elapsed() >= every)
        {
            self.poll_cancellation(run)?;
        }
        Ok(())
    }

    fn send_control_poll(&mut self, run: &runtime_collab::TaskRunEnvelope) -> Result<()> {
        let request_id = self.next_request_id();
        let request = if self.native_task.is_some() {
            CollabRequest::TaskRunCancellation {
                request_id,
                run_id: run.run_id,
                lease: self.task_lease(run),
            }
        } else {
            CollabRequest::EnrolledTaskRunControl {
                request_id,
                run_id: run.run_id,
                lease: run.lease.clone(),
            }
        };
        self.link.send_detached(&request)?;
        let control = &mut self.outstanding.control;
        control.polls.push((request_id, control.settled));
        control.polled_at = Some(ui::time::now());
        Ok(())
    }

    /// Read every cancellation answer that has arrived, without waiting.
    /// `Ok(true)` once the host has said the task is cancelled.
    ///
    /// # Errors
    ///
    /// When the broker refused to answer a poll, as the old in-line poll did.
    pub(super) fn cancellation_known(&mut self) -> Result<bool> {
        let polls = std::mem::take(&mut self.outstanding.control.polls);
        for (request_id, at) in polls {
            match self.link.take_detached(request_id) {
                Some(reply) => self.settle_cancellation_poll(request_id, reply, at)?,
                None => self.outstanding.control.polls.push((request_id, at)),
            }
        }
        if let Some(error) = self.outstanding.control.failed.take() {
            return Err(anyhow::anyhow!(error));
        }
        Ok(self.outstanding.control.cancelled)
    }

    /// Apply every arrived control frontier without blocking and hand exact
    /// owner directions to the model loop once. Cancellation always wins.
    pub(super) fn task_control_change_known(&mut self) -> Result<TaskControlChange> {
        if self.cancellation_known()? {
            return Ok(TaskControlChange::Cancelled);
        }
        let directions = std::mem::take(&mut self.outstanding.control.directions);
        if directions.is_empty() {
            Ok(TaskControlChange::Current)
        } else {
            Ok(TaskControlChange::Directed(directions))
        }
    }

    /// Wait for every cancellation poll already out. They are ahead of any
    /// request sent now in the client's one ordered queue, so waiting here
    /// adds nothing to that request's own round trip.
    fn await_cancellation_polls(&mut self, deadline: Instant) -> Result<bool> {
        while let Some(&(request_id, at)) = self.outstanding.control.polls.first() {
            let Some(reply) = self.link.wait_detached(request_id, deadline) else {
                return Err(anyhow::anyhow!(
                    "the broker did not answer task cancellation status"
                ));
            };
            self.outstanding.control.polls.remove(0);
            self.settle_cancellation_poll(request_id, reply, at)?;
        }
        self.cancellation_known()
    }

    /// Whether `run` was cancelled, as the host answers it after the latest
    /// settled step: a poll sent since then is reused (waited for if it is
    /// still out), otherwise one is sent and waited for.
    pub(super) fn task_cancelled(&mut self, run: &runtime_collab::TaskRunEnvelope) -> Result<bool> {
        let deadline = self.task_wait_deadline(CANCELLATION_WAIT);
        loop {
            if self.cancellation_known()? {
                return Ok(true);
            }
            let control = &self.outstanding.control;
            if control.clear_at == Some(control.settled) {
                // Rules-only work has no model turn to restart with a saved
                // direction. Match the former synchronous control survey:
                // it observes cancellation here and leaves direction
                // handling to the model task path.
                self.outstanding.control.directions.clear();
                return Ok(false);
            }
            if !control.polls.iter().any(|(_, at)| *at == control.settled) {
                self.send_control_poll(run)?;
            }
            if self.await_cancellation_polls(deadline)? {
                return Ok(true);
            }
        }
    }

    /// Whether a cancellation poll already out says to stop before the step
    /// about to be sent. Never sends a poll of its own.
    pub(super) fn cancelled_before_step(&mut self) -> Result<bool> {
        if self.outstanding.control.polls.is_empty() {
            return self.cancellation_known();
        }
        let deadline = self.task_wait_deadline(CANCELLATION_WAIT);
        self.await_cancellation_polls(deadline)
    }

    fn settle_cancellation_poll(
        &mut self,
        request_id: u64,
        reply: Detached,
        at: u64,
    ) -> Result<()> {
        let controls = self.link.take_queued(|event| {
            matches!(event, CollabEvent::TaskRunControl { request_id: id, .. } if *id == request_id)
        });
        let observed = !controls.is_empty();
        for event in controls {
            if let CollabEvent::TaskRunControl { control, .. } = event {
                self.observe_native_control(control)?;
            }
        }
        if matches!(reply, Detached::Cancellation(Ok(false)))
            && self.native_task.is_some()
            && !observed
        {
            return Err(anyhow::anyhow!(
                "native task control unavailable; refusing further actions"
            ));
        }
        self.outstanding.control.settle(reply, at);
        Ok(())
    }
}

impl ControlWatch {
    fn settle(&mut self, reply: Detached, at: u64) {
        match reply {
            Detached::Control(Ok(control)) if control.run_epoch == 0 => {
                self.failed
                    .get_or_insert_with(|| "the broker returned an invalid task epoch".into());
            }
            Detached::Control(Ok(control)) => {
                self.cancelled |= control.cancel_requested;
                if !control.cancel_requested {
                    self.clear_at = Some(self.clear_at.map_or(at, |clear| clear.max(at)));
                    self.directions = control.directions;
                }
            }
            Detached::Control(Err(error)) | Detached::Cancellation(Err(error)) => {
                self.failed.get_or_insert(error);
            }
            Detached::Cancellation(Ok(true)) => self.cancelled = true,
            Detached::Cancellation(Ok(false)) => {
                self.clear_at = Some(self.clear_at.map_or(at, |clear| clear.max(at)));
            }
            Detached::Report(_) => {}
        }
    }
}
