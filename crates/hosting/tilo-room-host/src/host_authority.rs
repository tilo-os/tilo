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
//! An agent's grant (a key the person certified for a service that acts for
//! an agent they approved, [`tilo_user_identity::AgentScope`]) is the person
//! too, within its scope: it makes workspaces when it reaches all of them,
//! lists the ones it reaches, and reads the person's revocations. Nothing
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
    /// Serve a workspace a device of the host's person governs, as its governor
    /// asks ([`crate::hosted`]).
    HostWorkspace,
    /// Stop serving a workspace for its governor.
    UnhostWorkspace,
    /// Have a host of the person serve a workspace governed here as well. Which
    /// hosts serve a workspace is its owner's decision.
    AttachHost,
    /// Stop having a host of the person serve a workspace governed here.
    DetachHost,
    /// Read the revocations of the host's person this host enforces: the
    /// devices their root revoked, as the person's devices announced them.
    ListRevocations,
}

impl HostOperation {
    /// The operation's stable name, for the log line of a refusal.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::CreateWorkspace => "room.create",
            Self::ListWorkspaces => "room.list",
            Self::EnterWorkspace => "room.enter",
            Self::PreAdmit => "invitation.pre-admit",
            Self::PutWorkspaceGrants => "room.grants.put",
            Self::SetAdministrator => "member.administrator.set",
            Self::GovernBot => "bot.govern",
            Self::AuthorApplications => "application.author",
            Self::ComputeControl => "compute.control",
            Self::HostWorkspace => "room.host",
            Self::UnhostWorkspace => "room.unhost",
            Self::AttachHost => "room.attach-host",
            Self::DetachHost => "room.detach-host",
            Self::ListRevocations => "identity.revocations",
        }
    }

    /// Whether the device `caller` may do this on the host of the person
    /// `host`. Every host operation is the host's person's own; an agent's
    /// grant of theirs does what its scope allows (see the module). The
    /// caller's certificate, revocation and agent standing are checked by the
    /// door before it asks.
    pub fn permits(self, host: &[u8; 32], caller: &DeviceCert) -> bool {
        if caller.user_public_key != *host {
            return false;
        }
        let Some(scope) = caller.capabilities.agent else {
            return true;
        };
        match self {
            Self::CreateWorkspace => scope.makes_workspaces(),
            // The listing answers only the workspaces the scope reaches.
            Self::ListWorkspaces | Self::ListRevocations => true,
            Self::EnterWorkspace
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

    const ALL: [HostOperation; 14] = [
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
        }
    }

    #[test]
    fn only_the_hosts_person_does_what_no_room_decides() {
        let host = [1; 32];
        for operation in ALL {
            assert!(
                operation.permits(&host, &device(host, DeviceCapabilities::member())),
                "{}",
                operation.name()
            );
            assert!(
                !operation.permits(&host, &device([2; 32], DeviceCapabilities::member())),
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
                HostOperation::ListWorkspaces | HostOperation::ListRevocations
            );
            let makes = operation == HostOperation::CreateWorkspace;
            assert_eq!(
                operation.permits(&host, &all),
                reads || makes,
                "{}",
                operation.name()
            );
            assert_eq!(
                operation.permits(&host, &one),
                reads,
                "{}",
                operation.name()
            );
            // Someone else's grant is someone else.
            let stranger = device(
                [2; 32],
                DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
            );
            assert!(!operation.permits(&host, &stranger), "{}", operation.name());
        }
    }
}
