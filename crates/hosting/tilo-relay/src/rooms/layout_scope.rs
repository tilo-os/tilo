//! Whether a room shares its members' layout is its governance's word
//! ([`RoomLayout`]), and the governor holds the room to it.
//!
//! In a room whose layout is personal each member's strips and windows stay
//! on their device: which apps someone has open, and their titles, are the
//! person's own. Only the room's records (Chat, Notes, Canvas documents)
//! and its people are shared. A room admitted through a public door is
//! personal (the persistent room a community site runs, settled at every
//! start), and so is one with open admission (listed in the public
//! directory) unless its governor chose otherwise; an invite-only room
//! shares its layout.
//!
//! The governor writes the scope into the room's governance
//! ([`tilo_desktop_model::projection::ROOM_LAYOUT_KEY`]), where every
//! member's device reads it, and in a personal room:
//! - refuses a member's change to the room's topology (`SyncResp`) or to its
//!   layout document at every door it enters by
//!   ([`Rooms::judge_layout_write`]): neither kept, nor forwarded, nor
//!   taken in; the room's admission refuses it too
//!   (`RoomRefusal::PersonalLayout`);
//! - replays to nobody what the relay retained of such changes before
//!   ([`crate::store::WorkspaceStore::replay_member_layout`]);
//! - removes what it committed of members' windows while the room shared
//!   its layout, from the room document and the layout document, when
//!   the room becomes personal and whenever the relay opens it
//!   ([`Rooms::keep_personal`]). Each device keeps its own layout: that
//!   lives on the device, not in the room.

use std::collections::HashMap;

#[cfg(feature = "agents")]
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD};
use tilo_desktop_model::projection::RoomLayout;
use tilo_lock::MutexRecover as _;

use super::layout_writes::is_layout;
use super::{CollaborationError, GovernedRoom, Rooms, SystemTime};

/// Why a room whose layout is personal refuses a member's change to it. A
/// device from before personal rooms sends its windows again every few
/// seconds, so the refusal is not worth a warning each time.
pub(crate) const LAYOUT_REFUSAL: &str = tilo_relay_protocol::refusal::PERSONAL_LAYOUT;

/// Each governed room's layout scope, by room UUID: what its governance
/// says, read when the relay opens it and written with it
/// ([`Rooms::set_layout`], the only writer of a room's scope here). Kept
/// apart from the room's lock because every door decides a member's write
/// by it.
#[derive(Default)]
pub(super) struct Layouts(std::sync::Mutex<HashMap<String, RoomLayout>>);

impl Layouts {
    fn get(&self, room: &str) -> RoomLayout {
        self.0.lock_recover().get(room).copied().unwrap_or_default()
    }

    fn set(&self, room: &str, layout: RoomLayout) {
        self.0.lock_recover().insert(room.to_owned(), layout);
    }

    fn personal(&self) -> Vec<String> {
        self.0
            .lock_recover()
            .iter()
            .filter(|(_, layout)| layout.is_personal())
            .map(|(room, _)| room.clone())
            .collect()
    }
}

/// What a member's retained write `body` does to a room's shared layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LayoutWrite {
    /// Changes the room document of `room`: its topology, windows included.
    Topology,
    /// Writes the room's layout document on its record route.
    Document,
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
        } if is_layout(&app_id, &record_id) => Some(LayoutWrite::Document),
        _ => None,
    }
}

/// Remove every window and window group from `doc`, a room's layout
/// document, and every strip a person made; the strips the room itself has
/// (its operator's, which name no author) stay. Returns whether it changed
/// the document.
#[cfg(any(feature = "agents", test))]
pub(super) fn drop_shared_windows(doc: &mut automerge::AutoCommit) -> Result<bool, String> {
    use tilo_desktop_model::projection::drop_contents_except;
    let Some(layout) = tilo_desktop_model::room_layout::read(doc, None) else {
        return Ok(false);
    };
    let kept: std::collections::BTreeSet<u64> = layout
        .tree
        .workspaces
        .first()
        .into_iter()
        .flat_map(|workspace| workspace.strips.iter())
        .map(|strip| strip.id.raw())
        .filter(|id| !layout.authors.contains_key(id))
        .collect();
    // Every entry that is not a strip the room itself made goes, with what it
    // holds; the room's strips stay, emptied.
    Ok(drop_contents_except(doc, &|id| kept.contains(&id)) > 0)
}

impl Rooms {
    /// `room`'s layout scope, as its governance says.
    #[must_use]
    pub fn layout(&self, room: &str) -> RoomLayout {
        self.inner.layouts.get(room)
    }

    /// Whether `room` takes in none of its members' layout, as a room
    /// behind a public door does: its layout is personal.
    #[must_use]
    pub fn is_public(&self, room: &str) -> bool {
        self.layout(room).is_personal()
    }

    /// Read each room's layout scope from its governance as the relay
    /// opens, before any door decides a write by it.
    pub(super) async fn learn_layouts(&self) {
        let rooms = self.inner.rooms.lock().await.clone();
        for (room, state) in rooms {
            let layout = state.lock().await.peer().governance().room_layout();
            self.inner.layouts.set(&room, layout);
        }
    }

    /// Settle every room's layout as the relay opens: a room listed in the
    /// directory whose governor chose no scope becomes personal (open
    /// admission), and each personal room loses what was committed of its
    /// members' windows before it was ([`Self::keep_personal`]). A room
    /// that cannot be settled is reported and left to the next start; it
    /// does not keep the others, or the relay, from opening.
    pub(super) async fn settle_layouts(&self) {
        let rooms: Vec<String> = self.inner.rooms.lock().await.keys().cloned().collect();
        for room in rooms {
            if self.listing(&room).is_some()
                && let Err(error) = self.default_to_personal(&room).await
            {
                tracing::warn!(%room, %error, "a listed room still shares its layout");
            }
        }
        for room in self.inner.layouts.personal() {
            if let Err(error) = self.keep_personal(&room).await {
                tracing::warn!(%room, %error, "a personal room still holds its members' windows");
            }
        }
    }

    /// Make `room`, which just opened a door (a public listing), personal
    /// unless its governor already chose its scope.
    pub(super) async fn default_to_personal(&self, room: &str) -> Result<(), CollaborationError> {
        let chosen = {
            let state = self.room(room).await?;
            let governed = state.lock().await;
            governed.peer().governance().room_layout_choice()
        };
        if chosen.is_some() {
            return Ok(());
        }
        self.set_layout(room, RoomLayout::Personal).await
    }

    /// Set `room`'s layout scope to `layout`, as its governor: written into
    /// its governance for every member's device to read, and held to here.
    /// Making it personal removes what the room holds of its members'
    /// windows (`Self::keep_personal`); what could not be removed now is
    /// reported, and removed when the relay next opens the room.
    ///
    /// A room made personal is held to it here at once, even when its
    /// governance cannot be written: the relay never shares more than the
    /// room was asked to.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its decision is committed and settled; whether the lint sees it depends on the features built"
    )]
    pub async fn set_layout(
        &self,
        room: &str,
        layout: RoomLayout,
    ) -> Result<(), CollaborationError> {
        let host = &self.inner.host;
        if layout.is_personal() {
            self.inner.layouts.set(room, layout);
        }
        {
            let state = self.room(room).await?;
            let mut governed = state.lock().await;
            let now = SystemTime::now();
            let mut candidate = governed.peer().clone();
            let changed = candidate
                .governance_mut()
                .author_room_layout(layout, &host.key, now)
                .map_err(CollaborationError::new)?;
            if changed {
                governed
                    .commit(candidate, &host.key, now, |record, _| {
                        Ok(record.collaboration.clone())
                    })
                    .map_err(CollaborationError::new)?;
                tracing::info!(%room, layout = layout.id(), "room layout scope set");
            }
            self.inner.layouts.set(room, layout);
            self.bind(&governed)
                .await
                .map_err(CollaborationError::new)?;
            if changed {
                self.settle(&mut governed)
                    .await
                    .map_err(CollaborationError::new)?;
            }
        }
        if layout.is_personal()
            && let Err(error) = self.keep_personal(room).await
        {
            tracing::warn!(%room, %error, "a personal room still holds its members' windows");
        }
        Ok(())
    }

    /// Remove from `room`, whose layout is personal, what its governor
    /// committed of its members' windows while it shared them: from the
    /// room document (each entry in a member's topology namespace) and from
    /// the layout document (every window, and the strips people made).
    /// What the room itself holds stays. Changes nothing a second time.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its decision is committed and settled; whether the lint sees it depends on the features built"
    )]
    pub(super) async fn keep_personal(&self, room: &str) -> Result<(), CollaborationError> {
        let host = &self.inner.host;
        {
            let state = self.room(room).await?;
            let mut governed = state.lock().await;
            let now = SystemTime::now();
            let mut candidate = governed.peer().clone();
            let dropped = candidate
                .governance_mut()
                .drop_member_windows(&host.key, now)
                .map_err(CollaborationError::new)?;
            if dropped {
                governed
                    .commit(candidate, &host.key, now, |record, _| {
                        Ok(record.collaboration.clone())
                    })
                    .map_err(CollaborationError::new)?;
                self.settle(&mut governed)
                    .await
                    .map_err(CollaborationError::new)?;
                tracing::info!(%room, "removed members' windows from a personal room's document");
            }
        }
        self.clear_shared_desktop(room).await
    }

    /// Remove what members put on `room`'s layout document
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
            tracing::info!(%room, "removed members' windows from a personal room's layout");
        }
        Ok(())
    }

    /// Without the browser plane's record authoring this relay keeps no
    /// layout document a member's page wrote: nothing to remove.
    #[cfg(not(feature = "agents"))]
    #[expect(
        clippy::unused_async,
        reason = "one signature with the build that authors room records"
    )]
    async fn clear_shared_desktop(&self, room: &str) -> Result<(), CollaborationError> {
        let _ = room;
        Ok(())
    }

    /// Decide `author`'s write `body` (a retained message) to `room`'s
    /// shared layout: in a room whose layout is personal a member's change
    /// to the room's topology or layout is refused, and a layout
    /// write anywhere is decided by what it does to what others made
    /// ([`Self::judge_layout_write`]). Anything else, and the relay's own
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
            return Err(LAYOUT_REFUSAL.into());
        }
        match write {
            LayoutWrite::Topology => Ok(()),
            LayoutWrite::Document => self.judge_layout_write(room, author, body).await,
        }
    }

    /// Whether the governor of `room` takes in `author`'s retained write
    /// `message` when it replays what the relay retains: never a member's
    /// layout write in a room whose layout is personal.
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
            || !is_layout(app_id, record_id)
            || !self.is_public(room.room())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_personal_rooms_shared_desktop_keeps_only_the_rooms_own_strips() {
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
