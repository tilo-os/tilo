//! This device hosting the rooms placed on it.
//!
//! Every device runs the room host a relay runs (`tilo_room_host::RoomHost`)
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

use tilo_actions::remote::{Caller, RemoteActionMessage};
use tilo_relay_session::rooms::{
    CREATE_ROOM, CreatedRoom, ENTER_ROOM, EnteredRoom, HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE,
};
use tilo_room_host::{HostConfig, HostMode, RoomHost};

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
    /// rooms only this device holds stays dark.
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
                    Ok(hosted_here
                        && view
                            .peer()
                            .certificates()
                            .iter()
                            .any(|cert| cert.device_public_key != own))
                })
                .unwrap_or(false)
        })
    }

    /// Where this device's host keeps its rooms.
    fn host_state_root(&self) -> Result<PathBuf, String> {
        let replica = self
            .replica
            .lock()
            .map_err(|_| "the workspaces are unavailable")?;
        let path = replica
            .tree
            .read()
            .persist_path
            .clone()
            .ok_or("this device keeps no workspaces on disk to host")?;
        drop(replica);
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
        let config = HostConfig {
            state_root: root,
            mode: HostMode::Device,
            ..HostConfig::default()
        };
        let host_identity = tilo_room_host::HostIdentity {
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
        let (certificate, _) = identity.current_native_material(ws)?;
        let invoke = relay_rooms::invocation(
            HOST_ACTIONS_PROVIDER,
            HOST_ROOMS_RESOURCE.to_owned(),
            action,
            arguments,
        );
        let caller =
            Caller::authenticated(certificate.user_public_key, certificate.device_public_key);
        let pending = host
            .actions()
            .serve_certified(
                ws,
                &caller,
                &certificate,
                RemoteActionMessage::Invoke(invoke),
            )
            .ok_or("this device's host does not answer its own operations")?;
        match self.runtime_handle()?.block_on(pending) {
            Some(RemoteActionMessage::Receipt(receipt)) => {
                relay_rooms::receipt_value(receipt.status)
            }
            Some(_) => Err("this device's host answered with something else".into()),
            None => Err("this device's host dropped the request".into()),
        }
    }

    /// Whether a workspace waits to be placed and no pass is queued for it,
    /// never waiting: the frame loop asks.
    fn placements_pending(&self) -> bool {
        !self.embedded_host.queued.load(Ordering::Acquire)
            && self.replica.try_lock().is_ok_and(|replica| {
                replica.tree.try_read().is_some_and(|tree| {
                    let placing = self
                        .embedded_host
                        .placing
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    tree.pending_placements
                        .iter()
                        .any(|room| !placing.contains(room))
                })
            })
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
        let waiting = {
            let Ok(replica) = self.replica.lock() else {
                return;
            };
            let tree = replica.tree.read();
            let mut placing = self
                .embedded_host
                .placing
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let waiting = waiting(&tree, &placing);
            placing.extend(waiting.iter().map(|(room, _, _)| *room));
            waiting
        };
        for (room, local, name) in waiting {
            let placed = self.place(room, local, &name);
            if let Err(error) = &placed {
                tracing::warn!(%room, %error, "a workspace was not placed on this device");
            }
            if placed.is_ok()
                && let Ok(replica) = self.replica.lock()
            {
                let mut tree = replica.tree.write();
                if tree.pending_placements.remove(&room) {
                    tree.queue_save();
                }
            }
            // A failed placement is tried again on the next pass.
            self.embedded_host
                .placing
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&room);
        }
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

#[cfg(test)]
#[path = "embedded_host_tests.rs"]
mod tests;
