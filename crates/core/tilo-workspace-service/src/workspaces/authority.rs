//! Who may administer a workspace served here, decided in one place: its signed member list.
//!
//! Every administration entry (the `tilo.workspace` and `tilo.host` actions,
//! the `/workspaces` pages, calls, the moderation
//! agent) asks [`Workspaces::authorize`] with what it is about to do. The
//! workspace's member list decides for every device alike, this server's own
//! included. The answer comes in three steps:
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
//! ([`super::actions::agents`]); here it is held to what its person may, and a
//! key that is no device of theirs to no more than the workspace leaves a device
//! that is not theirs.

use tilo_action_policy::{Change, Context, Role, roles};
use tilo_user_identity::DeviceCert;

use super::{
    CollaborationError, HostedWorkspace, MemberPlace, SetMemberRoleRequest, Workspaces, system_time,
};
use crate::service_authority::ServiceOperation;

/// What a workspace administration request asks of its authority.
#[derive(Clone, Copy, Debug)]
pub(super) enum WorkspaceOp<'a> {
    /// The host's own business, which no workspace decides.
    Host(ServiceOperation),
    /// One of the workspace's governance actions on the workspace itself
    /// ([`roles::governance`]).
    Govern(&'static str),
    /// Reading one of the workspace's governance listings through `action`, the
    /// governance action whose grant a reading member holds
    /// ([`roles::governance`]): reading changes nothing, so what a key may not
    /// change ([`Self::Govern`]) it may still read.
    Read(&'static str),
    /// Let someone in at `Role` (`add-member`), never above the role of whoever lets them in:
    /// issuing an invitation, which is letting its holder in put off, and redeeming it, decided
    /// then by its issuer's grants.
    Admit(Role),
    /// Let someone in as an administrator, by an invitation: for someone who may make
    /// administrators themselves.
    AdmitAdministrator,
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
    /// Letting someone in by an invitation: at `role`, or as an administrator.
    pub(super) const fn admit(role: Role, administrator: bool) -> Self {
        if administrator {
            Self::AdmitAdministrator
        } else {
            Self::Admit(role)
        }
    }

    /// The operation's name, for the log line of a refusal.
    fn name(self) -> &'static str {
        match self {
            Self::Host(operation) => operation.name(),
            Self::Govern(action) | Self::Read(action) => action,
            Self::MakeAdministrator => "make-administrator",
            Self::MakeOwner => "make-owner",
            Self::Change(change) => change.action(),
            Self::Place(..) | Self::Admit(_) | Self::AdmitAdministrator => {
                roles::governance::ADD_MEMBER
            }
            Self::Role(..) => roles::governance::SET_ROLE,
        }
    }
}

/// What someone who tried to let a person in reads when their grants refuse it.
const ADMIT_NOT_PERMITTED: &str = "admit-not-permitted";
/// What someone who tried to let a person in above their own role reads.
const ADMIT_ABOVE_ROLE: &str = "admit-above-role";

/// Why the workspace refused `op`, in the words its asker reads at the moment they try it: one
/// letting someone in (an invitation's issue among them) learns what to do, everything else is
/// the workspace's own reason.
fn refused(op: WorkspaceOp<'_>, refusal: &tilo_action_policy::Refusal) -> CollaborationError {
    use tilo_action_policy::Refusal;
    let mut args = tilo_history::FluentArgs::new();
    let key = match (op, refusal) {
        (WorkspaceOp::Admit(_) | WorkspaceOp::AdmitAdministrator, Refusal::NotPermitted(_)) => {
            ADMIT_NOT_PERMITTED
        }
        (
            WorkspaceOp::Admit(role),
            Refusal::AboveActorRole(_) | Refusal::AboveActorGrant { .. },
        ) => {
            args.set("role", role.to_string());
            ADMIT_ABOVE_ROLE
        }
        _ => return CollaborationError::new(refusal.to_string()),
    };
    CollaborationError::new(crate::i18n::message(None, key, &args))
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
        workspace: Option<&HostedWorkspace>,
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
        let hosted = self.workspace(workspace).await?;
        let hosted = hosted.lock().await;
        self.authorize(device, Some(&hosted), op, now).map(drop)
    }

    /// Whether [`Self::authorize`] would let `device` do `op` in `workspace` at
    /// `now`: the same decision, asked without acting on it, so a catalogue
    /// can leave out what its caller may not do. A refusal here is not
    /// logged; nothing was attempted.
    pub(super) fn permits(
        &self,
        device: &DeviceCert,
        workspace: Option<&HostedWorkspace>,
        op: WorkspaceOp<'_>,
        now: u64,
    ) -> bool {
        self.decide(device, workspace, op, now).is_ok()
    }

    fn decide(
        &self,
        device: &DeviceCert,
        workspace: Option<&HostedWorkspace>,
        op: WorkspaceOp<'_>,
        now: u64,
    ) -> Result<String, CollaborationError> {
        let host = self.inner.host.cert.user_public_key;
        // A device this host placed for a Browser environment carries a certificate of the
        // host's person, and is still no device of theirs: it serves its one record, and
        // enters, lets in and administers nothing.
        self.inner
            .host
            .environments
            .refuse_joining(&device.device_public_key, now)?;
        if self.revocations().refuses(device) || self.blocked_keys().revokes(device) {
            return Err(CollaborationError::new("device revoked"));
        }
        tilo_user_identity::verify_device_cert_at(device, now)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        if let WorkspaceOp::Host(operation) = op {
            // What anyone may do here with workspaces of their own is for a host
            // that keeps them: making, deleting and branching one, and listing
            // them. A host that keeps none lists to the people it knows all the
            // same: whom its operator trusts and the members of the workspaces
            // it holds.
            let anyones = match operation {
                ServiceOperation::CreateOwnWorkspace
                | ServiceOperation::DeleteOwnWorkspace
                | ServiceOperation::BranchOwnWorkspace => true,
                ServiceOperation::ListWorkspaces => !self.knows_at_host(device),
                _ => false,
            } && device.user_public_key != host;
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
            .member_policy()
            .map_err(CollaborationError::new)?;
        // A place is changed on a person the workspace has, whoever asks: someone it does not
        // have is given one by `member.add`, never a place here.
        if let WorkspaceOp::Role(target, _) = op {
            let target = hex::encode(target);
            if !policy.is_member(&target) {
                return Err(CollaborationError::new(
                    tilo_action_policy::Refusal::NotAMember(target).to_string(),
                ));
            }
        }
        decide_by_governance(workspace, device, op, now, Some(host))
    }
}

/// Whether `device` is a device of a person `policy`, the workspace's, has as a member: one of
/// their own, certified by their root or a device of theirs, that the workspace keeps as no
/// revoked key.
fn of_a_member(
    workspace: &HostedWorkspace,
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
    workspace: &HostedWorkspace,
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
        .member_policy()
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
    workspace: &HostedWorkspace,
    device: &DeviceCert,
    op: WorkspaceOp<'_>,
    now: u64,
    host: Option<[u8; 32]>,
) -> Result<String, CollaborationError> {
    let member = member_access(workspace, device, op, now)?;
    let person = certified_person(&member, device)?;
    let cx = Context::at_time(system_time(now));
    // A place is a role, or the workspace's administration; a role a member
    // holds already is set, and anyone else is added with it.
    let change;
    let op = match op {
        WorkspaceOp::Host(_) => {
            return Err(CollaborationError::new(NO_WORKSPACE));
        }
        // Who the place belongs to was checked before: one the workspace has is
        // moved as a place is given.
        WorkspaceOp::Place(target, place) | WorkspaceOp::Role(target, place) => match place {
            MemberPlace::Owner => WorkspaceOp::MakeOwner,
            MemberPlace::Administrator => WorkspaceOp::MakeAdministrator,
            MemberPlace::Role(role) => {
                let person = hex::encode(target);
                change = if member.policy.is_member(&person) {
                    Change::SetRole { person, role }
                } else {
                    Change::AddMember { person, role }
                };
                WorkspaceOp::Change(&change)
            }
        },
        op => op,
    };
    // A key is no device of the person's own: what the workspace's own rule leaves to one of
    // those it may not do ([`a_key_may_not`]).
    if !crate::workspace_access::person_device(device)
        && let Some(refusal) = a_key_may_not(&member.policy, person, op, now)
    {
        return Err(refused(op, &refusal));
    }
    // Its person decides, whichever key of theirs asks: an agent's own grants were decided where
    // it asked.
    permits(
        &member.policy,
        op,
        &cx,
        person,
        member.acting.foreign_device,
    )
    .map_err(|refusal| refused(op, &refusal))?;
    let Some(host) = host else {
        return Ok(person.to_owned());
    };
    // This host signs what it carries out, so its own person's row on the member list must let
    // it do the same: a server acts in a workspace only through its row there, and in one whose
    // list does not have its person it carries out nothing.
    permits(&member.policy, op, &cx, &hex::encode(host), false)
        .map_err(|refusal| CollaborationError::new(format!("{OWNER_SIGNS}: {refusal}")))?;
    Ok(person.to_owned())
}

/// The person the workspace decides for in `member`, the access it gave `device`: the workspace
/// decides for the person it certified this device to, and a certificate naming anyone else
/// speaks for nobody here.
fn certified_person<'a>(
    member: &'a tilo_action_policy::MemberAccess,
    device: &DeviceCert,
) -> Result<&'a str, CollaborationError> {
    let person = member
        .person_id()
        .ok_or_else(|| CollaborationError::new("the device acts for no person"))?;
    if person != hex::encode(device.user_public_key) {
        return Err(CollaborationError::new(
            "the device's certificate names another person than the workspace admitted",
        ));
    }
    Ok(person)
}

/// What `policy`, a workspace's member list, lets `who` do as `op`, on a device of theirs
/// (`foreign` when it is no device of their own). No person observed an approval here: an
/// answer that asks first is a refusal, never a pass.
fn permits(
    policy: &tilo_action_policy::WorkspacePolicy,
    op: WorkspaceOp<'_>,
    cx: &Context<'_>,
    who: &str,
    foreign: bool,
) -> Result<(), tilo_action_policy::Refusal> {
    let authority = policy.person_on_device(who, foreign);
    match op {
        WorkspaceOp::Govern(action) | WorkspaceOp::Read(action) => {
            policy.check_action(&authority, action, cx, false)
        }
        WorkspaceOp::Admit(role) => policy.check_admission(&authority, role, cx, false),
        WorkspaceOp::AdmitAdministrator => {
            policy.check_administrator_invite(who, &authority, cx, false)
        }
        WorkspaceOp::MakeAdministrator => {
            if policy.may_make_administrators(&authority, cx) {
                Ok(())
            } else {
                Err(tilo_action_policy::Refusal::Administrators {
                    actor: who.to_owned(),
                    unmaking: false,
                })
            }
        }
        // Only the owner hands the workspace to someone else.
        WorkspaceOp::MakeOwner => {
            if policy.is_owner(who) {
                Ok(())
            } else {
                Err(tilo_action_policy::Refusal::OwnershipByOwner)
            }
        }
        WorkspaceOp::Change(change) => policy.check(who, &authority, change, cx, false),
        WorkspaceOp::Host(_) | WorkspaceOp::Place(..) | WorkspaceOp::Role(..) => {
            Err(tilo_action_policy::Refusal::NotAMember(who.to_owned()))
        }
    }
}

/// Why a key that is no device of the person's own may not do `op` in the workspace `policy`
/// decides, when the workspace's own rule refuses it: `None` where the workspace leaves the
/// operation to such a key. Its own rule decides it, asked as a member whose device is not
/// theirs ([`tilo_action_policy::Cap::FOREIGN_DEVICE`]): what that member may not is the
/// administration, the workspace itself, and a place. It lets people in at a role its person may
/// hand out, and reads what the workspace lists.
fn a_key_may_not(
    policy: &tilo_action_policy::WorkspacePolicy,
    person: &str,
    op: WorkspaceOp<'_>,
    now: u64,
) -> Option<tilo_action_policy::Refusal> {
    let capped = policy.person_on_device(person, true);
    let cx = Context::at_time(system_time(now));
    match op {
        WorkspaceOp::MakeAdministrator | WorkspaceOp::AdmitAdministrator => {
            Some(tilo_action_policy::Refusal::Administrators {
                actor: person.to_owned(),
                unmaking: false,
            })
        }
        WorkspaceOp::MakeOwner => Some(tilo_action_policy::Refusal::OwnershipByOwner),
        WorkspaceOp::Change(change) if policy.unmakes_administrator(person, change) => {
            Some(tilo_action_policy::Refusal::Administrators {
                actor: person.to_owned(),
                unmaking: true,
            })
        }
        // Letting someone in, at a role the workspace already let this key hand out, and the
        // listings a reading member holds. A place was turned into one of the above by the time
        // an operation is decided.
        WorkspaceOp::Admit(_)
        | WorkspaceOp::Read(_)
        | WorkspaceOp::Change(Change::AddMember { .. })
        | WorkspaceOp::Host(_)
        | WorkspaceOp::Place(..)
        | WorkspaceOp::Role(..) => None,
        // Letting people in is the one governance a key keeps.
        WorkspaceOp::Govern(action) if action == roles::governance::ADD_MEMBER => None,
        // The workspace itself: its name, its settings and its grants.
        WorkspaceOp::Govern(action) => policy.check_action(&capped, action, &cx, false).err(),
        // Who is in it and what they hold, beyond letting someone in.
        WorkspaceOp::Change(change) => policy.check(person, &capped, change, &cx, false).err(),
    }
}

/// Why this host does not carry out a change the workspace lets its caller
/// make: the host signs what it carries out, and its own row on the member
/// list does not let it make the change.
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
        workspace: &HostedWorkspace,
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
    /// The workspace is not hosted here, or keeps no governance.
    pub async fn membership_log(&self, workspace: &str) -> Result<Vec<String>, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let hosted = state.lock().await;
        hosted
            .peer()
            .governance()
            .membership_log()
            .map_err(CollaborationError::new)
    }

    /// The people the devices `workspace` admits now belong to, by the device:
    /// whose record a device's writes are. And the person who owns the
    /// workspace. `None` for a workspace not hosted here.
    pub async fn people_of(
        &self,
        workspace: &str,
    ) -> Option<(
        Option<[u8; 32]>,
        std::collections::BTreeMap<[u8; 32], [u8; 32]>,
    )> {
        let peer = self.state(workspace).await?;
        let now = super::system_time(tilo_clock::unix_now_ns());
        let people = peer
            .certificates()
            .into_iter()
            .filter(|cert| peer.permissions(cert.device_public_key, now).is_ok())
            .map(|cert| (cert.device_public_key, cert.user_public_key))
            .collect();
        Some((Some(peer.owner()), people))
    }

    /// The people `workspace` has as members now, its owner among them: whose
    /// holds on content a host keeps stand. One whose device's certificate
    /// ran out is still a member. `None` for a workspace not hosted here.
    pub async fn member_people(
        &self,
        workspace: &str,
    ) -> Option<std::collections::BTreeSet<[u8; 32]>> {
        let peer = self.state(workspace).await?;
        let policy = peer.governance().member_policy().ok()?;
        let owner = peer.owner();
        Some(
            peer.certificates()
                .into_iter()
                .map(|cert| cert.user_public_key)
                .filter(|person| {
                    *person == owner || policy.active_role(&hex::encode(person)).is_some()
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests;
