//! Who may administer a workspace this relay governs, decided in one place.
//!
//! Every administration door (the `tilo.workspace` and `tilo.host` actions,
//! the `/rooms` pages, calls, the moderation
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
//!    every governance action and is still bounded by their Editor role; an
//!    Editor by role alone governs nothing.

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
    /// A change to who is in the workspace or what they hold.
    Change(&'a Change),
}

impl WorkspaceOp<'_> {
    /// The operation's name, for the log line of a refusal.
    fn name(self) -> &'static str {
        match self {
            Self::Host(operation) => operation.name(),
            Self::Govern(action) => action,
            Self::Invite(_) => roles::governance::INVITE,
            Self::Change(change) => change.action(),
        }
    }
}

/// Why a device of another person cannot do what only the host's person
/// does. Kept as it was before workspaces decided their own administration, so
/// a client reading it keeps working.
const HOST_PERSON_ONLY: &str = "room administration requires a device of the relay's user";

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
                "room administration denied"
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
                "room administration requires a person's device, not an agent's",
            ));
        }
        // An agent's grant acts in workspaces through the person's runtime,
        // never on their governance: it may do only what the host's own
        // operations allow it.
        if device.capabilities.agent.is_some() && !matches!(op, WorkspaceOp::Host(_)) {
            return Err(CollaborationError::new(
                "room administration requires a person's device, not an agent's grant",
            ));
        }
        if self.revocations().is_revoked(&device.device_public_key) {
            return Err(CollaborationError::new("device revoked"));
        }
        tilo_user_identity::verify_device_cert_at(device, now)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        if let WorkspaceOp::Host(operation) = op {
            if operation == HostOperation::CreateOwnWorkspace && self.own().is_none() {
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
        decide_by_governance(workspace, device, op, now)
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
                "{HOST_PERSON_ONLY}, or a member the room lets {}: {reason}",
                op.name()
            ))
        })?;
    // The workspace decides for the person it certified this device to; a
    // certificate naming anyone else speaks for nobody here.
    if member.person != hex::encode(device.user_public_key) {
        return Err(CollaborationError::new(
            "the device's certificate names another person than the room admitted",
        ));
    }
    let bound = roles::foreign_device_bound();
    let authority = member.authority(&bound);
    let cx = Context::at_time(system_time(now));
    // No person witnessed an approval here: an answer that asks first is a
    // refusal, never a pass.
    let decided = match op {
        WorkspaceOp::Host(_) => return Err(CollaborationError::new(HOST_PERSON_ONLY)),
        WorkspaceOp::Govern(action) => member.policy.check_action(&authority, action, &cx, false),
        WorkspaceOp::Invite(role) => {
            member
                .policy
                .check_invite(&member.person, &authority, role, &cx, false)
        }
        WorkspaceOp::Change(change) => {
            member
                .policy
                .check(&member.person, &authority, change, &cx, false)
        }
    };
    decided.map_err(|refusal| CollaborationError::new(refusal.to_string()))?;
    Ok(member.person)
}

impl Workspaces {
    /// Whether `device` may give the member `request` names its new role in
    /// `workspace`: the workspace's set-role change. Setting a role also takes back an
    /// administrator's standing, which only the owner gives
    /// ([`HostOperation::SetAdministrator`]), so only the owner's devices
    /// change an administrator's role.
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
            role: request.role.into(),
        };
        let by = self.authorize(device, Some(workspace), WorkspaceOp::Change(&change), now)?;
        if device.user_public_key != self.inner.host.cert.user_public_key
            && is_administrator(workspace, request.user_public_key, now)?
        {
            let refused = CollaborationError::new(
                "only the owner changes the role of one of the room's administrators",
            );
            tracing::warn!(
                device = %hex::encode(device.device_public_key),
                operation = change.action(),
                reason = %refused,
                "room administration denied"
            );
            return Err(refused);
        }
        Ok(by)
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
        .iter()
        .any(|member| member.user_public_key == user_public_key && member.administrator))
}

#[cfg(test)]
mod tests;
