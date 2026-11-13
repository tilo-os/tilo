use std::sync::Arc;

use tilo_lock::MutexRecover as _;
use tokio::sync::mpsc as tokio_mpsc;

use super::{
    BridgeState, CollabEvent, DocumentReassembly, EventTurn, QueuedCollabEvent,
    SDK_CATALOGUE_EVENT_BYTES, SDK_DOCUMENT_EVENT_BYTES, SDK_DOCUMENT_EVENT_RETRY,
    SDK_EVENT_QUEUE_BYTES, event_bytes, event_kind,
};

/// Bounded inbound half. Durable events wait for the owner to make room
/// (their streams stop reading meanwhile); presence is explicitly lossy.
/// Stream tasks stop once the owner has gone away.
#[derive(Clone)]
pub(super) struct InboundBridge {
    pub(super) tx: tokio_mpsc::Sender<QueuedCollabEvent>,
    pub(super) state: BridgeState,
    pub(super) reassembly: Arc<DocumentReassembly>,
    pub(super) replies: Arc<ReplyLane>,
}

impl InboundBridge {
    pub(super) fn new(tx: tokio_mpsc::Sender<QueuedCollabEvent>, state: BridgeState) -> Self {
        Self {
            tx,
            state,
            reassembly: Arc::default(),
            replies: Arc::default(),
        }
    }

    /// Deliver a document event without ever disconnecting for size: wait
    /// for the owner to drain earlier document events when the document
    /// budget is full. Returns false only when the bridge has failed or the
    /// owner went away.
    pub(super) async fn send_document(&self, event: CollabEvent) -> bool {
        let bytes = event_bytes(&event).unwrap_or(usize::MAX);
        let budget = &self.state.document_event_bytes;
        let reservation = loop {
            let freed = budget.1.notified();
            tokio::pin!(freed);
            freed.as_mut().enable();
            if let Some(reservation) = budget.reserve_sole(bytes, SDK_DOCUMENT_EVENT_BYTES) {
                break reservation;
            }
            if !self.state.is_healthy() {
                return false;
            }
            let _ = tokio::time::timeout(SDK_DOCUMENT_EVENT_RETRY, freed).await;
        };
        self.enqueue(QueuedCollabEvent {
            event,
            _reservation: reservation,
            _turn: None,
        })
        .await
    }

    pub(super) async fn send_durable(&self, event: CollabEvent) -> bool {
        self.send_durable_turn(event, None).await
    }

    /// Queue a durable event, waiting while the owner is behind on its
    /// events: the stream that produced it reads nothing more from the
    /// broker until the owner has made workspace, so a burst slows its source
    /// down instead of ending collaboration. The producer turn is retained
    /// through the owner's acknowledgement. False only when the owner went
    /// away or the event can never fit.
    pub(super) async fn send_durable_turn(
        &self,
        event: CollabEvent,
        turn: Option<EventTurn>,
    ) -> bool {
        let Some(bytes) = self.fitting_bytes(&event) else {
            return false;
        };
        let (budget, limit) = self.budget(&event);
        let reservation = loop {
            let freed = budget.1.notified();
            tokio::pin!(freed);
            freed.as_mut().enable();
            if let Some(reservation) = budget.reserve(bytes, limit) {
                break reservation;
            }
            if self.tx.is_closed() {
                self.state.receiver_gone("inbound event queue");
                return false;
            }
            freed.await;
        };
        self.enqueue(QueuedCollabEvent {
            event,
            _reservation: reservation,
            _turn: turn,
        })
        .await
    }

    /// Deliver the answer to something the owner asked, from a task that
    /// must not wait on the owner: the owner may itself be waiting for that
    /// task (a publication receipt, a join). Answers keep their order; one
    /// that finds the owner behind waits in [`ReplyLane`] for
    /// [`Self::forward_replies`]. The lane holds at most what the owner has
    /// asked and not yet been answered.
    pub(super) fn send_reply(&self, event: CollabEvent) -> bool {
        self.send_reply_turn(event, None)
    }

    pub(super) fn send_reply_turn(&self, event: CollabEvent, turn: Option<EventTurn>) -> bool {
        let mut lane = self.replies.state.lock_recover();
        if lane.closed {
            return false;
        }
        if lane.forwarding || !lane.waiting.is_empty() {
            lane.waiting.push_back((event, turn));
            return true;
        }
        match self.try_queue(event, turn) {
            Ok(delivered) => delivered,
            Err(waiting) => {
                lane.waiting.push_back(waiting);
                drop(lane);
                self.replies.wake.notify_one();
                true
            }
        }
    }

    /// Deliver what waits in the reply lane, in order, for as long as the
    /// owner reads its events. Runs on the shovel's `LocalSet`.
    pub(super) async fn forward_replies(&self) {
        loop {
            let wake = self.replies.wake.notified();
            tokio::pin!(wake);
            wake.as_mut().enable();
            let next = {
                let mut lane = self.replies.state.lock_recover();
                let next = lane.waiting.pop_front();
                lane.forwarding = next.is_some();
                next
            };
            let Some((event, turn)) = next else {
                wake.await;
                continue;
            };
            if !self.send_durable_turn(event, turn).await {
                let mut lane = self.replies.state.lock_recover();
                lane.waiting.clear();
                lane.forwarding = false;
                lane.closed = true;
                return;
            }
        }
    }

    /// Queue `event` at once when the owner has room for it. `Err` hands it
    /// back when the owner is behind; `Ok(false)` when it can never be
    /// delivered.
    fn try_queue(&self, event: CollabEvent, turn: Option<EventTurn>) -> Result<bool, ReplyParts> {
        let Some(bytes) = self.fitting_bytes(&event) else {
            return Ok(false);
        };
        let (budget, limit) = self.budget(&event);
        let Some(reservation) = budget.reserve(bytes, limit) else {
            return Err((event, turn));
        };
        let queued = QueuedCollabEvent {
            event,
            _reservation: reservation,
            _turn: turn,
        };
        match self.tx.try_send(queued) {
            Ok(()) => {
                self.state.notify_events();
                Ok(true)
            }
            Err(tokio_mpsc::error::TrySendError::Full(QueuedCollabEvent {
                event, _turn, ..
            })) => Err((event, _turn)),
            Err(tokio_mpsc::error::TrySendError::Closed(_)) => {
                self.state.receiver_gone("inbound event queue");
                Ok(false)
            }
        }
    }

    /// The budget `event` is charged to, and its limit: an action catalogue
    /// change has its own, so a big workspace's catalogue neither starves
    /// action results nor is refused for their budget.
    fn budget(&self, event: &CollabEvent) -> (&super::ByteBudget, usize) {
        if matches!(event, CollabEvent::ActionCatalogue { .. }) {
            (&self.state.catalogue_event_bytes, SDK_CATALOGUE_EVENT_BYTES)
        } else {
            (&self.state.event_bytes, SDK_EVENT_QUEUE_BYTES)
        }
    }

    /// The bytes `event` retains, when it can fit its budget at all. One
    /// that never can is a protocol violation, not a burst: the bridge fails
    /// closed rather than wait for workspace that never comes.
    fn fitting_bytes(&self, event: &CollabEvent) -> Option<usize> {
        let required_bytes = event_bytes(event).unwrap_or(usize::MAX);
        let (_, limit) = self.budget(event);
        if required_bytes <= limit {
            return Some(required_bytes);
        }
        tracing::error!(
            event_kind = event_kind(event),
            required_bytes,
            limit_bytes = limit,
            "collab: inbound control event exceeds its retained byte budget"
        );
        self.state.fail_closed("inbound event byte budget");
        None
    }

    async fn enqueue(&self, queued: QueuedCollabEvent) -> bool {
        if self.tx.send(queued).await.is_ok() {
            self.state.notify_events();
            true
        } else {
            self.state.receiver_gone("inbound event queue");
            false
        }
    }

    /// Returns false only when the receiver has gone away. A full queue is a
    /// permitted transient-presence drop and does not terminate the stream.
    pub(super) fn send_presence(&self, event: CollabEvent) -> bool {
        let Some(reservation) = self.state.event_bytes.reserve(
            event_bytes(&event).unwrap_or(usize::MAX),
            SDK_EVENT_QUEUE_BYTES,
        ) else {
            return true;
        };
        match self.tx.try_send(QueuedCollabEvent {
            event,
            _reservation: reservation,
            _turn: None,
        }) {
            Ok(()) => {
                self.state.notify_events();
                true
            }
            Err(tokio_mpsc::error::TrySendError::Full(_)) => true,
            Err(tokio_mpsc::error::TrySendError::Closed(_)) => {
                self.state.receiver_gone("inbound presence queue");
                false
            }
        }
    }
}

pub(super) type ReplyParts = (CollabEvent, Option<EventTurn>);

/// Answers waiting, in order, for the owner to make room.
#[derive(Default)]
pub(super) struct ReplyLane {
    pub(super) state: std::sync::Mutex<ReplyLaneState>,
    wake: tokio::sync::Notify,
}

#[derive(Default)]
pub(super) struct ReplyLaneState {
    pub(super) waiting: std::collections::VecDeque<ReplyParts>,
    /// The forwarder holds an answer it took from `waiting`; new answers
    /// queue behind it.
    forwarding: bool,
    /// The owner went away; nothing more is delivered.
    closed: bool,
}
