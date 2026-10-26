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
//! - refuses a member's change to the room's topology (`SyncResp`) at every
//!   door it enters by ([`Rooms::decide_layout_write`]): neither kept, nor
//!   forwarded, nor taken in; the room's admission refuses it too
//!   (`RoomRefusal::PersonalLayout`);
//! - replays to nobody what the relay retained of such changes before
//!   ([`crate::store::WorkspaceStore::replay_member_layout`]);
//! - removes what it committed of members' windows while the room shared
//!   its layout, from the room document, when the room becomes personal and
//!   whenever the relay opens it ([`Rooms::keep_personal`]). Each device
//!   keeps its own layout: that lives on the device, not in the room.

use std::collections::HashMap;

use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD};
use tilo_desktop_model::projection::RoomLayout;
use tilo_desktop_model::room_layout;
use tilo_lock::MutexRecover as _;

use super::authority::RoomOp;
use super::{CollaborationError, GovernedRoom, Rooms, SystemTime, system_time};
use crate::host_authority::HostOperation;
use tilo_user_identity::DeviceCert;

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

/// Whether `body`, a member's retained write to `room`, changes the room's
/// own document: its topology, windows and retained commands included. What
/// a door asks before deciding it.
pub(super) fn writes_topology(room: &str, body: &[u8]) -> bool {
    matches!(
        postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body),
        Ok(neighbour_wire::NeighbourMsg::SyncResp { room: named, .. }) if named == room
    )
}

/// Apply every retained command addressed to `salt`'s owner in `doc`, the
/// room's document, then record that it did in the same change. The owner is
/// the host itself here: its operator strips and its own windows. Returns
/// whether `doc` changed.
pub(super) fn apply_requests(
    doc: &mut automerge::AutoCommit,
    room: &str,
    salt: u16,
    person: &str,
) -> Result<bool, String> {
    use tilo_desktop_model::commands::{BoundCommand, apply_command};
    let requests = room_layout::pending(doc, salt);
    if requests.is_empty() {
        return Ok(false);
    }
    let Some(mut layout) = room_layout::read(doc, None) else {
        return Ok(false);
    };
    let mut changed = false;
    for request in requests {
        let Ok(bound) = serde_json::from_slice::<BoundCommand>(&request.command) else {
            continue;
        };
        let Some(workspace) = layout
            .tree
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room().to_string() == room)
        else {
            continue;
        };
        if apply_command(
            workspace,
            &bound.command,
            &mut || Err("a retained command opens nothing".into()),
            &mut |_, _| Err("a retained command opens nothing".into()),
        )
        .is_ok()
        {
            changed = true;
        }
        changed |= room_layout::claim(doc, salt, &request).map_err(|e| e.to_string())?;
    }
    if changed {
        changed |= room_layout::write(doc, &layout, salt, Some(person))
            .map_err(|error| error.to_string())?;
    }
    Ok(changed)
}

impl Rooms {
    /// Author `change`'s edit of `room`'s own document as its exact governor:
    /// its operator strips, retained command marks, or a host-owned window.
    /// The room's `StatePeer` takes the edit in exactly as it takes a
    /// member's `SyncResp`, so a host write has one admission path.
    ///
    /// # Errors
    /// Refuses another governor's certificate, stale custody, an edit that
    /// fails, or a document the room refuses.
    pub async fn author_room_document(
        &self,
        room: &str,
        governor: &DeviceCert,
        change: impl FnOnce(&mut automerge::AutoCommit) -> Result<bool, String>,
    ) -> Result<bool, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            governor,
            None,
            RoomOp::Host(HostOperation::AuthorApplications),
            now,
        )?;
        if governor != &self.inner.host.cert {
            return Err(CollaborationError::new(
                "room document authoring requires exact governor custody",
            ));
        }
        let state = self.room(room).await?;
        let mut governed = state.lock().await;
        if governed.peer().authority() != governor {
            return Err(CollaborationError::new(
                "room document governor differs from current room authority",
            ));
        }
        governed
            .peer()
            .workspace()
            .check_workspace_administration(governor.device_public_key, system_time(now))?;
        let mut candidate = governed.peer().clone();
        let changed = candidate
            .governance_mut()
            .author_room_document(&self.inner.host.key, system_time(now), change)
            .map_err(CollaborationError::new)?;
        if changed {
            let collaboration = governed.collaboration().clone();
            governed.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
                Ok(collaboration)
            })?;
            self.settle(&mut governed).await?;
        }
        Ok(changed)
    }

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
        Ok(())
    }

    /// Decide `author`'s write `body` (a retained message) to `room`'s own
    /// document: in a room whose layout is personal a member's change to its
    /// topology is refused. Anything else, and the relay's own writes, pass
    /// untouched. The room's `StatePeer` decides every accepted change by
    /// the one-writer-per-entry rule; there is no second authority here.
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
        if self.is_public(room) && writes_topology(room, body) {
            return Err(LAYOUT_REFUSAL.into());
        }
        Ok(())
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
        let legacy_layout = app_id == DESKTOP_APP && record_id == DESKTOP_RECORD;
        author == self.inner.host.cert.device_public_key
            || !legacy_layout
            || !self.is_public(room.room())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_door_knows_a_room_document_write_from_any_other() {
        let room = "07848ffe-1898-4f72-8419-0b54c93e6dad";
        let topology = postcard::to_stdvec(&neighbour_wire::NeighbourMsg::SyncResp {
            changes: vec![vec![1]],
            room: room.into(),
        })
        .unwrap();
        assert!(writes_topology(room, &topology));
        assert!(!writes_topology("another-room", &topology));
        assert!(!writes_topology(room, b"not a message"));
    }
}
