//! Remote actions on the v1 `RemoteAction` envelope: routing to another
//! subscribed device, and the app action server for frames addressed to this
//! server.

use super::{
    AuthenticatedPeer, Envelope, Refusal, SessionCtx, action_peer_admitted, current_session, mpsc,
    unavailable_target,
};
use tilo_lock::RwLockRecover as _;
use tilo_server_protocol::WorkspaceId;

/// Where an inbound [`Envelope::RemoteAction`] asked to go.
pub(super) struct RemoteActionRoute {
    pub(super) workspace_id: WorkspaceId,
    pub(super) version: u16,
    pub(super) to: [u8; 32],
}

/// Handle an inbound `RemoteAction`: stamp the authenticated sender, then
/// hand it to the addressed device's live connection, or — when addressed to
/// this server — to the app action server. Never stored, never executed by the
/// server itself; every failure is answered as undeliverable so the sender
/// need not wait out a timeout, with its typed [`Refusal`].
///
/// Both ends are re-authorised on every frame under a fresh membership read
/// with [`action_peer_admitted`]: the connection-local `subscribed` list alone
/// goes stale the moment a device is removed, revoked, or its agent
/// delegation lapses.
pub(super) async fn handle_remote_action(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
    route: RemoteActionRoute,
    frame: Vec<u8>,
) -> Option<Envelope> {
    let RemoteActionRoute {
        workspace_id,
        version,
        to,
    } = route;
    let refuse = |refusal: Refusal| {
        Some(Envelope::RemoteActionUndeliverable {
            workspace_id,
            to_device_public_key: to,
            refusal,
        })
    };
    if version != crate::transport::REMOTE_ACTION_ENVELOPE_VERSION {
        return refuse(Refusal::Invalid(
            "unsupported remote action envelope version".into(),
        ));
    }
    // An action asked before the session subscribed, or before the
    // workspace is open here, goes through once the host says it is ready.
    if !subscribed.contains(&workspace_id) {
        return refuse(Refusal::NotReady);
    }
    if frame.len() > tilo_actions::remote::MAX_REMOTE_ACTION_FRAME_BYTES {
        return refuse(Refusal::Invalid(
            "remote action frame exceeds the limit".into(),
        ));
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return refuse(Refusal::NotReady);
    };
    let Ok(mut wr) = crate::workspace_io::fresh_membership(&ws).await else {
        return refuse(Refusal::NotReady);
    };
    if !current_session(&wr, peer, session.outbound) {
        // The workspace dropped this connection's subscription (its device
        // revoked or removed): its audit ends it.
        return refuse(not_a_member(
            &wr,
            peer,
            "sender may not act in the workspace",
        ));
    }
    if action_peer_admitted(&wr, peer, session).is_err() {
        return refuse(not_a_member(
            &wr,
            peer,
            "sender may not act in the workspace",
        ));
    }
    // A browser environment's device answers the page actions the server hands
    // it and originates none: what it holds is a window, not the right of a
    // member to call the workspace's action APIs. What it asks of this host about
    // itself — its own streams' media keys, its own windows' shares — is the
    // one exception, and this host answers it only for that device.
    if session.environment(&peer.device_public_key).is_some()
        && matches!(
            tilo_actions::remote::RemoteActionFrame::decode(&frame),
            Ok(tilo_actions::remote::RemoteActionFrame {
                message: tilo_actions::remote::RemoteActionMessage::Invoke(request),
                ..
            }) if to != *session.own_device_public_key
                || tilo_actions::remote::environment_device::action_of(&request.target).is_none()
        )
    {
        return refuse(Refusal::NotAllowed(
            "a browser environment's device answers page actions, it does not call actions".into(),
        ));
    }
    if to == *session.own_device_public_key {
        // What this host runs for someone it runs as the key that signed it.
        let invoker = match signed_invoker(peer, session, &mut wr, workspace_id, &frame) {
            Ok(invoker) => invoker,
            Err(NotRun::Refused(refusal)) => return refuse(refusal),
            Err(NotRun::Conflict(invocation)) => {
                return conflict(session, peer, (workspace_id, version), invocation);
            }
        };
        let peer = &invoker;
        // An action of a window this host serves but does not hold (a Browser
        // whose page runs on a VM host) is handed to the device that holds it,
        // stamped with this sender, or answered by the app action server itself, which may
        // take its time about it.
        if let Some(server) = session.app_serving
            && let Ok(decoded) = tilo_actions::remote::RemoteActionFrame::decode(&frame)
            && let Ok(caller) = session_caller(peer, session)
                .map(|caller| caller.in_named_session(&decoded.message))
            && let Some(route) =
                server.route_certified(workspace_id, &caller, &peer.cert, &decoded.message)
        {
            return hand_routed(
                peer,
                session,
                &mut wr,
                (workspace_id, version),
                (&decoded, frame),
                route,
            );
        }
        // Never hold the workspace across the app action server: it may take its time.
        drop(wr);
        let Some(server) = session.app_serving else {
            return refuse(Refusal::Other("this host does not serve apps".into()));
        };
        let local = Local {
            server: server.as_ref(),
            workspace_id,
        };
        return serve_locally(peer, session, &local, &frame).unwrap_or_else(refuse);
    }
    hand_over(peer, session, &mut wr, workspace_id, version, to, frame)
}

/// Write the answer the app action server routed a request to, on the connection of the
/// device that asked, whenever the app action server has it: an answer it drops is the
/// refusal the request's kind takes. `None`: nothing is returned to the
/// connection handler, which is how a later answer is written.
fn answered_by_server(
    session: &SessionCtx<'_>,
    peer: &AuthenticatedPeer,
    workspace_id: WorkspaceId,
    version: u16,
    request: &tilo_actions::remote::RemoteActionFrame,
    answer: tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>,
) -> Option<Envelope> {
    let abandoned = tilo_actions::remote::refuse(
        &request.message,
        tilo_actions::remote::ActionCallError::Unavailable {
            reason: "the host dropped the request".into(),
        },
    );
    let outbound = session.outbound.clone();
    let to_device_public_key = peer.device_public_key;
    let from_device_public_key = *session.own_device_public_key;
    let from_user_public_key = *session.own_user_public_key;
    answer_later(answer, abandoned, move |answer| {
        let frame = tilo_actions::remote::RemoteActionFrame::new(answer)
            .encode()
            .ok()?;
        Some((
            outbound,
            Envelope::RemoteAction {
                workspace_id,
                version,
                to_device_public_key,
                from_device_public_key,
                from_user_public_key,
                frame,
            },
        ))
    });
    None
}

/// Why [`signed_invoker`] runs nothing for a frame.
enum NotRun {
    /// The frame is refused, undeliverable.
    Refused(Refusal),
    /// Its invocation id names another request this host took within the
    /// freshness window: answered as the invocation log answers one, a
    /// conflict.
    Conflict(tilo_actions::InvocationId),
}

impl From<Refusal> for NotRun {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

/// The answer to `invocation`, signed for another request than the one this host took
/// under its id: rejected as a conflict, on the connection that asked.
fn conflict(
    session: &SessionCtx<'_>,
    peer: &AuthenticatedPeer,
    (workspace_id, version): (WorkspaceId, u16),
    invocation: tilo_actions::InvocationId,
) -> Option<Envelope> {
    use tilo_actions::remote::{
        ActionCallError, InvocationReceipt, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    };
    let receipt = RemoteActionMessage::Receipt(InvocationReceipt {
        invocation,
        status: ReceiptStatus::Rejected {
            error: ActionCallError::InvocationConflict,
        },
    });
    let frame = RemoteActionFrame::new(receipt).encode().ok()?;
    Some(Envelope::RemoteAction {
        workspace_id,
        version,
        to_device_public_key: peer.device_public_key,
        from_device_public_key: *session.own_device_public_key,
        from_user_public_key: *session.own_user_public_key,
        frame,
    })
}

/// Hand `frame` to the connected device `to` of the workspace, stamped with
/// the authenticated `peer` as its sender: the server routes what it forwards,
/// and never runs it.
/// Who an invocation `frame` carries for this host is run as: its verified
/// signer, the very device that sent it. The signature, the certificate chain,
/// its revocation, a grant's reach, the issue time and the invocation id
/// ([`tilo_server_protocol::invocation::verify_invocation`]) are checked, then
/// the signer is admitted to the workspace under a fresh membership read as
/// any acting device is ([`action_peer_admitted`]). A frame that carries no
/// invocation (an observation, a cancellation, an answer) is the sender's:
/// those name an invocation the signer started, which the provider holds to
/// its caller.
///
/// # Errors
///
/// The refusal of an unsigned or wrongly signed invocation, of one signed by
/// another key than the sender's, and of a signer the workspace does not
/// admit; a conflict for an invocation id this host took for another request.
fn signed_invoker(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    wr: &mut crate::workspace::WorkspaceRuntime,
    workspace_id: WorkspaceId,
    frame: &[u8],
) -> Result<AuthenticatedPeer, NotRun> {
    use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
    use tilo_server_protocol::invocation::{InvocationError, verify_invocation};
    // A frame that does not decode is refused where it is served.
    let Ok(RemoteActionFrame {
        message: RemoteActionMessage::Invoke(request),
        ..
    }) = RemoteActionFrame::decode(frame)
    else {
        return Ok(peer.clone());
    };
    let now = tilo_clock::unix_now_ns();
    let refused = |error: InvocationError| match error {
        InvocationError::Revoked => Refusal::NotAMember(crate::workspace::key_revoked_reason()),
        other => Refusal::NotAllowed(format!("{}: {other}", other.code())),
    };
    let cert = {
        let revocations = session
            .revocations
            .read()
            .map_err(|_| Refusal::Other("this host cannot read its revocations".into()))?;
        verify_invocation(&workspace_id.to_string(), &request, &revocations, now)
            .map_err(refused)?
    };
    // The answer goes to the connection that asked: a key that did not sign
    // the request is never answered what the signer may read. The one other
    // key a device carries requests for is that of the agent whose run its
    // session is: the key its person made for that agent alone, which the
    // request is the agent's by.
    let agent_signed = cert.device_public_key != peer.device_public_key
        && cert.user_public_key == peer.user_public_key
        && session.agent.get().is_some_and(|agent| {
            let agent = tilo_actions::remote::device_agent_id(&peer.user_public_key, agent);
            session.revocations.read().is_ok_and(|revocations| {
                tilo_server_protocol::invocation::verify_agent_invocation(
                    &workspace_id.to_string(),
                    &request,
                    (&peer.user_public_key, &agent, &cert.device_public_key),
                    &revocations,
                    now,
                )
                .is_ok()
            })
        });
    if !agent_signed
        && (cert.device_public_key != peer.device_public_key
            || cert.user_public_key != peer.user_public_key)
    {
        return Err(Refusal::NotAllowed(
            "the invocation is signed by another key than the device asking".into(),
        )
        .into());
    }
    // An agent's request is admitted as the device that carries it, and runs as
    // the agent whose key signed it ([`session_caller`]).
    let invoker = AuthenticatedPeer {
        user_public_key: cert.user_public_key,
        device_public_key: peer.device_public_key,
        cert,
    };
    session
        .trust()
        .cert(&invoker, now)
        .map_err(|_| not_a_member(wr, &invoker, "the signer may not act in the workspace"))?;
    action_peer_admitted(wr, &invoker, session)
        .map_err(|_| not_a_member(wr, &invoker, "the signer may not act in the workspace"))?;
    if let Some(signed) = &request.signed {
        wr.invocations
            .admit(signed, now)
            .map_err(|error| match error {
                InvocationError::Replayed => NotRun::Conflict(request.invocation),
                other => NotRun::Refused(refused(other)),
            })?;
    }
    Ok(invoker)
}

/// Where an app action server routed the frame of `decoded`'s — `frame`, as the caller
/// wrote it — in `workspace_id`: handed to the device holding the window, as
/// the caller wrote it or as the app action server wrote it in the caller's place, or
/// answered here.
fn hand_routed(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    wr: &mut tokio::sync::MutexGuard<'_, crate::workspace::WorkspaceRuntime>,
    (workspace_id, version): (WorkspaceId, u16),
    (decoded, frame): (&tilo_actions::remote::RemoteActionFrame, Vec<u8>),
    route: crate::app_serving::Routing,
) -> Option<Envelope> {
    let handed = match route {
        crate::app_serving::Routing::Device(holder) => {
            hand_over(peer, session, wr, workspace_id, version, holder, frame)
        }
        crate::app_serving::Routing::Replaced { to, message } => {
            hand_over_replaced(peer, session, wr, (workspace_id, version), to, *message)
        }
        crate::app_serving::Routing::Answer(answer) => {
            return answered_by_server(session, peer, workspace_id, version, decoded, answer);
        }
    };
    // The caller asked this host: a frame the device holding the window could
    // not take is undeliverable from this host, the device it addressed, or
    // the caller waits for an answer that never names it.
    handed.map(|refused| match refused {
        Envelope::RemoteActionUndeliverable {
            workspace_id,
            refusal,
            ..
        } => Envelope::RemoteActionUndeliverable {
            workspace_id,
            to_device_public_key: *session.own_device_public_key,
            refusal,
        },
        other => other,
    })
}

/// [`hand_over`] of the frame `message` writes in place of the caller's: what
/// an app action server that answers for the person hands the window.
///
/// # Errors
///
/// Never: a frame that does not encode is refused as undeliverable.
fn hand_over_replaced(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    wr: &mut tokio::sync::MutexGuard<'_, crate::workspace::WorkspaceRuntime>,
    (workspace_id, version): (WorkspaceId, u16),
    to: [u8; 32],
    message: tilo_actions::remote::RemoteActionMessage,
) -> Option<Envelope> {
    match tilo_actions::remote::RemoteActionFrame::new(message).encode() {
        Ok(frame) => hand_over(peer, session, wr, workspace_id, version, to, frame),
        Err(_) => Some(Envelope::RemoteActionUndeliverable {
            workspace_id,
            to_device_public_key: to,
            refusal: Refusal::Invalid("the host could not write the frame".into()),
        }),
    }
}

/// The refusal of `peer`, which `wr` no longer admits: a key its person revoked is told so, with
/// the one command that approves another, so every caller knows what to do.
fn not_a_member(
    wr: &crate::workspace::WorkspaceRuntime,
    peer: &AuthenticatedPeer,
    reason: &str,
) -> Refusal {
    if wr.revoked_key(&peer.device_public_key) {
        Refusal::NotAMember(crate::workspace::key_revoked_reason())
    } else {
        Refusal::NotAMember(reason.into())
    }
}

fn hand_over(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    wr: &mut tokio::sync::MutexGuard<'_, crate::workspace::WorkspaceRuntime>,
    workspace_id: WorkspaceId,
    version: u16,
    to: [u8; 32],
    frame: Vec<u8>,
) -> Option<Envelope> {
    let refuse = |refusal: Refusal| {
        Some(Envelope::RemoteActionUndeliverable {
            workspace_id,
            to_device_public_key: to,
            refusal,
        })
    };
    // Every production subscription registers its authenticated peer beside
    // its subscriber, so the addressee is re-authorised from it.
    let Some(target) = wr.subscriber(&to) else {
        return refuse(unavailable_target(wr, &to));
    };
    if action_peer_admitted(wr, &target.peer, session).is_err() {
        return refuse(Refusal::NotAMember(
            "addressed device may not act in the workspace".into(),
        ));
    }
    let mut frame = frame;
    if let Some(server) = session.app_serving
        && server.tracks(workspace_id)
        && let Ok(decoded) = tilo_actions::remote::RemoteActionFrame::decode(&frame)
        && let Some(answer) = server.relayed(workspace_id, peer.device_public_key, &decoded.message)
        && let Ok(encoded) = tilo_actions::remote::RemoteActionFrame::new(answer).encode()
    {
        frame = encoded;
    }
    // What a device answers a caller whose frame this host handed it (a
    // Browser window in the workspace's VM) reaches that caller as this
    // host's: the caller asked this host, and hears its answer from the
    // device it asked. Who ran what stays the window's own record of it, as
    // the caller's.
    let (from_device_public_key, from_user_public_key) = if let Some(server) = session.app_serving
        && server.answers_for_host(workspace_id, peer.device_public_key, to)
    {
        (*session.own_device_public_key, *session.own_user_public_key)
    } else {
        (peer.device_public_key, peer.user_public_key)
    };
    let stamped = Envelope::RemoteAction {
        workspace_id,
        version,
        to_device_public_key: to,
        from_device_public_key,
        from_user_public_key,
        frame,
    };
    let delivered = wr.route_directed(&to, stamped);
    if delivered {
        None
    } else {
        refuse(Refusal::Offline)
    }
}

/// Handle an inbound `HostAction`: one of this host's own operations
/// (`tilo.host`), asked over the caller's authenticated connection with no
/// workspace. The host's governance decides who may, as for every action API:
/// only the devices of the host's own person, speaking for themselves.
///
/// Every request that can be read is answered: a refusal comes back at once
/// as the returned envelope, and an answer the app action server abandons comes back as a
/// refusal ([`answer_later`]). A frame too large to read or that does not
/// decode names no request to answer; it is logged.
pub(super) fn handle_host_action(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    frame: &[u8],
) -> Option<Envelope> {
    use tilo_actions::remote::{ActionCallError, RemoteActionFrame, refuse};
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
    let refused = |error: ActionCallError| host_action_envelope(refuse(&request.message, error)?);
    // A host whose work setting takes other people's workspaces is handed
    // one by its governing host, whose person nothing here trusts yet: that
    // one operation is asked of it, and the setting decides
    // ([`crate::workspaces`]). Every other one needs a device the host trusts.
    match session.trust().cert(peer, tilo_clock::unix_now_ns()) {
        Ok(()) => {}
        Err(crate::workspace_access::DenyReason::NotTrusted) if asks_to_hold(&request.message) => {}
        // A person nobody here let in yet is told so, and what makes them known: not that some
        // action is not theirs.
        Err(crate::workspace_access::DenyReason::NotTrusted) => {
            return refused(tilo_actions::remote::not_on_this_server(
                &peer.user_public_key,
            ));
        }
        Err(_) => return refused(ActionCallError::NotPermitted),
    }
    let Some(server) = session.app_serving else {
        return refused(ActionCallError::Unavailable {
            reason: "this host serves no host operations".into(),
        });
    };
    let caller = match session_caller(peer, session) {
        Ok(caller) => caller,
        Err(refusal) => {
            return refused(ActionCallError::Refused {
                reason: refusal.to_string(),
            });
        }
    };
    let unanswered = refuse(
        &request.message,
        ActionCallError::Unavailable {
            reason: "this host answers no such operation".into(),
        },
    );
    let abandoned = refuse(
        &request.message,
        ActionCallError::Unavailable {
            reason: "the host dropped the request".into(),
        },
    );
    let caller = caller.in_named_session(&request.message);
    let Some(reply) = server.serve_host(&caller, &peer.cert, request.message) else {
        // `None` for a response (which needs no answer) or an operation the
        // app action server does not serve, which is refused.
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
/// an answer the app action server drops is `abandoned` instead (a refusal, or `None` for
/// a message that needed no answer). The answer waits for workspace in the
/// connection's queue rather than being dropped when the workspace's own traffic
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

/// Where [`serve_locally`] answers: the server's app action server, in one
/// workspace.
struct Local<'a> {
    server: &'a dyn crate::app_serving::AppActionServer,
    workspace_id: WorkspaceId,
}

/// Answer a frame addressed to this server through its app action server,
/// with the caller the connection authenticated. The answer is written to
/// the connection whenever the app action server has it, so a request that needs an
/// application's help never stalls this peer's other traffic. `Err` when
/// the frame cannot be used. The caller was already admitted for the workspace
/// by [`handle_remote_action`], agent delegation included.
fn serve_locally(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    local: &Local<'_>,
    frame: &[u8],
) -> Result<Option<Envelope>, Refusal> {
    use tilo_actions::remote::RemoteActionFrame;
    let Local {
        server,
        workspace_id,
    } = *local;
    let request = RemoteActionFrame::decode(frame)
        .map_err(|_| Refusal::Invalid("malformed remote action frame".into()))?;
    let caller = session_caller(peer, session)?.in_named_session(&request.message);
    if !server.answers(&caller, &request.message) {
        return Err(Refusal::Other("this host does not serve apps".into()));
    }
    let abandoned = tilo_actions::remote::refuse(
        &request.message,
        tilo_actions::remote::ActionCallError::Unavailable {
            reason: "the host dropped the request".into(),
        },
    );
    let Some(reply) = server.serve_certified(workspace_id, &caller, &peer.cert, request.message)
    else {
        // A response needs no answer; a request the app action server leaves unanswered
        // is refused, so its asker does not wait for nothing.
        return if abandoned.is_some() {
            Err(Refusal::Other(
                "this host does not answer that request".into(),
            ))
        } else {
            Ok(None)
        };
    };
    let outbound = session.outbound.clone();
    let to_device_public_key = peer.device_public_key;
    let from_device_public_key = *session.own_device_public_key;
    let from_user_public_key = *session.own_user_public_key;
    // The answer is all the asker waits for: one the app action server drops is a
    // refusal, never nothing.
    answer_later(reply, abandoned, move |reply| {
        let frame = match RemoteActionFrame::new(reply).encode() {
            Ok(frame) => frame,
            Err(error) => {
                tracing::error!(
                    workspace_id = %workspace_id,
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

/// Whether `message` asks this host to hold a workspace another host governs.
fn asks_to_hold(message: &tilo_actions::remote::RemoteActionMessage) -> bool {
    matches!(
        message,
        tilo_actions::remote::RemoteActionMessage::Invoke(invoke)
            if invoke.target.provider == tilo_server_session::workspaces::HOST_ACTIONS_PROVIDER
                && invoke.target.address.action
                    == tilo_server_session::workspaces::HOST_WORKSPACE
    )
}

/// Who `peer` acts as: the person on their own device, the agent whose run
/// this session is ([`SessionCtx::agent`]) where its own key signed the
/// request, or the agent or bot whose live delegation that device carries.
pub(super) fn session_caller(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<tilo_actions::remote::Caller, Refusal> {
    use tilo_actions::remote::Caller;
    let agent = session
        .agents
        .read_recover()
        .get(&peer.device_public_key)
        .cloned();
    let caller = match agent {
        Some(Some(delegation)) => match delegation.delegate {
            // An agent goes by the name its person approved it under: what History and the
            // questions put to its person name it by.
            tilo_server_protocol::delegation::Delegate::Agent { agent_id } => {
                Caller::delegated_agent(agent_id, peer.user_public_key, peer.device_public_key)
                    .named(&delegation.label)
            }
            tilo_server_protocol::delegation::Delegate::Bot { installation } => {
                Caller::bot(installation, peer.user_public_key, peer.device_public_key)
            }
        },
        Some(None) => {
            return Err(Refusal::NotAMember("the agent was withdrawn".into()));
        }
        // The agent whose run this session is signed the request with its own
        // key ([`signed_invoker`] checked it): one agent of the person's under its
        // name, on every computer of theirs, by the id their workspaces keep it by.
        None if peer.cert.device_public_key != peer.device_public_key => {
            let Some(agent) = session.agent.get() else {
                return Err(Refusal::NotAllowed(
                    "the invocation is signed by another key than the device asking".into(),
                ));
            };
            Caller::delegated_agent(
                tilo_actions::remote::device_agent_id(&peer.user_public_key, agent),
                peer.user_public_key,
                peer.cert.device_public_key,
            )
            .named(agent)
        }
        // An agent's grant (a key its person certified for a service acting
        // for an agent they approved) is their agent: what it does is the
        // agent's, for them, never above what an agent of theirs may do, and
        // it governs nothing. It goes by the name of what holds the key (an API
        // key by the name its person gave it), not by the service it came through.
        None if peer.cert.capabilities.agent.is_some() => Caller::delegated_agent(
            collab_types::workspace_grant::key_agent_id(&peer.device_public_key),
            peer.user_public_key,
            peer.device_public_key,
        )
        .named(tilo_server_client::grant_holder(&peer.cert.device_label))
        .held_to(Vec::new()),
        None => Caller::authenticated(peer.user_public_key, peer.device_public_key),
    };
    // An agent's (or a key's) requests on this connection are one run, the
    // host's to name.
    Ok(if caller.acting().is_agent() {
        caller.in_execution(session.execution.clone())
    } else {
        caller
    })
}
