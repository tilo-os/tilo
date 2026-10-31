//! Anti-entropy: a peer asks for the changes its materialized heads lack.

use super::{AuthenticatedPeer, Envelope, PeerError, RoomOperation, SessionCtx, StateSource};

/// Handle `SyncRequest`: reply with exactly the changes the requester is
/// missing, computed against this host's materialized pinned doc.
/// Read-only — does not touch subscriptions or the pending queues, so a
/// peer can reconcile whether or not it is currently subscribed. The
/// room's membership gates the requester, as it does on `Subscribe`.
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
