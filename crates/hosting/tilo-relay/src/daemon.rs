//! The long-running daemon: binds an iroh endpoint, accepts on
//! `tilo-relay/1`, runs the auth handshake, hands off to
//! [`crate::peer::run_peer`].
//!
//! Also exposes the `Relay` struct so integration tests can spin up
//! two relays in the same process and let them merge.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use iroh::endpoint::{Endpoint, presets};
use iroh::protocol::{AcceptError, DynProtocolHandler, ProtocolHandler, Router};
use iroh_base::{EndpointAddr, PublicKey};
use tilo_clock::Clock;
use tokio::task::JoinHandle;

use tilo_user_identity::{RevocationList, verify_revocation_entry};

use crate::compute_session::ComputeProtocol;
use crate::compute_transport::COMPUTE_ALPN;
use crate::compute_worker::{ProductionComputeWorkerConfig, run_production_compute_worker};
use crate::config::{PeerRelayConfig, RelayConfig};
use crate::handshake::{EndpointTrust, LiveIdentity};
use crate::host::RelayProtocol;
use crate::metrics::Metrics;
use crate::peer::{WorkspaceAutobind, WorkspaceRegistry, get_or_autobind_workspace};
use crate::policy::TrustPolicy;
use crate::transport::{
    ALPN, ContentEndpointAdvertisement, ContentEndpointRequest, DISCOVERY_ALPN,
    HomePeerManagementRequest, HomePeerManagementResponse, MANAGEMENT_ALPN, MAX_FRAME_BYTES,
};
use crate::workspace::SharedWorkspace;
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_room_host::federation::{PeerRelayDialer, Upstream};

#[derive(serde::Deserialize)]
struct FullReplicaStatusProof {
    state: String,
    full_content_replication: bool,
    updated_at_ms: u64,
    #[serde(default)]
    filesystem_endpoints: Vec<EndpointAddr>,
}

/// The headless replica's completeness proof: the status file it writes,
/// honoured only while the replica that wrote it is alive. The running
/// replica holds its liveness lock beside the file
/// ([`fs::full_replica::replica_is_running`]), which the kernel releases
/// when it exits however it exits; a stamp recent by `clock` also counts,
/// for a replica that keeps no lock (#4224: the replica used to restamp
/// the file every second to stay recent).
#[derive(Clone)]
struct ReplicaProof {
    path: PathBuf,
    clock: Arc<dyn Clock>,
}

impl ReplicaProof {
    fn current(&self) -> Option<FullReplicaStatusProof> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<FullReplicaStatusProof>(&bytes).ok())
            .filter(|proof| {
                proof.state == "ready"
                    && proof.full_content_replication
                    && !proof.filesystem_endpoints.is_empty()
                    && (self.clock.now_ms().saturating_sub(proof.updated_at_ms) <= 5_000
                        || fs::full_replica::replica_is_running(&self.path))
            })
    }
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
    /// Governed workspace admission could not be reconciled safely.
    #[error("governed access: {0}")]
    GovernedAccess(String),
    /// An additional endpoint protocol conflicts with the relay router.
    #[error("protocol registration: {0}")]
    ProtocolRegistration(String),
}

impl From<crate::host::HostError> for RelayError {
    fn from(error: crate::host::HostError) -> Self {
        use crate::host::HostError;
        match error {
            HostError::Identity(reason) => Self::Identity(reason),
            HostError::Store(error) => Self::Store(error),
            HostError::Io(error) => Self::Io(error),
            HostError::GovernedAccess(reason) => Self::GovernedAccess(reason),
            HostError::Rooms(reason) => Self::Identity(format!("governed rooms: {reason}")),
            HostError::Handshake(reason) => Self::Iroh(reason),
        }
    }
}

/// The identity a relay runs with: a host's.
pub use crate::host::HostIdentity as RelayIdentity;

/// What an embedder plugs into a relay it starts.
struct Planes {
    /// Answers remote-action frames addressed to this host, see
    /// [`Relay::start_with_app_serving`].
    app_serving: Option<Arc<dyn crate::app_serving::AppServingPlane>>,
    /// Classifies co-edit bodies, see [`Relay::start_with_shell_payloads`].
    shell_payloads: Arc<dyn crate::payloads::ShellPayloads>,
}

impl Planes {
    /// The desktop's payload adapter, with `app_serving` if any.
    fn desktop(app_serving: Option<Arc<dyn crate::app_serving::AppServingPlane>>) -> Self {
        Self {
            app_serving,
            shell_payloads: crate::payloads::neighbour(),
        }
    }
}

/// A running relay. Prefer [`Relay::shutdown`]; dropping the handle also
/// stops the daemon and every federation dial task, but leaves peers to
/// notice by timeout.
pub struct Relay {
    pub endpoint: Endpoint,
    /// Iroh router that wires the ALPN → protocol handler.
    router: Router,
    /// The room host this relay is: its workspaces, the handshake that
    /// admits their members and every member's session.
    sessions: crate::host::HostSessions,
    /// Only the browser listener publishes this public navigation origin.
    #[cfg(feature = "web")]
    pub(crate) browser_origin: Arc<std::sync::RwLock<Option<Arc<String>>>>,
    /// Federation dial tasks — one per known peer relay. Aborted
    /// (per-handle) when the `Relay` drops.
    peer_relay_tasks: Arc<std::sync::Mutex<Vec<AbortOnDrop>>>,
    /// Effective same-identity replication and serving policy, safe to expose
    /// to the shell/fs orchestrators because service claims were intersected
    /// with the verified certificate at startup.
    mesh_profile: Arc<crate::mesh::DeviceMeshProfile>,
    /// Atomic filesystem completeness proof shared with the headless replica.
    full_replica: ReplicaProof,
    /// What the browser carriers need to serve sessions of this relay.
    web: Box<WebContext>,
    /// Live proof that the owned signed-package compute worker completed all
    /// health checks and still owns its authenticated session.
    compute_ready: Arc<AtomicBool>,
    /// The rooms this relay governs; absent when it hosts no room authority.
    rooms: Option<crate::rooms::Rooms>,
    /// Sealing what the rooms it stewards hold in the clear, started when
    /// its operator decided it ([`crate::reseal`]).
    #[cfg(feature = "agents")]
    resealing: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// The relay state the browser carriers serve sessions with.
#[derive(Clone)]
pub(crate) struct WebContext {
    pub(crate) relay: RelayProtocol,
    #[cfg(feature = "web")]
    pub(crate) certification: crate::certification::CertificationProtocol,
    /// The device of the session beside this node, once named.
    pub(crate) node_session: Arc<std::sync::OnceLock<[u8; 32]>>,
}

impl Relay {
    #[cfg(feature = "web")]
    pub(crate) fn web_context(&self) -> &WebContext {
        &self.web
    }

    /// Name the device of the session beside this node (the shell that runs
    /// the person's apps), which pages address to open apps there. Named
    /// once; a later call keeps the first.
    pub fn set_node_session(&self, device: [u8; 32]) {
        let _ = self.web.node_session.set(device);
    }
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
    full_replica: ReplicaProof,
    compute_ready: Arc<AtomicBool>,
}

impl Relay {
    /// Start on an explicitly selected packet relay without public address lookup.
    ///
    /// Device certificate validation, workspace membership and relay/1 framing
    /// remain identical to [`Self::start`]. The default constructor retains N0.
    ///
    /// # Errors
    /// Returns the same identity, storage and endpoint errors as [`Self::start`].
    pub async fn start_with_relay_mode(
        cfg: RelayConfig,
        identity: RelayIdentity,
        relay_mode: iroh::RelayMode,
    ) -> Result<Self, RelayError> {
        Box::pin(Self::start_inner(
            cfg,
            identity,
            Some(relay_mode),
            None,
            Vec::new(),
            Planes::desktop(None),
            Arc::new(tilo_clock::SystemClock),
        ))
        .await
    }

    /// Start relay services on an already-bound endpoint and compose any
    /// number of caller-owned protocols into the same router. This is the
    /// production shell path: relay state/action traffic, workspace joining,
    /// surfaces, gossip, and personal-fabric protocols retain one stable node
    /// address and one shutdown lifecycle.
    ///
    /// Ownership of `endpoint` transfers to the returned relay. Calling
    /// [`Self::shutdown`] closes the router and invokes every additional
    /// handler's shutdown hook.
    ///
    /// # Errors
    ///
    /// Returns [`RelayError::ProtocolRegistration`] if an ALPN is empty,
    /// duplicates another additional protocol, or collides with a protocol
    /// owned by the relay.
    pub async fn start_on_endpoint_with_protocols(
        cfg: RelayConfig,
        identity: RelayIdentity,
        endpoint: Endpoint,
        protocols: Vec<(Vec<u8>, Box<dyn DynProtocolHandler>)>,
    ) -> Result<Self, RelayError> {
        Box::pin(Self::start_inner(
            cfg,
            identity,
            None,
            Some(endpoint),
            protocols,
            Planes::desktop(None),
            Arc::new(tilo_clock::SystemClock),
        ))
        .await
    }

    /// Start the daemon with the given config + identity.
    ///
    /// Binds an iroh endpoint (using the persistent secret at
    /// `<state_root>/iroh-secret.bin`) and spawns an accept task that runs the auth
    /// handshake + dispatches to `run_peer` for every inbound
    /// connection.
    ///
    /// # Errors
    ///
    /// Returns [`RelayError::Identity`] if `identity` fails validation
    /// (bad signature, wrong chain, revoked, or not host-capable),
    /// [`RelayError::Io`] if the state root can't be created,
    /// [`RelayError::Store`] if the persistent iroh secret can't be
    /// loaded, and [`RelayError::Iroh`] if
    /// the listen address is invalid or the endpoint fails to bind.
    pub async fn start(cfg: RelayConfig, identity: RelayIdentity) -> Result<Self, RelayError> {
        Self::start_with_app_serving(cfg, identity, None).await
    }

    /// [`Relay::start`], with an optional plane that answers remote-action
    /// frames addressed to this host.
    ///
    /// The relay links no apps; a process that hosts them passes a plane
    /// here. Supplying one makes `app_serving` available, still subject to
    /// the configuration requesting it and the certificate authorising it.
    /// When the effective services leave `app_serving` off, the plane is
    /// dropped and such frames are refused as undeliverable.
    ///
    /// # Errors
    ///
    /// The same as [`Relay::start`].
    pub async fn start_with_app_serving(
        cfg: RelayConfig,
        identity: RelayIdentity,
        app_serving: Option<Arc<dyn crate::app_serving::AppServingPlane>>,
    ) -> Result<Self, RelayError> {
        Box::pin(Self::start_inner(
            cfg,
            identity,
            None,
            None,
            Vec::new(),
            Planes::desktop(app_serving),
            Arc::new(tilo_clock::SystemClock),
        ))
        .await
    }

    /// Start an app-serving host with an explicit transport relay policy.
    ///
    /// # Errors
    /// The same as [`Relay::start`].
    pub async fn start_with_app_serving_and_relay_mode(
        cfg: RelayConfig,
        identity: RelayIdentity,
        app_serving: Option<Arc<dyn crate::app_serving::AppServingPlane>>,
        relay_mode: iroh::RelayMode,
    ) -> Result<Self, RelayError> {
        Box::pin(Self::start_inner(
            cfg,
            identity,
            Some(relay_mode),
            None,
            Vec::new(),
            Planes::desktop(app_serving),
            Arc::new(tilo_clock::SystemClock),
        ))
        .await
    }

    /// [`Relay::start`], asking `shell_payloads` what the host may know
    /// about opaque co-edit bodies instead of the desktop's
    /// [`NeighbourPayloads`](crate::payloads::NeighbourPayloads).
    ///
    /// The adapter decides document compaction, retained-checkpoint
    /// replacement, continuity evidence, surface admission and the live
    /// checkpoint gate for every workspace this relay opens. Data retained
    /// under one adapter must be served by the same one: its verdicts are
    /// reproduced from the stored bytes after a restart.
    ///
    /// # Errors
    ///
    /// The same as [`Relay::start`].
    pub async fn start_with_shell_payloads(
        cfg: RelayConfig,
        identity: RelayIdentity,
        shell_payloads: Arc<dyn crate::payloads::ShellPayloads>,
    ) -> Result<Self, RelayError> {
        Box::pin(Self::start_inner(
            cfg,
            identity,
            None,
            None,
            Vec::new(),
            Planes {
                app_serving: None,
                shell_payloads,
            },
            Arc::new(tilo_clock::SystemClock),
        ))
        .await
    }

    /// [`Relay::start`], judging how recent the full-replica status stamp
    /// is by `replica_clock` instead of the wall clock.
    ///
    /// # Errors
    ///
    /// The same as [`Relay::start`].
    pub async fn start_with_replica_clock(
        cfg: RelayConfig,
        identity: RelayIdentity,
        replica_clock: Arc<dyn Clock>,
    ) -> Result<Self, RelayError> {
        Box::pin(Self::start_inner(
            cfg,
            identity,
            None,
            None,
            Vec::new(),
            Planes::desktop(None),
            replica_clock,
        ))
        .await
    }

    /// The shared startup transaction. Its future is large (endpoint binding,
    /// workspace loading, router wiring), so every public constructor awaits
    /// it boxed rather than embedding it in the caller's future.
    #[allow(
        clippy::too_many_lines,
        reason = "ordered startup preserves native identity, policy and task ownership"
    )]
    async fn start_inner(
        mut cfg: RelayConfig,
        identity: RelayIdentity,
        relay_mode: Option<iroh::RelayMode>,
        endpoint: Option<Endpoint>,
        additional_protocols: Vec<(Vec<u8>, Box<dyn DynProtocolHandler>)>,
        planes: Planes,
        replica_clock: Arc<dyn Clock>,
    ) -> Result<Self, RelayError> {
        let Planes {
            app_serving,
            shell_payloads,
        } = planes;
        validate_additional_protocols(&additional_protocols)?;
        if cfg.retired_workspaces.take().is_some() {
            tracing::warn!(
                "the relay configuration's [[workspace]] tables are ignored: the rooms this relay \
                 holds are those whose governance makes it one of their hosts"
            );
        }
        import_home_peer(&mut cfg)?;
        validate_relay_identity(&identity)?;
        if !cfg.device_role.can_serve() {
            return Err(RelayError::Identity(
                "tilo-relay requires an always-on-server or hybrid device role".into(),
            ));
        }
        // A certificate authorises a service, but it cannot make a service
        // exist. The composite process starts relay + filesystem planes; live
        // workspace control uses relay's non-retained presence plane, while
        // compute and app serving exist only when their workers are present:
        // app serving needs a plane supplied by an embedding process.
        let compute_requested = cfg.services.compute
            && matches!(cfg.device_role, crate::mesh::DeviceRole::AlwaysOnServer);
        let mut available_services = crate::mesh::ServiceCapabilities::home_peer();
        available_services.compute = compute_requested;
        available_services.app_serving = app_serving.is_some();
        cfg.services = cfg
            .services
            .authorized_by(&identity.device_cert.capabilities)
            .limited_to(&available_services);
        let app_serving = app_serving.filter(|_| cfg.services.app_serving);
        // Reject unavailable local backends before binding endpoints, publishing
        // capabilities or starting background tasks.
        let environments = crate::environment_host::worker_environments(
            cfg.environments.as_ref().filter(|_| cfg.services.compute),
            &cfg.state_root.join("environments"),
            &identity.device_signing_key,
            &tilo_user_identity::IdentityPaths::under(&cfg.identity_root).device_privkey,
        )?;
        let compute_ready = Arc::new(AtomicBool::new(false));
        let mesh_profile = Arc::new(crate::mesh::DeviceMeshProfile {
            device: identity.device_cert.device_public_key,
            role: cfg.device_role,
            services: cfg.services.clone(),
            replication: BTreeMap::new(),
            filesystem_endpoints: Vec::new(),
        });
        std::fs::create_dir_all(&cfg.state_root)?;
        let full_replica = ReplicaProof {
            path: cfg.full_replica_status.clone(),
            clock: replica_clock,
        };
        let endpoint = match endpoint {
            Some(endpoint) => endpoint,
            None => bind_endpoint(&cfg, &identity.device_signing_key, relay_mode).await?,
        };
        // A certified host governs the rooms it creates, unless its embedder
        // serves workspace joining itself. It answers their governance as
        // actions itself, beside whatever apps its embedder serves; and it
        // routes its owner's compute jobs around both, answering which of
        // the owner's devices a job should run on. It serves no apps by
        // doing so.
        let governs_rooms = !matches!(cfg.trust_policy, TrustPolicy::AllowAny)
            && !additional_protocols.iter().any(|(alpn, _)| {
                alpn.as_slice() == tilo_delivery_protocol::workspace_join::WORKSPACE_JOIN_ALPN
                    || alpn.as_slice() == crate::rooms::ROOM_ADMIN_ALPN
            });
        let owner = identity.user_public_key;
        let (sessions, governed) = crate::host::HostSessions::open(
            crate::host::HostConfig {
                state_root: cfg.state_root.clone(),
                mode: crate::host::HostMode::CertifiedHost,
                trust_policy: cfg.trust_policy.clone(),
                member_limits: cfg.member_limits,
                max_room_grants: cfg.max_room_grants,
                apps: app_serving,
                shell_payloads,
            },
            identity.clone(),
            endpoint.clone(),
            governs_rooms,
            |workspaces, rest| {
                Some(Arc::new(crate::compute_actions::ComputePlane {
                    compute: crate::compute_actions::ComputeActions::new(owner, workspaces.clone()),
                    rest,
                })
                    as Arc<dyn crate::app_serving::AppServingPlane>)
            },
        )
        .await?;
        let rooms = governed.map(|governed| governed.rooms);
        let workspaces = sessions.workspaces.clone();
        let metrics = sessions.metrics.clone();
        let live = sessions.identity.clone();
        let handler = sessions.handler.clone();
        let live_trust_policy = handler.trust_policy.clone();
        let discovery = DiscoveryProtocol {
            workspaces: workspaces.clone(),
            identity: live.clone(),
            trust_policy: live_trust_policy.clone(),
            mesh_profile: mesh_profile.clone(),
            full_replica: full_replica.clone(),
            compute_ready: Arc::clone(&compute_ready),
            home_peer_bundle: cfg.home_peer_bundle.clone(),
            autobind: handler.autobind.clone(),
            agents: handler.agents.clone(),
        };
        let management = ManagementProtocol {
            endpoint: endpoint.clone(),
            identity: live.clone(),
            mesh_profile: mesh_profile.clone(),
            full_replica: full_replica.clone(),
            compute_ready: Arc::clone(&compute_ready),
            state_root: cfg.state_root.clone(),
            workspaces: workspaces.clone(),
        };
        let compute = ComputeProtocol::new(
            workspaces.clone(),
            live.clone(),
            crate::compute_session::ComputeTrust {
                trust_policy: live_trust_policy.clone(),
                agents: handler.agents.clone(),
            },
            Arc::new(crate::environment_host::TracingMetering),
        );
        let certification = crate::certification::CertificationProtocol::new(live.clone());
        let web = Box::new(WebContext {
            relay: handler.clone(),
            #[cfg(feature = "web")]
            certification: certification.clone(),
            node_session: Arc::default(),
        });
        #[cfg(feature = "agents")]
        let mut resealing = None;
        // What the rooms it stewards hold in the clear is sealed only once
        // its operator decided it, recorded in its state.
        #[cfg(feature = "agents")]
        if let Some(rooms) = &rooms
            && crate::reseal::decided(&cfg.state_root)
        {
            let (rooms, compactor) = (rooms.clone(), handler.compactor.clone());
            rooms.reseal_existing();
            compactor.reseal_existing();
            resealing = Some(tokio::spawn(async move {
                rooms.reseal_stewarded(&compactor).await;
            }));
        }
        let mut router = Router::builder(endpoint.clone())
            .accept(ALPN, handler)
            .accept(crate::certify::CERTIFY_ALPN, certification)
            .accept(DISCOVERY_ALPN, discovery)
            .accept(MANAGEMENT_ALPN, management)
            .accept(COMPUTE_ALPN, compute);
        if let Some(rooms) = &rooms {
            for (alpn, handler) in crate::host::rooms_protocols(&sessions, rooms) {
                router = router.accept(alpn, handler);
            }
        }
        for (alpn, handler) in additional_protocols {
            router = router.accept(alpn, handler);
        }
        let router = router.spawn();

        let peer_relay_tasks =
            spawn_background_tasks(&cfg, &identity, &endpoint, &workspaces, &metrics, &live);
        if cfg.services.compute {
            // Running jobs on the replica's copies only saves transfers;
            // without them every input is fetched.
            let replicas = home_peer_replicas(&cfg.home_peer_bundle).unwrap_or_else(|error| {
                tracing::warn!(%error, "compute worker runs without the home-peer replica");
                BTreeMap::new()
            });
            // The worker serves the rooms this host holds when it starts and
            // those its home-peer replica keeps.
            let mut worker_workspaces = replicas.keys().copied().collect::<BTreeSet<u64>>();
            for (id, workspace) in workspaces.lock().await.iter() {
                workspace
                    .lock()
                    .await
                    .admit_device(identity.device_cert.device_public_key)?;
                worker_workspaces.insert(*id);
            }
            let worker_config = ProductionComputeWorkerConfig {
                endpoint: endpoint.clone(),
                relay_address: endpoint.addr(),
                user_public_key: identity.user_public_key,
                device_cert: identity.device_cert.clone(),
                device_signing_key: identity.device_signing_key.clone(),
                revocations: identity.revocations.clone(),
                workspaces: worker_workspaces,
                state_root: cfg.state_root.join("compute-worker"),
                readiness: Arc::clone(&compute_ready),
                environments,
                replicas,
            };
            let worker_task = tokio::spawn(async move {
                if let Err(error) = run_production_compute_worker(worker_config).await {
                    tracing::warn!(%error, "production compute worker withdrawn");
                }
            });
            push_peer_relay_task(&peer_relay_tasks, worker_task);
        }
        Ok(Self {
            endpoint,
            router,
            sessions,
            #[cfg(feature = "agents")]
            resealing: std::sync::Mutex::new(resealing),
            #[cfg(feature = "web")]
            browser_origin: Arc::new(std::sync::RwLock::new(None)),
            peer_relay_tasks,
            mesh_profile,
            full_replica: full_replica.clone(),
            web,
            compute_ready,
            rooms,
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
        self.sessions.is_subscribed(workspace_id, device).await
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
        if let Err(error) = self.router.shutdown().await {
            tracing::debug!(%error, "relay router shutdown task failed");
        }
        self.sessions.shutdown().await;
    }

    /// Add a peer relay with a fully-resolved endpoint address —
    /// same dial loop, but skips iroh discovery. Useful for tests
    /// that already know the target's iroh addresses, and for
    /// pinned-address deployments.
    pub fn add_peer_relay_at(&self, addr: EndpointAddr, label: String, _priority: i32) {
        let task = PeerRelayDialer {
            endpoint: self.endpoint.clone(),
            identity: self.sessions.identity.clone(),
            workspaces: self.sessions.workspaces.clone(),
            metrics: self.sessions.metrics.clone(),
            scope: None,
        }
        .spawn(Upstream::Relay(addr), label);
        push_peer_relay_task(&self.peer_relay_tasks, task);
    }

    /// Cheap external handle.
    pub fn handle(&self) -> RelayHandle {
        RelayHandle {
            node_addr: self.endpoint.addr(),
            metrics: self.sessions.metrics.clone(),
            mesh_profile: self.mesh_profile.clone(),
            full_replica: self.full_replica.clone(),
            compute_ready: Arc::clone(&self.compute_ready),
        }
    }

    /// Tell the host that a device is an agent: verify its root-signed
    /// delegation against this relay's user and confine the device to the
    /// delegated rooms from its next envelope on. Replaces an earlier
    /// delegation of the same device.
    pub fn register_agent(
        &self,
        delegation: tilo_relay_protocol::delegation::Delegation,
    ) -> Result<(), tilo_relay_protocol::delegation::DelegationError> {
        delegation.verify(
            &self.web.relay.identity.user_public_key,
            tilo_clock::unix_now_ns(),
        )?;
        self.web
            .relay
            .agents
            .write_recover()
            .insert(delegation.device_public_key, Some(delegation));
        // A replacement may narrow an earlier delegation's rooms.
        crate::authority_epoch::bump();
        Ok(())
    }

    /// The iroh endpoint address (node id + reachable addresses) —
    /// what a peer needs to dial this relay.
    pub fn node_addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// Wait until the relay has sealed what the rooms it stewards held in
    /// the clear when it started, if its operator decided it
    /// ([`crate::reseal`]).
    #[cfg(feature = "agents")]
    pub async fn resealed(&self) {
        let task = self
            .resealing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    /// The compactor keeping this relay's retained document channels short.
    pub fn compactor(&self) -> &crate::compaction::Compactor {
        &self.web.relay.compactor
    }

    /// The local sockets the relay's endpoint is bound to.
    #[must_use]
    pub fn bound_sockets(&self) -> Vec<std::net::SocketAddr> {
        self.endpoint.bound_sockets()
    }

    /// Atomically replace the native governor's exact member devices and the
    /// user roots those devices need at the relay handshake. This is a
    /// trusted, server-local reconciliation API: roots open only the global
    /// handshake, while the exact device set continues to gate this room.
    ///
    /// Removals narrow live room access before disk I/O and additions become
    /// reachable only after one crash-safe access record commits. The global
    /// handshake allowlist is then rebuilt from configured pins and every
    /// room's projection, so removing one room cannot revoke another room or
    /// widen either room to sibling devices under the same user root.
    ///
    /// # Errors
    ///
    /// Returns [`RelayError::GovernedAccess`] when called for an `allow_any`
    /// relay or with a state root other than the relay's configured root, and
    /// [`RelayError::Store`] when workspace initialization or durable
    /// replacement fails.
    pub async fn replace_governed_workspace_access(
        &self,
        workspace_id: u64,
        pinned: bool,
        access: tilo_peer_state::GovernedAccess,
        state_root: PathBuf,
    ) -> Result<(), RelayError> {
        Ok(self
            .sessions
            .replace_governed_workspace_access(workspace_id, pinned, access, state_root)
            .await?)
    }

    /// Let visitors watch `workspace_id` read-only as `spectators`: every
    /// device the spectator root certified reads the room's state and
    /// writes nothing, up to `spectators.seats` subscribed at once (see
    /// [`crate::room_access`]). The spectator person must be a member of the
    /// room, which is also what lets its devices through the handshake.
    pub async fn admit_spectators(
        &self,
        workspace_id: u64,
        spectators: crate::room_access::Spectators,
    ) -> Result<(), RelayError> {
        let workspace = self
            .sessions
            .workspaces
            .lock()
            .await
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| {
                RelayError::GovernedAccess(format!("room {workspace_id} is not open here"))
            })?;
        workspace.lock().await.spectators = Some(spectators);
        // Live subscriptions are audited again: fewer seats end the extra.
        crate::authority_epoch::bump();
        Ok(())
    }

    /// The rooms this relay governs, when it hosts the room authority.
    #[must_use]
    pub const fn rooms(&self) -> Option<&crate::rooms::Rooms> {
        self.rooms.as_ref()
    }

    /// Where this relay keeps its state.
    #[cfg(feature = "web")]
    pub(crate) fn state_root(&self) -> &std::path::Path {
        &self.sessions.state_root
    }

    /// Own user identity — for tests.
    pub fn own_user_public_key(&self) -> [u8; 32] {
        self.sessions.identity.user_public_key
    }

    /// In-process access to the records of the workspaces this relay hosts,
    /// for a service embedded in the host process.
    #[must_use]
    pub fn room_records(&self) -> crate::room_records::RoomRecords {
        self.sessions.room_records()
    }
}

/// A home-peer workspace's filesystem ticket and the relay workspaces it
/// backs.
fn home_peer_relay_workspaces(
    workspace: &fs::full_replica::HomePeerBundle,
) -> Result<(fs::Ticket, Vec<u64>), RelayError> {
    let ticket = fs::Ticket::decode(&workspace.ticket)
        .map_err(|error| RelayError::Identity(format!("invalid home-peer ticket: {error}")))?;
    let relay_workspace_ids = if workspace.relay_workspace_ids.is_empty() {
        // Bundles exported before room bindings existed have one stable
        // fallback shard. New enrollment always writes explicit ids.
        vec![
            crate::room_workspace_id(&ticket.workspace_id().to_string())
                .map_err(|error| RelayError::Identity(format!("invalid workspace id: {error}")))?,
        ]
    } else {
        workspace.relay_workspace_ids.clone()
    };
    Ok((ticket, relay_workspace_ids))
}

/// The filesystem replica stores the home peer on this device keeps, by the
/// relay workspace each backs: content this device's compute worker may run
/// a job on once the job's requester proves it holds the same bytes.
fn home_peer_replicas(bundle_path: &Path) -> Result<BTreeMap<u64, Vec<PathBuf>>, RelayError> {
    let mut replicas = BTreeMap::<u64, Vec<PathBuf>>::new();
    if !bundle_path.exists() {
        return Ok(replicas);
    }
    let text = std::fs::read_to_string(bundle_path)?;
    let bundle: fs::full_replica::HomePeerSet = toml::from_str(&text)
        .map_err(|error| RelayError::Identity(format!("invalid home-peer bundle: {error}")))?;
    for workspace in &bundle.workspaces {
        let (_, relay_workspace_ids) = home_peer_relay_workspaces(workspace)?;
        for id in relay_workspace_ids {
            let dirs = replicas.entry(id).or_default();
            if !dirs.contains(&workspace.cache_dir) {
                dirs.push(workspace.cache_dir.clone());
            }
        }
    }
    Ok(replicas)
}

/// Check the home-peer bundle the filesystem worker consumes and, when there
/// is one, serve full-content replication. Which rooms this relay holds is
/// not the bundle's to say: the rooms' own governance admits it as one of
/// their hosts.
fn import_home_peer(cfg: &mut RelayConfig) -> Result<(), RelayError> {
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
    for workspace in &bundle.workspaces {
        home_peer_relay_workspaces(workspace)?;
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
        services.compute &= self.compute_ready.load(Ordering::Acquire);
        services
    }

    /// Filesystem endpoints that proved complete residency in the current
    /// heartbeat. Consumers use these for blob and namespace protocols while
    /// retaining `node_addr` for relay traffic.
    pub fn filesystem_endpoints(&self) -> Vec<EndpointAddr> {
        self.full_replica
            .current()
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
/// processes from publishing competing address records for one Iroh `NodeId`.
pub fn relay_service_node_id(device_key: &ed25519_dalek::SigningKey) -> [u8; 32] {
    tilo_relay_protocol::relay_service_node_id(&device_key.to_bytes())
}

/// Bind the relay plane with a domain-separated transport key. This is one
/// physical certified device, not a second user identity: protocol auth uses
/// `device_key` and its device certificate after QUIC connects.
async fn bind_endpoint(
    cfg: &RelayConfig,
    device_key: &ed25519_dalek::SigningKey,
    relay_mode: Option<iroh::RelayMode>,
) -> Result<Endpoint, RelayError> {
    let secret = iroh::SecretKey::from_bytes(&tilo_relay_protocol::relay_service_secret(
        &device_key.to_bytes(),
    ));
    let builder = match relay_mode {
        Some(mode) => Endpoint::builder(presets::Minimal).relay_mode(mode),
        None => Endpoint::builder(presets::N0),
    };
    builder
        .secret_key(secret)
        .bind_addr(cfg.iroh_listen)
        .map_err(|e| RelayError::Iroh(format!("invalid iroh listen address: {e}")))?
        .bind()
        .await
        .map_err(|e| RelayError::Iroh(e.to_string()))
}

fn validate_additional_alpn(alpn: &[u8]) -> Result<(), RelayError> {
    let reserved: &[&[u8]] = &[ALPN, DISCOVERY_ALPN, MANAGEMENT_ALPN, COMPUTE_ALPN];
    if alpn.is_empty() {
        return Err(RelayError::ProtocolRegistration(
            "additional ALPN must not be empty".into(),
        ));
    }
    if reserved.contains(&alpn) {
        return Err(RelayError::ProtocolRegistration(format!(
            "{} is reserved by tilo-relay",
            String::from_utf8_lossy(alpn)
        )));
    }
    Ok(())
}

fn validate_additional_protocols(
    protocols: &[(Vec<u8>, Box<dyn DynProtocolHandler>)],
) -> Result<(), RelayError> {
    validate_additional_alpns(protocols.iter().map(|(alpn, _)| alpn.as_slice()))
}

fn validate_additional_alpns<'a>(
    alpns: impl IntoIterator<Item = &'a [u8]>,
) -> Result<(), RelayError> {
    let mut seen = HashSet::new();
    for alpn in alpns {
        validate_additional_alpn(alpn)?;
        if !seen.insert(alpn) {
            return Err(RelayError::ProtocolRegistration(format!(
                "{} is registered more than once",
                String::from_utf8_lossy(alpn)
            )));
        }
    }
    Ok(())
}

/// Spawn the revocation-refresh loop, the metrics-persist loop, the
/// membership-file watch, and one reconnect-with-backoff task per configured
/// peer relay,
/// returning the abort-on-drop handle list `Relay` owns.
///
/// Each peer-relay task dials via `RelayClient::connect` (same code
/// path any client uses), exchanges `PeerRelayHello`, subscribes to
/// the intersection, then feeds inbound deltas into the shared ordered
/// state persistence owner.
fn spawn_background_tasks(
    cfg: &RelayConfig,
    identity: &RelayIdentity,
    endpoint: &Endpoint,
    workspaces: &WorkspaceRegistry,
    metrics: &Arc<Metrics>,
    live: &LiveIdentity,
) -> Arc<std::sync::Mutex<Vec<AbortOnDrop>>> {
    let peer_relay_tasks: Arc<std::sync::Mutex<Vec<AbortOnDrop>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    let revocation_task = spawn_revocation_refresh_task(
        cfg.state_root.clone(),
        identity.revocations.clone(),
        live.revocations.clone(),
    );
    push_peer_relay_task(&peer_relay_tasks, revocation_task);

    let metrics_task = spawn_metrics_persist_task(cfg.state_root.clone(), metrics.clone());
    push_peer_relay_task(&peer_relay_tasks, metrics_task);

    // The host's sessions watch the membership files themselves.

    for peer_cfg in cfg.peer_relays.iter().cloned() {
        let task = spawn_peer_relay_dial(
            peer_cfg,
            endpoint.clone(),
            live.clone(),
            workspaces.clone(),
            metrics.clone(),
        );
        push_peer_relay_task(&peer_relay_tasks, task);
    }

    peer_relay_tasks
}

/// How often the revocation log is re-read while its directory cannot be
/// watched: the cadence `tilo-relay-admin` promises ("within one second").
const REVOCATION_POLL: Duration = Duration::from_secs(1);

/// The revocation-refresh task: re-read the on-disk revocation log whenever
/// it changes and, if every entry still applies cleanly against the
/// identity's own base list, swap it in and wake every admitted session to
/// re-check. A malformed entry leaves the previously-live list untouched
/// rather than partially applying.
///
/// The log is followed through a watch on the state root's own entries. When
/// that watch cannot be installed the task says so and falls back to reading
/// the log every [`REVOCATION_POLL`], as before the watch existed.
fn spawn_revocation_refresh_task(
    state_root: PathBuf,
    base_revocations: RevocationList,
    live_revocations: Arc<std::sync::RwLock<RevocationList>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut refresh = RevocationRefresh {
            state_root,
            base: base_revocations,
            live: live_revocations,
            applied: None,
        };
        let watch = match tilo_files::native::watch_entries(&refresh.state_root) {
            Ok(watch) => Some(watch),
            Err(error) => {
                tracing::warn!(
                    path = %refresh.state_root.display(),
                    %error,
                    "the revocation log cannot be watched; reading it every second"
                );
                None
            }
        };
        // Read once the watch exists, so an edit made before it began is seen.
        refresh.reload();
        let Some(watch) = watch else {
            let mut interval = tokio::time::interval(REVOCATION_POLL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                refresh.reload();
            }
        };
        loop {
            let change = watch.next().await;
            if change.kind == tilo_files::ChangeKind::Rescan
                || change.path.as_str() == crate::store::REVOCATIONS_FILE
            {
                refresh.reload();
            }
        }
    })
}

/// The live revocation list and the log entries last applied to it.
struct RevocationRefresh {
    state_root: PathBuf,
    base: RevocationList,
    live: Arc<std::sync::RwLock<RevocationList>>,
    applied: Option<Vec<tilo_user_identity::RevocationEntry>>,
}

impl RevocationRefresh {
    fn reload(&mut self) {
        let Ok(entries) = crate::store::load_revocations(&self.state_root) else {
            return;
        };
        if self.applied.as_ref() == Some(&entries) {
            return;
        }
        let mut next = self.base.clone();
        for entry in &entries {
            if next.insert(entry.clone()).is_err() {
                return;
            }
        }
        if let Ok(mut current) = self.live.write() {
            *current = next;
            drop(current);
            self.applied = Some(entries);
            crate::authority_epoch::bump();
        }
    }
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
    tasks.lock_recover()
}

/// Register a spawned task on the peer-relay task list (see
/// [`lock_peer_relay_tasks`]).
fn push_peer_relay_task(tasks: &Arc<std::sync::Mutex<Vec<AbortOnDrop>>>, task: JoinHandle<()>) {
    lock_peer_relay_tasks(tasks).push(AbortOnDrop(task));
}

fn validate_relay_identity(identity: &RelayIdentity) -> Result<(), RelayError> {
    crate::host::validate_host_device_identity(identity)?;
    if !identity.device_cert.capabilities.can_act_as_host {
        return Err(RelayError::Identity(
            "device certificate is not host-capable".into(),
        ));
    }
    Ok(())
}

#[derive(Clone)]
struct DiscoveryProtocol {
    workspaces: WorkspaceRegistry,
    identity: LiveIdentity,
    trust_policy: Arc<std::sync::RwLock<TrustPolicy>>,
    mesh_profile: Arc<crate::mesh::DeviceMeshProfile>,
    full_replica: ReplicaProof,
    compute_ready: Arc<AtomicBool>,
    home_peer_bundle: PathBuf,
    autobind: Option<WorkspaceAutobind>,
    agents: crate::peer::AgentRegistry,
}

impl DiscoveryProtocol {
    fn trust(&self) -> crate::room_access::RelayTrust<'_> {
        crate::room_access::RelayTrust {
            revocations: &self.identity.revocations,
            policy: &self.trust_policy,
            own_user: &self.identity.user_public_key,
            agents: &self.agents,
        }
    }
}

/// What this relay's workers run for environments, across its workspaces.
async fn environment_offer(
    workspaces: &WorkspaceRegistry,
) -> tilo_relay_protocol::RelayEnvironments {
    let bound: Vec<SharedWorkspace> = workspaces.lock().await.values().cloned().collect();
    let now_ms = tilo_clock::unix_now_ms();
    let mut offer = tilo_relay_protocol::RelayEnvironments::default();
    for workspace in bound {
        let here = crate::compute_transport::compute_state(&mut *workspace.lock().await)
            .environment_offer(now_ms);
        offer.sessions |= here.sessions;
        offer.legacy_apps |= here.legacy_apps;
    }
    offer
}

#[derive(Clone)]
struct ManagementProtocol {
    endpoint: Endpoint,
    identity: LiveIdentity,
    mesh_profile: Arc<crate::mesh::DeviceMeshProfile>,
    full_replica: ReplicaProof,
    compute_ready: Arc<AtomicBool>,
    state_root: PathBuf,
    workspaces: WorkspaceRegistry,
}

impl std::fmt::Debug for ManagementProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ManagementProtocol")
    }
}

impl ProtocolHandler for ManagementProtocol {
    #[allow(
        clippy::too_many_lines,
        reason = "one authenticated management exchange must keep handshake, capability checks, durable mutation, response, and shutdown sequencing together"
    )]
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        if self.state_root.join("decommissioned").exists() {
            return Err(AcceptError::from_err(std::io::Error::other(
                "home peer is decommissioned",
            )));
        }
        let (mut send, mut recv) = connection.accept_bi().await?;
        // Only the owner's devices manage the home peer, whatever the live
        // policy admits.
        let peer = self
            .identity
            .authenticate(
                EndpointTrust::Fixed(crate::auth::HandshakeTrust::OwnerDevicesOnly),
                &mut recv,
                &mut send,
                MAX_FRAME_BYTES,
            )
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
        let mut services = self.mesh_profile.services.clone();
        services.compute &= self.compute_ready.load(Ordering::Acquire);
        let request: HomePeerManagementRequest =
            postcard::from_bytes(&read_frame(&mut recv, MAX_FRAME_BYTES).await?)
                .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
        let mut decommissioned = false;
        match request {
            HomePeerManagementRequest::Status => {}
            HomePeerManagementRequest::Rename { label } => {
                if !peer.cert.capabilities.can_admit_devices {
                    return Err(AcceptError::from_err(std::io::Error::other(
                        "device is not authorized to rename home peers",
                    )));
                }
                let label = label.trim();
                if !crate::valid_home_peer_label(label) {
                    return Err(AcceptError::from_err(std::io::Error::other(
                        "home-peer name must be 1 to 80 bytes",
                    )));
                }
                tilo_files::native::atomic::write_with_mode(
                    &self.state_root.join("home-peer-name"),
                    label.as_bytes(),
                    0o644,
                )
                .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
            }
            HomePeerManagementRequest::Decommission { revocation } => {
                if !peer.cert.capabilities.can_admit_devices {
                    return Err(AcceptError::from_err(std::io::Error::other(
                        "device is not authorized to remove home peers",
                    )));
                }
                verify_revocation_entry(&revocation).map_err(|error| {
                    AcceptError::from_err(std::io::Error::other(error.to_string()))
                })?;
                if revocation.user_public_key != self.identity.user_public_key
                    || revocation.revoked_device_public_key != self.identity.cert.device_public_key
                {
                    return Err(AcceptError::from_err(std::io::Error::other(
                        "revocation does not target this home peer",
                    )));
                }
                crate::store::append_revocation(&self.state_root, &revocation).map_err(
                    |error| AcceptError::from_err(std::io::Error::other(error.to_string())),
                )?;
                // Apply it here too rather than wait for the log to be read
                // back: this process stops serving its own revoked identity
                // now. A conflicting entry is the log reader's to reject.
                let applied = self.identity.revocations.write_recover().insert(revocation);
                if matches!(applied, Ok(true)) {
                    crate::authority_epoch::bump();
                }
                tilo_files::native::atomic::write_with_mode(
                    &self.state_root.join("decommissioned"),
                    b"decommissioned\n",
                    0o600,
                )
                .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
                decommissioned = true;
            }
        }
        let label = std::fs::read_to_string(self.state_root.join("home-peer-name"))
            .ok()
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| self.identity.cert.device_label.clone());
        let files_ready = self.full_replica.current().is_some();
        let environments = environment_offer(&self.workspaces).await;
        write_frame(
            &mut send,
            &postcard::to_allocvec(&HomePeerManagementResponse {
                device_public_key: self.identity.cert.device_public_key,
                relay_endpoint: self.endpoint.addr(),
                label: label.trim().to_string(),
                files_ready,
                services,
                environments,
                decommissioned,
            })
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?,
        )
        .await
        .map_err(AcceptError::from_err)?;
        send.finish().map_err(AcceptError::from_err)?;
        if decommissioned && !cfg!(test) {
            tokio::spawn(async {
                tokio::time::sleep(Duration::from_millis(250)).await;
                std::process::exit(0);
            });
        }
        connection.closed().await;
        Ok(())
    }
}

impl std::fmt::Debug for DiscoveryProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DiscoveryProtocol")
    }
}

impl ProtocolHandler for DiscoveryProtocol {
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace handle is consumed by the match that reads admission; there is nothing left to drop early"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one authenticated discovery exchange keeps authorization, workspace lookup, content proof, and response framing in one security-reviewed sequence"
    )]
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        if self
            .autobind
            .as_ref()
            .is_some_and(|autobind| autobind.state_root.join("decommissioned").exists())
        {
            return Err(AcceptError::from_err(std::io::Error::other(
                "home peer is decommissioned",
            )));
        }
        let (mut send, mut recv) = connection.accept_bi().await?;
        // Any device the live policy admits may ask; the request is then
        // scoped to a room it belongs to.
        let peer = self
            .identity
            .authenticate(
                EndpointTrust::Live(&self.trust_policy),
                &mut recv,
                &mut send,
                MAX_FRAME_BYTES,
            )
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
        let request: ContentEndpointRequest =
            postcard::from_bytes(&read_frame(&mut recv, MAX_FRAME_BYTES).await?)
                .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
        if peer.user_public_key == self.identity.user_public_key {
            get_or_autobind_workspace(
                &self.workspaces,
                self.autobind.as_ref(),
                request.workspace_id,
            )
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
        }
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
                crate::room_access::decide(
                    &workspace,
                    &self.trust(),
                    &peer,
                    crate::room_access::RoomOperation::ContentDiscovery,
                )
                .admitted()
            }
            None => false,
        };
        if !permitted {
            return Err(AcceptError::from_err(std::io::Error::other(
                "workspace is not bound or not authorised on this relay",
            )));
        }
        let mut profile = (*self.mesh_profile).clone();
        profile.services.compute &= self.compute_ready.load(Ordering::Acquire);
        let proof = self.full_replica.current();
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
        let same_owner = peer.user_public_key == self.identity.user_public_key;
        let filesystem_ticket = std::fs::read_to_string(&self.home_peer_bundle)
            .ok()
            .and_then(|text| toml::from_str::<fs::full_replica::HomePeerSet>(&text).ok())
            .and_then(|set| {
                let sole_workspace = same_owner && set.workspaces.len() == 1;
                set.workspaces.into_iter().find(|bundle| {
                    bundle.relay_workspace_ids.contains(&request.workspace_id) || sole_workspace
                })
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

/// The relay frame codec, re-exported under the daemon path older callers use.
pub use crate::frame::{read_frame, write_frame};

/// Spawn the outbound reconnect-with-backoff task for one peer relay
/// identified by its config entry ([`tilo_room_host::federation`]).
fn spawn_peer_relay_dial(
    peer_cfg: PeerRelayConfig,
    endpoint: Endpoint,
    identity: LiveIdentity,
    workspaces: WorkspaceRegistry,
    metrics: Arc<Metrics>,
) -> JoinHandle<()> {
    let Some(node_id) = parse_iroh_node_id(&peer_cfg.iroh_node_id) else {
        tracing::warn!(?peer_cfg.iroh_node_id, "peer relay iroh_node_id not parseable, skipping");
        return tokio::spawn(async {});
    };
    PeerRelayDialer {
        endpoint,
        identity,
        workspaces,
        metrics,
        scope: None,
    }
    .spawn(Upstream::Relay(EndpointAddr::new(node_id)), peer_cfg.label)
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
#[allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
mod tests {
    use super::*;
    use tilo_clock::unix_now_ns;
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, DeviceIdentity, UserIdentity, sign_device_cert,
    };

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
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn startup_validation_and_room_authority_admit_only_signed_hosts() {
        // Phase `valid_host_identity_passes_startup_validation`.
        {
            validate_relay_identity(&identity(DeviceCapabilities::host())).unwrap();
        }
        // Phase `ordinary_device_cannot_start_a_relay`.
        {
            let error =
                validate_relay_identity(&identity(DeviceCapabilities::member())).unwrap_err();
            assert!(error.to_string().contains("not host-capable"));
        }
        // Phase `mismatched_device_key_is_rejected_at_startup`.
        {
            let mut relay = identity(DeviceCapabilities::host());
            relay.device_signing_key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
            let error = validate_relay_identity(&relay).unwrap_err();
            assert!(error.to_string().contains("does not match"));
        }
        // Phase `a_home_peer_bundle_serves_full_content_replication`.
        {
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
            // The compute worker beside the replica holds its store for the
            // shard's jobs, and only that shard's.
            assert_eq!(
                home_peer_replicas(&bundle_path).unwrap(),
                BTreeMap::from([(42, vec![temp.path().join("cache")])])
            );
            assert!(
                home_peer_replicas(&temp.path().join("absent.toml"))
                    .unwrap()
                    .is_empty()
            );
            let mut config = RelayConfig {
                home_peer_bundle: bundle_path,
                full_replica_status: status,
                ..RelayConfig::default()
            };

            import_home_peer(&mut config).unwrap();
            assert!(config.services.full_content_replication);
        }
    }

    /// Write `status` as the full replica does, once, and return a clock
    /// stopped at its stamp: the proof stays as recent as when it was written
    /// however long starting the relay or answering a discovery takes.
    fn publish_status(
        path: &std::path::Path,
        status: &fs::full_replica::FullReplicaStatus,
    ) -> Arc<dyn Clock> {
        fs::full_replica::save_status(path, status).unwrap();
        let stamped = fs::full_replica::load_status(path)
            .unwrap()
            .unwrap()
            .updated_at_ms;
        Arc::new(tilo_clock::FixedClock(stamped))
    }

    #[test]
    fn a_replica_proof_lapses_five_seconds_after_its_stamp() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("status.json");
        let mut status = fs::full_replica::FullReplicaStatus::syncing();
        status.state = "ready".into();
        status.full_content_replication = true;
        status.filesystem_endpoints.push(EndpointAddr::new(
            iroh::SecretKey::from_bytes(&[7; 32]).public(),
        ));
        fs::full_replica::save_status(&path, &status).unwrap();
        let stamped = fs::full_replica::load_status(&path)
            .unwrap()
            .unwrap()
            .updated_at_ms;
        let proof_at = |now_ms: u64| {
            ReplicaProof {
                path: path.clone(),
                clock: Arc::new(tilo_clock::FixedClock(now_ms)),
            }
            .current()
        };
        assert!(proof_at(stamped.saturating_add(5_000)).is_some());
        assert!(proof_at(stamped.saturating_add(5_001)).is_none());
    }

    /// #4224. A replica that holds its liveness lock keeps its ready
    /// proof without restamping it; the proof lapses the moment the lock
    /// is released (the replica exited), however old the stamp.
    #[test]
    fn a_running_replica_keeps_its_proof_without_restamping_it() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("status.json");
        let mut status = fs::full_replica::FullReplicaStatus::syncing();
        status.state = "ready".into();
        status.full_content_replication = true;
        status.filesystem_endpoints.push(EndpointAddr::new(
            iroh::SecretKey::from_bytes(&[7; 32]).public(),
        ));
        let alive = fs::full_replica::ReplicaLiveness::hold(&path).unwrap();
        fs::full_replica::save_status(&path, &status).unwrap();
        let stamped = fs::full_replica::load_status(&path)
            .unwrap()
            .unwrap()
            .updated_at_ms;
        let an_hour_later = ReplicaProof {
            path,
            clock: Arc::new(tilo_clock::FixedClock(stamped.saturating_add(3_600_000))),
        };
        assert!(
            an_hour_later.current().is_some(),
            "a live replica's proof must not depend on how recently it was written"
        );
        drop(alive);
        assert!(
            an_hour_later.current().is_none(),
            "the proof of a replica that exited must lapse"
        );
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the end-to-end discovery regression keeps host, client, and filesystem assertions in one readable protocol scenario"
    )]
    async fn advertised_endpoints_are_the_hosted_services_own() {
        // Phase `composite_relay_clamps_only_unhosted_service_advertisements`.
        {
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
            assert!(services.remote_control_relay);
            relay.compute_ready.store(true, Ordering::Release);
            assert!(relay.handle().mesh_profile().services.compute);
            relay.compute_ready.store(false, Ordering::Release);
            assert!(!relay.handle().mesh_profile().services.compute);
            relay.shutdown().await;
        }
        // Phase `ready_profile_serializes_the_direct_filesystem_endpoint`.
        {
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
            let status_clock = publish_status(&status_path, &status);
            let relay = Relay::start_with_replica_clock(
                RelayConfig {
                    state_root: temp.path().join("relay-state"),
                    full_replica_status: status_path.clone(),
                    device_role: crate::mesh::DeviceRole::AlwaysOnServer,
                    services: crate::mesh::ServiceCapabilities::home_peer(),
                    ..RelayConfig::default()
                },
                relay_identity,
                status_clock,
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
        // Phase `authenticated_discovery_returns_filesystem_not_relay_endpoint`.
        {
            let temp = tempfile::tempdir().unwrap();
            let (root, user) = UserIdentity::generate("owner", 1);
            let (host_key, host) = DeviceIdentity::generate("home", 2);
            let (client_key, client) = DeviceIdentity::generate("laptop", 3);
            let (admin_key, admin) = DeviceIdentity::generate("primary", 5);
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
            let mut admin_cert = DeviceCert {
                user_public_key: user.public_key,
                device_public_key: admin.public_key,
                device_label: admin.label,
                capabilities: DeviceCapabilities::primary(),
                issued_at_ns: 4,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut host_cert, &root).unwrap();
            sign_device_cert(&mut client_cert, &root).unwrap();
            sign_device_cert(&mut admin_cert, &root).unwrap();
            let filesystem =
                EndpointAddr::new(iroh::SecretKey::from_bytes(&host_key.to_bytes()).public())
                    .with_ip_addr("127.0.0.1:40123".parse().unwrap());
            let status_path = temp.path().join("status.json");
            let mut status = fs::full_replica::FullReplicaStatus::syncing();
            status.state = "ready".into();
            status.full_content_replication = true;
            status.filesystem_endpoints.push(filesystem.clone());
            let status_clock = publish_status(&status_path, &status);
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
            let relay = Relay::start_with_replica_clock(
                RelayConfig {
                    state_root: temp.path().join("relay"),
                    full_replica_status: status_path.clone(),
                    home_peer_bundle: bundle_path,
                    device_role: crate::mesh::DeviceRole::AlwaysOnServer,
                    services: crate::mesh::ServiceCapabilities::home_peer(),
                    ..RelayConfig::default()
                },
                RelayIdentity {
                    user_public_key: user.public_key,
                    device_signing_key: host_key,
                    device_cert: host_cert,
                    revocations: RevocationList::new(),
                },
                status_clock,
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
            let auto_bound = crate::client::RelayClient::discover_content_peer(
                &client_endpoint,
                relay.node_addr(),
                99,
                user.public_key,
                host.public_key,
                client_cert.clone(),
                client_key.clone(),
                RevocationList::new(),
            )
            .await
            .unwrap();
            assert_eq!(auto_bound.endpoint, filesystem);
            assert!(relay.sessions.workspaces.lock().await.contains_key(&99));
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
            let status = crate::client::RelayClient::manage_home_peer(
                &client_endpoint,
                relay.node_addr(),
                user.public_key,
                host.public_key,
                client_cert.clone(),
                client_key.clone(),
                RevocationList::new(),
                HomePeerManagementRequest::Status,
            )
            .await
            .unwrap();
            assert_eq!(status.label, "home");
            assert!(status.files_ready);
            assert!(
                crate::client::RelayClient::manage_home_peer(
                    &client_endpoint,
                    relay.node_addr(),
                    user.public_key,
                    host.public_key,
                    client_cert.clone(),
                    client_key.clone(),
                    RevocationList::new(),
                    HomePeerManagementRequest::Rename {
                        label: "Not authorized".into(),
                    },
                )
                .await
                .is_err()
            );
            assert!(
                crate::client::RelayClient::manage_home_peer(
                    &client_endpoint,
                    relay.node_addr(),
                    user.public_key,
                    host.public_key,
                    admin_cert.clone(),
                    admin_key.clone(),
                    RevocationList::new(),
                    HomePeerManagementRequest::Rename {
                        label: "NAS\u{202e}hidden".into(),
                    },
                )
                .await
                .is_err()
            );
            let renamed = crate::client::RelayClient::manage_home_peer(
                &client_endpoint,
                relay.node_addr(),
                user.public_key,
                host.public_key,
                admin_cert.clone(),
                admin_key.clone(),
                RevocationList::new(),
                HomePeerManagementRequest::Rename {
                    label: "Living room NAS".into(),
                },
            )
            .await
            .unwrap();
            assert_eq!(renamed.label, "Living room NAS");
            let mut revocation = tilo_user_identity::RevocationEntry {
                user_public_key: user.public_key,
                revoked_device_public_key: host.public_key,
                revoked_at_ns: unix_now_ns(),
                reason: tilo_user_identity::RevocationReason::Retired,
                signature: [0; 64],
            };
            tilo_user_identity::sign_revocation_entry(&mut revocation, &root).unwrap();
            let removed = crate::client::RelayClient::manage_home_peer(
                &client_endpoint,
                relay.node_addr(),
                user.public_key,
                host.public_key,
                admin_cert.clone(),
                admin_key.clone(),
                RevocationList::new(),
                HomePeerManagementRequest::Decommission { revocation },
            )
            .await
            .unwrap();
            assert!(removed.decommissioned);
            assert!(temp.path().join("relay/decommissioned").exists());
            assert!(
                crate::client::RelayClient::manage_home_peer(
                    &client_endpoint,
                    relay.node_addr(),
                    user.public_key,
                    host.public_key,
                    admin_cert,
                    admin_key,
                    RevocationList::new(),
                    HomePeerManagementRequest::Status,
                )
                .await
                .is_err()
            );
            client_endpoint.close().await;
            relay.shutdown().await;
        }
    }

    #[test]
    fn additional_protocol_rejects_relay_owned_alpns() {
        for alpn in [ALPN, DISCOVERY_ALPN, MANAGEMENT_ALPN, COMPUTE_ALPN] {
            assert!(matches!(
                validate_additional_alpn(alpn),
                Err(RelayError::ProtocolRegistration(_))
            ));
        }
        validate_additional_alpn(b"tilo-workspace-join/1").unwrap();
        assert!(matches!(
            validate_additional_alpns([
                b"tilo-workspace-join/1".as_slice(),
                b"tilo-workspace-join/1".as_slice(),
            ]),
            Err(RelayError::ProtocolRegistration(_))
        ));
    }
}
