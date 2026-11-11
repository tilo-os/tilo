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
    ATTACH_HOST, ActionInvitation, CREATE_OWN_ROOM, CREATE_ROOM, CreatedWorkspace, DELETE_ROOM,
    DETACH_HOST, ENTER_ROOM, EnteredWorkspace, HOST_ACTIONS_PROVIDER, HOST_ROOM,
    HOST_ROOMS_RESOURCE, HostedWorkspace, KEEP_PERSONAL, LIST_PERSONAL, LIST_REVOCATIONS,
    LIST_ROOMS, NOT_YOUR_OWN, PersonalList, UNHOST_ROOM, WorkspaceListing,
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

/// The part a person takes in a workspace of their own, as a listing names
/// it: they made it and administer it.
const OWNER_ROLE: &str = "owner";

/// The host's operations in catalogue order, each with the
/// [`HostOperation`] it is.
const HOST_VERBS: [(Verb, HostOperation); 12] = [
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
            id: DELETE_ROOM,
            label: "Delete your workspace",
            gate: DELETE_ROOM,
            kind: roles::GOVERNANCE_KIND,
            params: workspace_params,
        },
        HostOperation::DeleteOwnWorkspace,
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
            id: LIST_PERSONAL,
            label: "List your workspaces, wherever they are hosted",
            gate: LIST_PERSONAL,
            kind: ActionKind::Read,
            params: Vec::new,
        },
        HostOperation::ReadPersonalList,
    ),
    (
        Verb {
            id: KEEP_PERSONAL,
            label: "Keep your workspace list here",
            gate: KEEP_PERSONAL,
            kind: roles::GOVERNANCE_KIND,
            params: keep_personal_params,
        },
        HostOperation::KeepPersonalList,
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

fn keep_personal_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "list",
        ParamType::Object,
        "Your workspaces, each with where it is hosted, as your device holds them.",
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
            "The new workspace's name, 1 to 80 printable characters: \"My workspace\" when omitted. Naming one makes another; omitting it answers the first you made.",
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
        role: None,
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
                answer(&self.listed(device).await)
            }
            HostOperation::DeleteOwnWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                let workspace = args.required_text("room")?;
                // The host's person deletes any workspace it governs; anyone
                // else one of their own, which their grant reaches.
                let person = device.user_public_key;
                let theirs =
                    person == self.inner.host.cert.user_public_key || self.owns(&person, workspace);
                if !theirs || !self.lists_to(device, workspace) {
                    return Err(refused(&NOT_YOUR_OWN));
                }
                self.delete(workspace)
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(serde_json::json!({ "deleted": workspace }))
            }
            HostOperation::ReadPersonalList | HostOperation::KeepPersonalList => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                self.personal_invoke(device, operation, &args).await
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

    /// Read or keep `device`'s person's workspace list, already permitted to.
    async fn personal_invoke(
        &self,
        device: &DeviceCert,
        operation: HostOperation,
        args: &Args,
    ) -> Result<Value, ActionHostError> {
        if operation == HostOperation::ReadPersonalList {
            let here = self.listed(device).await;
            return answer(&self.personal(device, &here));
        }
        let list: PersonalList = args
            .object("list")?
            .ok_or_else(|| invalid("`list` is required"))?;
        // A list is kept for a person who takes part here: one the site let
        // in, or a member of a workspace here, whom an invitation brought.
        // Anyone else holds nothing here to keep a list of.
        let person = device.user_public_key;
        if !self.inner.memberships.holds_any(&person)
            && !self.let_in(&person, tilo_clock::unix_now_ns()).await
        {
            return Err(refused(&self.not_let_in().to_reason()));
        }
        let kept = self
            .keep_personal(device, list)
            .map_err(|error| refused(&error))?;
        Ok(serde_json::json!({ "kept": kept }))
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

    /// The workspaces `device` lists on this host, each with the part their
    /// person takes in it: the host's person every one, anyone else the ones
    /// of their own and the ones they were invited into, found by their person
    /// so a host with many workspaces answers a person with a few of them in
    /// the time those few take.
    async fn listed(&self, device: &DeviceCert) -> Vec<WorkspaceListing> {
        let person = device.user_public_key;
        if person != self.inner.host.cert.user_public_key {
            let own = self.own().map(|own| own.of(&person)).unwrap_or_default();
            let mut listed: Vec<WorkspaceListing> = Vec::new();
            for workspace in &own {
                if !self.lists_to(device, workspace) {
                    continue;
                }
                if let Ok(mut one) = self.listing_of(workspace).await {
                    one.role = Some(OWNER_ROLE.to_owned());
                    listed.push(one);
                }
            }
            let now = tilo_clock::unix_now_ns();
            for workspace in self.inner.memberships.of(&person) {
                if own.contains(&workspace) || !self.lists_to(device, &workspace) {
                    continue;
                }
                let Some(role) = self.role_of(&workspace, person, now).await else {
                    continue;
                };
                if let Ok(mut one) = self.listing_of(&workspace).await {
                    one.role = Some(super::role_name(role).to_owned());
                    listed.push(one);
                }
            }
            listed.sort_by(|a, b| {
                a.name
                    .cmp(&b.name)
                    .then_with(|| a.workspace.cmp(&b.workspace))
            });
            return listed;
        }
        self.list()
            .await
            .iter()
            .filter(|summary| self.lists_to(device, &summary.workspace))
            .map(|summary| WorkspaceListing {
                role: Some(OWNER_ROLE.to_owned()),
                ..listing(summary)
            })
            .collect()
    }

    /// Make another workspace of `device`'s person's own, already permitted
    /// to, and invite the person into it: the one they have when the ask
    /// names none, the one `room` names when it is one of theirs.
    async fn create_own_invoke(
        &self,
        device: &DeviceCert,
        args: &Args,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<Value, ActionHostError> {
        let own = self.own().ok_or(ActionHostError::UnknownAction)?;
        let person = device.user_public_key;
        let name = args.text("name")?;
        let moving = args.text("room")?;
        let seconds = args
            .integer("expires_in_seconds")?
            .unwrap_or(OWN_INVITATION_SECONDS);
        // A grant for one workspace enters that one, already made: it makes no other.
        let makes = device
            .capabilities
            .agent
            .is_none_or(|scope| scope.makes_workspaces());
        if !makes
            && (name.is_some()
                || !moving.is_some_and(|room| {
                    device
                        .capabilities
                        .agent
                        .is_some_and(|scope| scope.reaches(room))
                }))
        {
            return Err(refused(&NOT_YOUR_OWN));
        }
        let making = own.making.lock().await;
        let mine = own.of(&person);
        let theirs = moving.filter(|moving| mine.iter().any(|own| own == moving));
        let summary = if let Some(moving) = theirs {
            // One of theirs: a device of theirs entering it, or a grant's
            // parent asking for the invitation into it.
            self.summary(moving)
                .await
                .map_err(|error| refused(&error))?
        } else if let Some(joined) = moving.filter(|_| name.is_none())
            && let Some(role) = self.role_of(joined, person, now).await
        {
            // Another's, which the person was invited into: another device
            // of theirs (their key) enters it as them, at the role they hold.
            drop(making);
            let summary = self
                .summary(joined)
                .await
                .map_err(|error| refused(&error))?;
            return self
                .invite_own_person(summary, person, role, request_id, expiry(now, seconds))
                .await;
        } else if name.is_none() && moving.is_some() && !mine.is_empty() {
            // A workspace this host does not keep for them, asked for as one
            // of theirs.
            return Err(refused(&NOT_YOUR_OWN));
        } else if name.is_none()
            && moving.is_none()
            && let Some(first) = mine.first()
        {
            // A plain ask for "my workspace" answers the first they made.
            self.summary(first).await.map_err(|error| refused(&error))?
        } else if !makes {
            return Err(refused(&NOT_YOUR_OWN));
        } else if !self.let_in(&person, now).await {
            // A new workspace is for a person the site let in. Anyone can
            // make a root, so a certificate alone is nobody's way past the
            // door: they hear the door's own refusal, with its wait list.
            return Err(refused(&self.not_let_in().to_reason()));
        } else {
            // Another one: there is no bound on how many a person keeps.
            let name = name.unwrap_or(OWN_WORKSPACE_NAME);
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
        self.invite_own_person(
            summary,
            person,
            MemberRole::Editor,
            request_id,
            expiry(now, seconds),
        )
        .await
    }

    /// The role `person` holds in `workspace` now, a member it has not
    /// removed; `None` for anyone else, and for a workspace not governed
    /// here.
    async fn role_of(&self, workspace: &str, person: [u8; 32], now: u64) -> Option<MemberRole> {
        let governed = self.workspace(workspace).await.ok()?;
        let governed = governed.lock().await;
        governed
            .peer()
            .governance()
            .governed_members(crate::workspaces::system_time(now))
            .ok()?
            .into_iter()
            .find(|member| member.user_public_key == person && !member.removed)
            .map(|member| member.role)
    }

    /// Answer `summary` with an invitation for its `person` as `role`, until
    /// `expires_at_ns`: the workspace's governor, the host, invites them.
    async fn invite_own_person(
        &self,
        summary: WorkspaceSummary,
        person: [u8; 32],
        role: MemberRole,
        request_id: [u8; 32],
        expires_at_ns: u64,
    ) -> Result<Value, ActionHostError> {
        let terms = IssueInvitationRequest {
            request_id,
            role,
            recipient_user_public_key: Some(person),
            invitee_name: None,
            expires_at_ns,
            inviter_name: None,
        };
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
mod personal_tests;
#[cfg(test)]
mod tests;
