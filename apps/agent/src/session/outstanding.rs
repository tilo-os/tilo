//! What a task asks its host without waiting in line for the answer.
//!
//! Cancellation polls are round trips to the host, and none of them is on
//! the work's critical path: the model call and the steps are. They are sent
//! as detached requests ([`crate::wire::Link::send_detached`]) and their
//! answers are read where they matter.
//!
//! Ordering is not given up by not waiting. The collaboration client
//! dispatches requests to the broker one at a time, in the order they are
//! sent, and answers them in that order — so a cancellation poll sent before
//! a step is answered before that step's reply.

use ui::time::{Duration, Instant};

use anyhow::Result;
use ui::collaboration::{CollabEvent, CollabRequest};

use super::Session;
use crate::wire::Detached;

/// How long a task waits for the broker to answer one cancellation poll.
const CANCELLATION_WAIT: Duration = Duration::from_secs(5);

/// Detached requests of the task in progress.
#[derive(Debug, Default)]
pub(super) struct Outstanding {
    cancel: CancelWatch,
}

/// What this task knows about its own cancellation.
#[derive(Debug, Default)]
struct CancelWatch {
    /// Polls not answered yet: request id, and [`Self::settled`] when sent.
    polls: Vec<(u64, u64)>,
    /// The latest [`Self::settled`] at which a poll answered "not cancelled".
    clear_at: Option<u64>,
    cancelled: bool,
    /// The broker's refusal to answer, reported at the next check.
    failed: Option<String>,
    /// Steps settled so far in this task.
    settled: u64,
    polled_at: Option<Instant>,
}

impl Session {
    /// Start a task's watch afresh: an earlier task's polls say nothing
    /// about this one.
    pub(super) fn begin_task_watch(&mut self) {
        for (request_id, _) in std::mem::take(&mut self.outstanding.cancel.polls) {
            self.link.forget(request_id);
        }
        self.outstanding = Outstanding::default();
    }

    /// A step of the task settled: a cancellation answer from before it no
    /// longer speaks for the next one.
    pub(super) fn step_settled(&mut self) {
        self.outstanding.cancel.settled = self.outstanding.cancel.settled.saturating_add(1);
    }

    /// Ask the host whether `run` was cancelled, without waiting, unless a
    /// poll is already out.
    pub(super) fn poll_cancellation(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
    ) -> Result<()> {
        if self.outstanding.cancel.polls.is_empty() {
            self.send_cancellation_poll(run)?;
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
            .cancel
            .polled_at
            .is_none_or(|at| at.elapsed() >= every)
        {
            self.poll_cancellation(run)?;
        }
        Ok(())
    }

    fn send_cancellation_poll(&mut self, run: &runtime_collab::TaskRunEnvelope) -> Result<()> {
        let request_id = self.next_request_id();
        self.link
            .send_detached(&CollabRequest::TaskRunCancellation {
                request_id,
                run_id: run.run_id,
                lease: self.task_lease(run),
            })?;
        let cancel = &mut self.outstanding.cancel;
        cancel.polls.push((request_id, cancel.settled));
        cancel.polled_at = Some(ui::time::now());
        Ok(())
    }

    /// Read every cancellation answer that has arrived, without waiting.
    /// `Ok(true)` once the host has said the task is cancelled.
    ///
    /// # Errors
    ///
    /// When the broker refused to answer a poll, as the old in-line poll did.
    pub(super) fn cancellation_known(&mut self) -> Result<bool> {
        let polls = std::mem::take(&mut self.outstanding.cancel.polls);
        for (request_id, at) in polls {
            match self.link.take_detached(request_id) {
                Some(reply) => self.settle_cancellation_poll(request_id, reply, at)?,
                None => self.outstanding.cancel.polls.push((request_id, at)),
            }
        }
        if let Some(error) = self.outstanding.cancel.failed.take() {
            return Err(anyhow::anyhow!(error));
        }
        Ok(self.outstanding.cancel.cancelled)
    }

    /// Wait for every cancellation poll already out. They are ahead of any
    /// request sent now in the client's one ordered queue, so waiting here
    /// adds nothing to that request's own round trip.
    fn await_cancellation_polls(&mut self, deadline: Instant) -> Result<bool> {
        while let Some(&(request_id, at)) = self.outstanding.cancel.polls.first() {
            let Some(reply) = self.link.wait_detached(request_id, deadline) else {
                return Err(anyhow::anyhow!(
                    "the broker did not answer task cancellation status"
                ));
            };
            self.outstanding.cancel.polls.remove(0);
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
            let cancel = &self.outstanding.cancel;
            if cancel.clear_at == Some(cancel.settled) {
                return Ok(false);
            }
            if !cancel.polls.iter().any(|(_, at)| *at == cancel.settled) {
                self.send_cancellation_poll(run)?;
            }
            if self.await_cancellation_polls(deadline)? {
                return Ok(true);
            }
        }
    }

    /// Whether a cancellation poll already out says to stop before the step
    /// about to be sent. Never sends a poll of its own.
    pub(super) fn cancelled_before_step(&mut self) -> Result<bool> {
        if self.outstanding.cancel.polls.is_empty() {
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
        self.outstanding.cancel.settle(reply, at);
        Ok(())
    }
}

impl CancelWatch {
    fn settle(&mut self, reply: Detached, at: u64) {
        match reply {
            Detached::Cancellation(Ok(true)) => self.cancelled = true,
            Detached::Cancellation(Ok(false)) => {
                self.clear_at = Some(self.clear_at.map_or(at, |clear| clear.max(at)));
            }
            Detached::Cancellation(Err(error)) => {
                self.failed.get_or_insert(error);
            }
        }
    }
}
