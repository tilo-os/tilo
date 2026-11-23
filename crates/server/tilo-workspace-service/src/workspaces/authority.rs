//! Who may administer a workspace served here, decided in one place: its signed member list.
//!
//! Every administration entry (the `tilo.workspace` and `tilo.host` actions,
//! the `/workspaces` pages, calls, the moderation
//! agent) asks [`Workspaces::authorize`] with what it is about to do. No device or
//! server governs a workspace: the workspace's member list decides for every
//! device alike, this server's own included. The answer comes in three steps:
//!
//! 1. The device must be a person's current device: certified now, not
//!    revoked, and not one of the server's agents (an agent administers
//!    nothing; it joins workspaces through its delegation).
//! 2. What no workspace decides ([`ServiceOperation`]: making this host's
//!    workspaces, its machines, which workspaces it holds) is this host's
//!    person's, but for making a person's own workspace and keeping their
//!    list, which a host that keeps them does for anyone.
//! 3. Inside a workspace, a device does what the member list lets its person
//!    do: the governance action, invitation or change is checked against
//!    the workspace's policy, read under the workspace's lock so a role
//!    change and an administration call cannot race. A device of a member
//!    the list does not hold yet is that member. A Moderator removes,
//!    bans and restricts members by its role's grants, as anyone granted
//!    those actions does; a member the owner granted `invite` invites, never
//!    above their own role; an administrator holds every governance action;
//!    the workspace keeps its owner whoever asks; an Editor by role alone
//!    administers nothing. This server's own person is no exception: in a
//!    workspace whose list does not have them, they may do nothing, and in
//!    one that does, they hold the place the list gives them, which for a
//!    workspace this server made for someone is an administrator's grant
//!    the owner can withdraw.
//!
//! What this server carries out it signs, so it carries out only what its
//! own person may do there as well: what only the workspace's owner may do is
//! signed on a device of theirs, never relayed.
//!
//! An agent's call is decided by its own grants first, where it is asked
//! ([`super::actions::agents`]); here it is held to what its person may, as
//! every key of theirs is.

use tilo_action_policy::{Change, Context, Role, roles};
use tilo_user_identity::DeviceCert;

use super::{
    CollaborationError, GovernedWorkspace, MemberPlace, SetMemberRoleRequest, Workspaces,
    system_time,
};
use crate::service_authority::ServiceOperation;
use tilo_lock::RwLockRecover as _;

/// What a workspace administration request asks of its authority.
#[derive(Clone, Copy, Debug)]
pub(super) enum WorkspaceOp<'a> {
    /// The host's own business, which no workspace decides.
    Host(ServiceOperation),
    /// Let a further device of a member in, as theirs.
    Enter,
    /// One of the workspace's governance actions on the workspace itself
    /// ([`roles::governance`]).
    Govern(&'static str),
    /// Issue an invitation carrying `Role`: the invite action, and never a
    /// role above the inviter's own.
    Invite(Role),
    /// Issue an invitation that also makes its holder an administrator: for
    /// someone who may make administrators themselves.
    InviteAdministrator,
    /// Make a member an administrator: for the owner and its administrators.
    MakeAdministrator,
    /// Hand the workspace to another person as its owner: for its owner.
    MakeOwner,
    /// A change to who is in the workspace or what they hold.
    Change(&'a Change),
    /// Give the person with this root key a place: a role, which adds them
    /// when they are not in yet, or the workspace's administration.
    Place([u8; 32], MemberPlace),
    /// Change the place of a person the workspace holds: a role, its administration, or its
    /// ownership. Someone it does not hold has no place to change; [`Self::Place`] gives them
    /// one.
    Role([u8; 32], MemberPlace),
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
            Self::Enter => "workspace.enter",
            Self::Govern(action) => action,
            Self::Invite(_) | Self::InviteAdministrator => roles::governance::INVITE,
            Self::MakeAdministrator => "make-administrator",
            Self::MakeOwner => "make-owner",
            Self::Change(change) => change.action(),
            Self::Place(..) => roles::governance::ADD_MEMBER,
            Self::Role(..) => roles::governance::SET_ROLE,
        }
    }
}

/// Why an operation inside a workspace names none here.
const NO_WORKSPACE: &str = "this server does not hold that workspace";

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

    /// Whether `device` may do `op` in the workspace `workspace` names
    /// (`None`: of the server itself) now, as [`Self::authorize`] decides once
    /// the request is carried out: what an agent's call is checked for before its person
    /// is asked ([`super::actions::agents`]), so they are never asked about
    /// what would be refused anyway.
    ///
    /// # Errors
    ///
    /// Why the device may not.
    pub(super) async fn check_now(
        &self,
        device: &DeviceCert,
        workspace: Option<&str>,
        op: WorkspaceOp<'_>,
    ) -> Result<(), CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let Some(workspace) = workspace else {
            return self.authorize(device, None, op, now).map(drop);
        };
        let governed = self.workspace(workspace).await?;
        let governed = governed.lock().await;
        self.authorize(device, Some(&governed), op, now).map(drop)
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

    /// Whether `device` may make the change `op` in `workspace` now, signed on that device
    /// itself and relayed by this host: what its person may do there, whether or not this host
    /// could sign it for them.
    ///
    /// # Errors
    ///
    /// Why the device may not.
    pub(super) async fn check_signed_now(
        &self,
        device: &DeviceCert,
        workspace: &str,
        op: WorkspaceOp<'_>,
    ) -> Result<(), CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let governed = governed.lock().await;
        self.decide_signed(
            device,
            Some(&governed),
            op,
            tilo_clock::unix_now_ns(),
            false,
        )
        .map(drop)
    }

    fn decide(
        &self,
        device: &DeviceCert,
        workspace: Option<&GovernedWorkspace>,
        op: WorkspaceOp<'_>,
        now: u64,
    ) -> Result<String, CollaborationError> {
        self.decide_signed(device, workspace, op, now, true)
    }

    /// [`Self::decide`], for a change this host signs when `host_signs`, and otherwise for one
    /// the device signs itself.
    fn decide_signed(
        &self,
        device: &DeviceCert,
        workspace: Option<&GovernedWorkspace>,
        op: WorkspaceOp<'_>,
        now: u64,
        host_signs: bool,
    ) -> Result<String, CollaborationError> {
        let host = self.inner.host.cert.user_public_key;
        // An agent is a device of its person too, but it administers nothing:
        // it joins workspaces only through its delegation. The registry is the
        // server's in every build, so is this refusal: an agent registered with
        // a server built without its supervisor is still no person.
        if self
            .inner
            .host
            .agents
            .read_recover()
            .contains_key(&device.device_public_key)
        {
            return Err(CollaborationError::new(
                "workspace administration requires a person's device, not an agent's",
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
            let anyones = matches!(
                operation,
                ServiceOperation::CreateOwnWorkspace
                    | ServiceOperation::ListWorkspaces
                    | ServiceOperation::DeleteOwnWorkspace
                    | ServiceOperation::BranchOwnWorkspace
                    | ServiceOperation::KeepPersonalList
                    | ServiceOperation::ReadPersonalList
                    | ServiceOperation::NamePersonalDevice
            ) && device.user_public_key != host;
            if anyones && self.own().is_none() {
                return Err(CollaborationError::new(
                    "this host keeps no workspaces of people's own",
                ));
            }
            return if operation.permits(&host, device) {
                Ok(hex::encode(host))
            } else {
                Err(CollaborationError::new(
                    "only a device of this server's person does that here",
                ))
            };
        }
        let workspace = workspace.ok_or_else(|| CollaborationError::new(NO_WORKSPACE))?;
        let policy = workspace
            .peer()
            .governance()
            .governed_policy()
            .map_err(CollaborationError::new)?;
        match op {
            // A further device of a member comes in as theirs, whichever server they ask;
            // anyone else comes in by an invitation.
            WorkspaceOp::Enter => {
                let person = hex::encode(device.user_public_key);
                return if of_a_member(workspace, device, &policy) {
                    Ok(person)
                } else {
                    Err(CollaborationError::new(
                        tilo_action_policy::Refusal::NotAMember(person).to_string(),
                    ))
                };
            }
            // A place is changed on a person the workspace has, whoever asks: someone it does
            // not have is given one by `member.add`, never a place here.
            WorkspaceOp::Role(target, _) => {
                let target = hex::encode(target);
                if !policy.is_member(&target) {
                    return Err(CollaborationError::new(
                        tilo_action_policy::Refusal::NotAMember(target).to_string(),
                    ));
                }
            }
            _ => {}
        }
        decide_by_governance(workspace, device, op, now, host_signs.then_some(host))
    }
}

/// Whether `device` is a device of a person `policy`, the workspace's, has as a member: one of
/// their own, certified by their root or a device of theirs, that the workspace keeps as no
/// revoked key.
fn of_a_member(
    workspace: &GovernedWorkspace,
    device: &DeviceCert,
    policy: &tilo_action_policy::WorkspacePolicy,
) -> bool {
    crate::workspace_access::person_device(device)
        && policy.is_member(&hex::encode(device.user_public_key))
        && !super::collaboration::revoked_keys(workspace).contains(&device.device_public_key)
}

/// What `workspace` lets `device` do: the access of a device its member list holds, or, for a
/// device the list does not hold yet, its person's, when it is a device of a member's own.
fn member_access(
    workspace: &GovernedWorkspace,
    device: &DeviceCert,
    op: WorkspaceOp<'_>,
    now: u64,
) -> Result<tilo_action_policy::MemberAccess, CollaborationError> {
    let held = workspace
        .peer()
        .permissions(device.device_public_key, system_time(now));
    let reason = match held {
        Ok(member) => return Ok(member),
        Err(reason) => reason,
    };
    let policy = workspace
        .peer()
        .governance()
        .governed_policy()
        .map_err(CollaborationError::new)?;
    if !of_a_member(workspace, device, &policy) {
        return Err(CollaborationError::new(format!(
            "a member the workspace lets {} is needed: {reason}",
            op.name()
        )));
    }
    Ok(tilo_action_policy::MemberAccess::person(
        policy,
        hex::encode(device.user_public_key),
    ))
}

/// Whether `op` by `device` is one the workspace's member list lets its person
/// do now, and this host, whose key signs what it carries out, may make that
/// change itself: what only the workspace's owner may do is signed on a device
/// of theirs, never relayed by its host. `host` is the host's own person, when
/// it signs the change; `None` for one the device signs itself. A device the
/// list does not hold yet is its person, when it is a device of theirs and
/// the list has them.
fn decide_by_governance(
    workspace: &GovernedWorkspace,
    device: &DeviceCert,
    op: WorkspaceOp<'_>,
    now: u64,
    host: Option<[u8; 32]>,
) -> Result<String, CollaborationError> {
    let member = member_access(workspace, device, op, now)?;
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
    // Its person decides, whichever key of theirs asks: an agent's own
    // grants were decided where it asked.
    let authority = member
        .policy
        .person_on_device(person, member.acting.foreign_device);
    let cx = Context::at_time(system_time(now));
    // No person observed an approval here: an answer that asks first is a
    // refusal, never a pass.
    let decided = match op {
        WorkspaceOp::Host(_) | WorkspaceOp::Enter => {
            return Err(CollaborationError::new(NO_WORKSPACE));
        }
        WorkspaceOp::Govern(action) => member.policy.check_action(&authority, action, &cx, false),
        WorkspaceOp::Invite(role) => member.policy.check_invite(&authority, role, &cx, false),
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
        // Only the owner hands the workspace to someone else.
        WorkspaceOp::MakeOwner => {
            if member.policy.is_owner(person) {
                Ok(())
            } else {
                Err(tilo_action_policy::Refusal::OwnershipByOwner)
            }
        }
        WorkspaceOp::Change(change) => member.policy.check(person, &authority, change, &cx, false),
        // A place is a role, or the workspace's administration; a role a member
        // holds already is set, and anyone else is added with it.
        WorkspaceOp::Place(target, place) => {
            let target = hex::encode(target);
            let change;
            let op = match place {
                MemberPlace::Owner => WorkspaceOp::MakeOwner,
                MemberPlace::Administrator => WorkspaceOp::MakeAdministrator,
                MemberPlace::Role(role) => {
                    change = if member.policy.is_member(&target) {
                        Change::SetRole {
                            person: target,
                            role,
                        }
                    } else {
                        Change::AddMember {
                            person: target,
                            role,
                        }
                    };
                    WorkspaceOp::Change(&change)
                }
            };
            return decide_by_governance(workspace, device, op, now, host);
        }
        // Who the place belongs to was checked before: one the workspace has is
        // moved as [`Self::Place`] moves one.
        WorkspaceOp::Role(target, place) => {
            return decide_by_governance(
                workspace,
                device,
                WorkspaceOp::Place(target, place),
                now,
                host,
            );
        }
    };
    decided.map_err(|refusal| CollaborationError::new(refusal.to_string()))?;
    let Some(host) = host else {
        return Ok(person.to_owned());
    };
    let host = hex::encode(host);
    let signs = member.policy.person_on_device(&host, false);
    let relayed = match op {
        WorkspaceOp::Change(change) => member.policy.check(&host, &signs, change, &cx, false),
        WorkspaceOp::MakeOwner if !member.policy.is_owner(&host) => {
            Err(tilo_action_policy::Refusal::OwnershipByOwner)
        }
        _ => Ok(()),
    };
    relayed.map_err(|refusal| CollaborationError::new(format!("{OWNER_SIGNS}: {refusal}")))?;
    Ok(person.to_owned())
}

/// Why this host does not carry out a change the workspace lets its caller
/// make: the host signs what it carries out, and the change is one only the
/// workspace's owner makes.
const OWNER_SIGNS: &str = "only the workspace's owner makes this change, and from a device of their own: \
     this host serves the workspace as one of its administrators and may not sign it for them";

impl Workspaces {
    /// Whether `device` may give the member `request` names its new role in
    /// `workspace`: the workspace's set-role change. Setting a role also takes
    /// back an administrator's access, which the workspace's policy gives
    /// only its owner
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

#[cfg(test)]
mod tests;
