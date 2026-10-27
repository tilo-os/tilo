//! Ephemeral traffic the relay never keeps: the live plane (`LiveJoin`,
//! `LiveLeave`, `Live`), presence, voice calls and record content handed
//! to one device.

use super::{
    Arc, AuthenticatedPeer, Envelope, PeerError, SessionCtx, room_access, session_peer_authorized,
    unix_now_ns,
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
        return refused(UNSUBSCRIBED);
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return refused(UNSUBSCRIBED);
    };
    let members = {
        let mut runtime = ws.lock().await;
        // A spectator neither shows nor sends a cursor.
        if room_access::watches_only(&runtime, peer) {
            return refused(room_access::DenyReason::WatchOnly.reason());
        }
        runtime.live_join(peer.device_public_key, workspace_id)
    };
    let Some(members) = members else {
        return refused(UNSUBSCRIBED);
    };
    for device in members {
        // Live state is never authority: a member notice lost to a full
        // queue costs only a late cursor.
        let _ = session.outbound.try_send(Envelope::LiveMember {
            workspace_id,
            device_public_key: device,
            active: true,
        });
    }
    Ok(None)
}

/// The room authority `peer` takes part in calls of `workspace_id` through:
/// none unless they subscribed to it and this relay governs its room, and
/// none for a spectator, who takes no part in the room's calls.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the registry guard only lives for the lookup and the workspace guard for the answer read from it"
)]
async fn call_authority(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    workspace_id: u64,
) -> Option<crate::rooms::RoomAuthority> {
    if !subscribed.contains(&workspace_id) {
        return None;
    }
    let ws = session
        .workspaces
        .lock()
        .await
        .get(&workspace_id)
        .cloned()?;
    let runtime = ws.lock().await;
    (!room_access::watches_only(&runtime, peer))
        .then(|| runtime.room_authority.clone())
        .flatten()
}

/// Handle a call envelope for a subscribed workspace whose room this relay
/// governs; see [`crate::rooms`].
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
    let authority = call_authority(peer, session, subscribed, workspace_id).await;
    let rooms = authority
        .as_ref()
        .and_then(|authority| Some((authority.rooms()?, authority.room().to_owned())));
    let refusal = "calls require a subscription to a room this host governs";
    Ok(match env {
        Envelope::VoiceCall {
            request, operation, ..
        } => {
            let result = match &rooms {
                Some((rooms, room)) => rooms.voice_call(room, peer, &operation).await,
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
            let result = match &rooms {
                Some((rooms, room)) => {
                    rooms
                        .voice_join(room, peer, call, participation, session.outbound.clone())
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
            if let Some((rooms, _)) = &rooms {
                rooms.voice_leave(peer, call, participation);
            }
            None
        }
        Envelope::VoiceShow {
            call,
            participation,
            showing,
            ..
        } => {
            if let Some((rooms, _)) = &rooms {
                rooms.voice_show(peer, call, participation, showing);
            }
            None
        }
        Envelope::VoiceCalls { .. } => Some(Envelope::VoiceCallList {
            workspace_id,
            result: match &rooms {
                Some((rooms, room)) => {
                    rooms
                        .voice_calls(room, peer, session.outbound.clone())
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
    let Ok(sealed) = crate::live::SealedLiveFrame::decode(&frame) else {
        return Ok(None);
    };
    let presence = sealed.kind == crate::live::LiveKind::Presence;
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return Ok(None);
    };
    if let Some(bridge) = session.live_bridge
        && ws.lock().await.is_live_member(&peer.device_public_key)
    {
        bridge.live(workspace_id, peer.device_public_key, &frame);
    }
    let stamped = Envelope::Live {
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
    // A presence that is no toolkit presence may ask the room for content.
    let mut asks = None;
    let Envelope::Presence { workspace_id, .. } = &env else {
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
    if peer.role == crate::auth::PeerRole::Device
        && let Envelope::Presence { payload, .. } = &mut env
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
    let ws = {
        let map = session.workspaces.lock().await;
        map.get(&workspace_id).cloned()
    };
    let mut legacy_writer = false;
    if let Some(ws) = ws {
        if session.live_bridge.is_some() {
            let authority = {
                let runtime = ws.lock().await;
                if runtime.has_subscriber(&peer.device_public_key)
                    && session_peer_authorized(peer, session)
                {
                    legacy_writer = true;
                }
                runtime.room_authority.clone()
            };
            if let Some(authority) = authority {
                legacy_writer &= match authority.rooms() {
                    Some(rooms) => rooms
                        .legacy_writer_role(authority.room(), peer.device_public_key)
                        .await
                        .is_some_and(room_access::role_writes),
                    None => false,
                };
            }
        }
        let mut wr = ws.lock().await;
        // A device sends presence only to say hello, to ask for a baseline
        // (`SyncReq`) or after rejecting a checkpoint — each means it needs the
        // next live checkpoint (the per-device checkpoint gate in
        // `fanout_delta`).
        if peer.role == crate::auth::PeerRole::Device {
            wr.request_checkpoint(&peer.device_public_key);
        }
        // A spectator's presence asks for its baseline: nobody sees it. In
        // a public room it reads what members read, so it may also ask the
        // room for the content those records reference.
        if room_access::watches_only(&wr, peer) {
            let reads = wr.spectators.is_some_and(|spectators| spectators.public);
            let authority = wr.room_authority.clone();
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
        wr.fanout_presence(&peer.device_public_key, peer.role, &env, slot);
        let authority = wr.room_authority.clone();
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
    if legacy_writer
        && let (Some(bridge), crate::auth::PeerRole::Device, Envelope::Presence { payload, .. }) =
            (session.live_bridge, peer.role, &env)
    {
        bridge.presence(
            workspace_id,
            peer.device_public_key,
            peer.user_public_key,
            payload,
        );
    }
    Ok(None)
}

/// Answer a member's request for content its room's records reference
/// from what this host keeps ([`crate::rooms::RoomContent`]), to the
/// member's device alone, off the connection's task: reading the content
/// may touch the disk. A host that governs no room here keeps none of it,
/// and says so, so the member stops counting on it.
fn answer_content(
    ws: &Arc<tokio::sync::Mutex<crate::workspace::WorkspaceRuntime>>,
    authority: Option<&crate::rooms::RoomAuthority>,
    workspace_id: u64,
    (host, member): ([u8; 32], [u8; 32]),
    body: Vec<u8>,
) {
    let served = authority.and_then(|authority| {
        authority
            .rooms()
            .map(|rooms| (rooms, authority.room().to_owned()))
    });
    let ws = Arc::clone(ws);
    tokio::spawn(async move {
        let Ok(frames) = tokio::task::spawn_blocking(move || match served {
            Some((rooms, room)) => rooms.content_answer(&room, &body),
            None => crate::rooms::lacks_answer(&host, &body)
                .into_iter()
                .collect(),
        })
        .await
        else {
            return;
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
    if room_access::watches_only(&runtime, peer) {
        return;
    }
    // A piece answering the host's own request is the host's to keep.
    let host = if to == *session.own_device_public_key {
        runtime
            .room_authority
            .as_ref()
            .and_then(|authority| Some((authority.rooms()?, authority.room().to_owned())))
    } else {
        None
    };
    // What the host asked for counts against what it keeps of a member's
    // files (its attachment quotas), so a file as large as it keeps
    // reaches it; every other piece spends the member's content budget.
    let asked = host
        .as_ref()
        .is_some_and(|(rooms, room)| rooms.host_expects(room, &frame));
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
        if let Some((rooms, room)) = host {
            let from = peer.device_public_key;
            tokio::task::spawn_blocking(move || rooms.content_for_host(&room, from, &frame));
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
