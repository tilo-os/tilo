//! What a caller may do to this host itself, where no workspace decides.
//!
//! A workspace this relay governs decides its own administration from its
//! governance (`workspaces::authority`). Some operations name no workspace: making or
//! listing the host's workspaces, entering one as its owner, the host's compute.
//! Those belong to the host's person, and this is the one place that says
//! so. Each operation is named, so a later rule (a delegated administrator
//! of the host, say) changes this table rather than a comparison in a
//! handler.

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
        }
    }

    /// Whether a device of the person `caller` may do this on the host of
    /// the person `host`. Today every host operation is the host's person's
    /// own; the caller's certificate, revocation and agent standing are
    /// checked by the door before it asks.
    pub fn permits(self, host: &[u8; 32], caller: &[u8; 32]) -> bool {
        match self {
            Self::CreateWorkspace
            | Self::ListWorkspaces
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
            | Self::DetachHost => host == caller,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HostOperation;

    const ALL: [HostOperation; 13] = [
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
    ];

    #[test]
    fn only_the_hosts_person_does_what_no_room_decides() {
        let host = [1; 32];
        for operation in ALL {
            assert!(operation.permits(&host, &host), "{}", operation.name());
            assert!(!operation.permits(&host, &[2; 32]), "{}", operation.name());
        }
    }
}
