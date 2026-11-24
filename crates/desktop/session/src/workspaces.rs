//! The workspace menu's host rows, the directory they open, the person's own
//! workspaces as all their devices list them, and what the session says when
//! a workspace is left: the parts of workspaces the session itself cannot
//! answer, because they are the session's to know.
//!
//! A workspace's content — its strips, window groups and tabs — lives in the
//! workspace's collaboration and reaches every device that takes part in it.
//! What a device additionally needs is the *list*: which workspaces are this
//! person's, so that one made on the laptop appears on the board as well.
//! That list is a resource of the person's own workspace
//! ([`WorkspaceList`]), which the session follows at the device's daemon and
//! changes only through its typed actions; it keeps no copy of it.
//!
//! Each pass compares the shell with the shell as the last pass left it: a
//! workspace made, renamed, moved or taken off here since is written to the
//! list. What the list says is then brought onto the shell: a workspace it
//! names that this device does not hold is entered at the device hosting it
//! ([`ListedWorkspace::host`]), one taken off the list or kept off this
//! device is let go, and the shell takes the list's order. A workspace the
//! list was never told of — one joined through someone's invitation, which
//! each device of the person joins on its own — stays as it is. Nothing asks
//! the devices on a timer.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_resource::Invoke;
use tilo_session_core::desktop::state::DesktopHandle;
use tilo_sheets::SheetState as _;
use tilo_startup_metrics::JOIN_FLOW_TARGET;
use tilo_user_identity::{ListedWorkspace, WorkspaceList};
use tilo_workspace_service::workspaces::person_list;
use tilo_workspace_service::workspaces::person_workspace::devices::{DEVICES, Devices, KeyKind};

use crate::directory;
use crate::followed_resource::FollowedResource;
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
    /// Workspaces to take off this session: taken off the list, or no longer shown on this one.
    pub(crate) release: Vec<WorkspaceId>,
}

/// A write of the person's list.
pub(crate) type ListWrite = Invoke<WorkspaceList>;

/// The person's list of their own workspaces as this session follows it, and what this session
/// did with it.
pub(crate) struct Personal {
    /// This device's collaboration identity: its certificate names the device to the person's
    /// others.
    identity: tilo_network_actor::DeviceIdentity,
    /// This device, as the list names it.
    device: [u8; 32],
    /// The list, followed at the device's daemon: `None` in a session that reaches none.
    list: Option<FollowedResource<WorkspaceList>>,
    /// The shell's workspaces, by id with their names in its order, as the last pass left them:
    /// what the next pass compares the shell with to learn what the person did here since. `None`
    /// until the first pass, which takes the shell as it found it.
    passed: Option<Vec<(WorkspaceId, String)>>,
    /// Listed workspaces handed to the session to open, until their workspace is here: none of them is
    /// taken for one deleted here meanwhile.
    entering: BTreeSet<WorkspaceId>,
    /// Workspaces joined on this session through someone's invitation while it runs: the person's
    /// other devices join them each on their own, so the list is never told of them.
    borrowed: BTreeSet<WorkspaceId>,
    /// Where each workspace this device holds is hosted, as this device's workspace itself says:
    /// the list names that device, never this one, so a device of the person's that opens the
    /// workspace from the list enters it at the device that hosts it.
    hosts: BTreeMap<WorkspaceId, [u8; 32]>,
    /// The entries asked for of the person's other devices, by workspace and the host hint they
    /// were asked at: one ask per hint, so a workspace whose host has not answered yet is not asked
    /// for again on every pass. A newer hint the person's devices say is a new ask.
    asked: BTreeMap<WorkspaceId, tilo_peer_state::join::TransportHint>,
    /// The person's devices as their own workspace names them ([`DEVICES`]), followed beside the
    /// list: what a workspace's settings offer to show it on.
    devices: Option<FollowedResource<Devices>>,
    /// The readings of the list and of the devices the last pass saw: one that moved since is news
    /// to an open sheet.
    seen: (u64, u64),
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
    /// The workspaces placed on a host whose roster, as the device's daemon reads it, named this
    /// device: one whose roster names it no more is one the person left, or was taken out of.
    in_hosted: BTreeSet<WorkspaceId>,
}

/// One of the person's workspaces on a host that the device's daemon holds and this session holds
/// no copy of.
pub(crate) struct OnHost {
    /// Its name, as the daemon lists it.
    pub(crate) name: String,
    /// Where its host is reached, as the host signed it: where this session asks to be taken in.
    pub(crate) host: tilo_peer_state::join::TransportHint,
}

/// The person's workspaces on a host other than `device` that the daemon `view` reaches lists
/// and this session does not hold (`held`), each with the host's signed address.
pub(crate) fn on_hosts(
    view: &crate::workspace_view::WorkspaceView,
    device: &[u8; 32],
    held: &BTreeSet<WorkspaceId>,
) -> BTreeMap<WorkspaceId, OnHost> {
    view.listed()
        .into_iter()
        .filter_map(|listed| {
            let workspace = WorkspaceId::parse(&listed.id)?;
            if held.contains(&workspace) {
                return None;
            }
            let host = tilo_workspace_service::workspaces::standing::HostElsewhere::read(
                listed.host.as_deref()?,
                device,
            )?;
            Some((
                workspace,
                OnHost {
                    name: listed.name,
                    host: host.hint,
                },
            ))
        })
        .collect()
}

/// Every workspace of the person's the device's daemon holds, as `view` reads its list; `None`
/// while it lists none, when what the person has is not known yet: the daemon enters the
/// workspaces their server lists as soon as it hears the list.
fn listed_by_daemon(view: &crate::workspace_view::WorkspaceView) -> Option<BTreeSet<WorkspaceId>> {
    let listed: BTreeSet<WorkspaceId> = view
        .listed()
        .iter()
        .filter_map(|listed| WorkspaceId::parse(&listed.id))
        .collect();
    (!listed.is_empty()).then_some(listed)
}

/// How long after a host did not take this device in it is asked again.
const ASK_AGAIN_AFTER: std::time::Duration = std::time::Duration::from_secs(20);

/// The shell's workspaces, by id with their names, in its order.
fn shown(handle: &DesktopHandle) -> Vec<(WorkspaceId, String)> {
    handle.read(|tree| {
        tree.workspaces
            .iter()
            .map(|workspace| (workspace.id(), workspace.name.clone()))
            .collect()
    })
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

    /// The person's list as this device reads it, read nowhere yet.
    pub(crate) fn open(identity: tilo_network_actor::DeviceIdentity) -> Self {
        let device = identity.device_cert.device_public_key;
        Self {
            identity,
            device,
            list: None,
            passed: None,
            entering: BTreeSet::new(),
            borrowed: BTreeSet::new(),
            hosts: BTreeMap::new(),
            asked: BTreeMap::new(),
            devices: None,
            seen: (0, 0),
            told_awaiting: BTreeSet::new(),
            ask_again: BTreeMap::new(),
            seed: None,
            in_hosted: BTreeSet::new(),
        }
    }

    /// Follow the list in the person's own workspace at `daemon` from now on, waking the session
    /// through `wake` each time it moves.
    pub(crate) fn follow(
        &mut self,
        daemon: &tilo_session_client::daemon_documents::DaemonShell,
        wake: tilo_service::Wake,
    ) {
        let person = self.identity.device_cert.user_public_key;
        let own = tilo_server_protocol::servers::person_workspace_id(&person);
        let member = tilo_action_policy::PersonId::from_root(person);
        self.list = Some(FollowedResource::follow(
            daemon,
            (
                &own,
                tilo_user_identity::workspace_list::APP,
                tilo_user_identity::workspace_list::RESOURCE,
            ),
            member.as_str(),
            None,
            std::sync::Arc::clone(&wake),
        ));
        self.devices = Some(FollowedResource::follow(
            daemon,
            (&own, DEVICES.0, DEVICES.1),
            member.as_str(),
            None,
            wake,
        ));
    }

    /// The list as last read: `None` while it was not read yet, or nothing reads it.
    pub(crate) fn read(&self) -> Option<Rc<WorkspaceList>> {
        let list = self.list.as_ref()?;
        list.held()?;
        Some(list.get())
    }

    /// Whether the list or the person's devices moved since the last time this was asked.
    fn moved(&mut self) -> bool {
        let revision = (
            self.list.as_ref().map_or(0, FollowedResource::revision),
            self.devices.as_ref().map_or(0, FollowedResource::revision),
        );
        let moved = revision != self.seen;
        self.seen = revision;
        moved
    }

    /// Make `writes` at the daemon, in order. Whether every one was made: one that was not is
    /// made again by a later pass, which finds the list still lacking it.
    fn write(&self, writes: &[ListWrite]) -> bool {
        let Some(list) = &self.list else {
            return writes.is_empty();
        };
        writes.iter().all(|write| match list.invoke(write) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, action = write.id, "the person's list was not changed");
                false
            }
        })
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

    /// Settle the workspace the shell started with ([`Self::seed`]) once what the person has is
    /// known. A person with a workspace of their own anywhere — on their `list`, held on this
    /// device (`held`), on what their hosts list (`listed`), or one their governance names them
    /// the owner of (`owned`, which a workspace they joined is not otherwise) — has it taken off
    /// `handle`'s shell once another workspace stands there, never founded nor listed; a person
    /// with none has it as their first, founded and listed from then on. `listed` is `None` while
    /// a host has not answered, which leaves it waiting when nothing else says the person has one.
    /// Whether it settled.
    pub(crate) fn settle_seed(
        &mut self,
        handle: &DesktopHandle,
        list: &WorkspaceList,
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
        let has = list.listed().any(|(workspace, _)| workspace != seed)
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
        true
    }

    /// `workspace` was joined here through someone's invitation: it is the workspace of whoever
    /// let the person in, not one of their own, so their other devices are never told to open it
    /// themselves — each joins it through its own invitation.
    pub(crate) fn joined(&mut self, workspace: WorkspaceId) {
        if !workspace.is_nil() {
            self.borrowed.insert(workspace);
        }
    }

    /// The workspaces joined on this session through someone's invitation while it runs: whose
    /// workspace they are is their host's governance's to say, not this person's.
    pub(crate) fn borrowed(&self) -> &BTreeSet<WorkspaceId> {
        &self.borrowed
    }

    /// Whether this device is the one that made `workspace` : it hosts it, or the list names this
    /// device as its host, or names no host for it. A workspace another device of the person's
    /// made is hosted there, and one this device is entering from there is not its to make:
    /// neither is founded here.
    pub(crate) fn made_here(&self, workspace: WorkspaceId) -> bool {
        if self.entering.contains(&workspace) {
            return false;
        }
        self.hosts
            .get(&workspace)
            .copied()
            .or_else(|| {
                self.read()
                    .and_then(|list| list.get(workspace).and_then(ListedWorkspace::host))
            })
            .is_none_or(|host| host == self.device)
    }

    /// What changed on this shell since the last pass, as writes of `list`: new and renamed
    /// workspaces, ones taken off the shell here, and
    /// a new order. A workspace the list does not name is listed when it appeared here since the
    /// last pass, or this device hosts it or nothing does yet (it is being made here): a workspace
    /// taken off the list elsewhere, or joined through an invitation, is neither.
    pub(crate) fn publish_local(
        &self,
        handle: &DesktopHandle,
        list: &WorkspaceList,
    ) -> Vec<ListWrite> {
        let now = shown(handle);
        let before = self.passed.as_ref().unwrap_or(&now);
        let mut writes = Vec::new();
        for (workspace, name) in &now {
            let id = *workspace;
            if id.is_nil()
                || self.borrowed.contains(&id)
                || self.seed == Some(id)
                || self.is_own(id)
            {
                continue;
            }
            let held_host = self.hosts.get(&id).copied();
            match list.get(id) {
                Some(listed) => {
                    if !name.trim().is_empty() && listed.name != *name {
                        writes.extend(person_list::listing(&id.to_string(), name, None));
                    }
                    if let Some(host) = held_host
                        && listed.host() != Some(host)
                    {
                        writes.push(person_list::hosting(&id.to_string(), host));
                    }
                }
                None => {
                    let new_here = !before.iter().any(|(was, _)| *was == id);
                    let gone_elsewhere = list.went(id) && !new_here;
                    let made_here = held_host.is_none_or(|host| host == self.device);
                    if gone_elsewhere || !(new_here || made_here) {
                        continue;
                    }
                    let host = held_host.unwrap_or(self.device);
                    writes.extend(person_list::listing(
                        &id.to_string(),
                        name,
                        Some(&person_list::KeptAt {
                            device: host,
                            hint: None,
                        }),
                    ));
                }
            }
        }
        // A listed workspace this shell showed and shows no more was taken off it here.
        for (workspace, _) in before {
            if now.iter().any(|(is, _)| is == workspace) || self.entering.contains(workspace) {
                continue;
            }
            if list
                .get(*workspace)
                .is_some_and(|listed| listed.shows_on(&self.device))
            {
                writes.push(person_list::unlisting(&workspace.to_string()));
            }
        }
        writes.extend(Self::publish_order(list, before, &now));
        writes
    }

    /// Put the listed workspaces in the order this shell shows them, when the person moved them
    /// here since the last pass: each one out of place goes right after the one before it here, so
    /// a concurrent move of another workspace on another device still lands. Only workspaces the
    /// shell showed before and shows still say how they were moved: one arriving or going is no
    /// move.
    fn publish_order(
        list: &WorkspaceList,
        before: &[(WorkspaceId, String)],
        now: &[(WorkspaceId, String)],
    ) -> Vec<ListWrite> {
        let kept = |shell: &[(WorkspaceId, String)], other: &[(WorkspaceId, String)]| {
            shell
                .iter()
                .map(|(workspace, _)| *workspace)
                .filter(|workspace| {
                    list.get(*workspace).is_some() && other.iter().any(|(is, _)| is == workspace)
                })
                .collect::<Vec<WorkspaceId>>()
        };
        let here = kept(now, before);
        if here == kept(before, now) {
            return Vec::new();
        }
        // The list's order, as the writes so far leave it.
        let mut order: Vec<WorkspaceId> = list.listed().map(|(workspace, _)| workspace).collect();
        let mut writes = Vec::new();
        for pair in here.windows(2) {
            let [previous, workspace] = pair else {
                continue;
            };
            let at = |order: &[WorkspaceId], which: &WorkspaceId| {
                order.iter().position(|listed| listed == which)
            };
            let (Some(after), Some(from)) = (at(&order, previous), at(&order, workspace)) else {
                continue;
            };
            if from < after {
                order.remove(from);
                order.insert(after, *workspace);
                writes.push(
                    WorkspaceList::PLACE_WORKSPACE
                        .with((workspace.to_string(), previous.to_string())),
                );
            }
        }
        writes
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

    /// Bring `list` onto this device: the person's order, and which listed workspaces this device
    /// lets go (taken off the list, or no longer shown on this one) or opens at the device hosting
    /// them (listed, shown here, and not on this session).
    /// A listed workspace is entered only at a host the list names: a device must never guess where
    /// someone's workspace lives, so an entry naming none is left until one lists it. This device
    /// enters nothing at itself.
    /// A workspace on this session whose governance does not know this device (`unknown_here`) is one
    /// a machine copied from another holds as the device it was copied from: it is entered as this
    /// device, at the device hosting it, and stays on the shell meanwhile.
    pub(crate) fn adopt_into(
        &mut self,
        handle: &DesktopHandle,
        list: &WorkspaceList,
        unknown_here: &dyn Fn(WorkspaceId) -> bool,
    ) -> Adoption {
        let mut adoption = Adoption::default();
        let present: Vec<WorkspaceId> =
            handle.read(|tree| tree.workspaces.iter().map(Workspace::id).collect());
        let mut shown_here: Vec<WorkspaceId> = Vec::new();
        for (workspace, listed) in list.listed() {
            if !listed.shows_on(&self.device) {
                if present.contains(&workspace) {
                    adoption.release.push(workspace);
                }
                self.entering.remove(&workspace);
                continue;
            }
            shown_here.push(workspace);
            let host = listed.host().filter(|host| *host != self.device);
            // On this session already: nothing to enter, unless it is held as the device this machine
            // was copied from.
            if present.contains(&workspace) {
                if let Some(host) = host
                    && unknown_here(workspace)
                {
                    adoption.enter.push(ToEnter {
                        workspace,
                        name: listed.name.clone(),
                        host,
                        held: true,
                    });
                }
                continue;
            }
            let Some(host) = host else {
                continue;
            };
            adoption.enter.push(ToEnter {
                workspace,
                name: listed.name.clone(),
                host,
                held: false,
            });
            self.entering.insert(workspace);
        }
        for workspace in &present {
            if list.went(*workspace) && !self.borrowed.contains(workspace) {
                self.entering.remove(workspace);
                adoption.release.push(*workspace);
            }
        }
        adoption.changed = arrange(handle, &present, &shown_here);
        adoption
    }

    /// Write to the person's list what changed on `handle`'s shell since the last pass, with
    /// `writes` the session adds, then bring the list as it stands after onto the shell
    /// ([`Self::adopt_into`]). `None` when the daemon did not take a write: the next pass makes
    /// it, comparing the shell with the same last pass, and nothing is brought onto the shell
    /// from a list that lacks it meanwhile.
    pub(crate) fn reconcile(
        &mut self,
        handle: &DesktopHandle,
        list: Rc<WorkspaceList>,
        mut writes: Vec<ListWrite>,
        unknown_here: &dyn Fn(WorkspaceId) -> bool,
    ) -> Option<(Rc<WorkspaceList>, Adoption)> {
        writes.extend(self.publish_local(handle, &list));
        if !self.write(&writes) {
            return None;
        }
        let list = if writes.is_empty() {
            list
        } else {
            self.read().unwrap_or(list)
        };
        self.ask_again_due();
        let adoption = self.adopt_into(handle, &list, unknown_here);
        Some((list, adoption))
    }

    /// The pass is over: what the shell shows now is what the next pass compares it with.
    pub(crate) fn passed(&mut self, handle: &DesktopHandle) {
        let now = shown(handle);
        let present: BTreeSet<WorkspaceId> = now.iter().map(|(workspace, _)| *workspace).collect();
        // A listed workspace being entered has no workspace here until it arrives.
        self.entering
            .retain(|workspace| !present.contains(workspace));
        self.passed = Some(now);
    }

    /// The person's devices as a workspace's settings offer them, this device always among them:
    /// whether the workspace shows on each, and which one is this device.
    /// Empty for a workspace the person's list does not name: one joined through someone else's
    /// invitation is not theirs to show or keep off.
    pub(crate) fn device_choices(&self, workspace: WorkspaceId) -> Vec<DeviceChoice> {
        let Some(list) = self.read() else {
            return Vec::new();
        };
        let Some(entry) = list.get(workspace) else {
            return Vec::new();
        };
        // The person's devices as their own workspace names them: an agent's or a script's key
        // shows no desktop, and a device their root took back shows nothing.
        let person = self.identity.device_cert.user_public_key;
        let mut devices: Vec<([u8; 32], String)> = self
            .devices
            .as_ref()
            .filter(|devices| devices.held().is_some())
            .map(|devices| devices.get().listed(&person))
            .unwrap_or_default()
            .into_iter()
            .filter(|listed| listed.device.kind == KeyKind::Device && !listed.device.revoked)
            .map(|listed| {
                (
                    listed.cert.device_public_key,
                    listed.device.called(&listed.cert),
                )
            })
            .collect();
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
                shown: entry.shows_on(&device),
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
        Some(self.read()?.get(workspace)?.shows_on(device))
    }

    /// Show the workspace on `device` , or keep it off there. Only the devices that opted out are
    /// named, so a device linked later shows it. Returns whether the list changed.
    pub(crate) fn set_shown(&self, workspace: WorkspaceId, device: [u8; 32], shown: bool) -> bool {
        // A workspace the list does not name has no entry to keep anything off.
        if !matches!(self.shown_on(workspace, &device), Some(current) if current != shown) {
            return false;
        }
        self.write(&[WorkspaceList::SHOW_WORKSPACE.with((
            workspace.to_string(),
            hex::encode(device),
            shown,
        ))])
    }

    /// Take in the person's workspaces on hosts that the device's daemon lists and this device
    /// holds no copy of (`listed`, by workspace, [`on_hosts`]), as writes of `list`: each one the list does not name yet is
    /// listed, hosted by the device that signed where it is reached, and with that address, so
    /// this device enters it there as it enters every listed workspace ([`Self::adopt_into`]) and
    /// the person's other devices read it too. A workspace the list names already keeps its name
    /// and host, and one taken off it stays off.
    pub(crate) fn take_hosted(
        list: &WorkspaceList,
        listed: BTreeMap<WorkspaceId, OnHost>,
    ) -> Vec<ListWrite> {
        let mut writes = Vec::new();
        for (workspace, entry) in listed {
            let host = hex::encode(entry.host.host.writes_for());
            let Ok(hint) = postcard::to_allocvec(&entry.host) else {
                continue;
            };
            let hint = hex::encode(hint);
            if list.went(workspace) {
                continue;
            }
            let id = workspace.to_string();
            let named = list.get(workspace);
            if named.is_none() {
                writes.push(WorkspaceList::LIST_WORKSPACE.with((id.clone(), entry.name.clone())));
                writes.push(WorkspaceList::HOST_WORKSPACE.with((id.clone(), host.clone())));
            }
            // The address is the host's own word for where it answers: it is said under its key.
            if named.and_then(|named| named.addresses.get(&host)) != Some(&hint) {
                writes.push(WorkspaceList::REACH_WORKSPACE.with((id, host, hint)));
            }
        }
        writes
    }

    /// The workspaces on a host whose roster, as the daemon `view` reaches reads it, named this
    /// device and names it no more: the person left them, or was taken out of them. A roster that
    /// never named this device says nothing of it: the device may not have been let in yet.
    pub(crate) fn taken_out(
        &mut self,
        view: &crate::workspace_view::WorkspaceView,
    ) -> Vec<WorkspaceId> {
        let mut gone = Vec::new();
        for listed in view.listed() {
            let Some(workspace) = WorkspaceId::parse(&listed.id).filter(|_| listed.host.is_some())
            else {
                continue;
            };
            let Ok(roster) = view.roster_devices(&listed.id) else {
                continue;
            };
            if roster.contains(&self.device) {
                self.in_hosted.insert(workspace);
            } else if !roster.is_empty() && self.in_hosted.remove(&workspace) {
                tracing::info!(%workspace, "a workspace on a host no longer names this device");
                gone.push(workspace);
            }
        }
        gone
    }

    /// Say where this device reaches the host of each listed workspace it holds, as each
    /// workspace's own state names it (`hints`, by workspace id), as writes of `list`: a device of
    /// the person's that does not hold one yet enters it there.
    fn say_hints(&self, list: &WorkspaceList, hints: &[(String, Vec<u8>)]) -> Vec<ListWrite> {
        let device = hex::encode(self.device);
        hints
            .iter()
            .filter_map(|(workspace, hint)| {
                let listed = list.get(WorkspaceId::parse(workspace)?)?;
                let hint = hex::encode(hint);
                (listed.addresses.get(&device) != Some(&hint)).then(|| {
                    WorkspaceList::REACH_WORKSPACE.with((workspace.clone(), device.clone(), hint))
                })
            })
            .collect()
    }

    /// Where `workspace` 's host is reached, as a device of the person's that holds it said in
    /// `list`. `None` while none of them said it yet.
    pub(crate) fn host_hint(
        list: &WorkspaceList,
        workspace: WorkspaceId,
    ) -> Option<tilo_peer_state::join::TransportHint> {
        let bytes = list.get(workspace)?.address()?;
        match postcard::from_bytes(&bytes) {
            Ok(hint) => Some(hint),
            Err(error) => {
                tracing::warn!(%workspace, %error, "a device's host hint did not decode");
                None
            }
        }
    }
}

/// Put this session's workspaces in the person's order: the workspaces the list names, in its
/// order, then the rest as they were. Whether they moved.
fn arrange(handle: &DesktopHandle, present: &[WorkspaceId], order: &[WorkspaceId]) -> bool {
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
    handle.replace_tree(tree);
    true
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

/// The person's own workspaces as this session follows them at `daemon`: nothing when this
/// computer knows no identity to follow them for.
pub(crate) fn personal_for(
    daemon: &tilo_session_client::daemon_documents::DaemonShell,
    wake: &tilo_service::Wake,
) -> Option<RefCell<Personal>> {
    let identity = crate::sync::device_identity()?;
    let mut personal = Personal::open(identity);
    personal.follow(daemon, std::sync::Arc::clone(wake));
    Some(RefCell::new(personal))
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
    pub(crate) fn follow_records(&self) {
        self.follow_data_usage();
        self.follow_personal();
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

    /// Keep the person's own workspaces as all their devices hold them: write to their list what
    /// this session made, renamed, moved or took off since the last pass, open here what the list
    /// names that this device does not hold, and take off the shell what it no longer names here.
    pub(crate) fn follow_personal(&self) {
        let Some(personal) = &self.personal else {
            return;
        };
        // The person's workspaces as the device's daemon holds them: it follows their list on
        // their server and enters each one, so a workspace of theirs a host holds that no device
        // of theirs listed here (as on a device restored from their identity alone) is the
        // daemon's word, with where its host is reached.
        let view = self.view.as_deref();
        let moved = {
            let mut personal = personal.borrow_mut();
            // Until the list is read nothing says which workspaces are the person's: the shell
            // stays as it is.
            let Some(list) = personal.read() else {
                return;
            };
            // Where this device holds each of its workspaces, as the workspaces themselves say: the
            // list names the device hosting a workspace, never the one that happens to hold it
            // here.
            personal.refresh_hosts(self.engine.as_deref());
            let mut writes = Vec::new();
            if let Some(view) = view {
                let held: BTreeSet<WorkspaceId> = self
                    .engine
                    .as_ref()
                    .map(|engine| engine.workspaces().into_iter().collect())
                    .unwrap_or_default();
                let listed = on_hosts(view, &personal.device, &held);
                writes.extend(Personal::take_hosted(&list, listed));
            }
            if let Some(engine) = &self.engine {
                writes.extend(personal.say_hints(&list, &engine.host_hints()));
            }
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
            let Some((list, adoption)) =
                personal.reconcile(&self.handle, list, writes, &unknown_here)
            else {
                return;
            };
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
                self.enter_listed(&mut personal, &list, &opened);
            }
            for workspace in adoption.release {
                self.release_workspace(workspace);
            }
            // A workspace on a host whose roster names this device no more goes the same way: the
            // person left it, or was taken out of it, and what it holds stays theirs to whom it
            // belongs.
            if let Some(view) = view {
                for workspace in personal.taken_out(view) {
                    self.release_workspace(workspace);
                }
            }
            self.settle_seed(&mut personal, &list, view);
            personal.passed(&self.handle);
            if adoption.changed {
                self.keep_layout_now();
            }
            personal.moved()
        };
        // The list moved, so what an open sheet says of the person's devices may have too: a device
        // they linked, or a choice made on another one, arrives in the list alone.
        if moved {
            self.ws_settings_fed.set(None);
            workspace_settings::refresh(
                self.workspace_settings.as_ref(),
                &self.workspace_settings_feed(),
            );
        }
    }

    /// Settle the workspace the shell started with on a device that kept none
    /// ([`Personal::seed`]) against what the person has: the workspaces the device's daemon holds
    /// of theirs, once it published its list, unless it is redeeming an invitation, which lists
    /// them once it is in. A join under way is the person having a workspace, so settling the seed before it
    /// lands would make the shell's own workspace theirs and list it to their other devices.
    fn settle_seed(
        &self,
        personal: &mut Personal,
        list: &WorkspaceList,
        view: Option<&crate::workspace_view::WorkspaceView>,
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
            view.map_or_else(|| Some(BTreeSet::new()), listed_by_daemon)
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
        personal.settle_seed(&self.handle, list, &held, listed.as_ref(), &owned);
    }

    /// Take one of the person's workspaces in that this device does not hold: the list says which
    /// device hosts it and the person's devices said where that host is reached, so this device
    /// asks the host to take it in as a further device of the person's
    /// ([`crate::sync::Client::enter`]). Nothing is invited here: a workspace that never arrives
    /// leaves the session as it found it.
    fn enter_listed(&self, personal: &mut Personal, list: &WorkspaceList, opened: &ToEnter) {
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
        let hint = Personal::host_hint(list, opened.workspace).or_else(|| {
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

    /// The person whose devices the tests run, by the daemon that holds their own workspace,
    /// where their list is. The daemon's root is the fixture's `[1; 32]`.
    struct Person {
        daemon: crate::daemon_fixture::TestDaemon,
    }

    impl Person {
        fn start() -> Self {
            let daemon = crate::daemon_fixture::TestDaemon::start();
            let workspaces = daemon.workspaces().clone();
            tilo_rt::app()
                .block_on(async move {
                    tilo_workspace_service::workspaces::person_workspace::found(&workspaces).await
                })
                .expect("the daemon holds the person's own workspace");
            Self { daemon }
        }

        /// A session of theirs on the device `device`, called `label`, following their list.
        /// Their own workspace keeps the device among their devices, as every device of theirs
        /// keeps itself there.
        fn device(&self, device: u8, label: &str) -> Personal {
            let identity = identity(1, device, label);
            let keep = Devices::keeping(&identity.device_cert).expect("a certificate encodes");
            let workspaces = self.daemon.workspaces().clone();
            let own = tilo_server_protocol::servers::person_workspace_id(
                &identity.device_cert.user_public_key,
            );
            tilo_rt::app()
                .block_on(async move {
                    tilo_workspace_service::workspaces::person_workspace::apply_on(
                        &workspaces,
                        &own,
                        DEVICES,
                        keep,
                    )
                    .await
                })
                .expect("the person's own workspace keeps the device");
            let mut personal = Personal::open(identity);
            personal.follow(&self.daemon.shell(), std::sync::Arc::new(|| {}));
            personal
        }
    }

    /// A pass of `personal`'s session over `session`, as the session makes one on a device whose
    /// workspaces nothing hosts elsewhere: what changed here goes to the list, and the list comes
    /// onto the shell. The workspaces it lets go leave the shell, as the session's own action
    /// takes them off.
    fn pass(personal: &mut Personal, session: &DesktopHandle) -> Adoption {
        personal
            .list
            .as_ref()
            .expect("the list is followed")
            .refresh();
        let list = personal.read().expect("the person's list reads");
        let (_, adoption) = personal
            .reconcile(session, list, Vec::new(), &|_| false)
            .expect("the daemon takes the pass's writes");
        if !adoption.release.is_empty() {
            let mut tree = session.read(Clone::clone);
            for workspace in &adoption.release {
                tree.take_off(*workspace);
            }
            session.replace_tree(tree);
        }
        personal.passed(session);
        adoption
    }

    /// Read `personal`'s list and the person's devices again now, as the follows do once the daemon
    /// wrote them.
    fn fresh(personal: &Personal) {
        personal
            .list
            .as_ref()
            .expect("the list is followed")
            .refresh();
        personal
            .devices
            .as_ref()
            .expect("the devices are followed")
            .refresh();
    }

    /// What `personal`'s list names: each workspace by id, with its name and host, in order.
    fn listed(personal: &Personal) -> Vec<(WorkspaceId, String, Option<[u8; 32]>)> {
        personal
            .list
            .as_ref()
            .expect("the list is followed")
            .refresh();
        personal
            .read()
            .expect("the person's list reads")
            .listed()
            .map(|(workspace, entry)| (workspace, entry.name.clone(), entry.host()))
            .collect()
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

    /// The workspaces on `handle` 's session, in order.
    fn on_shell(handle: &DesktopHandle) -> Vec<WorkspaceId> {
        handle.read(|tree| tree.workspaces.iter().map(Workspace::id).collect())
    }

    /// Put the workspace `workspace` on `handle` 's session, as the session's own action for making
    /// one does.
    fn open_here(handle: &DesktopHandle, workspace: WorkspaceId, name: &str) {
        let mut tree = handle.read(Clone::clone);
        tree.workspaces.push(Workspace::new(workspace, name));
        handle.replace_tree(tree);
    }

    /// The device key of `identity(1, device, _)`.
    fn key(device: u8) -> [u8; 32] {
        identity(1, device, "").device_cert.device_public_key
    }

    /// Whether `personal`'s list names `workspace`.
    fn names(personal: &Personal, workspace: WorkspaceId) -> bool {
        listed(personal)
            .iter()
            .any(|(listed, _, _)| *listed == workspace)
    }

    /// A person's laptop with two workspaces of its own, which their desktop then enters at the
    /// laptop and shows in the person's order; and a workspace kept off the desktop leaves it while
    /// it stays theirs.
    #[test]
    fn the_persons_list_carries_what_one_device_does_to_the_others() {
        let person = Person::start();
        let laptop_shell = session();
        let [first] = on_shell(&laptop_shell)
            .try_into()
            .expect("a fresh session holds one workspace");
        let mut laptop = person.device(0x21, "The laptop");
        pass(&mut laptop, &laptop_shell);
        assert_eq!(
            listed(&laptop),
            [(first, "Session".to_owned(), Some(key(0x21)))],
            "the list names it, hosted by the device that made it"
        );
        let list = laptop.read().expect("the list reads");
        assert!(
            laptop.publish_local(&laptop_shell, &list).is_empty(),
            "a pass with nothing new writes nothing"
        );

        // A workspace made here, then the first renamed and moved after it.
        let second = WorkspaceId::new();
        open_here(&laptop_shell, second, "Second");
        pass(&mut laptop, &laptop_shell);
        let mut tree = laptop_shell.read(Clone::clone);
        tree.workspaces.reverse();
        tree.workspaces
            .iter_mut()
            .find(|workspace| workspace.id() == first)
            .expect("the first workspace")
            .name = "Renamed".to_owned();
        laptop_shell.replace_tree(tree);
        pass(&mut laptop, &laptop_shell);
        assert_eq!(
            listed(&laptop),
            [
                (second, "Second".to_owned(), Some(key(0x21))),
                (first, "Renamed".to_owned(), Some(key(0x21))),
            ],
            "the person's order and names, as this device last left them"
        );

        // The desktop enters them where the laptop hosts them.
        let desktop_shell = session();
        let [own] = on_shell(&desktop_shell)
            .try_into()
            .expect("a fresh session holds one workspace");
        let mut desktop = person.device(0x22, "The desktop");
        let adoption = pass(&mut desktop, &desktop_shell);
        let entered: Vec<(WorkspaceId, [u8; 32])> = adoption
            .enter
            .iter()
            .map(|opened| (opened.workspace, opened.host))
            .collect();
        assert_eq!(
            entered,
            [(second, key(0x21)), (first, key(0x21))],
            "the laptop's workspaces are entered where the laptop hosts them"
        );
        assert!(
            listed(&desktop)
                .iter()
                .any(|(workspace, _, host)| *workspace == own && *host == Some(key(0x22))),
            "and the desktop's own is listed beside them"
        );
        open_here(&desktop_shell, second, "Second");
        open_here(&desktop_shell, first, "Renamed");
        let adoption = pass(&mut desktop, &desktop_shell);
        assert!(adoption.enter.is_empty() && adoption.release.is_empty());
        assert_eq!(
            on_shell(&desktop_shell),
            [second, first, own],
            "they arrive in the person's order, which their arriving does not move"
        );

        // The laptop keeps the first off the desktop.
        fresh(&laptop);
        let choices: BTreeSet<(String, bool, bool)> = laptop
            .device_choices(first)
            .into_iter()
            .map(|choice| (choice.label, choice.shown, choice.here))
            .collect();
        assert_eq!(
            choices,
            BTreeSet::from([
                ("The laptop".to_owned(), true, true),
                ("The desktop".to_owned(), true, false),
            ]),
            "each device of the person's shows it"
        );
        assert!(
            laptop.set_shown(first, key(0x22), false),
            "the choice is written"
        );
        assert!(!laptop.set_shown(first, key(0x22), false), "once");
        let adoption = pass(&mut desktop, &desktop_shell);
        assert_eq!(adoption.release, [first], "the desktop lets it go");
        assert_eq!(on_shell(&desktop_shell), [second, own]);
        pass(&mut desktop, &desktop_shell);
        assert!(names(&laptop, first), "and it stays the person's");
        assert_eq!(laptop.shown_on(first, &key(0x22)), Some(false));

        // Shown there again, the desktop enters it again.
        assert!(laptop.set_shown(first, key(0x22), true));
        let adoption = pass(&mut desktop, &desktop_shell);
        assert!(
            adoption
                .enter
                .iter()
                .any(|opened| opened.workspace == first),
            "a workspace shown on the desktop again is entered there"
        );
    }

    /// A workspace taken off on one device of the person's is let go of on another that was away
    /// meanwhile: when that one's session starts again over a shell that still shows it, the list
    /// says it went, and it is neither listed again nor kept.
    #[test]
    fn a_workspace_taken_off_elsewhere_stays_gone_after_a_restart() {
        let person = Person::start();
        let laptop_shell = session();
        let shared = WorkspaceId::new();
        open_here(&laptop_shell, shared, "Shared");
        let mut laptop = person.device(0x21, "The laptop");
        pass(&mut laptop, &laptop_shell);
        assert!(names(&laptop, shared));

        let desktop_shell = session();
        let mut desktop = person.device(0x22, "The desktop");
        pass(&mut desktop, &desktop_shell);
        open_here(&desktop_shell, shared, "Shared");
        pass(&mut desktop, &desktop_shell);
        drop(desktop);

        // The laptop takes it off while the desktop is away.
        let mut tree = laptop_shell.read(Clone::clone);
        tree.take_off(shared);
        laptop_shell.replace_tree(tree);
        pass(&mut laptop, &laptop_shell);
        assert!(!names(&laptop, shared), "it goes from the list");

        // The desktop starts again, its shell as it kept it.
        assert!(on_shell(&desktop_shell).contains(&shared));
        let mut desktop = person.device(0x22, "The desktop");
        let adoption = pass(&mut desktop, &desktop_shell);
        assert_eq!(adoption.release, [shared], "the desktop lets it go");
        assert!(!on_shell(&desktop_shell).contains(&shared));
        assert!(!names(&desktop, shared), "and does not list it again");
        pass(&mut desktop, &desktop_shell);
        assert!(!names(&laptop, shared), "nor does a later pass");
    }

    /// A workspace the person joined on one device through someone's invitation is that person's
    /// workspace, not theirs: each device of theirs joins it on its own, so their list never names
    /// it, also when the session starts again and nothing remembers the join but the workspace
    /// itself, which says it is hosted at the device that let them in.
    #[test]
    fn a_workspace_joined_through_someones_invitation_is_never_listed() {
        let person = Person::start();
        let laptop_shell = session();
        let mut laptop = person.device(0x21, "The laptop");
        pass(&mut laptop, &laptop_shell);
        let club = WorkspaceId::new();
        laptop.joined(club);
        open_here(&laptop_shell, club, "Book club");
        pass(&mut laptop, &laptop_shell);
        assert!(!names(&laptop, club), "a joined workspace is not listed");

        // The session starts again: where the workspace is held says it is hosted by the device
        // of the person who let them in, as this device's workspace service reads it.
        let mut laptop = person.device(0x21, "The laptop");
        laptop.hosts.insert(club, key(0x33));
        let adoption = pass(&mut laptop, &laptop_shell);
        assert!(!names(&laptop, club), "nor after a restart");
        assert!(adoption.release.is_empty(), "and it stays on the shell");
        assert!(on_shell(&laptop_shell).contains(&club));
    }

    /// A device the person never named shows no name of its own, and names none to their others.
    #[test]
    fn a_device_never_named_shows_no_name() {
        let person = Person::start();
        let shell = session();
        let [own] = on_shell(&shell)
            .try_into()
            .expect("a fresh session holds one workspace");
        let mut phone = person.device(0x23, tilo_user_identity::UNNAMED_PRIMARY_DEVICE);
        pass(&mut phone, &shell);
        fresh(&phone);
        let choices = phone.device_choices(own);
        let [here] = choices.as_slice() else {
            panic!("the phone is offered once: {choices:?}");
        };
        assert!(here.here);
        assert_eq!(
            here.label, "",
            "the stand-in provisioning fills in is no name of the person's"
        );
    }

    /// What this device asks the host its list names for, from what it was told: at the address
    /// the host signed, once.
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
