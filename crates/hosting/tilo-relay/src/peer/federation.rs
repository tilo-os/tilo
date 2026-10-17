//! Peer-relay federation: `PeerRelayHello` and anti-entropy `SyncRequest`.

use super::{AuthenticatedPeer, Envelope, PeerError, RoomOperation, SessionCtx, StateSource};

/// Handle `PeerRelayHello`: record the dialer as a federated peer
/// relay for every workspace we both host.
pub(super) async fn handle_peer_relay_hello(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    hosted_workspaces: Vec<u64>,
) -> Result<Option<Envelope>, PeerError> {
    // Peer-relay handshake: silently ignore if the peer's cert is not
    // host-capable — otherwise a plain device could try to smuggle
    // its way into the federation set.
    if peer.role != crate::auth::PeerRole::PeerRelay
        || peer.user_public_key != *session.own_user_public_key
    {
        return Ok(None);
    }
    // Compute the intersection with what THIS relay hosts and record
    // the dialer's device pubkey as a federated peer relay for each
    // shared workspace. On next `Subscribe` (which the peer relay is
    // about to send for each intersection id), the existing
    // subscriber path enrols it with role `PeerRelay` and the
    // fan-out filter takes care of the rest.
    let mut hosted_here: Vec<u64> = Vec::new();
    let wsmap = session.workspaces.lock().await;
    for id in &hosted_workspaces {
        if let Some(ws) = wsmap.get(id).cloned() {
            ws.lock().await.record_peer_relay(peer.device_public_key);
            hosted_here.push(*id);
        }
    }
    drop(wsmap);
    Ok(Some(Envelope::PeerRelayHello {
        hosted_workspaces: hosted_here,
    }))
}

/// Handle `SyncRequest`: anti-entropy reply with exactly the changes
/// the requester is missing, computed against this host's
/// materialized pinned doc. Read-only — does not touch subscriptions
/// or the pending queues, so a peer can reconcile whether or not it
/// is currently subscribed. Membership gates device requesters the
/// same way `Subscribe` does; peer relays are gated by cross-user
/// auth + host capability.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the membership guard keeps the roster fixed until the reply is classified"
)]
pub(super) async fn handle_sync_request(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    workspace_id: u64,
    have_heads: Vec<[u8; 32]>,
) -> Result<Option<Envelope>, PeerError> {
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: "workspace not bound on this relay".into(),
        }));
    };
    let wr = crate::workspace_io::fresh_membership(&ws).await?;
    if !session.admits(&wr, peer, RoomOperation::State) {
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: "device not in workspace membership".into(),
        }));
    }
    let expected = wr
        .subscribers
        .get(&peer.device_public_key)
        .map(|sub| sub.tx.clone());
    let source = StateSource::new(peer, session);
    if let Some(mailbox) = &wr.sync_refresh
        && session.connection_live.is_some()
        && expected
            .as_ref()
            .is_some_and(|tx| tx.same_channel(session.outbound))
        && source.authorized(&wr)
    {
        mailbox.enqueue(workspace_id, ws.clone(), source.clone());
    }
    drop(wr);
    let owner = crate::persistence::handle(&ws).await;
    owner
        .submit_prepared_wait(
            have_heads.len().saturating_mul(32),
            async { Ok(()) },
            move |state, ()| Ok(state.changes(&have_heads)),
            move |result| async move {
                let changes = result?;
                let wr = crate::workspace_io::fresh_membership(&ws).await?;
                let same = match (
                    expected.as_ref(),
                    wr.subscribers.get(&source.peer.device_public_key),
                ) {
                    (None, None) => true,
                    (Some(expected), Some(current)) => expected.same_channel(&current.tx),
                    _ => false,
                };
                if source.alive()
                    && source.admits(&wr, &source.peer)
                    && same
                    && source
                        .tx
                        .try_send(Envelope::SyncResponse {
                            workspace_id,
                            delta_since: changes.clone(),
                        })
                        .is_err()
                {
                    let _ = source.disconnect.try_send(());
                }
                Ok(changes)
            },
        )
        .await?;
    Ok(None)
}
