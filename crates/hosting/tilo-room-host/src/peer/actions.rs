//! Remote actions on the v1 `RemoteAction` envelope: routing to another
//! subscribed device, and the app-serving plane for frames addressed to this
//! relay.

use super::{
    AuthenticatedPeer, Envelope, SessionCtx, UnavailableReason, action_peer_admitted,
    current_session, mpsc, unavailable_target,
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
/// with [`action_peer_admitted`]: the connection-local `subscribed` list alone
/// goes stale the moment a device is removed, revoked, or its agent
/// delegation lapses.
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
    // A browser environment's device answers the page actions the relay hands
    // it and originates none: what it holds is a window, not a member's
    // standing to call the workspace's doors.
    if session.environment(&peer.device_public_key).is_some()
        && matches!(
            tilo_actions::remote::RemoteActionFrame::decode(&frame),
            Ok(tilo_actions::remote::RemoteActionFrame {
                message: tilo_actions::remote::RemoteActionMessage::Invoke(_),
                ..
            })
        )
    {
        return refuse(
            U::NotAuthorised,
            "a browser environment's device answers page actions, it does not call actions",
        );
    }
    if to == *session.own_device_public_key {
        // An action of a window this host serves but does not hold (a Browser
        // whose page runs on a VM host) is handed to the device that holds it,
        // stamped with this sender, or refused as the plane says.
        if let Some(plane) = session.app_serving
            && let Ok(decoded) = tilo_actions::remote::RemoteActionFrame::decode(&frame)
            && let Some(route) = plane.route(workspace_id, peer.device_public_key, &decoded.message)
        {
            match route {
                crate::app_serving::Routing::Device(holder) => {
                    return hand_over(peer, session, &mut wr, workspace_id, version, holder, frame);
                }
                crate::app_serving::Routing::Refused(error) => {
                    return refused_answer(peer, session, workspace_id, version, &decoded, error);
                }
            }
        }
        let standing = super::change_standing(peer, &wr);
        // Never hold the workspace across the plane: it may take its time.
        drop(wr);
        let Some(plane) = session.app_serving else {
            return refuse(U::UnsupportedPeer, "this host does not serve apps");
        };
        let local = Local {
            plane: plane.as_ref(),
            workspace_id,
            standing,
        };
        return serve_locally(peer, session, &local, &frame)
            .unwrap_or_else(|(reason, detail)| refuse(reason, &detail));
    }
    hand_over(peer, session, &mut wr, workspace_id, version, to, frame)
}

/// Hand `frame` to the connected device `to` of the workspace, stamped with
/// the authenticated `peer` as its sender: the relay routes what it forwards,
/// and never runs it.
fn hand_over(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    wr: &mut tokio::sync::MutexGuard<'_, crate::workspace::WorkspaceRuntime>,
    workspace_id: u64,
    version: u16,
    to: [u8; 32],
    frame: Vec<u8>,
) -> Option<Envelope> {
    use UnavailableReason as U;
    let refuse = |reason: UnavailableReason, detail: &str| {
        Some(Envelope::RemoteActionUndeliverable {
            workspace_id,
            to_device_public_key: to,
            reason: format!("{}: {detail}", reason.wire_token()),
        })
    };
    // Every production subscription registers its authenticated peer beside
    // its subscriber, so the addressee is re-authorised from it.
    let Some(target) = wr.state_sessions.get(&to) else {
        return refuse(
            unavailable_target(wr, &to),
            "addressed device is not connected to the workspace",
        );
    };
    if action_peer_admitted(wr, &target.peer, session).is_err() {
        return refuse(
            U::NotAuthorised,
            "addressed device may not act in the workspace",
        );
    }
    if let Some(plane) = session.app_serving
        && plane.tracks(workspace_id)
        && let Ok(decoded) = tilo_actions::remote::RemoteActionFrame::decode(&frame)
    {
        plane.relayed(workspace_id, peer.device_public_key, &decoded.message);
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
    if delivered {
        None
    } else {
        refuse(
            U::Offline,
            "addressed device is not connected to the workspace",
        )
    }
}

/// Answer `request` to `peer` as refused with `error`: what it asked for is
/// not served for it now, and it is told so in the answer its request takes.
fn refused_answer(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    workspace_id: u64,
    version: u16,
    request: &tilo_actions::remote::RemoteActionFrame,
    error: tilo_actions::remote::ActionHostError,
) -> Option<Envelope> {
    let answer = tilo_actions::remote::refuse(&request.message, error)?;
    let frame = tilo_actions::remote::RemoteActionFrame::new(answer)
        .encode()
        .ok()?;
    Some(Envelope::RemoteAction {
        workspace_id,
        version,
        to_device_public_key: peer.device_public_key,
        from_device_public_key: *session.own_device_public_key,
        from_user_public_key: *session.own_user_public_key,
        frame,
    })
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

/// Where [`serve_locally`] answers: the relay's app-serving plane, in one
/// workspace, for a caller of the standing the change budget sees there.
struct Local<'a> {
    plane: &'a dyn crate::app_serving::AppServingPlane,
    workspace_id: u64,
    standing: crate::member_limits::Standing,
}

/// Answer a frame addressed to this relay through its app-serving plane,
/// with the caller the connection authenticated. The answer is written to
/// the connection whenever the plane has it, so a request that needs an
/// application's help never stalls this peer's other traffic. `Err` when
/// the frame cannot be used. The caller was already admitted for the workspace
/// by [`handle_remote_action`], agent delegation included.
fn serve_locally(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    local: &Local<'_>,
    frame: &[u8],
) -> Result<Option<Envelope>, (UnavailableReason, String)> {
    use tilo_actions::remote::RemoteActionFrame;
    let Local {
        plane,
        workspace_id,
        standing,
    } = *local;
    let request = RemoteActionFrame::decode(frame).map_err(|_| {
        (
            UnavailableReason::InvalidRequest,
            "malformed remote action frame".to_owned(),
        )
    })?;
    let caller =
        session_caller(peer, session).map_err(|(reason, detail)| (reason, detail.to_owned()))?;
    if !plane.answers(&caller, &request.message) {
        return Err((
            UnavailableReason::UnsupportedPeer,
            "this host does not serve apps".to_owned(),
        ));
    }
    // An action the host runs for a member (a comment, linking their own
    // account) changes a record as theirs: it spends their change budget,
    // as a change they send does. It is refused before it runs, saying when
    // the budget admits the next one, so the asker may simply ask again.
    if matches!(
        request.message,
        tilo_actions::remote::RemoteActionMessage::Invoke(_)
    ) && let Some(crate::member_limits::Limited::Changes { retry_after }) =
        super::over_change_limit(peer, session, standing)
    {
        return Err((
            UnavailableReason::Backpressure,
            tilo_relay_protocol::refusal::too_many_changes(retry_after),
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
                "this host does not answer that request".to_owned(),
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

/// Who `peer` acts as: the person on their own device, the agent that
/// device handed this session to ([`SessionCtx::agent`]), or the agent or bot
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
    let caller = match agent {
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
        // An agent's grant (a key its person certified for a service acting
        // for an agent they approved) is their agent: what it does is the
        // agent's, for them, never above what an agent of theirs may do, and
        // it governs nothing. It goes by the name the key was certified with.
        None if peer.cert.capabilities.agent.is_some() => {
            Caller::authenticated(peer.user_public_key, peer.device_public_key)
                .delegated_to(grant_agent_id(&peer.device_public_key))
                .named(&peer.cert.device_label)
        }
        None => {
            let person = Caller::authenticated(peer.user_public_key, peer.device_public_key);
            match session.agent.get() {
                Some(agent) => person.delegated_to(agent.clone()),
                None => person,
            }
        }
    };
    // An agent's (or a key's) requests on this connection are one run, the
    // host's to name.
    Ok(match caller.principal() {
        tilo_actions::remote::CallerPrincipal::Agent(_) => {
            caller.in_execution(session.execution.clone())
        }
        _ => caller,
    })
}

/// The agent an agent's grant is, by its key: History names its runs.
fn grant_agent_id(device: &[u8; 32]) -> String {
    format!("grant-{}", hex::encode(device))
}
