//! What a caller may do to this host itself, where no workspace decides.
//!
//! A workspace this relay governs decides its own administration from its
//! governance (`workspaces::authority`). Some operations name no workspace: making or
//! listing the host's workspaces, entering one as its owner, the host's compute.
//! Those belong to the host's person, and this is the one place that says
//! so. Each operation is named, so a later rule (a delegated administrator
//! of the host, say) changes this table rather than a comparison in a
//! handler.
//!
//! Five are anyone's: making the workspace of their own a host that offers
//! them keeps for each person ([`HostOperation::CreateOwnWorkspace`]),
//! deleting one of theirs ([`HostOperation::DeleteOwnWorkspace`]), listing
//! workspaces, which answers anyone but the host's person only the ones of
//! their own, and keeping and reading their own workspace list
//! ([`HostOperation::KeepPersonalList`]). The workspace is governed here like any other; the
//! person administers it.
//!
//! Which hosts hold a workspace is asked of anyone too, and decided by the
//! handler: the workspace's owner or the person it is kept for chooses the
//! hosts of a workspace governed here, and a host takes one another host
//! governs as its work setting says ([`crate::hosted`]).
//!
//! An agent's grant (a key the person certified for a service that acts for
//! an agent they approved, [`tilo_user_identity::AgentScope`]) is the person
//! too, within its scope: it makes workspaces when it reaches all of them,
//! enters the ones it reaches, lists them, and reads the person's revocations. Nothing
//! else here is an agent's: not entering a workspace as its owner, not the
//! host's compute, not which hosts serve a workspace.

use tilo_user_identity::DeviceCert;

/// An operation on the host that no workspace's governance decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostOperation {
    CreateWorkspace,
    ListWorkspaces,
    /// Admit the calling device to a workspace as its owner. Never a member's:
    /// that would let a member make any device the workspace's owner.
    EnterWorkspace,
    /// Record an owner-signed pre-admission and issue its invitation.
    PreAdmit,
    /// Write workspace grants the owner decided.
    PutWorkspaceGrants,
    /// Make a member an administrator, an Editor with the owner's
    /// governance. Only the owner makes or unmakes administrators.
    SetAdministrator,
    /// Install or renew a bot in a workspace.
    GovernBot,
    /// Author or prepare a workspace's applications in exact governor custody.
    AuthorApplications,
    /// Drive the host's personal compute over its control stream.
    ComputeControl,
    /// Hold a workspace another host governs, as its governor asks
    /// ([`crate::hosted`]): anyone's to ask, and the host's work setting
    /// decides whose it takes.
    HostWorkspace,
    /// Stop holding a workspace for its governor: the host's person's, or the
    /// governor's person's, which the handler checks.
    UnhostWorkspace,
    /// Have another host hold a workspace governed here as well. Which hosts
    /// hold a workspace is its owner's decision, or the person's it is kept
    /// for, which the handler checks.
    AttachHost,
    /// Stop having another host hold a workspace governed here.
    DetachHost,
    /// Read the revocations of the host's person this host enforces: the
    /// devices their root revoked, as the person's devices announced them.
    ListRevocations,
    /// Make, or answer again, the one workspace of the caller's own person on
    /// this host, and invite them into it; or invite them again into a
    /// workspace they were invited into, at the role they hold there, so
    /// another device of theirs (a key of theirs) enters it as them.
    /// Anyone's to ask, on a host that keeps workspaces of people's own
    /// (`crate::workspaces::OwnWorkspaceOffer`); the door says whether it does.
    /// Making a new one is only for a person the site let in through its home
    /// workspace, which the handler checks against the host's membership
    /// records: anyone else hears the door's refusal and its wait list.
    CreateOwnWorkspace,
    /// Delete a workspace of the caller's own person here, with everything
    /// the host keeps of it; the host's person deletes any it governs. Which
    /// workspace is theirs the operation's handler checks against the host's
    /// record of who owns what. Anyone's, on a host that keeps workspaces of
    /// people's own, as making one is; an agent's grant only when it reaches
    /// all its person's workspaces, as for making one.
    DeleteOwnWorkspace,
    /// Branch a workspace of the caller's own person here: a new workspace of
    /// theirs that starts as a copy of it and goes on apart from it. Which
    /// workspace is theirs the handler checks, as for deleting one. Anyone's,
    /// on a host that keeps workspaces of people's own; an agent's grant only
    /// when it reaches all its person's workspaces, as for making one.
    BranchOwnWorkspace,
    /// Keep the caller's person's workspace list here, as one of their own
    /// devices holds it: anyone's for their own list once the site let them in
    /// or they are a member of a workspace here, never an agent's grant.
    KeepPersonalList,
    /// Read the caller's person's workspace list: anyone's for their own
    /// list; an agent's grant reads what its scope reaches.
    ReadPersonalList,
    /// Name one of the caller's person's devices (`device`, `name`): what
    /// their lists read it by from then on. Only a device of their own, never
    /// an agent's grant or a stand-in.
    NamePersonalDevice,
}

impl HostOperation {
    /// The operation's stable name, for the log line of a refusal.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::CreateWorkspace => "workspace.create",
            Self::ListWorkspaces => "workspace.list",
            Self::EnterWorkspace => "workspace.enter",
            Self::PreAdmit => "invitation.pre-admit",
            Self::PutWorkspaceGrants => "workspace.grants.put",
            Self::SetAdministrator => "member.administrator.set",
            Self::GovernBot => "bot.govern",
            Self::AuthorApplications => "application.author",
            Self::ComputeControl => "compute.control",
            Self::HostWorkspace => "workspace.host",
            Self::UnhostWorkspace => "workspace.unhost",
            Self::AttachHost => "workspace.attach-host",
            Self::DetachHost => "workspace.detach-host",
            Self::ListRevocations => "identity.revocations",
            Self::CreateOwnWorkspace => "workspace.create-own",
            Self::DeleteOwnWorkspace => "workspace.delete",
            Self::BranchOwnWorkspace => "workspace.branch-own",
            Self::KeepPersonalList => "workspace.personal.keep",
            Self::ReadPersonalList => "workspace.personal",
            Self::NamePersonalDevice => "workspace.personal.name",
        }
    }

    /// Whether the operation is about where a workspace is held, which its own
    /// governance decides: its owner's or keeper's to name, or the host's work
    /// setting to accept ([`crate::hosted`]). The door lets anyone ask, since
    /// a device of neither person may keep one of the workspaces it governs; a
    /// catalogue, which has no workspace to ask, offers it to the host's own
    /// person alone.
    pub(crate) const fn decided_by_its_workspace(self) -> bool {
        matches!(
            self,
            Self::HostWorkspace | Self::UnhostWorkspace | Self::AttachHost | Self::DetachHost
        )
    }

    /// Whether the device `caller` may do this on the host of the person
    /// `host`. Every host operation but making a person's own workspace is
    /// the host's person's own; an agent's grant does what its scope allows
    /// (see the module). The caller's certificate, revocation and agent
    /// standing are checked by the door before it asks.
    pub fn permits(self, host: &[u8; 32], caller: &DeviceCert) -> bool {
        if caller.user_public_key != *host
            && !matches!(
                self,
                Self::CreateOwnWorkspace
                    | Self::DeleteOwnWorkspace
                    | Self::BranchOwnWorkspace
                    | Self::ListWorkspaces
                    | Self::KeepPersonalList
                    | Self::ReadPersonalList
                    | Self::NamePersonalDevice
                    | Self::HostWorkspace
                    | Self::UnhostWorkspace
                    | Self::AttachHost
                    | Self::DetachHost
            )
        {
            return false;
        }
        let Some(scope) = caller.capabilities.agent else {
            return true;
        };
        match self {
            // A branch is a workspace made: for a grant that makes them.
            Self::CreateWorkspace | Self::DeleteOwnWorkspace | Self::BranchOwnWorkspace => {
                scope.makes_workspaces()
            }
            // The listing answers only the workspaces the scope reaches. A grant for one
            // workspace asks for the invitation into that one, which its person already has:
            // making another is for a grant reaching all of them, and the handler holds a
            // narrower one to the workspace it names.
            Self::ListWorkspaces
            | Self::ListRevocations
            | Self::ReadPersonalList
            | Self::CreateOwnWorkspace => true,
            // The list is what the person's own devices hold; a grant reads it.
            Self::KeepPersonalList
            | Self::NamePersonalDevice
            | Self::EnterWorkspace
            | Self::PreAdmit
            | Self::PutWorkspaceGrants
            | Self::SetAdministrator
            | Self::GovernBot
            | Self::AuthorApplications
            | Self::ComputeControl
            | Self::HostWorkspace
            | Self::UnhostWorkspace
            | Self::AttachHost
            | Self::DetachHost => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HostOperation;
    use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert};

    const ALL: [HostOperation; 19] = [
        HostOperation::CreateWorkspace,
        HostOperation::ListWorkspaces,
        HostOperation::EnterWorkspace,
        HostOperation::PreAdmit,
        HostOperation::PutWorkspaceGrants,
        HostOperation::SetAdministrator,
        HostOperation::GovernBot,
        HostOperation::AuthorApplications,
        HostOperation::ComputeControl,
        HostOperation::HostWorkspace,
        HostOperation::UnhostWorkspace,
        HostOperation::AttachHost,
        HostOperation::DetachHost,
        HostOperation::ListRevocations,
        HostOperation::CreateOwnWorkspace,
        HostOperation::DeleteOwnWorkspace,
        HostOperation::BranchOwnWorkspace,
        HostOperation::KeepPersonalList,
        HostOperation::ReadPersonalList,
    ];

    fn device(person: [u8; 32], capabilities: DeviceCapabilities) -> DeviceCert {
        DeviceCert {
            user_public_key: person,
            device_public_key: [9; 32],
            device_label: "device".into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: Some(2),
            signature: [0; 64],
            issuer: None,
        }
    }

    #[test]
    fn only_the_hosts_person_does_what_no_workspace_decides() {
        let host = [1; 32];
        for operation in ALL {
            let anyones = matches!(
                operation,
                HostOperation::CreateOwnWorkspace
                    | HostOperation::DeleteOwnWorkspace
                    | HostOperation::BranchOwnWorkspace
                    | HostOperation::ListWorkspaces
                    | HostOperation::KeepPersonalList
                    | HostOperation::ReadPersonalList
                    | HostOperation::HostWorkspace
                    | HostOperation::UnhostWorkspace
                    | HostOperation::AttachHost
                    | HostOperation::DetachHost
            );
            assert!(
                operation.permits(&host, &device(host, DeviceCapabilities::member())),
                "{}",
                operation.name()
            );
            assert_eq!(
                operation.permits(&host, &device([2; 32], DeviceCapabilities::member())),
                anyones,
                "{}",
                operation.name()
            );
        }
    }

    #[test]
    fn an_agents_grant_is_its_person_only_within_its_scope() {
        let host = [1; 32];
        let all = device(
            host,
            DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        );
        let one = device(
            host,
            DeviceCapabilities::agent_grant(AgentScope::Workspace([7; 16])),
        );
        for operation in ALL {
            let reads = matches!(
                operation,
                HostOperation::ListWorkspaces
                    | HostOperation::ListRevocations
                    | HostOperation::ReadPersonalList
            );
            let makes = matches!(
                operation,
                HostOperation::CreateWorkspace
                    | HostOperation::CreateOwnWorkspace
                    | HostOperation::DeleteOwnWorkspace
                    | HostOperation::BranchOwnWorkspace
            );
            assert_eq!(
                operation.permits(&host, &all),
                reads || makes,
                "{}",
                operation.name()
            );
            // It asks to enter its own workspace (the handler holds it to that one).
            assert_eq!(
                operation.permits(&host, &one),
                reads || operation == HostOperation::CreateOwnWorkspace,
                "{}",
                operation.name()
            );
            // Someone else's grant is someone else, who may only make, list
            // and delete the workspaces of their own.
            let stranger = device(
                [2; 32],
                DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
            );
            assert_eq!(
                operation.permits(&host, &stranger),
                matches!(
                    operation,
                    HostOperation::CreateOwnWorkspace
                        | HostOperation::DeleteOwnWorkspace
                        | HostOperation::BranchOwnWorkspace
                        | HostOperation::ListWorkspaces
                        | HostOperation::ReadPersonalList
                ),
                "{}",
                operation.name()
            );
        }
    }
}
