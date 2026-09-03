//! The long-running daemon: binds an iroh endpoint, accepts on
//! `qos-relay/1`, runs the auth handshake, hands off to
//! [`crate::peer::run_peer`].
//!
//! Also exposes the `Relay` struct so integration tests can spin up
//! two relays in the same process and let them merge.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{Endpoint, RecvStream, SendStream, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh_base::{EndpointAddr, PublicKey};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use qos_user_identity::{DeviceCert, RevocationList, verify_device_cert_at};

use crate::auth::{self, Hello, HelloAck2, PeerRole};
use crate::client::RelayClient;
use crate::config::{PeerRelayConfig, RelayConfig};
use crate::metrics::Metrics;
use crate::peer::{PeerContext, WorkspaceAutobind, WorkspaceRegistry, now_ns, run_peer};
use crate::policy::TrustPolicy;
use crate::transport::{
    ALPN, ContentEndpointAdvertisement, ContentEndpointRequest, DISCOVERY_ALPN, Envelope,
    MAX_FRAME_BYTES,
};
use crate::workspace::{Membership, SharedWorkspace, WorkspaceRuntime};

#[derive(serde::Deserialize)]
struct FullReplicaStatusProof {
    state: String,
    full_content_replication: bool,
    updated_at_ms: u64,
    #[serde(default)]
    filesystem_endpoints: Vec<EndpointAddr>,
}

fn full_replica_proof(path: &std::path::Path) -> Option<FullReplicaStatusProof> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<FullReplicaStatusProof>(&bytes).ok())
        .filter(|proof| {
            proof.state == "ready"
                && proof.full_content_replication
                && !proof.filesystem_endpoints.is_empty()
                && unix_now_ms().saturating_sub(proof.updated_at_ms) <= 5_000
        })
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// Failure modes for `Relay::start`.
#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    /// Iroh endpoint bind / accept.
    #[error("iroh: {0}")]
    Iroh(String),
    /// On-disk persistence.
    #[error("store: {0}")]
    Store(#[from] crate::store::StoreError),
    /// Config load / save.
    #[error("config: {0}")]
    Config(#[from] crate::config::ConfigError),
    /// I/O — bubbled up from filesystem ops during startup.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Identity file missing / malformed.
    #[error("identity: {0}")]
    Identity(String),
}

/// A running relay. Prefer [`Relay::shutdown`]; dropping the handle also
/// stops the daemon and every federation dial task, but leaves peers to
/// notice by timeout.
pub struct Relay {
    /// Iroh endpoint the daemon accepts on.
    pub endpoint: Endpoint,
    /// Iroh router that wires the ALPN → protocol handler.
    _router: Router,
    /// Runtime state shared with peer tasks.
    workspaces: WorkspaceRegistry,
    /// Metrics — cheap counters.
    metrics: Arc<Metrics>,
    /// Own cert (device cert, cap `can_act_as_host`).
    own_cert: DeviceCert,
    /// Own device signing key.
    own_signing_key: ed25519_dalek::SigningKey,
    /// Own user pubkey — the one this relay chains to.
    own_user_public_key: [u8; 32],
    /// Federation dial tasks — one per known peer relay. Aborted
    /// (per-handle) when the `Relay` drops.
    peer_relay_tasks: Arc<std::sync::Mutex<Vec<AbortOnDrop>>>,
    /// Effective same-identity replication and serving policy, safe to expose
    /// to the shell/fs orchestrators because service claims were intersected
    /// with the verified certificate at startup.
    mesh_profile: Arc<crate::mesh::DeviceMeshProfile>,
    /// Atomic filesystem completeness proof shared with the headless replica.
    full_replica_status: PathBuf,
}

/// A tokio `JoinHandle` that aborts on drop — used so that dropping
/// `Relay` tears down every outbound peer-relay reconnect task.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        // Stop reconnect loops before closing the shared endpoint.  An
        // explicit close also tells connected peers about a host shutdown
        // immediately instead of making them wait for QUIC's idle timeout.
        if let Ok(mut tasks) = self.peer_relay_tasks.lock() {
            tasks.clear();
        }
        // `Relay` is normally dropped inside Tokio, but Drop must remain safe
        // in synchronous teardown and after a runtime has already stopped.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let endpoint = self.endpoint.clone();
            runtime.spawn(async move {
                endpoint.close().await;
            });
        }
    }
}

/// Cheap clone-able handle for admin/observer callers.
#[derive(Clone)]
pub struct RelayHandle {
    /// Reachability info a peer can dial.
    pub node_addr: EndpointAddr,
    /// Metrics view — read-only.
    pub metrics: Arc<Metrics>,
    /// Effective device-mesh policy for routing and replication consumers.
    mesh_profile: Arc<crate::mesh::DeviceMeshProfile>,
    full_replica_status: PathBuf,
}

/// Bundle of identity material the relay needs to run. Loaded by
/// the daemon's `main.rs` from
/// [`qos_user_identity::persist`] and passed in explicitly so the
/// integration tests can wire a `tempdir`-based identity.
#[derive(Clone)]
pub struct RelayIdentity {
    /// The user root pubkey the relay chains to.
    pub user_public_key: [u8; 32],
    /// This device's signing key.
    pub device_signing_key: ed25519_dalek::SigningKey,
    /// This device's signed cert (issued by the user root).
    pub device_cert: DeviceCert,
    /// Current revocation list (may be empty).
    pub revocations: RevocationList,
}

impl Relay {
    /// Start the daemon with the given config + identity.
    ///
    /// Binds an iroh endpoint (using the persistent secret at
    /// `<state_root>/iroh-secret.bin`), opens each bound workspace
    /// on disk, and spawns an accept task that runs the auth
    /// handshake + dispatches to `run_peer` for every inbound
    /// connection.
    ///
    /// # Errors
    ///
    /// Returns [`RelayError::Identity`] if `identity` fails validation
    /// (bad signature, wrong chain, revoked, or not host-capable),
    /// [`RelayError::Io`] if the state root can't be created,
    /// [`RelayError::Store`] if the persistent iroh secret or any
    /// configured workspace can't be loaded, and [`RelayError::Iroh`] if
    /// the listen address is invalid or the endpoint fails to bind.
    pub async fn start(mut cfg: RelayConfig, identity: RelayIdentity) -> Result<Self, RelayError> {
        import_home_peer_bindings(&mut cfg)?;
        validate_relay_identity(&identity)?;
        if !cfg.device_role.can_serve() {
            return Err(RelayError::Identity(
                "qos-relay requires an always-on-server or hybrid device role".into(),
            ));
        }
        // A certificate authorises a service, but it cannot make a service
        // exist. The composite process starts relay + filesystem planes; app,
        // compute, and remote-control claims remain unavailable here.
        cfg.services = cfg
            .services
            .authorized_by(&identity.device_cert.capabilities)
            .limited_to(&crate::mesh::ServiceCapabilities::home_peer());
        let mesh_profile = Arc::new(crate::mesh::DeviceMeshProfile {
            device: identity.device_cert.device_public_key,
            role: cfg.device_role,
            services: cfg.services.clone(),
            replication: cfg
                .workspaces
                .iter()
                .map(|binding| (binding.id, binding.replication.clone()))
                .collect(),
            filesystem_endpoints: Vec::new(),
        });
        std::fs::create_dir_all(&cfg.state_root)?;
        let endpoint = bind_endpoint(&cfg, &identity.device_signing_key).await?;
        let workspaces = build_workspace_registry(&cfg)?;
        let metrics = Arc::new(Metrics::default());
        let revocations = Arc::new(std::sync::RwLock::new(identity.revocations.clone()));

        let live_trust_policy = Arc::new(std::sync::RwLock::new(cfg.trust_policy.clone()));
        let handler = RelayProtocol {
            workspaces: workspaces.clone(),
            trust_policy: live_trust_policy.clone(),
            revocations: revocations.clone(),
            own_user_pk: identity.user_public_key,
            own_cert: identity.device_cert.clone(),
            own_sk: identity.device_signing_key.clone(),
            metrics: metrics.clone(),
            autobind: matches!(cfg.trust_policy, TrustPolicy::AllowMatchingUser).then(|| {
                WorkspaceAutobind {
                    state_root: cfg.state_root.clone(),
                }
            }),
        };
        let discovery = DiscoveryProtocol {
            workspaces: workspaces.clone(),
            own_user_pk: identity.user_public_key,
            trust_policy: live_trust_policy.clone(),
            revocations: revocations.clone(),
            own_cert: identity.device_cert.clone(),
            own_sk: identity.device_signing_key.clone(),
            mesh_profile: mesh_profile.clone(),
            full_replica_status: cfg.full_replica_status.clone(),
            home_peer_bundle: cfg.home_peer_bundle.clone(),
        };
        let router = Router::builder(endpoint.clone())
            .accept(ALPN, handler)
            .accept(DISCOVERY_ALPN, discovery)
            .spawn();

        let peer_relay_tasks = spawn_background_tasks(
            &cfg,
            &identity,
            &endpoint,
            &workspaces,
            &metrics,
            revocations,
        );
        if matches!(cfg.trust_policy, TrustPolicy::AllowList { .. })
            && cfg.home_peer_bundle.exists()
        {
            push_peer_relay_task(
                &peer_relay_tasks,
                spawn_home_peer_authority_refresh(
                    cfg.home_peer_bundle.clone(),
                    identity.user_public_key,
                    live_trust_policy,
                    workspaces.clone(),
                ),
            );
        }

        Ok(Self {
            endpoint,
            _router: router,
            workspaces,
            metrics,
            own_cert: identity.device_cert,
            own_signing_key: identity.device_signing_key,
            own_user_public_key: identity.user_public_key,
            peer_relay_tasks,
            mesh_profile,
            full_replica_status: cfg.full_replica_status.clone(),
        })
    }

    /// Whether this relay currently counts `device` as subscribed to
    /// `workspace_id`.
    ///
    /// This is the relay's own belief, which is what routing acts on: a
    /// device is here from the moment its `Subscribe` is accepted until
    /// the connection closes, and a delta published in between is fanned
    /// out to it rather than queued. There is no way to learn a peer has
    /// gone before its close arrives, so anything that depends on the
    /// departure having been noticed has to ask.
    pub async fn is_subscribed(&self, workspace_id: u64, device: &[u8; 32]) -> bool {
        let ws = self.workspaces.lock().await.get(&workspace_id).cloned();
        match ws {
            Some(ws) => ws.lock().await.has_subscriber(device),
            None => false,
        }
    }

    /// Stop the relay and tell every peer it has gone.
    ///
    /// Dropping the handle also stops it, but abandons the QUIC
    /// connections rather than closing them: each peer keeps this relay
    /// as a live subscriber until its own idle timeout, and for that
    /// whole window fans out to a connection nobody reads instead of
    /// queueing for store-and-forward. Deltas in that window are neither
    /// delivered nor queued. Closing the endpoint sends `CONNECTION_CLOSE`
    /// straight away, so peers drop the subscription and queue instead.
    pub async fn shutdown(self) {
        // Stop reconnecting first: the dial loop would otherwise open a
        // fresh connection while the endpoint is closing.
        lock_peer_relay_tasks(&self.peer_relay_tasks).clear();
        self.endpoint.close().await;
    }

    /// Add a peer relay after startup — used by tests + the eventual
    /// admin CLI. Same reconnect-with-backoff loop as the config-time
    /// path.
    pub fn add_peer_relay(&self, peer: PeerRelayConfig) {
        let Some(node_id) = parse_iroh_node_id(&peer.iroh_node_id) else {
            tracing::warn!(?peer.iroh_node_id, "peer relay iroh_node_id not parseable, skipping");
            return;
        };
        self.add_peer_relay_at(EndpointAddr::new(node_id), peer.label, peer.priority);
    }

    /// Add a peer relay with a fully-resolved endpoint address —
    /// same dial loop, but skips iroh discovery. Useful for tests
    /// that already know the target's iroh addresses, and for
    /// pinned-address deployments.
    pub fn add_peer_relay_at(&self, addr: EndpointAddr, label: String, priority: i32) {
        let task = spawn_peer_relay_dial_at(
            addr,
            label,
            priority,
            self.endpoint.clone(),
            self.own_user_public_key,
            self.own_cert.clone(),
            self.own_signing_key.clone(),
            RevocationList::new(),
            self.workspaces.clone(),
            self.metrics.clone(),
        );
        push_peer_relay_task(&self.peer_relay_tasks, task);
    }

    /// Cheap external handle.
    pub fn handle(&self) -> RelayHandle {
        RelayHandle {
            node_addr: self.endpoint.addr(),
            metrics: self.metrics.clone(),
            mesh_profile: self.mesh_profile.clone(),
            full_replica_status: self.full_replica_status.clone(),
        }
    }

    /// The iroh endpoint address (node id + reachable addresses) —
    /// what a peer needs to dial this relay.
    pub fn node_addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// Bind (or re-bind) a workspace's membership set. Called by
    /// the admin CLI after `bind-workspace`.
    ///
    /// # Errors
    ///
    /// Returns [`RelayError::Store`] if opening a not-yet-bound
    /// workspace, or persisting its membership list, fails.
    pub async fn set_workspace_membership(
        &self,
        workspace_id: u64,
        pinned: bool,
        members: Vec<[u8; 32]>,
        state_root: PathBuf,
    ) -> Result<(), RelayError> {
        let existing = {
            let map = self.workspaces.lock().await;
            map.get(&workspace_id).cloned()
        };
        if let Some(existing) = existing {
            let mut existing = existing.lock().await;
            existing.membership.set(members);
            existing
                .store
                .save_membership(&existing.membership.devices)?;
            drop(existing);
            return Ok(());
        }
        let mut rt =
            WorkspaceRuntime::open(&state_root, workspace_id, pinned, Membership::default())?;
        rt.membership.set(members);
        rt.store.save_membership(&rt.membership.devices)?;
        self.workspaces
            .lock()
            .await
            .insert(workspace_id, Arc::new(AsyncMutex::new(rt)));
        Ok(())
    }

    /// Own user identity — for tests.
    pub fn own_user_public_key(&self) -> [u8; 32] {
        self.own_user_public_key
    }
}

/// Import full-workspace residency from the same bundle the filesystem worker
/// consumes. The operator cannot accidentally advertise a relay-only binding
/// for a workspace whose NAS worker is configured to retain complete content.
fn import_home_peer_bindings(cfg: &mut RelayConfig) -> Result<(), RelayError> {
    if !cfg.home_peer_bundle.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&cfg.home_peer_bundle)?;
    let bundle: fs::full_replica::HomePeerSet = toml::from_str(&text)
        .map_err(|error| RelayError::Identity(format!("invalid home-peer bundle: {error}")))?;
    fs::full_replica::validate_home_peer_set(&bundle)
        .map_err(|error| RelayError::Identity(error.to_string()))?;
    if bundle.status_file != cfg.full_replica_status {
        return Err(RelayError::Identity(
            "home-peer status path differs from relay status path".into(),
        ));
    }
    let mut collaborators = Vec::new();
    let now = unix_now_secs();
    for workspace in bundle.workspaces {
        let ticket = fs::Ticket::decode(&workspace.ticket)
            .map_err(|error| RelayError::Identity(format!("invalid home-peer ticket: {error}")))?;
        let relay_workspace_ids = if workspace.relay_workspace_ids.is_empty() {
            // Bundles exported before room bindings existed have one stable
            // fallback shard. New enrollment always writes explicit ids.
            vec![
                crate::room_workspace_id(&ticket.workspace_id().to_string()).map_err(|error| {
                    RelayError::Identity(format!("invalid workspace id: {error}"))
                })?,
            ]
        } else {
            workspace.relay_workspace_ids
        };
        // An FS authority is deliberately *not* a relay-room membership
        // list.  In particular, an FsPath grant lets a collaborator read a
        // path in the global Files namespace; it must never silently give
        // that person access to every shell room wired to the namespace.
        // Only a governor-signed, explicit Scope::Room(<relay id>) grant
        // admits a user to that particular relay topic.
        let mut authority_users = Vec::new();
        let mut governor = None;
        if let Some(authority) = workspace.authority.as_ref() {
            governor = Some(authority.governor);
            authority_users.push(hex::encode(authority.governor));
        }
        authority_users.sort_unstable();
        authority_users.dedup();
        collaborators.extend(authority_users.iter().cloned());
        for id in relay_workspace_ids {
            let mut room_users = governor
                .map(|governor| vec![hex::encode(governor)])
                .unwrap_or_default();
            if let Some(authority) = workspace.authority.as_ref()
                && id != crate::personal_continuity_workspace_id(&authority.governor)
            {
                let room = id.to_string();
                for capability in &authority.capabilities {
                    if capability.effect == collab_types::Effect::Grant
                        && capability.is_live_at(Some(now))
                        && matches!(&capability.scope, collab_types::Scope::Room(scope) if scope == &room)
                        && let collab_types::Subject::User(user) = &capability.subject
                    {
                        let user = hex::encode(user.as_bytes());
                        room_users.push(user.clone());
                        collaborators.push(user);
                    }
                }
            }
            room_users.sort_unstable();
            room_users.dedup();
            if !cfg.workspaces.iter().any(|binding| binding.id == id) {
                cfg.workspaces.push(crate::config::WorkspaceBinding {
                    id,
                    pin: true,
                    replication: crate::mesh::ReplicationIntent::FullWorkspace,
                    admitted_users: room_users.clone(),
                });
            } else if let Some(binding) = cfg.workspaces.iter_mut().find(|binding| binding.id == id)
            {
                binding.admitted_users.extend(room_users);
                binding.admitted_users.sort_unstable();
                binding.admitted_users.dedup();
            }
        }
    }
    collaborators.sort_unstable();
    collaborators.dedup();
    if !collaborators.is_empty() && matches!(cfg.trust_policy, TrustPolicy::AllowMatchingUser) {
        cfg.trust_policy = TrustPolicy::AllowList {
            users: collaborators,
        };
    }
    cfg.services.full_content_replication = true;
    Ok(())
}

impl RelayHandle {
    /// Services safe to advertise at this instant.  The filesystem proof is
    /// read for every advertisement, so an edit immediately withdraws the
    /// full-content claim until its new blobs are resident.
    pub fn advertised_services(&self) -> crate::mesh::ServiceCapabilities {
        let mut services = self.mesh_profile.services.clone();
        services.full_content_replication &= !self.filesystem_endpoints().is_empty();
        services
    }

    /// Filesystem endpoints that proved complete residency in the current
    /// heartbeat. Consumers use these for blob and namespace protocols while
    /// retaining `node_addr` for relay traffic.
    pub fn filesystem_endpoints(&self) -> Vec<EndpointAddr> {
        full_replica_proof(&self.full_replica_status)
            .map(|proof| {
                proof
                    .filesystem_endpoints
                    .into_iter()
                    .filter(|endpoint| endpoint.id.as_bytes() == &self.mesh_profile.device)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Current profile safe to publish to placement/discovery consumers.
    pub fn mesh_profile(&self) -> crate::mesh::DeviceMeshProfile {
        let mut profile = (*self.mesh_profile).clone();
        profile.services = self.advertised_services();
        profile.filesystem_endpoints = self.filesystem_endpoints();
        profile
    }
}

/// Derive the relay transport key from the enrolled device secret. The relay
/// application handshake still proves possession of the certified device key;
/// this domain-separated transport key prevents the relay and filesystem
/// processes from publishing competing address records for one Iroh NodeId.
pub fn relay_service_node_id(device_key: &ed25519_dalek::SigningKey) -> [u8; 32] {
    let service_secret =
        blake3::derive_key("qos-relay-iroh-service-key-v1", &device_key.to_bytes());
    iroh::SecretKey::from_bytes(&service_secret)
        .public()
        .as_bytes()
        .to_owned()
}

/// Bind the relay plane with a domain-separated transport key. This is one
/// physical certified device, not a second user identity: protocol auth uses
/// `device_key` and its device certificate after QUIC connects.
async fn bind_endpoint(
    cfg: &RelayConfig,
    device_key: &ed25519_dalek::SigningKey,
) -> Result<Endpoint, RelayError> {
    let secret = iroh::SecretKey::from_bytes(&blake3::derive_key(
        "qos-relay-iroh-service-key-v1",
        &device_key.to_bytes(),
    ));
    Endpoint::builder(presets::N0)
        .secret_key(secret)
        .bind_addr(cfg.iroh_listen)
        .map_err(|e| RelayError::Iroh(format!("invalid iroh listen address: {e}")))?
        .bind()
        .await
        .map_err(|e| RelayError::Iroh(e.to_string()))
}

/// Open every configured workspace binding on disk and build the
/// shared registry `run_peer` and the peer-relay session loop route
/// through.
fn build_workspace_registry(cfg: &RelayConfig) -> Result<WorkspaceRegistry, RelayError> {
    let mut wsmap: HashMap<u64, SharedWorkspace> = HashMap::new();
    for binding in &cfg.workspaces {
        let mut rt = WorkspaceRuntime::open(
            &cfg.state_root,
            binding.id,
            binding.pin,
            Membership::default(),
        )?;
        rt.admit_authenticated = matches!(cfg.trust_policy, TrustPolicy::AllowMatchingUser);
        rt.admitted_users = binding
            .admitted_users
            .iter()
            .filter_map(|encoded| hex::decode(encoded).ok())
            .filter_map(|bytes| bytes.try_into().ok())
            .collect();
        wsmap.insert(binding.id, Arc::new(AsyncMutex::new(rt)));
    }
    Ok(Arc::new(AsyncMutex::new(wsmap)))
}

/// Spawn the revocation-refresh loop, the metrics-persist loop, and
/// one reconnect-with-backoff task per configured peer relay,
/// returning the abort-on-drop handle list `Relay` owns.
///
/// Each peer-relay task dials via `RelayClient::connect` (same code
/// path any client uses), exchanges `PeerRelayHello`, subscribes to
/// the intersection, then feeds inbound Deltas into the local
/// `WorkspaceRuntime::fanout_delta` — no new fan-out.
fn spawn_background_tasks(
    cfg: &RelayConfig,
    identity: &RelayIdentity,
    endpoint: &Endpoint,
    workspaces: &WorkspaceRegistry,
    metrics: &Arc<Metrics>,
    revocations: Arc<std::sync::RwLock<RevocationList>>,
) -> Arc<std::sync::Mutex<Vec<AbortOnDrop>>> {
    let peer_relay_tasks: Arc<std::sync::Mutex<Vec<AbortOnDrop>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    let revocation_task = spawn_revocation_refresh_task(
        cfg.state_root.clone(),
        identity.revocations.clone(),
        revocations,
    );
    push_peer_relay_task(&peer_relay_tasks, revocation_task);

    let metrics_task = spawn_metrics_persist_task(cfg.state_root.clone(), metrics.clone());
    push_peer_relay_task(&peer_relay_tasks, metrics_task);

    for peer_cfg in cfg.peer_relays.iter().cloned() {
        let task = spawn_peer_relay_dial(
            peer_cfg,
            endpoint.clone(),
            identity.clone(),
            workspaces.clone(),
            metrics.clone(),
        );
        push_peer_relay_task(&peer_relay_tasks, task);
    }

    peer_relay_tasks
}

/// Keep relay handshake admission derived from the same durable signed
/// authority that the filesystem worker materializes. A malformed update is
/// ignored (the last verified policy remains live); an empty valid authority
/// denies every cross-user collaborator on the next handshake.
fn spawn_home_peer_authority_refresh(
    bundle_path: PathBuf,
    own_user: [u8; 32],
    live_policy: Arc<std::sync::RwLock<TrustPolicy>>,
    workspaces: WorkspaceRegistry,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let Ok(text) = std::fs::read_to_string(&bundle_path) else {
                continue;
            };
            let Ok(bundle) = toml::from_str::<fs::full_replica::HomePeerSet>(&text) else {
                continue;
            };
            let mut users = vec![hex::encode(own_user)];
            let mut room_users: HashMap<u64, std::collections::HashSet<[u8; 32]>> = HashMap::new();
            let now = unix_now_secs();
            let mut valid = true;
            for workspace in bundle.workspaces {
                let Ok(ticket) = fs::Ticket::decode(&workspace.ticket) else {
                    valid = false;
                    break;
                };
                let cached = std::fs::read(workspace.cache_dir.join("workspace-authority.bin"))
                    .ok()
                    .and_then(|bytes| postcard::from_bytes(&bytes).ok());
                let Some(authority) = cached.or(workspace.authority) else {
                    valid = false;
                    break;
                };
                if authority
                    .verify(ticket.workspace_id(), authority.governor)
                    .is_err()
                {
                    valid = false;
                    break;
                }
                // Keep a distinct membership set for every relay room.  A
                // Files-path capability is not a shell-room invitation.
                // This is intentionally rebuilt from verified, signed
                // Scope::Room grants on each refresh so revocation removes
                // an existing collaborator from the affected topic.
                if !authority.tombstoned {
                    users.push(hex::encode(authority.governor));
                    for room in &workspace.relay_workspace_ids {
                        room_users
                            .entry(*room)
                            .or_default()
                            .insert(authority.governor);
                    }
                    for capability in &authority.capabilities {
                        let collab_types::Scope::Room(room) = &capability.scope else {
                            continue;
                        };
                        let Ok(room) = room.parse::<u64>() else {
                            continue;
                        };
                        // A signed authority may contain room grants for a
                        // different host; they have no force outside this
                        // bundle's explicit room binding.
                        if !workspace.relay_workspace_ids.contains(&room)
                            || room == crate::personal_continuity_workspace_id(&authority.governor)
                            || capability.effect != collab_types::Effect::Grant
                            || !capability.is_live_at(Some(now))
                        {
                            continue;
                        }
                        let collab_types::Subject::User(user) = &capability.subject else {
                            continue;
                        };
                        users.push(hex::encode(user.as_bytes()));
                        room_users.entry(room).or_default().insert(*user.as_bytes());
                    }
                }
            }
            if valid {
                users.sort_unstable();
                users.dedup();
                if let Ok(mut policy) = live_policy.write() {
                    *policy = TrustPolicy::AllowList { users };
                }
                let runtimes = workspaces.lock().await;
                for (room, users) in room_users {
                    if let Some(runtime) = runtimes.get(&room) {
                        runtime.lock().await.admitted_users = users;
                    }
                }
            }
        }
    })
}

/// The revocation-refresh loop: every second, re-read the on-disk
/// revocation log and, if every entry still applies cleanly against
/// the identity's own base list, swap it in. A malformed entry
/// leaves the previously-live list untouched rather than partially
/// applying.
fn spawn_revocation_refresh_task(
    state_root: PathBuf,
    base_revocations: RevocationList,
    live_revocations: Arc<std::sync::RwLock<RevocationList>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let Ok(entries) = crate::store::load_revocations(&state_root) else {
                continue;
            };
            let mut next = base_revocations.clone();
            let mut valid = true;
            for entry in entries {
                if next.insert(entry).is_err() {
                    valid = false;
                    break;
                }
            }
            if valid && let Ok(mut current) = live_revocations.write() {
                *current = next;
            }
        }
    })
}

/// The metrics-persist loop: snapshot + save to disk every second.
fn spawn_metrics_persist_task(state_root: PathBuf, live_metrics: Arc<Metrics>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = crate::store::save_metrics(&state_root, &live_metrics.snapshot()) {
                tracing::warn!(%error, "could not persist relay metrics snapshot");
            }
        }
    })
}

/// Lock the peer-relay task list, recovering the inner `Vec` if the
/// mutex was poisoned by an earlier panic rather than propagating one
/// here — losing track of a reconnect task would leak it past the
/// owning `Relay`'s lifetime.
fn lock_peer_relay_tasks(
    tasks: &Arc<std::sync::Mutex<Vec<AbortOnDrop>>>,
) -> std::sync::MutexGuard<'_, Vec<AbortOnDrop>> {
    tasks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Register a spawned task on the peer-relay task list (see
/// [`lock_peer_relay_tasks`]).
fn push_peer_relay_task(tasks: &Arc<std::sync::Mutex<Vec<AbortOnDrop>>>, task: JoinHandle<()>) {
    lock_peer_relay_tasks(tasks).push(AbortOnDrop(task));
}

fn validate_relay_identity(identity: &RelayIdentity) -> Result<(), RelayError> {
    verify_device_cert_at(&identity.device_cert, now_ns())
        .map_err(|error| RelayError::Identity(format!("invalid host certificate: {error}")))?;
    if identity.device_cert.user_public_key != identity.user_public_key {
        return Err(RelayError::Identity(
            "host certificate does not chain to the configured user public key".into(),
        ));
    }
    if identity.device_cert.device_public_key
        != identity.device_signing_key.verifying_key().to_bytes()
    {
        return Err(RelayError::Identity(
            "host certificate does not match the configured device signing key".into(),
        ));
    }
    if !identity.device_cert.capabilities.can_act_as_host {
        return Err(RelayError::Identity(
            "device certificate is not host-capable".into(),
        ));
    }
    if identity
        .revocations
        .is_revoked(&identity.device_cert.device_public_key)
    {
        return Err(RelayError::Identity(
            "host device certificate is revoked".into(),
        ));
    }
    Ok(())
}

/// The `ProtocolHandler` iroh wires to the ALPN.
#[derive(Clone)]
struct RelayProtocol {
    workspaces: WorkspaceRegistry,
    trust_policy: Arc<std::sync::RwLock<TrustPolicy>>,
    revocations: Arc<std::sync::RwLock<RevocationList>>,
    own_user_pk: [u8; 32],
    own_cert: DeviceCert,
    own_sk: ed25519_dalek::SigningKey,
    metrics: Arc<Metrics>,
    autobind: Option<WorkspaceAutobind>,
}

#[derive(Clone)]
struct DiscoveryProtocol {
    workspaces: WorkspaceRegistry,
    own_user_pk: [u8; 32],
    trust_policy: Arc<std::sync::RwLock<TrustPolicy>>,
    revocations: Arc<std::sync::RwLock<RevocationList>>,
    own_cert: DeviceCert,
    own_sk: ed25519_dalek::SigningKey,
    mesh_profile: Arc<crate::mesh::DeviceMeshProfile>,
    full_replica_status: PathBuf,
    home_peer_bundle: PathBuf,
}

impl std::fmt::Debug for DiscoveryProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DiscoveryProtocol")
    }
}

impl ProtocolHandler for DiscoveryProtocol {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        let hello: Hello = postcard::from_bytes(&read_frame(&mut recv, MAX_FRAME_BYTES).await?)
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
        let revocations = self
            .revocations
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let peer = auth::accept_hello(
            &hello,
            &self.own_user_pk,
            &self
                .trust_policy
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            &revocations,
            crate::peer::now_ns(),
        )
        .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
        let mut nonce = [0; 32];
        {
            use rand::RngCore;
            rand::thread_rng().fill_bytes(&mut nonce);
        }
        let ack = auth::build_hello_ack(
            self.own_cert.clone(),
            &self.own_sk,
            nonce,
            &hello.client_nonce,
        );
        write_frame(
            &mut send,
            &postcard::to_allocvec(&ack)
                .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?,
        )
        .await
        .map_err(AcceptError::from_err)?;
        let ack2: HelloAck2 = postcard::from_bytes(&read_frame(&mut recv, MAX_FRAME_BYTES).await?)
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
        auth::verify_hello_ack2(&ack2, &peer.device_public_key, &nonce)
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
        let request: ContentEndpointRequest =
            postcard::from_bytes(&read_frame(&mut recv, MAX_FRAME_BYTES).await?)
                .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
        // A discovery request is scoped to a relay room. Do not turn the
        // host into a global filesystem endpoint directory for arbitrary
        // authenticated peers.
        let workspace = self
            .workspaces
            .lock()
            .await
            .get(&request.workspace_id)
            .cloned();
        let permitted = match workspace {
            Some(workspace) => {
                let workspace = workspace.lock().await;
                workspace.admit_authenticated
                    || workspace.admitted_users.contains(&peer.user_public_key)
                    || workspace.membership.contains(&peer.device_public_key)
            }
            None => false,
        };
        if !permitted {
            return Err(AcceptError::from_err(std::io::Error::other(
                "workspace is not bound or not authorised on this relay",
            )));
        }
        let mut profile = (*self.mesh_profile).clone();
        let proof = full_replica_proof(&self.full_replica_status);
        profile.filesystem_endpoints = proof
            .as_ref()
            .map(|proof| {
                proof
                    .filesystem_endpoints
                    .iter()
                    .filter(|endpoint| endpoint.id.as_bytes() == &profile.device)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        profile.services.full_content_replication &= !profile.filesystem_endpoints.is_empty();
        let filesystem_ticket = std::fs::read_to_string(&self.home_peer_bundle)
            .ok()
            .and_then(|text| toml::from_str::<fs::full_replica::HomePeerSet>(&text).ok())
            .and_then(|set| {
                set.workspaces
                    .into_iter()
                    .find(|bundle| bundle.relay_workspace_ids.contains(&request.workspace_id))
            })
            .map(|bundle| bundle.ticket)
            .ok_or_else(|| {
                AcceptError::from_err(std::io::Error::other(
                    "workspace has no authenticated filesystem binding",
                ))
            })?;
        write_frame(
            &mut send,
            &postcard::to_allocvec(&ContentEndpointAdvertisement {
                schema_version: 1,
                workspace_id: request.workspace_id,
                filesystem_endpoints: profile.filesystem_endpoints,
                filesystem_ticket,
            })
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?,
        )
        .await
        .map_err(AcceptError::from_err)?;
        send.finish().map_err(AcceptError::from_err)?;
        connection.closed().await;
        Ok(())
    }
}

impl std::fmt::Debug for RelayProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RelayProtocol")
    }
}

impl ProtocolHandler for RelayProtocol {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let this = self.clone();
        let (send, recv) = connection.accept_bi().await?;
        // Run the whole session in a separate task so we return
        // promptly and the iroh router can accept the next
        // connection.
        tokio::spawn(async move {
            if let Err(e) = handle_conn(this, connection, send, recv).await {
                tracing::debug!("relay conn ended: {e}");
            }
        });
        Ok(())
    }
}

async fn handle_conn(
    ctx: RelayProtocol,
    connection: iroh::endpoint::Connection,
    mut send: SendStream,
    mut recv: RecvStream,
) -> Result<(), RelayError> {
    ctx.metrics.inc_conns_accepted();
    // 1. Read the client Hello.
    let hello_bytes = read_frame(&mut recv, MAX_FRAME_BYTES)
        .await
        .map_err(|e| RelayError::Iroh(format!("read hello: {e}")))?;
    let hello: Hello = postcard::from_bytes(&hello_bytes)
        .map_err(|e| RelayError::Iroh(format!("decode hello: {e}")))?;
    let now_ns = crate::peer::now_ns();
    let revs = ctx
        .revocations
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let client_peer = match auth::accept_hello(
        &hello,
        &ctx.own_user_pk,
        &ctx.trust_policy
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        &revs,
        now_ns,
    ) {
        Ok(p) => p,
        Err(e) => {
            ctx.metrics.inc_auth_rejected();
            return Err(RelayError::Iroh(format!("auth reject: {e}")));
        }
    };
    // 2. Reply with HelloAck.
    let mut server_nonce = [0u8; 32];
    {
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut server_nonce);
    }
    let ack = auth::build_hello_ack(
        ctx.own_cert.clone(),
        &ctx.own_sk,
        server_nonce,
        &hello.client_nonce,
    );
    let ack_bytes =
        postcard::to_allocvec(&ack).map_err(|e| RelayError::Iroh(format!("encode ack: {e}")))?;
    write_frame(&mut send, &ack_bytes)
        .await
        .map_err(|e| RelayError::Iroh(format!("write ack: {e}")))?;
    // 3. Read HelloAck2.
    let client_ack2_bytes = read_frame(&mut recv, MAX_FRAME_BYTES)
        .await
        .map_err(|e| RelayError::Iroh(format!("read ack2: {e}")))?;
    let ack2: HelloAck2 = postcard::from_bytes(&client_ack2_bytes)
        .map_err(|e| RelayError::Iroh(format!("decode ack2: {e}")))?;
    if let Err(e) = auth::verify_hello_ack2(&ack2, &client_peer.device_public_key, &server_nonce) {
        ctx.metrics.inc_auth_rejected();
        return Err(RelayError::Iroh(format!("auth ack2 reject: {e}")));
    }

    ctx.metrics.inc_conns_authed();

    // 4. Hand off to the peer loop.
    let peer_result = run_peer(
        client_peer,
        connection.clone(),
        recv,
        send,
        PeerContext {
            workspaces: ctx.workspaces.clone(),
            autobind: ctx.autobind,
            revocations: ctx.revocations,
            trust_policy: ctx.trust_policy,
            own_user_public_key: ctx.own_user_pk,
            metrics: ctx.metrics,
        },
    )
    .await;
    if let Err(e) = peer_result {
        tracing::debug!("peer session ended with error: {e}");
    }
    connection.close(0u32.into(), b"relay session ended");
    Ok(())
}

/// Read a 4-byte-LE-prefix framed message from `recv`.
///
/// # Errors
///
/// Returns an error if the stream closes or errors mid-read, or if the
/// decoded length prefix is `0` or exceeds `max` — the latter guards
/// against a peer claiming an unbounded frame.
pub async fn read_frame(recv: &mut RecvStream, max: usize) -> Result<Vec<u8>, std::io::Error> {
    let mut prefix = [0u8; 4];
    recv.read_exact(&mut prefix)
        .await
        .map_err(std::io::Error::other)?;
    let n = u32::from_le_bytes(prefix) as usize;
    if n == 0 || n > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame size {n} out of range"),
        ));
    }
    let mut buf = vec![0u8; n];
    recv.read_exact(&mut buf)
        .await
        .map_err(std::io::Error::other)?;
    Ok(buf)
}

/// Write a 4-byte-LE-prefix framed message to `send`.
///
/// # Errors
///
/// Returns an error if the stream write fails, or if `data` is longer
/// than `u32::MAX` bytes and so cannot be represented in the length
/// prefix.
pub async fn write_frame(send: &mut SendStream, data: &[u8]) -> Result<(), std::io::Error> {
    let len = u32::try_from(data.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "frame too large to encode a u32 length prefix",
        )
    })?;
    send.write_all(&len.to_le_bytes()).await?;
    if !data.is_empty() {
        send.write_all(data).await?;
    }
    Ok(())
}

/// Spawn the outbound reconnect-with-backoff task for one peer
/// relay identified by its config entry. Parses the iroh id and
/// hands off to [`spawn_peer_relay_dial_at`].
fn spawn_peer_relay_dial(
    peer_cfg: PeerRelayConfig,
    endpoint: Endpoint,
    identity: RelayIdentity,
    workspaces: WorkspaceRegistry,
    metrics: Arc<Metrics>,
) -> JoinHandle<()> {
    let node_id = parse_iroh_node_id(&peer_cfg.iroh_node_id);
    let label = peer_cfg.label.clone();
    let priority = peer_cfg.priority;
    tokio::spawn(async move {
        let Some(node_id) = node_id else {
            tracing::warn!(?peer_cfg.iroh_node_id, "peer relay iroh_node_id not parseable, skipping");
            return;
        };
        let addr = EndpointAddr::new(node_id);
        peer_relay_dial_loop(
            addr,
            label,
            priority,
            endpoint,
            identity.user_public_key,
            identity.device_cert,
            identity.device_signing_key,
            identity.revocations,
            workspaces,
            metrics,
        )
        .await;
    })
}

/// Spawn the outbound reconnect-with-backoff task for one peer
/// relay, with a fully-resolved endpoint address. The task loops
/// forever (until the returned join handle is aborted) — reconnect
/// gap is `1s, 2s, 4s, 8s, …, capped at 60s`. While connected it
/// holds a `RelayClient` session and mirrors every inbound Delta
/// into the local workspace runtime via the SAME `fanout_delta`
/// device peers use — that's the reuse: no separate federation
/// delivery path.
#[allow(clippy::too_many_arguments)]
fn spawn_peer_relay_dial_at(
    addr: EndpointAddr,
    label: String,
    priority: i32,
    endpoint: Endpoint,
    own_user_pk: [u8; 32],
    own_cert: DeviceCert,
    own_sk: ed25519_dalek::SigningKey,
    revocations: RevocationList,
    workspaces: WorkspaceRegistry,
    metrics: Arc<Metrics>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        peer_relay_dial_loop(
            addr,
            label,
            priority,
            endpoint,
            own_user_pk,
            own_cert,
            own_sk,
            revocations,
            workspaces,
            metrics,
        )
        .await;
    })
}

#[allow(clippy::too_many_arguments)]
async fn peer_relay_dial_loop(
    addr: EndpointAddr,
    label: String,
    _priority: i32,
    endpoint: Endpoint,
    own_user_pk: [u8; 32],
    own_cert: DeviceCert,
    own_sk: ed25519_dalek::SigningKey,
    revocations: RevocationList,
    workspaces: WorkspaceRegistry,
    _metrics: Arc<Metrics>,
) {
    let mut backoff_ms: u64 = 1_000;
    loop {
        let ep = endpoint.clone();
        match RelayClient::connect(
            ep,
            addr.clone(),
            own_user_pk,
            own_cert.clone(),
            own_sk.clone(),
            revocations.clone(),
        )
        .await
        {
            Ok(client) => {
                tracing::info!(%label, "peer relay federated");
                backoff_ms = 1_000;
                if let Err(e) = run_peer_relay_session(&client, &workspaces).await {
                    tracing::debug!(%label, "peer relay session ended: {e}");
                }
            }
            Err(e) => {
                tracing::debug!(%label, "peer relay dial failed: {e}");
            }
        }
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        backoff_ms = backoff_ms.saturating_mul(2).min(60_000);
    }
}

/// One iteration of the federated peer-relay session. Sends a
/// `PeerRelayHello` with the workspaces we host, waits for the
/// peer's `PeerRelayHello`, `Subscribe`s to the intersection, then
/// drains inbound Deltas + Presence into the LOCAL workspace
/// runtime via `fanout_delta` / `fanout_presence` — the very same
/// entry points a device peer's incoming envelope hits.
async fn run_peer_relay_session(
    client: &RelayClient,
    workspaces: &WorkspaceRegistry,
) -> Result<(), String> {
    let hosted_local = advertise_hosted_workspaces(client, workspaces).await?;
    let intersection = negotiate_intersection(client, &hosted_local).await?;
    subscribe_to_intersection(client, &intersection)?;
    drain_peer_relay_session(client, workspaces, &intersection).await
}

/// Step 1: advertise what we host, returning the list sent — needed
/// again once the peer replies with its own `PeerRelayHello` so we
/// can compute the intersection.
async fn advertise_hosted_workspaces(
    client: &RelayClient,
    workspaces: &WorkspaceRegistry,
) -> Result<Vec<u64>, String> {
    let hosted_local: Vec<u64> = {
        let map = workspaces.lock().await;
        map.keys().copied().collect()
    };
    client
        .send(Envelope::PeerRelayHello {
            hosted_workspaces: hosted_local.clone(),
        })
        .map_err(|_| "outbound closed".to_string())?;
    Ok(hosted_local)
}

/// Step 2: wait (bounded — 25 polls of 200ms) for the peer's
/// `PeerRelayHello` and compute the intersection with what we host
/// locally.
async fn negotiate_intersection(
    client: &RelayClient,
    hosted_local: &[u64],
) -> Result<Vec<u64>, String> {
    let mut intersection: Vec<u64> = Vec::new();
    let mut hello_seen = false;
    for _ in 0..25 {
        let env = tokio::time::timeout(Duration::from_millis(200), client.recv()).await;
        match env {
            Ok(Some(Envelope::PeerRelayHello { hosted_workspaces })) => {
                let local: std::collections::HashSet<u64> = hosted_local.iter().copied().collect();
                for id in hosted_workspaces {
                    if local.contains(&id) {
                        intersection.push(id);
                    }
                }
                hello_seen = true;
                break;
            }
            Ok(Some(other)) => {
                // Not the hello we wanted; buffer nothing — plain
                // devices should not talk on this session. Just log.
                tracing::debug!("peer-relay session got unexpected pre-hello envelope: {other:?}");
            }
            Ok(None) => return Err("connection closed pre-hello".into()),
            Err(_) => {} // timeout; keep waiting a bit
        }
    }
    if !hello_seen {
        return Err("peer relay never sent PeerRelayHello".into());
    }
    Ok(intersection)
}

/// Step 3: subscribe to each intersection workspace. This is a plain
/// `Subscribe` envelope — the accepter side's `handle_envelope`
/// registers us as a subscriber-with-role-PeerRelay (because our own
/// cert has `can_act_as_host = true`, that role was stamped at
/// handshake time).
fn subscribe_to_intersection(client: &RelayClient, intersection: &[u64]) -> Result<(), String> {
    for id in intersection {
        client
            .send(Envelope::Subscribe { workspace_id: *id })
            .map_err(|_| "outbound closed on subscribe".to_string())?;
    }
    Ok(())
}

/// Step 4: drain inbound envelopes for the life of this connection AND
/// periodically run anti-entropy against the upstream relay.
///
/// Live fan-out (Delta / Presence) is forwarded LOCALLY via the same
/// `fanout_delta` / `fanout_presence` device peers use; `PeerRole::PeerRelay`
/// suppresses re-echo to other peer relays.
///
/// Anti-entropy is the recovery tier layered on top: on a timer we
/// advertise our local materialized heads (`SyncRequest`) so the
/// upstream backfills any change we are missing. This heals two cases
/// live fan-out cannot: a change kept off the live path by
/// echo-suppression (the middle hop of a relay chain — a relay does
/// not re-forward a PeerRelay-origin delta to another peer relay), and
/// anything missed across a reconnect/restart. A converged pair
/// exchanges empty `SyncResponse`s, so this quiesces to near-zero work
/// once caught up.
async fn drain_peer_relay_session(
    client: &RelayClient,
    workspaces: &WorkspaceRegistry,
    intersection: &[u64],
) -> Result<(), String> {
    let mut resync = tokio::time::interval(Duration::from_millis(500));
    resync.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            // `interval` fires its first tick immediately, so the initial
            // resync goes out as soon as we enter the loop.
            _ = resync.tick() => {
                for id in intersection {
                    send_sync_request_for(client, workspaces, *id).await;
                }
            }
            maybe = client.recv() => {
                let Some(env) = maybe else { return Ok(()) }; // clean EOF
                handle_peer_relay_envelope(workspaces, client, env).await;
            }
        }
    }
}

/// Apply one inbound envelope from the federated peer-relay session:
/// fan `Delta`/`Presence` out to local subscribers, materialize a
/// `SyncResponse` backfill, or handle protocol housekeeping (`Ping`,
/// `NotAuthorised`).
async fn handle_peer_relay_envelope(
    workspaces: &WorkspaceRegistry,
    client: &RelayClient,
    env: Envelope,
) {
    match &env {
        Envelope::Delta {
            workspace_id,
            author_device_public_key,
            delta,
            ..
        } => {
            let ws = {
                let map = workspaces.lock().await;
                map.get(workspace_id).cloned()
            };
            if let Some(ws) = ws {
                let mut wr = ws.lock().await;
                let raw = delta.clone();
                let seq = now_ns();
                let author = *author_device_public_key;
                let _ = wr.fanout_delta(&author, PeerRole::PeerRelay, &env, &raw, seq);
            }
        }
        Envelope::Presence { workspace_id, .. } => {
            let ws = {
                let map = workspaces.lock().await;
                map.get(workspace_id).cloned()
            };
            if let Some(ws) = ws {
                // Presence has no persistent author field on the wire;
                // use zero as a sentinel — presence fanout only
                // compares to subscriber pubkeys and the "don't echo
                // to peer relays" filter.
                ws.lock()
                    .await
                    .fanout_presence(&[0u8; 32], PeerRole::PeerRelay, &env);
            }
        }
        Envelope::SyncResponse {
            workspace_id,
            delta_since,
        } => {
            // The upstream backfilled changes we were missing.
            // Materialize them into our pinned doc and fan the batch
            // out to our LOCAL devices so they converge — even when
            // echo-suppression kept these changes off the live
            // fan-out path.
            if !delta_since.is_empty() {
                let ws = {
                    let map = workspaces.lock().await;
                    map.get(workspace_id).cloned()
                };
                if let Some(ws) = ws {
                    let seq = now_ns();
                    let _ = ws
                        .lock()
                        .await
                        .ingest_sync_changes(*workspace_id, delta_since, seq);
                }
            }
        }
        Envelope::NotAuthorised {
            workspace_id,
            reason,
        } => {
            tracing::warn!(workspace_id, %reason, "peer relay refused subscribe");
        }
        Envelope::Ping => {
            let _ = client.send(Envelope::Pong);
        }
        _ => {}
    }
}

/// Advertise our local materialized heads for `ws_id` to the upstream
/// peer relay via a [`Envelope::SyncRequest`]. Best-effort: a workspace
/// we don't host, or a closed outbound, is silently skipped.
async fn send_sync_request_for(client: &RelayClient, workspaces: &WorkspaceRegistry, ws_id: u64) {
    let have_heads = {
        let map = workspaces.lock().await;
        match map.get(&ws_id) {
            Some(ws) => ws.lock().await.current_heads(),
            None => return,
        }
    };
    let _ = client.send(Envelope::SyncRequest {
        workspace_id: ws_id,
        have_heads,
    });
}

/// Parse the hex-encoded iroh node id string into an iroh
/// `PublicKey`. Accepts upper/lowercase.
fn parse_iroh_node_id(hex_id: &str) -> Option<PublicKey> {
    let bytes = hex::decode(hex_id).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    PublicKey::from_bytes(&arr).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_user_identity::{DeviceCapabilities, DeviceIdentity, UserIdentity, sign_device_cert};

    fn identity(capabilities: DeviceCapabilities) -> RelayIdentity {
        let (root, user) = UserIdentity::generate("test user", 1);
        let (device_signing_key, device) = DeviceIdentity::generate("test host", 2);
        let mut device_cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: device.public_key,
            device_label: device.label,
            capabilities,
            issued_at_ns: 3,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut device_cert, &root).unwrap();
        RelayIdentity {
            user_public_key: user.public_key,
            device_signing_key,
            device_cert,
            revocations: RevocationList::new(),
        }
    }

    #[test]
    fn valid_host_identity_passes_startup_validation() {
        validate_relay_identity(&identity(DeviceCapabilities::host())).unwrap();
    }

    #[test]
    fn ordinary_device_cannot_start_a_relay() {
        let error = validate_relay_identity(&identity(DeviceCapabilities::member())).unwrap_err();
        assert!(error.to_string().contains("not host-capable"));
    }

    #[test]
    fn mismatched_device_key_is_rejected_at_startup() {
        let mut relay = identity(DeviceCapabilities::host());
        relay.device_signing_key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
        let error = validate_relay_identity(&relay).unwrap_err();
        assert!(error.to_string().contains("does not match"));
    }

    #[tokio::test]
    async fn composite_relay_clamps_only_unhosted_service_advertisements() {
        let state = tempfile::tempdir().unwrap();
        let relay = Relay::start(
            RelayConfig {
                state_root: state.path().to_path_buf(),
                services: crate::mesh::ServiceCapabilities::personal_server(),
                ..RelayConfig::default()
            },
            identity(DeviceCapabilities::host()),
        )
        .await
        .unwrap();

        let services = relay.handle().mesh_profile().services;
        assert!(services.workspace_replication);
        assert!(services.inbound_relay);
        assert!(!services.full_content_replication);
        assert!(!services.app_serving);
        assert!(!services.compute);
        assert!(!services.remote_control_relay);
        relay.shutdown().await;
    }

    #[test]
    fn bundle_imports_the_shared_workspace_shard_as_full_replication() {
        let temp = tempfile::tempdir().unwrap();
        let status = temp.path().join("status.json");
        let bundle_path = temp.path().join("home-peer.toml");
        let ticket = fs::Ticket {
            node: EndpointAddr::new(iroh::SecretKey::from_bytes(&[7; 32]).public()),
            workspace: [9; 32],
        };
        let bundle = fs::full_replica::HomePeerSet {
            status_file: status.clone(),
            workspaces: vec![fs::full_replica::HomePeerBundle {
                ticket: ticket.encode().unwrap(),
                cache_dir: temp.path().join("cache"),
                capability_json: "[]".into(),
                authority: None,
                principals: Vec::new(),
                relay_workspace_ids: vec![42],
            }],
        };
        std::fs::write(&bundle_path, toml::to_string(&bundle).unwrap()).unwrap();
        let mut config = RelayConfig {
            home_peer_bundle: bundle_path,
            full_replica_status: status,
            ..RelayConfig::default()
        };

        import_home_peer_bindings(&mut config).unwrap();
        assert!(config.services.full_content_replication);
        assert!(config.workspaces.iter().any(|binding| {
            binding.id == 42
                && binding.pin
                && binding.replication == crate::mesh::ReplicationIntent::FullWorkspace
        }));
    }

    #[test]
    fn room_scoped_authority_admits_only_its_signed_room() {
        use collab_types::{CapOverride, Effect, Expiry, Scope, Subject, Verb};

        let temp = tempfile::tempdir().unwrap();
        let status = temp.path().join("status.json");
        let bundle_path = temp.path().join("home-peer.toml");
        let governor = ed25519_dalek::SigningKey::from_bytes(&[0x31; 32]);
        let collaborator_a = [0x32; 32];
        let path_only_collaborator = [0x33; 32];
        let ticket = fs::Ticket {
            node: EndpointAddr::new(iroh::SecretKey::from_bytes(&[7; 32]).public()),
            workspace: [9; 32],
        };
        let authority = fs::workspace_authority::WorkspaceAuthorityEnvelope::sign(
            ticket.workspace_id(),
            1,
            vec![
                // The only capability that maps to a relay topic.
                CapOverride::new(
                    Subject::User(collab_types::Ed25519PublicKey(collaborator_a)),
                    Scope::Room("42".into()),
                    Effect::Grant,
                    Verb::Read,
                ),
                // A Files grant is intentionally insufficient to subscribe
                // to any room, including unrelated room 43.
                CapOverride::new(
                    Subject::User(collab_types::Ed25519PublicKey(path_only_collaborator)),
                    Scope::FsPath("/".into()),
                    Effect::Grant,
                    Verb::Read,
                ),
                // Expired room grants must not resurrect a former member
                // merely because the NAS reloads the signed authority.
                CapOverride::new(
                    Subject::User(collab_types::Ed25519PublicKey(collaborator_a)),
                    Scope::Room("43".into()),
                    Effect::Grant,
                    Verb::Read,
                )
                .expiring(Expiry::AtUnix(0)),
            ],
            false,
            &governor,
        )
        .unwrap();
        let continuity = crate::personal_continuity_workspace_id(&authority.governor);
        let bundle = fs::full_replica::HomePeerSet {
            status_file: status.clone(),
            workspaces: vec![fs::full_replica::HomePeerBundle {
                ticket: ticket.encode().unwrap(),
                cache_dir: temp.path().join("cache"),
                capability_json: "[]".into(),
                authority: Some(authority.clone()),
                principals: Vec::new(),
                relay_workspace_ids: vec![42, 43, continuity],
            }],
        };
        std::fs::write(&bundle_path, toml::to_string(&bundle).unwrap()).unwrap();
        let mut config = RelayConfig {
            home_peer_bundle: bundle_path,
            full_replica_status: status,
            ..RelayConfig::default()
        };
        import_home_peer_bindings(&mut config).unwrap();
        let normal = config
            .workspaces
            .iter()
            .find(|binding| binding.id == 42)
            .unwrap();
        assert!(normal.admitted_users.contains(&hex::encode(collaborator_a)));
        assert!(
            !normal
                .admitted_users
                .contains(&hex::encode(path_only_collaborator))
        );
        let unrelated = config
            .workspaces
            .iter()
            .find(|binding| binding.id == 43)
            .unwrap();
        assert_eq!(
            unrelated.admitted_users,
            vec![hex::encode(authority.governor)]
        );
        let private = config
            .workspaces
            .iter()
            .find(|binding| binding.id == continuity)
            .unwrap();
        assert_eq!(
            private.admitted_users,
            vec![hex::encode(authority.governor)]
        );
    }

    #[tokio::test]
    async fn ready_profile_serializes_the_direct_filesystem_endpoint() {
        let temp = tempfile::tempdir().unwrap();
        let status_path = temp.path().join("full-replica.json");
        let relay_identity = identity(DeviceCapabilities::host());
        let filesystem_endpoint = EndpointAddr::new(
            iroh::SecretKey::from_bytes(&relay_identity.device_signing_key.to_bytes()).public(),
        )
        .with_ip_addr("127.0.0.1:43210".parse().unwrap());
        let mut status = fs::full_replica::FullReplicaStatus::syncing();
        status.state = "ready".into();
        status.full_content_replication = true;
        status
            .filesystem_endpoints
            .push(filesystem_endpoint.clone());
        fs::full_replica::save_status(&status_path, &status).unwrap();
        let relay = Relay::start(
            RelayConfig {
                state_root: temp.path().join("relay-state"),
                full_replica_status: status_path,
                device_role: crate::mesh::DeviceRole::AlwaysOnServer,
                services: crate::mesh::ServiceCapabilities::home_peer(),
                ..RelayConfig::default()
            },
            relay_identity,
        )
        .await
        .unwrap();

        let profile = relay.handle().mesh_profile();
        let wire = serde_json::to_vec(&profile).unwrap();
        let published: crate::mesh::DeviceMeshProfile = serde_json::from_slice(&wire).unwrap();
        assert_eq!(published.content_endpoint(), Some(filesystem_endpoint));
        assert!(published.services.full_content_replication);
        relay.shutdown().await;
    }

    #[tokio::test]
    async fn authenticated_discovery_returns_filesystem_not_relay_endpoint() {
        let temp = tempfile::tempdir().unwrap();
        let (root, user) = UserIdentity::generate("owner", 1);
        let (host_key, host) = DeviceIdentity::generate("home", 2);
        let (client_key, client) = DeviceIdentity::generate("laptop", 3);
        let mut host_cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: host.public_key,
            device_label: host.label,
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: 4,
            expires_at_ns: None,
            signature: [0; 64],
        };
        let mut client_cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: client.public_key,
            device_label: client.label,
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 4,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut host_cert, &root).unwrap();
        sign_device_cert(&mut client_cert, &root).unwrap();
        let filesystem =
            EndpointAddr::new(iroh::SecretKey::from_bytes(&host_key.to_bytes()).public())
                .with_ip_addr("127.0.0.1:40123".parse().unwrap());
        let status_path = temp.path().join("status.json");
        let mut status = fs::full_replica::FullReplicaStatus::syncing();
        status.state = "ready".into();
        status.full_content_replication = true;
        status.filesystem_endpoints.push(filesystem.clone());
        fs::full_replica::save_status(&status_path, &status).unwrap();
        let owner_endpoint = EndpointAddr::new(iroh::SecretKey::from_bytes(&[91; 32]).public());
        let workspace_topic = [29; 32];
        let bundle_path = temp.path().join("home-peer.toml");
        let bundle = fs::full_replica::HomePeerSet {
            status_file: status_path.clone(),
            workspaces: vec![fs::full_replica::HomePeerBundle {
                ticket: fs::Ticket {
                    node: owner_endpoint.clone(),
                    workspace: workspace_topic,
                }
                .encode()
                .unwrap(),
                cache_dir: temp.path().join("replica"),
                capability_json: "[]".into(),
                authority: None,
                principals: Vec::new(),
                relay_workspace_ids: vec![0],
            }],
        };
        std::fs::write(&bundle_path, toml::to_string(&bundle).unwrap()).unwrap();
        let relay = Relay::start(
            RelayConfig {
                state_root: temp.path().join("relay"),
                full_replica_status: status_path,
                home_peer_bundle: bundle_path,
                device_role: crate::mesh::DeviceRole::AlwaysOnServer,
                services: crate::mesh::ServiceCapabilities::home_peer(),
                workspaces: vec![crate::config::WorkspaceBinding {
                    id: 0,
                    pin: true,
                    replication: crate::mesh::ReplicationIntent::FullWorkspace,
                    admitted_users: vec![hex::encode(user.public_key)],
                }],
                ..RelayConfig::default()
            },
            RelayIdentity {
                user_public_key: user.public_key,
                device_signing_key: host_key,
                device_cert: host_cert,
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        assert_ne!(
            relay.node_addr().id.as_bytes(),
            host.public_key.as_slice(),
            "relay transport discovery must not collide with the filesystem node"
        );
        let client_endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let discovered = crate::client::RelayClient::discover_content_endpoint(
            &client_endpoint,
            relay.node_addr(),
            0,
            user.public_key,
            host.public_key,
            client_cert.clone(),
            client_key.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap();
        assert_eq!(discovered, filesystem);
        assert_ne!(discovered, relay.node_addr());
        let registration = crate::client::RelayClient::discover_content_peer(
            &client_endpoint,
            relay.node_addr(),
            0,
            user.public_key,
            host.public_key,
            client_cert.clone(),
            client_key.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap();
        let advertised_ticket = fs::Ticket::decode(&registration.ticket).unwrap();
        assert_ne!(owner_endpoint, filesystem);
        assert_eq!(advertised_ticket.node, owner_endpoint);
        assert_ne!(advertised_ticket.node, registration.endpoint);
        assert_eq!(advertised_ticket.workspace, workspace_topic);
        let wrong_device = [0x55; 32];
        assert!(
            crate::client::RelayClient::discover_content_endpoint(
                &client_endpoint,
                relay.node_addr(),
                0,
                user.public_key,
                wrong_device,
                // The client certificate remains valid; only the enrolled host
                // device pin changes, proving discovery does not accept an
                // arbitrary host certificate at the derived service NodeId.
                client_cert.clone(),
                client_key.clone(),
                RevocationList::new(),
            )
            .await
            .is_err()
        );
        client_endpoint.close().await;
        relay.shutdown().await;
    }
}
