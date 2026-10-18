//! The long-running daemon: binds an iroh endpoint, accepts on
//! `tilo-relay/1`, runs the auth handshake, hands off to
//! [`crate::peer::run_peer`].
//!
//! Also exposes the `Relay` struct so integration tests can spin up
//! two relays in the same process and let them merge.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use iroh::endpoint::{Endpoint, RecvStream, SendStream, presets};
use iroh::protocol::{AcceptError, DynProtocolHandler, ProtocolHandler, Router};
use iroh_base::{EndpointAddr, PublicKey};
use tilo_clock::{Clock, unix_now, unix_now_ns};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use tilo_user_identity::{
    DeviceCert, RevocationList, verify_device_cert_at, verify_revocation_entry,
};

use crate::auth::PeerRole;
use crate::client::RelayClient;
use crate::compute_session::ComputeProtocol;
use crate::compute_transport::COMPUTE_ALPN;
use crate::compute_worker::{ProductionComputeWorkerConfig, run_production_compute_worker};
use crate::config::{PeerRelayConfig, RelayConfig, WorkspaceBinding};
use crate::handshake::{EndpointTrust, HandshakeError, LiveIdentity};
use crate::metrics::Metrics;
use crate::peer::{
    PeerContext, WorkspaceAutobind, WorkspaceRegistry, get_or_autobind_workspace, run_peer,
};
use crate::policy::TrustPolicy;
use crate::transport::{
    ALPN, ContentEndpointAdvertisement, ContentEndpointRequest, DISCOVERY_ALPN, Envelope,
    HomePeerManagementRequest, HomePeerManagementResponse, MANAGEMENT_ALPN, MAX_FRAME_BYTES,
};
use crate::workspace::{Membership, SharedWorkspace, WorkspaceRuntime};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RelayStartupMode {
    CertifiedHost,
    NativeInteractiveWorkspaceOwner,
}

/// Replaces a governed workspace's admitted members and the handshake
/// allowlist derived from them, one replacement at a time.
#[derive(Clone)]
pub(crate) struct GovernedAccessWriter {
    workspaces: WorkspaceRegistry,
    handshake_trust: HandshakeTrust,
    update: Arc<AsyncMutex<()>>,
    state_root: PathBuf,
    shell_payloads: Arc<dyn crate::payloads::ShellPayloads>,
}

impl GovernedAccessWriter {
    pub(crate) fn state_root(&self) -> &std::path::Path {
        &self.state_root
    }

    /// The adapter the relay opens workspaces with.
    pub(crate) const fn shell_payloads(&self) -> &Arc<dyn crate::payloads::ShellPayloads> {
        &self.shell_payloads
    }

    /// Replace the standing of each member device of the room.
    pub(crate) async fn replace_standings(
        &self,
        workspace_id: u64,
        pinned: bool,
        standings: crate::workspace::Standings,
    ) -> Result<(), RelayError> {
        let workspace = crate::workspace_io::get_or_initialize(
            &self.workspaces,
            self.state_root.clone(),
            &self.shell_payloads,
            workspace_id,
            pinned,
            crate::room_access::AdmissionMode::Explicit,
            crate::peer::MAX_AUTOBOUND_WORKSPACES,
        )
        .await?;
        workspace.lock().await.standings = Some(Arc::new(standings));
        Ok(())
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the flagged value is a shared workspace handle (an Arc clone); dropping it earlier releases no lock"
    )]
    pub(crate) async fn replace(
        &self,
        workspace_id: u64,
        pinned: bool,
        access: tilo_peer_state::GovernedAccess,
    ) -> Result<(), RelayError> {
        if self.handshake_trust.allow_any {
            return Err(RelayError::GovernedAccess(
                "allow_any cannot host governed workspace admission".into(),
            ));
        }
        let _update = self.update.lock().await;
        let workspace = crate::workspace_io::get_or_initialize(
            &self.workspaces,
            self.state_root.clone(),
            &self.shell_payloads,
            workspace_id,
            pinned,
            crate::room_access::AdmissionMode::Explicit,
            crate::peer::MAX_AUTOBOUND_WORKSPACES,
        )
        .await?;
        let mut member_devices = access.member_devices;
        member_devices.sort_unstable();
        member_devices.dedup();
        let mut member_user_roots = access.member_user_roots;
        member_user_roots.sort_unstable();
        member_user_roots.dedup();
        let result = crate::workspace_io::replace_governed_access(
            &workspace,
            crate::store::GovernedAccessRecord {
                member_devices,
                member_user_roots,
            },
        )
        .await;
        // Also run after an I/O error: replace_governed_access keeps every
        // requested removal narrowed in memory, and the handshake union must
        // reflect that fail-closed ceiling.
        self.handshake_trust.refresh(&self.workspaces).await;
        result?;
        Ok(())
    }
}

#[derive(Clone)]
struct HandshakeTrust {
    base_roots: Arc<HashSet<[u8; 32]>>,
    authority_roots: Arc<std::sync::RwLock<HashSet<[u8; 32]>>>,
    live_policy: Arc<std::sync::RwLock<TrustPolicy>>,
    allow_any: bool,
}

impl HandshakeTrust {
    fn new(
        operator_policy: &TrustPolicy,
        effective_policy: &TrustPolicy,
        own_user: [u8; 32],
    ) -> Self {
        let allow_any = matches!(operator_policy, TrustPolicy::AllowAny);
        Self {
            base_roots: Arc::new(policy_roots(operator_policy, own_user)),
            authority_roots: Arc::new(std::sync::RwLock::new(policy_roots(
                effective_policy,
                own_user,
            ))),
            live_policy: Arc::new(std::sync::RwLock::new(if allow_any {
                TrustPolicy::AllowAny
            } else {
                TrustPolicy::AllowList { users: Vec::new() }
            })),
            allow_any,
        }
    }

    fn replace_authority_roots(&self, roots: HashSet<[u8; 32]>) {
        *self.authority_roots.write_recover() = roots;
    }

    async fn refresh(&self, workspaces: &WorkspaceRegistry) {
        if self.allow_any {
            return;
        }
        let mut roots = self.base_roots.as_ref().clone();
        roots.extend(self.authority_roots.read_recover().iter().copied());
        let runtimes = workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for runtime in runtimes {
            roots.extend(runtime.lock().await.governed_user_roots.iter().copied());
        }
        let mut users = roots.into_iter().map(hex::encode).collect::<Vec<_>>();
        users.sort_unstable();
        let next = TrustPolicy::AllowList { users };
        let previous = std::mem::replace(&mut *self.live_policy.write_recover(), next.clone());
        if narrows(&previous, &next) {
            crate::authority_epoch::bump();
        }
    }
}

/// Whether `next` trusts less than `previous` did: a user root it no longer
/// lists. One that only adds roots, as each person a public room admits
/// does, ends nobody's session and announces nothing.
fn narrows(previous: &TrustPolicy, next: &TrustPolicy) -> bool {
    match (previous, next) {
        (TrustPolicy::AllowList { users: before }, TrustPolicy::AllowList { users: after }) => {
            let after: HashSet<&String> = after.iter().collect();
            before.iter().any(|user| !after.contains(user))
        }
        _ => previous != next,
    }
}

fn policy_roots(policy: &TrustPolicy, own_user: [u8; 32]) -> HashSet<[u8; 32]> {
    let mut roots = HashSet::from([own_user]);
    if let TrustPolicy::AllowList { users } = policy {
        roots.extend(users.iter().filter_map(|encoded| {
            let bytes = hex::decode(encoded).ok()?;
            <[u8; 32]>::try_from(bytes).ok()
        }));
    }
    roots
}

/// A running relay. Prefer [`Relay::shutdown`]; dropping the handle also
/// stops the daemon and every federation dial task, but leaves peers to
/// notice by timeout.
pub struct Relay {
    /// Iroh endpoint the daemon accepts on.
    pub endpoint: Endpoint,
    /// Iroh router that wires the ALPN → protocol handler.
    router: Router,
    /// Runtime state shared with peer tasks.
    workspaces: WorkspaceRegistry,
    /// Metrics — cheap counters.
    metrics: Arc<Metrics>,
    /// Own certified device, signing key, user root and live revocations.
    /// Dedicated hosts require `can_act_as_host`; the typed interactive-owner
    /// path is instead bound to one shell authority.
    identity: LiveIdentity,
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
    handshake_trust: HandshakeTrust,
    governed_access_update: Arc<AsyncMutex<()>>,
    state_root: PathBuf,
    /// Classifies co-edit bodies for every workspace this relay opens.
    shell_payloads: Arc<dyn crate::payloads::ShellPayloads>,
    /// The rooms this relay governs; absent when it hosts no room authority.
    rooms: Option<crate::rooms::Rooms>,
    /// Exact server-side room ceiling for native interactive startup. A
    /// certified host retains the existing dynamic multi-workspace behavior.
    interactive_workspace_id: Option<u64>,
    interactive_owner_trust: Option<tilo_delivery_protocol::workspace_join::InteractiveOwnerTrust>,
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

/// Bundle of identity material the relay needs to run. Loaded by
/// the daemon's `main.rs` from
/// [`tilo_user_identity::persist`] and passed in explicitly so the
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
            RelayStartupMode::CertifiedHost,
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
            RelayStartupMode::CertifiedHost,
            Planes::desktop(None),
            Arc::new(tilo_clock::SystemClock),
        ))
        .await
    }

    /// Start a single native-owned governed workspace on the shell's already
    /// bound endpoint. This path accepts an interactive primary certificate
    /// only when the one configured binding carries the exact local shell
    /// authority and durable-baseline contract for that same device and room.
    /// It never grants general host capability or relay federation.
    ///
    /// # Errors
    ///
    /// Returns [`RelayError::Identity`] unless the binding is unambiguous,
    /// canonically room-bound, and governed by the exact configured identity.
    pub async fn start_native_interactive_workspace_on_endpoint_with_protocols(
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
            RelayStartupMode::NativeInteractiveWorkspaceOwner,
            Planes::desktop(None),
            Arc::new(tilo_clock::SystemClock),
        ))
        .await
    }

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
            RelayStartupMode::CertifiedHost,
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
            RelayStartupMode::CertifiedHost,
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
            RelayStartupMode::CertifiedHost,
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
            RelayStartupMode::CertifiedHost,
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
        startup_mode: RelayStartupMode,
        planes: Planes,
        replica_clock: Arc<dyn Clock>,
    ) -> Result<Self, RelayError> {
        let Planes {
            app_serving,
            shell_payloads,
        } = planes;
        validate_additional_protocols(&additional_protocols)?;
        let operator_trust_policy = cfg.trust_policy.clone();
        match startup_mode {
            RelayStartupMode::CertifiedHost => {
                import_home_peer_bindings(&mut cfg)?;
                validate_relay_identity(&identity)?;
                if !cfg.device_role.can_serve() {
                    return Err(RelayError::Identity(
                        "tilo-relay requires an always-on-server or hybrid device role".into(),
                    ));
                }
            }
            RelayStartupMode::NativeInteractiveWorkspaceOwner => {
                validate_native_interactive_workspace_identity(&cfg, &identity)?;
                cfg.services = crate::mesh::ServiceCapabilities::default();
            }
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
            replication: cfg
                .workspaces
                .iter()
                .map(|binding| (binding.id, binding.replication.clone()))
                .collect(),
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
        let interactive_owner_trust =
            if startup_mode == RelayStartupMode::NativeInteractiveWorkspaceOwner {
                let binding = cfg.workspaces.first().ok_or_else(|| {
                    RelayError::Identity("interactive workspace binding disappeared".into())
                })?;
                let authority = binding.shell_authority.as_ref().ok_or_else(|| {
                    RelayError::Identity("interactive shell authority disappeared".into())
                })?;
                Some(
                    tilo_delivery_protocol::workspace_join::InteractiveOwnerTrust::verify(
                        authority.room().to_owned(),
                        binding.id,
                        authority.governor().clone(),
                        *endpoint.id().as_bytes(),
                    )
                    .map_err(|error| RelayError::Identity(error.to_string()))?,
                )
            } else {
                None
            };
        let workspaces = build_workspace_registry(
            &cfg,
            &shell_payloads,
            startup_mode == RelayStartupMode::NativeInteractiveWorkspaceOwner,
        )?;
        if startup_mode == RelayStartupMode::NativeInteractiveWorkspaceOwner {
            let runtimes = workspaces
                .lock()
                .await
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for runtime in runtimes {
                let mut runtime = runtime.lock().await;
                runtime.membership_ceiling = Some(HashSet::new());
                runtime.governed_user_roots.clear();
                runtime.admission = crate::room_access::AdmissionMode::Governed;
            }
        }
        let metrics = Arc::new(Metrics::default());
        let revocations = Arc::new(std::sync::RwLock::new(identity.revocations.clone()));
        let live = LiveIdentity {
            user_public_key: identity.user_public_key,
            cert: identity.device_cert.clone(),
            signing_key: identity.device_signing_key.clone(),
            revocations: revocations.clone(),
        };

        let handshake_trust = HandshakeTrust::new(
            &operator_trust_policy,
            &cfg.trust_policy,
            identity.user_public_key,
        );
        handshake_trust.refresh(&workspaces).await;
        let live_trust_policy = handshake_trust.live_policy.clone();
        // A certified host governs the rooms it creates, unless its embedder
        // serves workspace joining itself. It answers their governance as
        // actions itself, beside whatever apps its embedder serves.
        let governs_rooms = startup_mode == RelayStartupMode::CertifiedHost
            && !handshake_trust.allow_any
            && !additional_protocols.iter().any(|(alpn, _)| {
                alpn.as_slice() == tilo_delivery_protocol::workspace_join::WORKSPACE_JOIN_ALPN
                    || alpn.as_slice() == crate::rooms::ROOM_ADMIN_ALPN
            });
        let governance = governs_rooms.then(crate::rooms::HostRoomActions::new);
        let app_serving = match &governance {
            Some(governance) => Some(Arc::new(crate::rooms::GovernedPlane {
                governance: governance.clone(),
                apps: app_serving,
            })
                as Arc<dyn crate::app_serving::AppServingPlane>),
            None => app_serving,
        };
        // A certified host routes its owner's compute jobs, so it answers
        // which of the owner's devices a job should run on, beside whatever
        // else it answers. It serves no apps by doing so: `app_serving`
        // above is unchanged.
        let app_serving = if startup_mode == RelayStartupMode::CertifiedHost {
            Some(Arc::new(crate::compute_actions::ComputePlane {
                compute: crate::compute_actions::ComputeActions::new(
                    identity.user_public_key,
                    workspaces.clone(),
                ),
                rest: app_serving,
            })
                as Arc<dyn crate::app_serving::AppServingPlane>)
        } else {
            app_serving
        };
        let handler = RelayProtocol {
            workspaces: workspaces.clone(),
            trust_policy: live_trust_policy.clone(),
            identity: live.clone(),
            metrics: metrics.clone(),
            autobind: (startup_mode == RelayStartupMode::CertifiedHost
                && matches!(cfg.trust_policy, TrustPolicy::AllowMatchingUser))
            .then(|| WorkspaceAutobind {
                state_root: cfg.state_root.clone(),
                shell_payloads: shell_payloads.clone(),
            }),
            app_serving,
            live_bridge: Arc::default(),
            agents: Arc::default(),
            compactor: crate::compaction::Compactor::new(
                identity.device_signing_key.clone(),
                identity.device_cert.clone(),
                shell_payloads.clone(),
            ),
            member_limits: cfg
                .member_limits
                .map(|limits| Arc::new(crate::member_limits::MemberLimits::new(limits))),
        };
        let discovery = DiscoveryProtocol {
            workspaces: workspaces.clone(),
            identity: live.clone(),
            trust_policy: live_trust_policy.clone(),
            mesh_profile: mesh_profile.clone(),
            full_replica: full_replica.clone(),
            compute_ready: Arc::clone(&compute_ready),
            home_peer_bundle: cfg.home_peer_bundle.clone(),
            autobind: (startup_mode == RelayStartupMode::CertifiedHost
                && matches!(cfg.trust_policy, TrustPolicy::AllowMatchingUser))
            .then(|| WorkspaceAutobind {
                state_root: cfg.state_root.clone(),
                shell_payloads: shell_payloads.clone(),
            }),
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
        let governed_access_update = Arc::new(AsyncMutex::new(()));
        let rooms = if governs_rooms {
            Some(
                crate::rooms::Rooms::open(
                    crate::rooms::RoomHost {
                        workspaces: workspaces.clone(),
                        access: GovernedAccessWriter {
                            workspaces: workspaces.clone(),
                            handshake_trust: handshake_trust.clone(),
                            update: Arc::clone(&governed_access_update),
                            state_root: cfg.state_root.clone(),
                            shell_payloads: shell_payloads.clone(),
                        },
                        records: crate::room_records::RoomRecords::new(
                            workspaces.clone(),
                            identity.device_cert.device_public_key,
                            identity.user_public_key,
                            identity.device_signing_key.clone(),
                            metrics.clone(),
                        ),
                        endpoint: endpoint.clone(),
                        cert: identity.device_cert.clone(),
                        key: identity.device_signing_key.clone(),
                        revocations: revocations.clone(),
                        max_pre_admissions: cfg
                            .max_room_grants
                            .unwrap_or(crate::rooms::DEFAULT_MAX_PRE_ADMISSIONS),
                        #[cfg(feature = "agents")]
                        trust_policy: handler.trust_policy.clone(),
                        agents: handler.agents.clone(),
                    },
                    cfg.state_root.join("rooms"),
                )
                .await
                .map_err(|error| RelayError::Identity(format!("governed rooms: {error}")))?,
            )
        } else {
            None
        };
        if let (Some(governance), Some(rooms)) = (&governance, &rooms) {
            governance.attach(rooms.clone());
        }
        #[cfg(any(feature = "agents", feature = "web"))]
        if let Some(rooms) = &rooms {
            handler.compactor.steward_with(Arc::new(rooms.clone()));
        }
        let mut router = Router::builder(endpoint.clone())
            .accept(ALPN, handler)
            .accept(crate::certify::CERTIFY_ALPN, certification);
        if startup_mode == RelayStartupMode::CertifiedHost {
            router = router
                .accept(DISCOVERY_ALPN, discovery)
                .accept(MANAGEMENT_ALPN, management)
                .accept(COMPUTE_ALPN, compute);
        }
        if let Some(rooms) = &rooms {
            router = router
                .accept(
                    tilo_delivery_protocol::workspace_join::WORKSPACE_JOIN_ALPN,
                    crate::workspace_join::WorkspaceJoinProtocol::new(rooms.join_service()),
                )
                .accept(
                    crate::rooms::ROOM_ADMIN_ALPN,
                    crate::rooms::RoomAdminProtocol::new(rooms.clone()),
                )
                .accept(
                    tilo_relay_session::rooms::private::PRIVATE_ALPN,
                    crate::rooms::PrivateProtocol::new(rooms.clone()),
                );
        }
        for (alpn, handler) in additional_protocols {
            router = router.accept(alpn, handler);
        }
        let router = router.spawn();

        let peer_relay_tasks =
            spawn_background_tasks(&cfg, &identity, &endpoint, &workspaces, &metrics, &live);
        if cfg.services.compute {
            for workspace in workspaces.lock().await.values() {
                workspace
                    .lock()
                    .await
                    .admit_device(identity.device_cert.device_public_key)?;
            }
            let worker_config = ProductionComputeWorkerConfig {
                endpoint: endpoint.clone(),
                relay_address: endpoint.addr(),
                user_public_key: identity.user_public_key,
                device_cert: identity.device_cert.clone(),
                device_signing_key: identity.device_signing_key.clone(),
                revocations: identity.revocations.clone(),
                workspaces: cfg.workspaces.iter().map(|binding| binding.id).collect(),
                state_root: cfg.state_root.join("compute-worker"),
                readiness: Arc::clone(&compute_ready),
                environments,
                // Running jobs on the replica's copies only saves transfers;
                // without them every input is fetched.
                replicas: home_peer_replicas(&cfg.home_peer_bundle).unwrap_or_else(|error| {
                    tracing::warn!(%error, "compute worker runs without the home-peer replica");
                    BTreeMap::new()
                }),
            };
            let worker_task = tokio::spawn(async move {
                if let Err(error) = run_production_compute_worker(worker_config).await {
                    tracing::warn!(%error, "production compute worker withdrawn");
                }
            });
            push_peer_relay_task(&peer_relay_tasks, worker_task);
        }
        if startup_mode == RelayStartupMode::CertifiedHost
            && matches!(cfg.trust_policy, TrustPolicy::AllowList { .. })
            && cfg.home_peer_bundle.exists()
        {
            push_peer_relay_task(
                &peer_relay_tasks,
                spawn_home_peer_authority_refresh(
                    cfg.home_peer_bundle.clone(),
                    identity.user_public_key,
                    handshake_trust.clone(),
                    workspaces.clone(),
                ),
            );
        }

        let interactive_workspace_id = interactive_owner_trust
            .as_ref()
            .map(tilo_delivery_protocol::workspace_join::InteractiveOwnerTrust::workspace_id);
        Ok(Self {
            endpoint,
            router,
            workspaces,
            metrics,
            identity: live,
            #[cfg(feature = "web")]
            browser_origin: Arc::new(std::sync::RwLock::new(None)),
            peer_relay_tasks,
            mesh_profile,
            full_replica: full_replica.clone(),
            web,
            compute_ready,
            handshake_trust,
            governed_access_update,
            state_root: cfg.state_root,
            shell_payloads,
            interactive_workspace_id,
            interactive_owner_trust,
            rooms,
        })
    }

    /// Install the original native owner's bounded refresh mailbox for one bound room.
    /// Replacing an installed owner requires closing/recreating this relay.
    ///
    /// # Errors
    ///
    /// Returns `String` if the inputs violate this operation's bounds or invariants.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the runtime guard borrows the registry handle and ends with this short scope, right after its only write"
    )]
    pub async fn install_sync_refresh(
        &self,
        workspace_id: u64,
        mailbox: crate::peer::sync_refresh::Mailbox,
    ) -> Result<(), String> {
        let workspace = self
            .workspaces
            .lock()
            .await
            .get(&workspace_id)
            .cloned()
            .ok_or("checkpoint refresh workspace is not bound")?;
        let mut workspace = workspace.lock().await;
        if workspace.sync_refresh.is_some() {
            return Err("checkpoint refresh owner already installed".into());
        }
        workspace.sync_refresh = Some(mailbox);
        Ok(())
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
        if let Err(error) = self.router.shutdown().await {
            tracing::debug!(%error, "relay router shutdown task failed");
        }
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
        let task = PeerRelayDialer {
            endpoint: self.endpoint.clone(),
            identity: self.identity.clone(),
            workspaces: self.workspaces.clone(),
            metrics: self.metrics.clone(),
        }
        .spawn_at(addr, label, priority);
        push_peer_relay_task(&self.peer_relay_tasks, task);
    }

    /// Cheap external handle.
    pub fn handle(&self) -> RelayHandle {
        RelayHandle {
            node_addr: self.endpoint.addr(),
            metrics: self.metrics.clone(),
            mesh_profile: self.mesh_profile.clone(),
            full_replica: self.full_replica.clone(),
            compute_ready: Arc::clone(&self.compute_ready),
        }
    }

    /// Tell the host that a device is an agent: verify its root-signed
    /// delegation against this relay's user and confine the device to the
    /// delegated rooms from its next envelope on. Replaces an earlier
    /// delegation of the same device.
    ///
    /// # Errors
    ///
    /// Returns why the delegation is not usable now.
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

    /// The compactor keeping this relay's retained document channels short.
    pub fn compactor(&self) -> &crate::compaction::Compactor {
        &self.web.relay.compactor
    }

    /// The local sockets the relay's endpoint is bound to.
    #[must_use]
    pub fn bound_sockets(&self) -> Vec<std::net::SocketAddr> {
        self.endpoint.bound_sockets()
    }

    /// Bind (or re-bind) a workspace's membership set. Called by
    /// the admin CLI after `bind-workspace`.
    ///
    /// # Errors
    ///
    /// Returns [`RelayError::GovernedAccess`] when native interactive mode is
    /// asked to mutate any workspace other than its configured room, or
    /// [`RelayError::Store`] if opening a not-yet-bound workspace or
    /// persisting its membership list fails.
    pub async fn set_workspace_membership(
        &self,
        workspace_id: u64,
        pinned: bool,
        members: Vec<[u8; 32]>,
        state_root: PathBuf,
    ) -> Result<(), RelayError> {
        self.ensure_workspace_mutation_scope(workspace_id)?;
        let workspace = crate::workspace_io::get_or_initialize(
            &self.workspaces,
            state_root,
            &self.shell_payloads,
            workspace_id,
            pinned,
            crate::room_access::AdmissionMode::Explicit,
            crate::peer::MAX_AUTOBOUND_WORKSPACES,
        )
        .await?;
        crate::workspace_io::replace_membership(&workspace, members).await?;
        Ok(())
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
        if self.handshake_trust.allow_any {
            return Err(RelayError::GovernedAccess(
                "allow_any cannot host governed workspace admission".into(),
            ));
        }
        if state_root != self.state_root {
            return Err(RelayError::GovernedAccess(
                "state root does not match the running relay".into(),
            ));
        }
        self.ensure_workspace_mutation_scope(workspace_id)?;
        self.governed_access_writer()
            .replace(workspace_id, pinned, access)
            .await
    }

    /// Let visitors watch `workspace_id` read-only as `spectators`: every
    /// device the spectator root certified reads the room's state and
    /// writes nothing, up to `spectators.seats` subscribed at once (see
    /// [`crate::room_access`]). The spectator person must be a member of the
    /// room, which is also what lets its devices through the handshake.
    ///
    /// # Errors
    ///
    /// The room is not open on this relay.
    pub async fn admit_spectators(
        &self,
        workspace_id: u64,
        spectators: crate::room_access::Spectators,
    ) -> Result<(), RelayError> {
        let workspace = self
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

    fn governed_access_writer(&self) -> GovernedAccessWriter {
        GovernedAccessWriter {
            workspaces: self.workspaces.clone(),
            handshake_trust: self.handshake_trust.clone(),
            update: Arc::clone(&self.governed_access_update),
            state_root: self.state_root.clone(),
            shell_payloads: self.shell_payloads.clone(),
        }
    }

    /// The rooms this relay governs, when it hosts the room authority.
    #[must_use]
    pub const fn rooms(&self) -> Option<&crate::rooms::Rooms> {
        self.rooms.as_ref()
    }

    /// Where this relay keeps its state.
    #[cfg(feature = "web")]
    pub(crate) fn state_root(&self) -> &std::path::Path {
        &self.state_root
    }

    fn ensure_workspace_mutation_scope(&self, workspace_id: u64) -> Result<(), RelayError> {
        if self
            .interactive_workspace_id
            .is_some_and(|configured| configured != workspace_id)
        {
            return Err(RelayError::GovernedAccess(
                "interactive workspace owner cannot mutate an unconfigured workspace".into(),
            ));
        }
        Ok(())
    }

    /// Return the opaque one-room trust token derived during interactive
    /// startup. Certified-host relays return `None`.
    #[must_use]
    pub fn native_interactive_owner_trust(
        &self,
    ) -> Option<tilo_delivery_protocol::workspace_join::InteractiveOwnerTrust> {
        self.interactive_owner_trust.clone()
    }

    /// Own user identity — for tests.
    pub fn own_user_public_key(&self) -> [u8; 32] {
        self.identity.user_public_key
    }

    /// In-process access to the records of the workspaces this relay hosts,
    /// for a service embedded in the host process.
    #[must_use]
    pub fn room_records(&self) -> crate::room_records::RoomRecords {
        crate::room_records::RoomRecords::new(
            self.workspaces.clone(),
            self.identity.cert.device_public_key,
            self.identity.user_public_key,
            self.identity.signing_key.clone(),
            self.metrics.clone(),
        )
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
    let now = unix_now();
    for workspace in bundle.workspaces {
        let (ticket, relay_workspace_ids) = home_peer_relay_workspaces(&workspace)?;
        // An FS authority is deliberately *not* a relay-room membership
        // list.  In particular, an FsPath grant lets a collaborator read a
        // path in the global Files namespace; it must never silently give
        // that person access to every shell room wired to the namespace.
        // Only the governor-signed members of a room admit a user to that
        // particular relay topic.
        // Do not seed a just-restarted relay from the enrollment copy when a
        // newer materialized authority has already been observed.  The
        // refresh task starts after the router, so this check closes the
        // otherwise-real admission window during startup.
        let high_water = load_authority_high_water(&workspace.cache_dir).map_err(|error| {
            RelayError::Identity(format!("invalid authority high-water: {error}"))
        })?;
        let authority = match current_workspace_authority(&workspace) {
            Ok(authority) => {
                verify_and_advance_authority(&workspace, &ticket, &authority).map_err(|error| {
                    RelayError::Identity(format!("home-peer authority rejected: {error}"))
                })?;
                Some(authority)
            }
            // Preserve compatibility with pre-authority bundles only when
            // this relay has never accepted an authority for the workspace.
            Err(_) if high_water.is_none() && workspace.authority.is_none() => None,
            Err(error) => {
                return Err(RelayError::Identity(format!(
                    "home-peer current authority is unavailable: {error}"
                )));
            }
        };
        let mut authority_users = Vec::new();
        let mut governor = None;
        if let Some(authority) = authority.as_ref() {
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
            if let Some(authority) = authority.as_ref()
                && !crate::personal_continuity_workspace_id_matches(&authority.governor, id)
            {
                for member in authority
                    .room_members
                    .get(&id.to_string())
                    .into_iter()
                    .flatten()
                    .filter(|member| member.is_live_at(now))
                {
                    let user = hex::encode(member.user.as_bytes());
                    room_users.push(user.clone());
                    collaborators.push(user);
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
                    shell_authority: None,
                    checkpoint_prefix_recovery: crate::config::CheckpointPrefixRecovery::Disabled,
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

/// Open every configured workspace binding on disk and build the
/// shared registry `run_peer` and the peer-relay session loop route
/// through.
fn build_workspace_registry(
    cfg: &RelayConfig,
    shell_payloads: &Arc<dyn crate::payloads::ShellPayloads>,
    require_durable_snapshot: bool,
) -> Result<WorkspaceRegistry, RelayError> {
    let mut wsmap: HashMap<u64, SharedWorkspace> = HashMap::new();
    for binding in &cfg.workspaces {
        let runtime = open_bound_workspace(cfg, binding, shell_payloads, require_durable_snapshot)?;
        wsmap.insert(binding.id, Arc::new(AsyncMutex::new(runtime)));
    }
    Ok(Arc::new(AsyncMutex::new(wsmap)))
}

/// Open the runtime of one configured workspace binding.
fn open_bound_workspace(
    cfg: &RelayConfig,
    binding: &WorkspaceBinding,
    shell_payloads: &Arc<dyn crate::payloads::ShellPayloads>,
    require_durable_snapshot: bool,
) -> Result<WorkspaceRuntime, RelayError> {
    let mut rt = if require_durable_snapshot {
        WorkspaceRuntime::open_with_required_snapshot(
            &cfg.state_root,
            binding.id,
            binding.pin,
            Membership::default(),
            shell_payloads.clone(),
        )
        .map_err(|error| {
            RelayError::Identity(format!(
                "interactive workspace owner requires a valid durable snapshot: {error}"
            ))
        })?
    } else {
        WorkspaceRuntime::open_with_payloads(
            &cfg.state_root,
            binding.id,
            binding.pin,
            Membership::default(),
            shell_payloads.clone(),
        )?
    };
    if let Some(authority) = &binding.shell_authority {
        // The configured authority is typed at the config edge; the store's
        // shell adapter reads its serialised form.
        let authority = serde_json::to_value(authority)
            .map_err(|error| crate::store::StoreError::Json(error.to_string()))?;
        rt.store.bind_shell(&authority)?;
    }
    rt.checkpoint_prefix_recovery = binding.checkpoint_prefix_recovery;
    rt.admission = rt.admission.rebind(
        if matches!(cfg.trust_policy, TrustPolicy::AllowMatchingUser) {
            crate::room_access::AdmissionMode::SameOwner
        } else {
            crate::room_access::AdmissionMode::Explicit
        },
    );
    rt.admitted_users = binding
        .admitted_users
        .iter()
        .filter_map(|encoded| hex::decode(encoded).ok())
        .filter_map(|bytes| bytes.try_into().ok())
        .collect();
    Ok(rt)
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

    let membership_task = crate::membership_watch::spawn_membership_watch(&cfg.state_root);
    push_peer_relay_task(&peer_relay_tasks, membership_task);

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

const AUTHORITY_HIGH_WATER_FILE: &str = "workspace-authority-high-water.bin";

/// The part of a workspace authority that must survive a relay restart.  The
/// digest also makes equal-revision equivocation fail closed: a different
/// signed payload is not a retry of the authority we already accepted.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct AuthorityHighWater {
    workspace: uuid::Uuid,
    governor: [u8; 32],
    revision: u64,
    digest: [u8; 32],
}

#[derive(Default)]
struct AuthorityRefreshState {
    /// Rooms previously governed by this bundle.  Keep them so a malformed
    /// bundle/current authority revokes their live admission rather than
    /// leaving the last good projection active forever.
    governed_rooms: std::collections::HashSet<u64>,
}

fn authority_high_water_path(cache_dir: &std::path::Path) -> PathBuf {
    cache_dir.join(AUTHORITY_HIGH_WATER_FILE)
}

fn authority_digest(
    authority: &fs::workspace_authority::WorkspaceAuthorityEnvelope,
) -> Result<[u8; 32], String> {
    postcard::to_allocvec(authority)
        .map(|bytes| *blake3::hash(&bytes).as_bytes())
        .map_err(|error| format!("could not encode signed authority: {error}"))
}

fn load_authority_high_water(
    cache_dir: &std::path::Path,
) -> Result<Option<AuthorityHighWater>, String> {
    let path = authority_high_water_path(cache_dir);
    match std::fs::read(&path) {
        Ok(bytes) => postcard::from_bytes(&bytes)
            .map(Some)
            .map_err(|error| format!("{} is corrupt: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

fn persist_authority_high_water(
    cache_dir: &std::path::Path,
    authority: &fs::workspace_authority::WorkspaceAuthorityEnvelope,
    digest: [u8; 32],
) -> Result<(), String> {
    let high_water = AuthorityHighWater {
        workspace: authority.workspace,
        governor: authority.governor,
        revision: authority.revision,
        digest,
    };
    let bytes = postcard::to_allocvec(&high_water)
        .map_err(|error| format!("could not encode authority high-water mark: {error}"))?;
    let path = authority_high_water_path(cache_dir);
    tilo_files::native::atomic::write_creating_parent(&path, &bytes)
        .map_err(|error| format!("could not persist {}: {error}", path.display()))
}

/// Read the current materialized authority.  A corrupt materialized file is
/// never silently replaced by the enrollment bundle: after a newer revision
/// was observed that would resurrect an old revocation or tombstone.
fn current_workspace_authority(
    workspace: &fs::full_replica::HomePeerBundle,
) -> Result<fs::workspace_authority::WorkspaceAuthorityEnvelope, String> {
    let path = workspace.cache_dir.join("workspace-authority.bin");
    match std::fs::read(&path) {
        Ok(bytes) => postcard::from_bytes(&bytes)
            .map_err(|error| format!("{} is corrupt: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            workspace.authority.clone().ok_or_else(|| {
                format!(
                    "{} is missing and the enrollment bundle has no authority",
                    path.display()
                )
            })
        }
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

fn verify_and_advance_authority(
    workspace: &fs::full_replica::HomePeerBundle,
    ticket: &fs::Ticket,
    authority: &fs::workspace_authority::WorkspaceAuthorityEnvelope,
) -> Result<(), String> {
    let high_water = load_authority_high_water(&workspace.cache_dir)?;
    let expected_governor = high_water
        .as_ref()
        .map(|record| record.governor)
        // The enrollment bundle is the governor trust anchor before the
        // relay has persisted an authority itself.
        .or_else(|| {
            workspace
                .authority
                .as_ref()
                .map(|authority| authority.governor)
        })
        .unwrap_or(authority.governor);
    authority
        .verify(ticket.workspace_id(), expected_governor)
        .map_err(|error| format!("authority signature or governor is invalid: {error}"))?;
    let digest = authority_digest(authority)?;

    if let Some(record) = high_water {
        if record.workspace != authority.workspace {
            return Err("authority high-water workspace does not match the bundle".into());
        }
        if record.governor != authority.governor {
            return Err("authority governor regressed from the durable high-water mark".into());
        }
        if authority.revision < record.revision {
            return Err(format!(
                "authority revision {} regressed below durable revision {}",
                authority.revision, record.revision
            ));
        }
        if authority.revision == record.revision {
            if digest != record.digest {
                return Err("authority equivocated at its durable revision".into());
            }
            return Ok(());
        }
    }
    persist_authority_high_water(&workspace.cache_dir, authority, digest)
}

async fn apply_home_peer_projection(
    handshake_trust: &HandshakeTrust,
    workspaces: &WorkspaceRegistry,
    own_user: [u8; 32],
    state: &mut AuthorityRefreshState,
    users: Vec<String>,
    room_users: HashMap<u64, std::collections::HashSet<[u8; 32]>>,
    next_governed_rooms: std::collections::HashSet<u64>,
) {
    handshake_trust.replace_authority_roots(
        users
            .into_iter()
            .filter_map(|encoded| {
                hex::decode(encoded)
                    .ok()
                    .and_then(|bytes| bytes.try_into().ok())
            })
            .collect(),
    );
    let affected_rooms = state
        .governed_rooms
        .union(&next_governed_rooms)
        .copied()
        .collect::<Vec<_>>();
    let runtimes = workspaces.lock().await;
    for room in affected_rooms {
        if let Some(runtime) = runtimes.get(&room) {
            let next = room_users.get(&room).cloned().unwrap_or_default();
            let mut runtime = runtime.lock().await;
            let changed = runtime.admitted_users != next;
            runtime.admitted_users = next;
            drop(runtime);
            if changed {
                crate::authority_epoch::bump();
            }
        }
    }
    drop(runtimes);
    state.governed_rooms = next_governed_rooms;
    let _ = own_user; // The configured base pins always retain the relay owner.
    handshake_trust.refresh(workspaces).await;
}

type HomePeerAuthorityProjection = (
    Vec<String>,
    HashMap<u64, std::collections::HashSet<[u8; 32]>>,
    std::collections::HashSet<u64>,
);

/// Re-evaluate the materialized authority at `now`, even when no file bytes
/// changed.  Capability expiry is wall-clock state, not source-file state.
/// A rejected/missing current authority clears every previously governed room
/// and leaves only this relay's own user admitted.
async fn refresh_home_peer_authority_once(
    bundle_path: &std::path::Path,
    own_user: [u8; 32],
    handshake_trust: &HandshakeTrust,
    workspaces: &WorkspaceRegistry,
    state: &mut AuthorityRefreshState,
    now: u64,
) -> bool {
    let mut refresh = || -> Result<HomePeerAuthorityProjection, String> {
        let bytes = std::fs::read(bundle_path)
            .map_err(|error| format!("could not read {}: {error}", bundle_path.display()))?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| format!("{} is not UTF-8", bundle_path.display()))?;
        let bundle = toml::from_str::<fs::full_replica::HomePeerSet>(text)
            .map_err(|error| format!("{} is invalid: {error}", bundle_path.display()))?;
        let global_namespace = bundle.workspaces.len() == 1;
        let mut users = vec![hex::encode(own_user)];
        let mut room_users: HashMap<u64, std::collections::HashSet<[u8; 32]>> = HashMap::new();
        let mut governed_rooms = std::collections::HashSet::new();

        for workspace in bundle.workspaces {
            let ticket = fs::Ticket::decode(&workspace.ticket).map_err(|_| {
                "home-peer authority refresh rejected an invalid Files ticket".to_string()
            })?;
            governed_rooms.extend(workspace.relay_workspace_ids.iter().copied());
            // Remember rooms as soon as their bundle mapping is readable, so
            // an invalid current authority clears them below.
            state.governed_rooms.extend(governed_rooms.iter().copied());
            let authority = current_workspace_authority(&workspace)?;
            verify_and_advance_authority(&workspace, &ticket, &authority)?;

            if authority.tombstoned {
                continue;
            }
            users.push(hex::encode(authority.governor));
            for room in &workspace.relay_workspace_ids {
                room_users
                    .entry(*room)
                    .or_default()
                    .insert(authority.governor);
            }
            for (room, members) in &authority.room_members {
                let Ok(room) = room.parse::<u64>() else {
                    continue;
                };
                if (!global_namespace && !workspace.relay_workspace_ids.contains(&room))
                    || crate::personal_continuity_workspace_id_matches(&authority.governor, room)
                {
                    continue;
                }
                for member in members.iter().filter(|member| member.is_live_at(now)) {
                    users.push(hex::encode(member.user.as_bytes()));
                    governed_rooms.insert(room);
                    room_users
                        .entry(room)
                        .or_default()
                        .insert(*member.user.as_bytes());
                }
            }
        }
        users.sort_unstable();
        users.dedup();
        Ok((users, room_users, governed_rooms))
    };

    match refresh() {
        Ok((users, room_users, governed_rooms)) => {
            apply_home_peer_projection(
                handshake_trust,
                workspaces,
                own_user,
                state,
                users,
                room_users,
                governed_rooms,
            )
            .await;
            true
        }
        Err(error) => {
            tracing::warn!(path = %bundle_path.display(), "home-peer authority refresh failed closed: {error}");
            let governed_rooms = std::mem::take(&mut state.governed_rooms);
            apply_home_peer_projection(
                handshake_trust,
                workspaces,
                own_user,
                state,
                vec![hex::encode(own_user)],
                HashMap::new(),
                governed_rooms,
            )
            .await;
            false
        }
    }
}

/// Keep relay handshake admission derived from the same durable signed
/// authority that the filesystem worker materializes.
fn spawn_home_peer_authority_refresh(
    bundle_path: PathBuf,
    own_user: [u8; 32],
    handshake_trust: HandshakeTrust,
    workspaces: WorkspaceRegistry,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut state = AuthorityRefreshState::default();
        loop {
            interval.tick().await;
            let _ = refresh_home_peer_authority_once(
                &bundle_path,
                own_user,
                &handshake_trust,
                &workspaces,
                &mut state,
                unix_now(),
            )
            .await;
        }
    })
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
    validate_relay_device_identity(identity)?;
    if !identity.device_cert.capabilities.can_act_as_host {
        return Err(RelayError::Identity(
            "device certificate is not host-capable".into(),
        ));
    }
    Ok(())
}

fn validate_relay_device_identity(identity: &RelayIdentity) -> Result<(), RelayError> {
    verify_device_cert_at(&identity.device_cert, unix_now_ns())
        .map_err(|error| RelayError::Identity(format!("invalid device certificate: {error}")))?;
    if identity.device_cert.user_public_key != identity.user_public_key {
        return Err(RelayError::Identity(
            "device certificate does not chain to the configured user public key".into(),
        ));
    }
    if identity.device_cert.device_public_key
        != identity.device_signing_key.verifying_key().to_bytes()
    {
        return Err(RelayError::Identity(
            "device certificate does not match the configured device signing key".into(),
        ));
    }
    if identity
        .revocations
        .is_revoked(&identity.device_cert.device_public_key)
    {
        return Err(RelayError::Identity("device certificate is revoked".into()));
    }
    Ok(())
}

fn validate_native_interactive_workspace_identity(
    cfg: &RelayConfig,
    identity: &RelayIdentity,
) -> Result<(), RelayError> {
    validate_relay_device_identity(identity)?;
    if !identity.device_cert.capabilities.can_own_workspaces {
        return Err(RelayError::Identity(
            "interactive workspace owner cannot govern workspaces".into(),
        ));
    }
    if cfg.device_role != crate::mesh::DeviceRole::Interactive {
        return Err(RelayError::Identity(
            "interactive workspace owner requires the interactive device role".into(),
        ));
    }
    if !cfg.peer_relays.is_empty() {
        return Err(RelayError::Identity(
            "interactive workspace owner cannot configure relay federation".into(),
        ));
    }
    let [binding] = cfg.workspaces.as_slice() else {
        return Err(RelayError::Identity(
            "interactive workspace owner requires exactly one workspace binding".into(),
        ));
    };
    if matches!(cfg.trust_policy, TrustPolicy::AllowAny) || !binding.admitted_users.is_empty() {
        return Err(RelayError::Identity(
            "interactive workspace owner requires exact governed admission".into(),
        ));
    }
    let Some(authority) = binding.shell_authority.as_ref() else {
        return Err(RelayError::Identity(
            "interactive workspace owner requires local shell authority".into(),
        ));
    };
    if !binding.pin
        || binding.checkpoint_prefix_recovery
            != crate::config::CheckpointPrefixRecovery::NativeOwnerDurableBaseline
    {
        return Err(RelayError::Identity(
            "interactive workspace owner requires a pinned native durable baseline".into(),
        ));
    }
    if !crate::room_workspace_id_matches(authority.room(), binding.id) {
        return Err(RelayError::Identity(
            "interactive shell authority room does not match its workspace".into(),
        ));
    }
    if authority.governor() != &identity.device_cert {
        return Err(RelayError::Identity(
            "interactive shell authority is not the configured device certificate".into(),
        ));
    }
    Ok(())
}

/// The `ProtocolHandler` iroh wires to the ALPN.
#[derive(Clone)]
pub(crate) struct RelayProtocol {
    pub(crate) workspaces: WorkspaceRegistry,
    pub(crate) trust_policy: Arc<std::sync::RwLock<TrustPolicy>>,
    pub(crate) identity: LiveIdentity,
    pub(crate) metrics: Arc<Metrics>,
    pub(crate) autobind: Option<WorkspaceAutobind>,
    pub(crate) app_serving: Option<Arc<dyn crate::app_serving::AppServingPlane>>,
    /// Set once when the host starts serving browsers.
    pub(crate) live_bridge: Arc<std::sync::OnceLock<crate::peer::LiveBridge>>,
    pub(crate) agents: crate::peer::AgentRegistry,
    pub(crate) compactor: crate::compaction::Compactor,
    pub(crate) member_limits: Option<Arc<crate::member_limits::MemberLimits>>,
}

impl RelayProtocol {
    /// This relay's certificate and agent trust.
    #[cfg(feature = "web")]
    pub(crate) fn trust(&self) -> crate::room_access::RelayTrust<'_> {
        crate::room_access::RelayTrust {
            revocations: &self.identity.revocations,
            policy: &self.trust_policy,
            own_user: &self.identity.user_public_key,
            agents: &self.agents,
        }
    }

    /// The peer context a session of this relay runs with.
    pub(crate) fn peer_context(&self) -> PeerContext {
        PeerContext {
            workspaces: self.workspaces.clone(),
            autobind: self.autobind.clone(),
            revocations: self.identity.revocations.clone(),
            trust_policy: self.trust_policy.clone(),
            own_user_public_key: self.identity.user_public_key,
            own_device_cert: self.identity.cert.clone(),
            metrics: self.metrics.clone(),
            own_device_public_key: self.identity.cert.device_public_key,
            app_serving: self.app_serving.clone(),
            live_bridge: self.live_bridge.get().cloned(),
            agents: self.agents.clone(),
            positions: Some(crate::position::PositionIssuer::new(
                &self.identity.signing_key.to_bytes(),
            )),
            compactor: Some(self.compactor.clone()),
            member_limits: self.member_limits.clone(),
        }
    }
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
        let here = workspace.lock().await.compute.environment_offer(now_ms);
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
                let mut entries =
                    crate::store::load_revocations(&self.state_root).map_err(|error| {
                        AcceptError::from_err(std::io::Error::other(error.to_string()))
                    })?;
                if !entries.iter().any(|entry| {
                    entry.revoked_device_public_key == revocation.revoked_device_public_key
                }) {
                    entries.push(revocation.clone());
                    crate::store::save_revocations(&self.state_root, &entries).map_err(
                        |error| AcceptError::from_err(std::io::Error::other(error.to_string())),
                    )?;
                }
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

impl std::fmt::Debug for RelayProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RelayProtocol")
    }
}

impl ProtocolHandler for RelayProtocol {
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
        let this = self.clone();
        let (send, recv) = connection.accept_bi().await?;
        // Run the whole session in a separate task so we return
        // promptly and the iroh router can accept the next
        // connection.
        tokio::spawn(async move {
            if let Err(e) = Box::pin(handle_conn(this, connection, send, recv)).await {
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
    // 1. The bounded shared handshake, under the live policy: the owner and
    // every user the relay currently admits.
    let client_peer = match ctx
        .identity
        .authenticate(
            EndpointTrust::Live(&ctx.trust_policy),
            &mut recv,
            &mut send,
            MAX_FRAME_BYTES,
        )
        .await
    {
        Ok(peer) => peer,
        Err(error) => {
            if matches!(error, HandshakeError::Auth(_)) {
                ctx.metrics.inc_auth_rejected();
            }
            return Err(RelayError::Iroh(error.to_string()));
        }
    };

    ctx.metrics.inc_conns_authed();

    // 2. Hand off to the peer loop.
    let peer_result = Box::pin(run_peer(
        client_peer,
        connection.clone(),
        recv,
        send,
        ctx.peer_context(),
    ))
    .await;
    if let Err(e) = peer_result {
        tracing::debug!("peer session ended with error: {e}");
    }
    connection.close(0u32.into(), b"relay session ended");
    Ok(())
}

/// The relay frame codec, re-exported under the daemon path older callers use.
pub use crate::frame::{read_frame, write_frame};

/// Spawn the outbound reconnect-with-backoff task for one peer
/// relay identified by its config entry. Parses the iroh id and
/// hands off to [`PeerRelayDialer::spawn_at`]'s dial loop.
fn spawn_peer_relay_dial(
    peer_cfg: PeerRelayConfig,
    endpoint: Endpoint,
    identity: LiveIdentity,
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
        PeerRelayDialer {
            endpoint,
            identity,
            workspaces,
            metrics,
        }
        .dial_loop(addr, label, priority)
        .await;
    })
}

/// What an outbound peer-relay dial carries: the local endpoint, this
/// relay's own identity (its revocation list read afresh for every dial and
/// session), the hosted workspaces and metrics.
struct PeerRelayDialer {
    endpoint: Endpoint,
    identity: LiveIdentity,
    workspaces: WorkspaceRegistry,
    metrics: Arc<Metrics>,
}

impl PeerRelayDialer {
    /// Spawn the outbound reconnect-with-backoff task for one peer
    /// relay, with a fully-resolved endpoint address. The task loops
    /// forever (until the returned join handle is aborted) — reconnect
    /// gap is `1s, 2s, 4s, 8s, …, capped at 60s`. While connected it
    /// holds a `RelayClient` session and admits inbound deltas through the
    /// same ordered persistence owner used by device peers.
    fn spawn_at(self, addr: EndpointAddr, label: String, priority: i32) -> JoinHandle<()> {
        tokio::spawn(self.dial_loop(addr, label, priority))
    }

    async fn dial_loop(self, addr: EndpointAddr, label: String, _priority: i32) {
        let PeerRelayDialer {
            endpoint,
            identity,
            workspaces,
            metrics,
        } = self;
        let mut backoff_ms: u64 = 1_000;
        loop {
            let ep = endpoint.clone();
            let current_revocations = identity.revocations.read_recover().clone();
            match RelayClient::connect(
                ep,
                addr.clone(),
                identity.user_public_key,
                identity.cert.clone(),
                identity.signing_key.clone(),
                current_revocations,
            )
            .await
            {
                Ok(client) => {
                    tracing::info!(%label, "peer relay federated");
                    backoff_ms = 1_000;
                    if let Err(e) = run_peer_relay_session(
                        &client,
                        &workspaces,
                        identity.revocations.clone(),
                        identity.user_public_key,
                        metrics.clone(),
                    )
                    .await
                    {
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
}

/// One iteration of the federated peer-relay session. Sends a
/// `PeerRelayHello` with the workspaces we host, waits for the
/// peer's `PeerRelayHello`, `Subscribe`s to the intersection, then
/// drains inbound deltas through the local ordered persistence owner and
/// routes ephemeral presence through the local workspace runtime.
struct FederationLease(Arc<std::sync::atomic::AtomicBool>);
impl Drop for FederationLease {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

async fn run_peer_relay_session(
    client: &RelayClient,
    workspaces: &WorkspaceRegistry,
    revocations: Arc<std::sync::RwLock<RevocationList>>,
    own_user: [u8; 32],
    metrics: Arc<Metrics>,
) -> Result<(), String> {
    let hosted_local = advertise_hosted_workspaces(client, workspaces).await?;
    let intersection = negotiate_intersection(client, &hosted_local).await?;
    subscribe_to_intersection(client, &intersection)?;
    let lease = FederationLease(Arc::new(std::sync::atomic::AtomicBool::new(true)));
    let (failed, mut failures) = tokio::sync::mpsc::channel(1);
    let source = crate::peer::StateSource::federated(
        client.relay_peer.clone(),
        client.outbound.clone(),
        failed,
        revocations,
        own_user,
        lease.0.clone(),
    );
    for id in &intersection {
        let workspace = workspaces.lock().await.get(id).cloned();
        if let Some(workspace) = workspace {
            let mut runtime = workspace.lock().await;
            runtime.record_peer_relay(client.relay_peer.device_public_key);
            if let Some(previous) = runtime
                .outbound_federation
                .insert(client.relay_peer.device_public_key, lease.0.clone())
            {
                previous.store(false, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }
    tokio::select! {
        result = drain_peer_relay_session(client, workspaces, &intersection, &source, &metrics) => result,
        _ = failures.recv() => Err("federated state persistence/authorization failed".into()),
    }
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
/// Live deltas use the shared ordered state owner; ephemeral presence uses
/// the workspace router. `PeerRole::PeerRelay` suppresses re-echo to peers.
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
    source: &crate::peer::StateSource,
    metrics: &Arc<Metrics>,
) -> Result<(), String> {
    let mut resync = tokio::time::interval(Duration::from_millis(500));
    resync.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            // `interval` fires its first tick immediately, so the initial
            // resync goes out as soon as we enter the loop.
            _ = resync.tick() => {
                for id in intersection {
                    send_sync_request_for(client, workspaces, *id, source).await?;
                }
            }
            maybe = client.recv() => {
                let Some(env) = maybe else { return Ok(()) }; // clean EOF
                handle_peer_relay_envelope(workspaces, client, env, source, metrics).await?;
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
    source: &crate::peer::StateSource,
    metrics: &Arc<Metrics>,
) -> Result<(), String> {
    match &env {
        Envelope::Delta { workspace_id, .. } => {
            let ws = {
                let map = workspaces.lock().await;
                map.get(workspace_id).cloned()
            };
            if let Some(ws) = ws {
                crate::peer::enqueue_federated(
                    &ws,
                    env.clone(),
                    source.clone(),
                    metrics.clone(),
                    false,
                )
                .await
                .map_err(|e| e.to_string())?;
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
                    .fanout_presence(&[0u8; 32], PeerRole::PeerRelay, &env, None);
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
                    let env = Envelope::Delta {
                        workspace_id: *workspace_id,
                        author_device_public_key: [0; 32],
                        authored_at_ns: unix_now_ns(),
                        delta: delta_since.clone(),
                        signature: [0; 64],
                    };
                    crate::peer::enqueue_federated(&ws, env, source.clone(), metrics.clone(), true)
                        .await
                        .map_err(|e| e.to_string())?;
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
    Ok(())
}

/// Advertise our local materialized heads for `ws_id` to the upstream
/// peer relay via a [`Envelope::SyncRequest`]. Best-effort: a workspace
/// we don't host, or a closed outbound, is silently skipped.
async fn send_sync_request_for(
    client: &RelayClient,
    workspaces: &WorkspaceRegistry,
    ws_id: u64,
    source: &crate::peer::StateSource,
) -> Result<(), String> {
    let Some(ws) = workspaces.lock().await.get(&ws_id).cloned() else {
        return Ok(());
    };
    let owner = crate::persistence::handle(&ws).await;
    let outbound = client.outbound.clone();
    let source = source.clone();
    owner
        .submit(
            1,
            |state| Ok(state.heads()),
            move |result| async move {
                let result = async {
                    let heads = result?;
                    let runtime = crate::workspace_io::fresh_membership(&ws).await?;
                    if !source.authorized(&runtime) {
                        return Err(crate::persistence::error("federation session changed"));
                    }
                    outbound
                        .try_send(Envelope::SyncRequest {
                            workspace_id: ws_id,
                            have_heads: heads.clone(),
                        })
                        .map_err(|_| {
                            crate::persistence::error("federation outbound backpressure")
                        })?;
                    Ok(heads)
                }
                .await;
                if result.is_err() {
                    source.failed();
                }
                result
            },
        )
        .map_err(|e| e.to_string())?;
    Ok(())
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
    use tilo_user_identity::{DeviceCapabilities, DeviceIdentity, UserIdentity, sign_device_cert};

    /// Only a trust policy that drops a root, or changes kind, narrows who
    /// may stay connected.
    #[test]
    fn only_a_policy_that_drops_a_root_narrows() {
        let list = |users: &[&str]| TrustPolicy::AllowList {
            users: users.iter().map(|user| (*user).to_owned()).collect(),
        };
        assert!(!narrows(&list(&["a"]), &list(&["a", "b"])));
        assert!(!narrows(&list(&["a", "b"]), &list(&["b", "a"])));
        assert!(narrows(&list(&["a", "b"]), &list(&["b", "c"])));
        assert!(narrows(&TrustPolicy::AllowAny, &list(&["a"])));
        assert!(!narrows(&TrustPolicy::AllowAny, &TrustPolicy::AllowAny));
    }

    fn test_handshake_trust(
        live_policy: Arc<std::sync::RwLock<TrustPolicy>>,
        own_user: [u8; 32],
    ) -> HandshakeTrust {
        HandshakeTrust {
            base_roots: Arc::new(HashSet::from([own_user])),
            authority_roots: Arc::new(std::sync::RwLock::new(HashSet::new())),
            live_policy,
            allow_any: false,
        }
    }

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
        // Phase `bundle_imports_the_shared_workspace_shard_as_full_replication`.
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

            import_home_peer_bindings(&mut config).unwrap();
            assert!(config.services.full_content_replication);
            assert!(config.workspaces.iter().any(|binding| {
                binding.id == 42
                    && binding.pin
                    && binding.replication == crate::mesh::ReplicationIntent::FullWorkspace
            }));
        }
        // Phase `room_scoped_authority_admits_only_its_signed_room`.
        {
            use collab_types::{Access, CapOverride, Effect, Scope, Subject};

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
                collab_types::FsAuthority {
                    capabilities: vec![
                        // A Files grant is intentionally insufficient to subscribe
                        // to any room, including unrelated room 43.
                        CapOverride::new(
                            Subject::User(collab_types::Ed25519PublicKey(path_only_collaborator)),
                            Scope::FsPath("/".into()),
                            Effect::Grant,
                            Access::Read,
                        ),
                    ],
                    room_members: [
                        (
                            "42".to_owned(),
                            vec![collab_types::RoomMember {
                                user: collab_types::Ed25519PublicKey(collaborator_a),
                                until: None,
                            }],
                        ),
                        (
                            "43".to_owned(),
                            vec![collab_types::RoomMember {
                                user: collab_types::Ed25519PublicKey(collaborator_a),
                                until: Some(0),
                            }],
                        ),
                    ]
                    .into_iter()
                    .collect(),
                },
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
                    workspaces: vec![crate::config::WorkspaceBinding {
                        id: 0,
                        pin: true,
                        replication: crate::mesh::ReplicationIntent::FullWorkspace,
                        admitted_users: vec![hex::encode(user.public_key)],
                        shell_authority: None,
                        checkpoint_prefix_recovery:
                            crate::config::CheckpointPrefixRecovery::Disabled,
                    }],
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
            assert!(relay.workspaces.lock().await.contains_key(&99));
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

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one live refresh task must observe both the grant and the later revocation, so setup and both waits share a single lifecycle; the expiry regression deliberately shows unchanged authority bytes before and after the wall-clock transition; the restart/rollback regression retains setup and both persisted-state assertions together to document the recovery invariant"
    )]
    async fn live_authority_refresh_revokes_expires_and_resists_rollback() {
        // Phase `live_authority_refresh_removes_the_last_room_collaborator`.
        {
            let temp = tempfile::tempdir().unwrap();
            let cache = temp.path().join("cache");
            std::fs::create_dir_all(&cache).unwrap();
            let bundle_path = temp.path().join("home-peer.toml");
            let governor = ed25519_dalek::SigningKey::from_bytes(&[0x51; 32]);
            let collaborator = [0x52; 32];
            let ticket = fs::Ticket {
                node: EndpointAddr::new(iroh::SecretKey::from_bytes(&[7; 32]).public()),
                workspace: [9; 32],
            };
            let granted = fs::workspace_authority::WorkspaceAuthorityEnvelope::sign(
                ticket.workspace_id(),
                1,
                collab_types::FsAuthority {
                    capabilities: vec![],
                    room_members: [(
                        "42".to_owned(),
                        vec![collab_types::RoomMember {
                            user: collab_types::Ed25519PublicKey(collaborator),
                            until: None,
                        }],
                    )]
                    .into_iter()
                    .collect(),
                },
                false,
                &governor,
            )
            .unwrap();
            let bundle = fs::full_replica::HomePeerSet {
                status_file: temp.path().join("status.json"),
                workspaces: vec![fs::full_replica::HomePeerBundle {
                    ticket: ticket.encode().unwrap(),
                    cache_dir: cache.clone(),
                    capability_json: "[]".into(),
                    authority: Some(granted),
                    principals: Vec::new(),
                    relay_workspace_ids: vec![42],
                }],
            };
            std::fs::write(&bundle_path, toml::to_string(&bundle).unwrap()).unwrap();
            let config = RelayConfig {
                state_root: temp.path().join("relay"),
                workspaces: Vec::new(),
                ..RelayConfig::default()
            };
            let workspaces =
                build_workspace_registry(&config, &crate::payloads::neighbour(), false).unwrap();
            get_or_autobind_workspace(
                &workspaces,
                Some(&WorkspaceAutobind {
                    state_root: config.state_root.clone(),
                    shell_payloads: crate::payloads::neighbour(),
                }),
                42,
            )
            .await
            .unwrap()
            .expect("owner autobinds the newly opened room");
            let policy = Arc::new(std::sync::RwLock::new(TrustPolicy::AllowList {
                users: Vec::new(),
            }));
            let trust = test_handshake_trust(policy, governor.verifying_key().to_bytes());
            let task = spawn_home_peer_authority_refresh(
                bundle_path,
                governor.verifying_key().to_bytes(),
                trust,
                workspaces.clone(),
            );
            let registry = &workspaces;
            let admitted = move || async move {
                registry
                    .lock()
                    .await
                    .get(&42)
                    .unwrap()
                    .lock()
                    .await
                    .admitted_users
                    .clone()
            };
            // The refresh task rereads the bundle on its own period; wait for it
            // to apply rather than sleeping for one period and hoping.
            tilo_test_env::within("the refreshed authority admitted the collaborator", async {
                while !admitted().await.contains(&collaborator) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;

            let revoked = fs::workspace_authority::WorkspaceAuthorityEnvelope::sign(
                ticket.workspace_id(),
                2,
                Vec::new(),
                false,
                &governor,
            )
            .unwrap();
            std::fs::write(
                cache.join("workspace-authority.bin"),
                postcard::to_allocvec(&revoked).unwrap(),
            )
            .unwrap();
            let only_governor = std::collections::HashSet::from([revoked.governor]);
            tilo_test_env::within(
                "the revoked authority left only its governor admitted",
                async {
                    while admitted().await != only_governor {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                },
            )
            .await;
            task.abort();
        }
        // Phase `authority_refresh_rechecks_expiry_without_a_source_change`.
        {
            let temp = tempfile::tempdir().unwrap();
            let cache = temp.path().join("cache");
            let bundle_path = temp.path().join("home-peer.toml");
            let governor = ed25519_dalek::SigningKey::from_bytes(&[0x61; 32]);
            let collaborator = [0x62; 32];
            let ticket = fs::Ticket {
                node: EndpointAddr::new(iroh::SecretKey::from_bytes(&[7; 32]).public()),
                workspace: [9; 32],
            };
            let authority = fs::workspace_authority::WorkspaceAuthorityEnvelope::sign(
                ticket.workspace_id(),
                1,
                collab_types::FsAuthority {
                    capabilities: vec![],
                    room_members: [(
                        "42".to_owned(),
                        vec![collab_types::RoomMember {
                            user: collab_types::Ed25519PublicKey(collaborator),
                            until: Some(20),
                        }],
                    )]
                    .into_iter()
                    .collect(),
                },
                false,
                &governor,
            )
            .unwrap();
            let bundle = fs::full_replica::HomePeerSet {
                status_file: temp.path().join("status.json"),
                workspaces: vec![fs::full_replica::HomePeerBundle {
                    ticket: ticket.encode().unwrap(),
                    cache_dir: cache,
                    capability_json: "[]".into(),
                    authority: Some(authority),
                    principals: Vec::new(),
                    relay_workspace_ids: vec![42],
                }],
            };
            std::fs::write(&bundle_path, toml::to_string(&bundle).unwrap()).unwrap();
            let config = RelayConfig {
                state_root: temp.path().join("relay"),
                workspaces: Vec::new(),
                ..RelayConfig::default()
            };
            let workspaces =
                build_workspace_registry(&config, &crate::payloads::neighbour(), false).unwrap();
            get_or_autobind_workspace(
                &workspaces,
                Some(&WorkspaceAutobind {
                    state_root: config.state_root.clone(),
                    shell_payloads: crate::payloads::neighbour(),
                }),
                42,
            )
            .await
            .unwrap();
            let policy = Arc::new(std::sync::RwLock::new(TrustPolicy::AllowList {
                users: Vec::new(),
            }));
            let trust = test_handshake_trust(policy, governor.verifying_key().to_bytes());
            let mut state = AuthorityRefreshState::default();

            assert!(
                refresh_home_peer_authority_once(
                    &bundle_path,
                    governor.verifying_key().to_bytes(),
                    &trust,
                    &workspaces,
                    &mut state,
                    19,
                )
                .await
            );
            assert!(
                workspaces
                    .lock()
                    .await
                    .get(&42)
                    .unwrap()
                    .lock()
                    .await
                    .admitted_users
                    .contains(&collaborator)
            );

            assert!(
                refresh_home_peer_authority_once(
                    &bundle_path,
                    governor.verifying_key().to_bytes(),
                    &trust,
                    &workspaces,
                    &mut state,
                    20,
                )
                .await
            );
            assert!(
                !workspaces
                    .lock()
                    .await
                    .get(&42)
                    .unwrap()
                    .lock()
                    .await
                    .admitted_users
                    .contains(&collaborator)
            );
        }
        // Phase `authority_high_water_rejects_rollback_and_missing_cache_after_restart`.
        {
            let temp = tempfile::tempdir().unwrap();
            let cache = temp.path().join("cache");
            std::fs::create_dir_all(&cache).unwrap();
            let bundle_path = temp.path().join("home-peer.toml");
            let governor = ed25519_dalek::SigningKey::from_bytes(&[0x71; 32]);
            let other_governor = ed25519_dalek::SigningKey::from_bytes(&[0x72; 32]);
            let collaborator = [0x73; 32];
            let ticket = fs::Ticket {
                node: EndpointAddr::new(iroh::SecretKey::from_bytes(&[7; 32]).public()),
                workspace: [9; 32],
            };
            let granted = fs::workspace_authority::WorkspaceAuthorityEnvelope::sign(
                ticket.workspace_id(),
                1,
                collab_types::FsAuthority {
                    capabilities: vec![],
                    room_members: [(
                        "42".to_owned(),
                        vec![collab_types::RoomMember {
                            user: collab_types::Ed25519PublicKey(collaborator),
                            until: None,
                        }],
                    )]
                    .into_iter()
                    .collect(),
                },
                false,
                &governor,
            )
            .unwrap();
            let tombstone = fs::workspace_authority::WorkspaceAuthorityEnvelope::sign(
                ticket.workspace_id(),
                2,
                Vec::new(),
                true,
                &governor,
            )
            .unwrap();
            let bundle = fs::full_replica::HomePeerSet {
                status_file: temp.path().join("status.json"),
                workspaces: vec![fs::full_replica::HomePeerBundle {
                    ticket: ticket.encode().unwrap(),
                    cache_dir: cache.clone(),
                    capability_json: "[]".into(),
                    authority: Some(granted.clone()),
                    principals: Vec::new(),
                    relay_workspace_ids: vec![42],
                }],
            };
            std::fs::write(&bundle_path, toml::to_string(&bundle).unwrap()).unwrap();
            std::fs::write(
                cache.join("workspace-authority.bin"),
                postcard::to_allocvec(&tombstone).unwrap(),
            )
            .unwrap();
            let config = RelayConfig {
                state_root: temp.path().join("relay"),
                workspaces: Vec::new(),
                ..RelayConfig::default()
            };
            let workspaces =
                build_workspace_registry(&config, &crate::payloads::neighbour(), false).unwrap();
            get_or_autobind_workspace(
                &workspaces,
                Some(&WorkspaceAutobind {
                    state_root: config.state_root.clone(),
                    shell_payloads: crate::payloads::neighbour(),
                }),
                42,
            )
            .await
            .unwrap();
            let policy = Arc::new(std::sync::RwLock::new(TrustPolicy::AllowList {
                users: Vec::new(),
            }));
            let own_user = governor.verifying_key().to_bytes();
            let trust = test_handshake_trust(policy.clone(), own_user);
            let mut first_process = AuthorityRefreshState::default();
            assert!(
                refresh_home_peer_authority_once(
                    &bundle_path,
                    own_user,
                    &trust,
                    &workspaces,
                    &mut first_process,
                    1,
                )
                .await
            );
            assert_eq!(
                workspaces
                    .lock()
                    .await
                    .get(&42)
                    .unwrap()
                    .lock()
                    .await
                    .admitted_users,
                std::collections::HashSet::new()
            );

            // A validly signed pre-tombstone authority must not resurrect the
            // collaborator.  A new state models a relay process restart.
            std::fs::write(
                cache.join("workspace-authority.bin"),
                postcard::to_allocvec(&granted).unwrap(),
            )
            .unwrap();
            let mut restarted = AuthorityRefreshState::default();
            assert!(
                !refresh_home_peer_authority_once(
                    &bundle_path,
                    own_user,
                    &trust,
                    &workspaces,
                    &mut restarted,
                    1,
                )
                .await
            );
            assert_eq!(
                policy.read().unwrap().clone(),
                TrustPolicy::AllowList {
                    users: vec![hex::encode(own_user)]
                }
            );

            // Deleting the live cache would otherwise fall back to `granted` in
            // the enrollment bundle.  The durable high-water mark rejects it.
            std::fs::remove_file(cache.join("workspace-authority.bin")).unwrap();
            let mut restarted_after_deletion = AuthorityRefreshState::default();
            assert!(
                !refresh_home_peer_authority_once(
                    &bundle_path,
                    own_user,
                    &trust,
                    &workspaces,
                    &mut restarted_after_deletion,
                    1,
                )
                .await
            );

            // A governor switch cannot bypass the high-water mark either.
            let foreign = fs::workspace_authority::WorkspaceAuthorityEnvelope::sign(
                ticket.workspace_id(),
                3,
                Vec::new(),
                false,
                &other_governor,
            )
            .unwrap();
            std::fs::write(
                cache.join("workspace-authority.bin"),
                postcard::to_allocvec(&foreign).unwrap(),
            )
            .unwrap();
            let mut restarted_after_governor_change = AuthorityRefreshState::default();
            assert!(
                !refresh_home_peer_authority_once(
                    &bundle_path,
                    own_user,
                    &trust,
                    &workspaces,
                    &mut restarted_after_governor_change,
                    1,
                )
                .await
            );
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

    #[tokio::test]
    async fn dynamic_handshake_refresh_preserves_operator_root_pins() {
        let own_user = [1; 32];
        let operator_pin = [2; 32];
        let authority_root = [3; 32];
        let trust = HandshakeTrust::new(
            &TrustPolicy::AllowList {
                users: vec![hex::encode(operator_pin)],
            },
            &TrustPolicy::AllowList {
                users: vec![hex::encode(operator_pin), hex::encode(authority_root)],
            },
            own_user,
        );
        let workspaces = Arc::new(AsyncMutex::new(HashMap::new()));

        trust.replace_authority_roots(HashSet::new());
        trust.refresh(&workspaces).await;

        assert_eq!(
            trust.live_policy.read_recover().clone(),
            TrustPolicy::AllowList {
                users: vec![hex::encode(own_user), hex::encode(operator_pin)]
            }
        );
    }
}
