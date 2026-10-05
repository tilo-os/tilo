//! Owner-only room administration and admitted members' self-attributed audit.
//!
//! One bounded postcard request and reply per bidirectional stream, as in the
//! workspace-join protocol. A takeover alone sends a second frame, the room's
//! custody, read only once its caller and record are accepted. The request names the caller's certificate; the
//! relay accepts it only for the authenticated Iroh endpoint it arrived on.

use std::time::Duration;

use iroh::protocol::{AcceptError, ProtocolHandler};
use serde::{Deserialize, Serialize};
use tilo_peer_state::GovernedMember;
use tilo_peer_state::join::MemberRole;
use tilo_user_identity::DeviceCert;

use super::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
    RemoveMemberRequest, Rooms, SetMemberRoleRequest,
};

/// ALPN of room administration.
pub const ROOM_ADMIN_ALPN: &[u8] = b"tilo-room-admin/1";

const MAX_REQUEST_BYTES: usize = 64 * 1024;
/// Largest replica a device hands the relay to take a room over from. Read
/// only once the device and its succession record are accepted.
const MAX_CUSTODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_REPLY_BYTES: usize = 4 * 1024 * 1024;
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONCURRENT_EXCHANGES: usize = 64;

/// A governed room as its administrators see it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSummary {
    /// The room's UUID.
    pub room: String,
    /// The relay workspace carrying the room.
    pub workspace_id: u64,
    /// The workspace's name.
    pub name: String,
    /// The room's committed governance generation.
    pub generation: u64,
    /// Every person governance records, removed ones included.
    pub members: Vec<GovernedMember>,
}

/// What an administering device asks of the relay. `room` is a room UUID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomCommand {
    /// Create a governed workspace named `name`.
    Create {
        /// Its name, 1–80 printable characters.
        name: String,
    },
    /// Every governed room.
    List,
    /// Admit the calling device to `room` as its owner.
    Enter {
        /// The room.
        room: String,
    },
    /// Issue an invitation to `room`.
    Invite {
        /// The room.
        room: String,
        /// The invitation.
        request: IssueInvitationRequest,
    },
    /// Cancel an invitation to `room`.
    Cancel {
        /// The room.
        room: String,
        /// The invitation.
        request: CancelInvitationRequest,
    },
    /// The invitations to `room` still redeemable.
    Pending {
        /// The room.
        room: String,
    },
    /// Change a member's role in `room`.
    SetRole {
        /// The room.
        room: String,
        /// The change.
        request: SetMemberRoleRequest,
    },
    /// Remove a member from `room`.
    Remove {
        /// The room.
        room: String,
        /// The removal.
        request: RemoveMemberRequest,
    },
    /// Record only the authenticated admitted person's bounded terminal audit.
    /// This grants no administration, application authorship or other authority.
    RecordEffectReceipt {
        /// The governed room.
        room: String,
        /// The broker's terminal invocation identifier.
        invocation: uuid::Uuid,
        /// Canonical redacted public receipt; never action arguments or results.
        redacted: Vec<u8>,
    },
    /// Govern the room `succession` hands this relay, signed by the
    /// identity's root after it revoked the room's previous governor. The
    /// request is followed by a [`TakeOverCustody`] frame.
    TakeOver {
        /// The root-signed record naming this relay as successor.
        succession: Box<tilo_peer_state::succession::GovernorSuccession>,
    },
}

/// The room state a device hands the relay with [`RoomCommand::TakeOver`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TakeOverCustody {
    /// The device's durable replica of the room, pinned to the previous
    /// governor; without it the relay uses what it retains of the room.
    replica: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct RoomAdminRequest {
    device_cert: DeviceCert,
    command: RoomCommand,
}

/// The relay's answer to one [`RoomCommand`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomReply {
    /// The created room.
    Room(RoomSummary),
    /// Every governed room.
    Rooms(Vec<RoomSummary>),
    /// The caller is in the room.
    Entered {
        /// Whether governance changed to admit it.
        changed: bool,
    },
    /// The issued invitation.
    Invitation(IssuedInvitation),
    /// The redeemable invitations.
    Pending(Vec<PendingInvitation>),
    /// A committed governance change.
    Governance {
        /// The room generation that carries it.
        generation: u64,
    },
    /// Done, with nothing to report.
    Done,
    /// Refused, and why.
    Refused(String),
}

/// Why a room administration exchange failed.
#[derive(Debug, thiserror::Error)]
pub enum RoomAdminError {
    /// The connection or stream failed.
    #[error("room administration transport: {0}")]
    Transport(String),
    /// A message could not be encoded or decoded, or was unexpected.
    #[error("room administration encoding: {0}")]
    Codec(String),
    /// The exchange did not finish in time.
    #[error("room administration timed out")]
    Timeout,
    /// The relay refused the command.
    #[error("room administration refused: {0}")]
    Refused(String),
}

/// Serves [`RoomCommand`]s for the rooms this relay governs.
#[derive(Clone, Debug)]
pub struct RoomAdminProtocol {
    rooms: Rooms,
    exchanges: std::sync::Arc<tokio::sync::Semaphore>,
    /// One custody frame in memory at a time.
    custody: std::sync::Arc<tokio::sync::Semaphore>,
}

impl RoomAdminProtocol {
    /// Serve `rooms`.
    #[must_use]
    pub fn new(rooms: Rooms) -> Self {
        Self {
            rooms,
            exchanges: std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_EXCHANGES)),
            custody: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
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
        let reply = if request.device_cert.device_public_key != remote {
            RoomReply::Refused("certificate does not match the authenticated endpoint".into())
        } else if let RoomCommand::TakeOver { succession } = request.command {
            self.take_over(&request.device_cert, *succession, &mut recv)
                .await
        } else {
            self.execute(&request.device_cert, request.command).await
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

    /// Read the replica a takeover hands over only once the caller and its
    /// record are accepted, then take the room over.
    async fn take_over(
        &self,
        device: &DeviceCert,
        succession: tilo_peer_state::succession::GovernorSuccession,
        recv: &mut iroh::endpoint::RecvStream,
    ) -> RoomReply {
        if let Err(error) = self.rooms.check_take_over(device, &succession).await {
            let _ = recv.stop(0u32.into());
            return RoomReply::Refused(error.to_string());
        }
        let Ok(_custody) = self.custody.clone().try_acquire_owned() else {
            let _ = recv.stop(0u32.into());
            return RoomReply::Refused("another room is being taken over".into());
        };
        let custody = match crate::framing::read_frame(recv, MAX_CUSTODY_BYTES).await {
            Ok(bytes) => postcard::from_bytes::<TakeOverCustody>(&bytes),
            Err(error) => return RoomReply::Refused(format!("room custody: {error}")),
        };
        match custody {
            Ok(custody) => self
                .rooms
                .take_over(device, succession, custody.replica)
                .await
                .map_or_else(
                    |error| RoomReply::Refused(error.to_string()),
                    RoomReply::Room,
                ),
            Err(error) => RoomReply::Refused(format!("room custody: {error}")),
        }
    }

    async fn execute(&self, device: &DeviceCert, command: RoomCommand) -> RoomReply {
        let rooms = &self.rooms;
        let result = match command {
            RoomCommand::Create { name } => {
                match rooms.authorize(device, super::now_ns(), "room.create") {
                    Ok(()) => rooms
                        .create(&name)
                        .await
                        .map(RoomReply::Room)
                        .map_err(Into::into),
                    Err(error) => Err(error),
                }
            }
            RoomCommand::List => match rooms.authorize(device, super::now_ns(), "room.list") {
                Ok(()) => Ok(RoomReply::Rooms(rooms.list().await)),
                Err(error) => Err(error),
            },
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
            RoomCommand::TakeOver { .. } => Err(super::CollaborationError::new(
                "a takeover carries the room's custody",
            )),
        };
        result.unwrap_or_else(|error| RoomReply::Refused(error.to_string()))
    }
}

impl ProtocolHandler for RoomAdminProtocol {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let _permit = self.exchanges.clone().try_acquire_owned().map_err(|_| {
            AcceptError::from_err(std::io::Error::other(
                "room administration concurrency limit reached",
            ))
        })?;
        n0_future::time::timeout(EXCHANGE_TIMEOUT, self.exchange(&connection))
            .await
            .map_err(|_| {
                AcceptError::from_err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "room administration timed out",
                ))
            })?
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))
    }
}

/// A certified device using governed commands.
/// Administration is owner-only; terminal audit is restricted to its own admitted person.
#[derive(Clone, Debug)]
pub struct RoomAdminClient {
    endpoint: iroh::Endpoint,
    device_cert: DeviceCert,
}

impl RoomAdminClient {
    /// Administer from `endpoint`, whose key is `device_cert`'s device key.
    #[must_use]
    pub const fn new(endpoint: iroh::Endpoint, device_cert: DeviceCert) -> Self {
        Self {
            endpoint,
            device_cert,
        }
    }

    /// Send `command` to the relay at `relay` and return its reply.
    /// A refusal is returned as [`RoomAdminError::Refused`].
    ///
    /// # Errors
    ///
    /// Returns why the exchange failed or the relay refused it.
    pub async fn send(
        &self,
        relay: iroh::EndpointAddr,
        command: RoomCommand,
    ) -> Result<RoomReply, RoomAdminError> {
        self.send_with(relay, command, None).await
    }

    async fn send_with(
        &self,
        relay: iroh::EndpointAddr,
        command: RoomCommand,
        custody: Option<Vec<u8>>,
    ) -> Result<RoomReply, RoomAdminError> {
        let reply =
            n0_future::time::timeout(EXCHANGE_TIMEOUT, self.exchange(relay, command, custody))
                .await
                .map_err(|_| RoomAdminError::Timeout)??;
        match reply {
            RoomReply::Refused(reason) => Err(RoomAdminError::Refused(reason)),
            reply => Ok(reply),
        }
    }

    async fn exchange(
        &self,
        relay: iroh::EndpointAddr,
        command: RoomCommand,
        custody: Option<Vec<u8>>,
    ) -> Result<RoomReply, RoomAdminError> {
        let bytes = postcard::to_stdvec(&RoomAdminRequest {
            device_cert: self.device_cert.clone(),
            command,
        })
        .map_err(|error| RoomAdminError::Codec(error.to_string()))?;
        let connection = self
            .endpoint
            .connect(relay, ROOM_ADMIN_ALPN)
            .await
            .map_err(|error| RoomAdminError::Transport(error.to_string()))?;
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|error| RoomAdminError::Transport(error.to_string()))?;
        crate::framing::write_frame(&mut send, &bytes)
            .await
            .map_err(|error| RoomAdminError::Transport(error.to_string()))?;
        // A relay that refuses a takeover stops reading before its custody:
        // its reply says why.
        let custody_sent = match custody {
            Some(custody) => crate::framing::write_frame(&mut send, &custody)
                .await
                .is_ok(),
            None => true,
        };
        if custody_sent {
            send.finish()
                .map_err(|error| RoomAdminError::Transport(error.to_string()))?;
        }
        let reply = crate::framing::read_frame(&mut recv, MAX_REPLY_BYTES)
            .await
            .map_err(|error| RoomAdminError::Transport(error.to_string()))?;
        connection.close(0u32.into(), b"done");
        postcard::from_bytes(&reply).map_err(|error| RoomAdminError::Codec(error.to_string()))
    }

    /// Create a governed workspace named `name`.
    ///
    /// # Errors
    ///
    /// As [`Self::send`].
    pub async fn create(
        &self,
        relay: iroh::EndpointAddr,
        name: &str,
    ) -> Result<RoomSummary, RoomAdminError> {
        match self
            .send(relay, RoomCommand::Create { name: name.into() })
            .await?
        {
            RoomReply::Room(summary) => Ok(summary),
            reply => Err(unexpected(&reply)),
        }
    }

    /// Issue an invitation to `room` with `role`, redeemable for
    /// `lifetime`, optionally only by `recipient`'s user root.
    ///
    /// # Errors
    ///
    /// As [`Self::send`].
    pub async fn invite(
        &self,
        relay: iroh::EndpointAddr,
        room: &str,
        role: MemberRole,
        recipient: Option<[u8; 32]>,
        lifetime: Duration,
    ) -> Result<IssuedInvitation, RoomAdminError> {
        let mut request_id = [0; 32];
        getrandom::fill(&mut request_id)
            .map_err(|error| RoomAdminError::Codec(error.to_string()))?;
        let expires_at_ns = tilo_clock::unix_now_ns()
            .saturating_add(u64::try_from(lifetime.as_nanos()).unwrap_or(u64::MAX));
        match self
            .send(
                relay,
                RoomCommand::Invite {
                    room: room.into(),
                    request: IssueInvitationRequest {
                        request_id,
                        role,
                        recipient_user_public_key: recipient,
                        invitee_name: None,
                        expires_at_ns,
                        inviter_name: None,
                    },
                },
            )
            .await?
        {
            RoomReply::Invitation(issued) => Ok(issued),
            reply => Err(unexpected(&reply)),
        }
    }
}

impl RoomAdminClient {
    /// Hand the relay at `relay` the room `succession` names: the relay
    /// takes it over as its new governor, from `replica` (this device's
    /// durable replica of the room, pinned to the previous governor) or,
    /// without one, from what the relay retains of the room.
    ///
    /// # Errors
    ///
    /// As [`Self::send`].
    pub async fn take_over(
        &self,
        relay: iroh::EndpointAddr,
        succession: tilo_peer_state::succession::GovernorSuccession,
        replica: Option<Vec<u8>>,
    ) -> Result<RoomSummary, RoomAdminError> {
        let custody = postcard::to_stdvec(&TakeOverCustody { replica })
            .map_err(|error| RoomAdminError::Codec(error.to_string()))?;
        if custody.len() > MAX_CUSTODY_BYTES {
            return Err(RoomAdminError::Codec(
                "room custody exceeds the wire limit".into(),
            ));
        }
        match self
            .send_with(
                relay,
                RoomCommand::TakeOver {
                    succession: Box::new(succession),
                },
                Some(custody),
            )
            .await?
        {
            RoomReply::Room(summary) => Ok(summary),
            reply => Err(unexpected(&reply)),
        }
    }
}

fn unexpected(reply: &RoomReply) -> RoomAdminError {
    RoomAdminError::Codec(format!("unexpected room reply {reply:?}"))
}
