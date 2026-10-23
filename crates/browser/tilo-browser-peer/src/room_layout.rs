//! Whether each room this page joined shares its members' layout, as the
//! room's governance says ([`tilo_desktop_model::projection::RoomLayout`]).
//!
//! The scope is a field of the room document, which its governor alone
//! writes and the host relays to every member as the room's topology
//! (`NeighbourMsg::SyncResp`). A page keeps that document only to read the
//! field: in a room whose layout is personal its strips and windows stay on
//! the page, and it sends the room no desktop writes, which the room would
//! refuse.

use std::collections::BTreeMap;

use automerge::AutoCommit;
use tilo_automerge::ApplyIncoming as _;
use tilo_desktop_model::projection::{RoomLayout, room_layout};

/// The room documents this page heard, by room.
#[derive(Default)]
pub(crate) struct RoomLayouts {
    docs: BTreeMap<String, AutoCommit>,
}

impl RoomLayouts {
    /// Take in `changes` of `room`'s document; returns the room's layout
    /// scope as it now stands. Changes that do not load are skipped: the
    /// scope stays what the document held.
    pub(crate) fn hear(&mut self, room: &str, changes: &[Vec<u8>]) -> RoomLayout {
        let doc = self.docs.entry(room.to_owned()).or_default();
        for change in changes {
            let _ = doc.load_incoming(change);
        }
        room_layout(doc)
    }

    /// The members `room`'s document lists, by person id, once it lists
    /// any ([`tilo_desktop_model::projection::room_members`]).
    pub(crate) fn members(&self, room: &str) -> Option<std::collections::BTreeSet<String>> {
        self.docs
            .get(room)
            .and_then(tilo_desktop_model::projection::room_members)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;
    use tilo_desktop_model::projection::ROOM_LAYOUT_KEY;

    /// A room is shared until its governance says personal, and follows it
    /// when it changes back.
    #[test]
    fn a_room_follows_its_governance() {
        let mut governor = AutoCommit::new();
        let mut layouts = RoomLayouts::default();
        governor.put(automerge::ROOT, "room", "r").unwrap();
        assert_eq!(layouts.hear("r", &[governor.save()]), RoomLayout::Shared);
        let heads = governor.get_heads();
        governor
            .put(automerge::ROOT, ROOM_LAYOUT_KEY, RoomLayout::Personal.id())
            .unwrap();
        let personal = governor.save_after(&heads);
        assert_eq!(layouts.hear("r", &[personal]), RoomLayout::Personal);
        assert_eq!(
            layouts.hear("other", &[]),
            RoomLayout::Shared,
            "another room"
        );
        let heads = governor.get_heads();
        governor
            .put(automerge::ROOT, ROOM_LAYOUT_KEY, RoomLayout::Shared.id())
            .unwrap();
        assert_eq!(
            layouts.hear("r", &[governor.save_after(&heads)]),
            RoomLayout::Shared
        );
    }
}
