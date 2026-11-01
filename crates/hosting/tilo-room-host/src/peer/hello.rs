//! The hello a host dialing this one opens its session with: which
//! workspaces it hosts, so both sides attach where they share one.

use super::{AuthenticatedPeer, Envelope, PeerError, SessionCtx};

/// Handle the dialer's hello: record it as a host attached to every
/// workspace both host.
pub(super) async fn handle_host_hello(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    hosted_workspaces: Vec<u64>,
) -> Result<Option<Envelope>, PeerError> {
    // Only a host of this host's own person attaches: a device of another
    // person never does.
    if peer.user_public_key != *session.own_user_public_key {
        return Ok(None);
    }
    // Compute the intersection with what this host hosts and record the
    // dialer as a host attached to each shared workspace. On its next
    // `Subscribe` (which it is about to send for each intersection id), the
    // existing subscriber path enrols it and the fan-out filter takes care
    // of the rest.
    let mut hosted_here: Vec<u64> = Vec::new();
    let mut governed = Vec::new();
    let wsmap = session.workspaces.lock().await;
    for id in &hosted_workspaces {
        if let Some(ws) = wsmap.get(id).cloned() {
            let mut runtime = ws.lock().await;
            runtime.note_attached_host(peer.device_public_key);
            runtime
                .attach_sessions
                .insert(peer.device_public_key, session.disconnect.clone());
            governed.extend(runtime.room_authority.clone());
            drop(runtime);
            hosted_here.push(*id);
        }
    }
    drop(wsmap);
    // A host serving a room governed here is answered with a session back
    // to it, for what its members change there.
    for authority in governed {
        if let Some(rooms) = authority.rooms() {
            rooms
                .answer_host(
                    authority.room(),
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
