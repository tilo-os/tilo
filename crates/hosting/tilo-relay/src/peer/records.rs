//! Opening an on-demand record: a record a joining device is not replayed
//! ([`crate::store::record_sync`]) is sent to it once it asks.

use super::{AuthenticatedPeer, Envelope, PeerError, RoomOperation, SessionCtx};

/// Handle `OpenRecord`: under the admission a replay has, mark the record
/// held by the device, so its live changes reach it from now on, then send
/// it everything retained of the record and [`Envelope::RecordOpened`].
/// Marking first means no change committed meanwhile is missed; one that is
/// both sent and retained arrives twice, which a record takes as once.
pub(super) async fn handle_open_record(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    workspace_id: u64,
    channel: [u8; 32],
) -> Result<Option<Envelope>, PeerError> {
    let refused = |reason: &str| {
        Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: reason.into(),
        }))
    };
    if !subscribed.contains(&workspace_id) {
        return refused("open a record of a subscribed room");
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return refused("workspace not bound on this relay");
    };
    let store = {
        let mut runtime = crate::workspace_io::fresh_membership(&ws).await?;
        if !session.admits(&runtime, peer, RoomOperation::State) {
            return refused("device not in workspace membership");
        }
        runtime.hold_record(&peer.device_public_key, channel);
        runtime.store.clone()
    };
    let frames = tokio::task::spawn_blocking(move || store.record_frames(&channel))
        .await
        .map_err(|error| PeerError::Protocol(format!("record read: {error}")))??;
    let sent = u32::try_from(frames.len()).unwrap_or(u32::MAX);
    for frame in frames {
        session
            .outbound
            .send(frame)
            .await
            .map_err(|_| PeerError::Protocol("outbound writer closed".into()))?;
    }
    Ok(Some(Envelope::RecordOpened {
        workspace_id,
        channel,
        frames: sent,
    }))
}
