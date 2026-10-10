//! Owner-only room administration and admitted members' self-attributed audit:
//! the relay's side of
//! [`ROOM_ADMIN_ALPN`](tilo_relay_session::rooms::ROOM_ADMIN_ALPN).
//!
//! One bounded postcard request and reply per bidirectional stream, as in the
//! workspace-join protocol. The request names the caller's certificate; the
//! relay accepts it only for the authenticated Iroh endpoint it arrived on
//! ([`auth::authenticate_endpoint_bound`]).
//! The wire vocabulary and the device's `RoomAdminClient` live in
//! `tilo_relay_session::rooms`.

use iroh::protocol::{AcceptError, ProtocolHandler};
use tilo_relay_session::rooms::{
    ROOM_ADMIN_EXCHANGE_TIMEOUT as EXCHANGE_TIMEOUT, ROOM_ADMIN_MAX_REPLY_BYTES as MAX_REPLY_BYTES,
    ROOM_ADMIN_MAX_REQUEST_BYTES as MAX_REQUEST_BYTES, RoomAdminError, RoomAdminRequest,
    RoomCommand, RoomReply,
};
use tilo_user_identity::DeviceCert;

use crate::auth::{self, AuthError, AuthenticatedPeer, HandshakeTrust};

use super::Rooms;

const MAX_CONCURRENT_EXCHANGES: usize = 64;

/// Serves [`RoomCommand`]s for the rooms this relay governs.
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
        let bytes = crate::framing::read_frame(&mut recv, MAX_REQUEST_BYTES)
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
        crate::framing::write_frame(&mut send, &bytes)
            .await
            .map_err(|error| RoomAdminError::Transport(error.to_string()))?;
        send.finish()
            .map_err(|error| RoomAdminError::Transport(error.to_string()))?;
        connection.closed().await;
        Ok(())
    }

    /// The caller's certificate, bound to the Iroh endpoint `remote` that
    /// QUIC already authenticated, valid and unrevoked. Who may run which
    /// command is each command's own decision: the owner's devices
    /// administer, and admitted members record their own receipts.
    fn authenticate(
        &self,
        device: &DeviceCert,
        remote: &[u8; 32],
    ) -> Result<AuthenticatedPeer, AuthError> {
        let host = &self.rooms.inner.host;
        let revocations = host
            .revocations
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let rooms = &self.rooms;
        let result = match command {
            RoomCommand::Create { name } => {
                match rooms.authorize(device, tilo_clock::unix_now_ns(), "room.create") {
                    Ok(()) => rooms
                        .create(&name)
                        .await
                        .map(RoomReply::Room)
                        .map_err(Into::into),
                    Err(error) => Err(error),
                }
            }
            RoomCommand::List => {
                match rooms.authorize(device, tilo_clock::unix_now_ns(), "room.list") {
                    Ok(()) => Ok(RoomReply::Rooms(rooms.list().await)),
                    Err(error) => Err(error),
                }
            }
            RoomCommand::Enter { room } => rooms
                .enter(&room, device)
                .await
                .map(|changed| RoomReply::Entered { changed }),
            RoomCommand::Invite { room, request } => rooms
                .issue(&room, device, request)
                .await
                .map(RoomReply::Invitation),
            RoomCommand::Cancel { room, request } => rooms
                .cancel(&room, device, request)
                .await
                .map(|()| RoomReply::Done),
            RoomCommand::Pending { room } => {
                rooms.pending(&room, device).await.map(RoomReply::Pending)
            }
            RoomCommand::SetRole { room, request } => rooms
                .set_member_role(&room, device, request)
                .await
                .map(|done| RoomReply::Governance {
                    generation: done.generation,
                }),
            RoomCommand::Remove { room, request } => rooms
                .remove_member(&room, device, request)
                .await
                .map(|done| RoomReply::Governance {
                    generation: done.generation,
                }),
            RoomCommand::RecordEffectReceipt {
                room,
                invocation,
                redacted,
            } => rooms
                .record_member_effect_receipt(&room, device, invocation, &redacted)
                .await
                .map(|done| RoomReply::Governance {
                    generation: done.generation,
                }),
        };
        result.unwrap_or_else(|error| RoomReply::Refused(error.to_string()))
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
