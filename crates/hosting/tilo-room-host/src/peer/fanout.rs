//! Live publication of a durable commit: the frames it publishes and the
//! sessions each frame reaches. A single state commit and a committed
//! checkpoint batch publish through the same steps.

use crate::store::record_sync::Reach;

use super::{
    Arc, Envelope, FanoutStamp, HashMap, Invariant, ReplayCatchup, StateDelivery, StateSession,
    StateSource, Subscriber, WorkspaceOperation, gated_checkpoint_channel, live_audience, mpsc,
    workspace_access,
};

/// A live frame's share of the relay's live-byte budget: `Ok(None)` for a
/// selected frame, which is not charged, and an error when the budget is full.
pub(super) type LiveBudget =
    Result<Option<Arc<tokio::sync::OwnedSemaphorePermit>>, crate::store::StoreError>;

/// Charge `envelope` to the live-byte budget.
pub(super) fn live_budget(envelope: &Envelope) -> LiveBudget {
    let bytes = match envelope {
        Envelope::Delta { delta, .. } => delta.len().saturating_add(256),
        _ => 1,
    };
    crate::persistence::delivery_budget(bytes).map(Some)
}

/// One frame of a publication, with what its recipients are checked against.
/// It borrows the frame and its budget, which the publisher holds past the
/// workspace's lock, so their last references never drop under it.
pub(super) struct LiveFrame<'a> {
    envelope: &'a Arc<Envelope>,
    budget: &'a LiveBudget,
    fanned: Option<FanoutStamp>,
    checkpoint_channel: Option<[u8; 32]>,
    audience: Option<std::collections::BTreeSet<[u8; 32]>>,
    /// Who holds the frame's record ([`crate::store::record_sync`]).
    reach: crate::store::record_sync::Reach,
}

impl<'a> LiveFrame<'a> {
    pub(super) fn new(
        runtime: &crate::workspace::WorkspaceRuntime,
        envelope: &'a Arc<Envelope>,
        budget: &'a LiveBudget,
        fanned: Option<FanoutStamp>,
        source: Option<&StateSource>,
        host: bool,
    ) -> Self {
        // The host's own change (a workspace's or a channel's governance) reaches
        // every live device: a governor that rebases or rolls over sends it
        // as checkpoints alone, which no delta before them covers.
        let payloads = &**runtime.store.payloads();
        let checkpoint_channel = (!host)
            .then(|| gated_checkpoint_channel(payloads, envelope, source))
            .flatten();
        let audience = live_audience(runtime, envelope);
        let reach = record_reach(runtime, envelope);
        Self {
            envelope,
            budget,
            fanned,
            checkpoint_channel,
            audience,
            reach,
        }
    }
}

/// Who holds the record a live frame changes: a document frame on a channel
/// whose record is on-demand reaches only the devices that hold it.
fn record_reach(
    runtime: &crate::workspace::WorkspaceRuntime,
    envelope: &Arc<Envelope>,
) -> crate::store::record_sync::Reach {
    use crate::coedit::CoeditPayload;
    let Envelope::Delta { delta, .. } = &**envelope else {
        return Reach::Everyone;
    };
    match CoeditPayload::decode(delta) {
        Some(
            CoeditPayload::Checkpoint { channel, body }
            | CoeditPayload::RetainedDelta { channel, body },
        ) => runtime.store.frame_reach(&channel, &body),
        None => Reach::Everyone,
    }
}

/// Fan one frame out to a commit's captured sessions, under the workspace's lock.
pub(super) struct LiveFanout<'a> {
    pub(super) frame: &'a LiveFrame<'a>,
    /// The frame's source; `None` for a host-authored frame.
    pub(super) source: Option<&'a StateSource>,
    /// Log each publication under correlated diagnostics.
    pub(super) trace_recipients: bool,
}

impl LiveFanout<'_> {
    /// Offer the frame to every captured session, counting each one that
    /// took it in `delivered`.
    pub(super) fn deliver(
        &self,
        runtime: &mut crate::workspace::WorkspaceRuntime,
        sessions: &[([u8; 32], mpsc::Sender<Envelope>)],
        pending: &mut HashMap<[u8; 32], std::path::PathBuf>,
        delivered: &mut u64,
    ) -> Result<(), crate::store::StoreError> {
        for (recipient, original) in sessions {
            self.deliver_to(runtime, recipient, original, pending, delivered)?;
        }
        Ok(())
    }

    /// The captured session's current subscriber and state session, when it
    /// is still the same channel and may receive this frame.
    fn recipient<'r>(
        &self,
        runtime: &'r crate::workspace::WorkspaceRuntime,
        recipient: &[u8; 32],
        original: &mpsc::Sender<Envelope>,
    ) -> Option<(&'r Subscriber, &'r StateSession)> {
        let sub = runtime.subscribers.get(recipient)?;
        if !sub.tx.same_channel(original) {
            return None;
        }
        let target = runtime.state_sessions.get(recipient)?;
        // A host-authored frame has no source trust to ask, and
        // checks the recipient's workspace listing only.
        let admitted = match self.source {
            Some(source) => source.admits(runtime, &target.peer),
            None => workspace_access::listed(
                workspace_access::WorkspaceFacts::current(runtime),
                &target.peer,
                WorkspaceOperation::State,
            ),
        };
        if !admitted {
            return None;
        }
        if self
            .frame
            .audience
            .as_ref()
            .is_some_and(|audience| !audience.contains(&target.peer.device_public_key))
        {
            return None;
        }
        // An on-demand record's changes go to who opened it, a chain's head
        // to who follows the chain; a host this workspace is attached to
        // keeps all of the workspace.
        let held = match self.frame.reach {
            Reach::Everyone | Reach::Head(_) => true,
            Reach::Opened(channel) => runtime.holds_record(recipient, &channel),
            Reach::ChainHead { head, chain } => {
                runtime.holds_record(recipient, &head) || runtime.holds_record(recipient, &chain)
            }
        };
        if !held && !runtime.attached(recipient) {
            return None;
        }
        Some((sub, target))
    }

    fn deliver_to(
        &self,
        runtime: &mut crate::workspace::WorkspaceRuntime,
        recipient: &[u8; 32],
        original: &mpsc::Sender<Envelope>,
        pending: &mut HashMap<[u8; 32], std::path::PathBuf>,
        delivered: &mut u64,
    ) -> Result<(), crate::store::StoreError> {
        let Some((sub, target)) = self.recipient(runtime, recipient, original) else {
            return Ok(());
        };
        let replay_phase = target
            .replay
            .as_ref()
            .map(|replay| replay.phase.load(std::sync::atomic::Ordering::SeqCst));
        if replay_phase == Some(ReplayCatchup::ACTIVE) {
            // This commit is already durable. The replay owner
            // will take one final coherent cut before opening
            // this exact session to post-Ready live delivery.
            let replay = target.replay.as_ref().invariant("replay missing")?;
            replay
                .dirty
                .store(true, std::sync::atomic::Ordering::SeqCst);
            // A frame for this device alone (its Welcome to a channel's
            // group) goes between the replay's batches: the keys it brings
            // open what the replay is still sending.
            if self
                .frame
                .audience
                .as_ref()
                .is_some_and(|audience| audience.contains(&target.peer.device_public_key))
            {
                replay.address(self.frame.envelope.clone());
            }
            return Ok(());
        }
        if !runtime.attached(recipient)
            && self
                .frame
                .checkpoint_channel
                .is_some_and(|channel| !runtime.needs_checkpoint(recipient, &channel))
        {
            return Ok(());
        }
        // Durable coverage was committed before publication. If
        // the shared live-byte budget is full, leave that coverage
        // for replay/anti-entropy without fencing a healthy wire.
        let Ok(budget) = self.frame.budget else {
            target.fall_behind(sub, replay_phase, runtime.store.workspace_id());
            return Ok(());
        };
        if self.trace_recipients
            && crate::persistence::correlated_diagnostics()
            && let Envelope::Delta {
                workspace_id,
                authored_at_ns,
                ..
            } = &**self.frame.envelope
        {
            tracing::info!(phase="server_delta_publication", workspace_id, authored_at_ns, recipient=%hex::encode(&recipient[..4]));
        }
        let delivery = StateDelivery {
            envelope: self.frame.envelope.clone(),
            cleanup: pending.remove(recipient),
            source: self.source.cloned(),
            _budget: budget.clone(),
            live: target.live.clone(),
            fanned: self.frame.fanned.clone(),
        };
        match target.tx.try_send(delivery) {
            Ok(()) => {
                *delivered = delivered.saturating_add(1);
                if !runtime.attached(recipient)
                    && let Some(channel) = self.frame.checkpoint_channel
                {
                    runtime.mark_checkpoint_served(recipient, channel);
                }
                // It follows this chain part now, and keeps it once a later
                // part is the head.
                if let Reach::Head(channel) | Reach::ChainHead { head: channel, .. } =
                    self.frame.reach
                {
                    runtime.hold_record(recipient, channel);
                }
            }
            // Ordinary live traffic retains replay coverage under
            // backpressure: the recipient catches up by replay.
            Err(mpsc::error::TrySendError::Full(_)) => {
                target.fall_behind(sub, replay_phase, runtime.store.workspace_id());
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                let _ = sub.disconnect.try_send(());
                runtime.remove_subscriber_session(recipient, original);
            }
        }
        Ok(())
    }
}
