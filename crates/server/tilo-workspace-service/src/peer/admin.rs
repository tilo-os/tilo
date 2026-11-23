//! A device's workspace administration on its session: what a page asks of its
//! host, answered by the same governance every action API answers with
//! ([`crate::workspaces::Workspaces::page_workspaces`]).
//!
//! The session's authenticated certificate is who asks, and the agent it was
//! handed to, if any, asks as its grants let it; the request names what it
//! wants done with which workspace, and its own authority decides. A host
//! that governs no workspace refuses every request rather than staying silent.

use super::{PeerError, SessionCtx};
use crate::auth::AuthenticatedPeer;
use crate::transport::Envelope;
use tilo_server_protocol::workspaces::{WorkspacesReply, WorkspacesRequest};

/// Answer the [`WorkspacesRequest`] in `frame`, asked as `request_id`.
pub(super) async fn handle_workspaces(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    request_id: u64,
    frame: &[u8],
) -> Result<Option<Envelope>, PeerError> {
    let reply = match postcard::from_bytes::<WorkspacesRequest>(frame) {
        Ok(request) => match (
            session.admin,
            super::actions::session_caller(peer, session, false),
        ) {
            (Some(admin), Ok(caller)) => {
                // The session's own agent asks as its grants let it, as
                // through the workspace's actions.
                match admin.admit_page(&peer.cert, &caller, &request).await {
                    Ok(()) => admin.page_workspaces(&peer.cert, request).await,
                    Err(why) => WorkspacesReply::Refused(why),
                }
            }
            (Some(_), Err(refusal)) => WorkspacesReply::Refused(refusal.to_string()),
            (None, _) => WorkspacesReply::Refused("this host governs no workspace".into()),
        },
        Err(error) => WorkspacesReply::Refused(format!("the request does not decode: {error}")),
    };
    let frame = postcard::to_stdvec(&reply).map_err(|error| {
        PeerError::Protocol(format!("a workspace answer does not encode: {error}"))
    })?;
    Ok(Some(Envelope::WorkspacesReply { request_id, frame }))
}
