//! The hello a host dialing this one opens its session with: which
//! workspaces it hosts, so both sides attach where they share one.

use super::{AuthenticatedPeer, Envelope, PeerError, SessionCtx};
use tilo_relay_protocol::WorkspaceId;

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
    let mut governed = Vec::new();
    let held: Vec<_> = {
        let wsmap = session.workspaces.lock().await;
        hosted_workspaces
            .iter()
            .filter_map(|id| Some((*id, wsmap.get(id).cloned()?)))
            .collect()
    };
    for (id, ws) in held {
        let (authority, replica) = {
            let runtime = ws.lock().await;
            (
                runtime.workspace_authority.clone(),
                runtime.hosted_replica.clone(),
            )
        };
        if !own {
            let named = match authority.as_ref().and_then(|authority| {
                Some((authority.workspaces()?, authority.workspace().to_owned()))
            }) {
                Some((workspaces, workspace)) => {
                    workspaces
                        .names_host(&workspace, peer.device_public_key)
                        .await
                }
                None => replica.is_some_and(|replica| replica.names_holder(peer.device_public_key)),
            };
            if !named {
                continue;
            }
        }
        let mut runtime = ws.lock().await;
        runtime.note_attached_host(peer.device_public_key);
        runtime
            .attach_sessions
            .insert(peer.device_public_key, session.disconnect.clone());
        drop(runtime);
        governed.extend(authority);
        hosted_here.push(id);
    }
    if !own && hosted_here.is_empty() {
        return Ok(None);
    }
    // A host serving a workspace governed here is answered with a session back
    // to it, for what its members change there.
    for authority in governed {
        if let Some(workspaces) = authority.workspaces() {
            workspaces
                .answer_host(
                    authority.workspace(),
                    peer.device_public_key,
                    session.disconnect.clone(),
                )
                .await;
        }
    }
    Ok(Some(Envelope::PeerRelayHello {
        hosted_workspaces: hosted_here,
    }))
}
