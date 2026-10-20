//! Public rooms share their apps' records and nothing of their members'
//! desktops.
//!
//! A room is public when it lets people in through a door: a site's (the
//! persistent room a community site runs, [`Rooms::make_public`]) or a
//! public directory's (a listed room, [`Rooms::publish`]). The people in
//! it are strangers to each other, so no member's windows, strips or layout
//! may reach anyone: which apps someone has open, and their titles, are the
//! person's own. Only the room's app documents (Chat, Notes, Canvas
//! records) are shared.
//!
//! The governor holds to that:
//! - a member's change to the room's topology (`SyncResp`) or to its shared
//!   desktop record is refused at every door it enters by
//!   ([`Rooms::decide_layout_write`]): neither kept, nor forwarded, nor
//!   taken in;
//! - what the relay retained of such changes before is left out of every
//!   replay ([`crate::store::WorkspaceStore::replay_without_member_layout`]),
//!   and the governor's copy of the shared desktop no longer takes it in
//!   when the room opens.
//!
//! Once public, a room stays public while the relay runs: the people who
//! came in through its door are still members after it leaves the
//! directory.

use std::collections::BTreeSet;

use tilo_lock::MutexRecover as _;

use super::desktop_writes::is_desktop;
use super::{CollaborationError, GovernedRoom, Rooms};

/// Why a public room refuses a member's change to its layout. A device from
/// before public rooms refused them sends its windows again every few
/// seconds, so the refusal is not worth a warning each time.
pub(crate) const PUBLIC_LAYOUT_REFUSAL: &str = "a public room shares no member's windows or layout";

/// The rooms public here, by room UUID.
#[derive(Default)]
pub(super) struct PublicRooms(std::sync::Mutex<BTreeSet<String>>);

impl PublicRooms {
    pub(super) fn contains(&self, room: &str) -> bool {
        self.0.lock_recover().contains(room)
    }

    /// Note `room` public; true when it was not yet.
    fn insert(&self, room: &str) -> bool {
        self.0.lock_recover().insert(room.to_owned())
    }
}

/// What a member's retained write `body` does to a room's shared layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LayoutWrite {
    /// Changes the room document of `room`: its topology, windows included.
    Topology,
    /// Writes the room's shared desktop record.
    Desktop,
}

/// Whether `body`, a member's retained write to `room`, changes the room's
/// shared layout, and how: what a door asks before deciding it.
pub(super) fn layout_write(room: &str, body: &[u8]) -> Option<LayoutWrite> {
    match postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body).ok()? {
        neighbour_wire::NeighbourMsg::SyncResp { room: named, .. } if named == room => {
            Some(LayoutWrite::Topology)
        }
        neighbour_wire::NeighbourMsg::AppDocument {
            app_id, record_id, ..
        } if is_desktop(&app_id, &record_id) => Some(LayoutWrite::Desktop),
        _ => None,
    }
}

impl Rooms {
    /// Whether `room` is public: it lets people in through a door.
    #[must_use]
    pub fn is_public(&self, room: &str) -> bool {
        self.inner.public.contains(room)
    }

    /// Note each room that is public as the relay opens: the directory
    /// lists it.
    pub(super) async fn learn_public(&self) {
        let rooms: Vec<String> = self.inner.rooms.lock().await.keys().cloned().collect();
        for room in rooms {
            if self.listing(&room).is_some() {
                self.inner.public.insert(&room);
            }
        }
    }

    /// Make `room` public: the persistent room a community site lets
    /// visitors into through its door, or a room just listed. From now on
    /// its doors refuse every member's change to its layout, and what
    /// members wrote of it before is replayed to nobody.
    ///
    /// # Errors
    ///
    /// The room is not governed here, or its workspace could not be bound.
    pub async fn make_public(&self, room: &str) -> Result<(), CollaborationError> {
        self.inner.public.insert(room);
        let state = self.room(room).await?;
        let governed = state.lock().await;
        let bound = self.bind(&governed).await;
        drop(governed);
        bound.map_err(CollaborationError::new)
    }

    /// Decide `author`'s write `body` (a retained message) to `room`'s
    /// shared layout: in a public room a member's change to the room's
    /// topology or shared desktop is refused, and a desktop write anywhere
    /// is decided by what it does to what others made
    /// ([`Self::decide_desktop_write`]). Anything else, and the relay's own
    /// writes, pass untouched.
    ///
    /// # Errors
    /// Why the room refuses it.
    pub(crate) async fn decide_layout_write(
        &self,
        room: &str,
        author: [u8; 32],
        body: &[u8],
    ) -> Result<(), String> {
        if author == self.inner.host.cert.device_public_key {
            return Ok(());
        }
        let Some(write) = layout_write(room, body) else {
            return Ok(());
        };
        if self.is_public(room) {
            return Err(PUBLIC_LAYOUT_REFUSAL.into());
        }
        match write {
            LayoutWrite::Topology => Ok(()),
            LayoutWrite::Desktop => self.decide_desktop_write(room, author, body).await,
        }
    }

    /// Whether the governor of `room` takes in `author`'s retained write
    /// `message` when it replays what the relay retains: never a member's
    /// shared desktop write in a public room.
    pub(super) fn recalls(
        &self,
        room: &GovernedRoom,
        author: [u8; 32],
        message: &neighbour_wire::NeighbourMsg,
    ) -> bool {
        let neighbour_wire::NeighbourMsg::AppDocument {
            app_id, record_id, ..
        } = message
        else {
            return true;
        };
        author == self.inner.host.cert.device_public_key
            || !is_desktop(app_id, record_id)
            || !self.is_public(room.room())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_door_knows_a_layout_write_from_any_other() {
        let room = "07848ffe-1898-4f72-8419-0b54c93e6dad";
        let topology = postcard::to_stdvec(&neighbour_wire::NeighbourMsg::SyncResp {
            changes: vec![vec![1]],
            room: room.into(),
        })
        .unwrap();
        assert_eq!(layout_write(room, &topology), Some(LayoutWrite::Topology));
        assert_eq!(layout_write("another-room", &topology), None);
        assert_eq!(layout_write(room, b"not a message"), None);
    }
}
