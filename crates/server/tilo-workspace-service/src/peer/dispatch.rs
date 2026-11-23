//! The inbound dispatcher: [`handle_envelope`] for the state queue
//! ([`Envelope`]).

use super::{
    AuthenticatedPeer, Envelope, PeerError, RemoteActionRoute, SessionCtx, handle_agent_session,
    handle_content_want, handle_host_action, handle_host_said, handle_live_join,
    handle_open_document, handle_presence, handle_remote_action, handle_subscribe,
    handle_unsubscribe, handle_workspaces, route_document_content,
};
use tilo_server_protocol::WorkspaceId;

/// The reply to an envelope that carried a workspace's records, which reach
/// a device over its link to the workspace rather than a session: the
/// session ends with it, as for any envelope the host does not take.
/// A session client's write, until it moves onto actions.
///
/// A session carries the client's signed document writes as `Commit`, and, from
/// a client that writes them straight to the link, as `Delta`; its identity hello
/// arrives as `Delta` too. This is only the transport into the device link's
/// one admission: the frame is admitted by [`tilo_peer_state::direct::DirectSync::admit_one`] exactly as
/// the workspace's link admits a frame, after the same author check
/// ([`tilo_peer_state::direct::DirectSync::admits_author`]) against the same roster, into the same store
/// and replica. A `Commit` is answered with the host order once the frame is
/// kept; a `Delta` is answered nothing.
async fn handle_session_write(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
    env: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    use tilo_server_protocol::refusal::Refusal;
    let (workspace_id, op, author, delta) = match env {
        Envelope::Commit {
            workspace_id,
            op,
            author_device_public_key,
            delta,
            ..
        } => (workspace_id, Some(op), author_device_public_key, delta),
        Envelope::Delta {
            workspace_id,
            author_device_public_key,
            delta,
            ..
        } => (workspace_id, None, author_device_public_key, delta),
        _ => return Ok(None),
    };
    let refused = |refusal: Refusal| {
        op.map(|op| Envelope::CommitRefused {
            workspace_id,
            op,
            refusal,
        })
    };
    // A session writes only into a workspace it subscribed to, which is where
    // its membership was checked.
    if !subscribed.contains(&workspace_id) {
        return Ok(refused(Refusal::NotAMember(
            "the session does not follow that workspace".into(),
        )));
    }
    // The session authenticated `peer`; its write names the author it signs as.
    if author != peer.device_public_key {
        return Ok(refused(Refusal::NotAMember(
            "the write names another device than the session's".into(),
        )));
    }
    let Some(
        crate::coedit::CoeditPayload::Checkpoint { body, .. }
        | crate::coedit::CoeditPayload::RetainedDelta { body, .. },
    ) = crate::coedit::CoeditPayload::decode(&delta)
    else {
        return Ok(refused(Refusal::NotAllowed(
            "the write does not decode".into(),
        )));
    };
    let Ok(message) = neighbour_wire::NeighbourMsg::decode(&body) else {
        return Ok(refused(Refusal::NotAllowed(
            "the write is not a message".into(),
        )));
    };
    // What may arrive is what the link itself admits: a member's signed record
    // frame, the workspace's own changes, a device's identity hello (its
    // certificate and revocations, which take the link's hello path), or a
    // channel's governance and a device's request to join its group, which the
    // link answers from the geneses it carries and hands to the channel's
    // members. Nothing else is a frame of the workspace. Each goes through the
    // same author and member checks below, and the link's one admission.
    if !matches!(
        message,
        neighbour_wire::NeighbourMsg::AppDocument { .. }
            | neighbour_wire::NeighbourMsg::SyncResp { .. }
            | neighbour_wire::NeighbourMsg::IdentityHello { .. }
            | neighbour_wire::NeighbourMsg::MlsGovernance { .. }
            | neighbour_wire::NeighbourMsg::MlsAccessRequest { .. }
    ) {
        tracing::debug!(
            %workspace_id,
            kind = message.kind(),
            "a session carried no frame of the workspace; nothing was admitted"
        );
        return Ok(None);
    }
    let Some(workspaces) = session.admin else {
        return Ok(refused(Refusal::NotReady));
    };
    let Some(link) = workspaces.direct().link(workspace_id) else {
        return Ok(refused(Refusal::NotReady));
    };
    if let Err(reason) = link.admits_author(author) {
        return Ok(refused(Refusal::NotAMember(reason)));
    }
    // The link hands the frame to its own reader, which follows it into the
    // workspace's copy as it follows any link's frame: one admission, one
    // follow, one notice to whoever follows the workspace's records. The
    // writer hears what that admission decided: a write the workspace refused
    // is never answered as committed, and one that waits for history the
    // workspace lacks is not answered as kept either.
    match link.admit_one(author, message).await {
        (_, Some(tilo_peer_state::direct::Kept::Refused)) => Ok(refused(Refusal::NotAllowed(
            "the workspace refused the write".into(),
        ))),
        (true, None) => Ok(refused(Refusal::NotReady)),
        _ => Ok(op.map(|op| Envelope::Committed {
            workspace_id,
            op,
            seq: crate::workspace_documents::host_order(),
        })),
    }
}

fn not_on_a_session(kind: &str) -> PeerError {
    PeerError::Protocol(format!(
        "{kind}: a workspace's records travel over the device link, not a session"
    ))
}

pub(super) async fn handle_envelope(
    peer: &AuthenticatedPeer,
    env: Envelope,
    session: &SessionCtx<'_>,
    subscribed: &mut Vec<WorkspaceId>,
) -> Result<Option<Envelope>, PeerError> {
    match env {
        Envelope::Subscribe { workspace_id } => {
            handle_subscribe(peer, session, subscribed, workspace_id).await
        }
        Envelope::Unsubscribe { workspace_id } => {
            handle_unsubscribe(peer, session, subscribed, workspace_id).await
        }
        env @ (Envelope::Delta { .. } | Envelope::Commit { .. }) => {
            handle_session_write(peer, session, subscribed, env).await
        }
        env @ Envelope::OpenDocument { .. } => {
            handle_open_document(peer, session, subscribed, env).await
        }
        env @ (Envelope::SubscribeFrom { .. }
        | Envelope::CompactVolunteer { .. }
        | Envelope::Compacted { .. }
        | Envelope::PeerServerHello { .. }) => Err(not_on_a_session(env.kind())),
        env @ Envelope::Presence { .. } => {
            handle_presence(peer, session, subscribed, env).await
        }
        env @ Envelope::DocumentContentWant { .. } => {
            handle_content_want(peer, session, subscribed, env).await
        }
        Envelope::Ping => Ok(Some(Envelope::Pong)),
        Envelope::RemoteAction {
            workspace_id,
            version,
            to_device_public_key,
            frame,
            ..
        } => Ok(handle_remote_action(
            peer,
            session,
            subscribed,
            RemoteActionRoute {
                workspace_id,
                version,
                to: to_device_public_key,
            },
            frame,
        )
        .await),
        env @ Envelope::DocumentContent { .. } => {
            route_document_content(peer, session, subscribed, env).await;
            Ok(None)
        }
        Envelope::LiveJoin { workspace_id } => {
            handle_live_join(peer, session, subscribed, workspace_id).await
        }
        env @ Envelope::HostSaid { .. } => {
            handle_host_said(peer, session, subscribed, env).await;
            Ok(None)
        }
        Envelope::HostAction { frame } => Ok(handle_host_action(peer, session, &frame)),
        Envelope::WorkspacesRequest { request_id, frame } => {
            Box::pin(handle_workspaces(peer, session, request_id, &frame)).await
        }
        Envelope::AgentSession { request_id, label } => {
            Ok(Some(handle_agent_session(
                peer, session, subscribed, request_id, &label,
            )))
        }
        // A device that only follows its channel groups said so to a host
        // that replayed it records; it is no different from any other here.
        Envelope::KeyFollower
        // Envelopes the server only *sends*, never receives, and an ask for a
        // workspace's state on a session, which reaches a device over its link
        // to the workspace instead: ignore silently so a peer bug doesn't wedge
        // the connection, and a client still asking reads on.
        | Envelope::SyncRequest { .. }
        | Envelope::CompactRequest { .. }
        | Envelope::Ready { .. }
        | Envelope::NotAuthorised { .. }
        | Envelope::Pong
        | Envelope::SyncResponse { .. }
        | Envelope::RemoteActionUndeliverable { .. }
        | Envelope::Committed { .. }
        | Envelope::CommitRefused { .. }
        | Envelope::ChangeRefused { .. }
        | Envelope::LiveMember { .. }
        | Envelope::Position { .. }
        | Envelope::DocumentOpened { .. }
        | Envelope::WorkspaceBootstrap { .. }
        | Envelope::WorkspacesReply { .. }
        | Envelope::AgentSessionAnswer { .. }
        | Envelope::AgentSessionStopped
        // Only the host answers an entry, once, before the session.
        | Envelope::Entered(_)
        | Envelope::FetchGrant { .. }
        // The person's list is the host's to send, never a device's to set.
        | Envelope::PersonalList { .. } => Ok(None),
    }
}
