//! Room administration from a device: the names of the actions a relay
//! serves for it, and the wire vocabulary of [`ROOM_ADMIN_ALPN`] with the
//! [`RoomAdminClient`] that speaks it.
//!
//! A relay serves what it governs as actions over `Envelope::RemoteAction`,
//! addressed to its own device, on a workspace the caller holds there:
//!
//! - a room's own governance (provider `tilo.workspace`, named in
//!   `tilo-actions`), on the room's workspace;
//! - the host's own business, which no room decides (creating, listing and
//!   entering rooms, letting an app author a room), as provider
//!   [`HOST_ACTIONS_PROVIDER`], to the devices of the relay's own person
//!   only, on any workspace they hold there: the person's continuity
//!   workspace before they hold any room;
//! - what a member does for themselves (claiming their topology namespace,
//!   recording their own effect receipt, and having the host create the
//!   empty record of an app they open there first) as provider
//!   [`MEMBER_ACTIONS_PROVIDER`], on the room's workspace.
//!
//! [`ROOM_ADMIN_ALPN`] is kept only for [`RoomCommand::Remove`], which the
//! actions do not run until the person's own confirmation can be asked for.
//! A relay refuses every other command there and names the action that
//! replaced it. One bounded postcard request and reply per bidirectional
//! stream, as in the workspace-join protocol. The request names the caller's
//! certificate; the relay accepts it only for the authenticated Iroh endpoint
//! it arrived on. The relay's side, `RoomAdminProtocol`, stays in
//! `tilo-relay`.
//!
//! These are postcard wire formats between separately updated hosts and
//! desktops: field and variant order are the wire, so append only.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;
use tilo_workspace_join::{GovernedMember, MemberRole};

/// The provider a relay answers its host operations under: creating,
/// listing and entering the rooms it governs. Only the devices of the
/// relay's own person are offered them, and invoking one is decided as
/// every door decides a host operation.
pub const HOST_ACTIONS_PROVIDER: &str = "tilo.host";
/// The one resource the host operations are published on.
pub const HOST_ROOMS_RESOURCE: &str = "host.rooms";
/// Create a governed room. Arguments: `name`, and the terms of its first
/// invitation when one is wanted (`invite_role`, `invite_recipient`,
/// `invitee_name`, `invite_expires_in_seconds`). Answers a [`CreatedRoom`].
pub const CREATE_ROOM: &str = "room.create";
/// Every room the host governs, each as a [`RoomListing`].
pub const LIST_ROOMS: &str = "room.list";
/// Admit the calling device to `room` as one of its owner's. Answers an
/// [`EnteredRoom`]: the invitation the device redeems to hold the room,
/// restricted to its person and redeemable `expires_in_seconds` (600 when
/// omitted).
pub const ENTER_ROOM: &str = "room.enter";
/// Let the application `app` author the records of `room`: what opening an
/// app in a room the host governs for the first time asks. Answers the
/// room's `generation` that carries it.
pub const GRANT_APP_AUTHORSHIP: &str = "room.grant-app-authorship";

/// The provider a relay answers a room member's own business under, on the
/// room's workspace: what they do for themselves, never governance.
pub const MEMBER_ACTIONS_PROVIDER: &str = "tilo.member";
/// The resource kind of a member's own business in a room.
pub const MEMBER_RESOURCE_KIND: &str = "workspace.member";
/// Claim the caller's own topology namespace `salt` (1–65535). Answers the
/// room's `generation` that carries it.
pub const CLAIM_TOPOLOGY_NAMESPACE: &str = "topology-namespace.claim";
/// Record the caller's own terminal effect receipt: `invocation` (a UUID)
/// and `redacted`, its canonical redacted public receipt as JSON text.
/// Answers the room's `generation` that carries it.
pub const RECORD_EFFECT_RECEIPT: &str = "effect-receipt.record";
/// Have the host governing the room create the record `record` (`""` for
/// the app's primary) of `app`, empty and signed by the host: what opening
/// an app there for the first time asks when the room does not hold its
/// record. Only for an app the room lets author and a record the caller may
/// change; no member-supplied bytes cross it, and the app's first state is
/// then the member's own change. Answers `created` (false when the record
/// was already there) and the room's `generation`.
pub const PREPARE_APPLICATION_RECORD: &str = "application-record.prepare";

/// The resource a member's own business in `room` is published on.
#[must_use]
pub fn member_resource(room: &str) -> String {
    format!("{MEMBER_RESOURCE_KIND}:{room}")
}

/// One room a relay governs, as its host actions answer it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomListing {
    /// The room's UUID.
    pub room: String,
    /// The relay workspace carrying the room.
    pub workspace_id: u64,
    /// The workspace's name.
    pub name: String,
    /// The room's committed governance generation.
    pub generation: u64,
}

/// An invitation a relay's room action issued, as it answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionInvitation {
    /// Digest of the signed invitation, 64 hex characters.
    pub invite: String,
    /// The signed invitation a device redeems, in hex.
    pub invitation: String,
}

impl ActionInvitation {
    /// `issued`, as an action answers it.
    #[must_use]
    pub fn of(issued: &IssuedInvitation) -> Self {
        Self {
            invite: hex::encode(issued.invite_id),
            invitation: hex::encode(&issued.signed_invitation),
        }
    }

    /// The invitation's id and its signed bytes.
    ///
    /// # Errors
    ///
    /// When either is not the hex it should be.
    pub fn decode(&self) -> Result<IssuedInvitation, String> {
        let mut invite_id = [0; 32];
        hex::decode_to_slice(&self.invite, &mut invite_id)
            .map_err(|_| "the relay named an invalid invitation id".to_owned())?;
        let signed_invitation = hex::decode(&self.invitation)
            .map_err(|_| "the relay sent an invalid invitation".to_owned())?;
        Ok(IssuedInvitation {
            invite_id,
            signed_invitation,
        })
    }
}

/// What [`CREATE_ROOM`] answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedRoom {
    /// The room.
    #[serde(flatten)]
    pub room: RoomListing,
    /// Its first invitation, when one was asked for.
    #[serde(default)]
    pub invitation: Option<ActionInvitation>,
}

/// What [`ENTER_ROOM`] answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnteredRoom {
    /// Whether governance changed to admit the caller.
    pub changed: bool,
    /// The invitation the caller redeems to hold the room.
    pub invitation: ActionInvitation,
}

/// ALPN of room administration: only [`RoomCommand::Remove`] is still
/// answered here (see the module documentation).
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

/// Delegate one application's workspace authorship. A repeated `request_id`
/// returns the change it committed, and fails if it named another app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantAppAuthorshipRequest {
    /// Caller-chosen idempotency key; never zero.
    pub request_id: [u8; 32],
    /// The exact registered application identifier.
    pub app: String,
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
    /// Let an application author `room`'s records: what opening an app there
    /// for the first time asks of the room's governor. The host's person only.
    GrantAppAuthorship {
        /// The governed room.
        room: String,
        /// The application.
        request: GrantAppAuthorshipRequest,
    },
}

impl RoomCommand {
    /// The provider and action a relay serves this command as, for every
    /// command but [`Self::Remove`]: what a relay that no longer answers it
    /// at [`ROOM_ADMIN_ALPN`] names in its refusal. `None` for the removal,
    /// still answered there.
    #[must_use]
    pub const fn replaced_by(&self) -> Option<(&'static str, &'static str)> {
        /// A room's own governance provider, as `tilo-actions` names it.
        const GOVERNANCE: &str = "tilo.workspace";
        match self {
            Self::Create { .. } => Some((HOST_ACTIONS_PROVIDER, CREATE_ROOM)),
            Self::List => Some((HOST_ACTIONS_PROVIDER, LIST_ROOMS)),
            Self::Enter { .. } => Some((HOST_ACTIONS_PROVIDER, ENTER_ROOM)),
            Self::Invite { .. } => Some((GOVERNANCE, "invite")),
            Self::Cancel { .. } => Some((GOVERNANCE, "invitation.cancel")),
            Self::Pending { .. } => Some((GOVERNANCE, "invitation.list")),
            Self::SetRole { .. } => Some((GOVERNANCE, "set-role")),
            Self::RecordEffectReceipt { .. } => {
                Some((MEMBER_ACTIONS_PROVIDER, RECORD_EFFECT_RECEIPT))
            }
            Self::ClaimTopologyNamespace { .. } => {
                Some((MEMBER_ACTIONS_PROVIDER, CLAIM_TOPOLOGY_NAMESPACE))
            }
            Self::GrantAppAuthorship { .. } => Some((HOST_ACTIONS_PROVIDER, GRANT_APP_AUTHORSHIP)),
            Self::Remove { .. } => None,
        }
    }
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

/// A certified device at a relay's [`ROOM_ADMIN_ALPN`]: it removes a member, the
/// one command still answered there, as far as the room lets its person.
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

    /// Remove the member `request` names from `room`, and every device of
    /// theirs: the one command this door still answers. Answers the room
    /// generation that carries the removal.
    ///
    /// # Errors
    ///
    /// As [`Self::send`].
    pub async fn remove(
        &self,
        relay: iroh::EndpointAddr,
        room: &str,
        request: RemoveMemberRequest,
    ) -> Result<u64, RoomAdminError> {
        match self
            .send(
                relay,
                RoomCommand::Remove {
                    room: room.into(),
                    request,
                },
            )
            .await?
        {
            RoomReply::Governance { generation } => Ok(generation),
            reply => Err(unexpected(&reply)),
        }
    }
}

fn unexpected(reply: &RoomReply) -> RoomAdminError {
    RoomAdminError::Codec(format!("unexpected room reply {reply:?}"))
}

#[cfg(test)]
mod wire_tests;
