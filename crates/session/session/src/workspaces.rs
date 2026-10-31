//! The workspace menu's host rows, the directory they open, the person's own
//! workspaces as all their devices hold them, and what the session says when
//! a workspace is left: the parts of workspaces the desk itself cannot
//! answer, because they are the session's to know.
//!
//! A workspace's content — its strips, window groups and tabs — lives in its
//! room and reaches every device that takes part in it. What a device
//! additionally needs is the *list*: which rooms are this person's
//! workspaces, so that one made on the laptop appears on the board as well.
//! That list is the person's workspace directory
//! ([`WorkspaceDirectory`]), a small CRDT every device of theirs holds and
//! only those devices can write: updates are device-signed and checked
//! against the person's root before they merge.
//!
//! The whole desk stays on the device. Besides the workspaces it records
//! what this device is looking at (the active workspace, which strip each
//! display shows, focus pointers) and rooms that belong to other people,
//! none of which another device of the person should take over.
//!
//! Reconciling is a three-way merge against [`Personal::agreed`], the list
//! as both sides last had it: a workspace that appeared or was renamed here
//! since then is written to the list, and one that appeared or was renamed
//! in the list is brought onto this device's desk. The same holds for a
//! workspace taken off one device, for the order of the list, and for the
//! devices a workspace shows on (every device, unless the person keeps it
//! off some).
//!
//! Each entry names the device hosting the room
//! ([`WorkspaceMeta::host_devices`]): the device that made it. Another of
//! the person's devices that does not hold a listed room yet enters it there
//! when the list brings it ([`Adoption::enter`]), which is how a workspace
//! made on one device opens on the others without joining anything anew.
//! Nothing asks the devices on a timer.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_desktop::desktop_host::{
    JOIN_WORKSPACE, LEAVE_WORKSPACE, NEW_WORKSPACE, WORKSPACE_SETTINGS,
};
use tilo_desktop_model::{DesktopTree, RoomId, Workspace};
use tilo_desktop_surface::navigation::Command;
use tilo_desktop_surface::{
    DesktopHandle, DesktopNotifications, HostAction, HostActionSource,
    notifications::{Notification, NotificationIconKind},
};
use tilo_sheets::Seat as _;
use tilo_sheets::directory::{DirectoryEffect, DirectorySeat, ListingAction};
use tilo_user_identity::{
    DeviceCert, DirectoryError, DirectoryUpdate, RevocationList, WorkspaceDirectory, WorkspaceMeta,
};

use crate::directory;
use crate::workspace_settings;

/// Offer, in the workspace menu and in search, what only the session can do
/// about workspaces: making one, joining one, leaving this one, and its
/// settings.
pub(crate) fn publish_host_actions(handle: &DesktopHandle) {
    let offer = |id: &str, title: &str, label: &str| HostAction {
        id: id.to_owned(),
        title: tilo_desktop_surface::i18n::t(title),
        subtitle: None,
        source: Some(HostActionSource::Workspace),
        label: Some(tilo_desktop_surface::i18n::t(label)),
    };
    handle.set_host_actions(vec![
        offer(
            NEW_WORKSPACE,
            "session-host-new-workspace",
            "workspace-menu-new",
        ),
        offer(
            JOIN_WORKSPACE,
            "session-host-join-workspace",
            "workspace-menu-join",
        ),
        offer(
            LEAVE_WORKSPACE,
            "session-host-leave-workspace",
            "workspace-menu-leave",
        ),
        offer(
            WORKSPACE_SETTINGS,
            "session-host-workspace-settings",
            "workspace-menu-settings",
        ),
    ]);
}

/// Show the workspaces this computer may join or open: the ones it holds,
/// and what the sites' directories list.
pub(crate) fn open_directory(
    seat: Option<&DirectorySeat>,
    handle: &DesktopHandle,
    directory: &directory::Store,
) {
    let Some(seat) = seat else {
        return;
    };
    seat.clear_query();
    seat.show(directory::view(directory, handle));
    seat.presentation().open(false);
    directory.ask(None);
}

/// Carry out what the directory asked: reading a site's directory again,
/// joining a public workspace, opening one this computer holds, and closing
/// it.
///
/// `add_workspace` gives this computer the workspace a join was granted, by
/// its room and its name, through the desk's own published action: the desk
/// makes workspaces, not the directory.
pub(crate) fn follow_directory(
    seat: Option<&DirectorySeat>,
    handle: &DesktopHandle,
    directory: &directory::Store,
    add_workspace: &dyn Fn(&directory::Joined) -> Result<(), String>,
) {
    let Some(seat) = seat else {
        return;
    };
    if directory.answered() {
        seat.show(directory::view(directory, handle));
    }
    for joined in directory.take_joined() {
        match add_workspace(&joined) {
            Ok(()) => {
                // The person lands in the workspace they joined, as they do
                // in any other they are given: the directory's work is done,
                // once what the room holds is here.
                if !directory.settling() {
                    seat.presentation().close();
                }
            }
            Err(error) => {
                tracing::warn!(%error, room = %joined.room, "a joined workspace was not added");
                directory.set_note(Some(error));
            }
        }
        seat.show(directory::view(directory, handle));
    }
    for effect in seat.take_asked() {
        match effect {
            DirectoryEffect::Join { key } => {
                join_listed(seat, handle, directory, &key);
            }
            DirectoryEffect::Open { key } => {
                open_listed(seat, handle, directory, &key);
            }
            DirectoryEffect::Look { text } => {
                if let Some(site) = typed_site(&text) {
                    // A site the person typed is asked for what it lists,
                    // and the sheet says it is being read until the worker
                    // answers.
                    directory.ask(Some(site));
                    seat.show(directory::view(directory, handle));
                    continue;
                }
                // One workspace answers the search: Enter takes it, opening
                // it when this computer holds it and joining it otherwise.
                let only: Vec<_> = seat
                    .directory()
                    .listings
                    .into_iter()
                    .filter(|listing| listing.matches(&text))
                    .collect();
                let [only] = only.as_slice() else {
                    tracing::debug!(
                        %text,
                        "the workspace directory looks up a site, a link or a code"
                    );
                    continue;
                };
                if only.action == ListingAction::Open {
                    open_listed(seat, handle, directory, &only.key);
                } else {
                    join_listed(seat, handle, directory, &only.key);
                }
            }
            DirectoryEffect::LinkOrCode { text } => {
                // A link or a code the person pasted joins at once: what it
                // is decides where, and the sheet says so until it lands.
                if let Err(error) = directory.take(&text) {
                    directory.set_note(Some(error));
                }
                seat.show(directory::view(directory, handle));
            }
            DirectoryEffect::Refresh => {
                directory.ask(None);
                seat.show(directory::view(directory, handle));
            }
            DirectoryEffect::Done => {
                seat.presentation().close();
            }
        }
    }
}

/// Go to the workspace `key` names, which this computer holds, and be done
/// with the directory. A workspace the desk no longer admits is said where
/// the person picked it, and the directory stays up to pick another.
fn open_listed(
    seat: &DirectorySeat,
    handle: &DesktopHandle,
    directory: &directory::Store,
    key: &str,
) {
    let opened = handle.dispatch(
        &Command::Workspace {
            generation: crate::GENERATION.into(),
            workspace: key.to_owned(),
        },
        crate::GENERATION,
    );
    if let Err(error) = opened {
        tracing::warn!(%error, %key, "workspace directory open");
        directory.set_note(Some(error));
        seat.show(directory::view(directory, handle));
        return;
    }
    seat.presentation().close();
}

/// Join the listing `key` names, one join at a time: the row joining says
/// so, and pressing it again says it is on its way.
fn join_listed(
    seat: &DirectorySeat,
    handle: &DesktopHandle,
    directory: &directory::Store,
    key: &str,
) {
    let Some(listing) = directory.listing(key) else {
        tracing::debug!(%key, "the workspace directory was asked to join a listing it has none of");
        return;
    };
    match directory.joining() {
        Some(joining) if joining == key => {
            directory.set_note(Some(tilo_sheets::words(
                "directory-already-joining",
                &[("name", &listing.entry.name)],
            )));
        }
        // One join at a time: the first is still on its way.
        Some(_) => return,
        None => {
            if let Err(error) = directory.join(key, &listing) {
                directory.set_note(Some(error));
            }
        }
    }
    seat.show(directory::view(directory, handle));
}

/// The site `text` names, when what was typed into the search is one: a
/// word that is no address looks at the list instead.
fn typed_site(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && text.contains('.') && !text.chars().any(char::is_whitespace))
        .then(|| text.to_owned())
}

/// Say the person left the workspace `name`, and what became of its
/// windows.
pub(crate) fn left_workspace(notices: &DesktopNotifications, name: &str) {
    let mut notice = Notification::new(
        tilo_desktop_surface::i18n::t_args("session-left-workspace", &[("name", name.into())]),
        tilo_desktop_surface::i18n::t("session-left-workspace-body"),
    );
    notice.icon_kind = NotificationIconKind::Workspace;
    notices.post(notice);
}

/// The file beside the desk that keeps the person's list of their own
/// workspaces.
const FILE: &str = "personal-workspaces.postcard";

/// One of the person's workspaces, as their list holds it on every device of
/// theirs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Listed {
    /// What the workspace is called.
    pub(crate) name: String,
    /// The device hosting the room: its device key.
    pub(crate) host: Option<[u8; 32]>,
}

/// One of the person's devices as a workspace's settings offer it: whether
/// the workspace shows there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeviceChoice {
    /// The device, by its key.
    pub(crate) device: [u8; 32],
    /// What the person named it, as their devices know it.
    pub(crate) label: String,
    /// Whether the workspace shows on it.
    pub(crate) shown: bool,
    /// This device.
    pub(crate) here: bool,
}

/// A room the person's list names that this device does not hold, and the
/// device hosting it, which serves it to the person's other devices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToEnter {
    /// The room of the workspace.
    pub(crate) room: RoomId,
    /// What the workspace is called.
    pub(crate) name: String,
    /// The device hosting it.
    pub(crate) host: [u8; 32],
}

/// What bringing the list onto this device's desk asks of the session.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Adoption {
    /// Whether the desk changed.
    pub(crate) changed: bool,
    /// Workspaces to open here, at the device hosting them.
    pub(crate) enter: Vec<ToEnter>,
    /// Workspaces to take off this desk: gone from the list, deleted on
    /// another device, or no longer shown on this one.
    pub(crate) release: Vec<RoomId>,
}

/// The person's list of their own workspaces, as this device holds it and
/// last agreed it with their others.
pub(crate) struct Personal {
    /// This device's collaboration identity: the list's updates are signed
    /// with its key, and its certificate names the device to the person's
    /// others.
    identity: tilo_network_actor::HostIdentity,
    directory: WorkspaceDirectory,
    /// This device, as the list's device registers name it.
    device: [u8; 32],
    /// The workspaces this device had at the last reconciliation.
    agreed: BTreeMap<RoomId, Listed>,
    /// Their order at the last reconciliation.
    agreed_order: Vec<RoomId>,
    /// Listed rooms handed to the desk to open, until their workspace is
    /// here: none of them is taken for one deleted here meanwhile.
    entering: BTreeSet<RoomId>,
    /// Rooms on this desk that are someone else's, joined through their
    /// door: the person's other devices join them each on their own, so the
    /// list never names them.
    borrowed: BTreeSet<RoomId>,
    /// Set when the list changed without a pass over this desk writing it —
    /// a device shown or kept off — so the person's other devices are owed
    /// the list even though this desk wrote nothing.
    untold: bool,
    /// Where the list and `agreed` survive a restart; `None` keeps them in
    /// memory only.
    store: Option<PathBuf>,
    /// What the desk offered the last time [`Self::publish_local`] ran to
    /// the end with everything written. While the desk offers the same and
    /// neither the list nor `agreed` moved since, running it again would
    /// write nothing, so it is skipped.
    published: Option<PublishInputs>,
}

/// The parts of a desk [`Personal::publish_local`] reads.
#[derive(PartialEq, Eq)]
struct PublishInputs {
    listed: BTreeMap<RoomId, Listed>,
    /// Every workspace room of the desk, in its order.
    order: Vec<RoomId>,
}

impl PublishInputs {
    /// What the desk hands the list: the person's own workspaces on it, in
    /// their order. A room joined from a door is not one of theirs.
    ///
    /// A workspace that was made on another device stays hosted by that one,
    /// so the devices holding it agree on where it lives; one this device
    /// made is hosted here.
    fn of(
        handle: &DesktopHandle,
        device: [u8; 32],
        borrowed: &BTreeSet<RoomId>,
        agreed: &BTreeMap<RoomId, Listed>,
    ) -> Self {
        handle.read(|tree| Self {
            listed: tree
                .workspaces
                .iter()
                .filter(|workspace| {
                    !workspace.room().is_nil() && !borrowed.contains(&workspace.room())
                })
                .map(|workspace| {
                    let host = agreed
                        .get(&workspace.room())
                        .and_then(|listed| listed.host)
                        .unwrap_or(device);
                    (
                        workspace.room(),
                        Listed {
                            name: workspace.name.clone(),
                            host: Some(host),
                        },
                    )
                })
                .collect(),
            order: tree.workspaces.iter().map(Workspace::room).collect(),
        })
    }
}

/// What is kept beside the desk.
#[derive(Serialize, Deserialize)]
struct Stored {
    directory: Vec<u8>,
    agreed: BTreeMap<String, Listed>,
    #[serde(default)]
    agreed_order: Vec<String>,
    #[serde(default)]
    borrowed: Vec<String>,
}

/// The list's stable id for `room`: the room's collaboration topic.
fn entry_id(room: RoomId) -> u64 {
    tilo_network_actor::room_topic_id(room)
}

/// The list's entries that carry a valid room, in the person's order.
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

/// Whether `entry` shows on `device`: every device shows it unless it opted
/// out, so a device linked later shows every workspace.
fn shows_on(entry: &WorkspaceMeta, device: &[u8; 32]) -> bool {
    !entry.kept_off_devices.contains(device)
}

impl Personal {
    /// The person's list as this device holds it, kept beside the desk under
    /// `state`; in memory only when there is no state directory.
    ///
    /// A file this build cannot read starts empty: the first pass lists this
    /// device's workspaces again, and the person's other devices answer with
    /// theirs.
    pub(crate) fn open(identity: tilo_network_actor::HostIdentity, state: Option<&Path>) -> Self {
        let user = identity.device_cert.user_public_key;
        let device = identity.device_cert.device_public_key;
        let store = state.map(|state| state.join("tilo").join(FILE));
        let mut personal = Self {
            identity,
            directory: WorkspaceDirectory::new(user, device),
            device,
            agreed: BTreeMap::new(),
            agreed_order: Vec::new(),
            entering: BTreeSet::new(),
            borrowed: BTreeSet::new(),
            untold: false,
            store,
            published: None,
        };
        let read = personal
            .store
            .as_deref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| postcard::from_bytes::<Stored>(&bytes).ok());
        if let Some(stored) = read
            && let Ok(directory) = WorkspaceDirectory::load(user, device, &stored.directory)
        {
            personal.directory = directory;
            personal.agreed = stored
                .agreed
                .into_iter()
                .filter_map(|(room, listed)| Some((RoomId::parse(&room)?, listed)))
                .collect();
            personal.agreed_order = stored
                .agreed_order
                .iter()
                .filter_map(|room| RoomId::parse(room))
                .collect();
            personal.borrowed = stored
                .borrowed
                .iter()
                .filter_map(|room| RoomId::parse(room))
                .collect();
        }
        personal
    }

    /// `room` arrived from a door: it is the workspace of whoever let the
    /// person in, not one of their own, so their other devices are never told
    /// to open it themselves — each joins it through its own invitation.
    pub(crate) fn joined(&mut self, room: RoomId) {
        if room.is_nil() || !self.borrowed.insert(room) {
            return;
        }
        self.published = None;
        self.persist();
    }

    /// Write what changed on this device's desk since the person's devices
    /// last agreed into the list: new and renamed workspaces, ones taken off
    /// the desk here, and a new order. Returns whether the list changed,
    /// which is when the person's other devices need to hear of it.
    ///
    /// Called after every pass, so when neither the desk nor the list moved
    /// since the last call it only compares the desk with what it offered
    /// then.
    pub(crate) fn publish_local(&mut self, handle: &DesktopHandle) -> bool {
        let inputs = PublishInputs::of(handle, self.device, &self.borrowed, &self.agreed);
        if self.published.as_ref() == Some(&inputs) {
            return false;
        }
        let listed = &inputs.listed;
        let mut complete = true;
        let mut changed = false;
        let label = self.identity.device_cert.device_label.trim().to_owned();
        if !label.is_empty() {
            let registered = self
                .directory
                .devices()
                .iter()
                .any(|(device, known)| *device == self.device && known == &label);
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
                    tracing::warn!(%error, %room, "a workspace of the person's was not listed");
                }
            }
        }
        // A workspace agreed on and gone from this desk was taken off it
        // here.
        let present: BTreeSet<RoomId> = inputs.order.iter().copied().collect();
        // A room being entered has no workspace here yet; it was not deleted
        // here.
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
        changed |= self.publish_order(&inputs.order);
        if changed {
            self.persist();
        }
        // A write that failed is tried again on the next call.
        self.published = complete.then_some(inputs);
        changed
    }

    /// Give the workspaces this desk holds positions in the list that match
    /// their order here, when that order moved since the last agreement. Only
    /// entries out of place get a new position, so a concurrent move of
    /// another workspace on another device still lands.
    fn publish_order(&mut self, order: &[RoomId]) -> bool {
        let here: Vec<RoomId> = order
            .iter()
            .copied()
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

    /// Bring what changed in the list since the last agreement onto this
    /// device's desk: the person's order, and which listed rooms this device
    /// lets go (gone from the list, or no longer shown on this one) or opens
    /// at the device hosting them (listed, shown here, and not on this desk).
    ///
    /// A room is entered only at a host the list names: a device must never
    /// guess where someone's room lives, so an entry naming none is left
    /// until one lists it. This device enters nothing at itself.
    pub(crate) fn adopt_into(&mut self, handle: &DesktopHandle) -> Adoption {
        let entries = entries_of(&self.directory);
        let mut adoption = Adoption::default();
        let present: Vec<RoomId> =
            handle.read(|tree| tree.workspaces.iter().map(Workspace::room).collect());
        let agreed = self.agreed.clone();
        let mut shown_here: Vec<RoomId> = Vec::new();
        for (room, meta) in &entries {
            if !shows_on(meta, &self.device) {
                if self.agreed.remove(room).is_some() && present.contains(room) {
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
            // On this desk already: nothing to enter.
            if present.contains(room) {
                self.agreed.insert(*room, entry);
                continue;
            }
            let Some(host) = entry.host.filter(|host| *host != self.device) else {
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
        let listed_rooms: BTreeSet<RoomId> = entries.iter().map(|(room, _)| *room).collect();
        let gone: Vec<RoomId> = self
            .agreed
            .keys()
            .filter(|room| !listed_rooms.contains(room))
            .copied()
            .collect();
        for room in gone {
            self.agreed.remove(&room);
            self.entering.remove(&room);
            if present.contains(&room) {
                adoption.release.push(room);
            }
        }
        adoption.changed = self.arrange(handle, &present, &shown_here);
        // Both run on every pass: the list is written, and what the desk
        // offered read again, only when one of the two moved.
        if adoption.changed || self.agreed != agreed {
            self.published = None;
            self.persist();
        }
        adoption
    }

    /// Put this desk's workspaces in the person's order: the rooms the list
    /// names, in its order, then the rest as they were. Whether they moved.
    fn arrange(&mut self, handle: &DesktopHandle, present: &[RoomId], order: &[RoomId]) -> bool {
        let mut wanted: Vec<RoomId> = order
            .iter()
            .copied()
            .filter(|room| present.contains(room))
            .collect();
        let rest: Vec<RoomId> = present
            .iter()
            .copied()
            .filter(|room| !wanted.contains(room))
            .collect();
        wanted.extend(rest);
        if wanted == present {
            return false;
        }
        let mut tree = handle.read(Clone::clone);
        reorder(&mut tree, &wanted);
        self.agreed_order = tree
            .workspaces
            .iter()
            .map(Workspace::room)
            .filter(|room| self.agreed.contains_key(room))
            .collect();
        handle.replace_tree(tree);
        true
    }

    /// The person's devices as the workspace of `room` offers them, this
    /// device always among them: whether the workspace shows on each, and
    /// which one is this device.
    ///
    /// Empty for a room the person's list does not name: a workspace joined
    /// through someone else's door is not theirs to show or keep off.
    pub(crate) fn device_choices(&self, room: RoomId) -> Vec<DeviceChoice> {
        let Some(entry) = entries_of(&self.directory)
            .into_iter()
            .find(|(candidate, _)| *candidate == room)
            .map(|(_, entry)| entry)
        else {
            return Vec::new();
        };
        let mut devices = self.directory.devices();
        if !devices.iter().any(|(device, _)| *device == self.device) {
            let label = self.identity.device_cert.device_label.trim().to_owned();
            devices.push((self.device, label));
        }
        devices
            .into_iter()
            .map(|(device, label)| DeviceChoice {
                device,
                label,
                shown: shows_on(&entry, &device),
                here: device == self.device,
            })
            .collect()
    }

    /// Whether `device` is the device this session runs on.
    pub(crate) fn is_this_device(&self, device: &[u8; 32]) -> bool {
        *device == self.device
    }

    /// Whether the workspace of `room` shows on `device`.
    pub(crate) fn shown_on(&self, room: RoomId, device: &[u8; 32]) -> Option<bool> {
        let entry = entries_of(&self.directory)
            .into_iter()
            .find(|(candidate, _)| *candidate == room)
            .map(|(_, entry)| entry)?;
        Some(shows_on(&entry, device))
    }

    /// Show the workspace of `room` on `device`, or keep it off there. Only
    /// the devices that opted out are named, so a device linked later shows
    /// it. Returns whether the list changed.
    pub(crate) fn set_shown(&mut self, room: RoomId, device: [u8; 32], shown: bool) -> bool {
        // A room the list does not name has no entry to keep anything off.
        if !matches!(self.shown_on(room, &device), Some(current) if current != shown) {
            return false;
        }
        if self
            .directory
            .set_kept_off(entry_id(room), device, !shown)
            .is_err()
        {
            return false;
        }
        // Writing the choice is not a pass over the desk, which is what
        // tells the person's other devices: this device says so itself.
        self.published = None;
        self.untold = true;
        self.persist();
        true
    }

    /// Merge another device's signed update. `author` is that device's
    /// certificate, as the transport authenticated it; the list checks that
    /// it belongs to this person and signed the update.
    ///
    /// # Errors
    /// The list's reason for refusing the update.
    pub(crate) fn merge(
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

    /// Whether `update` already holds everything this device's list does, so
    /// answering it with this device's own would tell its author nothing.
    pub(crate) fn covered_by(&mut self, update: &DirectoryUpdate) -> bool {
        self.directory.covered_by(&update.delta)
    }

    /// This device's whole list, signed for the person's other devices.
    pub(crate) fn snapshot(&mut self) -> Option<DirectoryUpdate> {
        self.directory
            .signed_snapshot(&self.identity.device_signing_key, tilo_clock::unix_now_ns())
            .ok()
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
            borrowed: self.borrowed.iter().map(ToString::to_string).collect(),
        };
        let Ok(bytes) = postcard::to_allocvec(&stored) else {
            return;
        };
        write_if_changed(&path, &bytes);
    }
}

/// Put `tree`'s workspaces in the order `rooms` names: those it names, in
/// its order, then the rest as they were. Whether they moved.
fn reorder(tree: &mut DesktopTree, rooms: &[RoomId]) -> bool {
    let before: Vec<u64> = tree
        .workspaces
        .iter()
        .map(|workspace| workspace.id.0)
        .collect();
    let mut rest = std::mem::take(&mut tree.workspaces);
    let mut arranged: Vec<Workspace> = Vec::with_capacity(rest.len());
    for room in rooms {
        if let Some(at) = rest.iter().position(|workspace| workspace.room() == *room) {
            arranged.push(rest.remove(at));
        }
    }
    arranged.append(&mut rest);
    let after: Vec<u64> = arranged.iter().map(|workspace| workspace.id.0).collect();
    tree.workspaces = arranged;
    before != after
}

/// Write the list where it is kept, unless what is there is what this writes.
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
        tracing::warn!(%error, path = %path.display(), "the person's workspaces could not be saved");
    }
}

/// The person's own workspaces as this computer can keep them: nothing when
/// this computer knows no identity to keep them for.
pub(crate) fn personal_for(state: Option<&Path>) -> Option<RefCell<Personal>> {
    crate::sync::device_identity().map(|identity| RefCell::new(Personal::open(identity, state)))
}

/// Take the workspace lists the person's other devices sent since the last
/// pass: whether anything of theirs was taken, and whether a device that sent
/// one is missing something of this one's.
fn take_personal(personal: &mut Personal, sync: &crate::sync::Client) -> (bool, bool) {
    let mut taken = false;
    let mut missing = false;
    for (update, author) in sync.take_personal() {
        // Asked before the merge: once theirs is taken, answering would only
        // repeat it. Asked first, it says whether the device that sent it is
        // missing something of this one's.
        let behind = !personal.covered_by(&update);
        match personal.merge(&update, &author, tilo_clock::unix_now_ns()) {
            Ok(()) => {
                taken = true;
                missing |= behind;
            }
            Err(error) => {
                tracing::debug!(%error, "a device's workspaces were not taken");
            }
        }
    }
    (taken, missing)
}

impl crate::Shared {
    /// Open the workspace of `room` on this desk, named `name`, through the
    /// desk's own action for making one: a workspace of the person's that
    /// their other device made opens here without joining anything anew.
    pub(crate) fn add_workspace(&self, room: &str, name: &str) -> Result<(), String> {
        let args = serde_json::Map::from_iter([
            ("room".to_owned(), serde_json::Value::from(room.to_owned())),
            ("name".to_owned(), serde_json::Value::from(name.to_owned())),
        ]);
        self.ask_of_desk("new-workspace", &args)
    }

    /// Give this computer the workspace a join was granted, through the
    /// desk's own action for making one, so the desk keeps its workspaces,
    /// and redeem the invitation it was granted with: what the room held
    /// before this device arrived comes with it.
    pub(crate) fn add_joined_workspace(&self, joined: &directory::Joined) -> Result<(), String> {
        self.add_workspace(&joined.room, &joined.name)?;
        // A workspace a door let the person into is whoever let them in's:
        // the person's other devices join it each through their own
        // invitation, so their list never names it.
        if let (Some(personal), Some(room)) = (&self.personal, RoomId::parse(&joined.room)) {
            personal.borrow_mut().joined(room);
        }
        self.redeem_invitation(joined);
        Ok(())
    }

    /// Redeem the invitation a join was granted with, off the loop: this
    /// device becomes a member of the room and takes in its state. A session
    /// that holds no rooms, or takes no part in the collaboration plane,
    /// shows the workspace with what this computer itself keeps in it.
    fn redeem_invitation(&self, joined: &directory::Joined) {
        let Some(room) = RoomId::parse(&joined.room) else {
            return;
        };
        let (Some(sync), Some(engine)) = (&self.sync, &self.engine) else {
            return;
        };
        let workspace = tilo_desktop_model::WorkspaceId::of_room(room);
        engine.bind(workspace, room);
        let asked = sync.enroll(crate::sync::Enrolling {
            workspace,
            room,
            invitation: joined.invitation.clone(),
        });
        match asked {
            Ok(()) => self.directory_store.expect_enrollment(&joined.room),
            Err(error) => {
                tracing::warn!(%error, room = %joined.room, "the invitation was not redeemed");
                engine.unbind(workspace);
                self.release_workspace(room);
                self.directory_store.set_note(Some(error));
            }
        }
    }

    /// Take what became of the invitations this session redeemed: the person
    /// lands in the workspace they joined once the room is held here, or it
    /// is taken off the desk and the sheet says why.
    pub(crate) fn follow_enrollments(&self) {
        let Some(sync) = &self.sync else {
            return;
        };
        let mut settled = false;
        for enrolled in sync.take_enrolled() {
            let key = enrolled.room.to_string();
            let name = self
                .handle
                .read(|tree| {
                    tree.workspaces
                        .iter()
                        .find(|workspace| workspace.room() == enrolled.room)
                        .map(|workspace| workspace.name.clone())
                })
                .unwrap_or_else(|| key.clone());
            if enrolled.result.is_err() {
                if let Some(engine) = &self.engine {
                    engine.unbind(enrolled.workspace);
                }
                self.release_workspace(enrolled.room);
            }
            self.directory_store.enrolled(&key, &name, &enrolled.result);
            settled = true;
        }
        let Some(seat) = &self.directory else {
            return;
        };
        if settled {
            if self.directory_store.settling() {
                seat.show(directory::view(&self.directory_store, &self.handle));
            } else {
                // The person lands in the workspace they joined: the
                // directory has nothing left to say.
                seat.presentation().close();
            }
        }
    }

    /// Keep the person's own workspaces as all their devices hold them:
    /// list what this desk has, take what the others listed, open here what
    /// they made there, take off the desk what they took off theirs, and say
    /// again what this device has when theirs was missing something.
    pub(crate) fn follow_personal(&self) {
        let Some(personal) = &self.personal else {
            return;
        };
        let news = {
            let mut personal = personal.borrow_mut();
            let mut news = personal.publish_local(&self.handle);
            // A device shown or kept off is a change this desk's pass did
            // not write. The list is sent whole, so this asks for one now;
            // a send that does not happen carries it the next time.
            news |= personal.untold;
            personal.untold = false;
            let mut behind = false;
            if let Some(sync) = &self.sync {
                let (taken, missing) = take_personal(&mut personal, sync);
                news |= taken;
                behind |= missing;
            }
            let adoption = personal.adopt_into(&self.handle);
            for opened in adoption.enter {
                let room = opened.room.to_string();
                if let Err(error) = self.add_workspace(&room, &opened.name) {
                    tracing::warn!(%error, %room, "a workspace of the person's was not opened here");
                }
            }
            for room in adoption.release {
                self.release_workspace(room);
            }
            if (news || behind)
                && let Some(sync) = &self.sync
                && let Some(update) = personal.snapshot()
            {
                sync.tell_personal(&update);
            }
            if adoption.changed {
                self.keep_desk_now();
            }
            news
        };
        // The list moved, so what an open sheet says of the person's
        // devices may have too: a device they linked, or a choice made on
        // another one, arrives in the list alone.
        if news {
            self.ws_settings_fed.set(None);
            workspace_settings::refresh(
                self.workspace_settings.as_ref(),
                &self.workspace_settings_feed(),
            );
        }
    }

    /// Take the workspace of `room` off this desk through the desk's own
    /// action, which refuses to leave the person with none.
    fn release_workspace(&self, room: RoomId) {
        let Some(id) = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .find(|workspace| workspace.room() == room)
                .map(|workspace| workspace.id.0.to_string())
        }) else {
            return;
        };
        let args =
            serde_json::Map::from_iter([("workspace".to_owned(), serde_json::Value::from(id))]);
        if let Err(error) = self.ask_of_desk("leave-workspace", &args) {
            // The desk keeps its one workspace: the person is never left
            // with none, and the list brings it back on the next pass.
            tracing::debug!(%error, %room, "a workspace not shown here was not taken off the desk");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::{IdMinter, Workspace, WorkspaceId};
    use tilo_desktop_surface::seed_tree;

    /// A device of the person's, certified for their own root key.
    fn identity(user: u8, device: u8, label: &str) -> tilo_network_actor::HostIdentity {
        let person = ed25519_dalek::SigningKey::from_bytes(&[user; 32]);
        let this = ed25519_dalek::SigningKey::from_bytes(&[device; 32]);
        let mut cert = DeviceCert {
            user_public_key: person.verifying_key().to_bytes(),
            device_public_key: this.verifying_key().to_bytes(),
            device_label: label.to_owned(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
        tilo_network_actor::HostIdentity {
            device_cert: cert,
            device_signing_key: this,
            workspace_device_certs: std::collections::HashMap::new(),
            workspace_revocations: std::collections::HashMap::new(),
            identity_paths: None,
        }
    }

    /// The desk a fresh device starts from.
    fn desk() -> DesktopHandle {
        let mut minter = IdMinter::new(1);
        let (policy, _surface) = tilo_desktop_policy::DesktopPolicy::with_tree(
            seed_tree(&mut minter, "Desk", Vec::new()),
            1,
        );
        policy.handle().clone()
    }

    /// The workspaces on `handle`'s desk: room and name, in order.
    fn on_desk(handle: &DesktopHandle) -> Vec<(RoomId, String)> {
        handle.read(|tree| {
            tree.workspaces
                .iter()
                .map(|workspace| (workspace.room(), workspace.name.clone()))
                .collect()
        })
    }

    /// Put a workspace of `room` on `handle`'s desk, as the desk's own action
    /// for making one does.
    fn open_here(handle: &DesktopHandle, room: RoomId, name: &str) {
        let mut tree = handle.read(Clone::clone);
        tree.workspaces
            .push(Workspace::in_room(WorkspaceId::of_room(room), room, name));
        handle.replace_tree(tree);
    }

    /// The list's entry for `room`, if it names one.
    fn listed(personal: &Personal, room: RoomId) -> Option<Listed> {
        entries_of(&personal.directory)
            .into_iter()
            .find(|(named, _)| *named == room)
            .map(|(_, entry)| Listed {
                name: entry.name,
                host: entry.host_devices.first().copied(),
            })
    }

    #[test]
    fn a_fresh_desks_workspace_is_listed_and_kept() {
        let state = tempfile::tempdir().expect("a state directory");
        let desk = desk();
        let desk_workspaces = on_desk(&desk);
        let [(room, name)] = desk_workspaces.as_slice() else {
            panic!("a fresh desk holds one workspace");
        };
        let (room, name) = (*room, name.clone());
        let identity = identity(0x11, 0x21, "The laptop");
        let device = identity.device_cert.device_public_key;
        let mut personal = Personal::open(identity.clone(), Some(state.path()));
        assert!(
            personal.publish_local(&desk),
            "the workspace the desk starts from is the person's own"
        );
        assert_eq!(
            listed(&personal, room),
            Some(Listed {
                name,
                host: Some(device),
            }),
            "the list names it, hosted by the device that made it"
        );

        // Kept beside the desk: the next session starts with the same list
        // and has nothing to write.
        let kept = state.path().join("tilo").join(FILE);
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&kept)
                .expect("the list is kept")
                .permissions(),
        );
        assert_eq!(mode & 0o777, 0o600, "the person's list is theirs alone");
        let mut reopened = Personal::open(identity, Some(state.path()));
        assert!(!reopened.publish_local(&desk), "nothing changed since");
        assert_eq!(
            reopened.agreed, personal.agreed,
            "the list survives the session"
        );
    }

    #[test]
    fn a_workspace_made_on_another_device_opens_here_without_joining() {
        let a_state = tempfile::tempdir().expect("a state directory");
        let b_state = tempfile::tempdir().expect("a state directory");
        let a = desk();
        let a_identity = identity(0x11, 0x21, "The laptop");
        let a_device = a_identity.device_cert.device_public_key;
        let mut a_list = Personal::open(a_identity.clone(), Some(a_state.path()));
        a_list.publish_local(&a);
        let a_workspaces = on_desk(&a);
        let [(a_room, a_name)] = a_workspaces.as_slice() else {
            panic!("the other device holds one workspace");
        };
        let (a_room, a_name) = (*a_room, a_name.clone());
        let snapshot = a_list.snapshot().expect("the list is signed");

        let b = desk();
        let b_identity = identity(0x11, 0x22, "The desktop");
        let b_device = b_identity.device_cert.device_public_key;
        let mut b_list = Personal::open(b_identity, Some(b_state.path()));
        b_list.publish_local(&b);
        assert!(
            !b_list.covered_by(&snapshot),
            "this device's list is missing theirs, so it is owed one back"
        );
        b_list
            .merge(
                &snapshot,
                &a_identity.device_cert,
                tilo_clock::unix_now_ns(),
            )
            .expect("the person's own device's list is taken");

        let adoption = b_list.adopt_into(&b);
        assert_eq!(
            adoption.enter,
            vec![ToEnter {
                room: a_room,
                name: a_name.clone(),
                host: a_device,
            }],
            "the workspace made on their other device opens here"
        );

        // What the session does with it: the desk makes the workspace.
        open_here(&b, a_room, &a_name);
        let again = b_list.adopt_into(&b);
        assert!(
            again.enter.is_empty(),
            "a workspace now here is not opened again"
        );
        b_list.publish_local(&b);
        assert_eq!(
            listed(&b_list, a_room).map(|listed| listed.host),
            Some(Some(a_device)),
            "it stays hosted by the device that made it, not by this one"
        );
        assert_ne!(a_device, b_device, "two devices of the same person");
    }

    #[test]
    fn a_workspace_joined_through_its_door_is_not_this_persons_to_list() {
        let state = tempfile::tempdir().expect("a state directory");
        let desk = desk();
        let desk_workspaces = on_desk(&desk);
        let [(own_room, _)] = desk_workspaces.as_slice() else {
            panic!("a fresh desk holds one workspace");
        };
        let own_room = *own_room;
        let joined = RoomId::parse("1b7c4e08-3a5d-42f6-8c9e-2d6f0a1b3c5e").expect("a room");
        open_here(&desk, joined, "Book club");
        let mut personal = Personal::open(identity(0x11, 0x21, "The laptop"), Some(state.path()));
        personal.joined(joined);
        personal.publish_local(&desk);
        assert!(listed(&personal, own_room).is_some(), "their own is listed");
        assert_eq!(
            listed(&personal, joined),
            None,
            "a workspace someone else let them into is joined on each device, never listed"
        );
    }

    #[test]
    fn a_workspace_taken_off_here_is_taken_off_the_list() {
        let state = tempfile::tempdir().expect("a state directory");
        let desk = desk();
        let identity = identity(0x11, 0x21, "The laptop");
        let other = RoomId::parse("1b7c4e08-3a5d-42f6-8c9e-2d6f0a1b3c5e").expect("a room");
        open_here(&desk, other, "Book club");
        let mut personal = Personal::open(identity.clone(), Some(state.path()));
        personal.publish_local(&desk);
        assert!(listed(&personal, other).is_some(), "both are listed");

        let mut tree = desk.read(Clone::clone);
        tree.workspaces
            .retain(|workspace| workspace.room() != other);
        desk.replace_tree(tree);
        assert!(personal.publish_local(&desk), "the list changed here");
        assert_eq!(
            listed(&personal, other),
            None,
            "a workspace taken off this desk is gone from the list"
        );
        let mut reopened = Personal::open(identity, Some(state.path()));
        assert!(!reopened.publish_local(&desk));
        assert_eq!(reopened.agreed.len(), 1, "and it is not brought back");
    }

    #[test]
    fn a_workspace_gone_from_the_list_is_taken_off_here() {
        let a_state = tempfile::tempdir().expect("a state directory");
        let b_state = tempfile::tempdir().expect("a state directory");
        let a = desk();
        let a_identity = identity(0x11, 0x21, "The laptop");
        let mut a_list = Personal::open(a_identity.clone(), Some(a_state.path()));
        let other = RoomId::parse("1b7c4e08-3a5d-42f6-8c9e-2d6f0a1b3c5e").expect("a room");
        open_here(&a, other, "Book club");
        a_list.publish_local(&a);

        let b = desk();
        let mut b_list = Personal::open(identity(0x11, 0x22, "The desktop"), Some(b_state.path()));
        b_list
            .merge(
                &a_list.snapshot().expect("the list is signed"),
                &a_identity.device_cert,
                tilo_clock::unix_now_ns(),
            )
            .expect("the person's list");
        let adoption = b_list.adopt_into(&b);
        for opened in &adoption.enter {
            open_here(&b, opened.room, &opened.name);
        }
        assert_eq!(
            on_desk(&b).len(),
            3,
            "this device's own and both of their other device's workspaces are here"
        );

        // Taken off the other device's desk, the list no longer names it.
        let mut tree = a.read(Clone::clone);
        tree.workspaces
            .retain(|workspace| workspace.room() != other);
        a.replace_tree(tree);
        a_list.publish_local(&a);
        b_list
            .merge(
                &a_list.snapshot().expect("the list is signed"),
                &a_identity.device_cert,
                tilo_clock::unix_now_ns(),
            )
            .expect("the person's list");
        let adoption = b_list.adopt_into(&b);
        assert_eq!(
            adoption.release,
            vec![other],
            "a workspace gone from their other device is taken off this desk"
        );
    }

    #[test]
    fn a_workspace_kept_off_a_device_is_taken_off_its_desk() {
        let a_state = tempfile::tempdir().expect("a state directory");
        let b_state = tempfile::tempdir().expect("a state directory");
        let a = desk();
        let a_identity = identity(0x11, 0x21, "The laptop");
        let mut a_list = Personal::open(a_identity.clone(), Some(a_state.path()));
        a_list.publish_local(&a);
        let a_workspaces = on_desk(&a);
        let [(a_room, _)] = a_workspaces.as_slice() else {
            panic!("the other device holds one workspace");
        };
        let a_room = *a_room;

        let b = desk();
        let b_identity = identity(0x11, 0x22, "The desktop");
        let b_device = b_identity.device_cert.device_public_key;
        let mut b_list = Personal::open(b_identity.clone(), Some(b_state.path()));
        b_list.publish_local(&b);
        // Each device tells the other what it holds, as the sessions they
        // keep between them do.
        b_list
            .merge(
                &a_list.snapshot().expect("the list is signed"),
                &a_identity.device_cert,
                tilo_clock::unix_now_ns(),
            )
            .expect("the person's list");
        a_list
            .merge(
                &b_list.snapshot().expect("the list is signed"),
                &b_identity.device_cert,
                tilo_clock::unix_now_ns(),
            )
            .expect("the person's list");
        for opened in b_list.adopt_into(&b).enter {
            open_here(&b, opened.room, &opened.name);
        }
        b_list.publish_local(&b);
        assert!(
            on_desk(&b).iter().any(|(room, _)| *room == a_room),
            "both devices hold the workspace"
        );

        // On the laptop, the person keeps the workspace off the desktop.
        assert!(
            a_list.set_shown(a_room, b_device, false),
            "the choice is a change to their list"
        );
        let choices = a_list.device_choices(a_room);
        let here = choices.iter().find(|choice| choice.here).expect("its row");
        assert!(here.shown, "the workspace still shows here");
        let elsewhere = choices
            .iter()
            .find(|choice| !choice.here)
            .expect("the other device's row");
        assert_eq!(
            (elsewhere.device, elsewhere.shown, elsewhere.label.as_str()),
            (b_device, false, "The desktop"),
            "the control shows the workspace kept off the desktop"
        );
        let reopened = Personal::open(a_identity.clone(), Some(a_state.path()));
        assert_eq!(
            reopened.device_choices(a_room),
            choices,
            "and keeps showing it: the choice is in their list, kept beside the desk"
        );

        // The desktop learns of it and honours it: the workspace leaves its
        // desk and is not opened there again.
        b_list
            .merge(
                &a_list.snapshot().expect("the list is signed"),
                &a_identity.device_cert,
                tilo_clock::unix_now_ns(),
            )
            .expect("the person's list");
        let adoption = b_list.adopt_into(&b);
        assert_eq!(
            adoption.release,
            vec![a_room],
            "a workspace kept off this device is taken off its desk"
        );
        let mut tree = b.read(Clone::clone);
        tree.workspaces
            .retain(|workspace| workspace.room() != a_room);
        b.replace_tree(tree);
        assert!(
            b_list.adopt_into(&b).enter.is_empty(),
            "and a desk without it does not open it again"
        );
        assert!(
            on_desk(&a).iter().any(|(room, _)| *room == a_room),
            "the device it shows on still has it"
        );
    }
}
