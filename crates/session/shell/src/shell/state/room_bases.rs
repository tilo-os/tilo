//! A room's document starts over from a base whenever the room admits
//! someone new ([`tilo_peer_state::room::base`]), so a later member is given
//! the room as it stands and never what was withdrawn before they joined.
//! The documents a base replaced stay archived here, for this person's own
//! devices alone.

use automerge::AutoCommit;

use super::{PeerOrigin, RoomAuthority, RoomId, WorkspaceTree};
use tilo_peer_state::room::base;

impl WorkspaceTree {
    /// Someone new to `room` asks for it. When this machine governs the room
    /// it answers them, and if the document holds anything past its base,
    /// from a new base. Returns whether it may answer.
    pub fn admit_room_participant(&mut self, room: RoomId) -> bool {
        if self.workspace_in_room(room).is_none() {
            return true;
        }
        if !self.governs_room(room) {
            return false;
        }
        if !self.try_sync_room_doc(room) {
            return true;
        }
        let Some(doc) = self.room_docs.get_mut(&room) else {
            return true;
        };
        let heads = doc.get_heads();
        if heads.is_empty() || base::root(doc).is_some_and(|root| heads == [root]) {
            return true;
        }
        match base::rebase(doc, automerge::ActorId::random()) {
            Ok(rebased) => {
                let replaced = std::mem::replace(doc, rebased);
                self.archive_room_doc(room, replaced);
                true
            }
            Err(error) => {
                tracing::warn!(target: "shell::state", %room, %error, "the room's document did not rebase");
                false
            }
        }
    }

    pub(super) fn archive_room_doc(&mut self, room: RoomId, mut replaced: AutoCommit) {
        let saved = replaced.save();
        let archive = self.room_archives.entry(room).or_default();
        if !archive.contains(&saved) {
            archive.push(saved);
        }
    }

    /// The documents of `room` that bases replaced, oldest first, each saved
    /// whole.
    #[must_use]
    pub fn room_archive(&self, room: RoomId) -> &[Vec<u8>] {
        self.room_archives.get(&room).map_or(&[], Vec::as_slice)
    }

    /// Keep `documents`, `room`'s archive as another device of this person
    /// sent it; the caller authenticated that device. Returns whether any
    /// was new.
    pub fn keep_room_archive(&mut self, room: RoomId, documents: &[Vec<u8>]) -> bool {
        if self.workspace_in_room(room).is_none() {
            return false;
        }
        let archive = self.room_archives.entry(room).or_default();
        let mut kept = false;
        for document in documents {
            if !archive.contains(document) && AutoCommit::load(document).is_ok() {
                archive.push(document.clone());
                kept = true;
            }
        }
        kept
    }

    /// Whether the transport authenticated `from` as another device of the
    /// person this device belongs to.
    #[must_use]
    pub fn is_own_device(&self, from: &PeerOrigin) -> bool {
        self.local_person
            .is_some_and(|local| self.authenticated_persons.get(from) == Some(&local))
    }

    /// Whether `from` writes `room`'s governance: a member answers it
    /// whatever it asks, since it holds the room already.
    #[must_use]
    pub fn is_room_authority(&self, room: RoomId, from: &PeerOrigin) -> bool {
        match self.room_authority(room) {
            RoomAuthority::Peer(pinned) => pinned == *from,
            RoomAuthority::Person(person) => self.authenticated_persons.get(from) == Some(&person),
            RoomAuthority::Local | RoomAuthority::Unpinned => false,
        }
    }
}
