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
//! the directory is brought into this device's tree. The same holds for a
//! workspace deleted on one device, for the order of the list, and for the
//! devices a workspace shows on (every device, unless the person keeps it
//! off some).
//!
//! Membership belongs to the person, not to a device, so the list holds
//! every room the person is in: the rooms they own, which any of their
//! devices governs, and rooms someone else invited them to, listed with the
//! peer that governs them so another of their devices pins that same
//! governor instead of trusting whichever device answers first.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_user_identity::{
    DeviceCert, DirectoryError, DirectoryUpdate, RevocationList, WorkspaceDirectory, WorkspaceMeta,
};

use crate::shell::state::{PeerOrigin, RoomAuthority, RoomId, UserId, WorkspaceTree};

/// One workspace as the person's list holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listed {
    pub name: String,
    /// The peer governing a room someone else owns, as an origin
    /// (`iroh:<endpoint>`); `None` for a room the person owns.
    pub governor: Option<String>,
}

/// The person's workspace list as this device holds and last agreed it.
pub struct PersonalWorkspaces {
    directory: WorkspaceDirectory,
    /// This device, as the directory's member lists name it.
    device: [u8; 32],
    /// This device's name for the person's other devices.
    label: Option<String>,
    /// The workspaces this device held at the last reconciliation.
    agreed: BTreeMap<RoomId, Listed>,
    /// Their order at the last reconciliation.
    agreed_order: Vec<RoomId>,
    /// Where the directory and `agreed` survive a restart; `None` keeps
    /// them in memory only.
    store: Option<PathBuf>,
    /// What the tree offered the last time [`Self::publish_local`] ran to
    /// the end with everything written. While the tree offers the same and
    /// neither the directory nor `agreed` moved since, running it again
    /// would write nothing, so it is skipped.
    published: Option<PublishInputs>,
    /// How often [`Self::publish_local`] compared the tree against the
    /// directory instead of answering from `published`.
    #[cfg(test)]
    publish_passes: usize,
}

/// The parts of a tree [`PersonalWorkspaces::publish_local`] reads.
#[derive(PartialEq, Eq)]
struct PublishInputs {
    listed: BTreeMap<RoomId, Listed>,
    /// Every workspace room of the tree, in its order.
    order: Vec<RoomId>,
    /// The listed rooms still waiting for their governor to be pinned.
    unpinned: Vec<RoomId>,
}

impl PublishInputs {
    fn of(tree: &WorkspaceTree, person: &str) -> Self {
        let listed = member_workspaces(tree, person);
        let unpinned = listed
            .keys()
            .copied()
            .filter(|room| tree.room_authority(*room) == RoomAuthority::Unpinned)
            .collect();
        Self {
            listed,
            order: tree.desktop.workspaces.iter().map(|w| w.room).collect(),
            unpinned,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    directory: Vec<u8>,
    agreed: BTreeMap<String, Listed>,
    #[serde(default)]
    agreed_order: Vec<String>,
}

/// One of the person's devices as the workspace's menu offers it: whether
/// the workspace shows there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceChoice {
    pub device: [u8; 32],
    pub label: String,
    pub shown: bool,
    /// This device.
    pub here: bool,
}

/// The rooms of `tree` that `person` is in, with their names and who
/// governs them: rooms the person owns, and rooms where they hold a live
/// membership under a governor this device pinned. A room governed by a
/// durable host has its own enrollment and a room only offered by another
/// device's continuation is not a workspace yet, so neither is listed; nor
/// is someone else's room whose governor this device does not know.
pub fn member_workspaces(tree: &WorkspaceTree, person: &str) -> BTreeMap<RoomId, Listed> {
    let person = person.to_string();
    tree.desktop
        .workspaces
        .iter()
        .filter(|workspace| {
            !workspace.room.is_nil()
                && !tree.requires_native_owner(workspace.room)
                && !tree.continuation_rooms.contains(&workspace.room)
        })
        .filter_map(|workspace| {
            let governor = if workspace.owner().map(String::as_str) == Some(person.as_str()) {
                None
            } else {
                let member = workspace.role_of(&person).is_some();
                match tree.room_authority(workspace.room) {
                    RoomAuthority::Peer(governor) if member => Some(governor.as_str().to_string()),
                    _ => return None,
                }
            };
            Some((
                workspace.room,
                Listed {
                    name: workspace.name.clone(),
                    governor,
                },
            ))
        })
        .collect()
}

/// The governing peer an entry names, when it names a real endpoint.
fn governor_origin(governor: &str) -> Option<PeerOrigin> {
    let endpoint = governor.strip_prefix("iroh:")?;
    endpoint.parse::<iroh::EndpointId>().ok()?;
    Some(PeerOrigin::peer_id(endpoint))
}

/// The directory's stable id for `room`: the room's collaboration topic.
fn entry_id(room: RoomId) -> u64 {
    crate::shell::network::room_topic_id(room)
}

/// The directory's entries that carry a valid room, in the person's order.
fn entries_of(directory: &WorkspaceDirectory) -> Vec<(RoomId, WorkspaceMeta)> {
    let mut out: Vec<(RoomId, WorkspaceMeta)> = directory
        .list_workspaces()
        .into_iter()
        .filter_map(|entry| {
            let room = RoomId::parse(entry.room.as_deref()?)?;
            (!room.is_nil() && entry_id(room) == entry.id).then_some((room, entry))
        })
        .collect();
    out.sort_by(|(left_room, left), (right_room, right)| {
        let key = |entry: &WorkspaceMeta| entry.order.unwrap_or(f64::INFINITY);
        key(left)
            .total_cmp(&key(right))
            .then_with(|| left_room.cmp(right_room))
    });
    out
}

/// Whether `entry` shows on `device`: every device shows it unless it
/// opted out, so a device linked later shows every workspace.
fn shows_on(entry: &WorkspaceMeta, device: &[u8; 32]) -> bool {
    !entry.kept_off_devices.contains(device)
}

impl PersonalWorkspaces {
    /// An empty list for `user`'s device `device`, kept in memory only.
    pub fn new(user: [u8; 32], device: [u8; 32]) -> Self {
        Self {
            directory: WorkspaceDirectory::new(user, device),
            device,
            label: None,
            agreed: BTreeMap::new(),
            agreed_order: Vec::new(),
            store: None,
            published: None,
            #[cfg(test)]
            publish_passes: 0,
        }
    }

    /// The list `store` holds for `user`'s device `device`, or an empty one
    /// that will be written there. An unreadable file starts empty: the
    /// first reconciliation lists this device's workspaces again and the
    /// other devices' snapshots bring back the rest.
    pub fn open(user: [u8; 32], device: [u8; 32], store: PathBuf) -> Self {
        let mut list = Self::new(user, device);
        if let Some(stored) = std::fs::read(&store)
            .ok()
            .and_then(|bytes| postcard::from_bytes::<Stored>(&bytes).ok())
            && let Ok(directory) = WorkspaceDirectory::load(user, device, &stored.directory)
        {
            list.directory = directory;
            list.agreed = stored
                .agreed
                .into_iter()
                .filter_map(|(room, listed)| Some((RoomId::parse(&room)?, listed)))
                .collect();
            list.agreed_order = stored
                .agreed_order
                .iter()
                .filter_map(|room| RoomId::parse(room))
                .collect();
        }
        list.store = Some(store);
        list
    }

    /// Name this device for the person's other devices.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self.published = None;
        self
    }

    /// Write what changed in `tree` since the last agreement into the
    /// directory: new and renamed workspaces, ones deleted here, and a new
    /// order. Returns whether the directory changed, which is when the
    /// other devices need a new snapshot.
    ///
    /// Called every frame tick, so when neither the tree's workspaces nor the
    /// directory changed since the last call it only compares the tree with
    /// what it offered then.
    pub fn publish_local(&mut self, tree: &WorkspaceTree, person: &str) -> bool {
        let inputs = PublishInputs::of(tree, person);
        if self.published.as_ref() == Some(&inputs) {
            return false;
        }
        #[cfg(test)]
        {
            self.publish_passes = self.publish_passes.saturating_add(1);
        }
        let listed = &inputs.listed;
        let mut complete = true;
        let mut changed = false;
        if let Some(label) = self.label.clone() {
            let registered = self
                .directory
                .devices()
                .iter()
                .any(|(device, known)| *device == self.device && *known == label);
            if !registered {
                if self.directory.register_device(self.device, &label).is_ok() {
                    changed = true;
                } else {
                    complete = false;
                }
            }
        }
        let next_order = entries_of(&self.directory)
            .iter()
            .filter_map(|(_, entry)| entry.order)
            .fold(0.0_f64, f64::max);
        let mut appended = 0.0;
        for (room, entry) in listed {
            let fresh = !self.agreed.contains_key(room);
            // Enrollment gives us a room ID before its real name arrives.
            // Do not publish that provisional name as a rename on the owner's
            // other devices. An adopted directory entry already has an agreed
            // baseline, so its later local renames can still be published.
            if fresh && tree.room_authority(*room) == RoomAuthority::Unpinned {
                continue;
            }
            if self.agreed.get(room) == Some(entry) {
                continue;
            }
            let id = entry_id(*room);
            let written = self
                .directory
                .add_room_workspace(id, &room.to_string(), &entry.name)
                .and_then(|()| self.directory.set_governor(id, entry.governor.as_deref()))
                .and_then(|()| {
                    if fresh {
                        appended += 1.0;
                        self.directory.set_order(id, next_order + appended)
                    } else {
                        Ok(())
                    }
                });
            match written {
                Ok(()) => {
                    self.agreed.insert(*room, entry.clone());
                    changed = true;
                }
                Err(error) => {
                    complete = false;
                    tracing::warn!(%error, %room, "personal workspace could not be listed");
                }
            }
        }
        // A workspace held at the last agreement and gone from this tree was
        // deleted (or left) here.
        let present: std::collections::BTreeSet<RoomId> =
            tree.desktop.workspaces.iter().map(|w| w.room).collect();
        let deleted: Vec<RoomId> = self
            .agreed
            .keys()
            .filter(|room| !present.contains(room))
            .copied()
            .collect();
        for room in deleted {
            let _ = self.directory.remove_workspace(entry_id(room));
            self.agreed.remove(&room);
            changed = true;
        }
        changed |= self.publish_order(tree);
        if changed {
            self.persist();
        }
        // A write that failed is tried again on the next call.
        self.published = complete.then_some(inputs);
        changed
    }

    /// Give the workspaces this tree holds positions in the directory that
    /// match their order here, when that order moved since the last
    /// agreement. Only entries out of place get a new position, so a
    /// concurrent move of another workspace on another device still lands.
    fn publish_order(&mut self, tree: &WorkspaceTree) -> bool {
        let here: Vec<RoomId> = tree
            .desktop
            .workspaces
            .iter()
            .map(|w| w.room)
            .filter(|room| self.agreed.contains_key(room))
            .collect();
        let agreed: Vec<RoomId> = self
            .agreed_order
            .iter()
            .filter(|room| self.agreed.contains_key(room))
            .copied()
            .collect();
        if here == agreed {
            self.agreed_order = here;
            return false;
        }
        let positions: BTreeMap<RoomId, f64> = entries_of(&self.directory)
            .into_iter()
            .filter_map(|(room, entry)| Some((room, entry.order?)))
            .collect();
        let mut changed = false;
        let mut previous = f64::NEG_INFINITY;
        for (index, room) in here.iter().enumerate() {
            if let Some(current) = positions.get(room).copied()
                && current > previous
            {
                previous = current;
                continue;
            }
            let next = here
                .get(index.saturating_add(1)..)
                .unwrap_or(&[])
                .iter()
                .filter_map(|later| positions.get(later).copied())
                .find(|later| *later > previous);
            let placed = match (previous.is_finite(), next) {
                (true, Some(next)) => f64::midpoint(previous, next),
                (true, None) => previous + 1.0,
                (false, Some(next)) => next - 1.0,
                (false, None) => 0.0,
            };
            if self.directory.set_order(entry_id(*room), placed).is_ok() {
                changed = true;
            }
            previous = placed;
        }
        self.agreed_order = here;
        changed
    }

    /// Bring what changed in the directory since the last agreement into
    /// `tree`: join each new workspace's room as `person`'s, take new names
    /// and the person's order, and drop workspaces deleted on another device
    /// or no longer shown on this one. Returns whether `tree` changed.
    pub fn adopt_into(&mut self, tree: &mut WorkspaceTree, person: &UserId) -> bool {
        self.published = None;
        let entries = entries_of(&self.directory);
        let mut changed = false;
        let mut shown_here: Vec<RoomId> = Vec::new();
        for (room, meta) in &entries {
            if !shows_on(meta, &self.device) {
                if self.agreed.remove(room).is_some() {
                    changed |= tree.forget_personal_room(*room).is_some();
                }
                continue;
            }
            shown_here.push(*room);
            let entry = Listed {
                name: meta.name.clone(),
                governor: meta.governor.clone(),
            };
            // An agreed room still unpinned — adopted before rooms pinned their
            // governor from this list — is adopted again so the list pins it.
            if self.agreed.get(room) == Some(&entry)
                && tree.room_authority(*room) != RoomAuthority::Unpinned
            {
                continue;
            }
            let governor = match entry.governor.as_deref() {
                None => None,
                Some(governor) => match governor_origin(governor) {
                    Some(origin) => Some(origin),
                    // An entry naming no usable governor is skipped: a
                    // device must never guess who governs someone's room.
                    None => continue,
                },
            };
            changed |= tree.adopt_personal_room(*room, &entry.name, person, governor.as_ref());
            self.agreed.insert(*room, entry);
        }
        let listed_rooms: std::collections::BTreeSet<RoomId> =
            entries.iter().map(|(room, _)| *room).collect();
        let gone: Vec<RoomId> = self
            .agreed
            .keys()
            .filter(|room| !listed_rooms.contains(room))
            .copied()
            .collect();
        for room in gone {
            self.agreed.remove(&room);
            changed |= tree.forget_personal_room(room).is_some();
        }
        changed |= tree.arrange_rooms(&shown_here);
        self.agreed_order = tree
            .desktop
            .workspaces
            .iter()
            .map(|w| w.room)
            .filter(|room| self.agreed.contains_key(room))
            .collect();
        self.persist();
        changed
    }

    /// The person's devices as the menu of `room`'s workspace offers them,
    /// this device always among them.
    pub fn device_choices(&self, room: RoomId) -> Vec<DeviceChoice> {
        let entry = entries_of(&self.directory)
            .into_iter()
            .find(|(candidate, _)| *candidate == room)
            .map(|(_, entry)| entry);
        let mut devices = self.directory.devices();
        if !devices.iter().any(|(device, _)| *device == self.device) {
            devices.push((self.device, self.label.clone().unwrap_or_default()));
        }
        devices
            .into_iter()
            .map(|(device, label)| DeviceChoice {
                device,
                label,
                shown: entry.as_ref().is_none_or(|entry| shows_on(entry, &device)),
                here: device == self.device,
            })
            .collect()
    }

    /// Show or keep `room`'s workspace off `device`. Only the devices that
    /// opted out are named, so a device linked later shows it. Returns
    /// whether the directory changed.
    pub fn set_shown(&mut self, room: RoomId, device: [u8; 32], shown: bool) -> bool {
        let Some(entry) = entries_of(&self.directory)
            .into_iter()
            .find(|(candidate, _)| *candidate == room)
            .map(|(_, entry)| entry)
        else {
            return false;
        };
        if shows_on(&entry, &device) == shown
            || self
                .directory
                .set_kept_off(entry_id(room), device, !shown)
                .is_err()
        {
            return false;
        }
        self.published = None;
        self.persist();
        true
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
        self.published = None;
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
                .map(|(room, listed)| (room.to_string(), listed.clone()))
                .collect(),
            agreed_order: self.agreed_order.iter().map(ToString::to_string).collect(),
        };
        let Ok(bytes) = postcard::to_allocvec(&stored) else {
            return;
        };
        // Reconciled from the compositor's loop: the comparison and the
        // write (whose fsyncs take seconds under I/O pressure) run on the
        // state writer, which keeps only the newest pending copy.
        crate::shell::durable_writer::global()
            .submit(path.clone(), move || write_if_changed(&path, &bytes));
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
