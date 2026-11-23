//! Writing to the wire: per-session encoding, and presence batches to
//! recipients the workspace still admits.

use super::{
    AsyncWriteExt, AuthenticatedPeer, Envelope, PeerError, SessionCtx, WorkspaceOperation,
    current_session,
};
use tilo_server_protocol::WorkspaceId;

/// `envelope` as the server stream carries it, for a frame about to be
/// written, charged to its workspace's egress. `live` marks live fan-out
/// (presence forwarded as it arrived), as opposed to replies.
pub(super) fn encode_charged(
    envelope: &Envelope,
    session: &SessionCtx<'_>,
    live: bool,
) -> Result<Vec<u8>, postcard::Error> {
    let frame = tilo_delivery_protocol::transport_v2::encode_state(envelope)?;
    if let Some(workspace) = envelope.workspace_id() {
        session.metrics.add_workspace_egress(
            workspace,
            u64::try_from(frame.len()).unwrap_or(u64::MAX),
            live,
        );
    }
    Ok(frame)
}

/// Presence items one `PresenceBatch` frame carries at most.
const PRESENCE_BATCH_ITEMS: usize = 64;

/// Write the presence due for this connection, with everything else that
/// may ride along (`PresenceMailbox::take`), in one write.
pub(super) async fn write_presence<W: tokio::io::AsyncWrite + Unpin>(
    mailbox: &crate::presence_mailbox::PresenceMailbox,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    write: &mut W,
) -> Result<(), PeerError> {
    let out = encode_presence(
        mailbox.take(tokio::time::Instant::now(), true),
        peer,
        session,
    )
    .await?;
    if !out.is_empty() {
        write.write_all(&out).await?;
    }
    Ok(())
}

/// Encode `due` presence for this connection, each workspace's authorised once,
/// as `PresenceBatch` frames. Charged to each workspace's live egress.
async fn encode_presence(
    due: Vec<crate::presence_mailbox::DuePresence>,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Vec<u8>, PeerError> {
    let mut out = Vec::new();
    for run in due.chunk_by(|a, b| a.workspace_id == b.workspace_id) {
        let Some(workspace) = run.first().map(|presence| presence.workspace_id) else {
            continue;
        };
        authorize_state_recipient(workspace, peer, session).await?;
        let before = out.len();
        for chunk in run.chunks(PRESENCE_BATCH_ITEMS) {
            let items: Vec<_> = chunk
                .iter()
                .map(
                    |presence| tilo_delivery_protocol::transport_v2::PresenceItem {
                        slot: None,
                        keep_prefix: 0,
                        keep_suffix: 0,
                        middle: presence.payload.to_vec(),
                    },
                )
                .collect();
            out.extend(tilo_delivery_protocol::transport_v2::encode_presence_batch(
                workspace, &items,
            )?);
        }
        let bytes = u64::try_from(out.len().saturating_sub(before)).unwrap_or(u64::MAX);
        session.metrics.add_workspace_egress(workspace, bytes, true);
    }
    Ok(out)
}

/// Whether `peer` may still be sent `id`'s live traffic on this connection:
/// the workspace admits it, and this connection is its current subscriber.
pub(super) async fn authorize_state_recipient(
    id: WorkspaceId,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<(), PeerError> {
    let workspace = session
        .workspaces
        .lock()
        .await
        .get(&id)
        .cloned()
        .ok_or_else(|| PeerError::Protocol("workspace disappeared".into()))?;
    let runtime = workspace.lock().await;
    if !session.admits(&runtime, peer, WorkspaceOperation::State)
        || !current_session(&runtime, peer, session.outbound)
    {
        return Err(PeerError::Protocol(
            "state recipient authorization changed".into(),
        ));
    }
    Ok(())
}
