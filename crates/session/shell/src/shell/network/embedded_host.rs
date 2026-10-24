//! This device hosting the rooms placed on it.
//!
//! Every device runs the room host a relay runs (`tilo_relay::host::RoomHost`)
//! in process, in [`HostMode::Device`]: the device governing its rooms
//! terminates their service itself, with no host capability. It opens only
//! once this device hosts a room: when the first workspace is placed here,
//! or at start when a room was hosted here before
//! ([`ShellNetwork::open_hosted_rooms`]). A device that hosts nothing never
//! opens it, and its router's handlers for it refuse every connection
//! ([`crate::network::local_host`]).
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
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::network::local_host::room_host::{self, HostConfig, HostMode, RoomHost};
use tilo_actions::remote::RemoteActionMessage;
use tilo_relay_session::rooms::{
    ATTACH_HOST, CREATE_ROOM, CreatedRoom, DETACH_HOST, ENTER_ROOM, EnteredRoom,
    HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE,
};

use super::{
    NetCmd, RoomId, ShellNetwork, personal_continuity_topic_id, relay_rooms, room_topic_id,
};
use crate::network::invite::enrollment_driver::Via;
use crate::network::local_host::LocalHost;
use crate::shell::peer_sync_overlay::PeerSyncResponse;
use crate::shell::state::WorkspaceId;

/// The directory, beside the native owner's state file, that this device's
/// room host keeps its rooms in.
pub(crate) const HOST_STATE_DIR: &str = "room-host";

/// How long the invitation this device redeems into a room placed on its
/// own host stays redeemable.
const OWN_INVITATION_SECONDS: u64 = 600;

#[derive(Default)]
pub(super) struct State {
    /// This device's host, shared with the actor; `None` for a network
    /// without a router of its own (tests).
    host: Option<LocalHost>,
    /// Held while the host opens, so it opens once.
    opening: Mutex<()>,
    /// Rooms being placed now, so a second pass leaves them alone.
    placing: Mutex<BTreeSet<RoomId>>,
    /// A placement pass is queued on the peer-sync worker and has not run.
    queued: Arc<AtomicBool>,
    /// What a pass that left a placement failing saw ([`Placing`]): no pass
    /// runs again until something it depends on changed.
    failed: Mutex<Option<Placing>>,
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

/// Where the host whose native owner keeps `state_file` keeps its rooms.
pub(crate) fn host_state_root(state_file: &Path) -> PathBuf {
    state_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(HOST_STATE_DIR)
}

/// Whether the host state at `root` holds anything: a room was hosted here.
/// The host writes nothing there before it hosts its first room.
pub(crate) fn hosted_before(root: &Path) -> bool {
    std::fs::read_dir(root).is_ok_and(|mut entries| entries.next().is_some())
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
                        .is_ok_and(|hint| tilo_joined_room::session::hosted_by(hint, &own));
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

    /// `channel`'s content keys from the steward of this device's own room
    /// host, when `room` is hosted here: what this device's broker seals and
    /// opens the room's records with. `None` for a room hosted elsewhere, or
    /// while the host is not open. Blocks on the host: call it off the frame
    /// loop.
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
        runtime.block_on(host.rooms().steward_channel_keys(
            &room.to_string(),
            channel,
            route,
            create,
        ))
    }

    /// Whether this device's own host holds `room`, as the native owner,
    /// already open, has it.
    pub(super) fn hosted_here(&self, room: RoomId) -> bool {
        let Some(owner) = self.opened_native_owner() else {
            return false;
        };
        let own = *self.local_peer_id.as_bytes();
        owner
            .with_current(room, |view| {
                Ok(view
                    .transport_hint()
                    .is_ok_and(|hint| tilo_joined_room::session::hosted_by(hint, &own)))
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

    /// Open this device's host when a room was hosted here before, so the
    /// rooms it hosts are served again and its native owner joins them. A
    /// device that never hosted a room opens nothing. Blocks on the disk:
    /// call it off the frame loop.
    pub(crate) fn open_hosted_rooms(&self) {
        let Ok(root) = self.host_state_root() else {
            return;
        };
        if !hosted_before(&root) {
            return;
        }
        if let Err(error) = self.local_host() {
            tracing::warn!(%error, "the rooms hosted on this device are not served");
        }
    }

    /// This device's host, opened now when it is not open yet. Blocks on
    /// the disk: call it off the frame loop.
    pub(super) fn local_host(&self) -> Result<Arc<RoomHost>, String> {
        let handle = self
            .embedded_host
            .host
            .as_ref()
            .ok_or("this device hosts no rooms")?;
        if let Some(host) = handle.get() {
            return Ok(host);
        }
        let _opening = self
            .embedded_host
            .opening
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(host) = handle.get() {
            return Ok(host);
        }
        let root = self.host_state_root()?;
        let identity = self
            .host_identity
            .as_ref()
            .ok_or("this device has no identity to host rooms with")?;
        let endpoint = self
            .private_chat_endpoint
            .clone()
            .ok_or("this device's endpoint is not up")?;
        let (certificate, entries) = identity.current_native_material(0)?;
        let mut revocations = tilo_user_identity::RevocationList::new();
        for entry in entries {
            revocations
                .insert(entry)
                .map_err(|error| error.to_string())?;
        }
        let config = HostConfig::new(root, HostMode::Device);
        let host_identity = room_host::HostIdentity {
            user_public_key: certificate.user_public_key,
            device_signing_key: identity.device_signing_key.clone(),
            device_cert: certificate,
            revocations,
        };
        let host = self
            .runtime_handle()?
            .block_on(RoomHost::open(config, host_identity, endpoint))
            .map_err(|error| format!("this device's room host did not open: {error}"))?;
        let host = handle.install(Arc::new(host));
        tracing::info!("this device's room host is open");
        self.embedded_host
            .host_generation
            .fetch_add(1, Ordering::AcqRel);
        if let Err(error) = self.install_own_governor(&host) {
            tracing::warn!(%error, "the rooms hosted on this device are governed over the network only");
        }
        // The rooms hosted here that the native owner holds join now.
        let _ = self.cmd_tx.try_send(NetCmd::LocalHostReady);
        Ok(host)
    }

    /// Run the host operation `action` with `arguments` at this device's own
    /// host, as this device on workspace `ws`: in process, the same
    /// `tilo.host` action a relay answers over a session.
    pub(super) fn local_host_action(
        &self,
        ws: u64,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let host = self.local_host()?;
        let identity = self
            .host_identity
            .as_ref()
            .ok_or("native identity unavailable")?;
        let invoke = relay_rooms::invocation(
            HOST_ACTIONS_PROVIDER,
            HOST_ROOMS_RESOURCE.to_owned(),
            action,
            arguments,
        );
        serve_in_process(&host, identity, &self.runtime_handle()?, ws, invoke)
    }

    /// Make this device's open `host` the governor of the rooms placed on
    /// it, so a change to one of them is asked there in process, with or
    /// without a running network.
    fn install_own_governor(&self, host: &Arc<RoomHost>) -> Result<(), String> {
        let identity = self
            .host_identity
            .clone()
            .ok_or("native identity unavailable")?;
        self.install_local_governor(Arc::new(OwnHostGovernor {
            host: Arc::clone(host),
            identity,
            runtime: self.runtime_handle()?,
            tree: self.tree.clone(),
            device: *self.local_peer_id.as_bytes(),
        }));
        Ok(())
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
            && self.tree.try_read().is_some_and(|tree| {
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
            // Placed, the room waits until this device's native owner holds
            // it ([`WorkspaceTree::project_native_room`]), its enrollment
            // still being placed: it is neither placed again nor a join
            // meanwhile. A failed placement is left to a later pass.
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
    pub(super) fn placement_enrollment_ended(&self, room: RoomId, enrolled: bool) {
        if enrolled {
            return;
        }
        let mut placing = self
            .embedded_host
            .placing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !placing.remove(&room) {
            return;
        }
        drop(placing);
        let tree = self.tree.read();
        let now = self.placing_now(&tree);
        drop(tree);
        *self
            .embedded_host
            .failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(now);
    }

    /// Create `room`, the workspace `local` called `name`, on this device's
    /// host, and enroll this device into it with the invitation the host
    /// issues it: the workspace says Joining… until its replica installs.
    fn place(&self, room: RoomId, local: WorkspaceId, name: &str) -> Result<(), String> {
        let host = self.local_host()?;
        let identity = self
            .host_identity
            .as_ref()
            .ok_or("native identity unavailable")?;
        let (certificate, _) = identity.current_native_material(room_topic_id(room))?;
        let ws = personal_continuity_topic_id(&certificate.user_public_key);
        // A placement cut short after the host made the room goes on from
        // there: the room it is, never another.
        let held = self
            .runtime_handle()?
            .block_on(host.rooms().summary(&room.to_string()))
            .is_ok();
        drop(host);
        if !held {
            let created: CreatedRoom = relay_rooms::answer_of(self.local_host_action(
                ws,
                CREATE_ROOM,
                relay_rooms::create_arguments(name, room),
            )?)?;
            if RoomId::parse(&created.room.room) != Some(room) {
                return Err("this device's host made another room than the workspace's".into());
            }
        }
        let mut arguments = serde_json::Map::new();
        arguments.insert("room".into(), room.to_string().into());
        arguments.insert("expires_in_seconds".into(), OWN_INVITATION_SECONDS.into());
        let entered: EnteredRoom =
            relay_rooms::answer_of(self.local_host_action(ws, ENTER_ROOM, arguments)?)?;
        let own = tilo_peer_state::join::encode_invitation_text(
            &entered.invitation.decode()?.signed_invitation,
        );
        let client = self
            .embedded_host
            .host
            .as_ref()
            .and_then(|handle| handle.join_client(certificate.device_public_key))
            .ok_or("this device's host is not open")?;
        if let PeerSyncResponse::EnrollmentPending { id } =
            self.accept_invitation_via(local, &own, Via::Local(client))?
        {
            self.finish_when_enrolled(room, id, true)?;
        }
        tracing::info!(%room, "a workspace is placed on this device's host");
        Ok(())
    }

    /// The way to the host that issued `invitation`: this device's own, in
    /// process, when it names this device as the room's host. `None`
    /// otherwise.
    pub(super) fn local_route(&self, invitation: &str) -> Option<Via> {
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
            .any(|hint| tilo_joined_room::session::hosted_by(hint, &own));
        if !hosted_here {
            return None;
        }
        self.embedded_host
            .host
            .as_ref()?
            .join_client(own)
            .map(Via::Local)
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
        let (ws, relay) = self.home_relay_for(room)?;
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
        self.local_host_action(ws, ATTACH_HOST, arguments).map(drop)
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
        let (ws, relay) = self.home_relay_for(room)?;
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
        self.local_host_action(ws, DETACH_HOST, arguments).map(drop)
    }

    /// The workspace this device's own host operations are asked on, and
    /// the person's home relay to serve `room`.
    fn home_relay_for(&self, room: RoomId) -> Result<(u64, super::RegisteredHomePeer), String> {
        let identity = self
            .host_identity
            .as_ref()
            .ok_or("native identity unavailable")?;
        let (certificate, _) = identity.current_native_material(room_topic_id(room))?;
        let user = certificate.user_public_key;
        let topics = super::relay_topics_for_home_peers(std::iter::empty(), Some(user));
        let relay = super::registered_home_peer_addrs(Some(user), &topics)?
            .into_iter()
            .next()
            .ok_or("this person has no home relay")?;
        Ok((personal_continuity_topic_id(&user), relay))
    }
}

#[cfg(test)]
#[path = "embedded_host_tests.rs"]
mod tests;

/// Run `invoke` at this device's own `host` as this device on workspace
/// `ws`, through the host's in-process caller, and answer its result.
fn serve_in_process(
    host: &RoomHost,
    identity: &crate::network::actor::HostIdentity,
    runtime: &tokio::runtime::Handle,
    ws: u64,
    invoke: tilo_actions::remote::InvokeRequest,
) -> Result<serde_json::Value, String> {
    let (certificate, _) = identity.current_native_material(ws)?;
    let caller = room_host::local_caller(&certificate);
    let pending = host
        .actions()
        .serve_certified(
            ws,
            &caller,
            &certificate,
            RemoteActionMessage::Invoke(invoke),
        )
        .ok_or("this device's host does not answer its own operations")?;
    match runtime.block_on(pending) {
        Some(RemoteActionMessage::Receipt(receipt)) => relay_rooms::receipt_value(receipt.status),
        Some(_) => Err("this device's host answered with something else".into()),
        None => Err("this device's host dropped the request".into()),
    }
}

/// This device's room host as the governor of the rooms placed on it: a
/// governance change to one of them is the host's own room governance
/// action, asked through its in-process caller.
struct OwnHostGovernor {
    host: Arc<RoomHost>,
    identity: crate::network::actor::HostIdentity,
    runtime: tokio::runtime::Handle,
    tree: crate::shell::state::WorkspaceTreeHandle,
    device: [u8; 32],
}

impl super::governance_requests::LocalGovernor for OwnHostGovernor {
    fn governs(&self, room: RoomId) -> bool {
        self.tree
            .read()
            .native_authority
            .hosted_on(room, &self.device)
    }

    fn ask(
        &self,
        room: RoomId,
        route: super::governance_requests::GovernanceRoute,
    ) -> Result<(), String> {
        let invoke = relay_rooms::invocation(
            tilo_actions::remote::ROOM_GOVERNANCE_PROVIDER,
            tilo_actions::remote::room_governance_resource(&room.to_string()),
            route.action,
            route.arguments,
        );
        serve_in_process(
            &self.host,
            &self.identity,
            &self.runtime,
            room_topic_id(room),
            invoke,
        )
        .map(drop)
    }

    fn record_receipt(
        &self,
        room: RoomId,
        receipt: &tilo_desktop_model::WorkspaceEffectReceipt,
    ) -> Result<(), String> {
        let invoke = relay_rooms::invocation(
            tilo_relay_session::rooms::MEMBER_ACTIONS_PROVIDER,
            tilo_relay_session::rooms::member_resource(&room.to_string()),
            tilo_relay_session::rooms::RECORD_EFFECT_RECEIPT,
            relay_rooms::receipt_arguments(receipt)?,
        );
        serve_in_process(
            &self.host,
            &self.identity,
            &self.runtime,
            room_topic_id(room),
            invoke,
        )
        .map(drop)
    }
}
