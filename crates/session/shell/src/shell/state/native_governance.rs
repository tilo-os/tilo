//! Which of this session's rooms are native rooms, and when the older engine
//! may stop writing the ones a state file from before already held.
use super::{RoomAuthority, RoomId, WorkspaceId, WorkspaceTree};

impl WorkspaceTree {
    /// Whether `room` is one this device governs itself, by the older
    /// room-authority record and the workspace that carries it. A room
    /// another person's device or relay governs is not.
    pub(super) fn governed_here(&self, room: RoomId) -> bool {
        if room.is_nil() || !self.has_workspace(room) {
            return false;
        }
        match self.room_authority(room) {
            RoomAuthority::Local => true,
            RoomAuthority::Person(person) => self.local_person == Some(person),
            RoomAuthority::Peer(_) | RoomAuthority::Unpinned => false,
        }
    }

    /// Move this session's governed rooms onto the native room owner. Called
    /// once the device's identity and owner are available; it changes no
    /// stored state by itself, and new rooms are native from their creation.
    pub fn enable_native_governance(&mut self) {
        self.native_governance = true;
    }

    /// Mark the rooms that predate this session as all on the native owner.
    pub(crate) fn native_rooms_ready(&mut self) {
        self.native_rooms_ready = true;
    }

    /// Whether this session still has to create a governed room it found.
    pub(crate) fn native_governance_settled(&self) -> bool {
        self.native_governance && self.rooms_awaiting_native_governance().is_empty()
    }

    /// The workspaces whose rooms this session governs and whose native
    /// owner has not created yet, as `(room, workspace, name)`. The network
    /// creates one governed room for each; until then `requires_native_owner`
    /// keeps the older engine away from it.
    pub(crate) fn rooms_awaiting_native_governance(&self) -> Vec<(RoomId, WorkspaceId, String)> {
        if !self.native_governance {
            return Vec::new();
        }
        self.desktop
            .workspaces
            .iter()
            .chain(&self.archived_workspaces)
            .filter(|workspace| {
                let room = workspace.room();
                !room.is_nil()
                    && !self.native_authority.holds(room)
                    && !self.joining_rooms.contains(&room)
                    && !self
                        .native_authority
                        .joins()
                        .any(|(joining, _)| joining == room)
                    && self.governed_here(room)
            })
            .map(|workspace| (workspace.room(), workspace.id, workspace.name.clone()))
            .collect()
    }
}
