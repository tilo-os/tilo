//! Who sends and receives a room's window streams: the devices its governor
//! admitted, as this device's native owner projected the room's roster.
//! A connection a peer opened is bound, per room, to the membership active
//! on its first frame there, and stops counting for that room once that
//! membership is removed, even if the device is admitted again later.
use std::collections::HashMap;

use iroh::EndpointId;

use super::{MembershipEpoch, WorkspaceId};

/// One window-stream connection a peer opened to this device.
#[derive(Default)]
struct Ingress {
    connection: Option<iroh::endpoint::Connection>,
    /// The membership each room's frames on it are bound to; `None` once
    /// that membership was removed, so the connection never rebinds.
    bound: HashMap<WorkspaceId, Option<MembershipEpoch>>,
}

/// The admitted rosters of the rooms this device is in, and the
/// window-stream connections bound to them.
#[derive(Default)]
pub(super) struct RoomMembers {
    rosters: HashMap<WorkspaceId, HashMap<EndpointId, MembershipEpoch>>,
    ingress: HashMap<(EndpointId, u64), Ingress>,
}

/// What a roster change ended: the connections bound to a removed
/// membership, and the devices no room holds any more.
#[derive(Default)]
pub(super) struct Removed {
    pub connections: Vec<iroh::endpoint::Connection>,
    pub devices: Vec<EndpointId>,
}

impl RoomMembers {
    /// Whether `ws` is a room with a roster here, rather than a topic whose
    /// members gossip names.
    pub(super) fn has_roster(&self, ws: WorkspaceId) -> bool {
        self.rosters.contains_key(&ws)
    }

    /// `peer`'s membership of room `ws`.
    pub(super) fn member(&self, ws: WorkspaceId, peer: EndpointId) -> Option<MembershipEpoch> {
        self.rosters.get(&ws)?.get(&peer).copied()
    }

    /// Every device on room `ws`'s roster.
    pub(super) fn members(&self, ws: WorkspaceId) -> Vec<EndpointId> {
        self.rosters
            .get(&ws)
            .map(|roster| roster.keys().copied().collect())
            .unwrap_or_default()
    }

    /// Whether any room here still holds `device`.
    fn holds(&self, device: EndpointId) -> bool {
        self.rosters
            .values()
            .any(|roster| roster.contains_key(&device))
    }

    /// Take room `ws`'s roster as `devices`, leaving out `local`: a device
    /// already on it keeps its membership, a new one gets `mint`'s. With no
    /// devices the room has no roster here any more.
    pub(super) fn set_roster(
        &mut self,
        ws: WorkspaceId,
        local: EndpointId,
        devices: impl IntoIterator<Item = EndpointId>,
        mut mint: impl FnMut() -> MembershipEpoch,
        keep: bool,
    ) -> Removed {
        let mut previous = self.rosters.remove(&ws).unwrap_or_default();
        let mut roster = HashMap::new();
        for device in devices.into_iter().filter(|device| *device != local) {
            let epoch = previous.remove(&device).unwrap_or_else(&mut mint);
            roster.insert(device, epoch);
        }
        if keep {
            self.rosters.insert(ws, roster);
        }
        let mut removed = Removed::default();
        for (device, epoch) in previous {
            for ((peer, _), ingress) in &mut self.ingress {
                if *peer != device {
                    continue;
                }
                if let Some(bound) = ingress.bound.get_mut(&ws)
                    && *bound == Some(epoch)
                {
                    *bound = None;
                    removed.connections.extend(ingress.connection.take());
                }
            }
            if !self.holds(device) {
                removed.devices.push(device);
            }
        }
        removed
    }

    /// A peer opened (`Some`) or closed (`None`) the window-stream
    /// connection `(peer, incarnation)`.
    pub(super) fn connection(
        &mut self,
        peer: EndpointId,
        incarnation: u64,
        connection: Option<iroh::endpoint::Connection>,
    ) {
        match connection {
            Some(connection) => {
                self.ingress
                    .entry((peer, incarnation))
                    .or_default()
                    .connection = Some(connection);
            }
            None => {
                self.ingress.remove(&(peer, incarnation));
            }
        }
    }

    /// Admit a frame of room `ws` that `peer` sent on its connection
    /// `incarnation`: its membership, binding the connection to it on its
    /// first frame of the room. `None` when the peer is no member, or the
    /// connection is bound to a membership that was removed.
    pub(super) fn admit(
        &mut self,
        ws: WorkspaceId,
        peer: EndpointId,
        incarnation: u64,
    ) -> Option<MembershipEpoch> {
        let epoch = self.member(ws, peer)?;
        let ingress = self.ingress.entry((peer, incarnation)).or_default();
        match ingress.bound.get(&ws) {
            Some(Some(bound)) => (*bound == epoch).then_some(epoch),
            Some(None) => None,
            None => {
                ingress.bound.insert(ws, Some(epoch));
                Some(epoch)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOM: WorkspaceId = 7;

    fn device(seed: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn minting() -> impl FnMut() -> MembershipEpoch {
        let mut next = 0;
        move || {
            next = next.saturating_add(1);
            MembershipEpoch(next)
        }
    }

    #[test]
    fn a_room_members_frame_is_taken_and_a_non_members_dropped() {
        let (here, member, outsider) = (device(1), device(2), device(3));
        let mut members = RoomMembers::default();
        let mut mint = minting();
        members.set_roster(ROOM, here, [here, member], &mut mint, true);

        assert!(
            members.admit(ROOM, member, 1).is_some(),
            "a member's frame is taken"
        );
        assert_eq!(
            members.admit(ROOM, outsider, 1),
            None,
            "a non-member's is dropped"
        );
        assert_eq!(
            members.admit(ROOM + 1, member, 1),
            None,
            "membership of one room is no membership of another"
        );
        assert_eq!(
            members.members(ROOM),
            vec![member],
            "this device streams to members only"
        );
    }

    #[test]
    fn a_connection_stays_bound_to_the_membership_it_started_in() {
        let (here, member) = (device(1), device(2));
        let mut members = RoomMembers::default();
        let mut mint = minting();
        members.set_roster(ROOM, here, [member], &mut mint, true);
        let first = members.admit(ROOM, member, 1).expect("a member");

        // Removed: its frames stop counting, and the device leaves.
        let removed = members.set_roster(ROOM, here, [], &mut mint, true);
        assert_eq!(removed.devices, vec![member]);
        assert_eq!(members.admit(ROOM, member, 1), None);

        // Admitted again: a new membership, which the old connection never
        // carries; a new connection does.
        members.set_roster(ROOM, here, [member], &mut mint, true);
        assert_eq!(members.admit(ROOM, member, 1), None);
        let again = members.admit(ROOM, member, 2).expect("a new connection");
        assert_ne!(first, again);
    }

    #[test]
    fn a_roster_change_keeps_the_devices_that_stay() {
        let (here, stays, leaves) = (device(1), device(2), device(3));
        let mut members = RoomMembers::default();
        let mut mint = minting();
        members.set_roster(ROOM, here, [stays, leaves], &mut mint, true);
        let before = members.admit(ROOM, stays, 1);
        members.set_roster(ROOM, here, [stays], &mut mint, true);
        assert_eq!(
            members.admit(ROOM, stays, 1),
            before,
            "its connection still counts"
        );
        assert_eq!(members.member(ROOM, leaves), None);
    }
}
