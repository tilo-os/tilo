//! Journal transitions nobody hears of before they are durable: an admitted
//! invocation, delivered and acknowledged once its admission is written,
//! and a definite outcome, published once its terminal record is.

use std::sync::Arc;

use tokio::sync::mpsc;

use super::{
    CapnpBrokerState, InvocationCompletion, InvocationEnvelope, InvocationJob, PendingInvocation,
    invocation_journal, offload_door,
};

/// An invocation admitted in the journal, which nobody hears of (neither
/// its application nor its asker) before the admission is durable.
#[must_use = "an admission is acknowledged and delivered only by starting it"]
pub struct StagedJob {
    pub(super) admitted: invocation_journal::Durable,
    pub(super) start: Admitted,
}

pub(super) enum Admitted {
    /// Kept only as a report of the application's own local run.
    Reported(InvocationJob),
    /// To be handed to the application.
    Delivered(Box<Delivery>),
}

pub(super) struct Delivery {
    pub(super) id: u64,
    pub(super) completion: std::sync::mpsc::Receiver<InvocationCompletion>,
    pub(super) pending: PendingInvocation,
    pub(super) envelope: InvocationEnvelope,
    pub(super) sender: mpsc::UnboundedSender<InvocationEnvelope>,
    pub(super) offload: Option<(
        Arc<dyn tilo_actions::offload::ActionOffload>,
        tilo_actions::offload::OffloadRequest,
    )>,
    pub(super) checkpoint: Option<(String, String, tilo_actions::ActionContract)>,
    pub(super) untrusted: bool,
}

impl StagedJob {
    /// The job's handle, known before it starts.
    pub(crate) fn id(&self) -> u64 {
        match &self.start {
            Admitted::Reported(job) => job.id,
            Admitted::Delivered(delivery) => delivery.id,
        }
    }

    /// Wait for the admission without holding the broker's loop, then
    /// deliver it: only now may the asker be told it was accepted.
    ///
    /// # Errors
    /// The admission could not be made durable, or its application's
    /// stream closed.
    pub async fn start(self, state: &CapnpBrokerState) -> Result<InvocationJob, String> {
        self.admitted.wait().await?;
        state.start_admitted(self.start)
    }

    /// Start it once durable and hand `then` the outcome, without waiting
    /// here: for a caller answered through a [`Pending`]. Both may run on
    /// the journal's writer thread, so neither may wait on the journal.
    pub(crate) fn start_then(
        self,
        state: &Arc<CapnpBrokerState>,
        then: impl FnOnce(Result<InvocationJob, String>) + Send + 'static,
    ) {
        let state = Arc::clone(state);
        let start = self.start;
        self.admitted.then(move |outcome| {
            then(outcome.and_then(|()| state.start_admitted(start)));
        });
    }

    /// [`Self::start`] on a thread that may block, never the broker's loop.
    ///
    /// # Errors
    /// As [`Self::start`].
    pub fn start_blocking(self, state: &CapnpBrokerState) -> Result<InvocationJob, String> {
        self.admitted.wait_blocking()?;
        state.start_admitted(self.start)
    }
}

/// A definite outcome staged in the journal, heard by nobody before it is
/// durable.
#[must_use = "a definite outcome is heard only once settled"]
pub(super) struct DefiniteOutcome<'a> {
    pub(super) state: &'a CapnpBrokerState,
    pub(super) id: Option<u64>,
    pub(super) durable: invocation_journal::Durable,
    pub(super) heard: Option<(
        tilo_relay_session::client::CertifiedAction,
        tilo_actions::InvocationUpdate,
    )>,
}

impl DefiniteOutcome<'_> {
    /// Wait for the write without holding the broker's loop, then tell the
    /// network. The invocation is left to expiry and disconnects again.
    pub(super) async fn settle(mut self) -> Result<(), String> {
        let durable = std::mem::replace(&mut self.durable, invocation_journal::Durable::settled());
        durable.wait().await?;
        if let Some((evidence, update)) = self.heard.take() {
            self.state.peer_action_queue.publish(evidence, update);
        }
        Ok(())
    }
}

impl Drop for DefiniteOutcome<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.id
            && let Ok(mut committing) = self.state.committing_invocations.lock()
        {
            committing.remove(&id);
        }
    }
}

impl CapnpBrokerState {
    /// The journal write invocation `invocation` waits for, if any: the
    /// last of its transitions staged.
    pub(super) fn journal_in_flight(
        &self,
        invocation: &tilo_actions::InvocationId,
    ) -> invocation_journal::Durable {
        self.journal
            .lock()
            .ok()
            .and_then(|journal| {
                journal
                    .as_ref()
                    .ok()?
                    .as_ref()
                    .map(|journal| journal.in_flight(invocation))
            })
            .unwrap_or_else(invocation_journal::Durable::settled)
    }

    /// Write `update`, invocation `id`'s definite outcome, if `peer_id`'s
    /// invocation still stands at `staged`; heard by nobody, and the loop
    /// free, until it is durable.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the pending entry borrows the registry until it is staged; the lint misreads the borrow"
    )]
    pub(super) async fn settle_definite_outcome(
        &self,
        peer_id: u64,
        id: u64,
        staged: &tilo_actions::InvocationUpdate,
        update: &tilo_actions::InvocationUpdate,
    ) -> Result<(), capnp::Error> {
        let outcome = {
            let registry = self
                .pending_invocations
                .lock()
                .map_err(|_| capnp::Error::failed("invocation registry unavailable".into()))?;
            let pending = registry
                .get(&id)
                .filter(|pending| pending.peer_id == peer_id && &pending.update == staged)
                .ok_or_else(|| {
                    capnp::Error::failed(
                        "invocation changed while committing; outcome unknown".into(),
                    )
                })?;
            self.stage_definite_outcome(id, pending, update)
                .map_err(capnp::Error::failed)?
        };
        outcome.settle().await.map_err(capnp::Error::failed)
    }

    pub(super) fn start_admitted(&self, admitted: Admitted) -> Result<InvocationJob, String> {
        match admitted {
            Admitted::Reported(job) => Ok(job),
            Admitted::Delivered(delivery) => self.deliver_admitted(*delivery),
        }
    }

    /// Hand admission `delivery`, now durable, to its application.
    fn deliver_admitted(&self, delivery: Delivery) -> Result<InvocationJob, String> {
        let Delivery {
            id,
            completion,
            pending,
            envelope,
            sender,
            offload,
            checkpoint,
            untrusted,
        } = delivery;
        let invocation = envelope.invocation;
        if let Some(checkpoint) = checkpoint {
            self.checkpoint_jobs
                .lock()
                .map_err(|_| "checkpoint registry unavailable")?
                .insert(id, checkpoint);
        }
        if untrusted {
            self.untrusted_jobs
                .lock()
                .map_err(|_| "untrusted job registry unavailable")?
                .insert(id);
        }
        let peer_id = pending.peer_id;
        // Keep acceptance before an immediately completing application's first
        // update. Sending to the unbounded app queue and bounded observation
        // bus never waits or calls back into the broker.
        let mut registry = self
            .pending_invocations
            .lock()
            .map_err(|_| "the invocation registry is unavailable".to_string())?;
        registry.insert(id, pending);
        self.invocation_changed();
        let envelope = match (offload, self.offloads.lock()) {
            (Some((route, request)), Ok(mut queued)) => {
                queued.push(offload_door::QueuedOffload {
                    id,
                    peer_id,
                    request,
                    route,
                    envelope,
                    app: sender.clone(),
                });
                None
            }
            _ => Some(envelope),
        };
        if let Some(envelope) = envelope
            && sender.send(envelope).is_err()
        {
            let removed = registry.remove(&id);
            drop(registry);
            self.invocation_changed();
            if let Some(pending) = removed {
                let _ = self
                    .persist_uncertain_outcome(&pending, invocation_journal::unknown(invocation));
            }
            return Err("the application invocation stream closed; outcome unknown".into());
        }
        if let Some(pending) = registry.get(&id) {
            self.publish_activity(pending.activity.as_ref(), &pending.update);
        }
        drop(registry);
        self.local_turn.notify_one();
        Ok(InvocationJob { id, completion })
    }
}
