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
//! Each entry names the device hosting the room (`WorkspaceMeta::
//! host_devices`): the device that made it, whose own host serves it, or a
//! home relay of the person's. Another of the person's devices that does not
//! hold a listed room yet enters it at that host and enrolls as a member
//! ([`Adoption::enter`]), when the list brings it: nothing asks the hosts
//! on a timer.
//!
//! The list holds the person's own rooms: those a host of theirs governs.
//! A room someone else invited them to is joined on each device through
//! its own invitation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_user_identity::{
    DeviceCert, DirectoryError, DirectoryUpdate, RevocationList, WorkspaceDirectory, WorkspaceMeta,
};

use crate::shell::state::{RoomId, WorkspaceTree};

/// One workspace as the person's list holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listed {
    pub name: String,
    /// The device hosting the room: its device key.
    pub host: Option<[u8; 32]>,
}

/// The person's own rooms this device holds, each with the device hosting
/// it: what the native owner's admitted state says, read outside the tree's
/// lock.
pub type HeldRooms = BTreeMap<RoomId, [u8; 32]>;

/// A room the person's list names that this device does not hold: entered
/// at its host, which issues the invitation this device enrolls with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToEnter {
    pub room: RoomId,
    pub name: String,
    /// The device hosting it.
    pub host: [u8; 32],
}

/// What bringing the list into a tree asks of the network.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Adoption {
    /// Whether the tree changed.
    pub changed: bool,
    /// Rooms to enter at their hosts.
    pub enter: Vec<ToEnter>,
    /// Rooms to let go: deleted on another device, or kept off this one.
    /// Releasing one takes its workspace and replica off together.
    pub release: Vec<RoomId>,
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
    /// Listed rooms handed to the network to enter, until their workspace
    /// is here: none of them is taken for one deleted here meanwhile.
    entering: std::collections::BTreeSet<RoomId>,
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
}

impl PublishInputs {
    fn of(tree: &WorkspaceTree, held: &HeldRooms) -> Self {
        Self {
            listed: member_workspaces(tree, held),
            order: tree
                .desktop
                .workspaces
                .iter()
                .map(crate::shell::state::Workspace::room)
                .collect(),
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

/// The workspaces of `tree` whose rooms are the person's own and held
/// here (`held`), with their names and the devices hosting them. A room
/// still joining or being placed is listed once it is held.
pub fn member_workspaces(tree: &WorkspaceTree, held: &HeldRooms) -> BTreeMap<RoomId, Listed> {
    tree.desktop
        .workspaces
        .iter()
        .filter_map(|workspace| {
            let host = held.get(&workspace.room())?;
            Some((
                workspace.room(),
                Listed {
                    name: workspace.name.clone(),
                    host: Some(*host),
                },
            ))
        })
        .collect()
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
            entering: std::collections::BTreeSet::new(),
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
    /// Called after every write to the tree, so when neither the tree's
    /// workspaces nor the directory changed since the last call it only
    /// compares the tree with what it offered then.
    pub fn publish_local(&mut self, tree: &WorkspaceTree, held: &HeldRooms) -> bool {
        let inputs = PublishInputs::of(tree, held);
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
            if self.agreed.get(room) == Some(entry) {
                continue;
            }
            let id = entry_id(*room);
            let hosts: Vec<[u8; 32]> = entry.host.into_iter().collect();
            let written = self
                .directory
                .add_room_workspace(id, &room.to_string(), &entry.name)
                .and_then(|()| self.directory.set_host_devices(id, &hosts))
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
        let present: std::collections::BTreeSet<RoomId> = tree
            .desktop
            .workspaces
            .iter()
            .map(crate::shell::state::Workspace::room)
            .collect();
        // A room being entered has no workspace here yet; it was not
        // deleted here.
        self.entering.retain(|room| !present.contains(room));
        let deleted: Vec<RoomId> = self
            .agreed
            .keys()
            .filter(|room| !present.contains(room) && !self.entering.contains(room))
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

    /// Whether the last [`Self::publish_local`] wrote everything the tree
    /// offered, and nothing moved the directory since: running it again on
    /// the same tree would write nothing.
    pub fn publish_settled(&self) -> bool {
        self.published.is_some()
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
            .map(crate::shell::state::Workspace::room)
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
    /// `tree`: the person's order, and which listed rooms this device lets go
    /// (deleted on another device, or no longer shown on this one) or
    /// enters at their hosts (listed, shown here, and not held). A room is
    /// entered only at a host the list names; `here` is this device, which
    /// enters nothing at itself.
    pub fn adopt_into(&mut self, tree: &mut WorkspaceTree, here: &[u8; 32]) -> Adoption {
        self.published = None;
        let entries = entries_of(&self.directory);
        let mut adoption = Adoption::default();
        let mut shown_here: Vec<RoomId> = Vec::new();
        for (room, meta) in &entries {
            if !shows_on(meta, &self.device) {
                if self.agreed.remove(room).is_some() && tree.has_workspace(*room) {
                    adoption.release.push(*room);
                }
                self.entering.remove(room);
                continue;
            }
            shown_here.push(*room);
            let entry = Listed {
                name: meta.name.clone(),
                host: meta.host_devices.first().copied(),
            };
            // Held here: nothing to enter. A workspace of the room is not
            // enough (one kept from an earlier build may never have been
            // entered), nor is a join left from the session before: entering
            // it is how that join goes on. Entering is idempotent: one
            // already running at the host is not started again.
            if tree.holds_room(*room) {
                self.agreed.insert(*room, entry);
                continue;
            }
            // A device must never guess where someone's room lives: an
            // entry naming no host is left until one lists it.
            let Some(host) = entry.host.filter(|host| host != here) else {
                continue;
            };
            adoption.enter.push(ToEnter {
                room: *room,
                name: entry.name.clone(),
                host,
            });
            self.entering.insert(*room);
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
            self.entering.remove(&room);
            if tree.has_workspace(room) {
                adoption.release.push(room);
            }
        }
        adoption.changed |= tree.arrange_rooms(&shown_here);
        self.agreed_order = tree
            .desktop
            .workspaces
            .iter()
            .map(crate::shell::state::Workspace::room)
            .filter(|room| self.agreed.contains_key(room))
            .collect();
        self.persist();
        adoption
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
