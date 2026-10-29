//! Room administration from a device: the names of the actions a relay
//! serves for it, and the requests they carry.
//!
//! A relay serves what it governs as actions addressed to its own device:
//!
//! - a room's own governance (provider `tilo.workspace`, named in
//!   `tilo-actions`), over `Envelope::RemoteAction` on the room's workspace;
//! - the host's own business, which no room decides (creating, listing and
//!   entering rooms, letting an app author a room), as provider
//!   [`HOST_ACTIONS_PROVIDER`], to the devices of the relay's own person
//!   only, over `Envelope::HostAction` on their session with it, naming no
//!   workspace ([`ask_host`]), or on a room's workspace they hold there.
//!
//! What a member does for themselves (their effect receipts, their shares,
//! the empty record of an app they open first) their own device writes into
//! the room; no relay is asked for it.
//!
//! These are postcard wire formats between separately updated hosts and
//! desktops: field and variant order are the wire, so append only.

pub mod govern;
pub mod private;

use serde::{Deserialize, Serialize};
use tilo_actions::remote::{InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage};
use tilo_workspace_join::{GovernedMember, MemberRole};

use crate::client::RelayClient;
use crate::transport::Envelope;

/// Ask `client`'s host its own operation `invoke`, directly over the
/// session (a device of the host's person asks; no workspace is named), and
/// answer its receipt.
///
/// # Errors
///
/// The session closed before the host answered, or the invocation could not
/// be encoded.
pub async fn ask_host(
    client: &RelayClient,
    invoke: InvokeRequest,
) -> Result<ReceiptStatus, String> {
    let invocation = invoke.invocation;
    let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(invoke))
        .encode()
        .map_err(|error| error.to_string())?;
    client
        .send(Envelope::HostAction { frame })
        .map_err(|_| "the host's session closed".to_string())?;
    loop {
        match client.recv().await {
            Some(Envelope::HostAction { frame }) => {
                let message = RemoteActionFrame::decode(&frame)
                    .map_err(|error| error.to_string())?
                    .message;
                if let RemoteActionMessage::Receipt(receipt) = message
                    && receipt.invocation == invocation
                {
                    return Ok(receipt.status);
                }
            }
            Some(_) => {}
            None => return Err("the host's session closed".into()),
        }
    }
}

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

/// Serve a room a device of the host's own person governs, as that governor
/// asks: arguments `room`, `workspace_id`, `governor_hint` (the governor's
/// signed address) and `bootstrap` (the room as a joining member gets it,
/// the host's device admitted), each signed value postcard in hex. Only a
/// certified host serves rooms this way. Answers a [`HostedRoom`].
pub const HOST_ROOM: &str = "room.host";
/// Stop serving `room` for its governor. Answers `{"stopped": bool}`.
pub const UNHOST_ROOM: &str = "room.unhost";
/// Have the certified host `host` (its device key, 64 hex) of the person,
/// reachable at `host_addr` (an endpoint address), serve the room `room`
/// governed here as well: admitted as one of the owner's devices, handed the
/// room with [`HOST_ROOM`], and named among the room's hosts. Answers a
/// [`HostedRoom`].
pub const ATTACH_HOST: &str = "room.attach-host";
/// Stop having the host `host` serve `room`: it is no longer named among the
/// room's hosts, and is asked to stop at `host_addr` when given, else at the
/// address it last answered. Answers `{"confirmed": bool}`, whether the host
/// confirmed it stopped.
pub const DETACH_HOST: &str = "room.detach-host";

/// What [`HOST_ROOM`] and [`ATTACH_HOST`] answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostedRoom {
    /// The serving host's own signed address for the room, postcard in hex.
    pub host_hint: String,
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
    #[must_use]
    pub fn of(issued: &IssuedInvitation) -> Self {
        Self {
            invite: hex::encode(issued.invite_id),
            invitation: hex::encode(&issued.signed_invitation),
        }
    }

    /// The invitation's id and its signed bytes.
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

pub use tilo_workspace_join::invitations::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
};

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
    pub generation: u64,
    /// Every person governance records, removed ones included.
    pub members: Vec<GovernedMember>,
}

#[cfg(test)]
mod wire_tests;
