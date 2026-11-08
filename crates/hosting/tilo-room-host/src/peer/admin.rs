//! A device's workspace administration on its session: what a page asks of its
//! host, answered by the same governance every door answers with
//! ([`crate::workspaces::Workspaces::page_rooms`]).
//!
//! The session's authenticated certificate is who asks; the request names what
//! it wants done with which workspace, and its own authority decides. A host
//! that governs no workspace refuses every request rather than staying silent.

use super::{PeerError, SessionCtx};
use crate::auth::AuthenticatedPeer;
use crate::transport::Envelope;
use tilo_relay_protocol::workspaces::{WorkspacesReply, WorkspacesRequest};

/// Answer the [`WorkspacesRequest`] in `frame`, asked as `request_id`.
pub(super) async fn handle_workspaces(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    request_id: u64,
    frame: &[u8],
) -> Result<Option<Envelope>, PeerError> {
    let reply = match postcard::from_bytes::<WorkspacesRequest>(frame) {
        Ok(request) => match session.admin {
            Some(admin) => admin.page_rooms(&peer.cert, request).await,
            None => WorkspacesReply::Refused("this host governs no workspace".into()),
        },
        Err(error) => WorkspacesReply::Refused(format!("the request does not decode: {error}")),
    };
    let frame = postcard::to_stdvec(&reply).map_err(|error| {
        PeerError::Protocol(format!("a workspace answer does not encode: {error}"))
    })?;
    Ok(Some(Envelope::WorkspacesReply { request_id, frame }))
}
