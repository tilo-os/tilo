//! Rooms this host serves for the device of its own person that governs
//! them: a hosting member.
//!
//! The governor asks with [`HOST_ROOM`](tilo_relay_session::rooms::HOST_ROOM)
//! after admitting this host's device to the room, handing it the room as a
//! joining member gets it (its signed governance, roster and revocations)
//! and its own signed address. This host then
//!
//! - keeps a record of the room under `<state_root>/hosted/<room>.json`, its
//!   own, never the room's files;
//! - federates the room's workspace with the governor's host, and only that
//!   workspace ([`crate::federation`]): subscribing replays what the
//!   governor retains, and live fan-out carries the rest both ways;
//! - follows the room's governance with a replica that governs nothing: each
//!   governance change the governor publishes is verified against the
//!   room's pinned authority, and the members it admits are what this host
//!   admits ([`crate::host::GovernedAccessWriter`]), also while the governor
//!   is offline;
//! - answers the governor with its own signed address, which the room's
//!   join answers carry beside the governor's.
//!
//! It stops serving the room when the governor's devices ask
//! ([`UNHOST_ROOM`](tilo_relay_session::rooms::UNHOST_ROOM)) or when the
//! room's governance no longer admits this host's device. The room's files
//! stay on disk either way.
//!
//! Only a certified host serves rooms for its person this way: a device
//! hosts the rooms placed on it, and runs nothing for anyone else's.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use neighbour_wire::NeighbourMsg;
use serde::{Deserialize, Serialize};
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::join::{JoinBootstrap, TransportHint};
use tilo_peer_state::{GovernedAccess, StatePeer, TransportOrigin};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::task::JoinHandle;
use web_time::SystemTime;

use crate::federation::{PeerRelayDialer, Upstream};
use crate::handshake::LiveIdentity;
use crate::rooms::RoomServices;

/// Governance changes the follower of one room has yet to take in.
const NOTICES: usize = 256;

/// What a governor hands the host it asks to serve one of its rooms.
#[derive(Clone, Debug)]
pub struct HostRoomRequest {
    /// The room's UUID.
    pub room: String,
    /// The room's workspace.
    pub workspace_id: u64,
    /// The governor's own signed address for the room.
    pub governor_hint: TransportHint,
    /// The room as the governor admits a joining member to it, this host's
    /// device admitted.
    pub bootstrap: JoinBootstrap,
}

/// The record a hosting member keeps of each room it serves.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct HostedRecord {
    room: String,
    workspace_id: u64,
    /// The governor's signed address, postcard in hex.
    governor_hint: String,
    /// The room as the governor handed it over, postcard in hex.
    bootstrap: String,
}

impl HostedRecord {
    fn of(request: &HostRoomRequest) -> Result<Self, String> {
        Ok(Self {
            room: request.room.clone(),
            workspace_id: request.workspace_id,
            governor_hint: hex::encode(encode(&request.governor_hint)?),
            bootstrap: hex::encode(encode(&request.bootstrap)?),
        })
    }

    fn request(&self) -> Result<HostRoomRequest, String> {
        Ok(HostRoomRequest {
            room: self.room.clone(),
            workspace_id: self.workspace_id,
            governor_hint: decode_hex(&self.governor_hint)?,
            bootstrap: decode_hex(&self.bootstrap)?,
        })
    }
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>, String> {
    postcard::to_stdvec(value).map_err(|error| error.to_string())
}

/// `text`, postcard in hex, decoded.
pub(crate) fn decode_hex<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    let bytes = hex::decode(text).map_err(|error| error.to_string())?;
    postcard::from_bytes(&bytes).map_err(|error| error.to_string())
}

/// Aborts its task when dropped.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The rooms this host serves for its person's governing devices.
pub(crate) struct Hosting {
    dir: PathBuf,
    services: RoomServices,
    /// Each served room's follower, by room. Dropping one stops the room's
    /// federation and its following.
    served: std::sync::Mutex<HashMap<String, AbortOnDrop>>,
}

impl Hosting {
    pub(crate) fn new(services: RoomServices) -> Self {
        Self {
            dir: services.access.state_root().join("hosted"),
            services,
            served: std::sync::Mutex::default(),
        }
    }

    fn record_path(&self, room: &str) -> PathBuf {
        self.dir.join(format!("{room}.json"))
    }

    /// Serve again every room recorded here, as the host opens. A record
    /// that no longer holds up is skipped with a warning; its room is
    /// served again when its governor asks.
    pub(crate) async fn resume(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let resumed = async {
                let text = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
                let record: HostedRecord =
                    serde_json::from_str(&text).map_err(|error| error.to_string())?;
                let request = record.request()?;
                self.serve(&request, true).await.map(drop)
            }
            .await;
            if let Err(error) = resumed {
                tracing::warn!(path = %path.display(), %error, "a hosted room was not served again");
            }
        }
    }

    /// Serve `request.room` for its governor, `caller`'s request; answer
    /// this host's own signed address for it. Asked again, it starts over
    /// from what the governor hands it now.
    ///
    /// # Errors
    ///
    /// This host is not a certified host, the caller or the governor is not
    /// a device of this host's person, the request does not hold up, or the
    /// room's governance does not admit this host's device.
    pub(crate) async fn host_room(
        &self,
        caller: &DeviceCert,
        request: &HostRoomRequest,
    ) -> Result<TransportHint, String> {
        let own = &self.services.cert;
        if caller.user_public_key != own.user_public_key {
            return Err("only its own person's devices ask a host to serve a room".into());
        }
        if self.services.mode != WorkspaceHostMode::CertifiedHost {
            return Err("a device hosts only the rooms placed on it".into());
        }
        let hint = self.serve(request, false).await?;
        let record = HostedRecord::of(request)?;
        let bytes = serde_json::to_vec_pretty(&record).map_err(|error| error.to_string())?;
        write_durably(&self.dir, &self.record_path(&request.room), &bytes)?;
        tracing::info!(room = %request.room, workspace_id = request.workspace_id, "serving a room for its governor");
        Ok(hint)
    }

    /// Stop serving `room`, `caller`'s request: its federation and following
    /// end, nobody is admitted to it here any more, and this host's record
    /// of it goes. The room's files stay.
    ///
    /// # Errors
    ///
    /// The caller is not a device of this host's person, or the room's
    /// access could not be narrowed.
    pub(crate) async fn unhost_room(
        &self,
        caller: &DeviceCert,
        room: &str,
    ) -> Result<bool, String> {
        if caller.user_public_key != self.services.cert.user_public_key {
            return Err("only its own person's devices ask a host to stop serving a room".into());
        }
        let path = self.record_path(room);
        let workspace_id = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<HostedRecord>(&text).ok())
            .map(|record| record.workspace_id);
        let served = self.served.lock_recover().remove(room).is_some();
        if let Some(workspace_id) = workspace_id {
            stop_serving(&self.services, workspace_id).await?;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        if served || workspace_id.is_some() {
            tracing::info!(%room, "no longer serving a room for its governor");
        }
        Ok(served || workspace_id.is_some())
    }

    /// Check `request`, admit what its governance admits, and start
    /// following the room and federating it with its governor. `resumed`
    /// takes in what this host retained of the room since it was handed
    /// over.
    async fn serve(
        &self,
        request: &HostRoomRequest,
        resumed: bool,
    ) -> Result<TransportHint, String> {
        let services = &self.services;
        let own = &services.cert;
        let governor = IrohWorkspaceHostHintV1::from_transport_hint(&request.governor_hint)
            .map_err(|error| error.to_string())?;
        governor
            .validate_bootstrap(&request.bootstrap)
            .map_err(|error| error.to_string())?;
        let authority = governor.governing_authority.clone();
        if governor.room != request.room
            || governor.workspace_id != request.workspace_id
            || authority.user_public_key != own.user_public_key
        {
            return Err("the room is not governed by a device of this host's person".into());
        }
        if request.governor_hint.host.device_public_key != authority.device_public_key {
            return Err("the governor's address is not the governor's own".into());
        }
        let now = SystemTime::now();
        let mut peer = replica(&request.bootstrap, now)?;
        if resumed {
            // What the room published since it was handed over, as this host
            // retained it; the room's workspace opens with its access.
            admit(services, request.workspace_id, &peer, now).await?;
            let retained = services.records.retained(request.workspace_id).await?;
            for body in retained {
                if body.author == authority.device_public_key {
                    follow(&mut peer, body.author, &body.body, now);
                }
            }
        }
        admit(services, request.workspace_id, &peer, now).await?;
        let workspace = services
            .workspaces
            .lock()
            .await
            .get(&request.workspace_id)
            .cloned()
            .ok_or("the room's workspace did not open")?;
        let (tap, notices) = tokio::sync::mpsc::channel(NOTICES);
        let wake = std::sync::Arc::new(tokio::sync::Notify::new());
        {
            let mut runtime = workspace.lock().await;
            if runtime.room_authority.is_some() {
                return Err("this host governs that room itself".into());
            }
            runtime.governor_tap = Some(tap);
            runtime.governor_dial = Some((authority.device_public_key, wake.clone()));
        }
        drop(workspace);
        let identity = LiveIdentity {
            user_public_key: own.user_public_key,
            cert: own.clone(),
            signing_key: services.key.clone(),
            revocations: services.revocations.clone(),
        };
        let dialer = PeerRelayDialer {
            endpoint: services.endpoint.clone(),
            identity,
            workspaces: services.workspaces.clone(),
            metrics: services.metrics.clone(),
            scope: Some(vec![request.workspace_id]),
        }
        .spawn_woken(
            Upstream::OwnDevice {
                addr: governor.endpoint_addr.clone(),
                device: authority.device_public_key,
            },
            format!("governor of {}", request.room),
            wake,
        );
        let follower = Follower {
            services: services.clone(),
            room: request.room.clone(),
            workspace_id: request.workspace_id,
            governor: authority.device_public_key,
            record: self.record_path(&request.room),
            peer,
        };
        let task = tokio::spawn(follower.run(notices, AbortOnDrop(dialer)));
        self.served
            .lock_recover()
            .insert(request.room.clone(), AbortOnDrop(task));
        own_hint(services, &governor)
    }
}

/// This host's own signed address for the room `governor` addresses.
fn own_hint(
    services: &RoomServices,
    governor: &IrohWorkspaceHostHintV1,
) -> Result<TransportHint, String> {
    let endpoint_addr = services.endpoint.addr();
    let packet_relay_url = endpoint_addr
        .relay_urls()
        .next()
        .map_or_else(|| "iroh:direct".to_owned(), ToString::to_string);
    IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::CertifiedHost,
        room: governor.room.clone(),
        workspace_id: governor.workspace_id,
        governing_authority: governor.governing_authority.clone(),
        endpoint_addr,
        host_user_public_key: services.cert.user_public_key,
        host_device_certificate: services.cert.clone(),
        packet_relay_url,
    }
    .transport_hint(&services.key)
    .map_err(|error| error.to_string())
}

/// A replica of the room `bootstrap` hands over, governing nothing: its
/// governance admitted from the bootstrap's signed messages. Record writes
/// are not this replica's to follow.
fn replica(bootstrap: &JoinBootstrap, now: SystemTime) -> Result<StatePeer, String> {
    let mut revocations = RevocationList::new();
    for entry in &bootstrap.revocations {
        revocations
            .insert(entry.clone())
            .map_err(|error| error.to_string())?;
    }
    let mut peer = StatePeer::joined(
        &bootstrap.room,
        bootstrap.authority.clone(),
        bootstrap.certified_roster.clone(),
        revocations,
        now,
    )?;
    peer.select_admission_v5()
        .map_err(|error| error.to_string())?;
    let origin = TransportOrigin::RetainedRelayDelta(bootstrap.authority.device_public_key);
    for bytes in &bootstrap.governance_messages {
        let message: NeighbourMsg =
            postcard::from_bytes(bytes).map_err(|error| error.to_string())?;
        if matches!(message, NeighbourMsg::AppDocument { .. }) {
            continue;
        }
        peer.receive_from(origin, message, now)?;
    }
    if !peer.has_governance() {
        return Err("the room was handed over without its governance".into());
    }
    Ok(peer)
}

/// Take `body`, a change the governor published, into `peer` when it is
/// governance. Whether governance changed.
fn follow(peer: &mut StatePeer, governor: [u8; 32], body: &[u8], now: SystemTime) -> bool {
    let Ok(message) = postcard::from_bytes::<NeighbourMsg>(body) else {
        return false;
    };
    if matches!(message, NeighbourMsg::AppDocument { .. }) {
        return false;
    }
    match peer.receive_from(TransportOrigin::RetainedRelayDelta(governor), message, now) {
        Ok(outcome) => outcome.changed || outcome.metadata_changed,
        Err(error) => {
            tracing::debug!(room = %peer.room(), %error, "a governance change was not taken in");
            false
        }
    }
}

/// Admit to the room's workspace exactly the members `peer`'s governance
/// admits, opening the workspace when it is not yet.
///
/// # Errors
///
/// The governance does not admit this host's device, or the access could not
/// be written.
async fn admit(
    services: &RoomServices,
    workspace_id: u64,
    peer: &StatePeer,
    now: SystemTime,
) -> Result<(), String> {
    let access: GovernedAccess = peer.governance().governed_access(now)?;
    if !access
        .member_devices
        .contains(&services.cert.device_public_key)
    {
        return Err("the room's governance does not admit this host".into());
    }
    services
        .access
        .replace(workspace_id, true, access)
        .await
        .map_err(|error| error.to_string())
}

/// Stop handing `workspace_id`'s changes to a follower.
async fn stop_following(services: &RoomServices, workspace_id: u64) {
    let workspace = services.workspaces.lock().await.get(&workspace_id).cloned();
    if let Some(workspace) = workspace {
        let mut runtime = workspace.lock().await;
        runtime.governor_tap = None;
        runtime.governor_dial = None;
    }
}

/// Admit nobody to `workspace_id` any more, stop handing its changes to a
/// follower, and take it out of this host's registry. Its files stay: a
/// later request to serve it starts from them.
async fn stop_serving(services: &RoomServices, workspace_id: u64) -> Result<(), String> {
    stop_following(services, workspace_id).await;
    services
        .access
        .replace(
            workspace_id,
            true,
            GovernedAccess {
                member_devices: Vec::new(),
                member_user_roots: Vec::new(),
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    crate::workspace_io::evict(&services.workspaces, workspace_id).await;
    Ok(())
}

/// Follows one served room's governance.
struct Follower {
    services: RoomServices,
    room: String,
    workspace_id: u64,
    governor: [u8; 32],
    record: PathBuf,
    peer: StatePeer,
}

impl Follower {
    /// Take in each change the room's workspace hands over, for as long as
    /// the room is served. `dialer` federates the room and ends with it.
    async fn run(
        mut self,
        mut notices: tokio::sync::mpsc::Receiver<([u8; 32], Vec<u8>)>,
        dialer: AbortOnDrop,
    ) {
        while let Some((author, body)) = notices.recv().await {
            if author != self.governor {
                continue;
            }
            let now = SystemTime::now();
            if !follow(&mut self.peer, author, &body, now) {
                continue;
            }
            match admit(&self.services, self.workspace_id, &self.peer, now).await {
                Ok(()) => {}
                Err(error) if self.dropped(now) => {
                    tracing::info!(room = %self.room, %error, "the room's governance no longer admits this host");
                    break;
                }
                Err(error) => {
                    tracing::warn!(room = %self.room, %error, "a hosted room's access was not updated");
                }
            }
        }
        drop(dialer);
        if self.dropped(SystemTime::now()) {
            if let Err(error) = stop_serving(&self.services, self.workspace_id).await {
                tracing::warn!(room = %self.room, %error, "a room no longer hosted here was not closed");
            }
            if let Err(error) = std::fs::remove_file(&self.record) {
                tracing::debug!(room = %self.room, %error, "the hosted room's record was not removed");
            }
        }
    }

    /// Whether the room's governance no longer admits this host's device.
    fn dropped(&self, now: SystemTime) -> bool {
        self.peer
            .governance()
            .governed_access(now)
            .is_ok_and(|access| {
                !access
                    .member_devices
                    .contains(&self.services.cert.device_public_key)
            })
    }
}

/// Write `bytes` to `path` under `dir` and make it durable: a temporary file
/// renamed into place, the directory synced.
fn write_durably(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let staged = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&staged).map_err(|error| error.to_string())?;
    file.write_all(bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    std::fs::rename(&staged, path).map_err(|error| error.to_string())?;
    std::fs::File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(|error| error.to_string())
}
