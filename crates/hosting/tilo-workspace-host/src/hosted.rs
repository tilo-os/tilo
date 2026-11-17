//! Workspaces this host holds for the host that governs them: a hosting
//! member, of the governor's own person or, where this host's operator takes
//! that work ([`tilo_relay_protocol::WorkSetting`]), of another person's.
//!
//! The governor asks with [`HOST_WORKSPACE`](tilo_relay_session::workspaces::HOST_WORKSPACE)
//! after admitting this host's device to the workspace, handing it the workspace as a
//! joining member gets it (its signed governance, roster and revocations)
//! and its own signed address. This host then
//!
//! - keeps a record of the workspace under `<state_root>/hosted/<workspace>.json`, its
//!   own, never the workspace's files;
//! - attaches to the governor's host for the workspace, and only
//!   that workspace ([`crate::attach`]): subscribing replays what the
//!   governor retains, and live fan-out carries the rest both ways;
//! - follows the workspace's governance with a replica that governs nothing: each
//!   governance change the governor publishes is verified against the
//!   workspace's pinned authority, and the members it admits are what this host
//!   admits ([`crate::host::GovernedAccessWriter`]), also while the governor
//!   is offline;
//! - answers the governor with its own signed address, which the workspace's
//!   join answers carry beside the governor's.
//!
//! It stops serving the workspace when the governor's person asks
//! ([`UNHOST_WORKSPACE`](tilo_relay_session::workspaces::UNHOST_WORKSPACE)) or when the
//! workspace's governance no longer names this host. The workspace's files
//! stay on disk either way.
//!
//! A host serves a workspace this way only when its work setting takes it:
//! by default its own person's, and another person's only for the workspaces
//! its operator names, or anyone's when the operator says so.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use neighbour_wire::NeighbourMsg;
use serde::{Deserialize, Serialize};
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::join::{JoinBootstrap, TransportHint};
use tilo_peer_state::{GovernedAccess, StatePeer, TransportOrigin};
use tilo_user_identity::DeviceCert;
use tokio::task::JoinHandle;
use web_time::SystemTime;

use crate::attach::{HostDialer, Upstream};
use crate::handshake::LiveIdentity;
use crate::workspaces::WorkspaceServices;
use tilo_relay_protocol::{Whose, WorkSetting, WorkspaceId};

/// Governance changes the follower of one workspace has yet to take in.
const NOTICES: usize = 256;

/// What a governor hands the host it asks to serve one of its workspaces.
#[derive(Clone, Debug)]
pub struct HostWorkspaceRequest {
    /// The workspace's UUID.
    pub workspace: String,
    /// The workspace's transport shard.
    pub workspace_id: WorkspaceId,
    /// The governor's own signed address for the workspace.
    pub governor_hint: TransportHint,
    /// The workspace as the governor admits a joining member to it, this host's
    /// device admitted.
    pub bootstrap: JoinBootstrap,
}

/// The record a hosting member keeps of each workspace it serves.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct HostedRecord {
    /// The workspace's UUID.
    workspace: String,
    workspace_id: WorkspaceId,
    /// The governor's signed address, postcard in hex.
    governor_hint: String,
    /// The workspace as the governor handed it over, postcard in hex.
    bootstrap: String,
}

impl HostedRecord {
    fn of(request: &HostWorkspaceRequest) -> Result<Self, String> {
        Ok(Self {
            workspace: request.workspace.clone(),
            workspace_id: request.workspace_id,
            governor_hint: hex::encode(encode(&request.governor_hint)?),
            bootstrap: hex::encode(encode(&request.bootstrap)?),
        })
    }

    fn request(&self) -> Result<HostWorkspaceRequest, String> {
        Ok(HostWorkspaceRequest {
            workspace: self.workspace.clone(),
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

/// The workspaces this host serves for its person's governing devices.
pub(crate) struct Hosting {
    dir: PathBuf,
    services: WorkspaceServices,
    /// Each served workspace's follower, by workspace. Dropping one stops the workspace's
    /// attach session and its following.
    served: std::sync::Mutex<HashMap<String, AbortOnDrop>>,
}

impl Hosting {
    pub(crate) fn new(services: WorkspaceServices) -> Self {
        Self {
            dir: services.access.state_root().join("hosted"),
            services,
            served: std::sync::Mutex::default(),
        }
    }

    fn resource_path(&self, workspace: &str) -> PathBuf {
        self.dir.join(format!("{workspace}.json"))
    }

    /// Serve again every workspace recorded here, as the host opens. A record
    /// that no longer holds up is skipped with a warning; its workspace is
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
                self.serve(None, &request, true).await.map(drop)
            }
            .await;
            if let Err(error) = resumed {
                tracing::warn!(path = %path.display(), %error, "a hosted workspace was not served again");
            }
        }
    }

    /// Serve `request.workspace` for its governor, `caller`'s request; answer
    /// this host's own signed address for it. Asked again, it starts over
    /// from what the governor hands it now.
    ///
    /// # Errors
    ///
    /// The device does not take the caller's work, the request does not hold
    /// up, or the workspace's governance does not admit this device.
    pub(crate) async fn host_workspace(
        &self,
        caller: &DeviceCert,
        request: &HostWorkspaceRequest,
    ) -> Result<TransportHint, String> {
        let hint = self.serve(Some(caller), request, false).await?;
        let record = HostedRecord::of(request)?;
        let bytes = serde_json::to_vec_pretty(&record).map_err(|error| error.to_string())?;
        write_durably(&self.dir, &self.resource_path(&request.workspace), &bytes)?;
        tracing::info!(workspace = %request.workspace, workspace_id = %request.workspace_id, "serving a workspace for its governor");
        Ok(hint)
    }

    /// Stop serving `workspace`, `caller`'s request: its attach session and
    /// following end, nobody is admitted to it here any more, and this
    /// host's record of it goes. The workspace's files stay.
    ///
    /// # Errors
    ///
    /// The caller is not a device of this host's person, or the workspace's
    /// access could not be narrowed.
    pub(crate) async fn unhost_workspace(
        &self,
        caller: &DeviceCert,
        workspace: &str,
    ) -> Result<bool, String> {
        let path = self.resource_path(workspace);
        // No record: the workspace was not hosted here. One that does not read
        // is an error, never "not hosted": that would forget the workspace while
        // it went on being served.
        let record = match std::fs::read_to_string(&path) {
            Ok(text) => Some(
                serde_json::from_str::<HostedRecord>(&text)
                    .map_err(|error| {
                        format!("the workspace's hosting record does not read: {error}")
                    })?
                    .request()?,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(format!(
                    "the workspace's hosting record does not read: {error}"
                ));
            }
        };
        // This host's own person, or the person of the host governing the
        // workspace, asks it to stop.
        let governor = record
            .as_ref()
            .map(|request| request.governor_hint.host.user_public_key);
        if caller.user_public_key != self.services.cert.user_public_key
            && governor != Some(caller.user_public_key)
        {
            return Err(
                "only its own person or the workspace's governor asks a host to stop serving it"
                    .into(),
            );
        }
        let workspace_id = record.map(|request| request.workspace_id);
        let served = self.served.lock_recover().remove(workspace).is_some();
        if let Some(workspace_id) = workspace_id {
            stop_serving(&self.services, workspace_id).await?;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        if served || workspace_id.is_some() {
            tracing::info!(%workspace, "no longer serving a workspace for its governor");
        }
        Ok(served || workspace_id.is_some())
    }

    /// Check `request`, `caller`'s when a device asks, admit what its
    /// governance admits, and start following the workspace and attaching it to
    /// its governor. `resumed` takes in what this host retained of the
    /// workspace since it was handed over.
    async fn serve(
        &self,
        caller: Option<&DeviceCert>,
        request: &HostWorkspaceRequest,
        resumed: bool,
    ) -> Result<TransportHint, String> {
        let services = &self.services;
        let own = &services.cert;
        let governor = WorkspaceDeviceHintV1::from_transport_hint(&request.governor_hint)
            .map_err(|error| error.to_string())?;
        governor
            .validate_bootstrap(&request.bootstrap)
            .map_err(|error| error.to_string())?;
        // The owner device serving the workspace, whose own address the hint is.
        let authority = governor.device_certificate.clone();
        // The hint names the workspace this host is asked to serve, by the same
        // uuid and shard, and the device asking is of the governor's person.
        if governor.workspace_id != request.workspace_id {
            return Err("the governor's address names another workspace".into());
        }
        if caller.is_some_and(|caller| caller.user_public_key != authority.user_public_key) {
            return Err("only the workspace's governor hands it to a host".into());
        }
        if !takes(&services.work, &authority, own, request.workspace_id) {
            return Err(
                "a device serves a workspace placed on it only when it takes that work".into(),
            );
        }
        let now = tilo_clock::wall();
        let mut peer = replica(&request.bootstrap, now)?;
        if resumed {
            // What the workspace published since it was handed over, as this host
            // retained it; the workspace opens with its access.
            admit(services, request.workspace_id, &peer, now).await?;
            let retained = services.records.retained(request.workspace_id).await?;
            for body in retained {
                if body.author == authority.device_public_key {
                    follow(&mut peer, body.author, &body.body, now);
                }
            }
        }
        // A host of the governor's person is admitted as one of its devices
        // before it is handed the workspace; another person's host is named
        // among the workspace's hosts once it answered its address, so the
        // first hand-over to it does not name it yet.
        let named = admit(services, request.workspace_id, &peer, now).await?;
        if !named && (resumed || authority.user_public_key == own.user_public_key) {
            stop_serving(services, request.workspace_id).await?;
            return Err("the workspace's governance does not admit this host".into());
        }
        let workspace = services
            .workspaces
            .lock()
            .await
            .get(&request.workspace_id)
            .cloned()
            .ok_or("the workspace did not open")?;
        let (tap, notices) = tokio::sync::mpsc::channel(NOTICES);
        let replica = Arc::new(HostedReplica {
            peer: std::sync::Mutex::new(peer.clone()),
            host_device: request.bootstrap.host.clone(),
            generation: request.bootstrap.generation,
        });
        {
            let mut runtime = workspace.lock().await;
            if runtime.workspace_authority.is_some() {
                return Err("this host governs that workspace itself".into());
            }
            runtime.governor_tap = Some(tap);
            runtime.hosted_replica = Some(Arc::clone(&replica));
            // The governor is a host this workspace is attached to from the
            // start: the session it answers this host's with keeps all of
            // the workspace, on-demand records included, whichever of the two
            // sessions comes up first.
            runtime.note_attached_host(authority.device_public_key);
        }
        drop(workspace);
        let identity = LiveIdentity {
            user_public_key: own.user_public_key,
            cert: own.clone(),
            signing_key: services.key.clone(),
            revocations: services.revocations.clone(),
        };
        let dialer = HostDialer {
            endpoint: services.endpoint.clone(),
            identity,
            trust_policy: services.trust_policy.clone(),
            workspaces: services.workspaces.clone(),
            metrics: services.metrics.clone(),
            scope: Some(vec![request.workspace_id]),
        }
        .spawn_woken(
            Upstream {
                addr: governor.endpoint_addr.clone(),
                device: authority.device_public_key,
                person: authority.user_public_key,
            },
            format!("governor of {}", request.workspace),
            &services.arrivals,
        );
        let follower = Follower {
            services: services.clone(),
            workspace: request.workspace.clone(),
            workspace_id: request.workspace_id,
            governor: authority.device_public_key,
            record: self.resource_path(&request.workspace),
            peer,
            replica,
            named,
        };
        let task = tokio::spawn(follower.run(notices, AbortOnDrop(dialer)));
        self.served
            .lock_recover()
            .insert(request.workspace.clone(), AbortOnDrop(task));
        own_hint(services, &governor)
    }
}

/// Whether `work` takes the workspace `workspace_id` that the host `authority`
/// governs, for this host `own`.
fn takes(
    work: &WorkSetting,
    authority: &DeviceCert,
    own: &DeviceCert,
    workspace_id: WorkspaceId,
) -> bool {
    let mut whose = Whose::of_roots(authority.user_public_key, own.user_public_key);
    whose.workspace = Some(workspace_id);
    work.from.admits(&whose)
}

/// This host's own signed address for the workspace `governor` addresses.
fn own_hint(
    services: &WorkspaceServices,
    governor: &WorkspaceDeviceHintV1,
) -> Result<TransportHint, String> {
    let endpoint_addr = services.endpoint.addr();
    let packet_relay_url = endpoint_addr
        .relay_urls()
        .next()
        .map_or_else(|| "iroh:direct".to_owned(), ToString::to_string);
    WorkspaceDeviceHintV1 {
        version: 1,
        holds_keys: true,
        runs_work: !services.work.takes_nothing(),
        workspace_id: governor.workspace_id,
        endpoint_addr,
        user_public_key: services.cert.user_public_key,
        device_certificate: services.cert.clone(),
        packet_relay_url,
    }
    .transport_hint(&services.key)
    .map_err(|error| error.to_string())
}

/// A replica of the workspace `bootstrap` hands over, governing nothing: its
/// governance admitted from the bootstrap's signed messages. Record writes
/// are not this replica's to follow.
fn replica(bootstrap: &JoinBootstrap, now: SystemTime) -> Result<StatePeer, String> {
    let mut peer = StatePeer::from_join(
        &bootstrap.workspace_id.to_string(),
        bootstrap.host.clone(),
        bootstrap.certified_roster.clone(),
        &bootstrap.revocations,
        now,
    )?;
    // This host's copy: it writes under the host's own actor, never the
    // one its device's replica of the workspace writes under.
    peer.write_as_workspace_host();
    let origin = TransportOrigin::RetainedRelayDelta(bootstrap.host.device_public_key);
    for bytes in &bootstrap.governance_messages {
        let message: NeighbourMsg =
            postcard::from_bytes(bytes).map_err(|error| error.to_string())?;
        if matches!(message, NeighbourMsg::AppDocument { .. }) {
            continue;
        }
        peer.receive_from(origin, message, now)
            .map_err(|error| error.to_string())?;
    }
    if !peer.has_governance() {
        return Err("the workspace was handed over without its governance".into());
    }
    Ok(peer)
}

/// Take `body`, a change the governor published, into `peer` when it is
/// governance. Whether governance changed.
fn follow(peer: &mut StatePeer, governor: [u8; 32], body: &[u8], now: SystemTime) -> bool {
    let message = match postcard::from_bytes::<NeighbourMsg>(body) {
        Ok(message) => message,
        Err(error) => {
            tracing::warn!(workspace = %peer.workspace_id(), %error, "a change the governor published does not decode");
            return false;
        }
    };
    if matches!(message, NeighbourMsg::AppDocument { .. }) {
        return false;
    }
    match peer.receive_from(TransportOrigin::RetainedRelayDelta(governor), message, now) {
        Ok(outcome) => outcome.changed || outcome.metadata_changed,
        Err(error) => {
            tracing::debug!(workspace = %peer.workspace_id(), %error, "a governance change was not taken in");
            false
        }
    }
}

/// Admit to the workspace exactly the members `peer`'s governance
/// admits, opening the workspace when it is not yet. Whether that governance
/// admits this host's own device.
///
/// # Errors
///
/// The access could not be written.
async fn admit(
    services: &WorkspaceServices,
    workspace_id: WorkspaceId,
    peer: &StatePeer,
    now: SystemTime,
) -> Result<bool, String> {
    let access: GovernedAccess = peer.governance().governed_access(now)?;
    let named = access
        .member_devices
        .contains(&services.cert.device_public_key);
    services
        .access
        .replace(workspace_id, true, access)
        .await
        .map_err(|error| error.to_string())?;
    Ok(named)
}

/// Stop handing `workspace_id`'s changes to a follower.
async fn stop_following(services: &WorkspaceServices, workspace_id: WorkspaceId) {
    let workspace = services.workspaces.lock().await.get(&workspace_id).cloned();
    if let Some(workspace) = workspace {
        let mut runtime = workspace.lock().await;
        runtime.governor_tap = None;
        runtime.hosted_replica = None;
    }
}

/// This host's replica of a hosted workspace as subscribing devices see it,
/// current with each governance change the workspace's governor published.
pub(crate) struct HostedReplica {
    peer: std::sync::Mutex<StatePeer>,
    /// The device that answered the workspace's join for its owner.
    host_device: DeviceCert,
    generation: u64,
}

impl HostedReplica {
    /// The membership answer `device` builds its replica of the workspace from
    /// ([`crate::workspaces::member_bootstrap`]).
    pub(crate) fn bootstrap(
        &self,
        workspace_id: WorkspaceId,
        device: &DeviceCert,
    ) -> Option<(Vec<u8>, crate::peer::GovernanceCertificates)> {
        let peer = self.peer.lock_recover();
        crate::workspaces::member_bootstrap(
            &peer,
            &self.host_device,
            workspace_id,
            self.generation,
            device,
        )
    }
}

/// Admit nobody to `workspace_id` any more, stop handing its changes to a
/// follower, and take it out of this host's registry. Its files stay: a
/// later request to serve it starts from them.
async fn stop_serving(
    services: &WorkspaceServices,
    workspace_id: WorkspaceId,
) -> Result<(), String> {
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

/// Follows one served workspace's governance.
struct Follower {
    services: WorkspaceServices,
    workspace: String,
    workspace_id: WorkspaceId,
    governor: [u8; 32],
    record: PathBuf,
    peer: StatePeer,
    /// What subscribing devices are answered from.
    replica: Arc<HostedReplica>,
    /// Whether the workspace's governance has admitted this host yet: one
    /// that named it and no longer does has let it go.
    named: bool,
}

impl Follower {
    /// Take in each change the workspace hands over, for as long as
    /// the workspace is served. `dialer` attaches the workspace and ends with it.
    async fn run(
        mut self,
        mut notices: tokio::sync::mpsc::Receiver<([u8; 32], Vec<u8>)>,
        dialer: AbortOnDrop,
    ) {
        while let Some((author, body)) = notices.recv().await {
            if author != self.governor {
                continue;
            }
            let now = tilo_clock::wall();
            if !follow(&mut self.peer, author, &body, now) {
                continue;
            }
            *self.replica.peer.lock_recover() = self.peer.clone();
            match admit(&self.services, self.workspace_id, &self.peer, now).await {
                Ok(true) => self.named = true,
                Ok(false) if self.named => {
                    tracing::info!(workspace = %self.workspace, "the workspace's governance no longer admits this host");
                    break;
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(workspace = %self.workspace, %error, "a hosted workspace's access was not updated");
                }
            }
        }
        drop(dialer);
        if self.named && self.dropped(tilo_clock::wall()) {
            if let Err(error) = stop_serving(&self.services, self.workspace_id).await {
                tracing::warn!(workspace = %self.workspace, %error, "a workspace no longer hosted here was not closed");
            }
            if let Err(error) = std::fs::remove_file(&self.record) {
                tracing::debug!(workspace = %self.workspace, %error, "the hosted workspace's record was not removed");
            }
        }
    }

    /// Whether the workspace's governance no longer admits this host's device.
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
