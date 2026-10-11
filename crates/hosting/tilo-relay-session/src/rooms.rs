//! Room administration from a device: the wire vocabulary of
//! [`ROOM_ADMIN_ALPN`] and the [`RoomAdminClient`] that speaks it.
//!
//! One bounded postcard request and reply per bidirectional stream, as in the
//! workspace-join protocol. The request names the caller's certificate; the
//! relay accepts it only for the authenticated Iroh endpoint it arrived on.
//! The relay's side, `RoomAdminProtocol`, stays in `tilo-relay`.
//!
//! These are postcard wire formats between separately updated hosts and
//! desktops: field and variant order are the wire, so append only.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_peer_state::GovernedMember;
use tilo_peer_state::join::MemberRole;
use tilo_user_identity::DeviceCert;

/// ALPN of room administration.
pub const ROOM_ADMIN_ALPN: &[u8] = b"tilo-room-admin/1";

/// The largest encoded [`RoomAdminRequest`] either side accepts.
pub const ROOM_ADMIN_MAX_REQUEST_BYTES: usize = 64 * 1024;
/// The largest encoded [`RoomReply`] either side accepts.
pub const ROOM_ADMIN_MAX_REPLY_BYTES: usize = 4 * 1024 * 1024;
/// How long one room administration exchange may take.
pub const ROOM_ADMIN_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

/// Issue one invitation. A repeated `request_id` returns the invitation it
/// issued, and fails if it named a different request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueInvitationRequest {
    /// Caller-chosen idempotency key; never zero.
    pub request_id: [u8; 32],
    /// The role the redeeming person gets.
    pub role: MemberRole,
    /// Restrict redemption to devices of this person.
    pub recipient_user_public_key: Option<[u8; 32]>,
    /// How the invitation names its recipient to them.
    pub invitee_name: Option<String>,
    /// When the invitation stops being redeemable, at most 30 days ahead.
    pub expires_at_ns: u64,
    /// The name the inviting person gives, recorded in the invitation with
    /// their user root.
    pub inviter_name: Option<String>,
}

/// An issued invitation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedInvitation {
    /// Digest of `signed_invitation`.
    pub invite_id: [u8; 32],
    /// The signed invitation a device redeems.
    pub signed_invitation: Vec<u8>,
}

/// Cancel an unredeemed invitation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelInvitationRequest {
    /// Caller-chosen idempotency key.
    pub request_id: [u8; 32],
    /// The invitation to cancel.
    pub invite_id: [u8; 32],
}

/// An invitation still redeemable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInvitation {
    /// Digest of the signed invitation.
    pub invite_id: [u8; 32],
    /// The role it grants.
    pub role: MemberRole,
    /// The only person who may redeem it, if restricted.
    pub recipient_user_public_key: Option<[u8; 32]>,
    /// How it names its recipient.
    pub invitee_name: Option<String>,
    /// When it expires.
    pub expires_at_ns: u64,
}

/// Change a member's role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetMemberRoleRequest {
    /// Caller-chosen idempotency key.
    pub request_id: [u8; 32],
    /// The member's person.
    pub user_public_key: [u8; 32],
    /// Their new role.
    pub role: MemberRole,
}

/// Remove a member and every device of theirs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveMemberRequest {
    /// Caller-chosen idempotency key.
    pub request_id: [u8; 32],
    /// The member's person.
    pub user_public_key: [u8; 32],
}

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
    /// Claim only the authenticated member's own topology namespace.
    /// Membership and current workspace Write remain independently required.
    ClaimTopologyNamespace {
        /// The governed room.
        room: String,
        /// Nonzero namespace; existing ownership can never be rebound.
        salt: u16,
    },
}

/// One room administration request on the wire: the caller's certificate and
/// its command. The relay honours it only when the certificate's device key is
/// the authenticated endpoint the request arrived on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomAdminRequest {
    /// The calling device's certificate.
    pub device_cert: DeviceCert,
    /// What it asks.
    pub command: RoomCommand,
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
        let reply =
            n0_future::time::timeout(ROOM_ADMIN_EXCHANGE_TIMEOUT, self.exchange(relay, command))
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
        tilo_relay_client::frame::write_frame(&mut send, &bytes)
            .await
            .map_err(|error| RoomAdminError::Transport(error.to_string()))?;
        send.finish()
            .map_err(|error| RoomAdminError::Transport(error.to_string()))?;
        let reply = tilo_relay_client::frame::read_frame(&mut recv, ROOM_ADMIN_MAX_REPLY_BYTES)
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

fn unexpected(reply: &RoomReply) -> RoomAdminError {
    RoomAdminError::Codec(format!("unexpected room reply {reply:?}"))
}

#[cfg(test)]
mod wire_tests;
