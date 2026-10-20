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
//! The governor records the fact in the room's governance
//! ([`tilo_desktop_model::projection::ROOM_PUBLIC_KEY`]), where every
//! member's device reads it, and holds to it itself:
//! - a member's change to the room's topology (`SyncResp`) or to its shared
//!   desktop record is refused at every door it enters by
//!   ([`Rooms::decide_layout_write`]): neither kept, nor forwarded, nor
//!   taken in;
//! - what the relay retained of such changes before is left out of every
//!   replay ([`crate::store::WorkspaceStore::replay_without_member_layout`]);
//! - what the governor committed of them before is removed from the room
//!   document and the shared desktop record when the room opens, or becomes
//!   public ([`Rooms::keep_public`]). Each device keeps its own layout: that
//!   lives on the device, not in the room.
//!
//! Once public, a room stays public: the people who came in through its
//! door are still members after it leaves the directory.

use std::collections::BTreeSet;

#[cfg(feature = "agents")]
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD};
use tilo_lock::MutexRecover as _;

use super::desktop_writes::is_desktop;
use super::{CollaborationError, GovernedRoom, Rooms, SystemTime};

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

/// Remove every window and window group from `doc`, a shared desktop
/// record, and every strip a person made; the strips the room itself has
/// (its operator's, which name no author) stay. Returns whether it changed
/// the record.
#[cfg(any(feature = "agents", test))]
pub(super) fn drop_shared_windows(doc: &mut automerge::AutoCommit) -> Result<bool, String> {
    use automerge::transaction::Transactable as _;
    use automerge::{ObjType, ROOT, ReadDoc as _, Value};
    let mut changed = false;
    for level in ["windows", "groups"] {
        if let Some((Value::Object(ObjType::Map), map)) =
            doc.get(ROOT, level).map_err(|error| error.to_string())?
        {
            let keys: Vec<String> = doc.keys(&map).collect();
            for key in keys {
                doc.delete(&map, key.as_str())
                    .map_err(|error| error.to_string())?;
                changed = true;
            }
        }
    }
    if let Some((Value::Object(ObjType::Map), strips)) =
        doc.get(ROOT, "strips").map_err(|error| error.to_string())?
    {
        let made: Vec<String> = doc
            .keys(&strips)
            .filter(|key| {
                doc.get(&strips, key.as_str())
                    .ok()
                    .flatten()
                    .and_then(|(_, strip)| doc.get(&strip, "author").ok().flatten())
                    .is_some()
            })
            .collect();
        for key in made {
            doc.delete(&strips, key.as_str())
                .map_err(|error| error.to_string())?;
            changed = true;
        }
    }
    Ok(changed)
}

impl Rooms {
    /// Whether `room` is public: it lets people in through a door.
    #[must_use]
    pub fn is_public(&self, room: &str) -> bool {
        self.inner.public.contains(room)
    }

    /// Note each room that is public as the relay opens: its governance
    /// says so, or the directory lists it.
    pub(super) async fn learn_public(&self) {
        let rooms = self.inner.rooms.lock().await.clone();
        for (room, state) in rooms {
            let marked = state.lock().await.peer().governance().room_public();
            if marked || self.listing(&room).is_some() {
                self.inner.public.insert(&room);
            }
        }
    }

    /// Make `room` public: the persistent room a community site lets
    /// visitors into through its door. See [`Self::keep_public`].
    ///
    /// # Errors
    ///
    /// The room is not governed here, or what it holds of its members'
    /// windows could not be removed.
    pub async fn make_public(&self, room: &str) -> Result<(), CollaborationError> {
        self.inner.public.insert(room);
        self.keep_public(room).await
    }

    /// Hold `room`, which is public, to what a public room shares: mark it
    /// public in its governance, stop replaying what members wrote of the
    /// room's layout, and remove what the governor committed of their
    /// windows, from the room document and from the shared desktop record.
    ///
    /// # Errors
    ///
    /// The room is not governed here, or a change could not be committed.
    pub(super) async fn keep_public(&self, room: &str) -> Result<(), CollaborationError> {
        let host = &self.inner.host;
        {
            let state = self.room(room).await?;
            let mut governed = state.lock().await;
            self.bind(&governed)
                .await
                .map_err(CollaborationError::new)?;
            let now = SystemTime::now();
            let mut candidate = governed.peer().clone();
            let marked = candidate
                .governance_mut()
                .author_room_public(&host.key, now)
                .map_err(CollaborationError::new)?;
            let dropped = candidate
                .governance_mut()
                .drop_member_windows(&host.key, now)
                .map_err(CollaborationError::new)?;
            if marked || dropped {
                let collaboration = governed.collaboration().clone();
                governed
                    .commit(candidate, &host.key, now, |_, _| Ok(collaboration))
                    .map_err(CollaborationError::new)?;
                self.settle(&mut governed)
                    .await
                    .map_err(CollaborationError::new)?;
            }
            if dropped {
                tracing::info!(%room, "removed members' windows from a public room's document");
            }
        }
        self.clear_shared_desktop(room).await
    }

    /// Remove what members put on `room`'s shared desktop record
    /// ([`drop_shared_windows`]), as its governor.
    ///
    /// # Errors
    ///
    /// The record could not be read or the change committed.
    #[cfg(feature = "agents")]
    async fn clear_shared_desktop(&self, room: &str) -> Result<(), CollaborationError> {
        let cleared = self
            .author_room_record(
                room,
                &self.inner.host.cert,
                (DESKTOP_APP, DESKTOP_RECORD),
                drop_shared_windows,
            )
            .await?;
        if cleared {
            tracing::info!(%room, "removed members' windows from a public room's shared desktop");
        }
        Ok(())
    }

    /// Without the browser plane's record authoring this relay keeps no
    /// shared desktop record a member's page wrote: nothing to remove.
    #[cfg(not(feature = "agents"))]
    #[expect(
        clippy::unused_async,
        reason = "one signature with the build that authors room records"
    )]
    async fn clear_shared_desktop(&self, room: &str) -> Result<(), CollaborationError> {
        let _ = room;
        Ok(())
    }

    /// Hold every public room to what a public room shares as the relay
    /// opens ([`Self::keep_public`]): what was committed before public rooms
    /// refused their members' windows goes. A room that cannot be held is
    /// reported and left to the next start; it does not keep the others,
    /// or the relay, from opening.
    pub(super) async fn keep_public_rooms(&self) {
        let rooms: Vec<String> = self.inner.public.0.lock_recover().iter().cloned().collect();
        for room in rooms {
            if let Err(error) = self.keep_public(&room).await {
                tracing::warn!(%room, %error, "a public room still holds its members' windows");
            }
        }
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
            return Err(match write {
                LayoutWrite::Topology => "a public room shares no member's windows or layout",
                LayoutWrite::Desktop => "a public room shares no member's desktop",
            }
            .into());
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
    fn a_public_rooms_shared_desktop_keeps_only_the_rooms_own_strips() {
        use automerge::transaction::Transactable as _;
        use automerge::{ObjType, ROOT, ReadDoc as _};
        let mut doc = automerge::AutoCommit::new();
        let strips = doc.put_object(ROOT, "strips", ObjType::Map).unwrap();
        let operator = doc.put_object(&strips, "1", ObjType::Map).unwrap();
        doc.put(&operator, "name", "Lobby").unwrap();
        let made = doc.put_object(&strips, "2", ObjType::Map).unwrap();
        doc.put(&made, "name", "Mine").unwrap();
        doc.put(&made, "author", "ab".repeat(32)).unwrap();
        let groups = doc.put_object(ROOT, "groups", ObjType::Map).unwrap();
        doc.put_object(&groups, "3", ObjType::Map).unwrap();
        let windows = doc.put_object(ROOT, "windows", ObjType::Map).unwrap();
        let window = doc.put_object(&windows, "4", ObjType::Map).unwrap();
        doc.put(&window, "tab", "{\"title\":\"Alacritty\"}")
            .unwrap();
        doc.commit();

        assert!(drop_shared_windows(&mut doc).unwrap());
        assert_eq!(doc.keys(&strips).collect::<Vec<_>>(), ["1"]);
        assert_eq!(doc.keys(&groups).count(), 0);
        assert_eq!(doc.keys(&windows).count(), 0);
        assert!(!drop_shared_windows(&mut doc).unwrap(), "nothing left");
    }

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
