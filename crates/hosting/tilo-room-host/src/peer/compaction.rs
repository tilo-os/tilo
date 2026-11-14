//! `CompactVolunteer` and `Compacted`: members volunteering to compact sealed
//! routes and the coverage statements the host acts on.

use super::{AuthenticatedPeer, Envelope, SessionCtx};
use tilo_relay_protocol::WorkspaceId;

/// A member volunteering to compact sealed routes, or its coverage
/// statement for one: the host removes the entries the statement names. A
/// refused statement removes nothing and only costs the member its snapshot
/// upload.
pub(super) async fn handle_compaction(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
    env: Envelope,
) {
    let Some(compactor) = session.compactor else {
        return;
    };
    let (Envelope::CompactVolunteer { workspace_id } | Envelope::Compacted { workspace_id, .. }) =
        env
    else {
        return;
    };
    if !subscribed.contains(&workspace_id) {
        return;
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return;
    };
    // Compaction is a device's own act: a host carrying its members' changes
    // does not compact on their behalf.
    if ws.lock().await.attached(&peer.device_public_key) {
        return;
    }
    let Envelope::Compacted {
        statement,
        signature,
        ..
    } = env
    else {
        compactor.volunteer(workspace_id, peer.device_public_key);
        return;
    };
    if let Err(error) = compactor
        .compacted(
            &ws,
            workspace_id,
            peer.device_public_key,
            &statement,
            &signature,
        )
        .await
    {
        tracing::warn!(workspace_id = %workspace_id, %error, "refused a compaction statement");
    }
}
