//! Opening an on-demand record: a record a joining device is not replayed
//! ([`crate::store::record_sync`]) is sent to it once it asks.

use super::{AuthenticatedPeer, Envelope, PeerError, SessionCtx, WorkspaceOperation};

/// Handle `OpenRecord`: under the admission a replay has, mark the record
/// held by the device, so its live changes reach it from now on, then send
/// it everything retained of the record and, after it, [`Envelope::RecordOpened`].
/// A chain's head ([`neighbour_wire::chain_head`]) sends the chain's newest
/// part, and the device follows the chain's heads from then on.
/// Marking first means no change committed meanwhile is missed; one that is
/// both sent and retained arrives twice, which a record takes as once.
pub(super) async fn handle_open_record(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    envelope: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    let Envelope::OpenRecord {
        workspace_id,
        channel,
    } = envelope
    else {
        return Ok(None);
    };
    let refused = |reason: &str| {
        Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: reason.into(),
        }))
    };
    if !subscribed.contains(&workspace_id) {
        return refused("open a record of a subscribed room");
    }
    // A browser environment's device opens its own record and no other.
    if session
        .environment(&peer.device_public_key)
        .is_some_and(|scope| !scope.reaches(Some(channel)))
    {
        return refused(crate::workspace_access::DenyReason::OutOfEnvironmentScope.reason());
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return refused("workspace not bound on this relay");
    };
    // A chain's head names whichever part is the chain's newest: that part
    // is what is sent.
    let head = {
        let store = ws.lock().await.store.clone();
        tokio::task::spawn_blocking(move || store.chain_head_channel(&channel))
            .await
            .map_err(|error| PeerError::Protocol(format!("record read: {error}")))??
    };
    let (store, governed) = {
        let mut runtime = crate::workspace_io::fresh_membership(&ws).await?;
        if !session.admits(&runtime, peer, WorkspaceOperation::State) {
            return refused("device not in workspace membership");
        }
        let device = &peer.device_public_key;
        runtime.hold_record(device, channel);
        if let Some(head) = head {
            runtime.hold_record(device, head);
        }
        (
            runtime.store.clone(),
            runtime
                .workspace_authority
                .as_ref()
                .and_then(crate::workspaces::WorkspaceAuthority::workspaces),
        )
    };
    // Sent whether or not the device holds the head already: an answer of no
    // frames says the chain has no part at all, never "you have it", so a
    // device whose copy has not opened yet never takes the chain as empty.
    let read = head.unwrap_or(channel);
    let frames = tokio::task::spawn_blocking(move || {
        let mut frames = store.record_frames(&read)?;
        // A linked member's write whose copy to the relay has not landed yet
        // is kept by the workspace's link: sent from there, so the page never
        // hears the record is absent while the workspace's host keeps it.
        if let Some(governed) = governed {
            match governed.link_frames_lacking(workspace_id, read, &frames) {
                Ok(lacking) => frames.extend(lacking),
                Err(error) => {
                    return Err(PeerError::Protocol(format!(
                        "the workspace link's history of the record: {error}"
                    )));
                }
            }
        }
        Ok::<_, PeerError>(frames)
    })
    .await
    .map_err(|error| PeerError::Protocol(format!("record read: {error}")))??;
    let sent = u32::try_from(frames.len()).unwrap_or(u32::MAX);
    // On the queue the frames go out on, behind them: a reply is written by
    // another task and could overtake them, so a device would hear the
    // record was sent before it arrived.
    let opened = Envelope::RecordOpened {
        workspace_id,
        channel,
        frames: sent,
    };
    for envelope in frames.into_iter().chain(std::iter::once(opened)) {
        session
            .outbound
            .send(envelope)
            .await
            .map_err(|_| PeerError::Protocol("outbound writer closed".into()))?;
    }
    Ok(None)
}
