//! The person's own workspaces, held alike by every one of their devices.
//!
//! A workspace's content (its strips, window groups and tabs, and the
//! records those tabs show) lives in its room document and replicates to
//! every member of the room; the person's own devices are members by
//! ownership, so they need no invitation. What a device additionally needs
//! is the *list*: which rooms are this person's workspaces, so that one
//! created on the laptop appears on the board as well. That list is the
//! person's workspace directory ([`WorkspaceDirectory`]), a small CRDT every
//! device of the person holds and only those devices can write: updates are
//! device-signed and checked against the person's root before they merge.
//!
//! The whole-desktop document stays on the device. Besides the workspaces it
//! records what this device is looking at (the active workspace, which group
//! each display shows, focus pointers) and rooms that belong to other
//! people, none of which another device of the person should take over.
//!
//! Reconciliation is a three-way merge against `agreed`, the list as both
//! sides last had it: a workspace that appeared or was renamed here since
//! then is written to the directory, and one that appeared or was renamed in
//! the directory is brought into this device's tree. Nothing deletes a
//! workspace yet, so the list only grows.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_user_identity::{
    DeviceCert, DirectoryError, DirectoryUpdate, RevocationList, WorkspaceDirectory,
};

use crate::shell::state::{RoomId, UserId, WorkspaceTree};

/// The person's workspace list as this device holds and last agreed it.
pub struct PersonalWorkspaces {
    directory: WorkspaceDirectory,
    /// The list as of the last reconciliation, room → name.
    agreed: BTreeMap<RoomId, String>,
    /// Where the directory and `agreed` survive a restart; `None` keeps
    /// them in memory only.
    store: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    directory: Vec<u8>,
    agreed: BTreeMap<String, String>,
}

/// The rooms of `tree` that are `person`'s own workspaces, with their names.
/// A room governed by a durable host has its own enrollment and a room only
/// offered by another device's continuation is not a workspace yet, so
/// neither is listed.
pub fn owned_workspaces(tree: &WorkspaceTree, person: &str) -> BTreeMap<RoomId, String> {
    tree.desktop
        .workspaces
        .iter()
        .filter(|workspace| {
            !workspace.room.is_nil()
                && workspace.owner.as_deref() == Some(person)
                && !tree.requires_native_owner(workspace.room)
                && !tree.continuation_rooms.contains(&workspace.room)
        })
        .map(|workspace| (workspace.room, workspace.name.clone()))
        .collect()
}

/// The directory's stable id for `room`: the room's collaboration topic.
fn entry_id(room: RoomId) -> u64 {
    crate::shell::network::room_topic_id(room)
}

impl PersonalWorkspaces {
    /// An empty list for `user`'s device `device`, kept in memory only.
    pub fn new(user: [u8; 32], device: [u8; 32]) -> Self {
        Self {
            directory: WorkspaceDirectory::new(user, device),
            agreed: BTreeMap::new(),
            store: None,
        }
    }

    /// The list `store` holds for `user`'s device `device`, or an empty one
    /// that will be written there. An unreadable file starts empty: the
    /// first reconciliation lists this device's workspaces again and the
    /// other devices' snapshots bring back the rest.
    pub fn open(user: [u8; 32], device: [u8; 32], store: PathBuf) -> Self {
        let loaded = std::fs::read(&store)
            .ok()
            .and_then(|bytes| postcard::from_bytes::<Stored>(&bytes).ok())
            .and_then(|stored| {
                let directory = WorkspaceDirectory::load(user, device, &stored.directory).ok()?;
                let agreed = stored
                    .agreed
                    .into_iter()
                    .filter_map(|(room, name)| Some((RoomId::parse(&room)?, name)))
                    .collect();
                Some((directory, agreed))
            });
        let (directory, agreed) =
            loaded.unwrap_or_else(|| (WorkspaceDirectory::new(user, device), BTreeMap::new()));
        Self {
            directory,
            agreed,
            store: Some(store),
        }
    }

    /// Write what changed in `tree` since the last agreement into the
    /// directory. Returns whether the directory changed, which is when the
    /// other devices need a new snapshot.
    pub fn publish_local(&mut self, tree: &WorkspaceTree, person: &str) -> bool {
        let owned = owned_workspaces(tree, person);
        let mut changed = false;
        for (room, name) in &owned {
            if self.agreed.get(room) == Some(name) {
                continue;
            }
            match self
                .directory
                .add_room_workspace(entry_id(*room), &room.to_string(), name)
            {
                Ok(()) => {
                    self.agreed.insert(*room, name.clone());
                    changed = true;
                }
                Err(error) => {
                    tracing::warn!(%error, %room, "personal workspace could not be listed");
                }
            }
        }
        if changed {
            self.persist();
        }
        changed
    }

    /// Bring what changed in the directory since the last agreement into
    /// `tree`: join each new workspace's room as `person`'s own, and take a
    /// new name. Returns whether `tree` changed.
    pub fn adopt_into(&mut self, tree: &mut WorkspaceTree, person: &UserId) -> bool {
        let listed: BTreeMap<RoomId, String> = self
            .directory
            .list_workspaces()
            .into_iter()
            .filter_map(|entry| {
                let room = RoomId::parse(entry.room.as_deref()?)?;
                (!room.is_nil() && entry_id(room) == entry.id).then_some((room, entry.name))
            })
            .collect();
        let mut changed = false;
        for (room, name) in listed {
            if self.agreed.get(&room) == Some(&name) {
                continue;
            }
            changed |= tree.adopt_personal_room(room, &name, person);
            self.agreed.insert(room, name);
        }
        self.persist();
        changed
    }

    /// Merge another device's signed snapshot. `author` is that device's
    /// certificate, as the transport authenticated it; the directory checks
    /// it belongs to this person and signed the update.
    ///
    /// # Errors
    ///
    /// The directory's reason for refusing the update.
    pub fn merge(
        &mut self,
        update: &DirectoryUpdate,
        author: &DeviceCert,
        now_ns: u64,
    ) -> Result<(), DirectoryError> {
        self.directory
            .apply_incremental(update, author, &RevocationList::new(), now_ns)?;
        self.persist();
        Ok(())
    }

    /// Whether `update` already holds everything this device's directory
    /// does, so answering it with a snapshot would tell its author nothing.
    pub fn covered_by(&mut self, update: &DirectoryUpdate) -> bool {
        self.directory.covered_by(&update.delta)
    }

    /// This device's whole directory, signed for the other devices.
    pub fn snapshot(
        &mut self,
        device_key: &ed25519_dalek::SigningKey,
        now_ns: u64,
    ) -> Option<DirectoryUpdate> {
        self.directory.signed_snapshot(device_key, now_ns).ok()
    }

    fn persist(&mut self) {
        let Some(path) = self.store.clone() else {
            return;
        };
        let stored = Stored {
            directory: self.directory.save(),
            agreed: self
                .agreed
                .iter()
                .map(|(room, name)| (room.to_string(), name.clone()))
                .collect(),
        };
        let Ok(bytes) = postcard::to_allocvec(&stored) else {
            return;
        };
        write_if_changed(&path, &bytes);
    }
}

fn write_if_changed(path: &Path, bytes: &[u8]) {
    if std::fs::read(path).is_ok_and(|current| current == bytes) {
        return;
    }
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    if let Err(error) = tilo_files::native::atomic::write_with_mode(path, bytes, 0o600) {
        tracing::warn!(%error, path = %path.display(), "personal workspaces could not be saved");
    }
}

#[cfg(test)]
#[path = "personal_workspaces_tests.rs"]
mod tests;
