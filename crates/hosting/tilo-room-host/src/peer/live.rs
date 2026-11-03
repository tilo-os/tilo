//! Ephemeral traffic the relay never keeps: the live plane (`LiveJoin`,
//! `LiveLeave`, `Live`), presence, voice calls and record content handed
//! to one device. Presence is every device's own, browser pages included.

use super::{
    Arc, AuthenticatedPeer, Decision, Envelope, PeerError, SessionCtx, WorkspaceOperation,
    unix_now_ns, workspace_access,
};

/// Handle `LiveJoin`: add a subscribed device to the workspace's live plane
/// and send it one `LiveMember` per member already present.
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
    let members = {
        let mut runtime = ws.lock().await;
        // A spectator neither shows nor sends a cursor.
        if workspace_access::watches_only(&runtime, peer) {
            return refused(workspace_access::DenyReason::WatchOnly.reason());
        }
        runtime.live_join(peer.device_public_key, workspace_id)
    };
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
/// None unless they subscribed to it, and none for a spectator, who takes
/// no part in the workspace's calls.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the registry guard only lives for the lookup and the workspace guard for the answer read from it"
)]
async fn call_host(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    workspace_id: u64,
) -> Option<(crate::workspaces::Workspaces, String)> {
    if !subscribed.contains(&workspace_id) {
        return None;
    }
    let ws = session
        .workspaces
        .lock()
        .await
        .get(&workspace_id)
        .cloned()?;
    let authority = {
        let runtime = ws.lock().await;
        if workspace_access::watches_only(&runtime, peer) {
            return None;
        }
        runtime.workspace_authority.clone()
    };
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
    let workspaces = call_host(peer, session, subscribed, workspace_id).await;
    let refusal = "calls require a subscription to the room";
    Ok(match env {
        Envelope::VoiceCall {
            request, operation, ..
        } => {
            let result = match &workspaces {
                Some((workspaces, workspace)) => {
                    workspaces.voice_call(workspace, peer, &operation).await
                }
                None => Err(refusal.into()),
            };
            Some(Envelope::VoiceCallResult {
                workspace_id,
                request,
                result,
            })
        }
        Envelope::VoiceJoin {
            call,
            participation,
            ..
        } => {
            let result = match &workspaces {
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
                None => Err(refusal.into()),
            };
            Some(match result {
                Ok(grant) => Envelope::VoiceGranted {
                    workspace_id,
                    grant: Box::new(grant),
                },
                Err(reason) => {
                    // Why a call drops: a beat for a participation the host
                    // forgot, or a fresh join it refused.
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
            })
        }
        Envelope::VoiceLeave {
            call,
            participation,
            ..
        } => {
            if let Some((workspaces, _)) = &workspaces {
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
            if let Some((workspaces, _)) = &workspaces {
                workspaces.voice_show(peer, call, participation, showing);
            }
            None
        }
        Envelope::VoiceCalls { .. } => Some(Envelope::VoiceCallList {
            workspace_id,
            result: match &workspaces {
                Some((workspaces, workspace)) => {
                    workspaces
                        .voice_calls(workspace, peer, session.outbound.clone())
                        .await
                }
                None => Err(refusal.into()),
            },
        }),
        _ => None,
    })
}

/// Handle `LiveLeave`.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the guard only lives for the lookup; the value kept is a clone"
)]
pub(super) async fn handle_live_leave(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    workspace_id: u64,
) -> Result<Option<Envelope>, PeerError> {
    let ws = session.workspaces.lock().await.get(&workspace_id).cloned();
    if let Some(ws) = ws {
        let mut ws = ws.lock().await;
        ws.live_leave(&peer.device_public_key, workspace_id);
        drop(ws);
    }
    Ok(None)
}

/// Handle an inbound `Live` frame: decode it canonically, stamp the
/// authenticated sender and relay it within the sender's rate budget.
/// Invalid, over-budget or non-member frames are dropped: live traffic is
/// never authority, and dropping it costs only a stale cursor.
pub(super) async fn handle_live(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    workspace_id: u64,
    frame: Vec<u8>,
) -> Result<Option<Envelope>, PeerError> {
    if !subscribed.contains(&workspace_id) {
        return Ok(None);
    }
    // Only a sealed frame is relayed; its content is never read here.
    // optional-decode: live traffic is never authority; a malformed frame costs a stale cursor (above)
    let Ok(sealed) = crate::live::SealedLiveFrame::decode(&frame) else {
        return Ok(None);
    };
    let presence = sealed.kind == crate::live::LiveKind::Presence;
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return Ok(None);
    };
    let stamped = Envelope::RetiredLive {
        workspace_id,
        from_device_public_key: peer.device_public_key,
        frame,
    };
    ws.lock()
        .await
        .fanout_live(&peer.device_public_key, presence, &stamped, unix_now_ns());
    Ok(None)
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
    let mut slot = None;
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
        slot = crate::coedit::presence_slot(&attributed.body);
        if slot.is_none() {
            asks = Some(attributed.body.clone());
        }
        *payload = attributed
            .encode()
            .map_err(|error| PeerError::Protocol(error.to_string()))?;
    }
    if let Some(ws) = ws {
        let mut wr = ws.lock().await;
        // A device sends presence only to say hello, to ask for a baseline
        // (`SyncReq`) or after rejecting a checkpoint — each means it needs the
        // next live checkpoint (the per-device checkpoint gate in
        // `fanout_delta`).
        if !attached {
            wr.request_checkpoint(&peer.device_public_key);
        }
        // A spectator's presence asks for its baseline: nobody sees it. In
        // a public workspace it reads what members read, so it may also ask the
        // workspace for the content those records reference.
        if workspace_access::watches_only(&wr, peer) {
            let reads = wr.spectators.is_some_and(|spectators| spectators.public);
            let authority = wr.workspace_authority.clone();
            drop(wr);
            if reads && let Some(body) = asks {
                answer_content(
                    &ws,
                    authority.as_ref(),
                    workspace_id,
                    (*session.own_device_public_key, peer.device_public_key),
                    body,
                );
            }
            return Ok(None);
        }
        wr.fanout_presence(&peer.device_public_key, &env, slot);
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

/// Handle `RecordContentWant`: fan a member's request for content out to the
/// workspace's other members, as presence was fanned out, and answer it here
/// from what the host keeps ([`answer_content`]). A spectator's request
/// nobody sees, as its presence was not.
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
    let frame = std::mem::take(frame);
    *from_device_public_key = peer.device_public_key;
    let ws = session.workspaces.lock().await.get(&workspace_id).cloned();
    let Some(ws) = ws else {
        return Ok(None);
    };
    let mut wr = ws.lock().await;
    let watching = workspace_access::watches_only(&wr, peer);
    if !watching {
        wr.fanout_presence(&peer.device_public_key, &env, None);
    }
    // In a public workspace a spectator reads what members read, so it may also
    // ask the workspace for the content those records reference.
    let reads = !watching || wr.spectators.is_some_and(|spectators| spectators.public);
    let authority = wr.workspace_authority.clone();
    drop(wr);
    if reads {
        answer_content(
            &ws,
            authority.as_ref(),
            workspace_id,
            (*session.own_device_public_key, peer.device_public_key),
            frame,
        );
    }
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
    // A spectator hands no member content.
    if workspace_access::watches_only(&runtime, peer) {
        return;
    }
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
            std::time::Instant::now(),
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
                workspaces.content_for_host(&workspace, from, &frame)
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
