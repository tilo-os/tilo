//! The hello a host dialing this one opens its session with: which
//! workspaces it hosts, so both sides attach where they share one.

use super::{AuthenticatedPeer, Envelope, PeerError, SessionCtx};
use tilo_server_protocol::WorkspaceId;

/// Handle the dialer's hello: record it as a host attached to every
/// workspace both host.
pub(super) async fn handle_host_hello(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    hosted_workspaces: Vec<WorkspaceId>,
) -> Result<Option<Envelope>, PeerError> {
    // A host of this host's own person attaches to every workspace both hold;
    // another person's host only to a workspace whose governance names it
    // among its holders. A device that is neither attaches nowhere.
    let own = peer.user_public_key == *session.own_user_public_key;
    let mut hosted_here: Vec<WorkspaceId> = Vec::new();
    let held: Vec<_> = {
        let wsmap = session.workspaces.lock().await;
        hosted_workspaces
            .iter()
            .filter_map(|id| Some((*id, wsmap.get(id).cloned()?)))
            .collect()
    };
    for (id, ws) in held {
        let authority = ws.lock().await.workspace_authority.clone();
        if !own {
            let named = match authority.as_ref().and_then(|authority| {
                Some((authority.workspaces()?, authority.workspace().to_owned()))
            }) {
                Some((workspaces, workspace)) => {
                    workspaces
                        .names_server(&workspace, peer.device_public_key)
                        .await
                }
                None => false,
            };
            if !named {
                continue;
            }
        }
        ws.lock().await.note_attached_host(peer.device_public_key);
        hosted_here.push(id);
    }
    if !own && hosted_here.is_empty() {
        return Ok(None);
    }
    Ok(Some(Envelope::PeerServerHello {
        hosted_workspaces: hosted_here,
    }))
}
