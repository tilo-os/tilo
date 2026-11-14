//! A page's fetch grant: it reads sites that send no CORS headers through the
//! host it joined ([`tilo_relay_protocol::web::FETCH_PATH`]), so the host mints
//! that connection's token as the session starts and takes it back when the
//! connection ends. A host that serves no proxy grants nothing.

use super::{Envelope, SessionCtx, mpsc};
use crate::auth::AuthenticatedPeer;

/// The host's fetch proxy, as a session asks it for this connection's token.
pub trait FetchGrants: Send + Sync {
    /// A token naming this connection of `device` of `user`; `None` when the
    /// host serves no proxy.
    fn grant(&self, user: [u8; 32], device: [u8; 32]) -> Option<[u8; 32]>;

    /// Take back `token`: the connection that held it is gone.
    fn revoke(&self, token: [u8; 32]);
}

/// The token this connection holds for the host's fetch proxy, minted as the
/// session starts and taken back when it ends.
pub(super) struct Grant<'a> {
    grants: Option<&'a dyn FetchGrants>,
    token: Option<[u8; 32]>,
}

impl<'a> Grant<'a> {
    /// Mint this connection's token and hand it to the page before anything
    /// else it reads, so a fetch it makes as soon as it is up carries one.
    pub(super) fn open(
        session: &SessionCtx<'a>,
        peer: &AuthenticatedPeer,
        outbound: &mpsc::Sender<Envelope>,
    ) -> Self {
        let grants = session.fetch;
        let token = grants.and_then(|grants| {
            let token = grants.grant(peer.user_public_key, peer.device_public_key)?;
            let _ = outbound.try_send(Envelope::FetchGrant { token });
            Some(token)
        });
        Self { grants, token }
    }

    /// Take the token back: no connection holds it any more.
    pub(super) fn end(&self) {
        if let (Some(grants), Some(token)) = (self.grants, self.token) {
            grants.revoke(token);
        }
    }
}
