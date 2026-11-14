//! Who may administer a workspace this relay governs, decided in one place.
//!
//! Every administration door (the `tilo.workspace` and `tilo.host` actions,
//! the `/workspaces` pages, calls, the moderation
//! agent) asks [`Workspaces::authorize`] with what it is about to do. The answer comes in three steps:
//!
//! 1. The device must be a person's current device: certified now, not
//!    revoked, and not one of the relay's agents (an agent governs nothing;
//!    it joins workspaces through its delegation).
//! 2. A device of the relay's own person administers every workspace here. That
//!    person is the host's, so what no workspace decides ([`HostOperation`]) is
//!    theirs alone, but for making a person's own workspace, which a host
//!    that keeps them does for anyone.
//! 3. Any other device administers a workspace as far as the workspace's governance
//!    lets its person: the governance action, invitation or change is
//!    checked against the workspace's policy, read under the workspace's lock so a
//!    role change and an administration call cannot race. A Moderator
//!    removes and bans those ranked below; a member the owner granted
//!    `invite` invites, never above their own role; an administrator holds
//!    every governance action over every member who is not an administrator;
//!    an Editor by role alone governs nothing.
//!
//! An agent's grant governs nothing, with one narrow exception: a person's
//! server key invites for them, as an Editor at most, into the workspaces
//! they administer (`server_key_invites`).

use tilo_action_policy::{Change, Context, Role, roles};
use tilo_user_identity::DeviceCert;

use super::{CollaborationError, GovernedWorkspace, SetMemberRoleRequest, Workspaces, system_time};
use crate::host_authority::HostOperation;
use tilo_lock::RwLockRecover as _;

/// What a workspace administration request asks of its authority.
#[derive(Clone, Copy, Debug)]
pub(super) enum WorkspaceOp<'a> {
    /// The host's own business, which no workspace's governance decides.
    Host(HostOperation),
    /// One of the workspace's governance actions on the workspace itself
    /// ([`roles::governance`]).
    Govern(&'static str),
    /// Issue an invitation carrying `Role`: the invite action, and never a
    /// role above the inviter's own.
    Invite(Role),
    /// Issue an invitation that also makes its holder an administrator: for
    /// someone who may make administrators themselves.
    InviteAdministrator,
    /// Make a member an administrator: for the owner, whom the workspace is
    /// kept for, and its administrators.
    MakeAdministrator,
    /// A change to who is in the workspace or what they hold.
    Change(&'a Change),
}

impl WorkspaceOp<'_> {
    /// The invitation an issue asks for: at `role`, or an administrator's.
    pub(super) const fn invite(role: Role, administrator: bool) -> Self {
        if administrator {
            Self::InviteAdministrator
        } else {
            Self::Invite(role)
        }
    }

    /// The operation's name, for the log line of a refusal.
    fn name(self) -> &'static str {
        match self {
            Self::Host(operation) => operation.name(),
            Self::Govern(action) => action,
            Self::Invite(_) | Self::InviteAdministrator => roles::governance::INVITE,
            Self::MakeAdministrator => "make-administrator",
            Self::Change(change) => change.action(),
        }
    }
}

/// Why a device of another person cannot do what only the host's person
/// does. Kept as it was before workspaces decided their own administration, so
/// a client reading it keeps working.
const HOST_PERSON_ONLY: &str = "workspace administration requires a device of the relay's user";

impl Workspaces {
    /// Whether `device` may do `op` in `workspace` (which every operation but a
    /// [`WorkspaceOp::Host`] one names) at `now`, Unix nanoseconds. The answer
    /// is the person deciding it, by their hex root key: the workspace's history
    /// names them.
    ///
    /// # Errors
    ///
    /// Why the device may not.
    pub(super) fn authorize(
        &self,
        device: &DeviceCert,
        workspace: Option<&GovernedWorkspace>,
        op: WorkspaceOp<'_>,
        now: u64,
    ) -> Result<String, CollaborationError> {
        let result = self.decide(device, workspace, op, now);
        if let Err(reason) = &result {
            tracing::warn!(
                device = %hex::encode(device.device_public_key),
                operation = op.name(),
                %reason,
                "workspace administration denied"
            );
        }
        result
    }

    /// Whether [`Self::authorize`] would let `device` do `op` in `workspace` at
    /// `now`: the same decision, asked without acting on it, so a catalogue
    /// can leave out what its caller may not do. A refusal here is not
    /// logged; nothing was attempted.
    pub(super) fn permits(
        &self,
        device: &DeviceCert,
        workspace: Option<&GovernedWorkspace>,
        op: WorkspaceOp<'_>,
        now: u64,
    ) -> bool {
        self.decide(device, workspace, op, now).is_ok()
    }

    fn decide(
        &self,
        device: &DeviceCert,
        workspace: Option<&GovernedWorkspace>,
        op: WorkspaceOp<'_>,
        now: u64,
    ) -> Result<String, CollaborationError> {
        let host = self.inner.host.cert.user_public_key;
        let relay_person = device.user_public_key == host;
        // An agent is a device of the relay's user too, but it governs
        // nothing: it joins workspaces only through its delegation. The registry
        // is the relay's in every build, so is this refusal: an agent
        // registered with a relay built without its supervisor is still no
        // person.
        let agent = relay_person
            && self
                .inner
                .host
                .agents
                .read_recover()
                .contains_key(&device.device_public_key);
        if agent {
            return Err(CollaborationError::new(
                "workspace administration requires a person's device, not an agent's",
            ));
        }
        // An agent's grant acts in workspaces through the person's runtime,
        // never on their governance: it may do only what the host's own
        // operations allow it. A person's server key alone also invites for
        // them ([`server_key_invites`]).
        if device.capabilities.agent.is_some()
            && !matches!(op, WorkspaceOp::Host(_))
            && !server_key_invites(device, op)
        {
            return Err(CollaborationError::new(
                "workspace administration requires a person's device, not an agent's grant",
            ));
        }
        if self.revocations().refuses(device) {
            return Err(CollaborationError::new("device revoked"));
        }
        tilo_user_identity::verify_device_cert_at(device, now)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        if let WorkspaceOp::Host(operation) = op {
            // What anyone may do here is for a host that keeps workspaces of
            // people's own: making theirs, listing them, and keeping their list.
            // The host's own person needs no offer: every workspace governed
            // here is theirs, and a key of theirs enters one by asking for it.
            let anyones = matches!(
                operation,
                HostOperation::CreateOwnWorkspace
                    | HostOperation::ListWorkspaces
                    | HostOperation::DeleteOwnWorkspace
                    | HostOperation::KeepPersonalList
                    | HostOperation::ReadPersonalList
                    | HostOperation::NamePersonalDevice
            ) && !relay_person;
            if anyones && self.own().is_none() {
                return Err(CollaborationError::new(
                    "this host keeps no workspaces of people's own",
                ));
            }
            return if operation.permits(&host, device) {
                Ok(hex::encode(host))
            } else {
                Err(CollaborationError::new(HOST_PERSON_ONLY))
            };
        }
        if relay_person {
            if let (WorkspaceOp::Change(change), Some(workspace)) = (op, workspace) {
                refuse_changing_the_owner(workspace, change)?;
            }
            return Ok(hex::encode(host));
        }
        let workspace = workspace.ok_or_else(|| CollaborationError::new(HOST_PERSON_ONLY))?;
        if device.capabilities.server_key
            && !is_administrator(workspace, device.user_public_key, now)?
        {
            return Err(CollaborationError::new(
                "a server key invites only into a workspace its person administers",
            ));
        }
        decide_by_governance(workspace, device, op, now)
    }
}

/// The highest role a person's server key invites anyone as.
pub(super) const SERVER_KEY_INVITES_AT_MOST: Role = Role::Editor;

/// Whether `device`, an agent's grant, may still do `op`: a person's server
/// key (an all-workspaces key they made for their own backend, never the
/// grant an agent gets through their consent nor a key minted for one
/// workspace) invites for its person into a workspace that person
/// administers, never as more than [`SERVER_KEY_INVITES_AT_MOST`] nor above
/// the person's own role, and lists and cancels those invitations. Every
/// other governance action stays the person's devices' own.
fn server_key_invites(device: &DeviceCert, op: WorkspaceOp<'_>) -> bool {
    device.capabilities.server_key
        && match op {
            WorkspaceOp::Invite(role) => role <= SERVER_KEY_INVITES_AT_MOST,
            // An administrator's invitation is the person's own decision as
            // much as one at a role is: their key makes one where they may
            // make administrators themselves, which the workspace decides.
            WorkspaceOp::InviteAdministrator => true,
            WorkspaceOp::Govern(action) => action == roles::governance::INVITE,
            WorkspaceOp::Host(_) | WorkspaceOp::Change(_) | WorkspaceOp::MakeAdministrator => false,
        }
}

/// The owner's devices administer everything but the owner's own place.
fn refuse_changing_the_owner(
    workspace: &GovernedWorkspace,
    change: &Change,
) -> Result<(), CollaborationError> {
    let owner = hex::encode(workspace.peer().owner());
    if let Change::RemoveMember { person }
    | Change::Ban { person }
    | Change::Restrict { person, .. } = change
        && *person == owner
    {
        return Err(CollaborationError::new("the owner cannot be removed"));
    }
    Ok(())
}

/// `op` by `device`, a device of a person other than the relay's, as the
/// workspace's governance decides it now.
fn decide_by_governance(
    workspace: &GovernedWorkspace,
    device: &DeviceCert,
    op: WorkspaceOp<'_>,
    now: u64,
) -> Result<String, CollaborationError> {
    let member = workspace
        .peer()
        .permissions(device.device_public_key, system_time(now))
        .map_err(|reason| {
            CollaborationError::new(format!(
                "{HOST_PERSON_ONLY}, or a member the workspace lets {}: {reason}",
                op.name()
            ))
        })?;
    // The workspace decides for the person it certified this device to; a
    // certificate naming anyone else speaks for nobody here.
    let person = member
        .person_id()
        .ok_or_else(|| CollaborationError::new("the device acts for no person"))?;
    if person != hex::encode(device.user_public_key) {
        return Err(CollaborationError::new(
            "the device's certificate names another person than the workspace admitted",
        ));
    }
    let authority = member.authority();
    let cx = Context::at_time(system_time(now));
    // No person witnessed an approval here: an answer that asks first is a
    // refusal, never a pass.
    let decided = match op {
        WorkspaceOp::Host(_) => return Err(CollaborationError::new(HOST_PERSON_ONLY)),
        WorkspaceOp::Govern(action) => member.policy.check_action(&authority, action, &cx, false),
        WorkspaceOp::Invite(role) => member
            .policy
            .check_invite(person, &authority, role, &cx, false),
        WorkspaceOp::InviteAdministrator => member
            .policy
            .check_administrator_invite(person, &authority, &cx, false),
        WorkspaceOp::MakeAdministrator => {
            if member.policy.may_make_administrators(&authority, &cx) {
                Ok(())
            } else {
                Err(tilo_action_policy::Refusal::Administrators {
                    actor: person.to_owned(),
                    unmaking: false,
                })
            }
        }
        WorkspaceOp::Change(change) => member.policy.check(person, &authority, change, &cx, false),
    };
    decided.map_err(|refusal| CollaborationError::new(refusal.to_string()))?;
    Ok(person.to_owned())
}

impl Workspaces {
    /// Whether `device` may give the member `request` names its new role in
    /// `workspace`: the workspace's set-role change. Setting a role also takes
    /// back an administrator's standing, which the workspace's policy gives
    /// only its owner and whom it is kept for
    /// ([`tilo_action_policy::WorkspacePolicy::may_unmake_administrators`]).
    ///
    /// # Errors
    ///
    /// Why the device may not.
    pub(super) fn authorize_role_change(
        &self,
        workspace: &GovernedWorkspace,
        device: &DeviceCert,
        request: &SetMemberRoleRequest,
        now: u64,
    ) -> Result<String, CollaborationError> {
        let change = Change::SetRole {
            person: hex::encode(request.user_public_key),
            role: request.role,
        };
        self.authorize(device, Some(workspace), WorkspaceOp::Change(&change), now)
    }
}

impl Workspaces {
    /// How `workspace`'s membership log stands against its rows, for an
    /// operator ([`tilo_peer_state::PeerGovernance::membership_log`]).
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, or holds no governance.
    pub async fn membership_log(&self, workspace: &str) -> Result<Vec<String>, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let governed = state.lock().await;
        governed
            .peer()
            .governance()
            .membership_log()
            .map_err(CollaborationError::new)
    }
}

/// Whether the person `user_public_key` administers `workspace`.
fn is_administrator(
    workspace: &GovernedWorkspace,
    user_public_key: [u8; 32],
    now: u64,
) -> Result<bool, CollaborationError> {
    Ok(workspace
        .peer()
        .governance()
        .governed_members(system_time(now))
        .map_err(CollaborationError::new)?
        .members
        .iter()
        .any(|member| member.root() == Some(user_public_key) && member.administrator))
}

#[cfg(test)]
mod tests;
