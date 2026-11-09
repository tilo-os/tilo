//! Who is here in a workspace, asked over the session: a page keeps no HTTPS
//! session to ask through, so it asks the host it is joined to.
//!
//! The answer is the host's: the live plane's members, what each person is
//! doing in their own words, and the profiles the host published. A host that
//! serves no such account says so instead of staying silent.

use std::future::Future;
use std::pin::Pin;

use super::{PeerError, SessionCtx};
use crate::auth::AuthenticatedPeer;
use crate::transport::Envelope;
use tilo_relay_protocol::web::{HereReply, HereRequest};

/// The host's account of who is here in a workspace, as a session asks it.
pub trait HereBoard: Send + Sync {
    /// Answer `request`, asked by `peer` on its session.
    fn answer<'a>(
        &'a self,
        peer: &'a AuthenticatedPeer,
        request: HereRequest,
    ) -> Pin<Box<dyn Future<Output = Result<HereReply, String>> + Send + 'a>>;
}

/// Answer the [`HereRequest`] in `frame`, asked as `request_id`.
pub(super) async fn handle_here(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    request_id: u64,
    frame: &[u8],
) -> Result<Option<Envelope>, PeerError> {
    let reply = match postcard::from_bytes::<HereRequest>(frame) {
        Ok(request) => match session.here {
            Some(here) if request.agent => {
                // The device runs an agent, and this session is one run of
                // it: the person's agent door lists it while it lasts.
                if let (Some(admin), Some(stop)) = (session.admin, session.stop) {
                    admin.note_agent(&peer.cert, &request.label, stop);
                }
                here.answer(peer, request).await
            }
            Some(here) => here.answer(peer, request).await,
            None => Err("this host keeps no account of who is here".into()),
        },
        Err(error) => Err(format!("the request does not decode: {error}")),
    };
    let frame = postcard::to_stdvec(&reply)
        .map_err(|error| PeerError::Protocol(format!("who is here does not encode: {error}")))?;
    Ok(Some(Envelope::HereReply { request_id, frame }))
}
