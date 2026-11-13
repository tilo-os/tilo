//! The workspace menu's host rows, the directory they open, the person's own
//! workspaces as all their devices hold them, and what the session says when
//! a workspace is left: the parts of workspaces the desk itself cannot
//! answer, because they are the session's to know.
//!
//! A workspace's content — its strips, window groups and tabs — lives in the
//! workspace's collaboration and reaches every device that takes part in it.
//! What a device additionally needs is the *list*: which workspaces are this
//! person's, so that one made on the laptop appears on the board as well.
//! That list is the person's workspace directory
//! ([`WorkspaceDirectory`]), a small CRDT every device of theirs holds and
//! only those devices can write: updates are device-signed and checked
//! against the person's root before they merge.
//!
//! The whole desk stays on the device. Besides the workspaces it records
//! what this device is looking at (the active workspace, which strip each
//! display shows, focus pointers) and workspaces that belong to other people,
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
//! Each entry names the device hosting the workspace
//! ([`WorkspaceMeta::host_devices`]): the device that made it. Another of
//! the person's devices that does not hold a listed workspace yet enters it there
//! when the list brings it ([`Adoption::enter`]), which is how a workspace
//! made on one device opens on the others without joining anything anew.
//! Nothing asks the devices on a timer.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId};
use tilo_desktop_surface::DesktopHandle;
use tilo_sheets::Seat as _;
use tilo_startup_metrics::JOIN_FLOW_TARGET;
use tilo_user_identity::{
    CommunityState, DeviceCert, DirectoryError, DirectoryUpdate, RevocationList,
    WorkspaceDirectory, WorkspaceMeta,
};

use crate::directory;
use crate::platform::notice::Notices;
use crate::workspace_settings;

/// Say the person left the workspace `name` , and what became of its windows.
pub(crate) fn left_workspace(notices: &dyn Notices, name: &str) {
    crate::directory::workspace_notice(
        notices,
        ("session-left-workspace", "session-left-workspace-body"),
        &tilo_desktop_surface::names::workspace(name),
    );
}

/// The file beside the desk that keeps the person's list of their own workspaces.
const FILE: &str = "personal-workspaces.postcard";

/// One of the person's workspaces, as their list holds it on every device of theirs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Listed {
    /// What the workspace is called.
    pub(crate) name: String,
    /// The device hosting the workspace: its device key.
    pub(crate) host: Option<[u8; 32]>,
}

/// One of the person's devices as a workspace's settings offer it: whether the workspace shows
/// there.
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

/// A workspace the person's list names that this device does not hold, and the device hosting it,
/// which serves it to the person's other devices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToEnter {
    /// The workspace it names.
    pub(crate) workspace: WorkspaceId,
    /// What the workspace is called.
    pub(crate) name: String,
    /// The device hosting it.
    pub(crate) host: [u8; 32],
    /// On this desk already, held as the device this machine was copied from: it is entered as this
    /// device without leaving the desk.
    pub(crate) held: bool,
}

/// How a workspace came to this desk, which says whether the person goes there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arrival {
    /// The person joined it here: they land in the workspace they joined.
    Joined,
    /// Their list brought it from another of their devices, with nobody here asking for it: it
    /// stands on the desk and the person stays in the workspace they are in, so a window they open
    /// as it arrives is a window of the workspace they opened it in.
    Listed,
}

/// What bringing the list onto this device's desk asks of the session.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Adoption {
    /// Whether the desk changed.
    pub(crate) changed: bool,
    /// Workspaces to open here, at the device hosting them.
    pub(crate) enter: Vec<ToEnter>,
    /// Workspaces to take off this desk: gone from the list, deleted on another device, or no
    /// longer shown on this one.
    pub(crate) release: Vec<WorkspaceId>,
}

/// The person's list of their own workspaces, as this device holds it and last agreed it with their
/// others.
pub(crate) struct Personal {
    /// This device's collaboration identity: the list's updates are signed with its key, and its
    /// certificate names the device to the person's others.
    identity: tilo_network_actor::HostIdentity,
    directory: WorkspaceDirectory,
    /// This device, as the list's device registers name it.
    device: [u8; 32],
    /// The workspaces this device had at the last reconciliation.
    agreed: BTreeMap<WorkspaceId, Listed>,
    /// Their order at the last reconciliation.
    agreed_order: Vec<WorkspaceId>,
    /// Listed workspaces handed to the desk to open, until their workspace is here: none of them is
    /// taken for one deleted here meanwhile.
    entering: BTreeSet<WorkspaceId>,
    /// Workspaces on this desk that are someone else's, joined through their door: the person's
    /// other devices join them each on their own, so the list never names them.
    borrowed: BTreeSet<WorkspaceId>,
    /// Where each workspace this device holds is hosted, as this device's workspace itself says:
    /// the list names that device, never this one, so a device of the person's that opens the
    /// workspace from the list enters it at the device that hosts it.
    hosts: BTreeMap<WorkspaceId, [u8; 32]>,
    /// The entries asked for of the person's other devices, by workspace and the host hint they
    /// were asked at: one ask per hint, so a workspace whose host has not answered yet is not asked
    /// for again on every pass. A device of the person's that just came up may carry a newer hint,
    /// so its arrival empties this.
    asked: BTreeMap<WorkspaceId, tilo_peer_state::join::TransportHint>,
    /// Set when the list changed without a pass over this desk writing it — a device shown or kept
    /// off — so the person's other devices are owed the list even though this desk wrote nothing.
    untold: bool,
    /// Where the list and `agreed` survive a restart; `None` keeps them in memory only.
    store: Option<PathBuf>,
    /// What the desk offered the last time [`Self::publish_local`] ran to the end with everything
    /// written. While the desk offers the same and neither the list nor `agreed` moved since,
    /// running it again would write nothing, so it is skipped.
    published: Option<PublishInputs>,
    /// Whether the list this computer kept could not be read, until the person is told: the list
    /// starts empty for it.
    unreadable: bool,
    /// The list as the person's keys read it, last kept on the hosts of their workspaces and handed
    /// to this device's door: kept again when it moves.
    kept: Option<tilo_relay_session::workspaces::PersonalList>,
    /// Whether `kept` was handed to the hosts too: a session's broker reaches them only once it
    /// serves, so a list said before then is kept on them once it does.
    kept_on_hosts: bool,
    /// What the files this device's fleet is read from looked like when the list was last said:
    /// a fleet that moved since — a device this device's root just certified — is said again.
    pub(crate) fleet: Option<tilo_identity_host::device_fleet::FleetStamps>,
    /// The workspaces whose host has not taken this device in yet that the person was told of:
    /// once each, not on every ask that times out.
    told_awaiting: BTreeSet<WorkspaceId>,
    /// When a workspace whose host did not take this device in is asked again: a host that was
    /// away comes back, and nothing else would ask it again ([`Self::asked`] holds one ask per
    /// hint).
    ask_again: BTreeMap<WorkspaceId, std::time::Instant>,
}

/// How long after a host did not take this device in it is asked again.
const ASK_AGAIN_AFTER: std::time::Duration = std::time::Duration::from_secs(20);

/// The parts of a desk [`Personal::publish_local`] reads.
#[derive(PartialEq, Eq)]
struct PublishInputs {
    listed: BTreeMap<WorkspaceId, Listed>,
    /// Every workspace of the desk, in its order.
    order: Vec<WorkspaceId>,
}

impl PublishInputs {
    /// What the desk hands the list: the person's own workspaces on it, in
    /// their order. A workspace joined from a door is not one of theirs.
    ///
    /// A workspace that was made on another device stays hosted by that one,
    /// so the devices holding it agree on where it lives; one this device
    /// made is hosted here.
    fn of(
        handle: &DesktopHandle,
        device: [u8; 32],
        borrowed: &BTreeSet<WorkspaceId>,
        agreed: &BTreeMap<WorkspaceId, Listed>,
        hosts: &BTreeMap<WorkspaceId, [u8; 32]>,
    ) -> Self {
        handle.read(|tree| Self {
            listed: tree
                .workspaces
                .iter()
                .filter(|workspace| !workspace.id().is_nil() && !borrowed.contains(&workspace.id()))
                .map(|workspace| {
                    // Where the workspace itself says it is hosted first: a device that joined one
                    // hosted elsewhere must not name itself, or the person's other devices enter a
                    // workspace that is not here.
                    let host = hosts
                        .get(&workspace.id())
                        .copied()
                        .or_else(|| agreed.get(&workspace.id()).and_then(|listed| listed.host))
                        .unwrap_or(device);
                    (
                        workspace.id(),
                        Listed {
                            name: workspace.name.clone(),
                            host: Some(host),
                        },
                    )
                })
                .collect(),
            order: tree.workspaces.iter().map(Workspace::id).collect(),
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

/// The list's stable id for a workspace: its collaboration topic.
fn entry_id(workspace: WorkspaceId) -> u64 {
    tilo_network_actor::workspace_topic_id(workspace)
}

/// The list's entries that carry a valid workspace, in the person's order.
fn entries_of(directory: &WorkspaceDirectory) -> Vec<(WorkspaceId, WorkspaceMeta)> {
    let mut out: Vec<(WorkspaceId, WorkspaceMeta)> = directory
        .list_workspaces()
        .into_iter()
        .filter_map(|entry| {
            let workspace = WorkspaceId::parse(entry.room.as_deref()?)?;
            (!workspace.is_nil() && entry_id(workspace) == entry.id).then_some((workspace, entry))
        })
        .collect();
    out.sort_by(|(left_id, left), (right_id, right)| {
        let key = |entry: &WorkspaceMeta| entry.order.unwrap_or(f64::INFINITY);
        key(left)
            .total_cmp(&key(right))
            .then_with(|| left_id.cmp(right_id))
    });
    out
}

/// Whether `entry` shows on `device` : every device shows it unless it opted out, so a device
/// linked later shows every workspace.
fn shows_on(entry: &WorkspaceMeta, device: &[u8; 32]) -> bool {
    !entry.kept_off_devices.contains(device)
}

impl Personal {
    /// Ask `workspace` 's host again after [`ASK_AGAIN_AFTER`]: it did not take this device in.
    fn ask_again_later(&mut self, workspace: WorkspaceId) {
        if let Some(at) = std::time::Instant::now().checked_add(ASK_AGAIN_AFTER) {
            self.ask_again.insert(workspace, at);
        }
    }

    /// Forget the asks whose wait is over, so the pass asks those hosts again.
    fn ask_again_due(&mut self) {
        let now = std::time::Instant::now();
        let due: Vec<WorkspaceId> = self
            .ask_again
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(workspace, _)| *workspace)
            .collect();
        for workspace in due {
            self.ask_again.remove(&workspace);
            self.asked.remove(&workspace);
        }
    }

    /// The person's list as this device holds it, kept beside the desk under `state` ; in memory
    /// only when there is no state directory.
    /// A file this build cannot read starts empty: the first pass lists this device's workspaces
    /// again, and the person's other devices answer with theirs.
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
            hosts: BTreeMap::new(),
            asked: BTreeMap::new(),
            untold: false,
            store,
            published: None,
            unreadable: false,
            kept: None,
            kept_on_hosts: false,
            fleet: None,
            told_awaiting: BTreeSet::new(),
            ask_again: BTreeMap::new(),
        };
        let read = personal
            .store
            .as_deref()
            .and_then(|path| match std::fs::read(path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        path = %path.display(),
                        "the remembered workspaces could not be read"
                    );
                    personal.unreadable = true;
                    None
                }
            })
            .and_then(|bytes| match postcard::from_bytes::<Stored>(&bytes) {
                Ok(stored) => Some(stored),
                Err(error) => {
                    tracing::warn!(%error, "the remembered workspaces do not read back");
                    personal.unreadable = true;
                    None
                }
            });
        if let Some(stored) = read {
            match WorkspaceDirectory::load(user, device, &stored.directory) {
                Ok(directory) => {
                    personal.directory = directory;
                    personal.agreed = stored
                        .agreed
                        .into_iter()
                        .filter_map(|(workspace, listed)| {
                            Some((WorkspaceId::parse(&workspace)?, listed))
                        })
                        .collect();
                    personal.agreed_order = stored
                        .agreed_order
                        .iter()
                        .filter_map(|workspace| WorkspaceId::parse(workspace))
                        .collect();
                    personal.borrowed = stored
                        .borrowed
                        .iter()
                        .filter_map(|workspace| WorkspaceId::parse(workspace))
                        .collect();
                }
                Err(error) => {
                    tracing::warn!(%error, "the remembered workspaces' list does not read back");
                    personal.unreadable = true;
                }
            }
        }
        personal
    }

    /// Whether the list this computer kept could not be read, once: the person is told so an empty
    /// list is not taken for workspaces that are gone.
    pub(crate) fn take_unreadable(&mut self) -> bool {
        std::mem::take(&mut self.unreadable)
    }

    /// `workspace` arrived from a door: it is the workspace of whoever let the person in, not one
    /// of their own, so their other devices are never told to open it themselves — each joins it
    /// through its own invitation.
    pub(crate) fn joined(&mut self, workspace: WorkspaceId) {
        if workspace.is_nil() || !self.borrowed.insert(workspace) {
            return;
        }
        self.published = None;
        self.persist();
    }

    /// The workspaces on this device's desk that are someone else's, joined through their door:
    /// whose workspace they are is their host's governance's to say, not this person's.
    pub(crate) fn borrowed(&self) -> &BTreeSet<WorkspaceId> {
        &self.borrowed
    }

    /// Whether this device is the one that made `workspace` : the list names this device as its
    /// host, or names no host yet for a workspace a pass has not listed. A workspace another device
    /// of the person's made is hosted there, and one this device is entering from there is not its
    /// to make: neither is founded here.
    pub(crate) fn made_here(&self, workspace: WorkspaceId) -> bool {
        if self.entering.contains(&workspace) {
            return false;
        }
        self.hosts
            .get(&workspace)
            .copied()
            .or_else(|| self.agreed.get(&workspace).and_then(|listed| listed.host))
            .is_none_or(|host| host == self.device)
    }

    /// What this person decided about the community at `site` , as all their devices hold it:
    /// `None` while they decided nothing, which is when their community is joined for them.
    pub(crate) fn community(&self, site: &str) -> Option<CommunityState> {
        self.directory.community(site)
    }

    /// Record what this person decided about the community at `site` : [`CommunityState::Pending`]
    /// as their door is asked, and [`CommunityState::Decided`] once the site let them in — their
    /// decision from then on, including after they leave it. It is the person's, not this desk's,
    /// so their other devices are told of it. Whether it changed anything.
    pub(crate) fn decide_community(&mut self, site: &str, state: CommunityState) -> bool {
        let changed = match self.directory.set_community(site, state) {
            Ok(changed) => changed,
            Err(error) => {
                tracing::warn!(%error, %site, "what this person decided about their community was not kept");
                return false;
            }
        };
        if changed {
            // Not a pass over the desk, which is what tells the person's other devices: this device
            // says so itself.
            self.published = None;
            self.untold = true;
            self.persist();
        }
        changed
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
        let inputs = PublishInputs::of(
            handle,
            self.device,
            &self.borrowed,
            &self.agreed,
            &self.hosts,
        );
        if self.published.as_ref() == Some(&inputs) {
            return false;
        }
        let listed = &inputs.listed;
        let mut complete = true;
        let mut changed = false;
        let named = tilo_user_identity::device_name(&self.identity.device_cert.device_label);
        if let Some(label) = named {
            let registered = self
                .directory
                .devices()
                .iter()
                .any(|(device, known)| *device == self.device && known == label);
            if !registered {
                if self.directory.register_device(self.device, label).is_ok() {
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
        for (workspace, entry) in listed {
            let fresh = !self.agreed.contains_key(workspace);
            if self.agreed.get(workspace) == Some(entry) {
                continue;
            }
            let id = entry_id(*workspace);
            let hosts: Vec<[u8; 32]> = entry.host.into_iter().collect();
            let written = self
                .directory
                .add_workspace_with_topic(id, &workspace.to_string(), &entry.name)
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
                    self.agreed.insert(*workspace, entry.clone());
                    changed = true;
                }
                Err(error) => {
                    complete = false;
                    tracing::warn!(%error, %workspace, "a workspace of the person's was not listed");
                }
            }
        }
        // A workspace agreed on and gone from this desk was taken off it here.
        let present: BTreeSet<WorkspaceId> = inputs.order.iter().copied().collect();
        // A listed workspace being entered has no workspace here yet; it was not deleted here.
        self.entering
            .retain(|workspace| !present.contains(workspace));
        let deleted: Vec<WorkspaceId> = self
            .agreed
            .keys()
            .filter(|workspace| !present.contains(workspace) && !self.entering.contains(workspace))
            .copied()
            .collect();
        for workspace in deleted {
            let _ = self.directory.remove_workspace(entry_id(workspace));
            self.agreed.remove(&workspace);
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

    /// Give the workspaces this desk holds positions in the list that match their order here, when
    /// that order moved since the last agreement. Only entries out of place get a new position, so
    /// a concurrent move of another workspace on another device still lands.
    fn publish_order(&mut self, order: &[WorkspaceId]) -> bool {
        let here: Vec<WorkspaceId> = order
            .iter()
            .copied()
            .filter(|workspace| self.agreed.contains_key(workspace))
            .collect();
        let agreed: Vec<WorkspaceId> = self
            .agreed_order
            .iter()
            .filter(|workspace| self.agreed.contains_key(workspace))
            .copied()
            .collect();
        if here == agreed {
            self.agreed_order = here;
            return false;
        }
        let positions: BTreeMap<WorkspaceId, f64> = entries_of(&self.directory)
            .into_iter()
            .filter_map(|(workspace, entry)| Some((workspace, entry.order?)))
            .collect();
        let mut changed = false;
        let mut previous = f64::NEG_INFINITY;
        for (index, workspace) in here.iter().enumerate() {
            if let Some(current) = positions.get(workspace).copied()
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
            if self
                .directory
                .set_order(entry_id(*workspace), placed)
                .is_ok()
            {
                changed = true;
            }
            previous = placed;
        }
        self.agreed_order = here;
        changed
    }

    /// Read where each workspace this device holds is hosted, as the workspaces themselves say: the
    /// list names the device that hosts a workspace, so a device of the person's that opens one
    /// from the list enters it there rather than at the device that happened to join it.
    fn refresh_hosts(&mut self, engine: Option<&crate::engine::Engine>) {
        let Some(engine) = engine else {
            return;
        };
        self.hosts = engine
            .workspaces()
            .into_iter()
            .filter_map(|workspace| Some((workspace, engine.host_of(workspace)?)))
            .collect();
    }

    /// Bring what changed in the list since the last agreement onto this device's desk: the
    /// person's order, and which listed workspaces this device lets go (gone from the list, or no
    /// longer shown on this one) or opens at the device hosting them (listed, shown here, and not
    /// on this desk).
    /// A listed workspace is entered only at a host the list names: a device must never guess where
    /// someone's workspace lives, so an entry naming none is left until one lists it. This device
    /// enters nothing at itself.
    /// A workspace on this desk whose governance does not know this device (`unknown_here`) is one
    /// a machine copied from another holds as the device it was copied from: it is entered as this
    /// device, at the device hosting it, and stays on the desk meanwhile.
    pub(crate) fn adopt_into(
        &mut self,
        handle: &DesktopHandle,
        unknown_here: &dyn Fn(WorkspaceId) -> bool,
    ) -> Adoption {
        let entries = entries_of(&self.directory);
        let mut adoption = Adoption::default();
        let present: Vec<WorkspaceId> =
            handle.read(|tree| tree.workspaces.iter().map(Workspace::id).collect());
        let agreed = self.agreed.clone();
        let mut shown_here: Vec<WorkspaceId> = Vec::new();
        for (workspace, meta) in &entries {
            if !shows_on(meta, &self.device) {
                if self.agreed.remove(workspace).is_some() && present.contains(workspace) {
                    adoption.release.push(*workspace);
                }
                self.entering.remove(workspace);
                continue;
            }
            shown_here.push(*workspace);
            let entry = Listed {
                name: meta.name.clone(),
                host: meta.host_devices.first().copied(),
            };
            // On this desk already: nothing to enter, unless it is held as the device this machine
            // was copied from.
            if present.contains(workspace) {
                let copied = unknown_here(*workspace);
                if let Some(host) = entry.host.filter(|host| *host != self.device)
                    && copied
                {
                    adoption.enter.push(ToEnter {
                        workspace: *workspace,
                        name: entry.name.clone(),
                        host,
                        held: true,
                    });
                }
                self.agreed.insert(*workspace, entry);
                continue;
            }
            let Some(host) = entry.host.filter(|host| *host != self.device) else {
                continue;
            };
            adoption.enter.push(ToEnter {
                workspace: *workspace,
                name: entry.name.clone(),
                host,
                held: false,
            });
            self.entering.insert(*workspace);
            self.agreed.insert(*workspace, entry);
        }
        let named: BTreeSet<WorkspaceId> =
            entries.iter().map(|(workspace, _)| *workspace).collect();
        let gone: Vec<WorkspaceId> = self
            .agreed
            .keys()
            .filter(|workspace| !named.contains(workspace))
            .copied()
            .collect();
        for workspace in gone {
            self.agreed.remove(&workspace);
            self.entering.remove(&workspace);
            if present.contains(&workspace) {
                adoption.release.push(workspace);
            }
        }
        adoption.changed = self.arrange(handle, &present, &shown_here);
        // Both run on every pass: the list is written, and what the desk offered read again, only
        // when one of the two moved.
        if adoption.changed || self.agreed != agreed {
            self.published = None;
            self.persist();
        }
        adoption
    }

    /// Put this desk's workspaces in the person's order: the workspaces the list names, in its
    /// order, then the rest as they were. Whether they moved.
    fn arrange(
        &mut self,
        handle: &DesktopHandle,
        present: &[WorkspaceId],
        order: &[WorkspaceId],
    ) -> bool {
        let mut wanted: Vec<WorkspaceId> = order
            .iter()
            .copied()
            .filter(|workspace| present.contains(workspace))
            .collect();
        let rest: Vec<WorkspaceId> = present
            .iter()
            .copied()
            .filter(|workspace| !wanted.contains(workspace))
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
            .map(Workspace::id)
            .filter(|workspace| self.agreed.contains_key(workspace))
            .collect();
        handle.replace_tree(tree);
        true
    }

    /// The person's devices as a workspace's settings offer them, this device always among them:
    /// whether the workspace shows on each, and which one is this device.
    /// Empty for a workspace the person's list does not name: one joined through someone else's
    /// door is not theirs to show or keep off.
    pub(crate) fn device_choices(&self, workspace: WorkspaceId) -> Vec<DeviceChoice> {
        let Some(entry) = entries_of(&self.directory)
            .into_iter()
            .find(|(candidate, _)| *candidate == workspace)
            .map(|(_, entry)| entry)
        else {
            return Vec::new();
        };
        let mut devices = self.directory.devices();
        if !devices.iter().any(|(device, _)| *device == self.device) {
            let label = self.identity.device_cert.device_label.clone();
            devices.push((self.device, label));
        }
        devices
            .into_iter()
            .map(|(device, label)| DeviceChoice {
                device,
                label: tilo_user_identity::device_name(&label)
                    .unwrap_or_default()
                    .to_owned(),
                shown: shows_on(&entry, &device),
                here: device == self.device,
            })
            .collect()
    }

    /// Whether `device` is the device this session runs on.
    pub(crate) fn is_this_device(&self, device: &[u8; 32]) -> bool {
        *device == self.device
    }

    /// Whether the workspace shows on `device` .
    pub(crate) fn shown_on(&self, workspace: WorkspaceId, device: &[u8; 32]) -> Option<bool> {
        let entry = entries_of(&self.directory)
            .into_iter()
            .find(|(candidate, _)| *candidate == workspace)
            .map(|(_, entry)| entry)?;
        Some(shows_on(&entry, device))
    }

    /// Show the workspace on `device` , or keep it off there. Only the devices that opted out are
    /// named, so a device linked later shows it. Returns whether the list changed.
    pub(crate) fn set_shown(
        &mut self,
        workspace: WorkspaceId,
        device: [u8; 32],
        shown: bool,
    ) -> bool {
        // A workspace the list does not name has no entry to keep anything off.
        if !matches!(self.shown_on(workspace, &device), Some(current) if current != shown) {
            return false;
        }
        if self
            .directory
            .set_kept_off(entry_id(workspace), device, !shown)
            .is_err()
        {
            return false;
        }
        // Writing the choice is not a pass over the desk, which is what tells the person's other
        // devices: this device says so itself.
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

    /// Whether `update` already holds everything this device's list does, so answering it with this
    /// device's own would tell its author nothing.
    pub(crate) fn covered_by(&mut self, update: &DirectoryUpdate) -> bool {
        self.directory.covered_by(&update.delta)
    }

    /// This device's whole list, signed for the person's other devices.
    /// The person's workspaces as their keys read them: every one the list carries, in the person's
    /// order, each with the signed address of its host `hint` answers for it, as a workspace's id.
    pub(crate) fn readable(
        &self,
        hint: impl Fn(&str) -> Option<Vec<u8>>,
    ) -> tilo_relay_session::workspaces::PersonalList {
        let mut listed = self.directory.list_workspaces();
        listed.sort_by(|a, b| {
            a.order
                .unwrap_or(f64::INFINITY)
                .total_cmp(&b.order.unwrap_or(f64::INFINITY))
                .then(a.id.cmp(&b.id))
        });
        let workspaces = listed
            .into_iter()
            .filter_map(|meta| {
                let workspace = meta.room?;
                let host = hint(&workspace).and_then(|hint| {
                    tilo_relay_session::workspaces::WorkspaceHostAddress::of_hint(&hint)
                });
                Some(tilo_relay_session::workspaces::PersonalWorkspace {
                    name: meta.name,
                    host,
                    role: meta.governor.is_none().then(|| "owner".to_owned()),
                    workspace,
                })
            })
            .collect();
        tilo_relay_session::workspaces::PersonalList {
            workspaces,
            devices: Vec::new(),
        }
    }

    pub(crate) fn snapshot(&mut self) -> Option<DirectoryUpdate> {
        self.directory
            .signed_snapshot(self.identity.signer.as_ref(), tilo_clock::unix_now_ns())
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
                .map(|(workspace, listed)| (workspace.to_string(), listed.clone()))
                .collect(),
            agreed_order: self.agreed_order.iter().map(ToString::to_string).collect(),
            borrowed: self.borrowed.iter().map(ToString::to_string).collect(),
        };
        let Ok(bytes) = postcard::to_allocvec(&stored) else {
            return;
        };
        keep(&path, &bytes);
    }
}

/// Put `tree` 's workspaces in the order `wanted` names: those it names, in its order, then the
/// rest as they were. Whether they moved.
fn reorder(tree: &mut DesktopTree, wanted: &[WorkspaceId]) -> bool {
    let before: Vec<u64> = tree
        .workspaces
        .iter()
        .map(|workspace| workspace.slot.0)
        .collect();
    let mut rest = std::mem::take(&mut tree.workspaces);
    let mut arranged: Vec<Workspace> = Vec::with_capacity(rest.len());
    for workspace_id in wanted {
        if let Some(at) = rest
            .iter()
            .position(|workspace| workspace.id() == *workspace_id)
        {
            arranged.push(rest.remove(at));
        }
    }
    arranged.append(&mut rest);
    let after: Vec<u64> = arranged.iter().map(|workspace| workspace.slot.0).collect();
    tree.workspaces = arranged;
    before != after
}

/// Keep the list where it is kept: the state writer takes the write, so a pass that changes it does
/// not wait for the disk, and the latest list wins over one still being written.
fn keep(path: &Path, bytes: &[u8]) {
    tilo_durable_writer::keep(path.to_path_buf(), bytes.to_vec());
}

/// The person's own workspaces as this computer can keep them: nothing when this computer knows no
/// identity to keep them for.
pub(crate) fn personal_for(state: Option<&Path>) -> Option<RefCell<Personal>> {
    crate::sync::device_identity().map(|identity| RefCell::new(Personal::open(identity, state)))
}

/// Take the workspace lists the person's other devices sent since the last pass: whether anything
/// of theirs was taken, and whether a device that sent one is missing something of this one's.
fn take_personal(personal: &mut Personal, sync: &crate::sync::Client) -> (bool, bool) {
    let mut taken = false;
    let mut missing = false;
    for (update, author) in sync.take_personal() {
        // Asked before the merge: once theirs is taken, answering would only repeat it. Asked
        // first, it says whether the device that sent it is missing something of this one's.
        let behind = !personal.covered_by(&update);
        match personal.merge(&update, &author, tilo_clock::unix_now_ns()) {
            Ok(()) => {
                taken = true;
                missing |= behind;
            }
            Err(error) => {
                tracing::warn!(%error, "a device's workspaces were not taken");
            }
        }
    }
    (taken, missing)
}

impl crate::Shared {
    /// Give every workspace on the desk that is nobody's yet the owner its governance names: the
    /// person at this device for their own — the one seeded here, one made here, or one made on
    /// another device of theirs — and, for a workspace joined through its door, the owner its
    /// host's governance has, which this device learns when the workspace comes here. A workspace
    /// whose owner is not known here keeps none, and never takes this person's: a host's workspace
    /// is not theirs to claim. One naming someone else keeps that name: ownership moves through the
    /// workspace's own governance, never here.
    /// Run after every pass, so a desk this device kept from before its workspaces were anyone's is
    /// claimed the first time it is shown, and a workspace made here or joined here is claimed as
    /// it arrives. A desk kept from before this device had an identity is theirs all the same: one
    /// naming them as the login account at it is claimed for who they are now. A pass with nothing
    /// to claim writes nothing.
    /// Who each workspace has in it follows the same rule: what the workspace's own governance here
    /// names is what the desk shows, so a change another member's device made shows on this desk.
    pub(crate) fn own_workspaces(&self) {
        // A list this computer kept but could not read starts empty here, so the person is told why
        // instead of finding their workspaces gone.
        if let Some(personal) = &self.personal
            && personal.borrow_mut().take_unreadable()
        {
            self.notifications
                .borrow()
                .tell(&crate::notices::workspaces_notice());
        }
        let borrowed = self
            .personal
            .as_ref()
            .map(|personal| personal.borrow().borrowed().clone())
            .unwrap_or_default();
        let mut tree = self.handle.read(Clone::clone);
        let mut changed = false;
        let endpoint = self.sync.as_ref().map(|sync| sync.endpoint_addr());
        for workspace in &mut tree.workspaces {
            if let Some(engine) = &self.engine
                && self
                    .personal
                    .as_ref()
                    .is_some_and(|personal| personal.borrow().made_here(workspace.id()))
                && !workspace.id().is_nil()
                && !borrowed.contains(&workspace.id())
                && !engine.holds(workspace.id())
            {
                // A workspace made on this desk is a workspace from here on,
                // not only this desk's: its governance is founded in the
                // workspace it already has an identity for.
                // It is signed by this device's signer, off the loop: the pass after it landed
                // sees it held.
                let (engine, slot, id) = (
                    std::sync::Arc::clone(engine),
                    workspace.slot,
                    workspace.id(),
                );
                // The name it is founded under is the one a person reads: a workspace nobody
                // named keeps no name of its own, and a workspace is founded under a name, so
                // the word the desk reads for it stands in until one is given.
                let (name, endpoint) = (
                    tilo_desktop_surface::names::workspace(&workspace.name),
                    endpoint.clone(),
                );
                let founded = crate::landing::off_loop(async move {
                    engine.found(slot, id, &name, endpoint.as_ref()).await;
                });
                self.landings.then(founded, drop);
            }
            if workspace
                .owner()
                .is_some_and(|owner| !self.earlier.contains(owner))
            {
                continue;
            }
            if let Some(owner) = self.owner_of(workspace.id(), &borrowed) {
                workspace.set_owner(owner);
                changed = true;
            } else {
                tracing::warn!(
                    workspace = %workspace.id(),
                    borrowed = borrowed.contains(&workspace.id()),
                    "a workspace on the desk is nobody's this pass: nothing may be done in it"
                );
            }
        }
        if changed {
            self.handle.replace_tree(tree);
        }
        // A workspace founded before its record said where its own host is reached is named now: a
        // workspace hosted here is reached at this device, and nothing reaches its records until
        // the record says so.
        if let (Some(engine), Some(endpoint)) = (&self.engine, &endpoint) {
            engine.name_own_hosts(endpoint);
        }
        self.project_governance();
    }

    /// Who the governance of a workspace names as its owner: its own, when this device holds it,
    /// else the person at this device for a workspace of their own that no governance here has yet.
    /// A workspace founded before this device had an identity names the person by the id they went
    /// by then; it is theirs as the person they are now, which is what their own actions in it are
    /// decided as. `None` for one joined through a door whose host has not said whose it is here.
    fn owner_of(&self, workspace: WorkspaceId, borrowed: &BTreeSet<WorkspaceId>) -> Option<String> {
        if let Some(engine) = &self.engine
            && let Ok(owner) = engine.owner_of(&workspace.to_string())
        {
            return Some(if self.earlier.contains(&owner) {
                self.person.clone()
            } else {
                owner
            });
        }
        (!borrowed.contains(&workspace)).then(|| self.person.clone())
    }

    /// Open a listed workspace on this desk, named `name` , through the desk's own action for
    /// making one: a workspace of the person's that their other device made opens here without
    /// joining anything anew.
    pub(crate) fn add_workspace(
        &self,
        workspace: &str,
        name: &str,
        arrival: Arrival,
    ) -> Result<(), String> {
        let mut args = serde_json::Map::from_iter([
            (
                "workspace".to_owned(),
                serde_json::Value::from(workspace.to_owned()),
            ),
            ("name".to_owned(), serde_json::Value::from(name.to_owned())),
        ]);
        if arrival == Arrival::Listed {
            args.insert("background".to_owned(), serde_json::Value::from(true));
        }
        self.ask_of_desk("new-workspace", &args)
    }

    /// Give this computer the workspace a join chose, and come into it the way the join named:
    /// through its door, or with the invitation it holds. What the workspace held before this
    /// device arrived comes with it.
    /// A joined workspace the desk does not already hold is admitted before it gets a slot
    /// ([`Self::redeem_invitation`]): a host that refuses leaves the person where they were, with
    /// no empty workspace to arrive in and vanish from.
    pub(crate) fn add_joined_workspace(&self, joined: &directory::Joined) -> Result<(), String> {
        let Some(workspace) = WorkspaceId::parse(&joined.granted.workspace) else {
            return Err(tilo_desktop_surface::i18n::t(
                "desktop-joined-workspace-unknown",
            ));
        };
        // A workspace a door let the person into is whoever let them in's: the person's other
        // devices join it each through their own invitation, so their list never names it.
        if let Some(personal) = &self.personal {
            personal.borrow_mut().joined(workspace);
        }
        // A session the host started with a short link learns what it joined only here, and the
        // authority it waits for is asked of it.
        self.boot_join
            .borrow_mut()
            .joined(&joined.granted.workspace);
        self.redeem_invitation(joined);
        Ok(())
    }

    /// Come into the workspace a join chose, off the loop, through its door or
    /// with its invitation: this device becomes a member of the workspace and
    /// takes in its state. A
    /// session that holds no workspaces, or takes no part in the collaboration
    /// plane,
    /// shows the workspace with what this computer itself keeps in it.
    ///
    /// A workspace the desk does not already hold gets its slot when the
    /// host admits this device ([`Self::follow_enrollments`]), not before: a
    /// host that refuses leaves the person where they were, and no empty
    /// workspace is ever shown to arrive in and vanish from.
    fn redeem_invitation(&self, joined: &directory::Joined) {
        let Some(workspace) = WorkspaceId::parse(&joined.granted.workspace) else {
            // A slot for a workspace id that cannot be parsed can never be arrived in: take it
            // back.
            tracing::info!(
                target: JOIN_FLOW_TARGET,
                workspace = %joined.granted.workspace,
                outcome = "slot removed",
                "join: the workspace id does not parse, so no invitation can be presented and its slot is taken away"
            );
            if let Some(held) = self.slot_of(&joined.granted.workspace) {
                self.release_workspace(held);
            }
            return;
        };
        let (Some(sync), Some(engine)) = (&self.sync, &self.engine) else {
            tracing::info!(
                target: JOIN_FLOW_TARGET,
                workspace = %joined.granted.workspace,
                held_locally = true,
                "join: this session holds no workspaces or takes no part in the collaboration plane, so the workspace stays as this computer keeps it"
            );
            self.open_joined_workspace(&joined.granted.workspace, &joined.name, Arrival::Joined);
            return;
        };
        let slot = tilo_desktop_model::WorkspaceSlot::of_workspace(workspace);
        let on_desk = self.slot_of(&joined.granted.workspace).is_some();
        engine.bind(slot, workspace);
        tracing::info!(
            target: JOIN_FLOW_TARGET,
            workspace = %joined.granted.workspace,
            slot = slot.0,
            on_desk,
            "join: the workspace slot is made for the workspace"
        );
        let asked = sync.enroll(crate::sync::Enrolling {
            slot,
            workspace_id: workspace,
            way: joined.granted.way.clone(),
            // A door's workspace learns the person by the name the desk knows them by.
            name: crate::platform::directory::person()
                .map(|person| person.name)
                .unwrap_or_default(),
            renewal: self.fleet_renewal.clone(),
        });
        match asked {
            Ok(()) => {
                tracing::info!(
                    target: JOIN_FLOW_TARGET,
                    workspace = %joined.granted.workspace,
                    slot = slot.0,
                    "join: the way in is presented to the workspace's host"
                );
                self.directory_store
                    .expect_enrollment(&joined.granted.workspace);
            }
            Err(error) => {
                tracing::warn!(%error, workspace = %joined.granted.workspace, "the workspace was not asked to take this device in");
                tracing::info!(
                    target: JOIN_FLOW_TARGET,
                    workspace = %joined.granted.workspace,
                    slot = slot.0,
                    %error,
                    outcome = "slot removed",
                    "join: the way in could not be presented, so the slot is taken away"
                );
                engine.unbind(slot);
                if on_desk {
                    self.release_workspace(workspace);
                }
                self.directory_store.set_note(Some(error));
            }
        }
    }

    /// Tell the person, once, that `workspace` (`name` on their list) waits for its host to take
    /// this device in: until then the workspace's apps take part in nothing here.
    fn tell_awaited(&self, workspace: WorkspaceId, name: &str) {
        let host = self
            .engine
            .as_ref()
            .and_then(|engine| engine.awaited_host(workspace));
        if let (Some(host), Some(personal)) = (host, &self.personal)
            && personal.borrow_mut().told_awaiting.insert(workspace)
        {
            crate::directory::awaiting_notice(&*self.telling(), name, &host);
            tracing::info!(
                target: JOIN_FLOW_TARGET,
                %workspace,
                %host,
                "the person is told this device waits for the workspace's host to take it in"
            );
        }
    }

    /// Take what became of the invitations this session redeemed: the person lands in the workspace
    /// they joined once it is held here, or it is taken off the desk and the sheet says why.
    pub(crate) fn follow_enrollments(&self) {
        let Some(sync) = &self.sync else {
            return;
        };
        let mut settled = false;
        for enrolled in sync.take_enrolled() {
            let key = enrolled.workspace_id.to_string();
            let name = self
                .handle
                .read(|tree| {
                    tree.workspaces
                        .iter()
                        .find(|workspace| workspace.id() == enrolled.workspace_id)
                        .map(|workspace| tilo_desktop_surface::names::workspace(&workspace.name))
                })
                .unwrap_or_else(|| key.clone());
            // The name the site listed the workspace under, not the id the desk numbers it by: a
            // join that is refused says which workspace stayed away by the name the person chose it
            // under. A workspace of the person's own is named by the list it came from, which no
            // site lists.
            let listed = match &enrolled.name {
                Some(name) => name.clone(),
                None => self
                    .directory_store
                    .listed_name(&key)
                    .unwrap_or_else(|| name.clone()),
            };
            match &enrolled.result {
                Ok(()) => {
                    tracing::info!(
                        target: JOIN_FLOW_TARGET,
                        workspace = %key,
                        slot = enrolled.slot.0,
                        name = %listed,
                        member = enrolled.member,
                        outcome = "enrolled",
                        "join: the host admitted this device and the workspace is held here"
                    );
                    // The slot is made now, once the host admitted this
                    // device: the person lands in the workspace they joined,
                    // and the desk a refused join left is the desk it found.
                    // One of their own that their list brought from another
                    // device is put on the desk behind what they are doing.
                    let arrival = if enrolled.member {
                        Arrival::Listed
                    } else {
                        Arrival::Joined
                    };
                    self.open_joined_workspace(&key, &listed, arrival);
                    if !enrolled.member {
                        // The person is told the workspace is theirs now,
                        // where they are: the sheet closed as soon as they
                        // chose Join, and this is what became of it. A
                        // workspace of the person's own was on no sheet: it
                        // arrived on the desk where their list says it
                        // belongs.
                        crate::directory::joined_notice(&*self.telling(), &listed);
                    }
                }
                // A workspace this desk held stays on it as it was: the
                // device asks again when it next can.
                Err(error) if enrolled.held => {
                    tracing::warn!(
                        target: JOIN_FLOW_TARGET,
                        workspace = %key,
                        %error,
                        "the person's workspace did not take this copied machine in as its own device"
                    );
                    self.tell_awaited(enrolled.workspace_id, &listed);
                    if let Some(personal) = &self.personal {
                        personal.borrow_mut().ask_again_later(enrolled.workspace_id);
                    }
                }
                Err(error) => {
                    tracing::info!(
                        target: JOIN_FLOW_TARGET,
                        workspace = %key,
                        slot = enrolled.slot.0,
                        name = %listed,
                        %error,
                        outcome = "slot removed",
                        "join: the host refused the enrollment, so no workspace was joined and the person stays where they were"
                    );
                    if let Some(engine) = &self.engine {
                        engine.unbind(enrolled.slot);
                    }
                    // Only a desk that already held the workspace has a slot to take away: a join
                    // the host refused never had one made.
                    if let Some(held) = self.slot_of(&key) {
                        self.release_workspace(held);
                    }
                    // The sheet says why to a person who opens it again, and the banner says it
                    // where they are now.
                    tracing::warn!(%key, %error, "a workspace was not joined");
                    // A community whose host refused this device is not joined: the join is
                    // forgotten, and the community's own follow asks its door again when a network
                    // comes back.
                    self.directory_store.forget_community(&key);
                }
            }
            if enrolled.member {
                continue;
            }
            self.directory_store
                .enrolled(&key, &listed, &enrolled.result);
            settled = true;
        }
        let Some(seat) = &self.directory else {
            return;
        };
        if settled {
            // The sheet lists the directory as it now stands, a refusal included: its row says the
            // join did not happen and offers to join again, so the directory a person opens next is
            // not the one they left while the join was on its way.
            seat.show(
                self.directory_store
                    .view(&crate::platform::directory::held(&self.handle)),
            );
            if !self.directory_store.settling() {
                // The person lands in the workspace they joined: the directory has nothing left to
                // say.
                seat.presentation().close();
            }
        }
    }

    /// What the person's own records say: their workspaces as all their devices hold them, and what
    /// this device did since the last pass.
    pub(crate) fn follow_records(
        &self,
        hosted: Option<&dyn tilo_action_broker::services::HostedWorkspaces>,
    ) {
        self.follow_data_usage();
        self.follow_personal(hosted);
    }

    /// Count what this device did since the last pass into the person's records, through the verb
    /// of the service that holds them.
    fn follow_data_usage(&self) {
        self.table.follow(&mut self.services.borrow_mut());
    }

    /// Keep the person's own workspaces as all their devices hold them: list what this desk has,
    /// take what the others listed, open here what they made there, take off the desk what they
    /// took off theirs, and say again what this device has when theirs was missing something.
    pub(crate) fn follow_personal(
        &self,
        hosted: Option<&dyn tilo_action_broker::services::HostedWorkspaces>,
    ) {
        let Some(personal) = &self.personal else {
            return;
        };
        let news = {
            let mut personal = personal.borrow_mut();
            // Where this device holds each of its workspaces, as the workspaces themselves say: the
            // list names the device hosting a workspace, never the one that happens to hold it
            // here.
            personal.refresh_hosts(self.engine.as_deref());
            let mut news = personal.publish_local(&self.handle);
            news |= self.fleet_moved(&mut personal);
            // A device shown or kept off is a change this desk's pass did
            // not write. The list is sent whole, so this asks for one now;
            // a send that does not happen carries it the next time.
            news |= personal.untold;
            personal.untold = false;
            let mut behind = false;
            // A device that just came up has heard nothing of this device's workspaces yet, though
            // nothing changed here since they were last said. It may carry a newer place for a
            // workspace an entry is waiting on, so entries asked at an older one are asked again.
            let arrived = self
                .sync
                .as_ref()
                .is_some_and(|sync| sync.sibling_arrived());
            if let Some(sync) = &self.sync {
                let (taken, missing) = take_personal(&mut personal, sync);
                news |= taken;
                behind |= missing;
            }
            if arrived {
                personal.asked.clear();
                behind = true;
            }
            personal.ask_again_due();
            // A workspace whose governance holds no certificate of this device's is held as the
            // device this machine was copied from.
            let device = personal.device;
            let unknown_here = |workspace: WorkspaceId| {
                self.engine.as_ref().is_some_and(|engine| {
                    engine
                        .owner()
                        .with_current(workspace, |view| {
                            Ok(!view
                                .peer()
                                .certificates()
                                .iter()
                                .any(|cert| cert.device_public_key == device))
                        })
                        .unwrap_or(false)
                })
            };
            let adoption = personal.adopt_into(&self.handle, &unknown_here);
            let mut enter = adoption.enter;
            // A workspace on this desk the person's list does not carry (one
            // joined at someone's site) is entered as this device at the host
            // its own state names, as a listed one is.
            if let Some(engine) = &self.engine {
                for workspace in engine.workspaces() {
                    if enter.iter().any(|opened| opened.workspace == workspace)
                        || !unknown_here(workspace)
                    {
                        continue;
                    }
                    let Ok(hint) = engine
                        .owner()
                        .with_current(workspace, |view| view.transport_hint().cloned())
                    else {
                        continue;
                    };
                    if hint.host.device_public_key == device {
                        continue;
                    }
                    let name = self
                        .handle
                        .read(|tree| {
                            tree.workspaces
                                .iter()
                                .find(|held| held.id() == workspace)
                                .map(|held| held.name.clone())
                        })
                        .unwrap_or_default();
                    enter.push(ToEnter {
                        workspace,
                        name,
                        host: hint.host.device_public_key,
                        held: true,
                    });
                }
            }
            for opened in enter {
                self.enter_listed(&mut personal, &opened);
            }
            for workspace in adoption.release {
                self.release_workspace(workspace);
            }
            if (news || behind)
                && let Some(sync) = &self.sync
                && let Some(update) = personal.snapshot()
            {
                sync.tell_personal(&update);
            }
            self.keep_readable(&mut personal, hosted, news || behind);
            if adoption.changed {
                self.keep_desk_now();
            }
            news
        };
        // The list moved, so what an open sheet says of the person's devices may have too: a device
        // they linked, or a choice made on another one, arrives in the list alone.
        if news {
            self.ws_settings_fed.set(None);
            workspace_settings::refresh(
                self.workspace_settings.as_ref(),
                &self.workspace_settings_feed(),
            );
        }
    }

    /// Whether the fleet this device's root certifies moved since the last pass, which says the
    /// person's list again: the record keeps the keys with it, and a device paired here — a
    /// certificate the helper signed into the fleet, on a thread of its own — is in no record
    /// until the list is said. A fleet whose files stand still costs this pass two stamps.
    fn fleet_moved(&self, personal: &mut Personal) -> bool {
        let Some(engine) = &self.engine else {
            return false;
        };
        let stamps =
            crate::identity_admin::fleet_stamps(&engine.identity().device_cert.user_public_key);
        let moved = stamps != personal.fleet;
        personal.fleet = stamps;
        moved
    }

    /// Keep the person's list where their keys read it, when it moved: this device's door answers
    /// it to a key of theirs that dials it, and every dedicated host of a workspace held here keeps
    /// it for the keys that reach no device of theirs.
    fn keep_readable(
        &self,
        personal: &mut Personal,
        hosted: Option<&dyn tilo_action_broker::services::HostedWorkspaces>,
        moved: bool,
    ) {
        let owed = personal.kept.is_none() || (hosted.is_some() && !personal.kept_on_hosts);
        if !moved && !owed {
            return;
        }
        let hints: BTreeMap<String, Vec<u8>> = self
            .engine
            .as_ref()
            .map(|engine| engine.host_hints().into_iter().collect())
            .unwrap_or_default();
        let mut list = personal.readable(|workspace| {
            hints.get(workspace).cloned().or_else(|| {
                self.sync
                    .as_ref()?
                    .host_hint(workspace)
                    .and_then(|hint| postcard::to_stdvec(&hint).ok())
            })
        });
        // The keys the person's root certified on this device go with the
        // list, so the record every device of theirs reads holds them.
        if let Some(engine) = &self.engine {
            list.devices = crate::identity_admin::root_certificates(
                &engine.identity().device_cert.user_public_key,
            )
            .into_iter()
            .map(|cert| tilo_relay_session::workspaces::PersonalDevice {
                cert,
                revoked: false,
                browser: None,
                // What the keys' sessions on a host said of them is that
                // host's to keep, not this list's.
                agent: None,
                last_active_ms: None,
                online: false,
            })
            .collect();
        }
        if personal.kept.as_ref() == Some(&list) && (personal.kept_on_hosts || hosted.is_none()) {
            return;
        }
        self.remote.show_list(list.clone());
        if let (Some(engine), Some(hosted)) = (&self.engine, hosted) {
            crate::hosted_workspaces::keep_on_hosts(hosted, engine, &list);
        }
        personal.kept_on_hosts = hosted.is_some();
        personal.kept = Some(list);
    }

    /// Take one of the person's workspaces in that this device does not hold: the list says which
    /// device hosts it and the person's devices said where that host is reached, so this device
    /// asks the host to take it in as a further device of the person's
    /// ([`crate::sync::Client::enter`]). Nothing is invited here: a workspace that never arrives
    /// leaves the desk as it found it.
    fn enter_listed(&self, personal: &mut Personal, opened: &ToEnter) {
        let workspace = opened.workspace.to_string();
        let (Some(sync), Some(engine)) = (&self.sync, &self.engine) else {
            // Nothing here can take a workspace in: the desk shows it as this computer keeps it.
            if let Err(error) = self.add_workspace(&workspace, &opened.name, Arrival::Listed) {
                tracing::warn!(%error, %workspace, "a workspace of the person's was not opened here");
            }
            return;
        };
        // A workspace this desk holds already says itself where its host is
        // reached, should none of the person's devices have said it yet.
        let hint = sync.host_hint(&workspace).or_else(|| {
            opened
                .held
                .then(|| {
                    tilo_failure::ignored!(
                        "a held workspace whose state cannot be read has no hint, and the person's devices say one",
                        engine
                            .owner()
                            .with_current(opened.workspace, |view| view.transport_hint().cloned())
                            .ok()
                    )
                })
                .flatten()
        });
        let Some(entering) = Self::entry_to_ask(&personal.asked, opened, hint) else {
            return;
        };
        personal
            .asked
            .insert(opened.workspace, entering.host.clone());
        if !opened.held {
            engine.bind(entering.slot, entering.workspace_id);
        }
        tracing::info!(
            target: JOIN_FLOW_TARGET,
            %workspace,
            slot = entering.slot.0,
            host = %hex::encode(entering.host.host.device_public_key),
            "the person's workspace is asked to take this device in"
        );
        match sync.enter(entering) {
            Ok(()) => {}
            Err(error) => {
                // A workspace this desk held stays on it as it was.
                if !opened.held {
                    engine.unbind(tilo_desktop_model::WorkspaceSlot::of_workspace(
                        opened.workspace,
                    ));
                }
                tracing::warn!(%error, %workspace, "the person's workspace was not entered");
            }
        }
    }

    /// What to ask a workspace's host for, from what this device was told:
    /// where the person's devices said the host the list names is reached. A
    /// workspace whose host nobody said is reached waits, and one already
    /// asked at the same address is not asked again; a newer address for it,
    /// or a list naming another host, is a new ask.
    fn entry_to_ask(
        asked: &BTreeMap<WorkspaceId, tilo_peer_state::join::TransportHint>,
        opened: &ToEnter,
        hint: Option<tilo_peer_state::join::TransportHint>,
    ) -> Option<crate::sync::Entering> {
        let workspace = opened.workspace.to_string();
        let Some(hint) = hint else {
            tracing::debug!(
                %workspace,
                "the person's workspace waits for where its host is reached"
            );
            return None;
        };
        if opened.host != hint.host.device_public_key {
            tracing::debug!(
                %workspace,
                listed = %hex::encode(opened.host),
                hinted = %hex::encode(hint.host.device_public_key),
                "the person's workspace waits for the host its list names"
            );
            return None;
        }
        if asked.get(&opened.workspace) == Some(&hint) {
            return None;
        }
        Some(crate::sync::Entering {
            slot: tilo_desktop_model::WorkspaceSlot::of_workspace(opened.workspace),
            workspace_id: opened.workspace,
            name: opened.name.clone(),
            host: hint,
            held: opened.held,
        })
    }

    /// Take a workspace off this desk through the desk's own action, which refuses to leave the
    /// person with none.
    fn release_workspace(&self, workspace: WorkspaceId) {
        let Some((id, name, strips)) = self.slot_here(workspace) else {
            tracing::info!(
                target: JOIN_FLOW_TARGET,
                %workspace,
                outcome = "slot gone",
                "join: the workspace has no slot on this desk to take away"
            );
            return;
        };
        let args =
            serde_json::Map::from_iter([("workspace".to_owned(), serde_json::Value::from(id))]);
        match self.ask_of_desk("leave-workspace", &args) {
            Ok(()) => tracing::info!(
                target: JOIN_FLOW_TARGET,
                %workspace,
                name = %name,
                strips,
                "join: the workspace's slot is taken off this desk"
            ),
            Err(error) => {
                // The desk keeps its one workspace: the person is never left with none, and the
                // list brings it back on the next pass.
                tracing::debug!(%error, %workspace, "a workspace not shown here was not taken off the desk");
                tracing::info!(
                    target: JOIN_FLOW_TARGET,
                    %workspace,
                    name = %name,
                    %error,
                    outcome = "slot kept",
                    "join: the desk would not take the workspace's slot away"
                );
            }
        }
    }

    /// The workspace `workspace_id` on this desk, by the id it is shared under: what a join was
    /// granted, whose slot is made only once its host admits this device.
    fn slot_of(&self, workspace_id: &str) -> Option<WorkspaceId> {
        let wanted = WorkspaceId::parse(workspace_id)?;
        self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .map(Workspace::id)
                .find(|id| *id == wanted)
        })
    }

    /// The slot the workspace `workspace` stands in on this desk: its decimal slot id, the name it
    /// shows and how many strips it holds.
    fn slot_here(&self, workspace: WorkspaceId) -> Option<(String, String, usize)> {
        self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .find(|candidate| candidate.id() == workspace)
                .map(|held| {
                    (
                        held.slot.0.to_string(),
                        held.name.clone(),
                        held.strips.len(),
                    )
                })
        })
    }

    /// Open the workspace `workspace_id` on this desk under `name` , through the desk's own action
    /// for making one: the person lands in the workspace they joined ([`Arrival::Joined`]), one
    /// their list brought stands on the desk behind where they are ([`Arrival::Listed`]), and the
    /// desk keeps it from then on.
    fn open_joined_workspace(&self, workspace_id: &str, name: &str, arrival: Arrival) {
        if let Err(error) = self.add_workspace(workspace_id, name, arrival) {
            tracing::warn!(%error, %workspace_id, "a joined workspace was not opened");
            tracing::info!(
                target: JOIN_FLOW_TARGET,
                workspace = %workspace_id,
                name = %name,
                %error,
                outcome = "slot not made",
                "join: the host admitted this device but the desk would not make the workspace's slot"
            );
            self.directory_store.set_note(Some(error));
            return;
        }
        // A community whose home this workspace is is the person's from here on: no device of
        // theirs asks its door again — not to join it, and not to rejoin it once they left. Only
        // now, with the workspace on the desk: a community marked decided when its host refused the
        // enrollment was never joined, and nothing asked again.
        if let Some(site) = self.directory_store.community_of(workspace_id) {
            if let Some(personal) = &self.personal {
                personal
                    .borrow_mut()
                    .decide_community(&site, CommunityState::Decided);
            }
            self.directory_store.forget_community(workspace_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::{IdMinter, Workspace, WorkspaceSlot};
    use tilo_desktop_surface::seed_tree;

    /// The list as a later session opens it: a session flushes its writes before it ends, and
    /// opening here stands for that start.
    fn open(identity: tilo_network_actor::HostIdentity, state: Option<&Path>) -> Personal {
        written();
        Personal::open(identity, state)
    }

    /// Wait for the state writer to write what it was handed.
    fn written() {
        tilo_durable_writer::global().flush();
    }

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
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
        tilo_network_actor::HostIdentity::kept(cert, this)
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

    /// The workspaces on `handle` 's desk: their id and name, in order.
    fn on_desk(handle: &DesktopHandle) -> Vec<(WorkspaceId, String)> {
        handle.read(|tree| {
            tree.workspaces
                .iter()
                .map(|workspace| (workspace.id(), workspace.name.clone()))
                .collect()
        })
    }

    /// Put the workspace `workspace` on `handle` 's desk, as the desk's own action for making one
    /// does.
    fn open_here(handle: &DesktopHandle, workspace: WorkspaceId, name: &str) {
        let mut tree = handle.read(Clone::clone);
        tree.workspaces.push(Workspace::in_workspace(
            WorkspaceSlot::of_workspace(workspace),
            workspace,
            name,
        ));
        handle.replace_tree(tree);
    }

    /// The list's entry for the workspace `workspace` , if it names one.
    fn listed(personal: &Personal, workspace: WorkspaceId) -> Option<Listed> {
        entries_of(&personal.directory)
            .into_iter()
            .find(|(named, _)| *named == workspace)
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
        let [(id, name)] = desk_workspaces.as_slice() else {
            panic!("a fresh desk holds one workspace");
        };
        let (id, name) = (*id, name.clone());
        let identity = identity(0x11, 0x21, "The laptop");
        let device = identity.device_cert.device_public_key;
        let mut personal = open(identity.clone(), Some(state.path()));
        assert!(
            personal.publish_local(&desk),
            "the workspace the desk starts from is the person's own"
        );
        assert_eq!(
            listed(&personal, id),
            Some(Listed {
                name,
                host: Some(device),
            }),
            "the list names it, hosted by the device that made it"
        );

        // Kept beside the desk: the next session starts with the same list and has nothing to
        // write.
        written();
        let kept = state.path().join("tilo").join(FILE);
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&kept)
                .expect("the list is kept")
                .permissions(),
        );
        assert_eq!(mode & 0o777, 0o600, "the person's list is theirs alone");
        let mut reopened = open(identity, Some(state.path()));
        assert!(
            !reopened.take_unreadable(),
            "a list that reads back says nothing"
        );
        assert!(!reopened.publish_local(&desk), "nothing changed since");
        assert_eq!(
            reopened.agreed, personal.agreed,
            "the list survives the session"
        );
    }

    /// The list the person's keys read names every workspace of theirs in their order, with its
    /// host as the host's own signed address names it, and the ones of their own as theirs.
    #[test]
    fn the_list_a_key_reads_names_each_workspace_and_where_it_is_hosted() {
        let desk = desk();
        let [(first, _)] = on_desk(&desk)
            .try_into()
            .expect("a fresh desk holds one workspace");
        let second = WorkspaceId::new();
        open_here(&desk, second, "Second");
        let identity = identity(0x11, 0x21, "The laptop");
        let mut personal = open(identity.clone(), None);
        assert!(personal.publish_local(&desk));
        let workspace = first.to_string();
        let hint = tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1 {
            version: 1,
            mode: tilo_delivery_protocol::workspace_join::WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
            workspace_id: tilo_peer_state::join::room_workspace_id(&workspace).expect("an id"),
            room: workspace.clone(),
            endpoint_addr: iroh::EndpointAddr::new(
                iroh::EndpointId::from_bytes(&identity.device_cert.device_public_key)
                    .expect("a key"),
            ),
            host_user_public_key: identity.device_cert.user_public_key,
            host_device_certificate: identity.device_cert.clone(),
            packet_relay_url: "iroh:direct".into(),
        }
        .transport_hint(identity.signer.as_ref())
        .expect("a signed address");
        let hint = postcard::to_stdvec(&hint).expect("an address's bytes");
        let list = personal.readable(|asked| (asked == workspace).then(|| hint.clone()));
        let read: Vec<_> = list
            .workspaces
            .iter()
            .map(|entry| {
                (
                    entry.workspace.clone(),
                    entry.name.clone(),
                    entry.host.as_ref().map(|host| host.name.clone()),
                    entry.role.clone(),
                )
            })
            .collect();
        assert_eq!(
            read,
            [
                (
                    workspace.clone(),
                    "Desk".to_owned(),
                    Some("The laptop".to_owned()),
                    Some("owner".to_owned())
                ),
                (
                    second.to_string(),
                    "Second".to_owned(),
                    None,
                    Some("owner".to_owned())
                ),
            ],
            "the person's order, each named by the host that signed its address"
        );
        let address = list.workspaces[0]
            .host
            .as_ref()
            .and_then(|host| host.address().ok())
            .expect("the address reads back, signed");
        assert_eq!(address.room, workspace);
    }

    /// A list this computer kept but cannot read does not leave the person with an empty list and
    /// no word: they are told once, and what this computer kept is not written over.
    #[test]
    fn a_list_that_cannot_be_read_is_told_to_the_person() {
        let state = tempfile::tempdir().expect("a state directory");
        let kept = state.path().join("tilo").join(FILE);
        std::fs::create_dir_all(kept.parent().expect("the state's tilo directory"))
            .expect("the state directory");
        std::fs::write(&kept, b"not a workspace list").expect("a file this build cannot read");

        let mut personal = open(identity(0x11, 0x21, "The laptop"), Some(state.path()));
        assert!(
            personal.take_unreadable(),
            "the person is told their list could not be read"
        );
        assert!(!personal.take_unreadable(), "and told once");
        assert!(personal.agreed.is_empty(), "the list starts empty");
        assert_eq!(
            std::fs::read(&kept).expect("the list is still there"),
            b"not a workspace list",
            "what this computer kept is not written over before the list is rebuilt"
        );
    }

    #[test]
    fn a_workspace_made_on_another_device_opens_here_without_joining() {
        let a_state = tempfile::tempdir().expect("a state directory");
        let b_state = tempfile::tempdir().expect("a state directory");
        let a = desk();
        let a_identity = identity(0x11, 0x21, "The laptop");
        let a_device = a_identity.device_cert.device_public_key;
        let mut a_list = open(a_identity.clone(), Some(a_state.path()));
        a_list.publish_local(&a);
        let a_workspaces = on_desk(&a);
        let [(a_id, a_name)] = a_workspaces.as_slice() else {
            panic!("the other device holds one workspace");
        };
        let (a_id, a_name) = (*a_id, a_name.clone());
        let snapshot = a_list.snapshot().expect("the list is signed");

        let b = desk();
        let b_identity = identity(0x11, 0x22, "The desktop");
        let b_device = b_identity.device_cert.device_public_key;
        let mut b_list = open(b_identity, Some(b_state.path()));
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

        let adoption = b_list.adopt_into(&b, &|_| false);
        assert_eq!(
            adoption.enter,
            vec![ToEnter {
                workspace: a_id,
                name: a_name.clone(),
                host: a_device,
                held: false,
            }],
            "the workspace made on their other device opens here"
        );

        // What the session does with it: the desk makes the workspace.
        open_here(&b, a_id, &a_name);
        let again = b_list.adopt_into(&b, &|_| false);
        assert!(
            again.enter.is_empty(),
            "a workspace now here is not opened again"
        );
        // A machine copied from this one holds it on its desk as the device it was copied from: it
        // is entered as this device, at its host, and stays on the desk.
        let copied = b_list.adopt_into(&b, &|workspace| workspace == a_id);
        assert_eq!(
            copied.enter,
            vec![ToEnter {
                workspace: a_id,
                name: a_name,
                host: a_device,
                held: true,
            }]
        );
        assert!(copied.release.is_empty());
        assert!(on_desk(&b).iter().any(|(id, _)| *id == a_id));
        b_list.publish_local(&b);
        assert_eq!(
            listed(&b_list, a_id).map(|listed| listed.host),
            Some(Some(a_device)),
            "it stays hosted by the device that made it, not by this one"
        );
        assert_ne!(a_device, b_device, "two devices of the same person");
    }

    /// Where `identity` is reached for a workspace, as it signed it: `opaque` says which address.
    fn host_hint(
        identity: &tilo_network_actor::HostIdentity,
        opaque: &str,
    ) -> tilo_peer_state::join::TransportHint {
        tilo_peer_state::join::sign_transport_hint(
            tilo_peer_state::join::TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1,
            opaque,
            identity.device_cert.clone(),
            identity.device_public_key(),
            identity.signer.as_ref(),
        )
        .expect("a host signs its own address")
    }

    /// The workspace the person's other device made and hosts, as this device's list names it.
    #[test]
    fn a_workspace_hosted_on_another_device_is_listed_at_that_device() {
        let state = tempfile::tempdir().expect("a state directory");
        let desk = desk();
        let desk_workspaces = on_desk(&desk);
        let [(id, _)] = desk_workspaces.as_slice() else {
            panic!("a fresh desk holds one workspace");
        };
        let id = *id;
        let host = identity(0x11, 0x22, "The desktop");
        let host_device = host.device_cert.device_public_key;
        let mut personal = open(identity(0x11, 0x21, "The laptop"), Some(state.path()));
        // The workspace this desk holds is hosted by the person's other device: the list names that
        // device, never the one that holds it here.
        personal.hosts.insert(id, host_device);
        personal.publish_local(&desk);
        assert_eq!(
            listed(&personal, id).map(|listed| listed.host),
            Some(Some(host_device)),
            "a workspace that joined a host is listed at its host"
        );
    }

    #[test]
    fn a_workspace_joined_through_its_door_is_not_this_persons_to_list() {
        let state = tempfile::tempdir().expect("a state directory");
        let desk = desk();
        let desk_workspaces = on_desk(&desk);
        let [(own_id, _)] = desk_workspaces.as_slice() else {
            panic!("a fresh desk holds one workspace");
        };
        let own_id = *own_id;
        let joined =
            WorkspaceId::parse("1b7c4e08-3a5d-42f6-8c9e-2d6f0a1b3c5e").expect("a workspace id");
        open_here(&desk, joined, "Book club");
        let mut personal = open(identity(0x11, 0x21, "The laptop"), Some(state.path()));
        personal.joined(joined);
        personal.publish_local(&desk);
        assert!(listed(&personal, own_id).is_some(), "their own is listed");
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
        let other =
            WorkspaceId::parse("1b7c4e08-3a5d-42f6-8c9e-2d6f0a1b3c5e").expect("a workspace id");
        open_here(&desk, other, "Book club");
        let mut personal = open(identity.clone(), Some(state.path()));
        personal.publish_local(&desk);
        assert!(listed(&personal, other).is_some(), "both are listed");

        let mut tree = desk.read(Clone::clone);
        tree.workspaces.retain(|workspace| workspace.id() != other);
        desk.replace_tree(tree);
        assert!(personal.publish_local(&desk), "the list changed here");
        assert_eq!(
            listed(&personal, other),
            None,
            "a workspace taken off this desk is gone from the list"
        );
        let mut reopened = open(identity, Some(state.path()));
        assert!(!reopened.publish_local(&desk));
        assert_eq!(reopened.agreed.len(), 1, "and it is not brought back");
    }

    #[test]
    fn a_workspace_gone_from_the_list_is_taken_off_here() {
        let a_state = tempfile::tempdir().expect("a state directory");
        let b_state = tempfile::tempdir().expect("a state directory");
        let a = desk();
        let a_identity = identity(0x11, 0x21, "The laptop");
        let mut a_list = open(a_identity.clone(), Some(a_state.path()));
        let other =
            WorkspaceId::parse("1b7c4e08-3a5d-42f6-8c9e-2d6f0a1b3c5e").expect("a workspace id");
        open_here(&a, other, "Book club");
        a_list.publish_local(&a);

        let b = desk();
        let mut b_list = open(identity(0x11, 0x22, "The desktop"), Some(b_state.path()));
        b_list
            .merge(
                &a_list.snapshot().expect("the list is signed"),
                &a_identity.device_cert,
                tilo_clock::unix_now_ns(),
            )
            .expect("the person's list");
        let adoption = b_list.adopt_into(&b, &|_| false);
        for opened in &adoption.enter {
            open_here(&b, opened.workspace, &opened.name);
        }
        assert_eq!(
            on_desk(&b).len(),
            3,
            "this device's own and both of their other device's workspaces are here"
        );

        // Taken off the other device's desk, the list no longer names it.
        let mut tree = a.read(Clone::clone);
        tree.workspaces.retain(|workspace| workspace.id() != other);
        a.replace_tree(tree);
        a_list.publish_local(&a);
        b_list
            .merge(
                &a_list.snapshot().expect("the list is signed"),
                &a_identity.device_cert,
                tilo_clock::unix_now_ns(),
            )
            .expect("the person's list");
        let adoption = b_list.adopt_into(&b, &|_| false);
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
        let mut a_list = open(a_identity.clone(), Some(a_state.path()));
        a_list.publish_local(&a);
        let a_workspaces = on_desk(&a);
        let [(a_id, _)] = a_workspaces.as_slice() else {
            panic!("the other device holds one workspace");
        };
        let a_id = *a_id;

        let b = desk();
        let b_identity = identity(0x11, 0x22, "The desktop");
        let b_device = b_identity.device_cert.device_public_key;
        let mut b_list = open(b_identity.clone(), Some(b_state.path()));
        b_list.publish_local(&b);
        // Each device tells the other what it holds, as the sessions they keep between them do.
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
        for opened in b_list.adopt_into(&b, &|_| false).enter {
            open_here(&b, opened.workspace, &opened.name);
        }
        b_list.publish_local(&b);
        assert!(
            on_desk(&b).iter().any(|(id, _)| *id == a_id),
            "both devices hold the workspace"
        );

        // On the laptop, the person keeps the workspace off the desktop.
        assert!(
            a_list.set_shown(a_id, b_device, false),
            "the choice is a change to their list"
        );
        let choices = a_list.device_choices(a_id);
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
        let reopened = open(a_identity.clone(), Some(a_state.path()));
        assert_eq!(
            reopened.device_choices(a_id),
            choices,
            "and keeps showing it: the choice is in their list, kept beside the desk"
        );

        // The desktop learns of it and honours it: the workspace leaves its desk and is not opened
        // there again.
        b_list
            .merge(
                &a_list.snapshot().expect("the list is signed"),
                &a_identity.device_cert,
                tilo_clock::unix_now_ns(),
            )
            .expect("the person's list");
        let adoption = b_list.adopt_into(&b, &|_| false);
        assert_eq!(
            adoption.release,
            vec![a_id],
            "a workspace kept off this device is taken off its desk"
        );
        let mut tree = b.read(Clone::clone);
        tree.workspaces.retain(|workspace| workspace.id() != a_id);
        b.replace_tree(tree);
        assert!(
            b_list.adopt_into(&b, &|_| false).enter.is_empty(),
            "and a desk without it does not open it again"
        );
        assert!(
            on_desk(&a).iter().any(|(id, _)| *id == a_id),
            "the device it shows on still has it"
        );
    }

    #[test]
    fn a_device_never_named_shows_no_name() {
        let state = tempfile::tempdir().expect("a state directory");
        let a = desk();
        let a_identity = identity(0x11, 0x21, tilo_user_identity::UNNAMED_PRIMARY_DEVICE);
        let mut a_list = open(a_identity, Some(state.path()));
        a_list.publish_local(&a);
        let held = on_desk(&a);
        let [(a_id, _)] = held.as_slice() else {
            panic!("the desk holds one workspace");
        };
        let a_id = *a_id;

        let choices = a_list.device_choices(a_id);
        let here = choices.iter().find(|choice| choice.here).expect("its row");
        assert_eq!(
            here.label, "",
            "the stand-in provisioning fills in is no name of the person's"
        );
        assert!(
            a_list.directory.devices().is_empty(),
            "and it is no name to list this device by either"
        );
    }

    /// A workspace on the list this desk does not hold, hosted by `host` .
    fn to_enter(workspace: WorkspaceId, host: [u8; 32]) -> ToEnter {
        ToEnter {
            workspace,
            name: "Notes".into(),
            host,
            held: false,
        }
    }

    /// What this device asks the host its list names for, from what it was told: at the address the
    /// host signed, once.
    #[test]
    fn a_listed_workspace_is_asked_for_at_the_host_its_list_names() {
        let workspace =
            WorkspaceId::parse("1b7c4e08-3a5d-42f6-8c9e-2d6f0a1b3c5e").expect("a workspace id");
        let host = identity(0x11, 0x22, "The desktop");
        let hint = host_hint(&host, "the desktop's address");
        let opened = to_enter(workspace, host.device_cert.device_public_key);

        assert_eq!(
            crate::Shared::entry_to_ask(&BTreeMap::new(), &opened, None),
            None,
            "a workspace whose host nobody said is reached waits"
        );
        let entering = crate::Shared::entry_to_ask(&BTreeMap::new(), &opened, Some(hint.clone()))
            .expect("the host its list names is asked at its address");
        assert_eq!(entering.workspace_id, workspace);
        assert_eq!(entering.slot, WorkspaceSlot::of_workspace(workspace));
        assert_eq!(entering.name, "Notes");
        assert_eq!(entering.host, hint, "it is asked where its host is");

        let asked = BTreeMap::from([(workspace, hint.clone())]);
        assert_eq!(
            crate::Shared::entry_to_ask(&asked, &opened, Some(hint.clone())),
            None,
            "the same host at the same address is not asked again"
        );
        let moved = host_hint(&host, "the desktop's new address");
        assert!(
            crate::Shared::entry_to_ask(&asked, &opened, Some(moved)).is_some(),
            "a newer address for the host is a new ask"
        );
        assert_eq!(
            crate::Shared::entry_to_ask(
                &BTreeMap::new(),
                &to_enter(workspace, [0x33; 32]),
                Some(hint),
            ),
            None,
            "a list naming another host waits for the host it names"
        );
    }
}
