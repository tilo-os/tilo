//! Live publication of a durable commit: the frames it publishes and the
//! sessions each frame reaches. A single state commit and a committed
//! checkpoint batch publish through the same steps.

use super::{
    Arc, Envelope, FanoutStamp, HashMap, Invariant, LiveEnvelope, ReplayCatchup, RoomOperation,
    SelectedPublication, SessionLives, StateCommit, StateDelivery, StateSession, StateSource, Step,
    Subscriber, captured_live_current, gated_checkpoint_channel, live_audience, mpsc, room_access,
};

/// A live frame's share of the relay's live-byte budget: `Ok(None)` for a
/// selected frame, which is not charged, and an error when the budget is full.
pub(super) type LiveBudget =
    Result<Option<Arc<tokio::sync::OwnedSemaphorePermit>>, crate::store::StoreError>;

/// How a commit's publication treats the sessions it captured.
#[derive(Clone, Copy)]
pub(super) struct PublicationScope {
    /// A selected basis or a recovery scope is published: a recipient that
    /// misses a frame must be fenced rather than left behind.
    pub(super) activating: bool,
    /// Only sessions whose captured lease is still current are served.
    pub(super) recovering: bool,
}

impl PublicationScope {
    pub(super) fn of(commit: &StateCommit) -> Self {
        Self {
            activating: commit.selected.is_some() || commit.recovery_scope,
            recovering: commit.recovery_scope
                || matches!(commit.selected, Some(SelectedPublication::Recovery(_))),
        }
    }
}

/// The frames a commit publishes, in order: its selected basis, read one
/// output budget at a time on the owner's workers, or else its original
/// envelope.
pub(super) struct PublicationFrames {
    cursor: Option<crate::store::replay::PublicationCursor>,
    original: Option<Arc<Envelope>>,
    workers: Arc<tokio::sync::Semaphore>,
}

impl PublicationFrames {
    pub(super) async fn open(
        selected: Option<SelectedPublication>,
        envelope: &Arc<Envelope>,
        workers: Arc<tokio::sync::Semaphore>,
    ) -> Result<Self, crate::store::StoreError> {
        let cursor = if let Some(path) = selected {
            Some(crate::persistence::blocking(workers.clone(), move || path.open()).await??)
        } else {
            None
        };
        let original = cursor.is_none().then(|| envelope.clone());
        Ok(Self {
            cursor,
            original,
            workers,
        })
    }

    /// The next frame to publish; `None` once the publication is complete.
    pub(super) async fn next(&mut self) -> Result<Option<LiveEnvelope>, crate::store::StoreError> {
        while let Some(mut selected) = self.cursor.take() {
            let (next, result) = crate::persistence::blocking(self.workers.clone(), move || {
                let result = selected.step(crate::persistence::output_budget());
                (selected, result)
            })
            .await?;
            self.cursor = Some(next);
            match result? {
                Step::More => {}
                Step::Done(None) => return Ok(None),
                Step::Done(Some(batch)) => {
                    return Ok(Some(LiveEnvelope::Selected(Arc::new(batch))));
                }
            }
        }
        Ok(self.original.take().map(LiveEnvelope::Original))
    }
}

/// Charge `envelope` to the live-byte budget, which only an original frame
/// draws on.
pub(super) fn live_budget(envelope: &LiveEnvelope) -> LiveBudget {
    let LiveEnvelope::Original(original) = envelope else {
        return Ok(None);
    };
    let bytes = match &**original {
        Envelope::Delta { delta, .. } => delta.len().saturating_add(256),
        _ => 1,
    };
    crate::persistence::delivery_budget(bytes).map(Some)
}

/// One frame of a publication, with what its recipients are checked against.
/// It borrows the frame and its budget, which the publisher holds past the
/// room's lock, so their last references never drop under it.
pub(super) struct LiveFrame<'a> {
    envelope: &'a LiveEnvelope,
    budget: &'a LiveBudget,
    fanned: Option<FanoutStamp>,
    checkpoint_channel: Option<[u8; 32]>,
    audience: Option<std::collections::BTreeSet<[u8; 32]>>,
}

impl<'a> LiveFrame<'a> {
    pub(super) fn new(
        runtime: &crate::workspace::WorkspaceRuntime,
        envelope: &'a LiveEnvelope,
        budget: &'a LiveBudget,
        fanned: Option<FanoutStamp>,
        source: Option<&StateSource>,
    ) -> Self {
        let payloads = &**runtime.store.payloads();
        let checkpoint_channel = gated_checkpoint_channel(payloads, envelope, source);
        let audience = live_audience(runtime, envelope);
        Self {
            envelope,
            budget,
            fanned,
            checkpoint_channel,
            audience,
        }
    }
}

/// Fan one frame out to a commit's captured sessions, under the room's lock.
pub(super) struct LiveFanout<'a> {
    pub(super) frame: &'a LiveFrame<'a>,
    /// The frame's source; `None` for a host-authored frame.
    pub(super) source: Option<&'a StateSource>,
    pub(super) scope: PublicationScope,
    pub(super) session_lives: &'a SessionLives,
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
        if self.scope.recovering
            && !captured_live_current(self.session_lives, runtime, recipient, original)
        {
            return None;
        }
        let sub = runtime.subscribers.get(recipient)?;
        if !sub.tx.same_channel(original) {
            return None;
        }
        let target = runtime.state_sessions.get(recipient)?;
        // A host-authored frame has no source trust to ask, and
        // checks the recipient's room listing only.
        let admitted = match self.source {
            Some(source) => source.admits(runtime, &target.peer),
            None => room_access::listed(
                room_access::Room::current(runtime),
                &target.peer,
                RoomOperation::State,
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
            target
                .replay
                .as_ref()
                .invariant("replay missing")?
                .dirty
                .store(true, std::sync::atomic::Ordering::SeqCst);
            return Ok(());
        }
        if target.peer.role == crate::auth::PeerRole::Device
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
            if self.scope.activating {
                return Err(crate::persistence::error(
                    "activation delivery byte backpressure",
                ));
            }
            if replay_phase == Some(ReplayCatchup::BOUNDARY) {
                target
                    .live
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                let _ = sub.disconnect.try_send(());
            } else if let Some(lag) = &target.lag {
                lag.mark(runtime.store.workspace_id());
            }
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
                if target.peer.role == crate::auth::PeerRole::Device
                    && let Some(channel) = self.frame.checkpoint_channel
                {
                    runtime.mark_checkpoint_served(recipient, channel);
                }
            }
            // Ordinary live traffic retains replay coverage under
            // backpressure. A selected activation is a multi-frame
            // basis whose exact retry is suppressed after CURRENT;
            // a partial recipient must reconnect to recover it.
            Err(mpsc::error::TrySendError::Full(delivery)) => {
                if self.scope.activating || matches!(&delivery.envelope, LiveEnvelope::Selected(_))
                {
                    let _ = sub.disconnect.try_send(());
                    runtime.remove_subscriber_session(recipient, original);
                } else if replay_phase == Some(ReplayCatchup::BOUNDARY) {
                    target
                        .live
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                    let _ = sub.disconnect.try_send(());
                } else if let Some(lag) = &target.lag {
                    lag.mark(runtime.store.workspace_id());
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                let _ = sub.disconnect.try_send(());
                runtime.remove_subscriber_session(recipient, original);
            }
        }
        Ok(())
    }
}
