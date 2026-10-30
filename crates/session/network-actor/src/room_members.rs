//! Who sends and receives a room's window streams: the devices its governor
//! admitted, as this device's native owner projected the room's roster.
//! A connection a peer opened is bound, per room, to the membership active
//! on its first frame there, and stops counting for that room once that
//! membership is removed, even if the device is admitted again later.
//! A member whose device certificate expired is removed by the first frame
//! that finds it past expiry, so no timer watches expiries.
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

/// A device on a room's roster, and when its certificate expires (Unix
/// nanoseconds; `None` when it never does).
pub type RosterDevice = (EndpointId, Option<u64>);

/// One device's membership of a room.
#[derive(Clone, Copy)]
struct Member {
    epoch: MembershipEpoch,
    expires_at_ns: Option<u64>,
}

impl Member {
    fn expired_at(&self, now_ns: u64) -> bool {
        self.expires_at_ns.is_some_and(|expiry| now_ns >= expiry)
    }
}

/// The admitted rosters of the rooms this device is in, and the
/// window-stream connections bound to them.
#[derive(Default)]
pub(super) struct RoomMembers {
    rosters: HashMap<WorkspaceId, HashMap<EndpointId, Member>>,
    ingress: HashMap<(EndpointId, u64), Ingress>,
}

/// Whether a frame counts for its room ([`RoomMembers::admit`]).
pub(super) enum Admission {
    /// It does, under this membership.
    Member(MembershipEpoch),
    /// The peer is no member, or its connection is bound to a membership
    /// that was removed.
    Refused,
    /// The peer's certificate expired: its membership ended with this frame,
    /// and what that removed is to be closed.
    Expired(Removed),
}

/// What a roster change ended: the connections bound to a removed
/// membership, and the devices no room holds any more.
#[derive(Default)]
pub(super) struct Removed {
    pub connections: Vec<iroh::endpoint::Connection>,
    pub devices: Vec<EndpointId>,
}

impl RoomMembers {
    /// Whether `ws` is a room with a roster here, rather than a personal
    /// topic whose members are the person's devices it has sessions with.
    pub(super) fn has_roster(&self, ws: WorkspaceId) -> bool {
        self.rosters.contains_key(&ws)
    }

    /// `peer`'s membership of room `ws` at `now_ns`.
    pub(super) fn member(
        &self,
        ws: WorkspaceId,
        peer: EndpointId,
        now_ns: u64,
    ) -> Option<MembershipEpoch> {
        let member = self.rosters.get(&ws)?.get(&peer)?;
        (!member.expired_at(now_ns)).then_some(member.epoch)
    }

    /// Every device on room `ws`'s roster whose certificate holds at `now_ns`.
    pub(super) fn members(&self, ws: WorkspaceId, now_ns: u64) -> Vec<EndpointId> {
        self.rosters
            .get(&ws)
            .map(|roster| {
                roster
                    .iter()
                    .filter(|(_, member)| !member.expired_at(now_ns))
                    .map(|(device, _)| *device)
                    .collect()
            })
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
        devices: impl IntoIterator<Item = RosterDevice>,
        mut mint: impl FnMut() -> MembershipEpoch,
        keep: bool,
    ) -> Removed {
        let mut previous = self.rosters.remove(&ws).unwrap_or_default();
        let mut roster = HashMap::new();
        for (device, expires_at_ns) in devices.into_iter().filter(|(device, _)| *device != local) {
            let epoch = previous
                .remove(&device)
                .map_or_else(&mut mint, |member| member.epoch);
            roster.insert(
                device,
                Member {
                    epoch,
                    expires_at_ns,
                },
            );
        }
        if keep {
            self.rosters.insert(ws, roster);
        }
        let mut removed = Removed::default();
        for (device, member) in previous {
            self.end_membership(ws, device, member.epoch, &mut removed);
        }
        removed
    }

    /// `device`'s membership `epoch` of room `ws`, already off its roster,
    /// ended: the connections bound to it stop counting and close, and so
    /// does this device's own to it once no room holds it.
    fn end_membership(
        &mut self,
        ws: WorkspaceId,
        device: EndpointId,
        epoch: MembershipEpoch,
        removed: &mut Removed,
    ) {
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
    /// `incarnation` at `now_ns`: its membership, binding the connection to
    /// it on its first frame of the room. A peer whose certificate expired
    /// leaves the roster here, and the connection the frame came on is
    /// tombstoned for the room with every other bound to that membership.
    pub(super) fn admit(
        &mut self,
        ws: WorkspaceId,
        peer: EndpointId,
        incarnation: u64,
        now_ns: u64,
    ) -> Admission {
        let Some(member) = self
            .rosters
            .get(&ws)
            .and_then(|roster| roster.get(&peer))
            .copied()
        else {
            return Admission::Refused;
        };
        let ingress = self.ingress.entry((peer, incarnation)).or_default();
        if member.expired_at(now_ns) {
            // An unbound connection binds first, so ending the membership
            // tombstones it with the rest.
            ingress.bound.entry(ws).or_insert(Some(member.epoch));
            if let Some(roster) = self.rosters.get_mut(&ws) {
                roster.remove(&peer);
            }
            let mut removed = Removed::default();
            self.end_membership(ws, peer, member.epoch, &mut removed);
            return Admission::Expired(removed);
        }
        let epoch = member.epoch;
        match ingress.bound.get(&ws) {
            Some(Some(bound)) if *bound == epoch => Admission::Member(epoch),
            Some(_) => Admission::Refused,
            None => {
                ingress.bound.insert(ws, Some(epoch));
                Admission::Member(epoch)
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

    const NOW: u64 = 1_000;

    /// `devices` on a roster with certificates that never expire.
    fn lasting(devices: &[EndpointId]) -> Vec<RosterDevice> {
        devices.iter().map(|device| (*device, None)).collect()
    }

    fn admitted(
        members: &mut RoomMembers,
        ws: WorkspaceId,
        peer: EndpointId,
        incarnation: u64,
    ) -> Option<MembershipEpoch> {
        match members.admit(ws, peer, incarnation, NOW) {
            Admission::Member(epoch) => Some(epoch),
            Admission::Refused | Admission::Expired(_) => None,
        }
    }

    fn minting() -> impl FnMut() -> MembershipEpoch {
        let mut next: u64 = 0;
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
        members.set_roster(ROOM, here, lasting(&[here, member]), &mut mint, true);

        assert!(
            admitted(&mut members, ROOM, member, 1).is_some(),
            "a member's frame is taken"
        );
        assert_eq!(
            admitted(&mut members, ROOM, outsider, 1),
            None,
            "a non-member's is dropped"
        );
        assert_eq!(
            admitted(&mut members, ROOM + 1, member, 1),
            None,
            "membership of one room is no membership of another"
        );
        assert_eq!(
            members.members(ROOM, NOW),
            vec![member],
            "this device streams to members only"
        );
    }

    #[test]
    fn a_connection_stays_bound_to_the_membership_it_started_in() {
        let (here, member) = (device(1), device(2));
        let mut members = RoomMembers::default();
        let mut mint = minting();
        members.set_roster(ROOM, here, lasting(&[member]), &mut mint, true);
        let first = admitted(&mut members, ROOM, member, 1).expect("a member");

        // Removed: its frames stop counting, and the device leaves.
        let removed = members.set_roster(ROOM, here, lasting(&[]), &mut mint, true);
        assert_eq!(removed.devices, vec![member]);
        assert_eq!(admitted(&mut members, ROOM, member, 1), None);

        // Admitted again: a new membership, which the old connection never
        // carries; a new connection does.
        members.set_roster(ROOM, here, lasting(&[member]), &mut mint, true);
        assert_eq!(admitted(&mut members, ROOM, member, 1), None);
        let again = admitted(&mut members, ROOM, member, 2).expect("a new connection");
        assert_ne!(first, again);
    }

    #[test]
    fn a_roster_change_keeps_the_devices_that_stay() {
        let (here, stays, leaves) = (device(1), device(2), device(3));
        let mut members = RoomMembers::default();
        let mut mint = minting();
        members.set_roster(ROOM, here, lasting(&[stays, leaves]), &mut mint, true);
        let before = admitted(&mut members, ROOM, stays, 1);
        members.set_roster(ROOM, here, lasting(&[stays]), &mut mint, true);
        assert_eq!(
            admitted(&mut members, ROOM, stays, 1),
            before,
            "its connection still counts"
        );
        assert_eq!(members.member(ROOM, leaves, NOW), None);
    }

    #[test]
    fn a_frame_from_a_device_whose_certificate_expired_is_dropped_and_its_connection_tombstoned() {
        let (here, lapsing, lasting_member) = (device(1), device(2), device(3));
        let mut members = RoomMembers::default();
        let mut mint = minting();
        members.set_roster(
            ROOM,
            here,
            [(lapsing, Some(NOW + 10)), (lasting_member, None)],
            &mut mint,
            true,
        );
        assert!(
            matches!(members.admit(ROOM, lapsing, 1, NOW), Admission::Member(_)),
            "before its expiry the device's frame is taken"
        );

        let Admission::Expired(removed) = members.admit(ROOM, lapsing, 1, NOW + 10) else {
            panic!("a frame at the certificate's expiry is dropped");
        };
        assert_eq!(
            removed.devices,
            vec![lapsing],
            "no room holds the device any more"
        );
        assert!(
            matches!(
                members.admit(ROOM, lapsing, 1, NOW + 10),
                Admission::Refused
            ),
            "the device is off the roster"
        );
        assert!(
            matches!(
                members.admit(ROOM, lapsing, 2, NOW + 10),
                Admission::Refused
            ),
            "a new connection does not bring it back"
        );
        assert!(
            members.ingress[&(lapsing, 1)].bound[&ROOM].is_none(),
            "the connection it came on is tombstoned for the room"
        );
        assert_eq!(members.members(ROOM, NOW + 10), vec![lasting_member]);
        assert!(matches!(
            members.admit(ROOM, lasting_member, 1, NOW + 10),
            Admission::Member(_)
        ));
    }

    #[test]
    fn a_device_past_its_certificate_expiry_is_streamed_to_no_more() {
        let (here, lapsing) = (device(1), device(2));
        let mut members = RoomMembers::default();
        members.set_roster(ROOM, here, [(lapsing, Some(NOW))], minting(), true);
        assert_eq!(members.members(ROOM, NOW - 1), vec![lapsing]);
        assert!(members.members(ROOM, NOW).is_empty());
        assert_eq!(members.member(ROOM, lapsing, NOW), None);
    }
}
