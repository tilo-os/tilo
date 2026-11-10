//! Workspace administration from a device: the names of the actions a relay
//! serves for it, and the requests they carry.
//!
//! A relay serves what it governs as actions addressed to its own device:
//!
//! - a workspace's own governance (provider `tilo.workspace`, named in
//!   `tilo-actions`), over `Envelope::RemoteAction` on the workspace;
//! - the host's own business, which no workspace decides (creating, listing and
//!   entering workspaces, letting an app author a workspace), as provider
//!   [`HOST_ACTIONS_PROVIDER`], to the devices of the relay's own person
//!   only, over `Envelope::HostAction` on their session with it, naming no
//!   workspace ([`ask_host`]), or on a workspace they hold there.
//!
//! What a member does for themselves (their effect receipts, their shares,
//! the empty record of an app they open first) their own device writes into
//! the workspace; no relay is asked for it.
//!
//! These are postcard wire formats between separately updated hosts and
//! desktops: field and variant order are the wire, so append only.

pub mod govern;

use serde::{Deserialize, Serialize};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget,
};
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

/// Ask `client`'s relay to enter this device into `workspace`, and answer the
/// invitation text the device redeems to hold it: [`ENTER_ROOM`]'s ask, and
/// the invitation it answers with, in the shape every join redeems.
///
/// # Errors
///
/// The host did not answer, refused, or answered no invitation.
pub async fn enter_invitation(
    client: &RelayClient,
    workspace: &str,
    expires_in_seconds: u64,
) -> Result<String, String> {
    let arguments = serde_json::Map::from_iter([
        ("room".to_owned(), workspace.into()),
        ("expires_in_seconds".to_owned(), expires_in_seconds.into()),
    ]);
    let invoke = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.to_owned(),
            address: ActionAddress::new(HOST_ROOMS_RESOURCE.to_owned(), ENTER_ROOM),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    let entered: EnteredWorkspace = match ask_host(client, invoke).await? {
        ReceiptStatus::Completed { output } => {
            serde_json::from_value(output.value.unwrap_or_default())
                .map_err(|error| format!("the relay answered no invitation: {error}"))?
        }
        ReceiptStatus::Rejected { error } => {
            return Err(format!("the relay refused to enter it: {error}"));
        }
        ReceiptStatus::Failed { error } => return Err(error.message),
        ReceiptStatus::Accepted | ReceiptStatus::Cancelled { .. } => {
            return Err("the relay did not answer the entry".into());
        }
    };
    let signed = entered.invitation.decode()?.signed_invitation;
    Ok(tilo_workspace_join::encode_invitation_text(&signed))
}

/// The provider a relay answers its host operations under: creating,
/// listing and entering the workspaces it governs. Only the devices of the
/// relay's own person are offered them, and invoking one is decided as
/// every door decides a host operation.
pub const HOST_ACTIONS_PROVIDER: &str = "tilo.host";
/// The one resource the host operations are published on.
pub const HOST_ROOMS_RESOURCE: &str = "host.rooms";
/// Create a governed workspace. Arguments: `name`, and the terms of its first
/// invitation when one is wanted (`invite_role`, `invite_recipient`,
/// `invitee_name`, `invite_expires_in_seconds`). Answers a [`CreatedWorkspace`].
pub const CREATE_ROOM: &str = "room.create";
/// Make the one workspace of the caller's own person on a host that keeps
/// workspaces of people's own, or answer theirs again: any person's device
/// may ask. Arguments: `name` (the new workspace's; "My workspace" when
/// omitted, unused once they have one), `room` (the identity of a workspace
/// that moves here as theirs; a different one than theirs is refused with a
/// `tilo_actions::remote::host_account::Refusal`), and `expires_in_seconds`
/// (how long the invitation stays redeemable; 600 when omitted). Answers a
/// [`CreatedWorkspace`] with the invitation the person's devices redeem to
/// hold it, as its administrator.
pub const CREATE_OWN_ROOM: &str = "room.create-own";
/// Every workspace the host governs, each as a [`WorkspaceListing`].
pub const LIST_ROOMS: &str = "room.list";
/// Delete the workspace `room`, one of the caller's person's own (any
/// workspace this host governs, for the host's own person), with everything
/// the host keeps of it: its records, history, files and the storage they
/// took. Everyone in it is told it was deleted
/// ([`tilo_relay_protocol::refusal::WORKSPACE_DELETED`]) and let in no more.
/// Final: there is no undo. Only a person's device, or an agent's grant
/// reaching all their workspaces, deletes; a workspace that is not theirs is
/// refused with [`NOT_YOUR_OWN`]. Answers `{"deleted": room}`.
pub const DELETE_ROOM: &str = "room.delete";
/// What the host refuses a person asking for, or deleting, a workspace that
/// is not one of their own with.
pub const NOT_YOUR_OWN: &str = "that workspace is not one of your own";
/// Admit the calling device to `workspace` as one of its owner's. Answers
/// an [`EnteredWorkspace`]: the invitation the device redeems to hold the workspace,
/// restricted to its person and redeemable `expires_in_seconds` (600 when
/// omitted).
pub const ENTER_ROOM: &str = "room.enter";
/// The revocations of the host's person the host enforces, as the person's
/// devices announced them: a JSON array of root-signed
/// `tilo_user_identity::RevocationEntry`. What a service that signs the
/// person in checks a device against, from the same list the host's own
/// handshakes read.
pub const LIST_REVOCATIONS: &str = "identity.revocations";

/// Serve a workspace a device of the host's own person governs, as that governor
/// asks: arguments `workspace`, `workspace_id`, `governor_hint` (the governor's
/// signed address) and `bootstrap` (the workspace as a joining member gets it,
/// the host's device admitted), each signed value postcard in hex. Only a
/// certified host serves workspaces this way. Answers a [`HostedWorkspace`].
pub const HOST_ROOM: &str = "room.host";
/// Stop serving `workspace` for its governor. Answers `{"stopped": bool}`.
pub const UNHOST_ROOM: &str = "room.unhost";
/// Have the certified host `host` (its device key, 64 hex) of the person,
/// reachable at `host_addr` (an endpoint address), serve the workspace `workspace`
/// governed here as well: admitted as one of the owner's devices, handed the
/// workspace with [`HOST_ROOM`], and named among the workspace's hosts. Answers a
/// [`HostedWorkspace`].
pub const ATTACH_HOST: &str = "room.attach-host";
/// Stop having the host `host` serve `workspace`: it is no longer named among the
/// workspace's hosts, and is asked to stop at `host_addr` when given, else at the
/// address it last answered. Answers `{"confirmed": bool}`, whether the host
/// confirmed it stopped.
pub const DETACH_HOST: &str = "room.detach-host";

/// What [`HOST_ROOM`] and [`ATTACH_HOST`] answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostedWorkspace {
    /// The serving host's own signed address for the workspace, postcard in hex.
    pub host_hint: String,
}

/// One workspace a relay governs, as its host actions answer it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceListing {
    /// The workspace's UUID.
    pub workspace: String,
    /// The relay workspace carrying the workspace.
    pub workspace_id: u64,
    /// The workspace's name.
    pub name: String,
    /// The workspace's committed governance generation.
    pub generation: u64,
    /// The part the asking device's person takes in it, when the answer is a
    /// person's listing: `owner` for a workspace of their own, else the role
    /// they hold (`viewer`, `commenter`, `editor`, `moderator`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// An invitation a relay's workspace action issued, as it answers it.
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
pub struct CreatedWorkspace {
    #[serde(flatten)]
    pub workspace: WorkspaceListing,
    /// Its first invitation, when one was asked for.
    #[serde(default)]
    pub invitation: Option<ActionInvitation>,
}

/// What [`ENTER_ROOM`] answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnteredWorkspace {
    /// Whether governance changed to admit the caller.
    pub changed: bool,
    /// The invitation the caller redeems to hold the workspace.
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

/// A governed workspace as its administrators see it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSummary {
    /// The workspace's UUID.
    pub workspace: String,
    /// The relay workspace carrying the workspace.
    pub workspace_id: u64,
    /// The workspace's name.
    pub name: String,
    pub generation: u64,
    /// Every person the workspace has — its owner and everyone its governance
    /// admitted — removed ones included.
    pub members: Vec<GovernedMember>,
}

impl WorkspaceSummary {
    /// How many people the workspace has now: everyone it has not removed.
    #[must_use]
    pub fn people(&self) -> usize {
        self.members.iter().filter(|member| !member.removed).count()
    }
}

#[cfg(test)]
mod wire_tests;
