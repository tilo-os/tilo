//! What a caller may do to this host itself, where no room decides.
//!
//! A room this relay governs decides its own administration from its
//! governance (`rooms::authority`). Some operations name no room: making or
//! listing the host's rooms, entering one as its owner, the host's compute.
//! Those belong to the host's person, and this is the one place that says
//! so. Each operation is named, so a later rule (a delegated administrator
//! of the host, say) changes this table rather than a comparison in a
//! handler.

/// An operation on the host that no room's governance decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostOperation {
    /// Create one of the host's rooms.
    CreateRoom,
    /// List the host's rooms.
    ListRooms,
    /// Admit the calling device to a room as its owner. Never a member's:
    /// that would let a member make any device the room's owner.
    EnterRoom,
    /// Record an owner-signed pre-admission and issue its invitation.
    PreAdmit,
    /// Write room grants the owner decided.
    PutRoomGrants,
    /// Make a member an administrator, an Editor with the owner's
    /// governance. Only the owner makes or unmakes administrators.
    SetAdministrator,
    /// Delegate an application's authorship of a room.
    GrantAppAuthorship,
    /// Install or renew a bot in a room.
    GovernBot,
    /// Author or prepare a room's applications in exact governor custody.
    #[cfg_attr(
        not(any(feature = "agents", test)),
        expect(
            dead_code,
            reason = "only a relay that runs agents authors applications"
        )
    )]
    AuthorApplications,
    /// Drive the host's personal compute over its control stream.
    ComputeControl,
}

impl HostOperation {
    /// The operation's stable name, for the log line of a refusal.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::CreateRoom => "room.create",
            Self::ListRooms => "room.list",
            Self::EnterRoom => "room.enter",
            Self::PreAdmit => "invitation.pre-admit",
            Self::PutRoomGrants => "room.grants.put",
            Self::SetAdministrator => "member.administrator.set",
            Self::GrantAppAuthorship => "app.authorship.grant",
            Self::GovernBot => "bot.govern",
            Self::AuthorApplications => "application.author",
            Self::ComputeControl => "compute.control",
        }
    }

    /// Whether a device of the person `caller` may do this on the host of
    /// the person `host`. Today every host operation is the host's person's
    /// own; the caller's certificate, revocation and agent standing are
    /// checked by the door before it asks.
    pub(crate) fn permits(self, host: &[u8; 32], caller: &[u8; 32]) -> bool {
        match self {
            Self::CreateRoom
            | Self::ListRooms
            | Self::EnterRoom
            | Self::PreAdmit
            | Self::PutRoomGrants
            | Self::SetAdministrator
            | Self::GrantAppAuthorship
            | Self::GovernBot
            | Self::AuthorApplications
            | Self::ComputeControl => host == caller,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HostOperation;

    const ALL: [HostOperation; 10] = [
        HostOperation::CreateRoom,
        HostOperation::ListRooms,
        HostOperation::EnterRoom,
        HostOperation::PreAdmit,
        HostOperation::PutRoomGrants,
        HostOperation::SetAdministrator,
        HostOperation::GrantAppAuthorship,
        HostOperation::GovernBot,
        HostOperation::AuthorApplications,
        HostOperation::ComputeControl,
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
