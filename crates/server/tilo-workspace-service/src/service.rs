//! The workspaces one device serves: their governance and the session stack their members
//! connect through, embeddable in any process that owns an iroh endpoint.
//!
//! A device runs the same stack whether it has a screen or not; [`ServiceConfig::work`] is what
//! it takes from others. [`WorkspaceService::open`] spawns nothing periodic: with no peers
//! connected the service wakes only on I/O, and a connected peer's session is audited when its
//! workspaces' membership files change, which the service watches. Its embedder registers
//! [`WorkspaceService::protocols`] on its own router, or, when the router is built before any
//! workspace is placed here, [`deferred_protocols`], which refuse connections until
//! [`ServiceSlot::install`] gives them one.
//!
//! A member in the same process connects through [`WorkspaceService::connect_local`]: the same
//! handshake and session as a remote member, over an in-process stream.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use iroh::endpoint::{Connection, Endpoint};
use iroh::protocol::{AcceptError, DynProtocolHandler, ProtocolHandler};
use tilo_clock::unix_now_ns;
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_user_identity::{DeviceCert, RevocationList, verify_device_cert_at};

use tilo_endpoint_reachability::Reachability;
use tokio::sync::Mutex as AsyncMutex;

use crate::Reaching;
use crate::app_serving::AppActionServer;
use crate::client::{ClientError, ServerClient};
use crate::handshake::{EndpointTrust, HandshakeError, LiveIdentity};
use crate::metrics::Metrics;
use crate::peer::{PeerContext, WorkspaceRegistry, run_peer_until};
use crate::policy::TrustPolicy;
use crate::transport::{ALPN, MAX_FRAME_BYTES};
use tilo_server_protocol::{WorkSetting, WorkspaceId};

/// How much an in-process session buffers each way before its writer waits.
const LOCAL_SESSION_BUFFER: usize = 1 << 20;

/// What a service is opened with.
#[derive(Clone)]
pub struct ServiceConfig {
    /// Where the service keeps its workspaces and their retained state.
    pub state_root: PathBuf,
    /// The work this device takes from others. A device of its person hands it a workspace to
    /// serve only when the setting takes that person's work.
    pub work: WorkSetting,
    /// The name this keeper goes by among the holders of the workspaces it serves: its
    /// server's name from its person's admin workspace, or its site's name for a hosting
    /// service. Empty for its device's own label.
    pub label: String,
    /// Whether the machine under this keeper is rented from a cloud provider, whose operator
    /// can read what the machine holds. A keeper declares it in the address it signs, so every
    /// member reads what can read their workspace.
    pub rented: bool,
    /// Who the handshake admits besides the person's own devices and the
    /// workspaces' members.
    pub trust_policy: TrustPolicy,
    /// Pre-admissions each workspace keeps, spent ones included.
    pub max_workspace_grants: Option<usize>,
    /// Answers remote actions addressed to this service beyond its own
    /// governance (`tilo.host`, `tilo.workspace`).
    pub apps: Option<Arc<dyn AppActionServer>>,
    /// Classifies co-edit bodies for every workspace the service opens.
    pub shell_payloads: Arc<dyn crate::payloads::ShellPayloads>,
    /// Whether the endpoint a server serves its workspaces on names where it is
    /// reached. Opening it waits for that name so no workspace learns an address
    /// it would have to rename; an endpoint that will never name one says so
    /// here, rather than making every open wait for it.
    pub reaching: Reaching,
    /// The relays this embedder holds back: a holder wakes them before it signs an
    /// invitation's address, so the link it hands out names where it is reached from any
    /// network. The default holds nothing back, so an invitation names the endpoint's address.
    pub reachability: Reachability,
    /// The device this service's member copies act as, when the embedder
    /// keeps it on a device that serves the workspaces that device joined as
    /// a member: `None` for a server, which joins none.
    pub member: Option<Arc<crate::workspaces::MemberDevice>>,
    /// The app surfaces the device knows, which the stores of its workspaces declare the
    /// resource types and verbs of: the process's own ([`collab_types::app_surface::Surfaces::process`]),
    /// or, for a test that runs several devices in one process, each device's own.
    pub surfaces: Arc<collab_types::app_surface::Surfaces>,
}

impl ServiceConfig {
    /// A service keeping its state under `state_root`, admitting the person's
    /// own devices and its workspaces' members, with the desktop's payloads.
    #[must_use]
    pub fn new(state_root: PathBuf, work: WorkSetting) -> Self {
        Self {
            state_root,
            work,
            label: String::new(),
            rented: false,
            trust_policy: TrustPolicy::AllowMatchingUser,
            max_workspace_grants: None,
            apps: None,
            shell_payloads: crate::payloads::neighbour(),
            reaching: Reaching::ByRelay,
            reachability: Reachability::default(),
            member: None,
            surfaces: Arc::clone(collab_types::app_surface::Surfaces::process()),
        }
    }
}

/// Why a service could not open or change.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// The identity is invalid.
    #[error("identity: {0}")]
    Identity(String),
    /// On-disk persistence.
    #[error("files: {0}")]
    Files(#[from] crate::workspace_files::FilesError),
    /// Filesystem I/O.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Governed workspace admission could not be reconciled safely.
    #[error("governed access: {0}")]
    GovernedAccess(String),
    /// The workspaces could not be opened.
    #[error("governed workspaces: {0}")]
    Workspaces(String),
    /// A member's handshake failed.
    #[error("handshake: {0}")]
    Handshake(String),
}

/// ALPN handlers to register on a router, each with its ALPN.
pub type Protocols = Vec<(Vec<u8>, Box<dyn DynProtocolHandler>)>;

/// The ALPNs a workspace service answers on.
const SERVICE_ALPNS: [&[u8]; 4] = [
    ALPN,
    tilo_delivery_protocol::workspace_join::WORKSPACE_JOIN_ALPN,
    tilo_peer_state::actions::ACTIONS_ALPN,
    tilo_peer_state::direct::DIRECT_ALPN,
];

/// A service's session stack: its workspaces, the handshake that admits their
/// members, and the session every member runs, whatever its carrier.
#[derive(Clone)]
pub struct ServiceSessions {
    pub(crate) endpoint: Endpoint,
    pub workspaces: WorkspaceRegistry,
    pub metrics: Arc<Metrics>,
    pub identity: LiveIdentity,
    pub(crate) handshake_trust: HandshakeTrust,
    pub state_root: PathBuf,
    /// The server ALPN's handler, and every session's context.
    pub handler: ServerProtocol,
    /// Ends the in-process sessions when the service shuts down.
    closing: Arc<tokio::sync::watch::Sender<bool>>,
    /// The watch that lets its workspaces' membership be read from memory until
    /// a file changes, so a live session is audited when something changed
    /// rather than every second.
    membership_watch: Arc<crate::membership_watch::WatchTask>,
}

/// An app action server that answers nothing: what a service opened only for
/// the member copies a device holds serves.
struct NoActions;

impl AppActionServer for NoActions {
    fn serve(
        &self,
        _workspace_id: WorkspaceId,
        _caller: &tilo_actions::remote::Caller,
        _message: tilo_actions::remote::RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>> {
        None
    }
}

/// The app action server such a service answers with.
fn no_actions() -> Arc<dyn AppActionServer> {
    Arc::new(NoActions)
}

/// A service's workspaces, when it governs them: the governance and the app action server
/// answering its actions.
pub struct Governed {
    pub workspaces: crate::workspaces::Workspaces,
    pub(crate) actions: Arc<dyn AppActionServer>,
}

/// The workspaces a service serves, by id, as the sessions it opens share them.
fn empty_registry() -> WorkspaceRegistry {
    Arc::new(AsyncMutex::new(std::collections::HashMap::new()))
}

impl ServiceSessions {
    /// Open a service's sessions and, when `governs`, its workspaces. `wrap` gives
    /// the app action server every session answers remote actions with, from the
    /// workspaces and the service's own app action server (its governance and apps): a
    /// server answers compute placement around it.
    ///
    /// # Errors
    ///
    /// The identity is invalid or, for a certified service, not able to serve
    /// or to govern the workspaces it was asked to govern; the state root
    /// cannot be created; or the workspaces cannot be opened.
    pub async fn open(
        cfg: ServiceConfig,
        identity: ServiceIdentity,
        endpoint: Endpoint,
        governs: bool,
        channels: Option<crate::workspaces::DeviceKeyGroups>,
        wrap: impl FnOnce(
            &WorkspaceRegistry,
            Option<Arc<dyn AppActionServer>>,
        ) -> Option<Arc<dyn AppActionServer>>,
    ) -> Result<(Self, Option<Governed>), ServiceError> {
        validate_service_device_identity(&identity)?;
        may_govern(governs, &identity.device_cert)?;
        std::fs::create_dir_all(&cfg.state_root)?;
        let workspaces = empty_registry();
        let metrics = Arc::new(Metrics::default());
        let revocations = Arc::new(std::sync::RwLock::new(identity.revocations.clone()));
        let live = LiveIdentity {
            user_public_key: identity.user_public_key,
            cert: identity.device_cert.clone(),
            signing_key: identity.device_signing_key.clone(),
            revocations: revocations.clone(),
        };
        let handshake_trust = HandshakeTrust::new(&cfg.trust_policy, identity.user_public_key);
        let governs = governs && !handshake_trust.allow_any;
        handshake_trust.refresh(&workspaces).await;
        // The service answers its workspaces' governance as actions itself, beside
        // whatever apps its embedder serves.
        let governance = governs.then(crate::workspaces::ServiceWorkspaceActions::new);
        let own_server = match &governance {
            Some(governance) => Some(Arc::new(crate::workspaces::GovernedServer {
                governance: governance.clone(),
                apps: cfg.apps.clone(),
            }) as Arc<dyn AppActionServer>),
            None => cfg.apps.clone(),
        };
        let handler = ServerProtocol {
            workspaces: workspaces.clone(),
            trust_policy: handshake_trust.live_policy.clone(),
            identity: live.clone(),
            metrics: metrics.clone(),
            app_serving: wrap(&workspaces, own_server.clone()),
            admin: Arc::default(),
            fetch: Arc::default(),
            agents: Arc::default(),
            environments: crate::environment::EnvironmentRegistry::at(
                cfg.state_root.join("environments.json"),
            ),
            holds_for_others: cfg.work.from.takes_others(),
        };
        let sessions = Self {
            endpoint: endpoint.clone(),
            workspaces: workspaces.clone(),
            metrics: metrics.clone(),
            identity: live,
            handshake_trust,
            state_root: cfg.state_root.clone(),
            handler,
            closing: Arc::new(tokio::sync::watch::channel(false).0),
            membership_watch: Arc::new(crate::membership_watch::WatchTask::spawn(&cfg.state_root)),
        };
        // A service that governs nothing still holds the member copies of a
        // device that joined workspaces another device serves; with neither,
        // there is nothing to open.
        if governance.is_none() && cfg.member.is_none() {
            return Ok((sessions, None));
        }
        // No workspace this server keeps is opened with an address it would
        // have to rename: the endpoint names where it is reached first, when it
        // will at all.
        cfg.reaching.awaited(&endpoint).await;
        let workspaces = Box::pin(crate::workspaces::Workspaces::open(
            sessions.workspace_services(&cfg, &identity, endpoint, revocations, channels),
            cfg.state_root.join("workspaces"),
            cfg.state_root.join("store"),
        ))
        .await
        .map_err(ServiceError::Workspaces)?;
        if let Some(governance) = governance {
            governance.attach(workspaces.clone());
            // Sessions ask its governance what a device may do; the lock is set
            // before the endpoint routes any session to a service that governs here.
            let _ = sessions.handler.admin.set(Arc::new(workspaces.clone()));
            #[cfg(all(
                not(target_arch = "wasm32"),
                any(feature = "agents", feature = "device-keys")
            ))]
            {
                let carrying = workspaces.clone();
                tokio::spawn(async move { carrying.carry_vaults_over().await });
            }
        }
        Ok((
            sessions,
            Some(Governed {
                workspaces,
                actions: own_server.unwrap_or_else(no_actions),
            }),
        ))
    }

    /// What the workspaces a service governs take from it: its sessions' registry,
    /// access, records, identity and live trust, and `cfg`'s workspace settings.
    fn workspace_services(
        &self,
        cfg: &ServiceConfig,
        identity: &ServiceIdentity,
        endpoint: Endpoint,
        revocations: Arc<std::sync::RwLock<RevocationList>>,
        channels: Option<crate::workspaces::DeviceKeyGroups>,
    ) -> crate::workspaces::WorkspaceServices {
        crate::workspaces::WorkspaceServices {
            channels,
            workspaces: self.workspaces.clone(),
            access: self.access_writer(),
            endpoint,
            cert: identity.device_cert.clone(),
            key: identity.device_signing_key.clone(),
            revocations,
            max_pre_admissions: cfg
                .max_workspace_grants
                .unwrap_or(crate::workspaces::DEFAULT_MAX_PRE_ADMISSIONS),
            trust_policy: self.handler.trust_policy.clone(),
            agents: self.handler.agents.clone(),
            environments: self.handler.environments.clone(),
            work: cfg.work.clone(),
            reachability: cfg.reachability.clone(),
            holder: Arc::new(std::sync::RwLock::new(crate::workspaces::HolderClaim {
                label: cfg.label.clone(),
                cloud: cfg.rented,
                moderates: std::collections::BTreeSet::new(),
            })),
            member: cfg.member.clone(),
            surfaces: Arc::clone(&cfg.surfaces),
        }
    }

    /// The writer of the service's governed workspace access.
    pub(crate) fn access_writer(&self) -> GovernedAccessWriter {
        GovernedAccessWriter {
            workspaces: self.workspaces.clone(),
            handshake_trust: self.handshake_trust.clone(),
            state_root: self.state_root.clone(),
            environments: self.handler.environments.clone(),
        }
    }

    /// Whether the service counts `device` as subscribed to `workspace_id`.
    pub async fn is_subscribed(&self, workspace_id: WorkspaceId, device: &[u8; 32]) -> bool {
        let ws = self.workspaces.lock().await.get(&workspace_id).cloned();
        match ws {
            Some(ws) => ws.lock().await.has_subscriber(device),
            None => false,
        }
    }

    /// A member session of this device's own person to this service, over an
    /// in-process stream: the handshake and the session a remote member
    /// runs. It lasts until either side drops it or the service shuts down.
    ///
    /// # Errors
    ///
    /// As [`ServerClient::connect_v2_over_stream`]: the service refused the
    /// device, or the handshake failed.
    pub(crate) async fn connect_local(
        &self,
        cert: DeviceCert,
        key: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<ServerClient, ClientError> {
        let (client, server) = tokio::io::duplex(LOCAL_SESSION_BUFFER);
        let (server_read, server_write) = tokio::io::split(server);
        let handler = self.handler.clone();
        let mut closing = self.closing.subscribe();
        tokio::spawn(async move {
            let closed = async move {
                // The sender lives as long as the service; its end is one too.
                let _ = closing.wait_for(|closing| *closing).await;
            };
            if let Err(error) =
                serve_session(handler, server_read, server_write, None, closed).await
            {
                tracing::debug!(%error, "in-process session ended");
            }
        });
        let (client_read, client_write) = tokio::io::split(client);
        ServerClient::connect_v2_over_stream(
            client_read,
            client_write,
            self.identity.cert.device_public_key,
            cert,
            key,
            revocations,
        )
        .await
    }

    /// End every in-process session.
    pub fn shutdown(&self) {
        self.closing.send_replace(true);
        self.membership_watch.stop();
    }
}

/// Whether a service asked to govern under `cert` may: governing a workspace is holding its
/// keys, which its documents are sealed under, so a device enrolled without that capability
/// serves its person's sessions but keeps no workspace.
fn may_govern(governs: bool, cert: &DeviceCert) -> Result<(), ServiceError> {
    if governs && !cert.capabilities.holds_workspace_keys() {
        return Err(ServiceError::Identity(
            "this service's certificate cannot own workspaces, so it cannot govern \
             them: re-enroll it with the own capability"
                .into(),
        ));
    }
    Ok(())
}

/// The handlers of a governing service's ALPNs, shared between the routers
/// that register them.
fn service_handlers(
    sessions: &ServiceSessions,
    workspaces: &crate::workspaces::Workspaces,
) -> Vec<(&'static [u8], Arc<dyn DynProtocolHandler>)> {
    vec![
        (
            ALPN,
            Arc::new(sessions.handler.clone()) as Arc<dyn DynProtocolHandler>,
        ),
        (
            tilo_delivery_protocol::workspace_join::WORKSPACE_JOIN_ALPN,
            Arc::new(tilo_peer_state::join_transport::WorkspaceJoinProtocol::new(
                workspaces.join_service(),
            )),
        ),
        // The service's own device answers a workspace's member devices here, as
        // the members answer each other: an offer is answered by the service as
        // one of the workspace's member devices
        // (`crate::workspaces::device_actions`).
        (
            tilo_peer_state::actions::ACTIONS_ALPN,
            Arc::new(workspaces.action_service()) as Arc<dyn DynProtocolHandler>,
        ),
        // A member device of a workspace syncs with the service here, the same
        // link a device runs for itself: the dial names its workspace first.
        (
            tilo_peer_state::direct::DIRECT_ALPN,
            Arc::new(workspaces.direct()) as Arc<dyn DynProtocolHandler>,
        ),
    ]
}

/// The workspaces' own ALPNs on a server's router, beside its session ALPN.
pub fn workspaces_protocols(
    sessions: &ServiceSessions,
    workspaces: &crate::workspaces::Workspaces,
) -> Vec<(&'static [u8], Box<dyn DynProtocolHandler>)> {
    service_handlers(sessions, workspaces)
        .into_iter()
        .filter(|(alpn, _)| *alpn != ALPN)
        .map(|(alpn, handler)| {
            (
                alpn,
                Box::new(Shared(handler)) as Box<dyn DynProtocolHandler>,
            )
        })
        .collect()
}

/// A handler several routers share.
#[derive(Debug)]
struct Shared(Arc<dyn DynProtocolHandler>);

impl ProtocolHandler for Shared {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        self.0.accept(connection).await
    }

    async fn shutdown(&self) {
        self.0.shutdown().await;
    }
}

/// The governance of the workspaces placed on this device and the sessions of
/// their members.
pub struct WorkspaceService {
    sessions: ServiceSessions,
    workspaces: crate::workspaces::Workspaces,
    actions: Arc<dyn AppActionServer>,
    handlers: Vec<(&'static [u8], Arc<dyn DynProtocolHandler>)>,
    /// The thread the service makes its writes on, once it made one.
    writes: std::sync::OnceLock<Option<WritesThread>>,
}

/// A thread of the service's own that makes its writes: a write holds its workspace's lock while it
/// waits (for the key service, for the disk), and the threads that asked for it, a broker's loop or
/// a runtime's workers, may block on that lock to answer a read. A write on a thread nothing else
/// uses finishes whatever those threads wait for.
struct WritesThread {
    runtime: tokio::runtime::Handle,
    /// Dropped with the service, which ends the thread.
    _stop: tokio::sync::oneshot::Sender<()>,
}

impl WritesThread {
    fn start() -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let handle = runtime.handle().clone();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        std::thread::Builder::new()
            .name("tilo-ws-writes".into())
            .spawn(move || {
                runtime.block_on(async {
                    let _ = stopped.await;
                });
            })?;
        Ok(Self {
            runtime: handle,
            _stop: stop,
        })
    }
}

impl std::fmt::Debug for WorkspaceService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceService")
            .field(
                "device",
                &hex::encode(self.sessions.identity.cert.device_public_key),
            )
            .finish_non_exhaustive()
    }
}

impl WorkspaceService {
    /// Open the service on `endpoint`, which the embedder owns and routes.
    /// Spawns nothing periodic: with no peers connected it wakes only on
    /// I/O.
    ///
    /// # Errors
    ///
    /// The identity is invalid; `cfg` admits anyone, under which no workspace is governed;
    /// the state root cannot be created; or the workspaces kept there cannot be
    /// opened.
    pub async fn open(
        cfg: ServiceConfig,
        identity: ServiceIdentity,
        endpoint: Endpoint,
        channels: Option<crate::workspaces::DeviceKeyGroups>,
    ) -> Result<Self, ServiceError> {
        if matches!(cfg.trust_policy, TrustPolicy::AllowAny) {
            return Err(ServiceError::Identity(
                "a service admitting anyone governs no workspaces".into(),
            ));
        }
        let (sessions, governed) = Box::pin(ServiceSessions::open(
            cfg,
            identity,
            endpoint,
            true,
            channels,
            |_, server| server,
        ))
        .await?;
        let Some(Governed {
            workspaces,
            actions,
        }) = governed
        else {
            return Err(ServiceError::Workspaces(
                "the service governs no workspaces".into(),
            ));
        };
        let handlers = service_handlers(&sessions, &workspaces);
        Ok(Self {
            sessions,
            workspaces,
            actions,
            handlers,
            writes: std::sync::OnceLock::new(),
        })
    }

    /// Open the service on `endpoint` without governing the workspaces kept
    /// there: the embedder serves them itself, and the service holds only the
    /// member copies the device joined. Its sessions answer the join protocol
    /// and the direct link, which member copies sync over.
    ///
    /// # Errors
    ///
    /// The identity is invalid or not able to serve; the state root cannot be
    /// created; or the workspaces kept there cannot be opened.
    pub async fn open_joining(
        cfg: ServiceConfig,
        identity: ServiceIdentity,
        endpoint: Endpoint,
        channels: Option<crate::workspaces::DeviceKeyGroups>,
    ) -> Result<Self, ServiceError> {
        if matches!(cfg.trust_policy, TrustPolicy::AllowAny) {
            return Err(ServiceError::Identity(
                "a service admitting anyone governs no workspaces".into(),
            ));
        }
        let (sessions, governed) = Box::pin(ServiceSessions::open(
            cfg,
            identity,
            endpoint,
            false,
            channels,
            |_, server| server,
        ))
        .await?;
        let Some(Governed {
            workspaces,
            actions,
        }) = governed
        else {
            return Err(ServiceError::Workspaces(
                "the service holds no member copies".into(),
            ));
        };
        let handlers = service_handlers(&sessions, &workspaces);
        Ok(Self {
            sessions,
            workspaces,
            actions,
            handlers,
            writes: std::sync::OnceLock::new(),
        })
    }

    /// The runtime the service makes its writes on ([`WritesThread`]), started with the first;
    /// `None` when no thread could start, and the caller makes the write itself.
    #[must_use]
    pub fn writes_runtime(&self) -> Option<tokio::runtime::Handle> {
        self.writes
            .get_or_init(|| {
                WritesThread::start()
                    .inspect_err(|error| {
                        tracing::warn!(%error, "the workspace service's writes thread did not start");
                    })
                    .ok()
            })
            .as_ref()
            .map(|writes| writes.runtime.clone())
    }

    /// The ALPN handlers to register on the embedder's router: the session,
    /// workspace join, workspace administration and private protocols.
    #[must_use]
    pub fn protocols(&self) -> Protocols {
        self.handlers
            .iter()
            .map(|(alpn, handler)| {
                (
                    alpn.to_vec(),
                    Box::new(Shared(Arc::clone(handler))) as Box<dyn DynProtocolHandler>,
                )
            })
            .collect()
    }

    fn handler(&self, alpn: &[u8]) -> Option<&Arc<dyn DynProtocolHandler>> {
        self.handlers
            .iter()
            .find(|(own, _)| *own == alpn)
            .map(|(_, handler)| handler)
    }

    /// The governance of the workspaces served here.
    #[must_use]
    pub const fn workspaces(&self) -> &crate::workspaces::Workspaces {
        &self.workspaces
    }

    /// The app action server answering the service's own actions (`tilo.host`:
    /// creating, listing and entering workspaces; `tilo.workspace`: each workspace's
    /// governance) and its embedder's apps, for in-process callers. A
    /// caller is named by [`local_caller`] and its certificate.
    #[must_use]
    pub fn actions(&self) -> Arc<dyn AppActionServer> {
        Arc::clone(&self.actions)
    }

    /// The workspace join service of the workspaces served here, for a joining
    /// device in the same process
    /// ([`WorkspaceJoinClient::local`](tilo_peer_state::join_transport::WorkspaceJoinClient::local)).
    #[must_use]
    pub fn join_service(&self) -> Arc<dyn tilo_peer_state::join_transport::LocalJoinService> {
        Arc::new(self.workspaces.join_service())
    }

    /// An in-process member session of this device's own person: the same
    /// handshake and session as a remote member, with its private action
    /// channel, over an in-process stream.
    ///
    /// # Errors
    ///
    /// The service refused `cert`, or the handshake failed.
    pub async fn connect_local(
        &self,
        cert: DeviceCert,
        key: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<ServerClient, ClientError> {
        self.sessions.connect_local(cert, key, revocations).await
    }

    /// This service's endpoint address: what a member dials.
    #[must_use]
    pub fn node_addr(&self) -> iroh_base::EndpointAddr {
        self.sessions.endpoint.addr()
    }

    /// The kept records of the workspaces served here, for a service the
    /// embedder runs beside them.
    #[must_use]
    pub fn workspace_documents(&self) -> crate::workspace_documents::WorkspaceDocuments {
        self.workspaces.documents()
    }

    /// The resources of the workspaces this service holds, as the broker answers an app's Open of
    /// one and the session's own reads of one.
    #[cfg(feature = "apps")]
    #[must_use]
    pub fn document_reads(&self) -> Arc<dyn crate::apps::services::NativeDocumentReads> {
        Arc::new(crate::apps::service_reads::ServiceReads::new(
            self.workspaces.clone(),
        ))
    }

    /// The certificate this service proves itself with.
    #[must_use]
    pub fn device_cert(&self) -> &DeviceCert {
        &self.sessions.identity.cert
    }

    /// Whether this service counts `device` as subscribed to `workspace_id`.
    pub async fn is_subscribed(&self, workspace_id: WorkspaceId, device: &[u8; 32]) -> bool {
        self.sessions.is_subscribed(workspace_id, device).await
    }

    /// Every counter of this service's sessions now.
    #[must_use]
    pub fn metrics(&self) -> crate::metrics::MetricsSnapshot {
        self.sessions.metrics.snapshot()
    }

    /// End the in-process sessions and stop the service's work. The embedder
    /// closes its own router and endpoint.
    ///
    /// What the workspaces took in of their members' writes is written
    /// through to their files first: that write-through is deferred, so a
    /// service stopped without this would end with a record a member wrote only
    /// in memory. A start replays what the server retains, but it takes a
    /// write only into a record its governor already holds
    /// ([`crate::workspaces::Workspaces::keep_what_was_followed`]).
    pub async fn shutdown(&self) {
        self.workspaces.keep_what_was_followed().await;
        self.sessions.shutdown();
    }
}

/// The caller an in-process device is to the service's [`WorkspaceService::actions`].
#[must_use]
pub fn local_caller(cert: &DeviceCert) -> tilo_actions::remote::Caller {
    tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key)
}

/// Where an embedder whose router is built before any workspace is placed on it
/// installs the service its [`deferred_protocols`] forward to.
#[derive(Clone, Default)]
pub struct ServiceSlot(Arc<Slot>);

/// What a [`ServiceSlot`] holds: the service, and a count of its installs that a waiter for one
/// follows.
struct Slot {
    service: std::sync::RwLock<Option<Arc<WorkspaceService>>>,
    installs: tokio::sync::watch::Sender<u64>,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            service: std::sync::RwLock::default(),
            installs: tokio::sync::watch::Sender::new(0),
        }
    }
}

impl std::fmt::Debug for ServiceSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ServiceSlot")
            .field(&self.0.service.read_recover().is_some())
            .finish()
    }
}

impl ServiceSlot {
    /// Forward this slot's connections to `service` from now on.
    pub fn install(&self, service: Arc<WorkspaceService>) {
        *self.0.service.write_recover() = Some(service);
        self.0
            .installs
            .send_modify(|installs| *installs = installs.wrapping_add(1));
    }

    /// Refuse this slot's connections again, handing back the service it
    /// forwarded to.
    pub fn clear(&self) -> Option<Arc<WorkspaceService>> {
        self.0.service.write_recover().take()
    }

    /// The service this slot forwards to.
    #[must_use]
    pub fn service(&self) -> Option<Arc<WorkspaceService>> {
        self.0.service.read_recover().clone()
    }

    /// The service this slot forwards to, once one is installed: what starts work on a device's
    /// service that its embedder opens later than it builds what the work needs.
    pub async fn installed(&self) -> Arc<WorkspaceService> {
        let mut installs = self.0.installs.subscribe();
        loop {
            if let Some(service) = self.service() {
                return service;
            }
            // The sender lives as long as this slot, which this call borrows.
            let _ = installs.changed().await;
        }
    }
}

/// Handlers for every ALPN a workspace service answers on, for a router built
/// before any service opens: each refuses connections until its slot holds
/// one ([`ServiceSlot::install`]), then forwards to it. Before then they cost
/// nothing.
#[must_use]
pub fn deferred_protocols() -> (ServiceSlot, Protocols) {
    let service = ServiceSlot::default();
    (service.clone(), deferred_protocols_in(&service))
}

/// [`deferred_protocols`] for a [`ServiceSlot`] the embedder already holds: what it answers once
/// a service is installed there ([`ServiceSlot::install`]).
#[must_use]
pub fn deferred_protocols_in(service: &ServiceSlot) -> Protocols {
    SERVICE_ALPNS
        .iter()
        .map(|alpn| {
            (
                alpn.to_vec(),
                Box::new(Deferred {
                    service: service.clone(),
                    alpn,
                }) as Box<dyn DynProtocolHandler>,
            )
        })
        .collect()
}

/// One ALPN of a [`ServiceSlot`].
#[derive(Debug)]
struct Deferred {
    service: ServiceSlot,
    alpn: &'static [u8],
}

impl ProtocolHandler for Deferred {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let Some(service) = self.service.service() else {
            return Err(AcceptError::from_err(std::io::Error::other(
                "no workspace is served here",
            )));
        };
        let Some(handler) = service.handler(self.alpn).cloned() else {
            return Err(AcceptError::from_err(std::io::Error::other(
                "the service does not answer this protocol",
            )));
        };
        handler.accept(connection).await
    }
}

/// Replaces a governed workspace's admitted members and the handshake
/// allowlist derived from them. Each workspace's governance hands it one
/// replacement at a time; replacements of different workspaces run side by
/// side, so no workspace's access write waits for another's.
#[derive(Clone)]
pub(crate) struct GovernedAccessWriter {
    workspaces: WorkspaceRegistry,
    handshake_trust: HandshakeTrust,
    state_root: PathBuf,
    /// The browser environments this server placed: their devices are listed
    /// beside the members governance names, so a governance change keeps them.
    environments: crate::environment::EnvironmentRegistry,
}

impl GovernedAccessWriter {
    pub(crate) fn state_root(&self) -> &std::path::Path {
        &self.state_root
    }

    /// A workspace this service deleted admits nobody at the handshake either.
    pub(crate) fn forget_workspace(&self, workspace_id: WorkspaceId) {
        self.handshake_trust.forget(workspace_id);
    }

    /// Whether what the server decides `workspace_id`'s changes against holds
    /// what `device` may do. A workspace this server has not opened yet is
    /// projected by the settle that opens it, so it counts as holding it.
    pub(crate) async fn projects_access(
        &self,
        workspace_id: WorkspaceId,
        device: [u8; 32],
    ) -> bool {
        let Some(workspace) = self.workspaces.lock().await.get(&workspace_id).cloned() else {
            return true;
        };
        workspace
            .lock()
            .await
            .device_access
            .as_ref()
            .is_none_or(|device_access| device_access.contains_key(&device))
    }

    /// Replace the access of each member device of the workspace.
    pub(crate) async fn replace_access(
        &self,
        workspace_id: WorkspaceId,
        device_access: crate::workspace::DeviceAccess,
    ) -> Result<(), ServiceError> {
        let workspace = crate::workspace_io::get_or_initialize(
            &self.workspaces,
            self.state_root.clone(),
            workspace_id,
            crate::workspace_access::AdmissionMode::Explicit,
            crate::peer::MAX_HELD_WORKSPACES,
        )
        .await?;
        workspace.lock().await.project_access(device_access);
        Ok(())
    }

    pub(crate) async fn replace(
        &self,
        workspace_id: WorkspaceId,
        access: tilo_peer_state::GovernedAccess,
        revoked: Vec<[u8; 32]>,
    ) -> Result<Vec<[u8; 32]>, ServiceError> {
        if self.handshake_trust.allow_any {
            return Err(ServiceError::GovernedAccess(
                "allow_any cannot serve governed workspace admission".into(),
            ));
        }
        let workspace = crate::workspace_io::get_or_initialize(
            &self.workspaces,
            self.state_root.clone(),
            workspace_id,
            crate::workspace_access::AdmissionMode::Explicit,
            crate::peer::MAX_HELD_WORKSPACES,
        )
        .await?;
        // An environment this server placed in the workspace is listed by
        // the server, not by governance: every projection keeps it until it
        // stops, confined to its document by the register.
        let mut member_devices = access.member_devices;
        member_devices.extend(
            self.environments
                .placed_in(workspace_id)
                .into_iter()
                .map(|(device, _, _)| device),
        );
        member_devices.sort_unstable();
        member_devices.dedup();
        let mut member_user_roots = access.member_user_roots;
        member_user_roots.sort_unstable();
        member_user_roots.dedup();
        let roots: HashSet<[u8; 32]> = member_user_roots.iter().copied().collect();
        let result = crate::workspace_io::replace_governed_access(
            &workspace,
            crate::workspace_files::GovernedAccessRecord {
                member_devices,
                member_user_roots,
                governed: true,
            },
            revoked,
        )
        .await;
        // Also after an I/O error the handshake union must reflect the
        // fail-closed ceiling. A successful write leaves the workspace
        // holding exactly the roots it was asked for; an error leaves the
        // intersection the workspace now holds.
        let roots = match &result {
            Ok(_) => roots,
            Err(_) => workspace.lock().await.governed_user_roots.clone(),
        };
        self.handshake_trust.settled(workspace_id, roots);
        Ok(result?)
    }
}

#[derive(Clone)]
pub(crate) struct HandshakeTrust {
    base_roots: Arc<HashSet<[u8; 32]>>,
    pub(crate) live_policy: Arc<std::sync::RwLock<TrustPolicy>>,
    pub(crate) allow_any: bool,
    /// The user roots each governed workspace admits, by workspace: the
    /// allow list is the operator's own together with all of them. One entry
    /// is replaced after that workspace's access settles, so an admission
    /// decision does not walk and lock every workspace the service governs —
    /// which, at hundreds of workspaces, is most of the cost of admitting one
    /// more.
    contributions: Arc<std::sync::Mutex<HashMap<WorkspaceId, HashSet<[u8; 32]>>>>,
}

impl HandshakeTrust {
    pub(crate) fn new(operator_policy: &TrustPolicy, own_user: [u8; 32]) -> Self {
        let allow_any = matches!(operator_policy, TrustPolicy::AllowAny);
        Self {
            base_roots: Arc::new(policy_roots(operator_policy, own_user)),
            live_policy: Arc::new(std::sync::RwLock::new(if allow_any {
                TrustPolicy::AllowAny
            } else {
                TrustPolicy::AllowList { users: Vec::new() }
            })),
            allow_any,
            contributions: Arc::new(std::sync::Mutex::default()),
        }
    }

    /// Rebuild every workspace's contribution by reading them, for a service
    /// starting.
    pub(crate) async fn refresh(&self, workspaces: &WorkspaceRegistry) {
        if self.allow_any {
            return;
        }
        let mut contributions = HashMap::new();
        let held = workspaces.lock().await;
        for (workspace_id, runtime) in held.iter() {
            let runtime = runtime.lock().await;
            contributions.insert(*workspace_id, runtime.governed_user_roots.clone());
        }
        drop(held);
        *self.contributions.lock_recover() = contributions;
        self.apply();
    }

    /// `workspace_id`'s access settled: what it admits is `roots` now.
    pub(crate) fn settled(&self, workspace_id: WorkspaceId, roots: HashSet<[u8; 32]>) {
        if self.allow_any {
            return;
        }
        self.contributions
            .lock_recover()
            .insert(workspace_id, roots);
        self.apply();
    }

    /// `workspace_id` is gone: it admits nobody here any more.
    pub(crate) fn forget(&self, workspace_id: WorkspaceId) {
        if self.allow_any {
            return;
        }
        if self
            .contributions
            .lock_recover()
            .remove(&workspace_id)
            .is_some()
        {
            self.apply();
        }
    }

    /// The live allow list: the operator's own roots and every workspace's.
    /// Held together with the contributions it reads, so a union computed
    /// from an older map never lands after a newer one.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the contributions lock is held until the allow list is written, so a union read from an older map cannot land after a newer one"
    )]
    fn apply(&self) {
        let contributions = self.contributions.lock_recover();
        let mut roots = self.base_roots.as_ref().clone();
        for admitted in contributions.values() {
            roots.extend(admitted.iter().copied());
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
/// lists. One that only adds roots, as each person a public workspace admits
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
            let key = hex::decode(encoded)
                .map_err(|error| error.to_string())
                .and_then(|bytes| {
                    <[u8; 32]>::try_from(bytes).map_err(|_| "not 32 bytes".to_owned())
                });
            match key {
                Ok(key) => Some(key),
                Err(error) => {
                    tracing::error!(user = %encoded, %error, "a trusted user in the service's allow list is not a key; it is not trusted");
                    None
                }
            }
        }));
    }
    roots
}

/// Bundle of identity material a service needs to run. The server daemon's
/// `main.rs` loads it from
/// [`tilo_device_identity::persist`] and passed in explicitly so the
/// integration tests can wire a `tempdir`-based identity.
#[derive(Clone)]
pub struct ServiceIdentity {
    /// The user root pubkey the server chains to.
    pub user_public_key: [u8; 32],
    pub device_signing_key: ed25519_dalek::SigningKey,
    /// This device's signed cert (issued by the user root).
    pub device_cert: DeviceCert,
    /// Current revocation list (may be empty).
    pub revocations: RevocationList,
}

/// Where a device's workspace service keeps its own key and the certificate
/// its device issued for it, under the service's state root.
const OWN_KEY_DIR: &str = "service-key";

impl ServiceIdentity {
    /// The identity of the workspace service of the device `device`, whose
    /// key `signer` holds, keeping its state under `root`: a
    /// key of the service's own, made the first time and kept there, and the
    /// certificate `device` issues for it as its workspace service
    /// ([`tilo_user_identity::prepare_service_cert`]). The certificate is
    /// issued again whenever the one kept no longer holds for `device` as it
    /// is certified now.
    ///
    /// The service never signs under the device's key: the device and its
    /// service each write under a key of their own, so neither takes a
    /// sequence number the other already gave a change. A machine copied from
    /// another serves under a key of its own too: the kept key's certificate
    /// was issued by the device this one was copied from, which still serves
    /// with it.
    ///
    /// # Errors
    ///
    /// The key or certificate cannot be kept under `root`, or `signer` does
    /// not sign for `device`.
    pub async fn beside_device(
        root: &std::path::Path,
        device: &DeviceCert,
        signer: &dyn collab_types::signer::DeviceSigner,
    ) -> Result<Self, ServiceError> {
        use tilo_device_identity::persist;
        let fail = |what: &str, error: &dyn std::fmt::Display| {
            ServiceError::Identity(format!("this device's workspace service {what}: {error}"))
        };
        let dir = root.join(OWN_KEY_DIR);
        let key_path = dir.join("key.bin");
        let cert_path = dir.join("cert.bin");
        // A certificate the person's other device issued belongs to the machine this one was
        // copied from: two machines serving as one key would be one author, giving changes the
        // same sequence numbers, as two devices holding one key would. The copy serves with a key
        // of its own from then on.
        let branched = persist::read_device_cert(&cert_path)
            .ok()
            .and_then(|cert| cert.issuer)
            .is_some_and(|issuer| issuer.device_public_key != device.device_public_key);
        let key = if key_path.exists() && !branched {
            persist::read_signing_key(&key_path).map_err(|error| fail("key", &error))?
        } else {
            std::fs::create_dir_all(&dir).map_err(|error| fail("key", &error))?;
            let key = ed25519_dalek::SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(
                getrandom::SysRng,
            ));
            persist::write_signing_key(&key_path, &key).map_err(|error| fail("key", &error))?;
            key
        };
        let public = key.verifying_key().to_bytes();
        let now = unix_now_ns();
        let kept = persist::read_device_cert(&cert_path).ok().filter(|cert| {
            cert.device_public_key == public
                && cert.issuer.as_deref() == Some(device)
                && verify_device_cert_at(cert, now).is_ok()
        });
        let cert = if let Some(cert) = kept {
            cert
        } else {
            let mut cert = tilo_user_identity::prepare_service_cert(
                device,
                public,
                &device.device_label,
                now.max(device.issued_at_ns),
            );
            tilo_user_identity::sign_device_cert_async(&mut cert, signer)
                .await
                .map_err(|error| fail("certificate", &error))?;
            persist::write_device_cert(&cert_path, &cert)
                .map_err(|error| fail("certificate", &error))?;
            cert
        };
        Ok(Self {
            user_public_key: cert.user_public_key,
            device_signing_key: key,
            device_cert: cert,
            // Each workspace's own revocations reach the service through its
            // governance.
            revocations: RevocationList::new(),
        })
    }
}

pub fn validate_service_device_identity(identity: &ServiceIdentity) -> Result<(), ServiceError> {
    verify_device_cert_at(&identity.device_cert, unix_now_ns())
        .map_err(|error| ServiceError::Identity(format!("invalid device certificate: {error}")))?;
    if identity.device_cert.user_public_key != identity.user_public_key {
        return Err(ServiceError::Identity(
            "device certificate does not chain to the configured user public key".into(),
        ));
    }
    if identity.device_cert.device_public_key
        != identity.device_signing_key.verifying_key().to_bytes()
    {
        return Err(ServiceError::Identity(
            "device certificate does not match the configured device signing key".into(),
        ));
    }
    if identity.revocations.refuses(&identity.device_cert) {
        return Err(ServiceError::Identity(
            "device certificate is revoked".into(),
        ));
    }
    Ok(())
}

/// The `ProtocolHandler` iroh wires to the ALPN.
#[derive(Clone)]
pub struct ServerProtocol {
    pub workspaces: WorkspaceRegistry,
    pub trust_policy: Arc<std::sync::RwLock<TrustPolicy>>,
    pub identity: LiveIdentity,
    pub metrics: Arc<Metrics>,
    pub(crate) app_serving: Option<Arc<dyn crate::app_serving::AppActionServer>>,
    /// Set once when the service governs workspaces: what its sessions answer a
    /// device's workspace administration with.
    pub admin: Arc<std::sync::OnceLock<Arc<crate::workspaces::Workspaces>>>,
    /// Set once when the service serves a fetch proxy: the grant a session takes
    /// for its connection there.
    pub fetch: Arc<std::sync::OnceLock<Arc<dyn crate::peer::FetchGrants>>>,
    pub agents: crate::peer::AgentRegistry,
    /// The devices this service certified for its browser environments, see
    /// [`crate::environment`].
    pub environments: crate::environment::EnvironmentRegistry,
    /// Whether this service's work setting takes workspaces of people other
    /// than its own to hold ([`tilo_server_protocol::WorkFrom::takes_others`]):
    /// their governing services, whom nothing here admits yet, then open a
    /// session to hand them over. Every workspace such a session reaches
    /// still asks the live policy, which admits none of theirs.
    pub(crate) holds_for_others: bool,
}

impl ServerProtocol {
    /// This server's certificate and agent trust.
    pub fn trust(&self) -> crate::workspace_access::ServerTrust<'_> {
        crate::workspace_access::ServerTrust {
            revocations: &self.identity.revocations,
            policy: &self.trust_policy,
            own_user: &self.identity.user_public_key,
            agents: &self.agents,
            environments: &self.environments,
        }
    }

    /// Whether `user` is the person this server runs for, or is admitted by a
    /// workspace it serves: who its certification queue takes requests from.
    pub async fn admits(&self, user: [u8; 32]) -> bool {
        crate::workspace_access::admits_person(
            &self.workspaces,
            self.identity.user_public_key,
            user,
        )
        .await
    }

    /// The peer context a session of this server runs with.
    pub fn peer_context(&self) -> PeerContext {
        PeerContext {
            workspaces: self.workspaces.clone(),
            revocations: self.identity.revocations.clone(),
            trust_policy: self.trust_policy.clone(),
            own_user_public_key: self.identity.user_public_key,
            own_device_cert: self.identity.cert.clone(),
            metrics: self.metrics.clone(),
            own_device_public_key: self.identity.cert.device_public_key,
            app_serving: self.app_serving.clone(),
            admin: self.admin.get().cloned(),
            fetch: self.fetch.get().cloned(),
            agents: self.agents.clone(),
            environments: self.environments.clone(),
        }
    }
}

impl std::fmt::Debug for ServerProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServerProtocol")
    }
}

impl ProtocolHandler for ServerProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.accept_streams(connection).await
    }
}

impl ServerProtocol {
    /// Take `connection`'s first stream and serve its sessions on a task of
    /// their own, so the iroh router can accept the next connection.
    async fn accept_streams(&self, connection: Connection) -> Result<(), AcceptError> {
        let connection = tilo_server_client::link::Link::iroh(connection);
        let binding = connection.offer_binding().await;
        let first = connection
            .accept_bi()
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))?;
        tokio::spawn(serve_streams(self.clone(), connection, binding, first));
        Ok(())
    }

    /// What shows its workspaces' apps as web pages, where the apps it serves
    /// do ([`crate::app_serving::AppActionServer::pages`]).
    #[must_use]
    pub fn pages(&self) -> Option<&dyn crate::app_serving::AppPages> {
        self.app_serving.as_deref()?.pages()
    }

    /// Prove which member device opened a stream that does not say so itself
    /// (a WebTransport session's first): the server handshake under the live
    /// policy, as every session opens with.
    ///
    /// # Errors
    ///
    /// The device is not one this service admits, or the handshake failed.
    pub async fn authenticate_member<R, W>(
        &self,
        recv: &mut R,
        send: &mut W,
    ) -> Result<crate::auth::AuthenticatedPeer, HandshakeError>
    where
        R: tokio::io::AsyncRead + Unpin + Send,
        W: tokio::io::AsyncWrite + Unpin + Send,
    {
        self.identity
            .authenticate(
                EndpointTrust::Live(&self.trust_policy),
                recv,
                send,
                MAX_FRAME_BYTES,
            )
            .await
    }

    /// Serve every session `link` carries, each on a stream of its own, until
    /// it closes: what a WebTransport session at the session path is, as a
    /// connection on the server ALPN is.
    pub async fn serve_link(&self, link: tilo_server_client::link::Link) {
        let binding = link.offer_binding().await;
        let Ok(first) = link.accept_bi().await else {
            return;
        };
        serve_streams(self.clone(), link, binding, first).await;
    }
}

/// Serve every session `connection` carries, `first` (already accepted)
/// first, each on a stream of its own, until the connection closes. An entry
/// on any of them is bound to `binding`
/// ([`tilo_server_client::link::Link::offer_binding`]).
async fn serve_streams(
    ctx: ServerProtocol,
    connection: tilo_server_client::link::Link,
    binding: Option<[u8; 32]>,
    first: (
        tilo_server_client::link::LinkSend,
        tilo_server_client::link::LinkRecv,
    ),
) {
    let sessions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut first = Some(first);
    loop {
        let (send, recv) = match first.take() {
            Some(streams) => streams,
            None => match connection.accept_bi().await {
                Ok(streams) => streams,
                Err(_) => return,
            },
        };
        sessions.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let (ctx, connection) = (ctx.clone(), connection.clone());
        let sessions = Arc::clone(&sessions);
        tokio::spawn(async move {
            let closed = connection.clone();
            let result = Box::pin(serve_session(ctx, recv, send, binding, async move {
                closed.closed().await;
            }))
            .await;
            if let Err(error) = result {
                tracing::debug!("server session ended: {error}");
            }
            let last = sessions.fetch_sub(1, std::sync::atomic::Ordering::AcqRel) == 1;
            if last {
                connection.close("server session ended");
            }
        });
    }
}

/// One member session over `recv`/`send`: the bounded shared handshake
/// under the live policy (the owner and every user the service currently
/// admits), then the peer loop until the stream ends or `closed` resolves.
/// Every carrier runs this: an iroh stream, and an in-process stream. Only a
/// stream with a connection's keying material (`binding`) takes an entry.
async fn serve_session<R, W>(
    ctx: ServerProtocol,
    mut recv: R,
    mut send: W,
    binding: Option<[u8; 32]>,
    closed: impl std::future::Future<Output = ()> + Send,
) -> Result<(), ServiceError>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    ctx.metrics.inc_conns_accepted();
    // A device the service has not admitted yet may enter a workspace through
    // its entry on this session, where the service governs workspaces.
    let admit = ctx.admin.get().cloned();
    // A service that keeps workspaces for the people it let in takes a workspace
    // another service governs for its owner, as it does for an operator taking
    // others' workspaces: that service's device opens the session to hand it
    // over before anything here admits it.
    let takes_handed_over = ctx.holds_for_others
        || admit
            .as_deref()
            .is_some_and(crate::workspaces::Workspaces::keeps_workspaces_for_people);
    let (client_peer, entered) = match ctx
        .identity
        .authenticate_or_enter(
            if takes_handed_over {
                EndpointTrust::Fixed(crate::auth::HandshakeTrust::AnyCertifiedDevice)
            } else {
                EndpointTrust::Live(&ctx.trust_policy)
            },
            &mut recv,
            &mut send,
            MAX_FRAME_BYTES,
            admit
                .as_deref()
                .map(|workspaces| workspaces as &dyn crate::handshake::Admit),
            binding,
        )
        .await
    {
        Ok(entered) => entered,
        Err(error) => {
            if matches!(error, HandshakeError::Auth(_)) {
                ctx.metrics.inc_auth_rejected();
            }
            return Err(ServiceError::Handshake(error.to_string()));
        }
    };
    if let Some(entered) = entered {
        // The answer is the session's first envelope; a refused device hears
        // it and nothing else.
        let admitted = matches!(
            entered,
            tilo_server_protocol::entering::Entered::Admitted { .. }
        );
        let frame = tilo_delivery_protocol::transport_v2::encode(
            &tilo_delivery_protocol::transport_v2::EnvelopeV2::State(
                crate::transport::Envelope::Entered(entered),
            ),
        )
        .map_err(|error| ServiceError::Handshake(error.to_string()))?;
        tokio::io::AsyncWriteExt::write_all(&mut send, &frame)
            .await
            .map_err(|error| ServiceError::Handshake(error.to_string()))?;
        if !admitted {
            ctx.metrics.inc_auth_rejected();
            told_then_closed(&mut send, closed).await;
            return Err(ServiceError::Handshake("the entry was refused".to_owned()));
        }
    }
    ctx.metrics.inc_conns_authed();
    // One of an environment's devices may be here: its environment is up, and
    // an end being waited on is not one.
    ctx.environments.arrived(client_peer.device_public_key);
    let context = ctx.peer_context();
    if let Err(e) = Box::pin(run_peer_until(client_peer, closed, recv, send, context)).await {
        tracing::debug!("peer session ended with error: {e}");
    }
    Ok(())
}

/// How long a refused entry's stream stays open for its device to read why.
const REFUSAL_READ: std::time::Duration = std::time::Duration::from_secs(5);

/// End a refused entry's stream once its device was told why: the stream
/// finishes, and the connection closes when the device closes it, or after
/// [`REFUSAL_READ`]. Closed at once, the connection would drop the answer
/// still on its way.
async fn told_then_closed<W>(send: &mut W, closed: impl std::future::Future<Output = ()>)
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let _ = tokio::io::AsyncWriteExt::shutdown(send).await;
    let _ = tokio::time::timeout(REFUSAL_READ, closed).await;
}

/// The device's own host, as its native owner joins the workspaces placed on it.
impl tilo_peer_state::owner::session::OwnHost for WorkspaceService {
    fn join_service(&self) -> Arc<dyn tilo_peer_state::join_transport::LocalJoinService> {
        Self::join_service(self)
    }

    fn direct(&self, workspace: WorkspaceId) -> Option<tilo_peer_state::direct::DirectSync> {
        self.workspaces().direct().link(workspace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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

    /// A machine copied from another serves under a key of its own: the key it carries was
    /// certified by the device it was copied from, which still serves with it, and the same key on
    /// two machines gives the same changes one sequence number.
    #[tokio::test]
    async fn a_machine_copied_from_another_serves_under_a_key_of_its_own() {
        use ed25519_dalek::SigningKey;
        use tilo_user_identity::DeviceCapabilities;

        let state = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[1; 32]);
        let source =
            crate::workspaces::fixtures::device_cert(&root, "source", DeviceCapabilities::member());
        let copy =
            crate::workspaces::fixtures::device_cert(&root, "copy", DeviceCapabilities::member());
        let first = ServiceIdentity::beside_device(state.path(), &source.1, &source.0)
            .await
            .unwrap();
        let copied = ServiceIdentity::beside_device(state.path(), &copy.1, &copy.0)
            .await
            .unwrap();
        assert_ne!(
            copied.device_cert.device_public_key, first.device_cert.device_public_key,
            "the copy serves under a key of its own"
        );
        assert!(copied.device_cert.is_of_device(&copy.1.device_public_key));
        let again = ServiceIdentity::beside_device(state.path(), &copy.1, &copy.0)
            .await
            .unwrap();
        assert_eq!(
            again.device_cert.device_public_key, copied.device_cert.device_public_key,
            "the key it chose is the key it keeps"
        );
    }

    #[tokio::test]
    async fn dynamic_handshake_refresh_preserves_operator_root_pins() {
        let own_user = [1; 32];
        let operator_pin = [2; 32];
        let trust = HandshakeTrust::new(
            &TrustPolicy::AllowList {
                users: vec![hex::encode(operator_pin)],
            },
            own_user,
        );
        let workspaces = Arc::new(AsyncMutex::new(HashMap::new()));

        trust.refresh(&workspaces).await;

        assert_eq!(
            trust.live_policy.read_recover().clone(),
            TrustPolicy::AllowList {
                users: vec![hex::encode(own_user), hex::encode(operator_pin)]
            }
        );
    }

    /// What one workspace settles joins the handshake's allow list, and a
    /// workspace forgotten leaves it, without reading any other workspace:
    /// the cost of admitting one more workspace does not grow with how many
    /// the service governs.
    #[test]
    fn a_settled_workspace_joins_the_allow_list_and_a_forgotten_one_leaves_it() {
        let own_user = [1; 32];
        let trust = HandshakeTrust::new(&TrustPolicy::AllowList { users: Vec::new() }, own_user);
        let first = WorkspaceId::from_u128(7);
        let second = WorkspaceId::from_u128(8);
        let listed = |trust: &HandshakeTrust| -> Vec<String> {
            let policy = trust.live_policy.read_recover().clone();
            match policy {
                TrustPolicy::AllowList { users } => users,
                // A service that lists nobody, or trusts anyone, reads as an
                // empty list; the assertions below are what notice.
                TrustPolicy::AllowMatchingUser | TrustPolicy::AllowAny => Vec::new(),
            }
        };

        trust.settled(first, HashSet::from([[9; 32]]));
        assert_eq!(
            listed(&trust),
            vec![hex::encode(own_user), hex::encode([9; 32])],
            "the settled workspace's reader is trusted with the operator"
        );

        trust.settled(second, HashSet::from([[8; 32]]));
        assert_eq!(
            listed(&trust),
            vec![
                hex::encode(own_user),
                hex::encode([8; 32]),
                hex::encode([9; 32])
            ],
            "a second workspace adds its reader and keeps the first's"
        );

        trust.forget(first);
        assert_eq!(
            listed(&trust),
            vec![hex::encode(own_user), hex::encode([8; 32])],
            "a forgotten workspace's reader is trusted no more"
        );
    }

    /// Governing a workspace is holding its keys, so a service whose
    /// certificate may not own workspaces keeps none: asked to govern, it
    /// refuses instead of serving workspaces it could not seal.
    #[tokio::test]
    async fn a_service_whose_cert_cannot_own_governs_no_workspace() {
        let state = tempfile::tempdir().expect("a state directory");
        let (key, cert) = crate::workspaces::fixtures::governor_with(
            tilo_user_identity::DeviceCapabilities::nothing(),
        );
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
            .bind()
            .await
            .expect("a loopback endpoint binds");
        let opened = ServiceSessions::open(
            ServiceConfig::new(
                state.path().to_owned(),
                tilo_server_protocol::WorkSetting::none(),
            ),
            ServiceIdentity {
                user_public_key: cert.user_public_key,
                device_signing_key: key,
                device_cert: cert,
                revocations: RevocationList::new(),
            },
            endpoint.clone(),
            true,
            None,
            |_, server| server,
        )
        .await;
        endpoint.close().await;
        let Err(ServiceError::Identity(reason)) = opened else {
            panic!("a service whose certificate cannot own workspaces governed them");
        };
        assert!(
            reason.contains("own capability"),
            "the refusal names what the device needs: {reason}"
        );
    }
}
