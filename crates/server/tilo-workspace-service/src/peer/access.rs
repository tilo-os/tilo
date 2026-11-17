//! Who may still act on this connection: the server-wide certificate and
//! membership checks the session handlers share, and the periodic
//! subscriber audit that ends a session whose access lapsed.

use super::{
    AuthenticatedPeer, Decision, Envelope, PeerError, Refusal, SessionCtx, WorkspaceOperation,
    mpsc, unix_now_ns, workspace_access,
};
use tilo_server_protocol::WorkspaceId;

/// Membership + revocation re-check for every workspace this peer is
/// subscribed to, run when [`AccessAudit`] says. Reloads membership
/// from disk when the membership watch saw it change, or on every audit
/// while no live watch covers the workspace, and ends the session if the
/// device's own cert was revoked or it fell out of membership.
///
/// A workspace its host deleted is not a lapsed subscription: the refusal
/// that went with the deletion says so, the connection stops following it,
/// and the session stays for the other workspaces the connection serves — a
/// backend reaches hundreds of its customers' workspaces on one session, and
/// deleting one of them must not take the rest away.
///
/// Returns whether every subscribed workspace's membership is change-notified,
/// so the next audit can wait for a change instead of the disk.
pub(super) async fn audit_subscriber_access(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &mut Vec<WorkspaceId>,
) -> Result<bool, PeerError> {
    let mut watched = true;
    // Server-wide first, so a revoked or untrusted device ends even with no
    // bound workspace left to ask. A connection that holds no workspace here
    // had no access to lose: it ends as a refusal, and only one that held a
    // subscription meets the protocol-violation close for changed authority.
    session
        .trust()
        .cert(peer, unix_now_ns())
        .map_err(|reason| {
            if subscribed.is_empty() {
                PeerError::NotAdmitted
            } else {
                PeerError::Protocol(reason.reason().into())
            }
        })?;
    let mut gone = Vec::new();
    for workspace_id in subscribed.iter().copied() {
        if session
            .admin
            .is_some_and(|admin| admin.deleted(workspace_id))
        {
            gone.push(workspace_id);
            continue;
        }
        let Some(workspace) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
            // Not served here any more (released to another host): the
            // connection follows where it went, and keeps the rest.
            gone.push(workspace_id);
            continue;
        };
        let workspace = crate::workspace_io::fresh_membership(&workspace).await?;
        watched &= workspace.membership_is_current();
        let lost = (!current_session(&workspace, peer, session.outbound))
            .then(|| lost_session(&workspace, peer, session, WorkspaceOperation::Subscribe));
        let decision = session.decide(&workspace, peer, WorkspaceOperation::Subscribe);
        drop(workspace);
        if let Some(lost) = lost {
            return Err(lost);
        }
        // A live subscription was enrolled when it subscribed: a device the
        // workspace would only enrol now is no longer a member.
        match decision {
            Decision::Admit => {}
            Decision::AdmitAndEnrol | Decision::Deny(workspace_access::DenyReason::NotAMember) => {
                return Err(PeerError::Protocol(
                    "device was removed from workspace membership".into(),
                ));
            }
            Decision::Deny(reason) => return Err(PeerError::Protocol(reason.reason().into())),
        }
    }
    if !gone.is_empty() {
        subscribed.retain(|workspace| !gone.contains(workspace));
    }
    Ok(watched)
}

pub(super) fn session_peer_authorized(peer: &AuthenticatedPeer, session: &SessionCtx<'_>) -> bool {
    session.trust().cert(peer, unix_now_ns()).is_ok()
}

/// May this connection originate (or receive) a remote action in this workspace
/// right now? One rule for v1 `RemoteAction` and v2 `Directed`: a current,
/// unrevoked, trusted device certificate; an agent only inside its live
/// delegation; and current workspace membership. Whether it is the caller's own
/// session is a separate check, since only the caller's channel is known.
pub(super) fn action_peer_admitted(
    wr: &crate::workspace::WorkspaceRuntime,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<(), Refusal> {
    // Private authority is explicit and current: a same-owner workspace enrols an
    // owner device on Subscribe, never here.
    if !session_peer_authorized(peer, session)
        || !session.admits(wr, peer, WorkspaceOperation::RemoteAction)
    {
        return Err(Refusal::NotAMember(
            "the device may not act in the workspace".into(),
        ));
    }
    Ok(())
}

pub(super) fn unavailable_target(
    wr: &crate::workspace::WorkspaceRuntime,
    target: &[u8; 32],
) -> Refusal {
    if wr.membership.contains(target) {
        Refusal::Offline
    } else {
        Refusal::NotAMember("addressed device is not in the workspace".into())
    }
}

/// Why this connection no longer holds `peer`'s subscriber slot. A newer
/// connection of the same device took it over while the device is still
/// let in for `op`: a normal handover (a page that reconnected, a second
/// tab). Anything else, a slot the workspace dropped or a device it no longer
/// admits, is the authorization change it is, replaced or not.
pub(super) fn lost_session(
    wr: &crate::workspace::WorkspaceRuntime,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    op: WorkspaceOperation,
) -> PeerError {
    if wr.subscribers.contains_key(&peer.device_public_key)
        && session_peer_authorized(peer, session)
        && matches!(session.decide(wr, peer, op), Decision::Admit)
    {
        PeerError::Superseded
    } else {
        PeerError::Protocol("subscriber authorization changed".into())
    }
}

pub(super) fn current_session(
    wr: &crate::workspace::WorkspaceRuntime,
    peer: &AuthenticatedPeer,
    tx: &mpsc::Sender<Envelope>,
) -> bool {
    wr.subscribers
        .get(&peer.device_public_key)
        .is_some_and(|sub| sub.tx.same_channel(tx))
}
