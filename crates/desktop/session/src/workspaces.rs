//! The workspace menu's host rows, the directory they open, the person's own
//! workspaces as all their devices hold them, and what the session says when
//! a workspace is left: the parts of workspaces the session itself cannot
//! answer, because they are the session's to know.
//!
//! A workspace's content — its strips, window groups and tabs — lives in the
//! workspace's collaboration and reaches every device that takes part in it.
//! What a device additionally needs is the *list*: which workspaces are this
//! person's, so that one made on the laptop appears on the board as well.
//! That list is the person's workspace directory
//! ([`WorkspaceDirectory`]), a small CRDT every device of theirs holds: one
//! resource of the person's own workspace, which carries it between their
//! devices as any other resource of theirs ([`Personal::sync_store`]).
//!
//! The whole session stays on the device. Besides the workspaces it records
//! what this device is looking at (the active workspace, which strip each
//! display shows, focus pointers) and workspaces that belong to other people,
//! none of which another device of the person should take over.
//!
//! Reconciling is a three-way merge against [`Personal::agreed`], the list
//! as both sides last had it: a workspace that appeared or was renamed here
//! since then is written to the list, and one that appeared or was renamed
//! in the list is brought onto this device. The same holds for a
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
use std::path::Path;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_desktop_surface::DesktopHandle;
use tilo_sheets::SheetState as _;
use tilo_startup_metrics::JOIN_FLOW_TARGET;
use tilo_user_identity::{WorkspaceDirectory, WorkspaceMeta};

use crate::directory;
use crate::platform::notice::Notices;
use crate::workspace_settings;

/// Say the person left the workspace `name` , and what became of its windows.
pub(crate) fn left_workspace(notices: &dyn Notices, name: &str) {
    crate::directory::workspace_notice(
        notices,
        (
            "session-left-workspace",
            Some("session-left-workspace-body"),
        ),
        &tilo_session_core::desktop::names::workspace(name),
    );
}

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
    /// On this session already, held as the device this machine was copied from: it is entered as this
    /// device without leaving the session.
    pub(crate) held: bool,
}

/// How a workspace came to this session, which says whether the person goes there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arrival {
    /// The person joined it here: they land in the workspace they joined.
    Joined,
    /// Their list brought it from another of their devices, with nobody here asking for it: it
    /// stands here and the person stays in the workspace they are in, so a window they open
    /// as it arrives is a window of the workspace they opened it in.
    Listed,
}

/// What bringing the list onto this device asks of the session.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Adoption {
    /// Whether the session changed.
    pub(crate) changed: bool,
    /// Workspaces to open here, at the device hosting them.
    pub(crate) enter: Vec<ToEnter>,
    /// Workspaces to take off this session: gone from the list, deleted on another device, or no
    /// longer shown on this one.
    pub(crate) release: Vec<WorkspaceId>,
}

/// The person's list of their own workspaces, as this device holds it and last agreed it with their
/// others.
pub(crate) struct Personal {
    /// This device's collaboration identity: its certificate names the device to the person's
    /// others.
    identity: tilo_network_actor::DeviceIdentity,
    directory: WorkspaceDirectory,
    /// The person's store's revision and this list's heads when the two were last brought together:
    /// while neither moved, there is nothing to take in or to write.
    synced: Option<(u64, Vec<automerge::ChangeHash>)>,
    /// This device, as the list's device registers name it.
    device: [u8; 32],
    /// The workspaces this device had at the last reconciliation.
    agreed: BTreeMap<WorkspaceId, Listed>,
    /// Their order at the last reconciliation.
    agreed_order: Vec<WorkspaceId>,
    /// Listed workspaces handed to the session to open, until their workspace is here: none of them is
    /// taken for one deleted here meanwhile.
    entering: BTreeSet<WorkspaceId>,
    /// Workspaces on this session that are someone else's, joined through their API: the person's
    /// other devices join them each on their own, so the list never names them.
    borrowed: BTreeSet<WorkspaceId>,
    /// Where each workspace this device holds is hosted, as this device's workspace itself says:
    /// the list names that device, never this one, so a device of the person's that opens the
    /// workspace from the list enters it at the device that hosts it.
    hosts: BTreeMap<WorkspaceId, [u8; 32]>,
    /// The entries asked for of the person's other devices, by workspace and the host hint they
    /// were asked at: one ask per hint, so a workspace whose host has not answered yet is not asked
    /// for again on every pass. A newer hint the person's devices say is a new ask.
    asked: BTreeMap<WorkspaceId, tilo_peer_state::join::TransportHint>,
    /// Where the list and `agreed` survive a restart; `None` keeps them in memory only.
    store: Option<crate::kept::Kept>,
    /// What the session offered the last time [`Self::publish_local`] ran to the end with everything
    /// written. While the session offers the same and neither the list nor `agreed` moved since,
    /// running it again would write nothing, so it is skipped.
    published: Option<PublishInputs>,
    /// Whether the list this computer kept could not be read, until the person is told: the list
    /// starts empty for it.
    unreadable: bool,
    /// The list as the person's keys read it, last kept on the hosts of their workspaces and handed
    /// to this device's API: kept again when it moves.
    kept: Option<tilo_server_protocol::workspaces::PersonalList>,
    /// Whether `kept` was handed to the hosts too: a session's broker reaches them only once it
    /// serves, so a list said before then is kept on them once it does.
    kept_on_hosts: bool,
    /// What the files this device's fleet is read from looked like when the list was last said:
    /// a fleet that moved since — a device this device's root just certified — is said again.
    pub(crate) fleet: Option<tilo_identity::device_fleet::FleetStamps>,
    /// The workspaces whose host has not taken this device in yet that the person was told of:
    /// once each, not on every ask that times out.
    told_awaiting: BTreeSet<WorkspaceId>,
    /// When a workspace whose host did not take this device in is asked again: a host that was
    /// away comes back, and nothing else would ask it again ([`Self::asked`] holds one ask per
    /// hint).
    ask_again: BTreeMap<WorkspaceId, std::time::Instant>,
    /// The workspace the shell started with on a device that kept none, while it is not known
    /// whether the person has workspaces of their own: it becomes their first only if they have
    /// none, and goes when they have some. Until then it is neither founded nor listed.
    seed: Option<WorkspaceId>,
    /// The parts the person takes in their workspaces, as governance named them when the list
    /// was last kept where their keys read it: a part that changed since says the list again.
    kept_roles: BTreeMap<WorkspaceId, String>,
}

/// How long after a host did not take this device in it is asked again.
const ASK_AGAIN_AFTER: std::time::Duration = std::time::Duration::from_secs(20);

/// The parts of a session [`Personal::publish_local`] reads.
#[derive(PartialEq, Eq)]
struct PublishInputs {
    listed: BTreeMap<WorkspaceId, Listed>,
    /// Every workspace of the session, in its order.
    order: Vec<WorkspaceId>,
}

impl PublishInputs {
    /// What the session hands the list: the person's own workspaces on it, in
    /// their order. A workspace joined from an API is not one of theirs.
    ///
    /// A workspace that was made on another device stays hosted by that one,
    /// so the devices holding it agree on where it lives; one this device
    /// made is hosted here.
    fn of(
        handle: &DesktopHandle,
        device: [u8; 32],
        borrowed: &BTreeSet<WorkspaceId>,
        seed: Option<WorkspaceId>,
        agreed: &BTreeMap<WorkspaceId, Listed>,
        hosts: &BTreeMap<WorkspaceId, [u8; 32]>,
    ) -> Self {
        handle.read(|tree| Self {
            listed: tree
                .workspaces
                .iter()
                .filter(|workspace| {
                    !workspace.id().is_nil()
                        && !borrowed.contains(&workspace.id())
                        && seed != Some(workspace.id())
                })
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

/// What is kept beside the session.
#[derive(Serialize, Deserialize)]
struct Stored {
    directory: Vec<u8>,
    agreed: BTreeMap<String, Listed>,
    #[serde(default)]
    agreed_order: Vec<String>,
    #[serde(default)]
    borrowed: Vec<String>,
}

/// The list's entries that carry a valid workspace, in the person's order.
fn entries_of(directory: &WorkspaceDirectory) -> Vec<(WorkspaceId, WorkspaceMeta)> {
    let mut out: Vec<(WorkspaceId, WorkspaceMeta)> = directory
        .list_workspaces()
        .into_iter()
        .filter_map(|entry| {
            let workspace = entry.id;
            (!workspace.is_nil()).then_some((workspace, entry))
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

/// The resource of the person's own workspace their list of workspaces is kept in.
pub(crate) const WORKSPACES_RESOURCE: (&str, &str) = ("so.tilo.identity", "identity.workspaces");

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

    /// The person's list as this device holds it, kept beside the session under `state` ; in memory
    /// only when there is no state directory.
    /// A file this build cannot read starts empty: the first pass lists this device's workspaces
    /// again, and the person's other devices answer with theirs.
    pub(crate) fn open(identity: tilo_network_actor::DeviceIdentity, state: Option<&Path>) -> Self {
        let user = identity.device_cert.user_public_key;
        let device = identity.device_cert.device_public_key;
        let store = state.map(|state| crate::kept::Kept::open(state, crate::kept::WORKSPACES));
        let mut personal = Self {
            identity,
            directory: WorkspaceDirectory::new(user, device),
            synced: None,
            device,
            agreed: BTreeMap::new(),
            agreed_order: Vec::new(),
            entering: BTreeSet::new(),
            borrowed: BTreeSet::new(),
            hosts: BTreeMap::new(),
            asked: BTreeMap::new(),
            store,
            published: None,
            unreadable: false,
            kept: None,
            kept_on_hosts: false,
            fleet: None,
            told_awaiting: BTreeSet::new(),
            ask_again: BTreeMap::new(),
            seed: None,
            kept_roles: BTreeMap::new(),
        };
        let read = personal
            .store
            .as_ref()
            .and_then(|store| match store.read() {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "the remembered workspaces could not be read");
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

    /// Hold `workspace`, the one the shell started with on a device that kept none, until it is
    /// known whether the person has workspaces of their own ([`Self::seed`]).
    pub(crate) fn hold_seed(&mut self, workspace: WorkspaceId) {
        if !workspace.is_nil() {
            self.seed = Some(workspace);
        }
    }

    /// Whether `workspace` is the person's own, which holds what is theirs alone (their devices,
    /// their settings, this list). This device holds it, but the shell never counts it as one to
    /// work in; the list never names it.
    fn is_own(&self, workspace: WorkspaceId) -> bool {
        workspace.to_string()
            == tilo_server_protocol::servers::person_workspace_id(
                &self.identity.device_cert.user_public_key,
            )
    }

    /// The workspace the shell started with, while it is not known whether the person has
    /// workspaces of their own: it is neither founded nor listed meanwhile.
    pub(crate) const fn seed(&self) -> Option<WorkspaceId> {
        self.seed
    }

    /// Whether the person's list names a workspace of theirs besides `except`: one this device or
    /// another of theirs made, or one a host of theirs lists.
    fn names_besides(&self, except: WorkspaceId) -> bool {
        entries_of(&self.directory)
            .iter()
            .any(|(workspace, _)| *workspace != except)
    }

    /// Settle the workspace the shell started with ([`Self::seed`]) once what the person has is
    /// known. A person with a workspace of their own anywhere — on their list, held on this device
    /// (`held`), on what their hosts list (`listed`), or one their governance names them the owner
    /// of (`owned`, which a workspace they joined is not otherwise) — has it taken off `handle`'s
    /// shell once another workspace stands there, never founded nor listed; a person with none has
    /// it as their first, founded and listed from then on. `listed` is `None` while a host has not
    /// answered, which leaves it waiting when nothing else says the person has one. Whether it
    /// settled.
    pub(crate) fn settle_seed(
        &mut self,
        handle: &DesktopHandle,
        held: &[WorkspaceId],
        listed: Option<&BTreeSet<WorkspaceId>>,
        owned: &BTreeSet<WorkspaceId>,
    ) -> bool {
        let Some(seed) = self.seed else {
            return false;
        };
        let theirs = |workspace: &WorkspaceId| {
            *workspace != seed
                && !self.is_own(*workspace)
                && (!self.borrowed.contains(workspace) || owned.contains(workspace))
        };
        let has = self.names_besides(seed)
            || held.iter().any(theirs)
            || match listed {
                None => return false,
                Some(listed) => listed.iter().any(theirs),
            };
        if has {
            let mut tree = handle.read(Clone::clone);
            // The shell always shows a workspace: this one stands until another does.
            if tree
                .workspaces
                .iter()
                .all(|workspace| workspace.id() == seed)
            {
                return false;
            }
            if tree.take_off(seed).is_some() {
                handle.replace_tree(tree);
            }
            tracing::info!(target: JOIN_FLOW_TARGET, %seed, "the person has workspaces: the one this shell started with goes");
        } else {
            tracing::info!(target: JOIN_FLOW_TARGET, %seed, "the person has no workspace: the one this shell started with is their first");
        }
        self.seed = None;
        self.published = None;
        true
    }

    /// `workspace` arrived from an API: it is the workspace of whoever let the person in, not one
    /// of their own, so their other devices are never told to open it themselves — each joins it
    /// through its own invitation.
    pub(crate) fn joined(&mut self, workspace: WorkspaceId) {
        if workspace.is_nil() || !self.borrowed.insert(workspace) {
            return;
        }
        self.published = None;
        self.persist();
    }

    /// The workspaces on this device that are someone else's, joined through their API:
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
            .or_else(|| {
                // One the list names a host of before this device agreed it: taken from a host's
                // list, it is entered there.
                entries_of(&self.directory)
                    .into_iter()
                    .find(|(listed, _)| *listed == workspace)
                    .and_then(|(_, entry)| entry.host_devices.first().copied())
            })
            .is_none_or(|host| host == self.device)
    }

    /// Write what changed on this device since the person's devices
    /// last agreed into the list: new and renamed workspaces, ones taken off
    /// the session here, and a new order. Returns whether the list changed,
    /// which is when the person's other devices need to hear of it.
    ///
    /// Called after every pass, so when neither the session nor the list moved
    /// since the last call it only compares the session with what it offered
    /// then.
    pub(crate) fn publish_local(&mut self, handle: &DesktopHandle) -> bool {
        let inputs = PublishInputs::of(
            handle,
            self.device,
            &self.borrowed,
            self.seed,
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
            let id = *workspace;
            let hosts: Vec<[u8; 32]> = entry.host.into_iter().collect();
            let written = self
                .directory
                .add_workspace(id, &entry.name)
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
        // A workspace agreed on and gone from this session was taken off it here.
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
            let _ = self.directory.remove_workspace(workspace);
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

    /// Give the workspaces this session holds positions in the list that match their order here, when
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
            if self.directory.set_order(*workspace, placed).is_ok() {
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
            .filter_map(|workspace| Some((workspace, engine.holder_of(workspace)?)))
            .collect();
    }

    /// Bring what changed in the list since the last agreement onto this device: the
    /// person's order, and which listed workspaces this device lets go (gone from the list, or no
    /// longer shown on this one) or opens at the device hosting them (listed, shown here, and not
    /// on this session).
    /// A listed workspace is entered only at a host the list names: a device must never guess where
    /// someone's workspace lives, so an entry naming none is left until one lists it. This device
    /// enters nothing at itself.
    /// A workspace on this session whose governance does not know this device (`unknown_here`) is one
    /// a machine copied from another holds as the device it was copied from: it is entered as this
    /// device, at the device hosting it, and stays on the shell meanwhile.
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
            // On this session already: nothing to enter, unless it is held as the device this machine
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
        // Both run on every pass: the list is written, and what the session offered read again, only
        // when one of the two moved.
        if adoption.changed || self.agreed != agreed {
            self.published = None;
            self.persist();
        }
        adoption
    }

    /// Put this session's workspaces in the person's order: the workspaces the list names, in its
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
    /// API is not theirs to show or keep off.
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
            .set_kept_off(workspace, device, !shown)
            .is_err()
        {
            return false;
        }
        // Writing the choice is not a pass over the shell: the list moved all the same.
        self.published = None;
        self.persist();
        true
    }

    /// Bring the list and the person's own workspace together: take in what their other devices
    /// wrote there, and write the list there when the workspace lacks something this device holds.
    /// The workspace carries it to their other devices. Returns whether another device's write
    /// moved this list.
    ///
    /// Cheap when neither the workspace nor the list moved since the last call, which every pass
    /// makes.
    pub(crate) fn sync_store(&mut self, store: &crate::PersonResources) -> bool {
        let revision = *store.changes().borrow();
        let heads = self.directory.heads();
        if self
            .synced
            .as_ref()
            .is_some_and(|(was, held)| *was == revision && *held == heads)
        {
            return false;
        }
        let held = store.document(WORKSPACES_RESOURCE);
        let taken = match held.as_deref().map(|bytes| self.directory.merge(bytes)) {
            Some(Ok(taken)) => taken,
            Some(Err(error)) => {
                tracing::warn!(%error, "the person's list of workspaces in their own workspace does not read");
                false
            }
            None => false,
        };
        if taken {
            self.published = None;
            self.persist();
        }
        let owed = match &held {
            Some(bytes) => self.directory.lacks(bytes),
            None => !self.directory.heads().is_empty(),
        };
        if owed {
            let bytes = self.directory.save();
            store.edit_document(WORKSPACES_RESOURCE, move |document| {
                let mut source =
                    automerge::AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
                tilo_automerge::ApplyIncoming::merge_incoming(document, &mut source)
                    .map_err(|error| error.to_string())?;
                Ok(())
            });
        }
        self.synced = Some((*store.changes().borrow(), self.directory.heads()));
        taken
    }

    /// Take in the person's workspaces their hosts list that this device holds no copy of
    /// (`listed`, by workspace): each one the list does not name yet is written to it, hosted by
    /// the device that signed where it is reached, and with that address, so this device enters it
    /// there as it enters every listed workspace ([`Self::adopt_into`]) and the person's other
    /// devices read it too. A workspace the list names already keeps its name and hosts. Whether
    /// the list moved.
    pub(crate) fn take_hosted(
        &mut self,
        listed: BTreeMap<WorkspaceId, crate::hosted_workspaces::Listed>,
    ) -> bool {
        if listed.is_empty() {
            return false;
        }
        let named: BTreeSet<WorkspaceId> = entries_of(&self.directory)
            .into_iter()
            .map(|(workspace, _)| workspace)
            .collect();
        let mut moved = false;
        for (workspace, entry) in listed {
            let host = entry.host.host.writes_for();
            let Ok(hint) = postcard::to_allocvec(&entry.host) else {
                continue;
            };
            let written = (|| {
                let mut changed = false;
                if !named.contains(&workspace) {
                    self.directory.add_workspace(workspace, &entry.name)?;
                    self.directory.assign_host(workspace, host)?;
                    changed = true;
                }
                // The address is the host's own word for where it answers: it is said under its key.
                changed |= self.directory.set_host_hint(workspace, host, &hint)?;
                Ok::<bool, tilo_user_identity::DirectoryError>(changed)
            })();
            match written {
                Ok(changed) => moved |= changed,
                Err(error) => {
                    tracing::warn!(%error, %workspace, "a workspace a host lists for the person was not listed here");
                }
            }
        }
        if moved {
            self.published = None;
            self.persist();
        }
        moved
    }

    /// Say where this device reaches the host of each listed workspace it holds, as each workspace's
    /// own state names it (`hints`, by workspace id): a device of the person's that does not hold one
    /// yet enters it there.
    pub(crate) fn say_hints(&mut self, hints: &[(String, Vec<u8>)]) {
        let listed: BTreeSet<WorkspaceId> = entries_of(&self.directory)
            .into_iter()
            .map(|(workspace, _)| workspace)
            .collect();
        let mut said = false;
        for (workspace, hint) in hints {
            let Some(workspace) = WorkspaceId::parse(workspace) else {
                continue;
            };
            if !listed.contains(&workspace) {
                continue;
            }
            match self.directory.set_host_hint(workspace, self.device, hint) {
                Ok(changed) => said |= changed,
                Err(error) => {
                    tracing::warn!(%error, %workspace, "where a workspace's host is reached was not listed");
                }
            }
        }
        if said {
            self.persist();
        }
    }

    /// Where `workspace` 's host is reached, as a device of the person's that holds it said in their
    /// list. `None` while none of them said it yet.
    pub(crate) fn host_hint(
        &self,
        workspace: WorkspaceId,
    ) -> Option<tilo_peer_state::join::TransportHint> {
        let bytes = self.directory.host_hint(workspace)?;
        match postcard::from_bytes(&bytes) {
            Ok(hint) => Some(hint),
            Err(error) => {
                tracing::warn!(%workspace, %error, "a device's host hint did not decode");
                None
            }
        }
    }

    /// The person's workspaces as their keys read them: every one the list carries, in the person's
    /// order, each with the signed address of its host: `hints` (by workspace id) where this device
    /// holds it, else where the person's devices said it is reached; and with the part the person
    /// takes in it as its governance names it (`roles`, by workspace), none where no governance here
    /// says yet.
    pub(crate) fn readable(
        &self,
        hints: &BTreeMap<String, Vec<u8>>,
        roles: &BTreeMap<WorkspaceId, String>,
    ) -> tilo_server_protocol::workspaces::PersonalList {
        let mut listed = self.directory.list_workspaces();
        listed.sort_by(|a, b| {
            a.order
                .unwrap_or(f64::INFINITY)
                .total_cmp(&b.order.unwrap_or(f64::INFINITY))
                .then(a.id.cmp(&b.id))
        });
        let workspaces = listed
            .into_iter()
            .map(|meta| {
                let workspace = meta.id;
                let host = hints
                    .get(&workspace.to_string())
                    .cloned()
                    .or_else(|| self.directory.host_hint(workspace))
                    .and_then(|hint| {
                        tilo_server_protocol::workspaces::KeepingDeviceAddress::of_hint(&hint)
                    });
                tilo_server_protocol::workspaces::PersonalWorkspace {
                    name: meta.name,
                    host,
                    role: roles.get(&workspace).cloned(),
                    workspace: workspace.to_string(),
                    labels: tilo_server_protocol::workspaces::WorkspaceLabels::new(),
                    unattended: false,
                    enter_at: None,
                    made_at_ms: None,
                }
            })
            .collect();
        tilo_server_protocol::workspaces::PersonalList { workspaces }
    }

    fn persist(&mut self) {
        let Some(store) = &self.store else {
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
        store.replace(bytes);
    }
}

/// Put `tree` 's workspaces in the order `wanted` names: those it names, in its order, then the
/// rest as they were. Whether they moved.
fn reorder(tree: &mut Layout, wanted: &[WorkspaceId]) -> bool {
    let before: Vec<WorkspaceId> = tree
        .workspaces
        .iter()
        .map(tilo_desktop_model::Workspace::id)
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
    let after: Vec<WorkspaceId> = arranged
        .iter()
        .map(tilo_desktop_model::Workspace::id)
        .collect();
    tree.workspaces = arranged;
    before != after
}

/// The part `person` takes in each workspace of `tree`, as the workspace's governance names it
/// there: `owner` for one of theirs, else the role they hold. A workspace whose governance names
/// nobody yet is missing: the person's list never guesses whose it is.
pub(crate) fn governed_roles(tree: &Layout, person: &str) -> BTreeMap<WorkspaceId, String> {
    tree.workspaces
        .iter()
        .filter_map(|workspace| {
            let role = workspace.role_of(person)?;
            Some((workspace.id(), role.key().to_owned()))
        })
        .collect()
}

/// The person's own workspaces as this computer can keep them: nothing when this computer knows no
/// identity to keep them for.
pub(crate) fn personal_for(state: Option<&Path>) -> Option<RefCell<Personal>> {
    crate::sync::device_identity().map(|identity| RefCell::new(Personal::open(identity, state)))
}

impl crate::Shared {
    /// Give every workspace on the shell that is nobody's yet the owner its governance names: the
    /// person at this device for their own — the one seeded here, one made here, or one made on
    /// another device of theirs — and, for a workspace joined through its API, the owner its
    /// host's governance has, which this device learns when the workspace comes here. A workspace
    /// whose owner is not known here keeps none, and never takes this person's: a host's workspace
    /// is not theirs to claim. One naming someone else keeps that name: ownership moves through the
    /// workspace's own governance, never here.
    /// Run after every pass, so a session this device kept from before its workspaces were anyone's is
    /// claimed the first time it is shown, and a workspace made here or joined here is claimed as
    /// it arrives. A session kept from before this device had an identity is theirs all the same: one
    /// naming them as the login account at it is claimed for who they are now. A pass with nothing
    /// to claim writes nothing.
    /// Who each workspace has in it follows the same rule: what the workspace's own governance here
    /// names is what the session shows, so a change another member's device made shows on this session.
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
        // The workspaces on the shell this device is founding or about to: an ask of one of them
        // waits for the founding to land and is delivered then, rather than refused for a
        // workspace this device is making ([`crate::mcp_server::SessionServices::hold_founding`]).
        let mut founding = BTreeSet::new();
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
                // A founding that failed is tried again when the next founding ends, not on every
                // pass: until then nothing waits on it, and an ask of it hears what the service
                // says.
                if !engine.founding_failed(workspace.id()) {
                    founding.insert(workspace.id().to_string());
                }
                // The workspace the shell started with is founded only once it is known to be the
                // person's first. Until then an ask of it waits all the same: it is founded the
                // moment it is known to be theirs, and taken off the shell otherwise.
                let settled = self
                    .personal
                    .as_ref()
                    .is_none_or(|personal| personal.borrow().seed() != Some(workspace.id()));
                if settled {
                    // A workspace made on this session is a workspace from here on,
                    // not only this session's: its governance is founded in the
                    // workspace it already has an identity for.
                    // It is signed by this device's signer, off the loop: the pass after it landed
                    // sees it held.
                    let (engine, id) = (std::sync::Arc::clone(engine), workspace.id());
                    // The name it is founded under is the one a person reads: a workspace nobody
                    // named keeps no name of its own, and a workspace is founded under a name, so
                    // the word the session reads for it stands in until one is given.
                    let name = tilo_session_core::desktop::names::workspace(&workspace.name);
                    let told = name.clone();
                    let founded =
                        crate::landing::off_loop(async move { engine.found(id, &name).await });
                    // A founding that failed is tried again when the next one ends, and the person
                    // hears why the workspace is not on the service yet: never a silent sole copy on
                    // this layout.
                    let sync = self.sync.clone();
                    self.landings.then(founded, {
                        let notifications = Rc::clone(&self.notifications);
                        move |outcome| match outcome {
                            Some(Err(error)) => notifications
                                .borrow()
                                .tell(&crate::notices::workspace_not_founded_notice(&told, &error)),
                            Some(Ok(())) => {
                                // The workspace is held here now, as one joined is: its live session
                                // opens, so this device says who is here in it and hears who else is.
                                if let Some(sync) = &sync {
                                    sync.workspace_arrived();
                                }
                            }
                            None => {}
                        }
                    });
                }
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
                tracing::debug!(
                    workspace = %workspace.id(),
                    borrowed = borrowed.contains(&workspace.id()),
                    "a workspace on the session is nobody's this pass: nothing may be done in it"
                );
            }
        }
        if changed {
            self.handle.replace_tree(tree);
        }
        self.session_services.keep_founding(founding);
        self.session_services.release_founding();
        self.project_governance();
    }

    /// Who the governance of a workspace names as its owner: its own, when this device holds it,
    /// else the person at this device for one this device is founding, which no governance has
    /// yet. A workspace founded before this device had an identity names the person by the id they
    /// went by then; it is theirs as the person they are now, which is what their own actions in it
    /// are decided as. `None` for every other workspace not held here yet — one joined through an
    /// API, or one the person's list names that this device is entering, which may be someone
    /// else's they were let in to: whose it is waits for its governance.
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
        let founding = self
            .personal
            .as_ref()
            .is_none_or(|personal| personal.borrow().made_here(workspace));
        (founding && !borrowed.contains(&workspace)).then(|| self.person.clone())
    }

    /// Open a listed workspace on this session, named `name` , through the session's own action for
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
        self.ask_of_shell("new-workspace", &args)
    }

    /// Give this computer the workspace a join chose, and come into it the way the join named:
    /// through its API, or with the invitation it holds. What the workspace held before this
    /// device arrived comes with it.
    /// A joined workspace the session does not already hold is admitted before it is put on the shell
    /// ([`Self::redeem_invitation`]): a host that refuses leaves the person where they were, with
    /// no empty workspace to arrive in and vanish from.
    pub(crate) fn add_joined_workspace(&self, joined: &directory::Joined) -> Result<(), String> {
        let Some(workspace) = WorkspaceId::parse(&joined.granted.workspace) else {
            return Err(tilo_session_core::desktop::i18n::t(
                "desktop-joined-workspace-unknown",
            ));
        };
        if matches!(joined.granted.way, tilo_workspace_directory::Way::Native(_)) {
            if self.sync.is_none() || self.engine.is_none() {
                return Err("This session cannot reach the invitation's workspace holder.".into());
            }
            if self.holds(workspace) {
                return Err("This workspace is already on this device.".into());
            }
        } else if let Some(personal) = &self.personal {
            personal.borrow_mut().joined(workspace);
        }
        self.redeem_invitation(joined);
        Ok(())
    }

    /// Come into the workspace a join chose, off the loop, through its API or
    /// with its invitation: this device becomes a member of the workspace and
    /// takes in its state. A
    /// session that holds no workspaces, or takes no part in the collaboration
    /// peer network,
    /// shows the workspace with what this computer itself keeps in it.
    ///
    /// A workspace the session does not already hold is put on it when the
    /// host admits this device ([`Self::follow_enrollments`]), not before: a
    /// host that refuses leaves the person where they were, and no empty
    /// workspace is ever shown to arrive in and vanish from.
    fn redeem_invitation(&self, joined: &directory::Joined) {
        let Some(workspace) = WorkspaceId::parse(&joined.granted.workspace) else {
            tracing::info!(
                target: JOIN_FLOW_TARGET,
                workspace = %joined.granted.workspace,
                outcome = "not joined",
                "join: the workspace id does not parse, so no invitation can be presented"
            );
            return;
        };
        let (Some(sync), Some(engine)) = (&self.sync, &self.engine) else {
            tracing::info!(
                target: JOIN_FLOW_TARGET,
                workspace = %joined.granted.workspace,
                held_locally = true,
                "join: this session holds no workspaces or takes no part in the peer network, so the workspace stays as this computer keeps it"
            );
            self.open_joined_workspace(&joined.granted.workspace, &joined.name, Arrival::Joined);
            return;
        };
        let on_shell = self.holds(workspace);
        engine.bind(workspace);
        tracing::info!(
            target: JOIN_FLOW_TARGET,
            workspace = %joined.granted.workspace,
            on_shell,
            "join: the workspace is made on the session"
        );
        let asked = sync.enroll(crate::sync::Enrolling {
            workspace_id: workspace,
            way: joined.granted.way.clone(),
            // A API's workspace learns the person by the name the session knows them by.
            name: crate::platform::directory::person()
                .map(|person| person.name)
                .unwrap_or_default(),
        });
        match asked {
            Ok(()) => {
                tracing::info!(
                    target: JOIN_FLOW_TARGET,
                    workspace = %joined.granted.workspace,
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
                    %error,
                    outcome = "removed",
                    "join: the way in could not be presented, so the workspace is taken off the session"
                );
                engine.unbind(workspace);
                if on_shell {
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

    /// The name the workspace's governance gives it, once this device holds the workspace: the one
    /// name every device and list of it reads, which the shell shows as its own. `None` while this
    /// device holds none of it, or it names none.
    pub(crate) fn governing_name(&self, workspace: WorkspaceId) -> Option<String> {
        let engine = self.engine.as_ref()?;
        let name = engine
            .owner()
            .with_current(workspace, |view| {
                Ok(view.peer().governance().workspace_name())
            })
            .ok()??;
        let name = name.trim();
        if name.is_empty() {
            None
        } else {
            Some(name.to_owned())
        }
    }

    /// Take what became of the invitations this session redeemed: the person lands in the workspace
    /// they joined once it is held here, or it is taken off the shell and the sheet says why.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass over every enrollment that settled since the last"
    )]
    pub(crate) fn follow_enrollments(&self) {
        let Some(sync) = &self.sync else {
            return;
        };
        let mut settled = false;
        for enrolled in sync.take_enrolled() {
            let key = enrolled.workspace_id.to_string();
            let named_on_shell = self.handle.read(|tree| {
                tree.workspaces
                    .iter()
                    .find(|workspace| workspace.id() == enrolled.workspace_id)
                    .map(|workspace| workspace.name.clone())
            });
            // The name the workspace goes by: the one its own governance gives it, which this
            // device holds the moment its host admits it — every device and list of the workspace
            // reads that one name, and the shell shows it. Until then the name the person's list
            // gave it, or the site listed it under, says which workspace is meant: a join that is
            // refused says which workspace stayed away the same way, and one nothing has named is
            // the shell's own word for a workspace, never the id it is numbered by.
            let listed = self
                .governing_name(enrolled.workspace_id)
                .or_else(|| match &enrolled.name {
                    Some(name) => Some(name.clone()),
                    None => self.directory_store.listed_name(&key),
                })
                .or(named_on_shell)
                .map_or_else(
                    || tilo_session_core::desktop::names::workspace(""),
                    |name| tilo_session_core::desktop::names::workspace(&name),
                );
            match &enrolled.result {
                Ok(()) => {
                    tracing::info!(
                        target: JOIN_FLOW_TARGET,
                        workspace = %key,
                        name = %listed,
                        member = enrolled.member,
                        outcome = "enrolled",
                        "join: the host admitted this device and the workspace is held here"
                    );
                    // A compact link's workspace id is only a routing hint until admission.
                    if !enrolled.member
                        && let Some(personal) = &self.personal
                    {
                        personal.borrow_mut().joined(enrolled.workspace_id);
                    }
                    // The workspace's live session opens now, not when a key of some workspace
                    // next moves here.
                    sync.workspace_arrived();
                    // The workspace is put on the shell now, once the host admitted this
                    // device: the person lands in the workspace they joined,
                    // and the session a refused join left is the session it found.
                    // One of their own that their list brought from another
                    // device is put on the shell behind what they are doing.
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
                        // arrived on the shell where their list says it
                        // belongs.
                        crate::directory::joined_notice(&*self.telling(), &listed);
                    }
                }
                // A workspace this session held stays on it as it was: the
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
                // This computer could not take the workspace in: nothing was
                // refused, so the workspace it made for the join stays and
                // the join is offered again.
                Err(error) if error.is_device_failure() => {
                    tracing::warn!(
                        target: JOIN_FLOW_TARGET,
                        workspace = %key,
                        name = %listed,
                        %error,
                        outcome = "deferred",
                        "join: this computer could not take the workspace in; the join stays for the next attempt"
                    );
                }
                Err(error) => {
                    tracing::info!(
                        target: JOIN_FLOW_TARGET,
                        workspace = %key,
                        name = %listed,
                        %error,
                        outcome = "removed",
                        "join: the host refused the enrollment, so no workspace was joined and the person stays where they were"
                    );
                    if let Some(engine) = &self.engine {
                        engine.unbind(enrolled.workspace_id);
                    }
                    // Only a session that already held the workspace has it to take away: a join the
                    // host refused never put one there.
                    if self.holds(enrolled.workspace_id) {
                        self.release_workspace(enrolled.workspace_id);
                    }
                    // The sheet says why to a person who opens it again, and the banner says it
                    // where they are now.
                    tracing::warn!(%key, %error, "a workspace was not joined");
                }
            }
            if enrolled.member {
                continue;
            }
            let said = enrolled
                .result
                .as_ref()
                .map_err(ToString::to_string)
                .copied();
            self.directory_store.enrolled(&key, &listed, &said);
            settled = true;
        }
        let Some(state) = &self.directory else {
            return;
        };
        if settled {
            // The sheet lists the directory as it now stands, a refusal included: its row says the
            // join did not happen and offers to join again, so the directory a person opens next is
            // not the one they left while the join was on its way.
            state.show(
                self.directory_store
                    .view(&crate::platform::directory::held(&self.handle)),
            );
            if !self.directory_store.settling() {
                // The person lands in the workspace they joined: the directory has nothing left to
                // say.
                state.presentation().close();
            }
        }
    }

    /// What the person's own records say: their workspaces as all their devices hold them, and what
    /// this device did since the last pass.
    pub(crate) fn follow_records(
        &self,
        hosted: Option<&crate::hosted_workspaces::HostedWorkspaces>,
    ) {
        self.follow_data_usage();
        self.follow_personal(hosted);
    }

    /// Count what this device did since the last pass into the person's records, through the verb
    /// of the service that holds them.
    fn follow_data_usage(&self) {
        self.table.follow(&mut self.services.borrow_mut());
    }

    /// The name the shell holds for `workspace`, or none when it holds none.
    fn held_name(&self, workspace: WorkspaceId) -> String {
        self.handle
            .read(|tree| {
                tree.workspaces
                    .iter()
                    .find(|held| held.id() == workspace)
                    .map(|held| held.name.clone())
            })
            .unwrap_or_default()
    }

    /// Keep the person's own workspaces as all their devices hold them: list what this session has,
    /// take what the others listed, open here what they made there, take off the shell what they
    /// took off theirs, and say again what this device has when theirs was missing something.
    pub(crate) fn follow_personal(
        &self,
        hosted: Option<&crate::hosted_workspaces::HostedWorkspaces>,
    ) {
        let Some(personal) = &self.personal else {
            return;
        };
        // The person's list as the hosts of their workspaces keep it: the workspaces of theirs a
        // host holds that no device of theirs listed here, as on a device restored from their
        // identity alone, which knows of them nowhere else.
        if let Some(hosted) = hosted {
            hosted.follow_lists(&self.wake_handle);
        }
        let news = {
            let mut personal = personal.borrow_mut();
            // Where this device holds each of its workspaces, as the workspaces themselves say: the
            // list names the device hosting a workspace, never the one that happens to hold it
            // here.
            personal.refresh_hosts(self.engine.as_deref());
            let store = self.sync.as_ref().and_then(|sync| sync.person_resources());
            // What the person's other devices wrote to the list is taken in before this session's
            // pass is written over it, so the pass reconciles against their list.
            let mut news = store
                .as_ref()
                .is_some_and(|store| personal.sync_store(store));
            if let Some(hosted) = hosted {
                news |= personal.take_hosted(hosted.take_listed());
            }
            news |= personal.publish_local(&self.handle);
            news |= self.fleet_moved(&mut personal);
            if let Some(engine) = &self.engine {
                personal.say_hints(&engine.host_hints());
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
            // A workspace on this session the person's list does not carry (one
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
                    if hint.host.is_of_device(&device) {
                        continue;
                    }
                    let name = self.held_name(workspace);
                    enter.push(ToEnter {
                        workspace,
                        name,
                        host: hint.host.writes_for(),
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
            // A workspace its host lists the person in no longer goes the same way: they left it,
            // or were taken out of it, and what it holds stays theirs to whom it belongs.
            if let Some(hosted) = hosted {
                for workspace in hosted.take_dropped() {
                    self.release_workspace(workspace);
                }
            }
            self.settle_seed(&mut personal, hosted);
            // What this pass wrote to the list goes to the person's own workspace, which carries it
            // to their other devices.
            if let Some(store) = &store {
                news |= personal.sync_store(store);
            }
            self.keep_readable(
                &mut personal,
                hosted.map(|hosted| {
                    hosted as &dyn tilo_workspace_service::apps::services::HostedWorkspaces
                }),
                news,
            );
            if adoption.changed {
                self.keep_layout_now();
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

    /// Settle the workspace the shell started with on a device that kept none
    /// ([`Personal::seed`]) against what the person has: what the hosts of the workspaces this
    /// device holds list for them, unless it is redeeming an invitation, which lists them once it
    /// is in. A join under way is the person having a workspace, so settling the seed before it
    /// lands would make the shell's own workspace theirs and list it to their other devices.
    fn settle_seed(
        &self,
        personal: &mut Personal,
        hosted: Option<&crate::hosted_workspaces::HostedWorkspaces>,
    ) {
        if personal.seed().is_none() {
            return;
        }
        let held = self
            .engine
            .as_ref()
            .map(|engine| engine.workspaces())
            .unwrap_or_default();
        let joining = self.directory_store.joining().is_some() || self.directory_store.settling();
        let listed = if joining {
            None
        } else {
            hosted.map_or_else(
                || Some(BTreeSet::new()),
                crate::hosted_workspaces::HostedWorkspaces::listed_everywhere,
            )
        };
        // A workspace whose governance names the person its owner is theirs, however it came to
        // this device: a computer restored from their words that joins their own workspace — their
        // site's — has a workspace already, and the shell's own is taken off rather than founded
        // beside it.
        let owned: BTreeSet<WorkspaceId> =
            self.engine.as_ref().map_or_else(BTreeSet::new, |engine| {
                held.iter()
                    .copied()
                    .filter(|workspace| {
                        engine.owner_of(&workspace.to_string()).is_ok_and(|owner| {
                            owner == self.person || self.earlier.contains(&owner)
                        })
                    })
                    .collect()
            });
        personal.settle_seed(&self.handle, &held, listed.as_ref(), &owned);
    }

    /// The part the person takes in each workspace on the shell ([`governed_roles`]).
    fn governed_roles(&self) -> BTreeMap<WorkspaceId, String> {
        self.handle.read(|tree| governed_roles(tree, &self.person))
    }

    /// Keep the person's list where their keys read it, when it moved: this device's API answers
    /// it to a key of theirs that dials it, and every dedicated host of a workspace held here keeps
    /// it for the keys that reach no device of theirs.
    fn keep_readable(
        &self,
        personal: &mut Personal,
        hosted: Option<&dyn tilo_workspace_service::apps::services::HostedWorkspaces>,
        moved: bool,
    ) {
        let owed = personal.kept.is_none() || (hosted.is_some() && !personal.kept_on_hosts);
        // A workspace's governance naming the person's part in it arrives after the workspace
        // does: the list says it again then.
        let roles = self.governed_roles();
        if !moved && !owed && roles == personal.kept_roles {
            return;
        }
        let hints: BTreeMap<String, Vec<u8>> = self
            .engine
            .as_ref()
            .map(|engine| engine.host_hints().into_iter().collect())
            .unwrap_or_default();
        let list = personal.readable(&hints, &roles);
        personal.kept_roles = roles;
        if personal.kept.as_ref() == Some(&list) && (personal.kept_on_hosts || hosted.is_none()) {
            return;
        }
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
    /// leaves the session as it found it.
    fn enter_listed(&self, personal: &mut Personal, opened: &ToEnter) {
        let workspace = opened.workspace.to_string();
        let (Some(sync), Some(engine)) = (&self.sync, &self.engine) else {
            // Nothing here can take a workspace in: the session shows it as this computer keeps it.
            if let Err(error) = self.add_workspace(&workspace, &opened.name, Arrival::Listed) {
                tracing::warn!(%error, %workspace, "a workspace of the person's was not opened here");
            }
            return;
        };
        // A workspace this session holds already says itself where its host is
        // reached, should none of the person's devices have said it yet.
        let hint = personal.host_hint(opened.workspace).or_else(|| {
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
            engine.bind(entering.workspace_id);
        }
        tracing::info!(
            target: JOIN_FLOW_TARGET,
            %workspace,
            host = %hex::encode(entering.host.host.device_public_key),
            "the person's workspace is asked to take this device in"
        );
        match sync.enter(entering) {
            Ok(()) => {}
            Err(error) => {
                // A workspace this session held stays on it as it was.
                if !opened.held {
                    engine.unbind(opened.workspace);
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
        // The address is signed by the service of the device the list names, under the service's own
        // key: it names that device all the same.
        if !hint.host.is_of_device(&opened.host) {
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
            workspace_id: opened.workspace,
            name: opened.name.clone(),
            host: hint,
            held: opened.held,
        })
    }

    /// Take a workspace off this session through the session's own action, which refuses to leave the
    /// person with none.
    fn release_workspace(&self, workspace: WorkspaceId) {
        let Some((name, strips)) = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .find(|held| held.id() == workspace)
                .map(|held| (held.name.clone(), held.strips.len()))
        }) else {
            tracing::info!(
                target: JOIN_FLOW_TARGET,
                %workspace,
                outcome = "gone",
                "join: the workspace is not on this session to take away"
            );
            return;
        };
        let args = serde_json::Map::from_iter([(
            "workspace".to_owned(),
            serde_json::Value::from(workspace.to_string()),
        )]);
        match self.ask_of_shell("leave-workspace", &args) {
            Ok(()) => tracing::info!(
                target: JOIN_FLOW_TARGET,
                %workspace,
                name = %name,
                strips,
                "join: the workspace is taken off this session"
            ),
            Err(error) => {
                // The session keeps its one workspace: the person is never left with none, and the
                // list brings it back on the next pass.
                tracing::debug!(%error, %workspace, "a workspace not shown here was not taken off the session");
                tracing::info!(
                    target: JOIN_FLOW_TARGET,
                    %workspace,
                    name = %name,
                    %error,
                    outcome = "kept",
                    "join: the session would not take the workspace away"
                );
            }
        }
    }

    /// Whether `workspace` is on this session.
    fn holds(&self, workspace: WorkspaceId) -> bool {
        self.handle
            .read(|tree| tree.workspaces.iter().any(|held| held.id() == workspace))
    }

    /// Open the workspace `workspace_id` on this session under `name` , through the session's own action
    /// for making one: the person lands in the workspace they joined ([`Arrival::Joined`]), one
    /// their list brought stands on the shell behind where they are ([`Arrival::Listed`]), and the
    /// session keeps it from then on.
    fn open_joined_workspace(&self, workspace_id: &str, name: &str, arrival: Arrival) {
        if let Err(error) = self.add_workspace(workspace_id, name, arrival) {
            tracing::warn!(%error, %workspace_id, "a joined workspace was not opened");
            tracing::info!(
                target: JOIN_FLOW_TARGET,
                workspace = %workspace_id,
                name = %name,
                %error,
                outcome = "not made",
                "join: the host admitted this device but the session would not make the workspace"
            );
            self.directory_store.set_note(Some(error));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::{IdMinter, Workspace, WorkspaceId};
    use tilo_session_core::desktop::seed_tree;
    use tilo_user_identity::DeviceCert;

    /// The list as a later session opens it.
    fn open(identity: tilo_network_actor::DeviceIdentity, state: Option<&Path>) -> Personal {
        Personal::open(identity, state)
    }

    /// The file the device's store keeps the list in, under `state`.
    fn kept_file(state: &Path) -> std::path::PathBuf {
        crate::kept::local_path(state, crate::kept::WORKSPACES)
    }

    /// A device of the person's, certified for their own root key.
    fn identity(user: u8, device: u8, label: &str) -> tilo_network_actor::DeviceIdentity {
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
        tilo_network_actor::DeviceIdentity::kept(cert, this)
    }

    /// The session a fresh device starts from.
    fn session() -> DesktopHandle {
        let mut minter = IdMinter::new(1);
        let (policy, _surface) = tilo_desktop_policy::DesktopPolicy::with_tree(
            seed_tree(&mut minter, "Session", Vec::new()),
            1,
        );
        policy.handle().clone()
    }

    /// The workspaces on `handle` 's session: their id and name, in order.
    fn on_shell(handle: &DesktopHandle) -> Vec<(WorkspaceId, String)> {
        handle.read(|tree| {
            tree.workspaces
                .iter()
                .map(|workspace| (workspace.id(), workspace.name.clone()))
                .collect()
        })
    }

    /// Put the workspace `workspace` on `handle` 's session, as the session's own action for making one
    /// does.
    fn open_here(handle: &DesktopHandle, workspace: WorkspaceId, name: &str) {
        let mut tree = handle.read(Clone::clone);
        tree.workspaces.push(Workspace::new(workspace, name));
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

    tilo_test_env::scenario! {
        fn the_persons_list_names_each_workspace_and_the_device_that_hosts_it;

        phase a_fresh_shells_workspace_is_listed_and_kept;
        {
            let state = tempfile::tempdir().expect("a state directory");
            let session = session();
            let workspaces = on_shell(&session);
            let [(id, name)] = workspaces.as_slice() else {
                panic!("a fresh session holds one workspace");
            };
            let (id, name) = (*id, name.clone());
            let identity = identity(0x11, 0x21, "The laptop");
            let device = identity.device_cert.device_public_key;
            let mut personal = open(identity.clone(), Some(state.path()));
            assert!(
                personal.publish_local(&session),
                "the workspace the session starts from is the person's own"
            );
            assert_eq!(
                listed(&personal, id),
                Some(Listed {
                    name,
                    host: Some(device),
                }),
                "the list names it, hosted by the device that made it"
            );

            // Kept beside the session: the next session starts with the same list and has nothing to
            // write.
            crate::kept::flush(Some(state.path()));
            let kept = kept_file(state.path());
            assert!(kept.is_file(), "the list is kept");
            let mode = std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(state.path().join("tilo").join("documents"))
                    .expect("the device's store")
                    .permissions(),
            );
            assert_eq!(mode & 0o777, 0o700, "the person's list is theirs alone");
            let mut reopened = open(identity, Some(state.path()));
            assert!(
                !reopened.take_unreadable(),
                "a list that reads back says nothing"
            );
            assert!(!reopened.publish_local(&session), "nothing changed since");
            assert_eq!(
                reopened.agreed, personal.agreed,
                "the list survives the session"
            );
        }

        phase the_list_a_key_reads_names_each_workspace_and_where_it_is_hosted;
        // The list the person's keys read names every workspace of theirs in their order, with its
        // host as the host's own signed address names it, and the part they take in it as its
        // governance names it: a workspace they were let in to is not listed as theirs.
        {
            let session = session();
            let [(first, _)] = on_shell(&session)
                .try_into()
                .expect("a fresh session holds one workspace");
            let second = WorkspaceId::new();
            open_here(&session, second, "Second");
            let identity = identity(0x11, 0x21, "The laptop");
            let mut personal = open(identity.clone(), None);
            assert!(personal.publish_local(&session));
            let workspace = first.to_string();
            let hint = tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1 {
                version: 1,
                holds_keys: true,
                runs_work: false,
                label: String::new(),
                cloud: false,
                origin: None,
                workspace_id: tilo_peer_state::join::WorkspaceId::parse(&workspace).expect("an id"),
                endpoint_addr: iroh::EndpointAddr::new(
                    iroh::EndpointId::from_bytes(&identity.device_cert.device_public_key)
                        .expect("a key"),
                ),
                user_public_key: identity.device_cert.user_public_key,
                device_certificate: identity.device_cert.clone(),
                packet_relay_url: "iroh:direct".into(),
            }
            .transport_hint(identity.signer.as_ref())
            .expect("a signed address");
            let hint = postcard::to_stdvec(&hint).expect("an address's bytes");
            let roles = BTreeMap::from([(first, "owner".to_owned()), (second, "editor".to_owned())]);
            let list = personal.readable(&BTreeMap::from([(workspace.clone(), hint)]), &roles);
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
                        workspace,
                        "Session".to_owned(),
                        Some("The laptop".to_owned()),
                        Some("owner".to_owned())
                    ),
                    (
                        second.to_string(),
                        "Second".to_owned(),
                        None,
                        Some("editor".to_owned())
                    ),
                ],
                "the person's order, each named by the host that signed its address"
            );
            let address = list.workspaces[0]
                .host
                .as_ref()
                .and_then(|host| host.address().ok())
                .expect("the address reads back, signed");
            assert_eq!(address.workspace_id, first);
        }

        phase a_list_that_cannot_be_read_is_told_to_the_person;
        // A list this computer kept but cannot read does not leave the person with an empty list and
        // no word: they are told once, and what this computer kept is not written over.
        {
            let state = tempfile::tempdir().expect("a state directory");
            let kept = kept_file(state.path());
            std::fs::create_dir_all(kept.parent().expect("the list's directory"))
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

        phase a_workspace_hosted_on_another_device_is_listed_at_that_device;
        // The workspace the person's other device made and hosts, as this device's list names it.
        {
            let state = tempfile::tempdir().expect("a state directory");
            let session = session();
            let workspaces = on_shell(&session);
            let [(id, _)] = workspaces.as_slice() else {
                panic!("a fresh session holds one workspace");
            };
            let id = *id;
            let host = identity(0x11, 0x22, "The desktop");
            let host_device = host.device_cert.device_public_key;
            let mut personal = open(identity(0x11, 0x21, "The laptop"), Some(state.path()));
            // The workspace this session holds is hosted by the person's other device: the list names that
            // device, never the one that holds it here.
            personal.hosts.insert(id, host_device);
            personal.publish_local(&session);
            assert_eq!(
                listed(&personal, id).map(|listed| listed.host),
                Some(Some(host_device)),
                "a workspace that joined a host is listed at its host"
            );
        }

        phase a_workspace_joined_through_its_api_is_not_this_persons_to_list;
        {
            let state = tempfile::tempdir().expect("a state directory");
            let session = session();
            let workspaces = on_shell(&session);
            let [(own_id, _)] = workspaces.as_slice() else {
                panic!("a fresh session holds one workspace");
            };
            let own_id = *own_id;
            let joined =
                WorkspaceId::parse("1b7c4e08-3a5d-42f6-8c9e-2d6f0a1b3c5e").expect("a workspace id");
            open_here(&session, joined, "Book club");
            let mut personal = open(identity(0x11, 0x21, "The laptop"), Some(state.path()));
            personal.joined(joined);
            personal.publish_local(&session);
            assert!(listed(&personal, own_id).is_some(), "their own is listed");
            assert_eq!(
                listed(&personal, joined),
                None,
                "a workspace someone else let them into is joined on each device, never listed"
            );
        }

        phase a_device_never_named_shows_no_name;
        {
            let state = tempfile::tempdir().expect("a state directory");
            let a = session();
            let a_identity = identity(0x11, 0x21, tilo_user_identity::UNNAMED_PRIMARY_DEVICE);
            let mut a_list = open(a_identity, Some(state.path()));
            a_list.publish_local(&a);
            let held = on_shell(&a);
            let [(a_id, _)] = held.as_slice() else {
                panic!("the session holds one workspace");
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

        phase a_listed_workspace_is_asked_for_at_the_host_its_list_names;
        // What this device asks the host its list names for, from what it was told: at the address the
        // host signed, once.
        {
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

    /// Where `identity` is reached for a workspace, as it signed it: `opaque` says which address.
    fn host_hint(
        identity: &tilo_network_actor::DeviceIdentity,
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

    /// A workspace on the list this session does not hold, hosted by `host` .
    fn to_enter(workspace: WorkspaceId, host: [u8; 32]) -> ToEnter {
        ToEnter {
            workspace,
            name: "Notes".into(),
            host,
            held: false,
        }
    }
}
