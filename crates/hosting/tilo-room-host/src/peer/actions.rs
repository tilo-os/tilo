//! Remote actions on both wire generations: v1 `RemoteAction` routing and
//! the app-serving plane, and the v2 private lane's targets, routing and
//! delivery authorization.

use super::{
    ActionCapability, ActionMessage, AsyncWriteExt, AuthenticatedPeer, DirectedAction, Envelope,
    EnvelopeV2, PeerError, RoomOperation, SessionCtx, SharedWorkspace, SignedDirectedAction,
    UnavailableReason, action_peer_admitted, current_action_peer, current_session, mpsc,
    test_point, unavailable_target, unix_now_ns,
};
use tilo_lock::RwLockRecover as _;

/// Where an inbound [`Envelope::RemoteAction`] asked to go.
pub(super) struct RemoteActionRoute {
    pub(super) workspace_id: u64,
    pub(super) version: u16,
    pub(super) to: [u8; 32],
}

/// Handle an inbound `RemoteAction`: stamp the authenticated sender, then
/// hand it to the addressed device's live connection, or — when addressed to
/// this relay — to the app-serving plane. Never stored, never executed by the
/// relay itself; every failure is answered as undeliverable so the sender
/// need not wait out a timeout, with a reason that leads with
/// [`UnavailableReason::wire_token`].
///
/// Both ends are re-authorised on every frame under a fresh membership read
/// with [`action_peer_admitted`], the rule the v2 private lane uses: the
/// connection-local `subscribed` list alone goes stale the moment a device is
/// removed, revoked, or its agent delegation lapses.
pub(super) async fn handle_remote_action(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    route: RemoteActionRoute,
    frame: Vec<u8>,
) -> Option<Envelope> {
    use UnavailableReason as U;
    let RemoteActionRoute {
        workspace_id,
        version,
        to,
    } = route;
    let refuse = |reason: UnavailableReason, detail: &str| {
        Some(Envelope::RemoteActionUndeliverable {
            workspace_id,
            to_device_public_key: to,
            reason: format!("{}: {detail}", reason.wire_token()),
        })
    };
    if version != crate::transport::REMOTE_ACTION_ENVELOPE_VERSION {
        return refuse(
            U::InvalidRequest,
            "unsupported remote action envelope version",
        );
    }
    if !subscribed.contains(&workspace_id) {
        return refuse(
            U::NotAuthorised,
            "sender is not subscribed to the workspace",
        );
    }
    if frame.len() > tilo_actions::remote::MAX_REMOTE_ACTION_FRAME_BYTES {
        return refuse(U::InvalidRequest, "remote action frame exceeds the limit");
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return refuse(U::NotAuthorised, "workspace is not bound on this relay");
    };
    let Ok(mut wr) = crate::workspace_io::fresh_membership(&ws).await else {
        return refuse(
            U::AdmissionUnavailable,
            "workspace membership is unreadable",
        );
    };
    if action_peer_admitted(&wr, peer, session).is_err()
        || !current_session(&wr, peer, session.outbound)
    {
        return refuse(U::NotAuthorised, "sender may not act in the workspace");
    }
    if to == *session.own_device_public_key {
        // Never hold the room across the plane: it may take its time.
        drop(wr);
        let Some(plane) = session.app_serving else {
            return refuse(U::UnsupportedPeer, "this host does not serve apps");
        };
        return serve_locally(peer, session, plane.as_ref(), workspace_id, &frame)
            .unwrap_or_else(|(reason, detail)| refuse(reason, detail));
    }
    // Every production subscription registers its authenticated peer beside
    // its subscriber, so the addressee is re-authorised from it.
    let Some(target) = wr.state_sessions.get(&to) else {
        return refuse(
            unavailable_target(&wr, &to),
            "addressed device is not connected to the workspace",
        );
    };
    if action_peer_admitted(&wr, &target.peer, session).is_err() {
        return refuse(
            U::NotAuthorised,
            "addressed device may not act in the workspace",
        );
    }
    let stamped = Envelope::RemoteAction {
        workspace_id,
        version,
        to_device_public_key: to,
        from_device_public_key: peer.device_public_key,
        from_user_public_key: peer.user_public_key,
        frame,
    };
    let delivered = wr.route_directed(&to, stamped);
    drop(wr);
    if delivered {
        None
    } else {
        refuse(
            U::Offline,
            "addressed device is not connected to the workspace",
        )
    }
}

/// Handle an inbound `HostAction`: one of this host's own operations
/// (`tilo.host`), asked over the caller's authenticated connection with no
/// workspace. The host's governance decides who may, as for every door:
/// only the devices of the host's own person, speaking for themselves.
///
/// Every request that can be read is answered: a refusal comes back at once
/// as the returned envelope, and an answer the plane abandons comes back as a
/// refusal ([`answer_later`]). A frame too large to read or that does not
/// decode names no request to answer; it is logged.
pub(super) fn handle_host_action(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    frame: &[u8],
) -> Option<Envelope> {
    use tilo_actions::remote::{ActionHostError, RemoteActionFrame, refuse};
    if frame.len() > tilo_actions::remote::MAX_REMOTE_ACTION_FRAME_BYTES {
        tracing::warn!(
            device = %hex::encode(peer.device_public_key),
            bytes = frame.len(),
            "a host action frame exceeds the limit; it was not read"
        );
        return None;
    }
    let request = match RemoteActionFrame::decode(frame) {
        Ok(request) => request,
        Err(error) => {
            tracing::warn!(
                device = %hex::encode(peer.device_public_key),
                %error,
                "a host action frame does not decode; it names no request to answer"
            );
            return None;
        }
    };
    let refused = |error: ActionHostError| host_action_envelope(refuse(&request.message, error)?);
    if !super::session_peer_authorized(peer, session) {
        return refused(ActionHostError::NotPermitted);
    }
    let Some(plane) = session.app_serving else {
        return refused(ActionHostError::Unavailable {
            reason: "this host serves no host operations".into(),
        });
    };
    let caller = match session_caller(peer, session) {
        Ok(caller) => caller,
        Err((_, detail)) => {
            return refused(ActionHostError::Refused {
                reason: detail.into(),
            });
        }
    };
    let unanswered = refuse(
        &request.message,
        ActionHostError::Unavailable {
            reason: "this host answers no such operation".into(),
        },
    );
    let abandoned = refuse(
        &request.message,
        ActionHostError::Unavailable {
            reason: "the host dropped the request".into(),
        },
    );
    let Some(reply) = plane.serve_host(&caller, &peer.cert, request.message) else {
        // `None` for a response (which needs no answer) or an operation the
        // plane does not serve, which is refused.
        return host_action_envelope(unanswered?);
    };
    let outbound = session.outbound.clone();
    answer_later(reply, abandoned, move |reply| {
        host_action_envelope(reply).map(|answer| (outbound, answer))
    });
    None
}

/// `message` as a `HostAction` answer; `None`, logged, if it cannot be
/// encoded.
fn host_action_envelope(message: tilo_actions::remote::RemoteActionMessage) -> Option<Envelope> {
    match tilo_actions::remote::RemoteActionFrame::new(message).encode() {
        Ok(frame) => Some(Envelope::HostAction { frame }),
        Err(error) => {
            tracing::error!(%error, "a host action answer does not encode; the asker is not answered");
            None
        }
    }
}

/// Write the answer `reply` resolves to on the connection once it is ready;
/// an answer the plane drops is `abandoned` instead (a refusal, or `None` for
/// a message that needed no answer). The answer waits for room in the
/// connection's queue rather than being dropped when the room's own traffic
/// filled it, and ends with the connection.
fn answer_later(
    reply: tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>,
    abandoned: Option<tilo_actions::remote::RemoteActionMessage>,
    envelope: impl FnOnce(
        tilo_actions::remote::RemoteActionMessage,
    ) -> Option<(mpsc::Sender<Envelope>, Envelope)>
    + Send
    + 'static,
) {
    let runtime = tokio::runtime::Handle::current();
    reply.then(move |reply| {
        let Some(message) = reply.or(abandoned) else {
            return;
        };
        let Some((outbound, answer)) = envelope(message) else {
            return;
        };
        if let Err(mpsc::error::TrySendError::Full(answer)) = outbound.try_send(answer) {
            tracing::info!("an answer to a member waits for room in its connection's queue");
            runtime.spawn(async move {
                // A closed connection has no asker left to answer.
                let _ = outbound.send(answer).await;
            });
        }
    });
}

/// Answer a frame addressed to this relay through its app-serving plane,
/// with the caller the connection authenticated. The answer is written to
/// the connection whenever the plane has it, so a request that needs an
/// application's help never stalls this peer's other traffic. `Err` when
/// the frame cannot be used. The caller was already admitted for the room
/// by [`handle_remote_action`], agent delegation included.
fn serve_locally(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    plane: &dyn crate::app_serving::AppServingPlane,
    workspace_id: u64,
    frame: &[u8],
) -> Result<Option<Envelope>, (UnavailableReason, &'static str)> {
    use tilo_actions::remote::RemoteActionFrame;
    let request = RemoteActionFrame::decode(frame).map_err(|_| {
        (
            UnavailableReason::InvalidRequest,
            "malformed remote action frame",
        )
    })?;
    let caller = session_caller(peer, session)?;
    if !plane.answers(&caller, &request.message) {
        return Err((
            UnavailableReason::UnsupportedPeer,
            "this host does not serve apps",
        ));
    }
    // An action the host runs for a member (a comment, linking their own
    // account) changes a record as theirs: it spends their change budget,
    // as a change they send does.
    if matches!(
        request.message,
        tilo_actions::remote::RemoteActionMessage::Invoke(_)
    ) && super::over_change_limit(peer, session).is_some()
    {
        return Err((
            UnavailableReason::Backpressure,
            tilo_relay_protocol::refusal::TOO_MANY_CHANGES,
        ));
    }
    let abandoned = tilo_actions::remote::refuse(
        &request.message,
        tilo_actions::remote::ActionHostError::Unavailable {
            reason: "the host dropped the request".into(),
        },
    );
    let Some(reply) = plane.serve_certified(workspace_id, &caller, &peer.cert, request.message)
    else {
        // A response needs no answer; a request the plane leaves unanswered
        // is refused, so its asker does not wait for nothing.
        return if abandoned.is_some() {
            Err((
                UnavailableReason::UnsupportedPeer,
                "this host does not answer that request",
            ))
        } else {
            Ok(None)
        };
    };
    let outbound = session.outbound.clone();
    let to_device_public_key = peer.device_public_key;
    let from_device_public_key = *session.own_device_public_key;
    let from_user_public_key = *session.own_user_public_key;
    // The answer is all the asker waits for: one the plane drops is a
    // refusal, never nothing.
    answer_later(reply, abandoned, move |reply| {
        let frame = match RemoteActionFrame::new(reply).encode() {
            Ok(frame) => frame,
            Err(error) => {
                tracing::error!(
                    workspace_id,
                    %error,
                    "a remote action answer does not encode; the asker is not answered"
                );
                return None;
            }
        };
        let answer = Envelope::RemoteAction {
            workspace_id,
            version: crate::transport::REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key,
            from_device_public_key,
            from_user_public_key,
            frame,
        };
        Some((outbound, answer))
    });
    Ok(None)
}

/// Answer a remote action addressed to this relay through its app-serving
/// plane, with the caller the connection authenticated. The answer goes back
/// on the asker's private lane whenever the plane has it, so a request that
/// needs an application's help never stalls this peer's other traffic.
/// `Some` reason when the action cannot be served here. The caller was
/// already admitted for the room by [`route_private_signed`], agent
/// delegation included.
fn serve_private_locally(
    action: &DirectedAction,
    workspace_id: u64,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Option<UnavailableReason> {
    use UnavailableReason as U;
    let ActionMessage::Remote { message } = &action.message else {
        return Some(U::UnsupportedPeer);
    };
    let Some(plane) = session.app_serving else {
        return Some(U::UnsupportedPeer);
    };
    // An answer is this device's own, signed: a host that cannot sign one
    // could only ask the asker to trust the relay's word, which it never does.
    let Some(key) = session.own_device_signing_key else {
        return Some(U::UnsupportedPeer);
    };
    let caller = match session_caller(peer, session) {
        Ok(caller) => caller,
        Err((reason, _)) => return Some(reason),
    };
    if !plane.answers(&caller, message) {
        return Some(U::UnsupportedPeer);
    }
    // An action the host runs for a member (a comment, linking their own
    // account) changes a record as theirs: it spends their change budget,
    // as a change they send does.
    if matches!(
        **message,
        tilo_actions::remote::RemoteActionMessage::Invoke(_)
    ) && super::over_change_limit(peer, session).is_some()
    {
        return Some(U::Backpressure);
    }
    let reply = plane.serve_certified(workspace_id, &caller, &peer.cert, (**message).clone())?;
    let replies = session.private_tx.clone();
    let (room, request_id) = (action.room, action.request_id);
    let (target, source) = (peer.device_public_key, *session.own_device_public_key);
    let source_certificate = serde_json::to_vec(session.own_device_cert).ok()?;
    let key = key.clone();
    // The answer is all the asker waits for: it waits for room in the
    // lane's queue rather than being dropped. It ends with the connection.
    let runtime = tokio::runtime::Handle::current();
    reply.then(move |reply| {
        let Some(message) = reply else {
            return;
        };
        // This device answers as itself: its own signature travels with the
        // answer, so the asker verifies a caller and not a relay's word.
        let answer = DirectedAction {
            room,
            request_id,
            source,
            target,
            message: ActionMessage::Remote {
                message: Box::new(message),
            },
        };
        let Ok(signed) = SignedDirectedAction::sign(&answer, unix_now_ns(), &key) else {
            tracing::error!(
                workspace_id,
                "a host answer does not encode; the asker is not answered"
            );
            return;
        };
        let answer = PrivateOutbound::Reply(EnvelopeV2::CertifiedSignedDelivered {
            action: signed,
            source_certificate,
        });
        if let Err(mpsc::error::TrySendError::Full(answer)) = replies.try_send(answer) {
            tracing::info!(
                workspace_id,
                "an answer to a member waits for room in its private queue"
            );
            runtime.spawn(async move {
                let _ = replies.send(answer).await;
            });
        }
    });
    None
}

/// Who `peer` acts as: the person on their own device, or the agent or bot
/// whose live delegation that device carries.
fn session_caller(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<tilo_actions::remote::Caller, (UnavailableReason, &'static str)> {
    use tilo_actions::remote::Caller;
    let agent = session
        .agents
        .read_recover()
        .get(&peer.device_public_key)
        .cloned();
    Ok(match agent {
        Some(Some(delegation)) => match delegation.delegate {
            tilo_relay_protocol::delegation::Delegate::Agent { agent_id } => {
                Caller::delegated_agent(agent_id, peer.user_public_key, peer.device_public_key)
            }
            tilo_relay_protocol::delegation::Delegate::Bot { installation } => {
                Caller::bot(installation, peer.user_public_key, peer.device_public_key)
            }
        },
        Some(None) => {
            return Err((UnavailableReason::NotAuthorised, "the agent was withdrawn"));
        }
        None => Caller::authenticated(peer.user_public_key, peer.device_public_key),
    })
}

/// A private queue belongs to one authenticated state session and exact room.
#[derive(Clone)]
pub(crate) struct ActionTarget {
    pub(super) broker_host: Option<uuid::Uuid>,
    pub(super) broker_protocol: bool,
    pub(super) room: uuid::Uuid,
    pub(super) peer: AuthenticatedPeer,
    pub(super) state_tx: mpsc::Sender<Envelope>,
    pub(super) tx: mpsc::Sender<PrivateOutbound>,
}

pub(crate) enum PrivateOutbound {
    Reply(EnvelopeV2),
    Delivery(PrivateDelivery),
}

#[derive(Clone)]
pub(crate) struct PrivateDelivery {
    pub(super) action: DirectedAction,
    /// The source device's signature over `action`, forwarded unchanged.
    /// Boxed so a queued reply is not as large as a signed delivery.
    pub(super) signed: Box<SignedDirectedAction>,
    pub(super) source: AuthenticatedPeer,
    pub(super) source_tx: mpsc::Sender<Envelope>,
    pub(super) reply: mpsc::Sender<PrivateOutbound>,
    // Capture the originating session, never re-resolve by device key: that
    // key may already identify a newer replacement connection at delivery.
    pub(super) source_disconnect: mpsc::Sender<()>,
}

// The UUID is carried on an authenticated private lane. Select only a live
// subscription on its derived shard; never infer membership from a numeric alias.
async fn private_workspace_binding(
    room: uuid::Uuid,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<(u64, SharedWorkspace)>, PeerError> {
    let id = crate::room_workspace_id(&room.to_string())
        .map_err(|_| PeerError::Protocol("invalid room".into()))?;
    let Some(workspace) = session.workspaces.lock().await.get(&id).cloned() else {
        return Ok(None);
    };
    let runtime = workspace.lock().await;
    if runtime.store.workspace_id() != id || !current_session(&runtime, peer, session.outbound) {
        return Ok(None);
    }
    drop(runtime);
    Ok(Some((id, workspace)))
}

async fn private_workspace(
    room: uuid::Uuid,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<SharedWorkspace>, PeerError> {
    Ok(private_workspace_binding(room, peer, session)
        .await?
        .map(|(_, workspace)| workspace))
}

async fn private_membership<'a>(
    workspace: &'a SharedWorkspace,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    failure: &str,
) -> Result<tokio::sync::MutexGuard<'a, crate::workspace::WorkspaceRuntime>, PeerError> {
    let runtime = crate::workspace_io::fresh_membership(workspace).await?;
    if action_peer_admitted(&runtime, peer, session).is_err()
        || !current_session(&runtime, peer, session.outbound)
    {
        return Err(PeerError::Protocol(failure.into()));
    }
    Ok(runtime)
}

/// `EnableActions`: open this session's private lane in `room`, carrying
/// transport only until it negotiates a broker.
pub(super) async fn enable_actions(
    room: uuid::Uuid,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<EnvelopeV2>, PeerError> {
    if room.is_nil() || !current_action_peer(peer, session) {
        return Err(PeerError::Protocol(
            "private capability not authorised".into(),
        ));
    }
    let Some(ws) = private_workspace(room, peer, session).await? else {
        return Err(PeerError::Protocol("private room not subscribed".into()));
    };
    let mut wr = private_membership(&ws, peer, session, "private room not subscribed").await?;
    wr.action_targets.insert(
        peer.device_public_key,
        ActionTarget {
            broker_host: None,
            broker_protocol: false,
            room,
            peer: peer.clone(),
            state_tx: session.outbound.clone(),
            tx: session.private_tx.clone(),
        },
    );
    drop(wr);
    Ok(Some(EnvelopeV2::ActionsReady {
        room,
        capability: ActionCapability::TransportOnly,
    }))
}

/// `EnableBrokerClient`: this session's enabled lane in `room` speaks the
/// broker protocol extensions.
pub(super) async fn enable_broker_client(
    room: uuid::Uuid,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<EnvelopeV2>, PeerError> {
    if !current_action_peer(peer, session) {
        return Err(PeerError::Protocol("broker protocol not authorised".into()));
    }
    let ws = private_workspace(room, peer, session)
        .await?
        .ok_or_else(|| PeerError::Protocol("room not subscribed".into()))?;
    let mut wr = private_membership(&ws, peer, session, "broker protocol not authorised").await?;
    let target = wr
        .action_targets
        .get_mut(&peer.device_public_key)
        .filter(|t| t.room == room && t.state_tx.same_channel(session.outbound))
        .ok_or_else(|| PeerError::Protocol("private lane not enabled".into()))?;
    target.broker_protocol = true;
    drop(wr);
    Ok(None)
}

/// `RegisterBroker`: this session's enabled lane in `room` is the durable
/// broker `host`; invocations bound to that host reach it.
pub(super) async fn register_broker(
    room: uuid::Uuid,
    host: uuid::Uuid,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<EnvelopeV2>, PeerError> {
    if host.is_nil() || !current_action_peer(peer, session) {
        return Err(PeerError::Protocol("broker not authorised".into()));
    }
    let ws = private_workspace(room, peer, session)
        .await?
        .ok_or_else(|| PeerError::Protocol("room not subscribed".into()))?;
    let mut wr = private_membership(&ws, peer, session, "broker not authorised").await?;
    let target = wr
        .action_targets
        .get_mut(&peer.device_public_key)
        .filter(|t| t.room == room && t.state_tx.same_channel(session.outbound))
        .ok_or_else(|| PeerError::Protocol("private lane not enabled".into()))?;
    target.broker_host = Some(host);
    target.broker_protocol = true;
    drop(wr);
    Ok(Some(EnvelopeV2::ActionsReady {
        room,
        capability: ActionCapability::DurableBroker { host },
    }))
}

/// `SignedDirected`: route a directed action with its source's signature.
/// The relay forwards the source's signed bytes; it never signs. A
/// signature that is not this session's device's is refused here, so
/// it cannot reach (and close) the target's attachment.
pub(super) async fn route_signed_directed(
    signed: &SignedDirectedAction,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<EnvelopeV2>, PeerError> {
    let action = signed.action();
    if signed.verify(&peer.device_public_key).is_err() {
        private_phase(action, "route_rejected_signature");
        return Ok(Some(EnvelopeV2::unavailable(
            action,
            UnavailableReason::NotAuthorised,
        )));
    }
    let reason = route_private_signed(action, signed, peer, session).await?;
    Ok(reason.map(|reason| EnvelopeV2::unavailable(action, reason)))
}

fn private_phase(action: &DirectedAction, phase: &'static str) {
    private_workspace_phase(action, None, phase);
}

fn private_workspace_phase(
    action: &DirectedAction,
    workspace_id: Option<u64>,
    phase: &'static str,
) {
    if crate::persistence::diagnostics() {
        tracing::info!(
            request_id = %action.request_id,
            workspace_id = ?workspace_id,
            phase,
            timestamp_ns = unix_now_ns()
        );
    }
}

struct DeliveryAuthorizationTrace<'a> {
    action: &'a DirectedAction,
    workspace_id: Option<u64>,
    finished: bool,
}

impl<'a> DeliveryAuthorizationTrace<'a> {
    fn new(action: &'a DirectedAction) -> Self {
        Self {
            action,
            workspace_id: None,
            finished: false,
        }
    }

    fn select_workspace(&mut self, workspace_id: u64) {
        self.workspace_id = Some(workspace_id);
    }

    fn phase(&self, phase: &'static str) {
        private_workspace_phase(self.action, self.workspace_id, phase);
    }

    fn reject(
        &mut self,
        phase: &'static str,
        reason: UnavailableReason,
    ) -> Result<(), UnavailableReason> {
        self.phase(phase);
        self.finished = true;
        Err(reason)
    }

    fn success(&mut self) {
        self.phase("delivery_authorization_success");
        self.finished = true;
    }
}

impl Drop for DeliveryAuthorizationTrace<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.phase("delivery_authorization_cancelled");
        }
    }
}

/// Route `action`, carrying the source's verified signature over it.
pub(super) async fn route_private_signed(
    action: &DirectedAction,
    signed: &SignedDirectedAction,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<UnavailableReason>, PeerError> {
    use UnavailableReason as U;
    private_phase(action, "route_enter");
    if action.source != peer.device_public_key || !current_action_peer(peer, session) {
        private_phase(action, "route_rejected_source");
        return Ok(Some(U::NotAuthorised));
    }
    if !action.validate() {
        private_phase(action, "route_rejected_message");
        return Ok(Some(U::InvalidRequest));
    }
    if !action.wire_fits() {
        private_phase(action, "route_rejected_encoding");
        return Ok(Some(U::InvalidRequest));
    }
    let Some((workspace_id, ws)) = private_workspace_binding(action.room, peer, session).await?
    else {
        return Ok(Some(U::NotAuthorised));
    };
    test_point!(pause(room ws, "private-authorization"));
    let wr = crate::workspace_io::fresh_membership(&ws).await?;
    private_phase(action, "route_lock_acquired");
    if action_peer_admitted(&wr, peer, session).is_err()
        || !current_session(&wr, peer, session.outbound)
        || !wr
            .action_targets
            .get(&peer.device_public_key)
            .is_some_and(|target| {
                target.room == action.room && target.state_tx.same_channel(session.outbound)
            })
    {
        return Ok(Some(U::NotAuthorised));
    }
    if action.target == *session.own_device_public_key {
        // Never hold the room across the plane: it may take its time.
        drop(wr);
        return Ok(serve_private_locally(action, workspace_id, peer, session));
    }
    let Some(sub) = wr.subscribers.get(&action.target) else {
        return Ok(Some(unavailable_target(&wr, &action.target)));
    };
    let Some(target) = wr.action_targets.get(&action.target) else {
        return Ok(Some(U::UnsupportedPeer));
    };
    if action_peer_admitted(&wr, &target.peer, session).is_err()
        || target.room != action.room
        || !target.state_tx.same_channel(&sub.tx)
    {
        return Ok(Some(U::NotAuthorised));
    }
    // Invocations require the exact host identity the recipient advertised.
    if matches!(&action.message, ActionMessage::InvokeBound { host, .. } if target.broker_host != Some(*host))
    {
        return Ok(Some(U::AdmissionUnavailable));
    }
    let source_extended = wr
        .action_targets
        .get(&peer.device_public_key)
        .is_some_and(|t| t.broker_protocol);
    let extension = matches!(
        action.message,
        ActionMessage::InvokeBound { .. }
            | ActionMessage::CommittedDesktop { .. }
            | ActionMessage::BrokerCatalogue { .. }
            | ActionMessage::Observation { .. }
            | ActionMessage::Rejected { .. }
    );
    let remote = matches!(action.message, ActionMessage::Remote { .. });
    if !remote
        && ((target.broker_host.is_some() && !source_extended)
            || (extension && (!source_extended || !target.broker_protocol)))
    {
        return Ok(Some(U::UnsupportedPeer));
    }
    // Checked above under the same membership read; keep the same refusal.
    if !wr.action_targets.contains_key(&peer.device_public_key) {
        return Ok(Some(U::NotAuthorised));
    }
    private_phase(action, "route_authorized");
    let delivery = PrivateDelivery {
        action: action.clone(),
        signed: Box::new(signed.clone()),
        source: peer.clone(),
        source_tx: session.outbound.clone(),
        reply: session.private_tx.clone(),
        source_disconnect: session.disconnect.clone(),
    };
    let result = target
        .tx
        .try_send(PrivateOutbound::Delivery(delivery))
        .err()
        .map(|error| match error {
            mpsc::error::TrySendError::Full(_) => U::Backpressure,
            mpsc::error::TrySendError::Closed(_) => U::Offline,
        });
    drop(wr);
    if result.is_none() {
        private_phase(action, "route_enqueued");
    }
    Ok(result)
}

pub(super) async fn write_private_delivery<W: tokio::io::AsyncWrite + Unpin>(
    delivery: &PrivateDelivery,
    recipient: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    write: &mut W,
) -> Result<(), PeerError> {
    private_phase(&delivery.action, "delivery_authorization_enter");
    match authorize_delivery(delivery, recipient, session).await {
        Ok(()) => {
            let source_certificate = serde_json::to_vec(&delivery.source.cert)
                .map_err(|_| PeerError::Protocol("certificate encoding failed".into()))?;
            let frame = EnvelopeV2::CertifiedSignedDelivered {
                action: SignedDirectedAction::clone(&delivery.signed),
                source_certificate,
            };
            let Ok(bytes) = tilo_delivery_protocol::transport_v2::encode(&frame) else {
                private_phase(&delivery.action, "delivery_encoding_rejected");
                let _ = delivery.source_disconnect.try_send(());
                return Ok(());
            };
            private_phase(&delivery.action, "write_authorized");
            write.write_all(&bytes).await?;
            private_phase(&delivery.action, "write_complete");
        }
        Err(reason) => {
            let unavailable =
                delivery
                    .reply
                    .try_send(PrivateOutbound::Reply(EnvelopeV2::unavailable(
                        &delivery.action,
                        reason,
                    )));
            if unavailable.is_err() {
                private_phase(&delivery.action, "delivery_unavailable_queue_failed");
                // A bounded failure queue cannot silently erase the outcome.
                // Its originating session must observe transport loss instead.
                let _ = delivery.source_disconnect.try_send(());
            } else {
                private_phase(&delivery.action, "delivery_unavailable_queued");
            }
        }
    }
    Ok(())
}

pub(super) async fn authorize_delivery(
    delivery: &PrivateDelivery,
    recipient: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<(), UnavailableReason> {
    use UnavailableReason as U;
    let mut trace = DeliveryAuthorizationTrace::new(&delivery.action);
    if !current_action_peer(&delivery.source, session) {
        return trace.reject("delivery_rejected_source_cert_initial", U::NotAuthorised);
    }
    if !current_action_peer(recipient, session) {
        return trace.reject("delivery_rejected_recipient_cert_initial", U::NotAuthorised);
    }
    trace.phase("delivery_registry_wait");
    let (workspace_id, ws) =
        match private_workspace_binding(delivery.action.room, recipient, session).await {
            Ok(Some(binding)) => binding,
            Ok(None) => {
                return trace.reject("delivery_rejected_workspace_missing", U::NotAuthorised);
            }
            Err(_) => {
                return trace.reject("delivery_rejected_workspace_lookup", U::NotAuthorised);
            }
        };
    trace.select_workspace(workspace_id);
    trace.phase("delivery_registry_acquired");
    let Ok(wr) = crate::workspace_io::fresh_membership_for_delivery(
        &ws,
        workspace_id,
        delivery.action.request_id,
    )
    .await
    else {
        return trace.reject("delivery_rejected_membership_error", U::NotAuthorised);
    };
    if !current_action_peer(&delivery.source, session) {
        return trace.reject("delivery_rejected_source_cert_current", U::NotAuthorised);
    }
    if !current_action_peer(recipient, session) {
        return trace.reject("delivery_rejected_recipient_cert_current", U::NotAuthorised);
    }
    if !session.admits(&wr, &delivery.source, RoomOperation::RemoteAction) {
        return trace.reject("delivery_rejected_source_member", U::NotAuthorised);
    }
    if !session.admits(&wr, recipient, RoomOperation::RemoteAction) {
        return trace.reject("delivery_rejected_recipient_member", U::NotAuthorised);
    }
    if delivery.action.target != recipient.device_public_key {
        return trace.reject("delivery_rejected_target_mismatch", U::NotAuthorised);
    }
    if !current_session(&wr, &delivery.source, &delivery.source_tx) {
        return trace.reject("delivery_rejected_source_session", U::NotAuthorised);
    }
    if !wr
        .action_targets
        .get(&delivery.source.device_public_key)
        .is_some_and(|target| {
            target.room == delivery.action.room && target.state_tx.same_channel(&delivery.source_tx)
        })
    {
        return trace.reject("delivery_rejected_source_action_target", U::NotAuthorised);
    }
    if !current_session(&wr, recipient, session.outbound) {
        return trace.reject("delivery_rejected_recipient_session", U::NotAuthorised);
    }
    if !wr
        .action_targets
        .get(&recipient.device_public_key)
        .is_some_and(|target| {
            target.room == delivery.action.room && target.state_tx.same_channel(session.outbound)
        })
    {
        return trace.reject(
            "delivery_rejected_recipient_action_target",
            U::NotAuthorised,
        );
    }
    // Both were checked above under the same membership read.
    if !wr
        .action_targets
        .contains_key(&delivery.source.device_public_key)
    {
        return trace.reject("delivery_rejected_source_action_target", U::NotAuthorised);
    }
    drop(wr);
    trace.success();
    Ok(())
}
