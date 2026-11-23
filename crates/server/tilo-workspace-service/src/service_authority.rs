//! What a caller may do to this host itself, where no workspace decides.
//!
//! Inside a workspace, its signed member list decides, for this host's own
//! person as for anyone (`workspaces::authority`): the host is a member and
//! holder there with the place the list gives it. Some operations name no
//! workspace: making or listing the host's workspaces, its machines, which
//! workspaces it holds. Those belong to the host's person, and this is the
//! one place that says so. Each operation is named, so a later rule (a
//! delegated administrator of the host, say) changes this table rather than
//! a comparison in a handler.
//!
//! Five are anyone's: making the workspace of their own a host that offers
//! them keeps for each person ([`ServiceOperation::CreateOwnWorkspace`]),
//! deleting one of theirs ([`ServiceOperation::DeleteOwnWorkspace`]), listing
//! workspaces, which answers anyone but the host's person only the ones of
//! their own, and keeping and reading their own workspace list
//! ([`ServiceOperation::KeepPersonalList`]). The workspace is served here like any other; the
//! person owns it, and this host stays in it as an administrator they can withdraw.
//!
//! Holding a workspace another host serves is asked of anyone too, and
//! decided by the handler: a host takes one as its work setting says, or for
//! an owner of it the host let in ([`crate::workspaces`]). Which hosts hold a
//! workspace is its owners' choice, made through its member list
//! (`server.add`, `server.remove`).
//!
//! An agent's grant (a key the person certified for a service that acts for
//! an agent they approved, [`tilo_user_identity::AgentScope`]) is the person
//! too, within its scope: it makes workspaces when it reaches all of them,
//! lists them, and reads the person's revocations. Nothing else here is an
//! agent's: not which hosts serve a workspace, not the person's machines.

use tilo_user_identity::DeviceCert;

/// An operation on the host that no workspace decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceOperation {
    CreateWorkspace,
    ListWorkspaces,
    /// Enrol a further device of a member of a workspace under its person, as
    /// the workspace's member list decides (`workspaces::authority`): anyone
    /// else comes in by an invitation.
    EnterWorkspace,
    /// Hold a workspace another host serves, as that host asks
    /// ([`crate::workspaces`]): anyone's to ask, and the host's work setting
    /// decides whose it takes.
    HostWorkspace,
    /// Stop holding a workspace for the host that handed it over: the host's
    /// person's, or that host's person's, which the handler checks.
    UnhostWorkspace,
    /// Take in what the store of a device keeping a workspace kept here keeps
    /// of it, as that device asks before it lets the workspace go: anyone's to
    /// ask, and the handler checks the workspace names the caller among those
    /// keeping it.
    CatchUpWorkspace,
    /// Read the revocations of the host's person this host enforces: the
    /// devices their root revoked, as the person's devices announced them.
    ListRevocations,
    /// Make, or answer again, the one workspace of the caller's own person on
    /// this host, and invite them into it; or invite them again into a
    /// workspace they were invited into, at the role they hold there, so
    /// another device of theirs (a key of theirs) comes in as them.
    /// Anyone's to ask, on a host that keeps workspaces of people's own
    /// (`crate::workspaces::OwnWorkspaceOffer`); the entry says whether it does.
    /// Making a new one is only for a person the site let in through its home
    /// workspace, which the handler checks against the host's membership
    /// records: anyone else hears the entry's refusal and its wait list.
    CreateOwnWorkspace,
    /// Delete a workspace of the caller's own person here, with everything
    /// the host keeps of it; the host's person deletes any it keeps. Which
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
    /// Plan a machine that is not installed yet: enter the plan document the
    /// caller wrote as an entry in their Servers workspace, which the host
    /// makes when it does not hold it yet. Only a device of the host's person
    /// that may admit a device, never an agent's grant or a stand-in.
    PlanMachine,
    /// The machines the host's person planned and has not installed: each
    /// plan as the machine reads it, with the drives its machine reported and
    /// the questions it asked.
    ListMachines,
    /// One of them, named by its plan.
    ShowMachine,
    /// The host's person's servers, as the document of their workspace
    /// names them: each one's device key, name and address.
    ListServers,
    /// Take a machine that enrolled with a plan's token into its entry, under
    /// its device key: the person's device that approved it does this, and the
    /// machine's own report follows when it holds the workspace.
    AdmitMachine,
    /// Take the server `cert` names, reached at `endpoint`, in among the host's
    /// person's servers: a new entry under its device key in their Servers
    /// document, which this host holds, as the server's own start
    /// would have made it. Only a device of the person, never an agent's grant.
    AdmitServer,
    /// Keep the disk recovery key of the calling machine in the vault of the host's person's
    /// Servers workspace, under the machine's own device key: the machine asks it for itself as
    /// it installs. Only a device of the person, never an agent's grant or a stand-in.
    KeepRecoveryKey,
    /// The disk recovery keys of the host's person's machines, as that vault keeps them. Only a
    /// device of the person, never an agent's grant or a stand-in.
    ListRecoveryKeys,
}

impl ServiceOperation {
    /// The operation's stable name, for the log line of a refusal.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::CreateWorkspace => "workspace.create",
            Self::ListWorkspaces => "workspace.list",
            Self::EnterWorkspace => "workspace.enter",
            Self::HostWorkspace => "workspace.host",
            Self::UnhostWorkspace => "workspace.unhost",
            Self::CatchUpWorkspace => "workspace.catch-up",
            Self::ListRevocations => "identity.revocations",
            Self::CreateOwnWorkspace => "workspace.create-own",
            Self::DeleteOwnWorkspace => "workspace.delete",
            Self::BranchOwnWorkspace => "workspace.branch-own",
            Self::KeepPersonalList => "workspace.personal.keep",
            Self::ReadPersonalList => "workspace.personal",
            Self::NamePersonalDevice => "workspace.personal.name",
            Self::PlanMachine => "machine.plan",
            Self::ListMachines => "machine.list",
            Self::ShowMachine => "machine.show",
            Self::ListServers => "machine.servers",
            Self::AdmitMachine => "machine.admit",
            Self::AdmitServer => "server.admit",
            Self::KeepRecoveryKey => "machine.keep-recovery-key",
            Self::ListRecoveryKeys => "machine.recovery-keys",
        }
    }

    /// Whether the operation is one of the person's machines and servers,
    /// answered from the document that names them.
    pub(crate) const fn of_machines(self) -> bool {
        matches!(
            self,
            Self::PlanMachine
                | Self::ListMachines
                | Self::ShowMachine
                | Self::ListServers
                | Self::AdmitMachine
                | Self::KeepRecoveryKey
                | Self::ListRecoveryKeys
        )
    }

    /// Whether the operation is one a workspace's own member list decides: a
    /// member's further device coming in, or where the workspace is held, its
    /// owner's or holder's to name, or the host's work setting to accept
    /// ([`crate::workspaces`]). The entry lets anyone ask, since a device of
    /// neither person may be a member of one of the workspaces it serves; a
    /// catalogue, which has no workspace to ask, offers it to the host's own
    /// person alone.
    pub(crate) const fn decided_by_its_workspace(self) -> bool {
        matches!(
            self,
            Self::EnterWorkspace
                | Self::HostWorkspace
                | Self::UnhostWorkspace
                | Self::CatchUpWorkspace
        )
    }

    /// Whether the device `caller` may do this on the host of the person
    /// `host`. Every host operation but making a person's own workspace is
    /// the host's person's own; an agent's grant does what its scope allows
    /// (see the module). The caller's certificate, revocation and agent
    /// member access are checked by the entry before it asks.
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
                    | Self::EnterWorkspace
                    | Self::HostWorkspace
                    | Self::UnhostWorkspace
                    | Self::CatchUpWorkspace
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
            | Self::HostWorkspace
            | Self::UnhostWorkspace
            | Self::CatchUpWorkspace
            | Self::PlanMachine
            | Self::ListMachines
            | Self::ShowMachine
            | Self::ListServers
            | Self::AdmitMachine
            | Self::AdmitServer
            | Self::KeepRecoveryKey
            | Self::ListRecoveryKeys => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ServiceOperation;
    use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert};

    const ALL: [ServiceOperation; 20] = [
        ServiceOperation::CreateWorkspace,
        ServiceOperation::ListWorkspaces,
        ServiceOperation::EnterWorkspace,
        ServiceOperation::HostWorkspace,
        ServiceOperation::UnhostWorkspace,
        ServiceOperation::CatchUpWorkspace,
        ServiceOperation::ListRevocations,
        ServiceOperation::CreateOwnWorkspace,
        ServiceOperation::DeleteOwnWorkspace,
        ServiceOperation::BranchOwnWorkspace,
        ServiceOperation::KeepPersonalList,
        ServiceOperation::ReadPersonalList,
        ServiceOperation::PlanMachine,
        ServiceOperation::ListMachines,
        ServiceOperation::ShowMachine,
        ServiceOperation::ListServers,
        ServiceOperation::AdmitMachine,
        ServiceOperation::AdmitServer,
        ServiceOperation::KeepRecoveryKey,
        ServiceOperation::ListRecoveryKeys,
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
                ServiceOperation::CreateOwnWorkspace
                    | ServiceOperation::DeleteOwnWorkspace
                    | ServiceOperation::BranchOwnWorkspace
                    | ServiceOperation::ListWorkspaces
                    | ServiceOperation::KeepPersonalList
                    | ServiceOperation::ReadPersonalList
                    | ServiceOperation::EnterWorkspace
                    | ServiceOperation::HostWorkspace
                    | ServiceOperation::UnhostWorkspace
                    | ServiceOperation::CatchUpWorkspace
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
                ServiceOperation::ListWorkspaces
                    | ServiceOperation::ListRevocations
                    | ServiceOperation::ReadPersonalList
            );
            let makes = matches!(
                operation,
                ServiceOperation::CreateWorkspace
                    | ServiceOperation::CreateOwnWorkspace
                    | ServiceOperation::DeleteOwnWorkspace
                    | ServiceOperation::BranchOwnWorkspace
            );
            assert_eq!(
                operation.permits(&host, &all),
                reads || makes,
                "{}",
                operation.name()
            );
            assert_eq!(
                operation.permits(&host, &one),
                reads || operation == ServiceOperation::CreateOwnWorkspace,
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
                    ServiceOperation::CreateOwnWorkspace
                        | ServiceOperation::DeleteOwnWorkspace
                        | ServiceOperation::BranchOwnWorkspace
                        | ServiceOperation::ListWorkspaces
                        | ServiceOperation::ReadPersonalList
                ),
                "{}",
                operation.name()
            );
        }
    }
}
