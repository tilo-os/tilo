//! Ephemeral traffic the server never keeps: the live session's membership,
//! presence and record content handed to one device. Presence is every
//! device's own, browser pages included.

use super::{
    Arc, AuthenticatedPeer, Decision, Envelope, PeerError, SessionCtx, WorkspaceOperation,
};
use tilo_server_protocol::WorkspaceId;

/// Handle `LiveJoin`: add a subscribed device to the workspace's live session
/// and send it one `LiveMember` per member already present.
pub(super) async fn handle_live_join(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
    workspace_id: WorkspaceId,
) -> Result<Option<Envelope>, PeerError> {
    let refused = |refusal: tilo_server_protocol::refusal::Refusal| {
        Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            refusal,
        }))
    };
    // A live join before the subscription, or before the workspace is open
    // here, goes through once the host says the workspace is ready.
    if !subscribed.contains(&workspace_id) {
        match subscription_refusal(session, peer, workspace_id).await? {
            Some(refusal) => return refused(refusal),
            None => return refused(tilo_server_protocol::refusal::Refusal::NotReady),
        }
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return refused(tilo_server_protocol::refusal::Refusal::NotReady);
    };
    let members = ws
        .lock()
        .await
        .live_join(peer.device_public_key, workspace_id);
    let Some(members) = members else {
        return refused(tilo_server_protocol::refusal::Refusal::NotReady);
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

/// Why the subscription a live join needs was refused, when the host refused
/// one. A page asks for the live session right behind its subscription, so a
/// refused subscription leaves this device unsubscribed too: it must hear the
/// reason the subscription failed, not that a join needs a subscription it
/// was never given.
async fn subscription_refusal(
    session: &SessionCtx<'_>,
    peer: &AuthenticatedPeer,
    workspace_id: WorkspaceId,
) -> Result<Option<tilo_server_protocol::refusal::Refusal>, PeerError> {
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return Ok(None);
    };
    let runtime = crate::workspace_io::fresh_membership(&ws).await?;
    Ok(
        match session.decide(&runtime, peer, WorkspaceOperation::Subscribe) {
            Decision::Deny(reason) => Some(reason.refusal()),
            Decision::Admit | Decision::AdmitAndEnrol => None,
        },
    )
}

/// Handle an inbound `Presence`: fan it out to every other subscriber.
/// Ephemeral, with device attribution stamped from the authenticated session.
pub(super) async fn handle_presence(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
    mut env: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    // A presence that is no toolkit presence may ask the workspace for content.
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
    let ws = session.workspaces.lock().await.get(&workspace_id).cloned();
    let attached = match &ws {
        Some(ws) => ws.lock().await.attached(&peer.device_public_key),
        None => false,
    };
    if !attached
        && let Envelope::Presence { payload, .. } = &mut env
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
        wr.fanout_presence(&peer.device_public_key, &env);
        let authority = wr.workspace_authority.clone();
        drop(wr);
        if let Some(body) = asks {
            let (host, member) = (*session.own_device_public_key, peer.device_public_key);
            answer_content(
                &ws,
                authority.as_ref(),
                workspace_id,
                (host, member),
                peer.user_public_key,
                body,
                move |runtime, envelope| {
                    runtime.route_content(&host, &member, &envelope);
                },
            );
        }
    }
    Ok(None)
}

/// Handle `HostSaid`: the host's own word for a run it executes
/// ([`tilo_server_protocol::host_voice`]), fanned out to the workspace's other
/// subscribers. Only this host's own device speaks for its runs, and only
/// with its signature over a run's word: anything else is dropped here, as
/// every device drops it again on arrival. The frame never touches the live
/// live session, so a host that is no member of it speaks without its keys.
pub(super) async fn handle_host_said(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
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
            workspace_id = %workspace_id,
            "host frame dropped: only the host's own subscribed device speaks for its runs"
        );
        return;
    }
    if let Err(error) =
        tilo_server_protocol::host_voice::HostSaid::open(said, signature, *workspace_id, host)
    {
        tracing::debug!(workspace_id = %workspace_id, %error, "host frame dropped");
        return;
    }
    let ws = session.workspaces.lock().await.get(workspace_id).cloned();
    if let Some(ws) = ws {
        ws.lock().await.fanout_presence(host, &env);
    }
}

/// Handle `DocumentContentWant`: fan a member's request for content out to the
/// workspace's other members, as presence was fanned out, and answer it here
/// from what the host keeps ([`answer_content`]).
pub(super) async fn handle_content_want(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
    mut env: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    let Envelope::DocumentContentWant {
        workspace_id,
        from_device_public_key,
        frame,
    } = &mut env
    else {
        return Err(PeerError::Protocol(
            "handle_content_want called with a non-DocumentContentWant envelope".into(),
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
    let (host, member) = (*session.own_device_public_key, peer.device_public_key);
    answer_content(
        &ws,
        authority.as_ref(),
        workspace_id,
        (host, member),
        peer.user_public_key,
        frame,
        move |runtime, envelope| {
            runtime.route_content(&host, &member, &envelope);
        },
    );
    Ok(None)
}

/// Answer a member's request for content its workspace's records reference
/// from what this host keeps ([`crate::workspaces::WorkspaceContent`]), to the
/// member's device alone, off the connection's task: reading the content
/// may touch the disk. A host that governs no workspace here keeps none of it,
/// and says so, so the member stops counting on it. Each piece goes where
/// `deliver` sends it: to a member's session here, or back on the session
/// to the other holder that asked.
pub(crate) fn answer_content(
    ws: &Arc<tokio::sync::Mutex<crate::workspace::WorkspaceRuntime>>,
    authority: Option<&crate::workspaces::WorkspaceAuthority>,
    workspace_id: WorkspaceId,
    (host, member): ([u8; 32], [u8; 32]),
    member_user: [u8; 32],
    body: Vec<u8>,
    deliver: impl Fn(&crate::workspace::WorkspaceRuntime, Envelope) + Send + 'static,
) {
    let served = authority.and_then(|authority| {
        authority
            .workspaces()
            .map(|workspaces| (workspaces, authority.workspace().to_owned()))
    });
    let ws = Arc::clone(ws);
    tokio::spawn(async move {
        // The host reads content one member's requests at a time: a member
        // asking faster than the disk answers cannot take the reads other
        // members' content needs ([`crate::content_work`]).
        let Some(_reading) = ws.lock().await.content_work.take_place(&member_user) else {
            return;
        };
        let frames = match tokio::task::spawn_blocking(move || match served {
            Some((workspaces, workspace)) => workspaces.content_answer(&workspace, &member, &body),
            None => neighbour_wire::content::answer(&host, &member, &body, |_, _, _| None),
        })
        .await
        {
            Ok(frames) => frames,
            Err(error) => {
                tracing::error!(workspace_id = %workspace_id, %error, "answering a member's content request failed");
                return;
            }
        };
        if frames.is_empty() {
            return;
        }
        let runtime = ws.lock().await;
        for frame in frames {
            deliver(
                &runtime,
                Envelope::DocumentContent {
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
#[expect(
    clippy::significant_drop_tightening,
    reason = "a member's place at the device's reads is held until the read finishes"
)]
pub(super) async fn route_document_content(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
    envelope: Envelope,
) {
    let Envelope::DocumentContent {
        workspace_id,
        to_device_public_key: to,
        frame,
        ..
    } = envelope
    else {
        return;
    };
    if !subscribed.contains(&workspace_id)
        || frame.len() > crate::transport::MAX_DOCUMENT_CONTENT_FRAME_BYTES
    {
        return;
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return;
    };
    let runtime = ws.lock().await;
    if to == *session.own_device_public_key {
        // A piece answering the host's own request is the host's to keep;
        // reading it may touch the disk, so it goes to the blocking pool.
        // One member may hold only so many of those reads at once, so a
        // flood cannot take the pool from another member's pieces
        // ([`crate::content_work`]).
        let host = runtime.workspace_authority.as_ref().and_then(|authority| {
            Some((authority.workspaces()?, authority.workspace().to_owned()))
        });
        let reading = runtime.content_work.take_place(&peer.user_public_key);
        drop(runtime);
        let Some(reading) = reading else {
            tracing::debug!(
                device = %hex::encode(peer.device_public_key),
                "dropped record content: this member's host reads are all running"
            );
            return;
        };
        if let Some((workspaces, workspace)) = host {
            let from = peer.device_public_key;
            tokio::task::spawn_blocking(move || {
                let _reading = reading;
                workspaces.content_for_host(&workspace, from, &frame);
            });
        }
        return;
    }
    let stamped = Envelope::DocumentContent {
        workspace_id,
        to_device_public_key: to,
        from_device_public_key: peer.device_public_key,
        frame,
    };
    runtime.route_content(&peer.device_public_key, &to, &stamped);
}
