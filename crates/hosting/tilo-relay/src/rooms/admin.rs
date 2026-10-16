//! Removing a member from a room this relay governs: the one command the
//! relay still answers at
//! [`ROOM_ADMIN_ALPN`](tilo_relay_session::rooms::ROOM_ADMIN_ALPN).
//!
//! Everything else this door once answered is served as actions on the
//! relay's own remote-action envelope (`super::actions`): a room's
//! governance as `tilo.workspace`, the host's own operations (create, list,
//! enter) as `tilo.host`, and a member's own namespace claim and effect
//! receipt as `tilo.member`. A device still sending one of those here is
//! refused with the action that replaced it. Removing a member stays here
//! because the actions do not run it until the person's own confirmation can
//! be asked for; this door goes when they do.
//!
//! One bounded postcard request and reply per bidirectional stream, as in the
//! workspace-join protocol. The request names the caller's certificate; the
//! relay accepts it only for the authenticated Iroh endpoint it arrived on
//! ([`auth::authenticate_endpoint_bound`]).

use iroh::protocol::{AcceptError, ProtocolHandler};
use tilo_relay_session::rooms::{
    ROOM_ADMIN_EXCHANGE_TIMEOUT as EXCHANGE_TIMEOUT, ROOM_ADMIN_MAX_REPLY_BYTES as MAX_REPLY_BYTES,
    ROOM_ADMIN_MAX_REQUEST_BYTES as MAX_REQUEST_BYTES, RoomAdminError, RoomAdminRequest,
    RoomCommand, RoomReply,
};
use tilo_user_identity::DeviceCert;

use crate::auth::{self, AuthError, AuthenticatedPeer, HandshakeTrust};

use super::Rooms;
use tilo_lock::RwLockRecover as _;

const MAX_CONCURRENT_EXCHANGES: usize = 64;

/// Serves [`RoomCommand::Remove`] for the rooms this relay governs, and
/// refuses every other [`RoomCommand`] with the action that replaced it.
#[derive(Clone, Debug)]
pub struct RoomAdminProtocol {
    rooms: Rooms,
    exchanges: std::sync::Arc<tokio::sync::Semaphore>,
}

impl RoomAdminProtocol {
    /// Serve `rooms`.
    #[must_use]
    pub fn new(rooms: Rooms) -> Self {
        Self {
            rooms,
            exchanges: std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_EXCHANGES)),
        }
    }

    async fn exchange(
        &self,
        connection: &iroh::endpoint::Connection,
    ) -> Result<(), RoomAdminError> {
        let remote = *connection.remote_id().as_bytes();
        let (mut send, mut recv) = connection
            .accept_bi()
            .await
            .map_err(|error| RoomAdminError::Transport(error.to_string()))?;
        let bytes = crate::frame::read_frame(&mut recv, MAX_REQUEST_BYTES)
            .await
            .map_err(|error| RoomAdminError::Transport(error.to_string()))?;
        let request: RoomAdminRequest = postcard::from_bytes(&bytes)
            .map_err(|error| RoomAdminError::Codec(error.to_string()))?;
        let reply = match self.authenticate(&request.device_cert, &remote) {
            Ok(_) => self.execute(&request.device_cert, request.command).await,
            Err(error) => RoomReply::Refused(error.to_string()),
        };
        let bytes = postcard::to_stdvec(&reply)
            .map_err(|error| RoomAdminError::Codec(error.to_string()))?;
        if bytes.len() > MAX_REPLY_BYTES {
            return Err(RoomAdminError::Codec(
                "room reply exceeds the wire limit".into(),
            ));
        }
        crate::frame::write_frame(&mut send, &bytes)
            .await
            .map_err(|error| RoomAdminError::Transport(error.to_string()))?;
        send.finish()
            .map_err(|error| RoomAdminError::Transport(error.to_string()))?;
        connection.closed().await;
        Ok(())
    }

    /// The caller's certificate, bound to the Iroh endpoint `remote` that
    /// QUIC already authenticated, valid and unrevoked. Whether it may
    /// remove someone is the room's decision ([`Rooms::authorize`]): the
    /// owner's devices may, and members as far as their room's governance
    /// lets them.
    fn authenticate(
        &self,
        device: &DeviceCert,
        remote: &[u8; 32],
    ) -> Result<AuthenticatedPeer, AuthError> {
        let host = &self.rooms.inner.host;
        let revocations = host.revocations.read_recover();
        auth::authenticate_endpoint_bound(
            device,
            remote,
            &host.cert.user_public_key,
            HandshakeTrust::AnyCertifiedDevice,
            &revocations,
            tilo_clock::unix_now_ns(),
        )
    }

    async fn execute(&self, device: &DeviceCert, command: RoomCommand) -> RoomReply {
        if let Some((provider, action)) = command.replaced_by() {
            return RoomReply::Refused(format!(
                "this relay serves that as the `{action}` action of `{provider}`"
            ));
        }
        let RoomCommand::Remove { room, request } = command else {
            return RoomReply::Refused("not a room administration command".into());
        };
        self.rooms
            .remove_member(&room, device, request)
            .await
            .map_or_else(
                |error| RoomReply::Refused(error.to_string()),
                |done| RoomReply::Governance {
                    generation: done.generation,
                },
            )
    }
}

impl ProtocolHandler for RoomAdminProtocol {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        crate::workspace_join::bounded_accept(
            &self.exchanges,
            EXCHANGE_TIMEOUT,
            "room administration",
            self.exchange(&connection),
        )
        .await
    }
}
