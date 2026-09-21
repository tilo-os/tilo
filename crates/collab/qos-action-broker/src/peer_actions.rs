//! Permissioned peer adapter for the ordinary native action broker.
//! Only inert authenticated requests cross into its bounded LocalSet queue.
use std::sync::Arc;
use tilo_relay::client::{CertifiedAction, CertifiedActionEvent};
use tilo_relay::peer_actions::{ActionMessage, DirectedAction};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use uuid::Uuid;

#[cfg(feature = "test-support")]
#[derive(Clone)]
pub struct ActionRegistrationBarrier {
    reached: Arc<std::sync::atomic::AtomicBool>,
    release: tokio::sync::watch::Receiver<bool>,
}

#[cfg(feature = "test-support")]
impl ActionRegistrationBarrier {
    pub fn new(
        reached: Arc<std::sync::atomic::AtomicBool>,
        release: tokio::sync::watch::Receiver<bool>,
    ) -> Self {
        Self { reached, release }
    }

    async fn wait(mut self) -> Result<(), ()> {
        self.reached
            .store(true, std::sync::atomic::Ordering::Release);
        while !*self.release.borrow() {
            self.release.changed().await.map_err(|_| ())?;
        }
        Ok(())
    }
}

/// Host-owned current certificate/revocation authority. Implementations read
/// the owner's explicit identity store on every check; this is never supplied
/// by a wire request. Signature, expiry, device role, room membership and action
/// policy remain mandatory broker checks independent of this provider.
pub trait CertificateAuthority: Send + Sync {
    fn authorize(
        &self,
        room: Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String>;
    /// Local SDK admission may additionally require a host's current enrollment record.
    fn authorize_join(
        &self,
        room: Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        self.authorize(room, certificate)?;
        if certificate.capabilities.can_act_as_host {
            return Err("device authority required".into());
        }
        Ok(())
    }
}

/// Explicit inputs for an ordinary peer hosting the native broker service.
/// The owner supplies its real publisher and current room tree separately.
#[derive(Clone)]
pub struct NativePeerBrokerConfig {
    pub journal_root: std::path::PathBuf,
    pub certificate_authority: Arc<dyn CertificateAuthority>,
}

const CAPACITY: usize = 32;

/// Why a native peer's private broker attachment ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerActionAttachmentError {
    /// The shared relay transport ended and may be replaced from durable state.
    TransportClosed,
    /// Local validation, custody, capacity, or broker work failed.
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestFailure {
    Capacity,
    QueueClosed,
    ReplyTimeout,
    ReplyClosed,
    BrokerRejected,
}

impl RequestFailure {
    fn label(self) -> &'static str {
        match self {
            Self::Capacity => "capacity",
            Self::QueueClosed => "queue_closed",
            Self::ReplyTimeout => "reply_timeout",
            Self::ReplyClosed => "reply_closed",
            Self::BrokerRejected => "broker_rejected",
        }
    }
}

#[derive(Clone, Debug)]
pub struct PeerActionQueue {
    sender: mpsc::Sender<Request>,
    permits: Arc<Semaphore>,
    transitions:
        tokio::sync::broadcast::Sender<(CertifiedAction, Option<tilo_actions::InvocationUpdate>)>,
}
pub struct Request {
    pub evidence: Option<CertifiedAction>,
    pub poll: Option<tilo_actions::InvocationId>,
    pub notification: Option<tilo_actions::InvocationUpdate>,
    pub reply: oneshot::Sender<Result<ActionMessage, ()>>,
    pub _permit: OwnedSemaphorePermit,
}

struct ReadinessReset(tokio::sync::watch::Sender<bool>);
impl Drop for ReadinessReset {
    fn drop(&mut self) {
        self.0.send_replace(false);
    }
}
impl PeerActionQueue {
    pub fn channel() -> (Self, mpsc::Receiver<Request>) {
        let (sender, receiver) = mpsc::channel(CAPACITY);
        (
            Self {
                sender,
                permits: Arc::new(Semaphore::new(CAPACITY)),
                transitions: tokio::sync::broadcast::channel(CAPACITY).0,
            },
            receiver,
        )
    }
    async fn request(
        &self,
        phase: &'static str,
        evidence: Option<CertifiedAction>,
        poll: Option<tilo_actions::InvocationId>,
        notification: Option<tilo_actions::InvocationUpdate>,
    ) -> Result<ActionMessage, RequestFailure> {
        let result = async {
            let permit = self
                .permits
                .clone()
                .try_acquire_owned()
                .map_err(|error| match error {
                    tokio::sync::TryAcquireError::Closed => RequestFailure::QueueClosed,
                    tokio::sync::TryAcquireError::NoPermits => RequestFailure::Capacity,
                })?;
            let (reply, rx) = oneshot::channel();
            self.sender
                .try_send(Request {
                    evidence,
                    poll,
                    notification,
                    reply,
                    _permit: permit,
                })
                .map_err(|error| match error {
                    mpsc::error::TrySendError::Full(_) => RequestFailure::Capacity,
                    mpsc::error::TrySendError::Closed(_) => RequestFailure::QueueClosed,
                })?;
            // Timeout is transport uncertainty. It never cancels admitted work.
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
                Err(_) => Err(RequestFailure::ReplyTimeout),
                Ok(Err(_)) => Err(RequestFailure::ReplyClosed),
                Ok(Ok(Err(()))) => Err(RequestFailure::BrokerRejected),
                Ok(Ok(Ok(message))) => Ok(message),
            }
        }
        .await;
        if let Err(problem) = result {
            tracing::warn!(
                phase,
                reason = problem.label(),
                "peer action attachment request failed"
            );
        }
        result
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn request_for_test(
        &self,
        evidence: Option<CertifiedAction>,
    ) -> Result<ActionMessage, ()> {
        self.request("test_request", evidence, None, None)
            .await
            .map_err(|_| ())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn available_permits_for_test(&self) -> usize {
        self.permits.available_permits()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn transitions_for_test(
        &self,
    ) -> tokio::sync::broadcast::Receiver<(CertifiedAction, Option<tilo_actions::InvocationUpdate>)>
    {
        self.transitions.subscribe()
    }

    pub fn publish(&self, evidence: CertifiedAction, update: tilo_actions::InvocationUpdate) {
        // Called only after the durable transition barrier. Lag closes the
        // observing session rather than silently skipping streaming items.
        let bounded = ActionMessage::Update {
            update: update.clone(),
        }
        .validate()
            && serde_json::to_vec(&update).is_ok_and(|bytes| {
                bytes.len() <= tilo_relay::peer_actions::MAX_PRIVATE_BYTES.saturating_sub(1024)
            });
        let _ = self.transitions.send((evidence, bounded.then_some(update)));
    }

    /// Serve an attached broker on the SAME certified state/private session.
    /// The caller must retain this task alongside its state receiver; failure
    /// must tear down that session, so stale capability cannot survive.
    pub async fn serve(
        &self,
        session: Arc<tilo_relay::RelayCoeditSession>,
        room: Uuid,
        host: Uuid,
    ) -> Result<(), ()> {
        let (acknowledged, _) = tokio::sync::watch::channel(false);
        let result = self
            .serve_with_readiness_inner(session.clone(), room, host, acknowledged, None)
            .await;
        session.close().await;
        result.map_err(|_| ())
    }

    /// Serve and report when this exact relay attachment acknowledges the
    /// expected durable broker capability. The acknowledgement returns to
    /// false when this serving future exits.
    pub async fn serve_with_readiness(
        &self,
        session: Arc<tilo_relay::RelayCoeditSession>,
        room: Uuid,
        host: Uuid,
        acknowledged: tokio::sync::watch::Sender<bool>,
    ) -> Result<(), PeerActionAttachmentError> {
        let result = self
            .serve_with_readiness_inner(session.clone(), room, host, acknowledged, None)
            .await;
        if result == Err(PeerActionAttachmentError::TransportClosed) {
            session.close().await;
        }
        result
    }

    #[cfg(feature = "test-support")]
    pub async fn serve_with_readiness_and_registration_barrier_for_test(
        &self,
        session: Arc<tilo_relay::RelayCoeditSession>,
        room: Uuid,
        host: Uuid,
        acknowledged: tokio::sync::watch::Sender<bool>,
        barrier: ActionRegistrationBarrier,
    ) -> Result<(), PeerActionAttachmentError> {
        let result = self
            .serve_with_readiness_inner(session.clone(), room, host, acknowledged, Some(barrier))
            .await;
        if result == Err(PeerActionAttachmentError::TransportClosed) {
            session.close().await;
        }
        result
    }

    async fn serve_with_readiness_inner(
        &self,
        session: Arc<tilo_relay::RelayCoeditSession>,
        room: Uuid,
        host: Uuid,
        acknowledged: tokio::sync::watch::Sender<bool>,
        #[cfg(feature = "test-support")] barrier: Option<ActionRegistrationBarrier>,
        #[cfg(not(feature = "test-support"))] _barrier: Option<()>,
    ) -> Result<(), PeerActionAttachmentError> {
        acknowledged.send_replace(false);
        let readiness = ReadinessReset(acknowledged);
        let ActionMessage::BrokerCatalogue { host: current, .. } = self
            .request("initial_catalogue", None, None, None)
            .await
            .map_err(|_| PeerActionAttachmentError::Failed)?
        else {
            return Err(PeerActionAttachmentError::Failed);
        };
        if current != host {
            return Err(PeerActionAttachmentError::Failed);
        }
        #[cfg(feature = "test-support")]
        if let Some(barrier) = barrier {
            barrier
                .wait()
                .await
                .map_err(|_| PeerActionAttachmentError::Failed)?;
        }
        session
            .enable_actions(room)
            .map_err(attachment_send_error)?;
        session
            .register_action_broker(room, host)
            .map_err(attachment_send_error)?;
        let mut watches: Vec<(CertifiedAction, tilo_actions::InvocationId, ActionMessage)> =
            Vec::new();
        let mut transitions = self.transitions.subscribe();
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            tokio::select! {
                event = session.recv_certified_action() => match event {
                    Some(CertifiedActionEvent::Delivered(evidence)) => {
                        if evidence.action().room != room { return Err(PeerActionAttachmentError::Failed); }
                        let response = self.request("delivered_action", Some(evidence.clone()), None, None).await.map_err(|_| PeerActionAttachmentError::Failed)?;
                        send(&session, &evidence, response.clone()).map_err(attachment_send_error)?;
                        if let ActionMessage::Observation { invocation, ref update, uncertain, .. } = response {
                            if !uncertain && update.as_ref().is_some_and(|u| !u.is_terminal()) {
                                if let Some(watch) = watches.iter_mut().find(|(old, id, _)| old.action().source == evidence.action().source && *id == invocation) { *watch = (evidence, invocation, response); }
                                else if watches.len() < CAPACITY { watches.push((evidence, invocation, response)); }
                                else { return Err(PeerActionAttachmentError::Failed); }
                            } else { watches.retain(|(old, id, _)| old.action().source != evidence.action().source || *id != invocation); }
                        }
                    }
                    Some(CertifiedActionEvent::Control(tilo_relay::transport_v2::EnvelopeV2::Unavailable { request_id, reason, .. })) => {
                        watches.retain(|(evidence, _, _)| evidence.action().request_id != request_id);
                        if reason == tilo_relay::peer_actions::UnavailableReason::Backpressure { return Err(PeerActionAttachmentError::Failed); }
                    },
                    Some(CertifiedActionEvent::Control(control)) => {
                        if registration_acknowledged(&control, room, host) {
                            readiness.0.send_replace(true);
                        }
                    },
                    None => return Err(PeerActionAttachmentError::TransportClosed),
                },
                transition = transitions.recv() => {
                    let (source, update) = transition.map_err(|_| PeerActionAttachmentError::Failed)?;
                    let update = update.ok_or(PeerActionAttachmentError::Failed)?;
                    if source.action().room != room { continue; }
                    if let Some((evidence, invocation, previous)) = watches.iter_mut().find(|(evidence, invocation, _)| evidence.action().source == source.action().source && *invocation == update.invocation()) {
                        if let ActionMessage::Observation { update: Some(prior), .. } = previous {
                            if sequence(&update).zip(sequence(prior)).is_some_and(|(next, old)| next <= old) { continue; }
                        }
                        let response = self.request("transition_poll", Some(evidence.clone()), Some(*invocation), Some(update)).await.map_err(|_| PeerActionAttachmentError::Failed)?;
                        if response != *previous { send(&session, evidence, response.clone()).map_err(attachment_send_error)?; *previous = response; }
                    }
                    watches.retain(|(_, _, response)| matches!(response, ActionMessage::Observation { update: Some(update), uncertain: false, .. } if !update.is_terminal()));
                }
                _ = tick.tick() => {
                    self.request("heartbeat", None, None, None).await.map_err(|_| PeerActionAttachmentError::Failed)?;
                }
            }
        }
    }
}
fn registration_acknowledged(
    control: &tilo_relay::transport_v2::EnvelopeV2,
    room: Uuid,
    host: Uuid,
) -> bool {
    matches!(
        control,
        tilo_relay::transport_v2::EnvelopeV2::ActionsReady {
            room: acknowledged_room,
            capability: tilo_relay::transport_v2::ActionCapability::DurableBroker {
                host: acknowledged_host,
            },
        } if *acknowledged_room == room && *acknowledged_host == host
    )
}
fn sequence(update: &tilo_actions::InvocationUpdate) -> Option<u64> {
    match update {
        tilo_actions::InvocationUpdate::Progress { sequence, .. }
        | tilo_actions::InvocationUpdate::Item { sequence, .. } => Some(*sequence),
        _ => None,
    }
}

fn send(
    session: &tilo_relay::RelayCoeditSession,
    evidence: &CertifiedAction,
    message: ActionMessage,
) -> Result<(), tilo_relay::client::ClientError> {
    let request = evidence.action();
    session.send_action(bounded_reply(request, message))
}

fn bounded_reply(request: &DirectedAction, message: ActionMessage) -> DirectedAction {
    let mut reply = DirectedAction {
        room: request.room,
        request_id: request.request_id,
        source: request.target,
        target: request.source,
        message,
    };
    // Reject locally with the original correlation ID. Letting the relay reject
    // an invalid response reports only to us and leaves the caller waiting.
    if !reply.validate()
        || tilo_relay::transport_v2::encode(&tilo_relay::transport_v2::EnvelopeV2::Directed(
            reply.clone(),
        ))
        .is_err()
    {
        let diagnostics = reply_limit_diagnostics(&reply);
        tracing::warn!(
            message_kind = diagnostics.message_kind,
            route_valid = diagnostics.route_valid,
            structural_valid = diagnostics.structural_valid,
            json_serialized = diagnostics.json_serialized,
            json_bytes = diagnostics.json_bytes,
            json_bytes_within_limit = diagnostics.json_bytes_within_limit,
            surface_count = ?diagnostics.surface_count,
            action_count = ?diagnostics.action_count,
            envelope_encoded = diagnostics.envelope_encoded,
            envelope_bytes = ?diagnostics.envelope_bytes,
            "native action reply exceeded transport limits"
        );
        reply.message = ActionMessage::Rejected {
            code: "action_response_exceeds_limits".into(),
        };
    }
    reply
}

struct ReplyLimitDiagnostics {
    message_kind: &'static str,
    route_valid: bool,
    structural_valid: bool,
    json_serialized: bool,
    json_bytes: usize,
    json_bytes_within_limit: bool,
    surface_count: Option<usize>,
    action_count: Option<usize>,
    envelope_encoded: bool,
    envelope_bytes: Option<usize>,
}

fn reply_limit_diagnostics(reply: &DirectedAction) -> ReplyLimitDiagnostics {
    let message = &reply.message;
    let message_kind = match message {
        ActionMessage::CatalogueRefresh { .. } => "catalogue_refresh",
        ActionMessage::CommittedDesktop { .. } => "committed_desktop",
        ActionMessage::BrokerCatalogue { .. } => "broker_catalogue",
        ActionMessage::InvokeBound { .. } => "invoke_bound",
        ActionMessage::Observation { .. } => "observation",
        ActionMessage::Rejected { .. } => "rejected",
        ActionMessage::Catalogue { .. } => "catalogue",
        ActionMessage::Invoke { .. } => "invoke",
        ActionMessage::Status { .. } => "status",
        ActionMessage::Update { .. } => "update",
        ActionMessage::Cancel { .. } => "cancel",
        ActionMessage::CancellationRequested { .. } => "cancellation_requested",
    };
    let catalogue = match message {
        ActionMessage::Catalogue { catalogue }
        | ActionMessage::BrokerCatalogue { catalogue, .. } => Some(catalogue),
        _ => None,
    };
    let surfaces = catalogue
        .and_then(|catalogue| catalogue.get("surfaces"))
        .and_then(serde_json::Value::as_array);
    let surface_count = surfaces.map(Vec::len);
    let action_count = surfaces.map(|surfaces| {
        surfaces
            .iter()
            .filter_map(|surface| surface.get("actions"))
            .filter_map(serde_json::Value::as_array)
            .map(Vec::len)
            .fold(0usize, usize::saturating_add)
    });
    let json = serde_json::to_vec(message);
    let json_serialized = json.is_ok();
    let json_bytes = json.as_ref().map_or(0, Vec::len);
    let json_bytes_within_limit = json
        .as_ref()
        .is_ok_and(|bytes| bytes.len() <= tilo_relay::peer_actions::MAX_PRIVATE_BYTES);
    let encoded = tilo_relay::transport_v2::encode(
        &tilo_relay::transport_v2::EnvelopeV2::Directed(reply.clone()),
    );
    ReplyLimitDiagnostics {
        message_kind,
        route_valid: !reply.room.is_nil() && !reply.request_id.is_nil(),
        structural_valid: message.validate(),
        json_serialized,
        json_bytes,
        json_bytes_within_limit,
        surface_count,
        action_count,
        envelope_encoded: encoded.is_ok(),
        envelope_bytes: encoded.as_ref().ok().map(Vec::len),
    }
}

fn attachment_send_error(error: tilo_relay::client::ClientError) -> PeerActionAttachmentError {
    match error {
        tilo_relay::client::ClientError::PrivateTransportClosed => {
            PeerActionAttachmentError::TransportClosed
        }
        _ => PeerActionAttachmentError::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_reply_returns_a_correlated_rejection_instead_of_disappearing() {
        let request = DirectedAction {
            room: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            source: [1; 32],
            target: [2; 32],
            message: ActionMessage::CatalogueRefresh { scope: None },
        };
        let reply = bounded_reply(
            &request,
            ActionMessage::BrokerCatalogue {
                host: Uuid::new_v4(),
                catalogue: serde_json::json!({"version":1,"surfaces":[],"text":"x".repeat(tilo_relay::peer_actions::MAX_PRIVATE_BYTES)}),
            },
        );
        assert!(matches!(reply.message, ActionMessage::Rejected { .. }));
        assert!(reply.validate());
        assert_eq!(reply.request_id, request.request_id);
        assert_eq!(
            (reply.source, reply.target),
            (request.target, request.source)
        );
        assert!(
            tilo_relay::transport_v2::encode(&tilo_relay::transport_v2::EnvelopeV2::Directed(
                reply
            ))
            .is_ok()
        );
    }

    #[test]
    fn oversized_reply_diagnostics_distinguish_json_bytes_from_structure() {
        let request = DirectedAction {
            room: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            source: [1; 32],
            target: [2; 32],
            message: ActionMessage::CatalogueRefresh { scope: None },
        };
        let message = ActionMessage::BrokerCatalogue {
            host: Uuid::new_v4(),
            catalogue: serde_json::json!({
                "version": 1,
                "surfaces": [{"actions": [{}, {}]}],
                "text": "x".repeat(tilo_relay::peer_actions::MAX_PRIVATE_BYTES),
            }),
        };
        let reply = DirectedAction {
            room: request.room,
            request_id: request.request_id,
            source: request.target,
            target: request.source,
            message,
        };
        let diagnostics = reply_limit_diagnostics(&reply);
        assert_eq!(diagnostics.message_kind, "broker_catalogue");
        assert!(diagnostics.structural_valid);
        assert!(!diagnostics.json_bytes_within_limit);
        assert!(diagnostics.json_bytes > tilo_relay::peer_actions::MAX_PRIVATE_BYTES);
        assert_eq!(diagnostics.surface_count, Some(1));
        assert_eq!(diagnostics.action_count, Some(2));
        assert!(!diagnostics.envelope_encoded);
    }

    #[test]
    fn only_exact_durable_registration_acknowledges_readiness() {
        use tilo_relay::transport_v2::{ActionCapability, EnvelopeV2};

        let room = Uuid::from_u128(1);
        let host = Uuid::from_u128(2);
        for (control, expected) in [
            (
                EnvelopeV2::ActionsReady {
                    room,
                    capability: ActionCapability::TransportOnly,
                },
                false,
            ),
            (
                EnvelopeV2::ActionsReady {
                    room: Uuid::from_u128(3),
                    capability: ActionCapability::DurableBroker { host },
                },
                false,
            ),
            (
                EnvelopeV2::ActionsReady {
                    room,
                    capability: ActionCapability::DurableBroker {
                        host: Uuid::from_u128(4),
                    },
                },
                false,
            ),
            (
                EnvelopeV2::ActionsReady {
                    room,
                    capability: ActionCapability::DurableBroker { host },
                },
                true,
            ),
        ] {
            assert_eq!(registration_acknowledged(&control, room, host), expected);
        }
    }

    #[test]
    fn attachment_exit_clears_only_its_own_readiness() {
        let (old, old_ready) = tokio::sync::watch::channel(false);
        let (replacement, replacement_ready) = tokio::sync::watch::channel(false);
        let old = ReadinessReset(old);
        let replacement = ReadinessReset(replacement);
        old.0.send_replace(true);
        assert!(*old_ready.borrow());
        assert!(!*replacement_ready.borrow());
        drop(old);
        assert!(!*old_ready.borrow());
        assert!(!*replacement_ready.borrow());
        replacement.0.send_replace(true);
        assert!(*replacement_ready.borrow());
    }

    #[test]
    fn only_closed_private_transport_is_retryable() {
        use tilo_relay::client::ClientError;

        assert_eq!(
            attachment_send_error(ClientError::PrivateTransportClosed),
            PeerActionAttachmentError::TransportClosed
        );
        for error in [
            ClientError::Codec("invalid frame".into()),
            ClientError::Codec("private queue full".into()),
            ClientError::UnsupportedProtocol,
        ] {
            assert_eq!(
                attachment_send_error(error),
                PeerActionAttachmentError::Failed
            );
        }
    }

    #[tokio::test]
    async fn native_peer_facade_queue_bounds_total_inflight_and_disconnects() {
        let (queue, receiver) = PeerActionQueue::channel();
        let mut pending = Vec::new();
        for _ in 0..CAPACITY {
            let queue = queue.clone();
            pending.push(tokio::spawn(async move {
                queue.request("test_request", None, None, None).await
            }));
        }
        tokio::task::yield_now().await;
        assert!(
            queue
                .request("test_request", None, None, None)
                .await
                .is_err()
        );
        drop(receiver);
        for request in pending {
            assert!(request.await.unwrap().is_err());
        }
        assert!(
            queue
                .request("test_request", None, None, None)
                .await
                .is_err()
        );
    }
}
