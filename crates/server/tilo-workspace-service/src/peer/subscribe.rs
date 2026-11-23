//! `Subscribe` and `Unsubscribe`: admission and live-subscriber registration.
//! A subscriber is reached with the workspace's live traffic (presence, calls,
//! actions and content) for as long as its session lasts; the workspace's
//! records reach a device over its link to the workspace, never here.

use super::{
    Arc, AuthenticatedPeer, Decision, Envelope, PeerError, SessionCtx, SharedWorkspace, Subscriber,
    WorkspaceOperation, current_session, session_peer_authorized, unix_now_ns,
};
use tilo_server_protocol::WorkspaceId;

/// Handle `Subscribe`: check on membership, then register the live
/// subscriber and answer `Ready`.
pub(super) async fn handle_subscribe(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &mut Vec<WorkspaceId>,
    workspace_id: WorkspaceId,
) -> Result<Option<Envelope>, PeerError> {
    let ws = match admit_subscription(peer, session, subscribed, workspace_id).await? {
        Ok(ws) => ws,
        Err(refusal) => return Ok(Some(refusal)),
    };
    let mut wr = ws.lock().await;
    wr.add_subscriber(Subscriber {
        device_public_key: peer.device_public_key,
        tx: session.outbound.clone(),
        disconnect: session.disconnect.clone(),
        content_waiting: Arc::clone(session.content),
        peer: peer.clone(),
    });
    if let Some(mailbox) = session.presence {
        wr.route_presence(
            peer.device_public_key,
            session.outbound,
            Arc::clone(mailbox),
        );
    }
    drop(wr);
    // A page builds its replica of the workspace from the membership answer its
    // host sends as it subscribes: the roster and the governance every record it
    // reads or writes is decided against. Sent before `Ready`, so what the session
    // asks for as soon as it is admitted is decided on a workspace it holds.
    if let Some(admin) = session.admin
        && let Some(bootstrap) = admin.subscriber_bootstrap(workspace_id, &peer.cert).await
    {
        session
            .outbound
            .send(Envelope::WorkspaceBootstrap {
                workspace_id,
                bootstrap,
            })
            .await
            .map_err(|_| PeerError::Protocol("the session's writer ended".into()))?;
    }
    // A channel's governance reaches this session from its start, whether or not
    // it opened a record, as a link hands it to the devices linked to it: sent
    // before `Ready`, so a page that writes its first sealed record now has the
    // group it writes under.
    if let Some(admin) = session.admin {
        admin.watch_session(
            workspace_id,
            peer.device_public_key,
            session.outbound,
            session.disconnect,
        );
        for envelope in admin.channel_governance(workspace_id, peer.device_public_key) {
            session
                .outbound
                .send(envelope)
                .await
                .map_err(|_| PeerError::Protocol("the session's writer ended".into()))?;
        }
        // A device over its link is replayed the workspace's records it may read, less those it
        // is sent when it opens them. A session that stands in for one is sent the same, and
        // holds each record from then on, so a record a page never opened is still one it
        // follows.
        for (envelope, holds) in admin
            .eager_records(workspace_id, peer.device_public_key)
            .await
        {
            for channel in holds {
                admin.watch_record(
                    workspace_id,
                    peer.device_public_key,
                    session.outbound,
                    session.disconnect,
                    channel,
                );
            }
            session
                .outbound
                .send(envelope)
                .await
                .map_err(|_| PeerError::Protocol("the session's writer ended".into()))?;
        }
    }
    session.metrics.inc_subscribes();
    if !subscribed.contains(&workspace_id) {
        subscribed.push(workspace_id);
    }
    Ok(Some(Envelope::Ready {
        workspace_id,
        pending_deltas: 0,
        documents_when_opened: true,
    }))
}

/// Whether `peer` may subscribe to `workspace_id`: the agent's delegation, the
/// workspace this host holds, and the workspace's decision by its member list,
/// enrolling the device when the workspace says so. A device
/// the server does not admit is refused as not admitted; a connection whose
/// own authorization changed meanwhile ends.
async fn admit_subscription(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
    workspace_id: WorkspaceId,
) -> Result<Result<SharedWorkspace, Envelope>, PeerError> {
    // A workspace its owner deleted here is gone for good: whoever asks is
    // told so, and forgets it rather than tries again.
    if session
        .admin
        .is_some_and(|workspaces| workspaces.deleted(workspace_id))
    {
        return Ok(Err(Envelope::NotAuthorised {
            workspace_id,
            refusal: tilo_server_protocol::refusal::Refusal::WorkspaceDeleted,
        }));
    }
    // An agent outside its delegation never gets a workspace bound for it.
    if let Err(reason) = session
        .trust()
        .agent(&peer.device_public_key, workspace_id, unix_now_ns())
    {
        return Ok(Err(Envelope::NotAuthorised {
            workspace_id,
            refusal: reason.refusal(),
        }));
    }
    // Only a workspace this host holds admits a device here: a person's own
    // server makes theirs, and one made elsewhere is handed over.
    let ws = session.workspaces.lock().await.get(&workspace_id).cloned();
    let Some(ws) = ws else {
        // A host that keeps nothing of this person forgot them: their session
        // stops as not admitted, and they join again through its entry. One
        // that knows them holds no such workspace yet: it is not ready until
        // the person's device hands it over or their server makes it.
        let known = session
            .admin
            .is_some_and(|workspaces| workspaces.knows(&peer.user_public_key));
        return Ok(Err(Envelope::NotAuthorised {
            workspace_id,
            refusal: if known {
                tilo_server_protocol::refusal::Refusal::NotReady
            } else {
                tilo_server_protocol::refusal::Refusal::NotAdmitted
            },
        }));
    };
    let mut wr = ws.lock().await;
    // The certificate passed when this session first subscribed: failing now
    // is a change mid-session. A first subscribe that fails it is refused
    // below, with no workspace data.
    if !subscribed.is_empty() && !session_peer_authorized(peer, session) {
        return Err(PeerError::Protocol(
            "subscriber authorization changed".into(),
        ));
    }
    if subscribed.contains(&workspace_id) && !current_session(&wr, peer, session.outbound) {
        return Err(PeerError::Protocol(
            "subscriber authorization changed".into(),
        ));
    }
    // A host attached to this one holds its session under an active lease,
    // so it doesn't need to appear in the workspace membership list.
    match session.decide(&wr, peer, WorkspaceOperation::Subscribe) {
        Decision::Admit => {}
        Decision::AdmitAndEnrol => {
            drop(wr);
            crate::workspace_io::enrol(&ws, &peer.cert).await;
            wr = ws.lock().await;
            if !session.admits(&wr, peer, WorkspaceOperation::Subscribe) {
                return Err(PeerError::Protocol(
                    "subscriber authorization changed".into(),
                ));
            }
            if subscribed.contains(&workspace_id) && !current_session(&wr, peer, session.outbound) {
                return Err(PeerError::Protocol(
                    "subscriber authorization changed".into(),
                ));
            }
        }
        Decision::Deny(reason) => {
            return Ok(Err(Envelope::NotAuthorised {
                workspace_id,
                refusal: reason.refusal(),
            }));
        }
    }
    drop(wr);
    Ok(Ok(ws))
}

/// Handle `Unsubscribe`: drop the live subscriber (if the workspace
/// is known) and stop tracking it for this connection's teardown.
pub(super) async fn handle_unsubscribe(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &mut Vec<WorkspaceId>,
    workspace_id: WorkspaceId,
) -> Result<Option<Envelope>, PeerError> {
    let ws = {
        let map = session.workspaces.lock().await;
        map.get(&workspace_id).cloned()
    };
    if let Some(ws) = ws {
        ws.lock()
            .await
            .remove_subscriber_session(&peer.device_public_key, session.outbound);
    }
    subscribed.retain(|id| id != &workspace_id);
    Ok(None)
}
