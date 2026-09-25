//! Actual broker attachment for the separately admitted private HTTPS lane.
use crate::action_host::ActionHost;
use qos_action_broker::peer_actions::{
    CarrierError, PeerActionQueue, PendingPeerAction, PolicyTime,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tilo_relay::{
    client::CertifiedAction,
    delivery_v3::{
        http_proof::VerifiedRequest,
        private_wire::{PrivateOperation, WireError},
    },
    peer_actions::DirectedAction,
};
use tokio::time::Instant;
#[derive(Debug)]
pub enum PrivateError {
    Unavailable,
    Wire(WireError),
    Broker(CarrierError),
}
impl std::fmt::Display for PrivateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => f.write_str("private broker unavailable"),
            Self::Wire(e) => e.fmt(f),
            Self::Broker(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for PrivateError {}
impl From<WireError> for PrivateError {
    fn from(e: WireError) -> Self {
        Self::Wire(e)
    }
}
impl From<CarrierError> for PrivateError {
    fn from(e: CarrierError) -> Self {
        Self::Broker(e)
    }
}
pub struct PrivateService {
    queue: PeerActionQueue,
    host: uuid::Uuid,
    closed: Arc<AtomicBool>,
    transitions: qos_action_broker::peer_actions::CarrierTransitions,
    watches: Vec<events::Watch>,
    next_watch: u64,
}
pub struct PendingPrivate {
    pending: PendingPeerAction,
    queue: PeerActionQueue,
    operation: PrivateOperation,
    session: [u8; 16],
    watch: Option<WatchId>,
    closed: Arc<AtomicBool>,
    host: uuid::Uuid,
}
impl PendingPrivate {
    pub fn try_reply(&mut self) -> Result<Option<DirectedAction>, PrivateError> {
        if self.closed.load(Ordering::Acquire) || self.queue.is_closed() {
            return Err(PrivateError::Unavailable);
        }
        let Some(reply) = self.pending.try_reply()? else {
            return Ok(None);
        };
        // Real queue replies still pass closed correlation before delivery.
        self.check_reply(&reply)?;
        Ok(Some(reply))
    }
    fn check_reply(&self, reply: &DirectedAction) -> Result<(), PrivateError> {
        if self.closed.load(Ordering::Acquire)
            || self.queue.is_closed()
            || Instant::now() >= self.pending.deadline()
        {
            return Err(PrivateError::Unavailable);
        }
        use tilo_relay::peer_actions::ActionMessage;
        if matches!(&reply.message,ActionMessage::Observation{host,..}|ActionMessage::BrokerCatalogue{host,..}|ActionMessage::AgentReadResultV1{host,..} if *host!=self.host)
        {
            return Err(PrivateError::Unavailable);
        }
        Ok(())
    }
    /// Structural/lifecycle validation only, never current result authorization.
    pub fn validate_response_unqualified(
        &self,
        reply: &DirectedAction,
    ) -> Result<(), PrivateError> {
        self.check_reply(reply)?;
        tilo_relay::delivery_v3::private_wire::encode_response(
            self.operation,
            self.pending.evidence().action(),
            reply,
        )?;
        Ok(())
    }
    pub fn original_request(&self) -> &DirectedAction {
        self.pending.evidence().action()
    }
    pub fn operation(&self) -> PrivateOperation {
        self.operation
    }
    pub fn session_id(&self) -> [u8; 16] {
        self.session
    }
    pub fn watch_id(&self) -> Option<WatchId> {
        self.watch
    }
    #[cfg(test)]
    fn encode_reply(&self, reply: &DirectedAction) -> Result<Vec<u8>, PrivateError> {
        self.check_reply(reply)?;
        Ok(tilo_relay::delivery_v3::private_wire::encode_response(
            self.operation,
            self.pending.evidence().action(),
            reply,
        )?)
    }
}
impl PrivateService {
    pub(crate) fn install(actions: &ActionHost) -> Result<Self, PrivateError> {
        let broker = actions
            .broker
            .app_action_broker()
            .ok_or(PrivateError::Unavailable)?;
        let (queue, host) = broker
            .peer_action_attachment()
            .map_err(|_| PrivateError::Unavailable)?;
        Self::from_queue_unqualified(queue, host)
    }
    /// Component transport only. A queue and host UUID never qualify installed
    /// handlers, current policy, an owner, or native allocation accounting.
    pub fn from_queue_unqualified(
        queue: PeerActionQueue,
        host: uuid::Uuid,
    ) -> Result<Self, PrivateError> {
        if queue.is_closed() || host.is_nil() {
            return Err(PrivateError::Unavailable);
        }
        let transitions = queue.carrier_transitions();
        Ok(Self {
            queue,
            transitions,
            watches: Vec::new(),
            next_watch: 1,
            host,
            closed: Arc::new(AtomicBool::new(false)),
        })
    }
    pub fn submit(
        &mut self,
        request: VerifiedRequest<'_>,
        peer: &qos_peer_state::StatePeer,
        deadline: Instant,
    ) -> Result<PendingPrivate, PrivateError> {
        self.submit_inner(request, peer, deadline, None)
    }
    /// Unqualified carrier plumbing only. The concrete owner must validate the
    /// registered clock, current origin, request admission and policy separately.
    pub fn submit_with_policy_time(
        &mut self,
        request: VerifiedRequest<'_>,
        peer: &qos_peer_state::StatePeer,
        deadline: Instant,
        time: PolicyTime,
    ) -> Result<PendingPrivate, PrivateError> {
        self.submit_inner(request, peer, deadline, Some(time))
    }
    fn submit_inner(
        &mut self,
        request: VerifiedRequest<'_>,
        peer: &qos_peer_state::StatePeer,
        deadline: Instant,
        time: Option<PolicyTime>,
    ) -> Result<PendingPrivate, PrivateError> {
        if self.is_closed() {
            return Err(PrivateError::Unavailable);
        }
        let operation = PrivateOperation::from_target("POST", request.request().target())?;
        let session = request.session().session_id();
        let evidence = CertifiedAction::from_http_request(request, peer)?;
        let pending = match time {
            Some(time) => self
                .queue
                .submit_certified_with_policy_time(evidence, deadline, time)?,
            None => self.queue.submit_certified(evidence, deadline)?,
        };
        Ok(PendingPrivate {
            pending,
            queue: self.queue.clone(),
            operation,
            session,
            watch: None,
            closed: self.closed.clone(),
            host: self.host,
        })
    }
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire) || self.queue.is_closed()
    }
    pub fn close(&mut self) {
        self.closed.store(true, Ordering::Release);
        self.watches.clear();
    }
}
#[cfg(test)]
mod tests;

impl Drop for PrivateService {
    fn drop(&mut self) {
        self.close();
    }
}

mod events;

/// Local carrier lifetime identity only: never serialized or an authority proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WatchId(u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchUpdate {
    Installed(WatchId),
    Unchanged,
}

mod native;
pub(crate) use native::{NativePendingPrivate, NativePrivateService};
