//! Ephemeral traffic the relay never keeps: the live plane's membership,
//! presence, voice calls and record content handed to one device. Presence
//! is every device's own, browser pages included.

use super::{
    Arc, AuthenticatedPeer, Decision, Envelope, PeerError, SessionCtx, WorkspaceOperation,
};

/// Handle `RetiredLiveJoin`: add a subscribed device to the workspace's live
/// plane and send it one `LiveMember` per member already present.
pub(super) async fn handle_live_join(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    workspace_id: u64,
) -> Result<Option<Envelope>, PeerError> {
    const UNSUBSCRIBED: &str = "live join requires a subscription";
    let refused = |reason: &str| {
        Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: reason.into(),
        }))
    };
    if !subscribed.contains(&workspace_id) {
        match subscription_refusal(session, peer, workspace_id).await? {
            Some(reason) => return refused(reason),
            None => return refused(UNSUBSCRIBED),
        }
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return refused(UNSUBSCRIBED);
    };
    let members = ws
        .lock()
        .await
        .live_join(peer.device_public_key, workspace_id);
    let Some(members) = members else {
        return refused(UNSUBSCRIBED);
    };
    for device in members {
        // Live state is never authority: a member notice lost to a full
        // queue costs only a late cursor.
        let _ = session.outbound.try_send(Envelope::RetiredLiveMember {
            workspace_id,
            device_public_key: device,
            active: true,
        });
    }
    Ok(None)
}

/// Why the subscription a live join needs was refused, when the host refused
/// one. A page asks for the live plane right behind its subscription, so a
/// refused subscription leaves this device unsubscribed too: it must hear the
/// reason the subscription failed, not that a join needs a subscription it
/// was never given.
async fn subscription_refusal(
    session: &SessionCtx<'_>,
    peer: &AuthenticatedPeer,
    workspace_id: u64,
) -> Result<Option<&'static str>, PeerError> {
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return Ok(None);
    };
    let runtime = crate::workspace_io::fresh_membership(&ws).await?;
    Ok(
        match session.decide(&runtime, peer, WorkspaceOperation::Subscribe) {
            Decision::Deny(reason) => Some(reason.reason()),
            Decision::Admit | Decision::AdmitAndEnrol => None,
        },
    )
}

/// Where `peer`'s calls in `workspace_id` are held: by this relay's workspace
/// authority when it governs the workspace, else by the workspace it carries (a
/// person's own workspace, synced across their devices through this host).
/// None unless they subscribed to it.
async fn call_host(
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    workspace_id: u64,
) -> Option<CallHost> {
    if !subscribed.contains(&workspace_id) {
        return None;
    }
    let authority = session
        .workspaces
        .lock()
        .await
        .get(&workspace_id)
        .cloned()?
        .lock()
        .await
        .workspace_authority
        .clone();
    match authority {
        Some(authority) => Some((authority.workspaces()?, authority.workspace().to_owned())),
        None => Some((
            session.carried_calls.as_ref()?.upgrade()?,
            crate::workspaces::carried_call_room(workspace_id),
        )),
    }
}

/// Handle a call envelope for a subscribed workspace: its calls are held
/// by this relay's workspace authority, or as a workspace it carries; see
/// [`crate::workspaces`].
pub(super) async fn handle_voice(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    env: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    let workspace_id = match &env {
        Envelope::VoiceCall { workspace_id, .. }
        | Envelope::VoiceJoin { workspace_id, .. }
        | Envelope::VoiceLeave { workspace_id, .. }
        | Envelope::VoiceShow { workspace_id, .. }
        | Envelope::VoiceCalls { workspace_id } => *workspace_id,
        _ => return Ok(None),
    };
    let host = call_host(session, subscribed, workspace_id).await;
    Ok(match env {
        Envelope::VoiceCall {
            request, operation, ..
        } => Some(Envelope::VoiceCallResult {
            workspace_id,
            request,
            result: call_answer(host.as_ref(), peer, &operation).await,
        }),
        Envelope::VoiceJoin {
            call,
            participation,
            ..
        } => Some(
            join_answer(
                host.as_ref(),
                workspace_id,
                peer,
                session,
                call,
                participation,
            )
            .await,
        ),
        Envelope::VoiceLeave {
            call,
            participation,
            ..
        } => {
            if let Some((workspaces, _)) = &host {
                workspaces.voice_leave(peer, call, participation);
            }
            None
        }
        Envelope::VoiceShow {
            call,
            participation,
            showing,
            ..
        } => {
            if let Some((workspaces, _)) = &host {
                workspaces.voice_show(peer, call, participation, showing);
            }
            None
        }
        Envelope::VoiceCalls { .. } => Some(Envelope::VoiceCallList {
            workspace_id,
            result: calls_answer(host.as_ref(), peer, session).await,
        }),
        _ => None,
    })
}

/// Where a peer's calls in one workspace are held: the workspace authority
/// that governs them there and the workspace as calls name it.
type CallHost = (crate::workspaces::Workspaces, String);

/// Why a peer holds no calls in a workspace: it does not subscribe to it, or
/// it watches it without taking part.
const CALLS_REFUSED: &str = "calls require a subscription to the workspace";

/// The answer to a call's change: the call as governance now records it, or
/// the reason this peer may not change it.
async fn call_answer(
    host: Option<&CallHost>,
    peer: &AuthenticatedPeer,
    operation: &tilo_relay_protocol::voice::CallOperation,
) -> Result<tilo_relay_protocol::voice::CallState, String> {
    match host {
        Some((workspaces, workspace)) => workspaces.voice_call(workspace, peer, operation).await,
        None => Err(CALLS_REFUSED.into()),
    }
}

/// The answer to a join: the participation's grant, or a refusal, which is
/// worth a line in the host's log: a beat for a participation it forgot, or a
/// fresh join it refused.
async fn join_answer(
    host: Option<&CallHost>,
    workspace_id: u64,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    call: [u8; 32],
    participation: Option<[u8; 32]>,
) -> Envelope {
    let result = match host {
        Some((workspaces, workspace)) => {
            workspaces
                .voice_join(
                    workspace,
                    peer,
                    call,
                    participation,
                    session.outbound.clone(),
                )
                .await
        }
        None => Err(CALLS_REFUSED.into()),
    };
    match result {
        Ok(grant) => Envelope::VoiceGranted {
            workspace_id,
            grant: Box::new(grant),
        },
        Err(reason) => {
            tracing::info!(
                call = %hex::encode(call),
                beat = participation.is_some(),
                %reason,
                "voice join refused"
            );
            Envelope::VoiceRefused {
                workspace_id,
                call,
                reason,
            }
        }
    }
}

/// The calls a peer may see in the workspace, or the reason it may see none.
async fn calls_answer(
    host: Option<&CallHost>,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Vec<tilo_relay_protocol::voice::CallSummary>, String> {
    match host {
        Some((workspaces, workspace)) => {
            workspaces
                .voice_calls(workspace, peer, session.outbound.clone())
                .await
        }
        None => Err(CALLS_REFUSED.into()),
    }
}

/// Handle an inbound `Presence`: fan it out to every other subscriber.
/// Ephemeral, with device attribution stamped from the authenticated session.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the flagged value is a shared workspace handle (an Arc clone); dropping it earlier releases no lock"
)]
pub(super) async fn handle_presence(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    mut env: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    // A presence that is no toolkit presence may ask the workspace for content.
    let mut asks = None;
    let Envelope::RetiredPresence { workspace_id, .. } = &env else {
        return Err(PeerError::Protocol(
            "handle_presence called with a non-Presence envelope".into(),
        ));
    };
    let workspace_id = *workspace_id;
    if !subscribed.contains(&workspace_id) {
        return Err(PeerError::Protocol(
            "presence sent before subscribing to workspace".into(),
        ));
    }
    let ws = session.workspaces.lock().await.get(&workspace_id).cloned();
    let attached = match &ws {
        Some(ws) => ws.lock().await.attached(&peer.device_public_key),
        None => false,
    };
    if !attached
        && let Envelope::RetiredPresence { payload, .. } = &mut env
        && let Some(mut attributed) = crate::AttributedPresence::decode(payload)
    {
        // Co-edit receivers recover the author from this opaque wrapper.
        // A sender's claim is not transport evidence, even if that other
        // device has a valid certificate in the same workspace.
        attributed.author = peer.device_public_key;
        asks = Some(attributed.body.clone());
        *payload = attributed
            .encode()
            .map_err(|error| PeerError::Protocol(error.to_string()))?;
    }
    if let Some(ws) = ws {
        let mut wr = ws.lock().await;
        // A device sends presence only to say hello, to ask for a baseline
        // (`SyncReq`) or after rejecting a checkpoint — each means it needs the
        // next live checkpoint (the per-device checkpoint gate of the live
        // fan-out).
        if !attached {
            wr.request_checkpoint(&peer.device_public_key);
        }
        wr.fanout_presence(&peer.device_public_key, &env);
        let authority = wr.workspace_authority.clone();
        drop(wr);
        if let Some(body) = asks {
            answer_content(
                &ws,
                authority.as_ref(),
                workspace_id,
                (*session.own_device_public_key, peer.device_public_key),
                body,
            );
        }
    }
    Ok(None)
}

/// Handle `HostSaid`: the host's own word for a run it executes
/// ([`tilo_relay_protocol::host_voice`]), fanned out to the workspace's other
/// subscribers. Only this host's own device speaks for its runs, and only
/// with its signature over a run's word: anything else is dropped here, as
/// every device drops it again on arrival. The frame never touches the live
/// plane, so a host that is no member of it speaks without its keys.
pub(super) async fn handle_host_said(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    env: Envelope,
) {
    let Envelope::HostSaid {
        workspace_id,
        said,
        signature,
    } = &env
    else {
        return;
    };
    let host = session.own_device_public_key;
    if peer.device_public_key != *host || !subscribed.contains(workspace_id) {
        tracing::debug!(
            workspace_id,
            "host frame dropped: only the host's own subscribed device speaks for its runs"
        );
        return;
    }
    if let Err(error) =
        tilo_relay_protocol::host_voice::HostSaid::open(said, signature, *workspace_id, host)
    {
        tracing::debug!(workspace_id, %error, "host frame dropped");
        return;
    }
    let ws = session.workspaces.lock().await.get(workspace_id).cloned();
    if let Some(ws) = ws {
        ws.lock().await.fanout_presence(host, &env);
    }
}

/// Handle `RecordContentWant`: fan a member's request for content out to the
/// workspace's other members, as presence was fanned out, and answer it here
/// from what the host keeps ([`answer_content`]).
#[expect(
    clippy::significant_drop_tightening,
    reason = "the flagged value is a shared workspace handle (an Arc clone); dropping it earlier releases no lock"
)]
pub(super) async fn handle_content_want(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    mut env: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    let Envelope::RecordContentWant {
        workspace_id,
        from_device_public_key,
        frame,
    } = &mut env
    else {
        return Err(PeerError::Protocol(
            "handle_content_want called with a non-RecordContentWant envelope".into(),
        ));
    };
    // A sender's claim is not transport evidence: the host stamps its own.
    let workspace_id = *workspace_id;
    if !subscribed.contains(&workspace_id) {
        return Err(PeerError::Protocol(
            "content wanted before subscribing to workspace".into(),
        ));
    }
    // The members fanned out to read the same request the host answers.
    let frame = frame.clone();
    *from_device_public_key = peer.device_public_key;
    let ws = session.workspaces.lock().await.get(&workspace_id).cloned();
    let Some(ws) = ws else {
        return Ok(None);
    };
    let mut wr = ws.lock().await;
    wr.fanout_presence(&peer.device_public_key, &env);
    let authority = wr.workspace_authority.clone();
    drop(wr);
    answer_content(
        &ws,
        authority.as_ref(),
        workspace_id,
        (*session.own_device_public_key, peer.device_public_key),
        frame,
    );
    Ok(None)
}

/// Answer a member's request for content its workspace's records reference
/// from what this host keeps ([`crate::workspaces::WorkspaceContent`]), to the
/// member's device alone, off the connection's task: reading the content
/// may touch the disk. A host that governs no workspace here keeps none of it,
/// and says so, so the member stops counting on it.
fn answer_content(
    ws: &Arc<tokio::sync::Mutex<crate::workspace::WorkspaceRuntime>>,
    authority: Option<&crate::workspaces::WorkspaceAuthority>,
    workspace_id: u64,
    (host, member): ([u8; 32], [u8; 32]),
    body: Vec<u8>,
) {
    let served = authority.and_then(|authority| {
        authority
            .workspaces()
            .map(|workspaces| (workspaces, authority.workspace().to_owned()))
    });
    let ws = Arc::clone(ws);
    tokio::spawn(async move {
        let frames = match tokio::task::spawn_blocking(move || match served {
            Some((workspaces, workspace)) => workspaces.content_answer(&workspace, &body),
            None => crate::workspaces::lacks_answer(&host, &body)
                .into_iter()
                .collect(),
        })
        .await
        {
            Ok(frames) => frames,
            Err(error) => {
                tracing::error!(workspace_id, %error, "answering a member's content request failed");
                return;
            }
        };
        if frames.is_empty() {
            return;
        }
        let runtime = ws.lock().await;
        for frame in frames {
            runtime.route_lossy(
                &member,
                Envelope::RecordContent {
                    workspace_id,
                    to_device_public_key: member,
                    from_device_public_key: host,
                    frame,
                },
            );
        }
    });
}

/// Hand a piece of a record's content to the one device it is addressed
/// to, stamped with the authenticated sender. Never stored, never fanned
/// out, never answered: a piece that cannot be delivered is dropped, and
/// the device that wanted it asks again.
pub(super) async fn route_record_content(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    envelope: Envelope,
) {
    let Envelope::RecordContent {
        workspace_id,
        to_device_public_key: to,
        frame,
        ..
    } = envelope
    else {
        return;
    };
    if !subscribed.contains(&workspace_id)
        || frame.len() > crate::transport::MAX_RECORD_CONTENT_FRAME_BYTES
    {
        return;
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return;
    };
    let runtime = ws.lock().await;
    // A piece answering the host's own request is the host's to keep.
    let host = if to == *session.own_device_public_key {
        runtime
            .workspace_authority
            .as_ref()
            .and_then(|authority| Some((authority.workspaces()?, authority.workspace().to_owned())))
    } else {
        None
    };
    // What the host asked for counts against what it keeps of a member's
    // files (its attachment quotas), so a file as large as it keeps
    // reaches it; every other piece spends the member's content budget.
    let asked = host
        .as_ref()
        .is_some_and(|(workspaces, workspace)| workspaces.host_expects(workspace, &frame));
    if !asked
        && let Some(limits) = session.member_limits
        && peer.user_public_key != *session.own_user_public_key
        && let Err(limited) = limits.admit_content(
            &peer.user_public_key,
            u64::try_from(frame.len()).unwrap_or(u64::MAX),
            tilo_clock::now(),
        )
    {
        tracing::warn!(%limited, device = %hex::encode(peer.device_public_key), "dropped record content over the member limit");
        return;
    }
    if to == *session.own_device_public_key {
        drop(runtime);
        if let Some((workspaces, workspace)) = host {
            let from = peer.device_public_key;
            tokio::task::spawn_blocking(move || {
                workspaces.content_for_host(&workspace, from, &frame);
            });
        }
        return;
    }
    let stamped = Envelope::RecordContent {
        workspace_id,
        to_device_public_key: to,
        from_device_public_key: peer.device_public_key,
        frame,
    };
    runtime.route_lossy(&to, stamped);
}
