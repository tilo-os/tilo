//! Who may still act on this connection: the relay-wide certificate and
//! membership checks the session handlers share, and the periodic
//! subscriber audit that ends a session whose access lapsed.

use super::{
    AuthenticatedPeer, Decision, Envelope, PeerError, RoomOperation, SessionCtx, UnavailableReason,
    mpsc, room_access, unix_now_ns,
};

/// Membership + revocation re-check for every workspace this peer is
/// subscribed to, run when [`AccessAudit`] says. Reloads membership
/// from disk when the membership watch saw it change, or on every audit
/// while no live watch covers the room, and ends the session if the
/// device's own cert was revoked or it fell out of membership.
///
/// Returns whether every subscribed room's membership is change-notified,
/// so the next audit can wait for a change instead of the disk.
pub(super) async fn audit_subscriber_access(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
) -> Result<bool, PeerError> {
    let mut watched = true;
    // Relay-wide first, so a revoked or untrusted device ends even with no
    // bound room left to ask.
    session
        .trust()
        .cert(peer, unix_now_ns())
        .map_err(|reason| PeerError::Protocol(reason.reason().into()))?;
    for workspace_id in subscribed {
        let Some(workspace) = session.workspaces.lock().await.get(workspace_id).cloned() else {
            continue;
        };
        let workspace = crate::workspace_io::fresh_membership(&workspace).await?;
        watched &= workspace.membership_is_current();
        let lost = (!current_session(&workspace, peer, session.outbound))
            .then(|| lost_session(&workspace, peer, session, RoomOperation::Subscribe));
        let decision = session.decide(&workspace, peer, RoomOperation::Subscribe);
        drop(workspace);
        if let Some(lost) = lost {
            return Err(lost);
        }
        // A live subscription was enrolled when it subscribed: a device the
        // room would only enrol now is no longer a member.
        match decision {
            Decision::Admit => {}
            Decision::AdmitAndEnrol | Decision::Deny(room_access::DenyReason::NotAMember) => {
                return Err(PeerError::Protocol(
                    "device was removed from workspace membership".into(),
                ));
            }
            Decision::Deny(reason) => return Err(PeerError::Protocol(reason.reason().into())),
        }
    }
    Ok(watched)
}

pub(super) fn current_action_peer(peer: &AuthenticatedPeer, session: &SessionCtx<'_>) -> bool {
    session_peer_authorized(peer, session)
        && (peer.role == crate::auth::PeerRole::Device || &peer.cert == session.own_device_cert)
}

pub(super) fn session_peer_authorized(peer: &AuthenticatedPeer, session: &SessionCtx<'_>) -> bool {
    session.trust().cert(peer, unix_now_ns()).is_ok()
}

/// May this connection originate (or receive) a remote action in this room
/// right now? One rule for v1 `RemoteAction` and v2 `Directed`: a current,
/// unrevoked, trusted device certificate; an agent only inside its live
/// delegation; and current room membership. Whether it is the caller's own
/// session is a separate check, since only the caller's channel is known.
pub(super) fn action_peer_admitted(
    wr: &crate::workspace::WorkspaceRuntime,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<(), UnavailableReason> {
    // Private authority is explicit and current: a same-owner room enrols an
    // owner device on Subscribe, never here.
    if !current_action_peer(peer, session) || !session.admits(wr, peer, RoomOperation::RemoteAction)
    {
        return Err(UnavailableReason::NotAuthorised);
    }
    Ok(())
}

pub(super) fn unavailable_target(
    wr: &crate::workspace::WorkspaceRuntime,
    target: &[u8; 32],
) -> UnavailableReason {
    if wr.membership.contains(target) {
        UnavailableReason::Offline
    } else {
        UnavailableReason::NotAuthorised
    }
}

/// Why this connection no longer holds `peer`'s subscriber slot. A newer
/// connection of the same device took it over while the device is still
/// let in for `op`: a normal handover (a page that reconnected, a second
/// tab). Anything else, a slot the room dropped or a device it no longer
/// admits, is the authorization change it is, replaced or not.
pub(super) fn lost_session(
    wr: &crate::workspace::WorkspaceRuntime,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    op: RoomOperation,
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
