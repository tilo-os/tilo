//! The host's own operations as actions: creating, listing and entering the
//! rooms this relay governs, and letting an app author one of them
//! (provider [`HOST_ACTIONS_PROVIDER`]).
//!
//! No room decides them: they are the relay's person's alone
//! ([`HostOperation`]), and [`Rooms::authorize`] says so for every door.
//! The catalogue asks the same decision, so a device of anyone else is
//! offered nothing, and invoking one is the same `UnknownAction` as invoking
//! one that does not exist. The device is the one whose certificate the
//! relay's handshake verified, which entering a room admits.
//!
//! Creating a room may issue its first invitation, and entering one answers
//! the invitation the device redeems to hold it: a device shares a workspace
//! or follows its host's rooms without asking any room's governance before
//! it holds the room.

use serde_json::Value;
use tilo_action_policy::roles;
use tilo_actions::remote::{ActionHostError, CatalogueRow, InvokeRequest};
use tilo_actions::{ActionKind, ActionParam, ParamType};
use tilo_peer_state::join::MemberRole;
use tilo_relay_session::rooms::{
    ATTACH_HOST, ActionInvitation, CREATE_ROOM, CreatedRoom, DETACH_HOST, ENTER_ROOM, EnteredRoom,
    GRANT_APP_AUTHORSHIP, GrantAppAuthorshipRequest, HOST_ACTIONS_PROVIDER, HOST_ROOM,
    HOST_ROOMS_RESOURCE, HostedRoom, LIST_ROOMS, RoomListing, UNHOST_ROOM,
};
use tilo_user_identity::DeviceCert;

use super::{
    Args, DEFAULT_INVITATION_HOURS, HOUR_NS, Publisher, ROLES, SECOND_NS, Verb, invalid, refused,
    request_id,
};
use crate::host_authority::HostOperation;
use crate::rooms::authority::RoomOp;
use crate::rooms::{IssueInvitationRequest, RoomSummary, Rooms};

/// The host's own operations.
pub(super) const HOST: Publisher = Publisher {
    provider: HOST_ACTIONS_PROVIDER,
    kind: HOST_ROOMS_RESOURCE,
};

/// What the host's rooms resource is called.
const LABEL: &str = "Rooms";

/// How long the invitation a device entering a room redeems stays
/// redeemable when it does not say.
const OWN_INVITATION_SECONDS: u64 = 600;

/// The host's operations in catalogue order, each with the
/// [`HostOperation`] it is.
const HOST_VERBS: [(Verb, HostOperation); 8] = [
    (
        Verb {
            id: CREATE_ROOM,
            label: "Create room",
            gate: CREATE_ROOM,
            kind: roles::GOVERNANCE_KIND,
            params: create_params,
        },
        HostOperation::CreateRoom,
    ),
    (
        Verb {
            id: LIST_ROOMS,
            label: "List rooms",
            gate: LIST_ROOMS,
            kind: ActionKind::Read,
            params: Vec::new,
        },
        HostOperation::ListRooms,
    ),
    (
        Verb {
            id: ENTER_ROOM,
            label: "Enter room",
            gate: ENTER_ROOM,
            kind: roles::GOVERNANCE_KIND,
            params: enter_params,
        },
        HostOperation::EnterRoom,
    ),
    (
        Verb {
            id: GRANT_APP_AUTHORSHIP,
            label: "Let an app author a room",
            gate: GRANT_APP_AUTHORSHIP,
            kind: roles::GOVERNANCE_KIND,
            params: grant_params,
        },
        HostOperation::GrantAppAuthorship,
    ),
    (
        Verb {
            id: ATTACH_HOST,
            label: "Have a host serve a room",
            gate: ATTACH_HOST,
            kind: roles::GOVERNANCE_KIND,
            params: attach_params,
        },
        HostOperation::AttachHost,
    ),
    (
        Verb {
            id: DETACH_HOST,
            label: "Stop a host serving a room",
            gate: DETACH_HOST,
            kind: roles::GOVERNANCE_KIND,
            params: detach_params,
        },
        HostOperation::DetachHost,
    ),
    (
        Verb {
            id: HOST_ROOM,
            label: "Serve a room for its governor",
            gate: HOST_ROOM,
            kind: roles::GOVERNANCE_KIND,
            params: host_room_params,
        },
        HostOperation::HostRoom,
    ),
    (
        Verb {
            id: UNHOST_ROOM,
            label: "Stop serving a room for its governor",
            gate: UNHOST_ROOM,
            kind: roles::GOVERNANCE_KIND,
            params: room_params,
        },
        HostOperation::UnhostRoom,
    ),
];

fn room_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "room",
        ParamType::Text,
        "The room, as listing rooms names it.",
    )]
}

fn host_param() -> ActionParam {
    ActionParam::required(
        "host",
        ParamType::Text,
        "The host's device key, 64 hex characters: a certified host of this person.",
    )
}

fn attach_params() -> Vec<ActionParam> {
    vec![
        room_params().remove(0),
        host_param(),
        ActionParam::required(
            "host_addr",
            ParamType::Object,
            "The host's endpoint address.",
        ),
    ]
}

fn detach_params() -> Vec<ActionParam> {
    vec![
        room_params().remove(0),
        host_param(),
        ActionParam::optional(
            "host_addr",
            ParamType::Object,
            "Where to ask the host to stop; the address it last answered when omitted.",
        ),
    ]
}

fn host_room_params() -> Vec<ActionParam> {
    vec![
        room_params().remove(0),
        ActionParam::required("workspace_id", ParamType::Integer, "The room's workspace."),
        ActionParam::required(
            "governor_hint",
            ParamType::Text,
            "The governor's signed address for the room, postcard in hex.",
        ),
        ActionParam::required(
            "bootstrap",
            ParamType::Text,
            "The room as a joining member gets it, this host's device admitted, postcard in hex.",
        ),
    ]
}

/// `value`, postcard in hex, as the host operations carry signed values.
fn hex_postcard(value: &impl serde::Serialize) -> Result<String, ActionHostError> {
    postcard::to_stdvec(value)
        .map(hex::encode)
        .map_err(|error| refused(&error))
}

fn create_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "name",
            ParamType::Text,
            "The room's name, 1 to 80 printable characters.",
        ),
        ActionParam::optional(
            "invite_role",
            ParamType::Text,
            "Issue the room's first invitation, with this role.",
        )
        .with_choices(ROLES),
        ActionParam::optional(
            "invite_recipient",
            ParamType::Text,
            "Only this person (user root key, 64 hex characters) may redeem the first invitation; anyone holding it when omitted.",
        ),
        ActionParam::optional(
            "invitee_name",
            ParamType::Text,
            "How the first invitation names its recipient to them.",
        ),
        ActionParam::optional(
            "inviter_name",
            ParamType::Text,
            "The name the inviting person gives, recorded in the first invitation.",
        ),
        ActionParam::optional(
            "invite_expires_in_seconds",
            ParamType::Integer,
            "How long the first invitation stays redeemable: 7 days when omitted, at most 30.",
        ),
        ActionParam::optional(
            "room",
            ParamType::Text,
            "The room it is, when a workspace moves here from the device that made it: its room (a UUID) stays its own. A new room when omitted.",
        ),
    ]
}

fn enter_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "room",
            ParamType::Text,
            "The room, as listing rooms names it.",
        ),
        ActionParam::optional(
            "expires_in_seconds",
            ParamType::Integer,
            "How long the invitation this device redeems to hold the room stays redeemable: 600 seconds when omitted.",
        ),
    ]
}

fn grant_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "room",
            ParamType::Text,
            "The room, as listing rooms names it.",
        ),
        ActionParam::required(
            "app",
            ParamType::Text,
            "The exact registered application identifier.",
        ),
    ]
}

/// `summary`, as the host's actions answer a room.
fn listing(summary: &RoomSummary) -> RoomListing {
    RoomListing {
        room: summary.room.clone(),
        workspace_id: summary.workspace_id,
        name: summary.name.clone(),
        generation: summary.generation,
    }
}

fn answer(value: &impl serde::Serialize) -> Result<Value, ActionHostError> {
    serde_json::to_value(value).map_err(|error| refused(&error))
}

/// When an invitation issued at `now` for `seconds` stops being redeemable.
const fn expiry(now: u64, seconds: u64) -> u64 {
    now.saturating_add(seconds.saturating_mul(SECOND_NS))
}

/// The terms of a room's first invitation, when its creation asks for one.
fn first_invitation(
    args: &Args,
    request_id: [u8; 32],
    now: u64,
) -> Result<Option<IssueInvitationRequest>, ActionHostError> {
    if args.text("invite_role")?.is_none() {
        return Ok(None);
    }
    let expires_at_ns = match args.integer("invite_expires_in_seconds")? {
        Some(seconds) => expiry(now, seconds),
        None => now.saturating_add(DEFAULT_INVITATION_HOURS.saturating_mul(HOUR_NS)),
    };
    Ok(Some(IssueInvitationRequest {
        request_id,
        role: args.role_at("invite_role")?,
        recipient_user_public_key: args.key("invite_recipient")?,
        invitee_name: args.text("invitee_name")?.map(str::to_owned),
        expires_at_ns,
        inviter_name: args.text("inviter_name")?.map(str::to_owned),
    }))
}

impl Rooms {
    /// The host's operations `device` may run.
    pub(super) fn host_rows(&self, device: &DeviceCert) -> Vec<CatalogueRow> {
        let now = tilo_clock::unix_now_ns();
        HOST_VERBS
            .iter()
            .filter(|(_, operation)| self.permits(device, None, RoomOp::Host(*operation), now))
            .map(|(verb, _)| verb.row(HOST, HOST_ROOMS_RESOURCE, LABEL))
            .collect()
    }

    /// Run the host operation `request` names as `device`, the certificate
    /// the relay's handshake verified for the caller.
    pub(super) async fn host_invoke(
        &self,
        device: &DeviceCert,
        request: &InvokeRequest,
    ) -> Result<Value, ActionHostError> {
        let address = &request.target.address;
        let now = tilo_clock::unix_now_ns();
        let operation = HOST_VERBS
            .iter()
            .find(|(verb, _)| verb.id == address.action)
            .map(|(_, operation)| *operation)
            .ok_or(ActionHostError::UnknownAction)?;
        // Unknown and forbidden are the same answer.
        if address.resource != HOST_ROOMS_RESOURCE
            || !self.permits(device, None, RoomOp::Host(operation), now)
        {
            return Err(ActionHostError::UnknownAction);
        }
        let args = Args::of(&request.argument)?;
        let request_id = request_id(&device.user_public_key, request.invocation);
        match operation {
            HostOperation::CreateRoom => {
                let name = args.required_text("name")?;
                let room = args.text("room")?;
                let first = first_invitation(&args, request_id, now)?;
                self.authorize(device, None, RoomOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                let summary = self
                    .create_room(name, room)
                    .await
                    .map_err(|error| refused(&error))?;
                let invitation = match first {
                    Some(terms) => Some(ActionInvitation::of(
                        &self
                            .issue(&summary.room, device, terms)
                            .await
                            .map_err(|error| refused(&error))?,
                    )),
                    None => None,
                };
                answer(&CreatedRoom {
                    room: listing(&summary),
                    invitation,
                })
            }
            HostOperation::ListRooms => {
                self.authorize(device, None, RoomOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                let rooms = self.list().await;
                answer(&rooms.iter().map(listing).collect::<Vec<_>>())
            }
            HostOperation::EnterRoom => {
                let room = args.required_text("room")?;
                let seconds = args
                    .integer("expires_in_seconds")?
                    .unwrap_or(OWN_INVITATION_SECONDS);
                let changed = self
                    .enter(room, device)
                    .await
                    .map_err(|error| refused(&error))?;
                let own = IssueInvitationRequest {
                    request_id,
                    role: MemberRole::Editor,
                    recipient_user_public_key: Some(device.user_public_key),
                    invitee_name: None,
                    expires_at_ns: expiry(now, seconds),
                    inviter_name: None,
                };
                let issued = self
                    .issue(room, device, own)
                    .await
                    .map_err(|error| refused(&error))?;
                answer(&EnteredRoom {
                    changed,
                    invitation: ActionInvitation::of(&issued),
                })
            }
            HostOperation::GrantAppAuthorship => {
                let request = GrantAppAuthorshipRequest {
                    request_id,
                    app: args.required_text("app")?.to_owned(),
                };
                let done = self
                    .grant_app_authorship(args.required_text("room")?, device, request)
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(serde_json::json!({ "generation": done.generation }))
            }
            HostOperation::AttachHost
            | HostOperation::DetachHost
            | HostOperation::HostRoom
            | HostOperation::UnhostRoom => self.serving_invoke(device, operation, &args, now).await,
            _ => Err(ActionHostError::UnknownAction),
        }
    }

    /// The host operations that have one host of the person serve a room
    /// another governs ([`crate::hosted`]), already permitted to `device`.
    async fn serving_invoke(
        &self,
        device: &DeviceCert,
        operation: HostOperation,
        args: &Args,
        now: u64,
    ) -> Result<Value, ActionHostError> {
        match operation {
            HostOperation::AttachHost => {
                let addr = args
                    .object("host_addr")?
                    .ok_or_else(|| invalid("`host_addr` is required"))?;
                let hint = self
                    .attach_host(
                        args.required_text("room")?,
                        device,
                        args.required_key("host")?,
                        addr,
                    )
                    .await
                    .map_err(|error| refused(&error))?;
                answer(&HostedRoom {
                    host_hint: hex_postcard(&hint)?,
                })
            }
            HostOperation::DetachHost => {
                let confirmed = self
                    .detach_host(
                        args.required_text("room")?,
                        device,
                        args.required_key("host")?,
                        args.object("host_addr")?,
                    )
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(serde_json::json!({ "confirmed": confirmed }))
            }
            HostOperation::HostRoom => {
                self.authorize(device, None, RoomOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                let request = crate::hosted::HostRoomRequest {
                    room: args.required_text("room")?.to_owned(),
                    workspace_id: args
                        .integer("workspace_id")?
                        .ok_or_else(|| invalid("`workspace_id` is required"))?,
                    governor_hint: crate::hosted::decode_hex(args.required_text("governor_hint")?)
                        .map_err(|error| invalid(format!("`governor_hint`: {error}")))?,
                    bootstrap: crate::hosted::decode_hex(args.required_text("bootstrap")?)
                        .map_err(|error| invalid(format!("`bootstrap`: {error}")))?,
                };
                let hint = self
                    .inner
                    .hosting
                    .host_room(device, &request)
                    .await
                    .map_err(|error| refused(&error))?;
                answer(&HostedRoom {
                    host_hint: hex_postcard(&hint)?,
                })
            }
            HostOperation::UnhostRoom => {
                self.authorize(device, None, RoomOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                let stopped = self
                    .inner
                    .hosting
                    .unhost_room(device, args.required_text("room")?)
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(serde_json::json!({ "stopped": stopped }))
            }
            _ => Err(ActionHostError::UnknownAction),
        }
    }
}

#[cfg(test)]
mod tests;
