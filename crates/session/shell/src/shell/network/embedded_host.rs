//! This device's own room host: the rooms placed on it, and its person's
//! topics for their other devices.
//!
//! Every device runs the room host a relay runs (`tilo_relay::host::RoomHost`)
//! in process, in [`HostMode::Device`]: the device governing its rooms
//! terminates their service itself, with no host capability, and the
//! person's other devices reach this one through it, on the same session
//! protocol, for the topics they share. It opens as the network starts on a
//! device with an identity and a place to keep it
//! ([`ShellNetwork::open_local_host`]); until then, and on a device without
//! one, its router's handlers for it refuse every connection
//! ([`tilo_network_actor::local_host`]).
//!
//! A new workspace is placed on this device's host
//! ([`ShellNetwork::place_pending_workspaces`]): the tree made the workspace
//! for its room in the joining state and recorded the room as pending
//! placement; here the host creates that same room, issues this device the
//! invitation it redeems (the host's own `tilo.host` operations, asked in
//! process as a relay is asked over a session), and the native owner
//! enrolls with it and installs the room, a member of the room like every
//! other. A personal workspace goes to a relay only by its person's choice.
//!
//! The host's state lives beside the native owner's state file, under
//! [`HOST_STATE_DIR`].
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tilo_network_actor::local_host::room_host::{self, HostConfig, HostMode, RoomHost};
use tilo_relay_session::rooms::{ATTACH_HOST, DETACH_HOST};

use super::{NetCmd, RoomId, ShellNetwork, relay_rooms, room_topic_id};
use crate::shell::state::WorkspaceId;
use tilo_network_actor::local_host::LocalHost;

/// The directory, beside the native owner's state file, that this device's
/// room host keeps its rooms in.
pub(crate) const HOST_STATE_DIR: &str = "room-host";

#[derive(Default)]
pub(super) struct State {
    /// This device's host, shared with the actor; `None` for a network
    /// without a router of its own (tests).
    host: Option<LocalHost>,
    /// Held while the host opens, so it opens once: by a task on the
    /// network's runtime, however the opening was asked.
    opening: Arc<tokio::sync::Mutex<()>>,
    /// Rooms being placed now, so a second pass leaves them alone.
    placing: Mutex<BTreeSet<RoomId>>,
    /// A placement pass is queued on the peer-sync worker and has not run.
    queued: Arc<AtomicBool>,
    /// What a pass that left a placement failing saw ([`Placing`]): no pass
    /// runs again until something it depends on changed.
    failed: Mutex<Option<Placing>>,
    /// Why the latest placement of each room still waiting failed: an app
    /// waiting for the room says so.
    failures: Mutex<BTreeMap<RoomId, String>>,
    /// Bumped when the host opens: a placement that failed for want of it
    /// may succeed now.
    host_generation: std::sync::atomic::AtomicU64,
    /// Placement passes that ran, for tests that a failing one is not
    /// retried on every pass.
    #[cfg(test)]
    passes: std::sync::atomic::AtomicU64,
}

/// Everything a placement depends on that can change: the rooms waiting,
/// the tree's rooms (a session coming up or a room's state changing bumps
/// its epoch), and the host.
#[derive(Clone, PartialEq, Eq)]
struct Placing {
    waiting: BTreeSet<RoomId>,
    room_epoch: u64,
    host_generation: u64,
}

impl State {
    pub(super) fn new(host: LocalHost) -> Self {
        Self {
            host: Some(host),
            ..Self::default()
        }
    }
}

/// Everything opening this device's host takes ([`ShellNetwork::local_host`]).
struct HostOpening {
    handle: LocalHost,
    lock: Arc<tokio::sync::Mutex<()>>,
    config: HostConfig,
    identity: room_host::HostIdentity,
    endpoint: iroh::Endpoint,
}

/// The opening of this device's host, held by the caller that opened it
/// until its rooms are governed.
type OpeningHeld = tokio::sync::OwnedMutexGuard<()>;

impl HostOpening {
    /// The host, opened now unless another caller opened it first; the
    /// held opening when this call opened it.
    async fn open(self) -> Result<(Arc<RoomHost>, Option<OpeningHeld>), String> {
        let held = self.lock.lock_owned().await;
        if let Some(host) = self.handle.get() {
            return Ok((host, None));
        }
        let host = RoomHost::open(self.config, self.identity, self.endpoint)
            .await
            .map_err(|error| format!("this device's room host did not open: {error}"))?;
        Ok((self.handle.install(Arc::new(host)), Some(held)))
    }
}

/// Where the host whose native owner keeps `state_file` keeps its rooms.
pub(crate) fn host_state_root(state_file: &Path) -> PathBuf {
    state_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(HOST_STATE_DIR)
}

/// The workspaces of `tree` waiting to be placed, with their rooms and
/// names, leaving out those `placing` already.
fn waiting(
    tree: &crate::shell::state::WorkspaceTree,
    placing: &BTreeSet<RoomId>,
) -> Vec<(RoomId, WorkspaceId, String)> {
    tree.pending_placements
        .iter()
        .filter(|room| !placing.contains(room))
        .filter_map(|room| {
            let workspace = tree
                .desktop
                .workspaces
                .iter()
                .chain(&tree.archived_workspaces)
                .find(|workspace| workspace.room() == *room)?;
            Some((*room, workspace.id, workspace.name.clone()))
        })
        .collect()
}

impl ShellNetwork {
    /// This device's room host, while it is open.
    pub(crate) fn room_host(&self) -> Option<Arc<RoomHost>> {
        self.embedded_host.host.as_ref()?.get()
    }

    /// Whether a room hosted here has a member device other than this one:
    /// then others dial this device, and it stays reachable. A host whose
    /// rooms only this device holds stays dark. Membership is the room's
    /// governed access: a removed person's devices stay in the document as
    /// signed history, and count for nothing here.
    pub(super) fn hosts_other_members(&self) -> bool {
        if self.room_host().is_none() {
            return false;
        }
        let Some(owner) = self.opened_native_owner() else {
            return false;
        };
        let own = *self.local_peer_id.as_bytes();
        owner.rooms().into_iter().any(|room| {
            owner
                .with_current(room, |view| {
                    let hosted_here = view
                        .transport_hint()
                        .is_ok_and(|hint| tilo_peer_state::owner::session::hosted_by(hint, &own));
                    if !hosted_here {
                        return Ok(false);
                    }
                    let access = view
                        .peer()
                        .governance()
                        .governed_access(std::time::SystemTime::now())?;
                    Ok(access.member_devices.iter().any(|device| *device != own))
                })
                .unwrap_or(false)
        })
    }

    /// `channel`'s content keys from this device's own room host, which takes
    /// part in the channel with this device's leaf, when `room` is hosted
    /// here: what this device's broker seals and
    /// opens the room's records with. `None` for a room hosted elsewhere, or
    /// while the host is not open. Blocks on the host: call it off the frame
    /// loop.
    ///
    /// The host answers on this network's runtime, never on the caller's
    /// thread: the broker asks from its connections' own runtime, where
    /// entering another runtime to wait panics and takes the asking app's
    /// connection with it.
    pub(crate) fn hosted_channel_keys(
        &self,
        room: RoomId,
        channel: collab_types::ChannelId,
        route: Option<&collab_types::ChannelRoute>,
        create: bool,
    ) -> Option<Result<channel_crypto::ChannelContentKeys, String>> {
        let own = *self.local_peer_id.as_bytes();
        if !self.tree.read().native_authority.hosted_on(room, &own) {
            return None;
        }
        let Some(host) = self.room_host() else {
            return Some(Err("this device's room host is not open".into()));
        };
        let runtime = match self.runtime_handle() {
            Ok(runtime) => runtime,
            Err(error) => return Some(Err(error)),
        };
        let (room, route) = (room.to_string(), route.cloned());
        let (answer, answered) = std::sync::mpsc::sync_channel(1);
        runtime.spawn(async move {
            let keys = host
                .rooms()
                .own_channel_keys(&room, channel, route.as_ref(), create)
                .await;
            let _ = answer.send(keys);
        });
        // The task ends without answering only when the runtime shuts down.
        answered
            .recv()
            .unwrap_or_else(|_| Some(Err("this device's network stopped".into())))
    }

    /// Whether this device's own host holds `room`, as the native owner,
    /// already open, has it.
    pub(crate) fn hosted_here(&self, room: RoomId) -> bool {
        let Some(owner) = self.opened_native_owner() else {
            return false;
        };
        let own = *self.local_peer_id.as_bytes();
        owner
            .with_current(room, |view| {
                Ok(view
                    .transport_hint()
                    .is_ok_and(|hint| tilo_peer_state::owner::session::hosted_by(hint, &own)))
            })
            .unwrap_or(false)
    }

    /// Where this device's host keeps its rooms.
    fn host_state_root(&self) -> Result<PathBuf, String> {
        let path = self
            .tree
            .read()
            .persist_path
            .clone()
            .ok_or("this device keeps no workspaces on disk to host")?;
        Ok(host_state_root(&path))
    }

    /// Open this device's host: the rooms it hosts are served again and
    /// its native owner joins them, and the person's other devices reach
    /// this one through it. A device with no identity, or nowhere to keep
    /// its workspaces, opens nothing. Blocks on the disk: call it off the
    /// frame loop.
    pub(crate) fn open_local_host(&self) {
        if !self.places_workspaces() || self.host_state_root().is_err() {
            return;
        }
        if let Err(error) = self.local_host() {
            tracing::warn!(%error, "this device's host is not served");
        }
    }

    /// This device's host, opened now when it is not open yet. Waits for
    /// the disk on the network's runtime ([`Self::wait_on`]): call it from
    /// a thread that may block, never from a task.
    pub(super) fn local_host(&self) -> Result<Arc<RoomHost>, String> {
        if let Some(host) = self.room_host() {
            return Ok(host);
        }
        let (host, opened) = self.wait_on(self.host_opening()?.open())??;
        Ok(self.host_opened(host, opened))
    }

    /// Everything opening this device's host takes, owned, so it opens on
    /// the network's runtime.
    fn host_opening(&self) -> Result<HostOpening, String> {
        let handle = self
            .embedded_host
            .host
            .clone()
            .ok_or("this device hosts no rooms")?;
        let root = self.host_state_root()?;
        let identity = self
            .host_identity
            .as_ref()
            .ok_or("this device has no identity to host rooms with")?;
        let endpoint = self
            .private_chat_endpoint
            .clone()
            .ok_or("this device's endpoint is not up")?;
        let (certificate, entries) = identity.current_person_material()?;
        let mut revocations = tilo_user_identity::RevocationList::new();
        for entry in entries {
            revocations
                .insert(entry)
                .map_err(|error| error.to_string())?;
        }
        let config = HostConfig::new(root, HostMode::Device);
        Ok(HostOpening {
            handle,
            lock: Arc::clone(&self.embedded_host.opening),
            config,
            identity: room_host::HostIdentity {
                user_public_key: certificate.user_public_key,
                device_signing_key: identity.device_signing_key.clone(),
                device_cert: certificate,
                revocations,
            },
            endpoint,
        })
    }

    /// The host [`HostOpening::open`] answered; when this call opened it,
    /// its rooms are governed and joined from here on. The opening stays
    /// held until then, so no caller gets the host before.
    fn host_opened(&self, host: Arc<RoomHost>, opened: Option<OpeningHeld>) -> Arc<RoomHost> {
        let Some(_held) = opened else {
            return host;
        };
        tracing::info!("this device's room host is open");
        self.embedded_host
            .host_generation
            .fetch_add(1, Ordering::AcqRel);
        // The rooms hosted here that the native owner holds join now.
        let _ = self.cmd_tx.try_send(NetCmd::LocalHostReady);
        host
    }

    /// Run the host operation `action` with `arguments` at this device's own
    /// host, as this device: in process, the same `tilo.host` action a relay
    /// answers over a session ([`RoomHost::ask_own`]).
    pub(super) fn local_host_action(
        &self,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let host = self.local_host()?;
        let caller = self.own_host_caller()?;
        let action = action.to_owned();
        super::native_enrollment::wait_on_runtime(&self.runtime_handle()?, async move {
            host.ask_own(&caller, &action, arguments).await
        })?
    }

    /// The certificate this device asks its own host's operations with: its
    /// person's.
    fn own_host_caller(&self) -> Result<tilo_user_identity::DeviceCert, String> {
        let identity = self
            .host_identity
            .as_ref()
            .ok_or("native identity unavailable")?;
        Ok(identity.current_person_material()?.0)
    }

    /// Whether this network places workspaces at all: it has a host of its
    /// own and an identity to create rooms as. Neither changes while it
    /// runs, so a device without them never tries, rather than failing on
    /// every pass; its workspaces wait, pending, for the network a
    /// provisioned identity starts.
    fn places_workspaces(&self) -> bool {
        self.embedded_host.host.is_some() && self.host_identity.is_some()
    }

    /// Whether a workspace waits to be placed and no pass is queued for it,
    /// never waiting: the frame loop asks.
    fn placements_pending(&self) -> bool {
        self.places_workspaces()
            && !self.embedded_host.queued.load(Ordering::Acquire)
            && self
                .tree
                .try_read_or_wake(self.release_wake())
                .is_some_and(|tree| {
                    let placing = self
                        .embedded_host
                        .placing
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    tree.pending_placements
                        .iter()
                        .any(|room| !placing.contains(room))
                        && self
                            .embedded_host
                            .failed
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .as_ref()
                            != Some(&self.placing_now(&tree))
                })
    }

    /// What a placement pass would depend on now.
    fn placing_now(&self, tree: &crate::shell::state::WorkspaceTree) -> Placing {
        Placing {
            waiting: tree.pending_placements.clone(),
            room_epoch: tree.desktop.room_epoch,
            host_generation: self.embedded_host.host_generation.load(Ordering::Acquire),
        }
    }

    /// Queue a placement pass on the peer-sync worker when a workspace
    /// waits for its room, for the frame loop: making a workspace wakes
    /// the loop, so it is placed the frame after. A network the session
    /// started no worker for places it in place.
    pub(super) fn place_pending_workspaces_off_loop(&self) {
        if !self.placements_pending() {
            return;
        }
        self.embedded_host.queued.store(true, Ordering::Release);
        if !self.queue_placements() {
            self.embedded_host.queued.store(false, Ordering::Release);
            self.place_pending_workspaces();
        }
    }

    /// Place every workspace waiting for this device's host, once, before
    /// the session lets apps start: blocks on the host and the disk, never
    /// on the network.
    pub(crate) fn place_before_launches(&self) {
        let started = std::time::Instant::now();
        self.place_pending_workspaces();
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis(),
            "workspaces waiting for this device's host are placed before apps start"
        );
    }

    /// The queued pass runs: a workspace made from now on queues another.
    pub(super) fn placement_pass_started(&self) {
        self.embedded_host.queued.store(false, Ordering::Release);
    }

    /// Place every workspace waiting for its room on this device's host,
    /// each once. Blocks on the host and the disk: runs on the peer-sync
    /// worker.
    pub(crate) fn place_pending_workspaces(&self) {
        if !self.places_workspaces() {
            return;
        }
        // A room waiting with no workspace has nothing to place; left
        // waiting, it would ask for a pass on every frame.
        let orphaned = if self.tree.read().has_orphaned_placements() {
            self.tree.write().drop_orphaned_placements()
        } else {
            Vec::new()
        };
        if !orphaned.is_empty() {
            tracing::info!(
                ?orphaned,
                "rooms waiting to be placed with no workspace are placed no more"
            );
        }
        let (waiting, before) = {
            let tree = self.tree.read();
            let before = self.placing_now(&tree);
            if self
                .embedded_host
                .failed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                == Some(&before)
            {
                return;
            }
            #[cfg(test)]
            self.embedded_host.passes.fetch_add(1, Ordering::AcqRel);
            let mut placing = self
                .embedded_host
                .placing
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            // A room placed and since held waits no more.
            placing.retain(|room| tree.pending_placements.contains(room));
            self.forget_placement_failures(|room| !tree.pending_placements.contains(room));
            let waiting = waiting(&tree, &placing);
            drop(tree);
            placing.extend(waiting.iter().map(|(room, _, _)| *room));
            drop(placing);
            (waiting, before)
        };
        let mut failed = false;
        for (room, local, name) in waiting {
            let placed = self.place(room, local, &name);
            if let Err(error) = &placed {
                tracing::warn!(%room, %error, "a workspace was not placed on this device");
                failed = true;
            }
            self.note_placement(room, placed.as_ref().err());
            // Placed, the room is held by this device's native owner
            // already ([`WorkspaceTree::project_native_room`]): its
            // enrollment ran in process. A failed placement is left to a
            // later pass.
            if placed.is_err() {
                self.embedded_host
                    .placing
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&room);
            }
        }
        // A failed placement is tried again once something it depends on
        // changes, never on every pass: as it was when this pass began, so
        // a change during the pass brings the next one.
        *self
            .embedded_host
            .failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = failed.then_some(before);
    }

    /// The enrollment of this device's native owner into `room` ended: a
    /// room placed here waits for its replica when it succeeded, and is
    /// placed again once something placement depends on changed when it
    /// failed.
    pub(super) fn placement_enrollment_ended(&self, room: RoomId, enrolled: Result<(), &String>) {
        let Err(error) = enrolled else {
            return;
        };
        let mut placing = self
            .embedded_host
            .placing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !placing.remove(&room) {
            return;
        }
        drop(placing);
        self.note_placement(room, Some(error));
        let tree = self.tree.read();
        let now = self.placing_now(&tree);
        drop(tree);
        *self
            .embedded_host
            .failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(now);
    }

    /// Why the latest placement of `room` on this device's host failed,
    /// while it waits to be placed again.
    pub(crate) fn placement_failure(&self, room: RoomId) -> Option<String> {
        self.embedded_host
            .failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&room)
            .cloned()
    }

    /// Keep why `room`'s placement failed, or that it did not, waking an
    /// app waiting for the room when that changed.
    fn note_placement(&self, room: RoomId, failure: Option<&String>) {
        let mut failures = self
            .embedded_host
            .failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let changed = match failure {
            Some(error) => failures.insert(room, error.clone()).as_ref() != Some(error),
            None => failures.remove(&room).is_some(),
        };
        drop(failures);
        if changed {
            self.tree.room_readiness().announce();
        }
    }

    /// Forget why rooms `gone` names failed to place: they wait no more.
    fn forget_placement_failures(&self, gone: impl Fn(&RoomId) -> bool) {
        let mut failures = self
            .embedded_host
            .failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let before = failures.len();
        failures.retain(|room, _| !gone(room));
        let changed = failures.len() != before;
        drop(failures);
        if changed {
            self.tree.room_readiness().announce();
        }
    }

    /// Create `room`, the workspace `local` called `name`, on this device's
    /// host, and enroll this device into it with the invitation the host
    /// issues it, in process ([`RoomHost::place_own_room`]): the room is
    /// held when this returns.
    fn place(&self, room: RoomId, local: WorkspaceId, name: &str) -> Result<(), String> {
        let host = self.local_host()?;
        let identity = self
            .host_identity
            .as_ref()
            .ok_or("native identity unavailable")?;
        let (certificate, _) = identity.current_native_material(room_topic_id(room))?;
        let caller = self.own_host_caller()?;
        let name = relay_rooms::room_name(name);
        let own =
            self.wait_on(
                async move { host.place_own_room(&room.to_string(), &name, &caller).await },
            )??;
        let client = self
            .embedded_host
            .host
            .as_ref()
            .and_then(|handle| handle.join_client(certificate.device_public_key))
            .ok_or("this device's host is not open")?;
        // In process, and installed before this returns: placing a room on
        // this device's own host waits on nothing but this device.
        self.enroll_in_process(local, &own, client)?;
        tracing::info!(%room, "a workspace is placed on this device's host");
        Ok(())
    }

    /// This device's own host, in process, when `invitation` names it as the
    /// room's host. `None` otherwise.
    pub(super) fn local_route(
        &self,
        invitation: &str,
    ) -> Option<tilo_relay_session::workspace_join::WorkspaceJoinClient> {
        let own = *self.local_peer_id.as_bytes();
        let verified = tilo_peer_state::join::decode_and_verify_invitation_text(
            invitation,
            None,
            &tilo_user_identity::RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .ok()?;
        let hosted_here = verified
            .grant()
            .transport
            .iter()
            .any(|hint| tilo_peer_state::owner::session::hosted_by(hint, &own));
        if !hosted_here {
            return None;
        }
        self.embedded_host.host.as_ref()?.join_client(own)
    }
}

impl ShellNetwork {
    /// Have this person's home relay serve `room`, a workspace this device's
    /// host governs, as well (their choice in Settings, Devices & Sync): the
    /// relay then holds and serves the room while this device is off. Blocks
    /// until the relay took the room up: call it off the frame loop.
    ///
    /// # Errors
    ///
    /// This device's host is not open or does not govern the room, the
    /// person has no home relay, or the relay could not be reached or
    /// refused the room.
    pub fn add_hosting_member(&self, room: RoomId) -> Result<(), String> {
        let relay = self.home_relay_for(room)?;
        let mut arguments = serde_json::Map::new();
        arguments.insert("room".into(), room.to_string().into());
        arguments.insert(
            "host".into(),
            hex::encode(relay.expectation.device_public_key).into(),
        );
        arguments.insert(
            "host_addr".into(),
            serde_json::to_value(&relay.addr).map_err(|error| error.to_string())?,
        );
        self.local_host_action(ATTACH_HOST, arguments).map(drop)
    }

    /// Take this person's home relay off as a host of `room` again: it no
    /// longer serves the room, and is asked to stop. Blocks like
    /// [`Self::add_hosting_member`].
    ///
    /// # Errors
    ///
    /// This device's host is not open or does not govern the room, or the
    /// person has no home relay.
    pub fn remove_hosting_member(&self, room: RoomId) -> Result<(), String> {
        let relay = self.home_relay_for(room)?;
        let mut arguments = serde_json::Map::new();
        arguments.insert("room".into(), room.to_string().into());
        arguments.insert(
            "host".into(),
            hex::encode(relay.expectation.device_public_key).into(),
        );
        arguments.insert(
            "host_addr".into(),
            serde_json::to_value(&relay.addr).map_err(|error| error.to_string())?,
        );
        self.local_host_action(DETACH_HOST, arguments).map(drop)
    }

    /// The person's home relay to serve `room`.
    pub(super) fn home_relay_for(&self, room: RoomId) -> Result<super::RegisteredHomePeer, String> {
        self.home_relay_of(room)?
            .ok_or_else(|| "this person has no home relay".into())
    }
}

#[cfg(test)]
#[path = "embedded_host_tests.rs"]
mod tests;
