//! The host's own operations as actions: creating, listing and entering the
//! workspaces this relay governs, and letting an app author one of them
//! (provider [`HOST_ACTIONS_PROVIDER`]).
//!
//! No workspace decides them: they are the relay's person's alone
//! ([`HostOperation`]), and [`Workspaces::authorize`] says so for every door,
//! but for making a person's own workspace, which a host that keeps them
//! offers anyone ([`CREATE_OWN_ROOM`]).
//! The catalogue asks the same decision, so a device of anyone else is
//! offered nothing, and invoking one is the same `UnknownAction` as invoking
//! one that does not exist. The device is the one whose certificate the
//! relay's handshake verified, which entering a workspace admits.
//!
//! Creating a workspace may issue its first invitation, and entering one answers
//! the invitation the device redeems to hold it: a device shares a workspace
//! or follows its host's workspaces without asking any workspace's governance before
//! it holds the workspace.

use serde_json::Value;
use tilo_action_policy::roles;
use tilo_actions::remote::{ActionHostError, CatalogueRow, InvokeRequest};
use tilo_actions::{ActionKind, ActionParam, ParamType};
use tilo_peer_state::join::MemberRole;
use tilo_relay_session::workspaces::{
    ATTACH_HOST, ActionInvitation, CREATE_OWN_ROOM, CREATE_ROOM, CreatedWorkspace, DETACH_HOST,
    ENTER_ROOM, EnteredWorkspace, HOST_ACTIONS_PROVIDER, HOST_ROOM, HOST_ROOMS_RESOURCE,
    HostedWorkspace, LIST_REVOCATIONS, LIST_ROOMS, UNHOST_ROOM, WorkspaceListing,
};
use tilo_user_identity::DeviceCert;

use super::{
    Args, DEFAULT_INVITATION_HOURS, HOUR_NS, Publisher, ROLES, SECOND_NS, Verb, invalid, refused,
    request_id,
};
use crate::host_authority::HostOperation;
use crate::workspaces::authority::WorkspaceOp;
use crate::workspaces::{IssueInvitationRequest, WorkspaceSummary, Workspaces};

/// The host's own operations.
pub(super) const HOST: Publisher = Publisher {
    provider: HOST_ACTIONS_PROVIDER,
    kind: HOST_ROOMS_RESOURCE,
};

/// What the host's workspaces resource is called.
const LABEL: &str = "Workspaces";

/// How long the invitation a device entering a workspace, or its person's
/// own, redeems stays redeemable when it does not say.
const OWN_INVITATION_SECONDS: u64 = 600;
/// What a person's own workspace is called when they do not say.
const OWN_WORKSPACE_NAME: &str = "My workspace";

/// The host's operations in catalogue order, each with the
/// [`HostOperation`] it is.
const HOST_VERBS: [(Verb, HostOperation); 9] = [
    (
        Verb {
            id: CREATE_ROOM,
            label: "Create workspace",
            gate: CREATE_ROOM,
            kind: roles::GOVERNANCE_KIND,
            params: create_params,
        },
        HostOperation::CreateWorkspace,
    ),
    (
        Verb {
            id: CREATE_OWN_ROOM,
            label: "Create your own workspace",
            gate: CREATE_OWN_ROOM,
            kind: roles::GOVERNANCE_KIND,
            params: create_own_params,
        },
        HostOperation::CreateOwnWorkspace,
    ),
    (
        Verb {
            id: LIST_ROOMS,
            label: "List workspaces",
            gate: LIST_ROOMS,
            kind: ActionKind::Read,
            params: Vec::new,
        },
        HostOperation::ListWorkspaces,
    ),
    (
        Verb {
            id: LIST_REVOCATIONS,
            label: "List revoked devices",
            gate: LIST_REVOCATIONS,
            kind: ActionKind::Read,
            params: Vec::new,
        },
        HostOperation::ListRevocations,
    ),
    (
        Verb {
            id: ENTER_ROOM,
            label: "Enter workspace",
            gate: ENTER_ROOM,
            kind: roles::GOVERNANCE_KIND,
            params: enter_params,
        },
        HostOperation::EnterWorkspace,
    ),
    (
        Verb {
            id: ATTACH_HOST,
            label: "Have a host serve a workspace",
            gate: ATTACH_HOST,
            kind: roles::GOVERNANCE_KIND,
            params: attach_params,
        },
        HostOperation::AttachHost,
    ),
    (
        Verb {
            id: DETACH_HOST,
            label: "Stop a host serving a workspace",
            gate: DETACH_HOST,
            kind: roles::GOVERNANCE_KIND,
            params: detach_params,
        },
        HostOperation::DetachHost,
    ),
    (
        Verb {
            id: HOST_ROOM,
            label: "Serve a workspace for its governor",
            gate: HOST_ROOM,
            kind: roles::GOVERNANCE_KIND,
            params: host_workspace_params,
        },
        HostOperation::HostWorkspace,
    ),
    (
        Verb {
            id: UNHOST_ROOM,
            label: "Stop serving a workspace for its governor",
            gate: UNHOST_ROOM,
            kind: roles::GOVERNANCE_KIND,
            params: workspace_params,
        },
        HostOperation::UnhostWorkspace,
    ),
];

fn workspace_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "room",
        ParamType::Text,
        "The workspace, as listing workspaces names it.",
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
        workspace_params().remove(0),
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
        workspace_params().remove(0),
        host_param(),
        ActionParam::optional(
            "host_addr",
            ParamType::Object,
            "Where to ask the host to stop; the address it last answered when omitted.",
        ),
    ]
}

fn host_workspace_params() -> Vec<ActionParam> {
    vec![
        workspace_params().remove(0),
        ActionParam::required(
            "workspace_id",
            ParamType::Integer,
            "The workspace's transport shard.",
        ),
        ActionParam::required(
            "governor_hint",
            ParamType::Text,
            "The governor's signed address for the workspace, postcard in hex.",
        ),
        ActionParam::required(
            "bootstrap",
            ParamType::Text,
            "The workspace as a joining member gets it, this host's device admitted, postcard in hex.",
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
            "The workspace's name, 1 to 80 printable characters.",
        ),
        ActionParam::optional(
            "invite_role",
            ParamType::Text,
            "Issue the workspace's first invitation, with this role.",
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
            "The workspace it is, when one moves here from the device that made it: its identity (a UUID) stays its own. A new workspace when omitted.",
        ),
    ]
}

fn create_own_params() -> Vec<ActionParam> {
    vec![
        ActionParam::optional(
            "name",
            ParamType::Text,
            "The workspace's name, 1 to 80 printable characters: \"My workspace\" when omitted. Unused once you have one.",
        ),
        ActionParam::optional(
            "room",
            ParamType::Text,
            "The workspace it is, when one of yours moves here (a UUID). A new workspace when omitted.",
        ),
        ActionParam::optional(
            "expires_in_seconds",
            ParamType::Integer,
            "How long the invitation your devices redeem to hold the workspace stays redeemable: 600 seconds when omitted.",
        ),
    ]
}

fn enter_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "room",
            ParamType::Text,
            "The workspace, as listing workspaces names it.",
        ),
        ActionParam::optional(
            "expires_in_seconds",
            ParamType::Integer,
            "How long the invitation this device redeems to hold the workspace stays redeemable: 600 seconds when omitted.",
        ),
    ]
}

/// `summary`, as the host's actions answer a workspace.
fn listing(summary: &WorkspaceSummary) -> WorkspaceListing {
    WorkspaceListing {
        workspace: summary.workspace.clone(),
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

/// The terms of a workspace's first invitation, when its creation asks for one.
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

impl Workspaces {
    /// The host's operations `device` may run.
    pub(super) fn host_rows(&self, device: &DeviceCert) -> Vec<CatalogueRow> {
        let now = tilo_clock::unix_now_ns();
        HOST_VERBS
            .iter()
            .filter(|(_, operation)| self.permits(device, None, WorkspaceOp::Host(*operation), now))
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
            || !self.permits(device, None, WorkspaceOp::Host(operation), now)
        {
            return Err(ActionHostError::UnknownAction);
        }
        let args = Args::of(&request.argument)?;
        let request_id = request_id(&device.user_public_key, request.invocation);
        match operation {
            HostOperation::CreateWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                self.create_invoke(device, &args, request_id, now).await
            }
            HostOperation::CreateOwnWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                self.create_own_invoke(device, &args, request_id, now).await
            }
            HostOperation::ListWorkspaces => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                let workspaces = self.list().await;
                answer(
                    &workspaces
                        .iter()
                        .filter(|summary| self.lists_to(device, &summary.workspace))
                        .map(listing)
                        .collect::<Vec<_>>(),
                )
            }
            HostOperation::ListRevocations => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                answer(&self.revocations().iter().cloned().collect::<Vec<_>>())
            }
            HostOperation::EnterWorkspace => {
                let workspace = args.required_text("room")?;
                let seconds = args
                    .integer("expires_in_seconds")?
                    .unwrap_or(OWN_INVITATION_SECONDS);
                let changed = self
                    .enter(workspace, device)
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
                    .issue(workspace, device, own)
                    .await
                    .map_err(|error| refused(&error))?;
                answer(&EnteredWorkspace {
                    changed,
                    invitation: ActionInvitation::of(&issued),
                })
            }
            HostOperation::AttachHost
            | HostOperation::DetachHost
            | HostOperation::HostWorkspace
            | HostOperation::UnhostWorkspace => {
                self.serving_invoke(device, operation, &args, now).await
            }
            _ => Err(ActionHostError::UnknownAction),
        }
    }

    /// Make the workspace `args` names, as `device`, already permitted to,
    /// with its first invitation when asked.
    async fn create_invoke(
        &self,
        device: &DeviceCert,
        args: &Args,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<Value, ActionHostError> {
        let name = args.required_text("name")?;
        let workspace = args.text("room")?;
        let first = first_invitation(args, request_id, now)?;
        // An agent's grant governs nothing, so it invites nobody: the
        // first invitation of a workspace it makes is for its own
        // person (their runtime joins with it), and the host issues
        // it, as the workspace's governor.
        let inviter = if device.capabilities.agent.is_some() {
            if first.as_ref().is_some_and(|terms| {
                terms.recipient_user_public_key != Some(device.user_public_key)
            }) {
                return Err(refused(
                    &"an agent's grant invites only its own person to a workspace it makes",
                ));
            }
            &self.inner.host.cert
        } else {
            device
        };
        let summary = self
            .create_workspace(name, workspace)
            .await
            .map_err(|error| refused(&error))?;
        let invitation = match first {
            Some(terms) => Some(ActionInvitation::of(
                &self
                    .issue(&summary.workspace, inviter, terms)
                    .await
                    .map_err(|error| refused(&error))?,
            )),
            None => None,
        };
        answer(&CreatedWorkspace {
            workspace: listing(&summary),
            invitation,
        })
    }

    /// Make or find the workspace of `device`'s person's own, already
    /// permitted to, and invite the person into it.
    async fn create_own_invoke(
        &self,
        device: &DeviceCert,
        args: &Args,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<Value, ActionHostError> {
        let own = self.own().ok_or(ActionHostError::UnknownAction)?;
        let person = device.user_public_key;
        let moving = args.text("room")?;
        let seconds = args
            .integer("expires_in_seconds")?
            .unwrap_or(OWN_INVITATION_SECONDS);
        let making = own.making.lock().await;
        let summary = if let Some(existing) = own.of(&person) {
            if moving.is_some_and(|moving| moving != existing) {
                return Err(ActionHostError::Refused {
                    reason: Self::second_own_refusal(&existing).to_reason(),
                });
            }
            self.list()
                .await
                .into_iter()
                .find(|summary| summary.workspace == existing)
                .ok_or_else(|| refused(&"your workspace here is gone"))?
        } else {
            let name = args.text("name")?.unwrap_or(OWN_WORKSPACE_NAME);
            let summary = self
                .create_workspace(name, moving)
                .await
                .map_err(|error| refused(&error))?;
            own.record(&person, &summary.workspace)
                .map_err(|error| refused(&error))?;
            summary
        };
        drop(making);
        // A person admitted before (another of their devices joined) is
        // made its administrator now; one joining with the invitation is
        // as they are admitted.
        self.administer_own(&summary.workspace, person, now)
            .await
            .map_err(|error| refused(&error))?;
        let terms = IssueInvitationRequest {
            request_id,
            role: MemberRole::Editor,
            recipient_user_public_key: Some(person),
            invitee_name: None,
            expires_at_ns: expiry(now, seconds),
            inviter_name: None,
        };
        // The workspace's governor, the host, invites its person.
        let issued = self
            .issue(&summary.workspace, &self.inner.host.cert, terms)
            .await
            .map_err(|error| refused(&error))?;
        answer(&CreatedWorkspace {
            workspace: listing(&summary),
            invitation: Some(ActionInvitation::of(&issued)),
        })
    }

    /// The host operations that have one host of the person serve a workspace
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
                answer(&HostedWorkspace {
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
            HostOperation::HostWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                let request = crate::hosted::HostWorkspaceRequest {
                    workspace: args.required_text("room")?.to_owned(),
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
                    .host_workspace(device, &request)
                    .await
                    .map_err(|error| refused(&error))?;
                answer(&HostedWorkspace {
                    host_hint: hex_postcard(&hint)?,
                })
            }
            HostOperation::UnhostWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                let stopped = self
                    .inner
                    .hosting
                    .unhost_workspace(device, args.required_text("room")?)
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(serde_json::json!({ "stopped": stopped }))
            }
            _ => Err(ActionHostError::UnknownAction),
        }
    }
}

#[cfg(test)]
mod own_tests;
#[cfg(test)]
mod tests;
