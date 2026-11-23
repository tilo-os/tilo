//! The workspaces one device serves: their governance and the links their members connect
//! through, embeddable in any process that owns an iroh endpoint.
//!
//! A device runs the same stack whether it has a screen or not; [`ServiceConfig::work`] is what
//! it takes from others. [`WorkspaceService::open`] spawns nothing periodic: with no peers
//! connected the service wakes only on I/O, and a connected peer's link is re-checked when a
//! workspace's signed member list leaves someone out ([`crate::authority_epoch`]). Its embedder registers
//! [`WorkspaceService::protocols`] on its own router, or, when the router is built before any
//! workspace is placed here, [`deferred_protocols`], which refuse connections until
//! [`ServiceSlot::install`] gives them one.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use iroh::endpoint::{Connection, Endpoint};
use iroh::protocol::{AcceptError, DynProtocolHandler, ProtocolHandler};
use tilo_clock::unix_now_ns;
use tilo_lock::RwLockRecover as _;
use tilo_user_identity::{DeviceCert, RevocationList, verify_device_cert_at};

use tilo_endpoint_reachability::Reachability;
use tokio::sync::Mutex as AsyncMutex;

use crate::Reaching;
use crate::app_serving::AppActionServer;
use crate::handshake::{EndpointTrust, HandshakeError, LiveIdentity};
use crate::metrics::Metrics;
use crate::policy::TrustPolicy;
use crate::registry::WorkspaceRegistry;
use crate::transport::MAX_FRAME_BYTES;
use tilo_peer_state::transport::{StreamFuture, StreamRecv, StreamSend, Transport};
use tilo_server_protocol::{WorkSetting, WorkspaceId};

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
    /// The web origin this keeper's site answers at (`https://eu1.tilo.sh`), when it serves
    /// one: the address it signs carries it, so a member's device finds the site's pages and
    /// API without guessing them from its name.
    pub origin: Option<String>,
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
    /// The device this service's member copies act as: the device the embedder
    /// runs on, which keeps a workspace of its person's that another device
    /// serves as a member copy. `None` for a service that joins none.
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
            origin: None,
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
const SERVICE_ALPNS: [&[u8]; 3] = [
    tilo_delivery_protocol::workspace_join::WORKSPACE_JOIN_ALPN,
    tilo_peer_state::actions::ACTIONS_ALPN,
    tilo_peer_state::direct::DIRECT_ALPN,
];

/// A service's workspace stack: its workspaces, the handshake that admits their
/// members, and the links they reach them over.
#[derive(Clone)]
pub struct ServiceSessions {
    pub(crate) endpoint: Endpoint,
    pub workspaces: WorkspaceRegistry,
    pub metrics: Arc<Metrics>,
    pub identity: LiveIdentity,
    pub(crate) handshake_trust: HandshakeTrust,
    pub state_root: PathBuf,
    /// The service's own protocol handler: what proves a member's device and
    /// answers the app-serving questions its links ask.
    pub handler: ServerProtocol,
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

/// A service's workspaces: their signed member lists, and the app action server answering
/// their actions.
pub struct Governed {
    pub workspaces: crate::workspaces::Workspaces,
    pub(crate) actions: Arc<dyn AppActionServer>,
}

/// The workspaces a service serves, by id, as the sessions it opens share them.
fn empty_registry() -> WorkspaceRegistry {
    Arc::new(AsyncMutex::new(std::collections::HashMap::new()))
}

impl ServiceSessions {
    /// Open a service's sessions and its workspaces: those it founds, when its certificate holds
    /// workspace keys and it admits no one beyond the people it serves, and the member copies of
    /// its device. `wrap` gives the app action server every session answers remote actions with,
    /// from the workspaces and the service's own app action server (its workspaces' actions and
    /// apps): a server answers compute placement around it.
    ///
    /// # Errors
    ///
    /// The identity is invalid; the state root cannot be created; or the workspaces cannot be
    /// opened.
    pub async fn open(
        cfg: ServiceConfig,
        identity: ServiceIdentity,
        endpoint: Endpoint,
        channels: Option<crate::workspaces::DeviceKeyGroups>,
        wrap: impl FnOnce(
            &WorkspaceRegistry,
            Option<Arc<dyn AppActionServer>>,
        ) -> Option<Arc<dyn AppActionServer>>,
    ) -> Result<(Self, Option<Governed>), ServiceError> {
        validate_service_device_identity(&identity)?;
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
        // Founding a workspace is holding its keys, which its records are sealed under: a
        // service whose certificate holds none, or that admits anyone, keeps no workspace of
        // its own. One that does answers its workspaces' actions itself, beside whatever apps
        // its embedder serves.
        let founds =
            !handshake_trust.allow_any && identity.device_cert.capabilities.holds_workspace_keys();
        let governance = founds.then(crate::workspaces::ServiceWorkspaceActions::new);
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
            blocked: Arc::default(),
            identity: live.clone(),
            metrics: metrics.clone(),
            app_serving: wrap(&workspaces, own_server.clone()),
            admin: Arc::default(),
            agents: Arc::default(),
            environments: crate::environment::EnvironmentRegistry::at(
                cfg.state_root.join("environments.json"),
            ),
            fetch_grants: Arc::default(),
        };
        let sessions = Self {
            endpoint: endpoint.clone(),
            workspaces: workspaces.clone(),
            metrics: metrics.clone(),
            identity: live,
            handshake_trust,
            state_root: cfg.state_root.clone(),
            handler,
        };
        // A service that founds nothing still holds the member copies of a
        // device that joined workspaces another device serves; with neither,
        // there is nothing to open.
        if governance.is_none() && cfg.member.is_none() {
            return Ok((sessions, None));
        }
        // No workspace this server keeps is opened with an address it would
        // have to rename: the endpoint names where it is reached first, when it
        // will at all.
        cfg.reaching.awaited(&endpoint).await;
        let host_revocations = revocations.clone();
        let workspaces = Box::pin(crate::workspaces::Workspaces::open(
            sessions.workspace_services(
                &cfg,
                &identity,
                endpoint,
                revocations,
                channels,
                sessions.handler.app_serving.clone(),
            ),
            cfg.state_root.join("workspaces"),
            cfg.state_root.join("store"),
        ))
        .await
        .map_err(ServiceError::Workspaces)?;
        // The workspaces held here admit through their governance on this device's dispatch,
        // founded here or not.
        if governance.is_none() {
            crate::workspaces::ServiceWorkspaceActions::new().serve(workspaces.clone());
        }
        if let Some(governance) = governance {
            governance.attach(workspaces.clone());
            // Sessions ask its workspaces what a device may do; the lock is set
            // before the endpoint routes any session to the service.
            let _ = sessions.handler.admin.set(Arc::new(workspaces.clone()));
            // The host's own operations answer on its action service for the requests that name
            // no workspace: a page's whole administration, asked by a device of its person.
            if let Some(server) = own_server.clone() {
                workspaces.action_service().admit_host_operations(Arc::new(
                    crate::app_serving::HostOperations::new(
                        server,
                        Arc::clone(&host_revocations),
                        workspaces.downgrade(),
                    ),
                ));
            }
            // A device without a certificate from this host enters through the entry action
            // alone, in a scope of its own: nothing else is reachable there.
            workspaces.action_service().admit_uncertified(
                tilo_server_protocol::workspaces::ENTRY_SCOPE,
                Arc::new(crate::app_serving::EntryOperations::new(
                    workspaces.clone(),
                    host_revocations,
                )),
            );
            #[cfg(all(
                not(target_arch = "wasm32"),
                any(feature = "agents", feature = "device-keys")
            ))]
            {
                let carrying = workspaces.clone();
                tokio::spawn(async move { carrying.carry_vaults_over().await });
            }
        }
        // One store of the calls that wait for a person's yes serves this device: every
        // dispatch here keeps its asked call in it, `tilo.asking#list` lists them, and
        // `tilo.invocations#resolve` answers them, whichever embedder opened the service.
        Box::pin(workspaces.open_asked_store(tokio::runtime::Handle::current()))
            .await
            .map_err(ServiceError::Workspaces)?;
        Ok((
            sessions,
            Some(Governed {
                workspaces,
                actions: own_server.unwrap_or_else(no_actions),
            }),
        ))
    }

    /// What the workspaces a service governs take from it: its workspaces' registry,
    /// access, records, identity and live trust, and `cfg`'s workspace settings.
    fn workspace_services(
        &self,
        cfg: &ServiceConfig,
        identity: &ServiceIdentity,
        endpoint: Endpoint,
        revocations: Arc<std::sync::RwLock<RevocationList>>,
        channels: Option<crate::workspaces::DeviceKeyGroups>,
        apps: Option<Arc<dyn AppActionServer>>,
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
            blocked: self.handler.blocked.clone(),
            agents: self.handler.agents.clone(),
            environments: self.handler.environments.clone(),
            work: cfg.work.clone(),
            reachability: cfg.reachability.clone(),
            reaching: cfg.reaching,
            holder: Arc::new(std::sync::RwLock::new(crate::workspaces::HolderClaim {
                label: cfg.label.clone(),
                cloud: cfg.rented,
                origin: cfg.origin.clone(),
            })),
            member: cfg.member.clone(),
            surfaces: {
                // The settings are resources of every workspace that keeps a person's or a
                // device's: each section's kind and setters are declared on every holder alike.
                cfg.surfaces.register_built_in(
                    settings_store::workspace::SURFACE_APP,
                    settings_store::workspace::surface_json(),
                );
                Arc::clone(&cfg.surfaces)
            },
            apps,
            fetch_grants: self.handler.fetch_grants.clone(),
        }
    }

    /// What settles each workspace's member list onto the service's links.
    pub(crate) fn access_writer(&self) -> GovernedAccessWriter {
        GovernedAccessWriter {
            workspaces: self.workspaces.clone(),
            handshake_trust: self.handshake_trust.clone(),
            state_root: self.state_root.clone(),
            environments: self.handler.environments.clone(),
        }
    }

    /// Whether `device` holds a link to `workspace_id` here now: one the
    /// workspace's host admitted and has not refused.
    pub async fn is_subscribed(&self, workspace_id: WorkspaceId, device: &[u8; 32]) -> bool {
        self.handler
            .admin
            .get()
            .is_some_and(|workspaces| workspaces.linked(workspace_id, device))
    }

    /// Stop what answered for the service's members' devices: the app action
    /// server the embedder brought stops here, while the runtime its threads
    /// work on still runs.
    pub fn shutdown(&self) {
        if let Some(answers) = &self.handler.app_serving {
            answers.shutdown();
        }
    }
}

/// The handlers of a governing service's ALPNs, shared between the routers
/// that register them.
fn service_handlers(
    sessions: &ServiceSessions,
    workspaces: &crate::workspaces::Workspaces,
) -> Vec<(&'static [u8], Arc<dyn DynProtocolHandler>)> {
    vec![
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
        // link a device runs for itself: its first stream proves which device
        // dialled it, then the dial names its workspace.
        (
            tilo_peer_state::direct::DIRECT_ALPN,
            Arc::new(WorkspaceLink {
                serving: workspaces.direct(),
                server: sessions.handler.clone(),
            }) as Arc<dyn DynProtocolHandler>,
        ),
    ]
}

/// The workspaces' own ALPNs on a server's router.
pub fn workspaces_protocols(
    sessions: &ServiceSessions,
    workspaces: &crate::workspaces::Workspaces,
) -> Vec<(&'static [u8], Box<dyn DynProtocolHandler>)> {
    service_handlers(sessions, workspaces)
        .into_iter()
        .map(|(alpn, handler)| {
            (
                alpn,
                Box::new(Shared(handler)) as Box<dyn DynProtocolHandler>,
            )
        })
        .collect()
}

/// A member device's link to its workspace over iroh: the first stream proves which device
/// dialled it, the session handshake this service admits its members with, and the device proved
/// is the one the workspace's admission decides on. A page dials with an endpoint key of its own,
/// which is not its device key, and is admitted as the device it proved.
#[derive(Clone, Debug)]
struct WorkspaceLink {
    /// The workspaces' links, serving the workspace the dial names.
    serving: tilo_peer_state::direct::Serving,
    /// The service, whose handshake proves the member device.
    server: ServerProtocol,
}

impl ProtocolHandler for WorkspaceLink {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let served: Result<(), String> = async {
            let (mut send, mut recv) = connection
                .accept_bi()
                .await
                .map_err(|error| error.to_string())?;
            let device = self
                .server
                .authenticate_member(&mut recv, &mut send)
                .await
                .map_err(|error| error.to_string())?;
            // The rest of the link is the connection, its device the one it proved.
            let proved = Proved {
                connection,
                device: device.device_public_key,
            };
            self.serving.serve_link(&proved).await
        }
        .await;
        if let Err(error) = served {
            tracing::debug!(%error, "a member's link to its workspace ended");
        }
        Ok(())
    }
}

/// A link whose device is the one its first stream proved: an iroh endpoint's key is not always
/// the device's, as a page's is not, so the transport's remote is the proved device.
struct Proved {
    connection: Connection,
    device: [u8; 32],
}

impl Transport for Proved {
    fn open_uni(&self) -> StreamFuture<'_, StreamSend> {
        Transport::open_uni(&self.connection)
    }

    fn accept_uni(&self) -> StreamFuture<'_, StreamRecv> {
        Transport::accept_uni(&self.connection)
    }

    fn open_bi(&self) -> StreamFuture<'_, (StreamSend, StreamRecv)> {
        Transport::open_bi(&self.connection)
    }

    fn accept_bi(&self) -> StreamFuture<'_, (StreamSend, StreamRecv)> {
        Transport::accept_bi(&self.connection)
    }

    fn remote(&self) -> [u8; 32] {
        self.device
    }

    fn close(&self, code: u32, reason: &[u8]) {
        Transport::close(&self.connection, code, reason);
    }

    fn close_reason(&self) -> Option<tilo_server_protocol::refusal::Refusal> {
        Transport::close_reason(&self.connection)
    }
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
    /// Open the service on `endpoint`, which the embedder owns and routes: the workspaces it
    /// founds, when its certificate holds workspace keys, and the member copies of the
    /// device it serves ([`ServiceConfig::member`]). Spawns nothing periodic: with no peers
    /// connected it wakes only on I/O.
    ///
    /// # Errors
    ///
    /// The identity is invalid; `cfg` admits anyone, under which no workspace is kept;
    /// the state root cannot be created; the service keeps no workspace at all; or the
    /// workspaces kept there cannot be opened.
    pub async fn open(
        cfg: ServiceConfig,
        identity: ServiceIdentity,
        endpoint: Endpoint,
        channels: Option<crate::workspaces::DeviceKeyGroups>,
    ) -> Result<Self, ServiceError> {
        if matches!(cfg.trust_policy, TrustPolicy::AllowAny) {
            return Err(ServiceError::Identity(
                "a service admitting anyone keeps no workspaces".into(),
            ));
        }
        let (sessions, governed) = Box::pin(ServiceSessions::open(
            cfg,
            identity,
            endpoint,
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
                "the service's certificate holds no workspace keys, and it keeps no member copies"
                    .into(),
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

    /// The ALPN handlers to register on the embedder's router: workspace join,
    /// workspace administration and the device link.
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

    /// The service's workspaces, handshake and identity, as its links read them.
    #[must_use]
    pub const fn sessions(&self) -> &ServiceSessions {
        &self.sessions
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
            self.metrics_counter(),
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

    /// The counters this service bumps, for a reader its embedder serves beside it.
    #[must_use]
    pub fn metrics_counter(&self) -> Arc<crate::metrics::Metrics> {
        Arc::clone(&self.sessions.metrics)
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

/// Where a device's workspaces are: held now, as a daemon's are when it starts its broker, or in
/// the device's own workspace service once it opens, as a desktop session's are after it starts
/// its broker. What needs them waits for the service rather than failing on that order.
#[derive(Clone)]
pub enum WorkspacesAt {
    Held(crate::workspaces::Workspaces),
    Opening(ServiceSlot),
}

impl WorkspacesAt {
    /// The workspaces, waiting for this device's service to open where it has not.
    pub async fn get(&self) -> crate::workspaces::Workspaces {
        match self {
            Self::Held(workspaces) => workspaces.clone(),
            Self::Opening(service) => service.installed().await.workspaces().clone(),
        }
    }

    /// The workspaces, where they are open now.
    #[must_use]
    pub fn now(&self) -> Option<crate::workspaces::Workspaces> {
        match self {
            Self::Held(workspaces) => Some(workspaces.clone()),
            Self::Opening(service) => Some(service.service()?.workspaces().clone()),
        }
    }
}

impl From<crate::workspaces::Workspaces> for WorkspacesAt {
    fn from(workspaces: crate::workspaces::Workspaces) -> Self {
        Self::Held(workspaces)
    }
}

impl From<ServiceSlot> for WorkspacesAt {
    fn from(service: ServiceSlot) -> Self {
        Self::Opening(service)
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

/// Settles each workspace's signed member list onto the sessions of the service: who its
/// sessions admit, what each member device may do, and the people the handshake trusts.
/// Nothing of it is kept on disk: the list is the workspace's own, and each start reads it
/// again.
#[derive(Clone)]
pub(crate) struct GovernedAccessWriter {
    workspaces: WorkspaceRegistry,
    handshake_trust: HandshakeTrust,
    state_root: PathBuf,
    /// The browser environments this server placed: their devices are listed
    /// beside the members the list names, so a change to it keeps them.
    environments: crate::environment::EnvironmentRegistry,
}

impl GovernedAccessWriter {
    pub(crate) fn state_root(&self) -> &std::path::Path {
        &self.state_root
    }

    /// Which workspaces here each person is a member of: what the handshake trusts.
    pub(crate) fn memberships(&self) -> Arc<crate::workspaces::memberships::Memberships> {
        Arc::clone(&self.handshake_trust.memberships)
    }

    /// The people the workspaces here have changed: the handshake trusts who they have now.
    pub(crate) fn people_moved(&self) {
        self.handshake_trust.apply();
    }

    /// Take `access`, the members `workspace_id`'s signed list has now, `revoked`, the keys its
    /// people revoked, and `device_access`, what each member device may do, as what the
    /// workspace's sessions admit and decide by.
    pub(crate) async fn settle(
        &self,
        workspace_id: WorkspaceId,
        mut access: tilo_peer_state::GovernedAccess,
        revoked: Vec<[u8; 32]>,
        device_access: crate::workspace::DeviceAccess,
    ) -> Result<(), ServiceError> {
        let workspace = crate::workspace_io::get_or_initialize(
            &self.workspaces,
            self.state_root.clone(),
            workspace_id,
            crate::registry::MAX_HELD_WORKSPACES,
        )
        .await?;
        // An environment this server placed in the workspace is listed by
        // the server, not by the member list: every settle keeps it until it
        // stops, confined to its document by the register.
        access.member_devices.extend(
            self.environments
                .placed_in(workspace_id)
                .into_iter()
                .map(|(device, _, _)| device),
        );
        workspace
            .lock()
            .await
            .settle_members(access, revoked, Some(device_access));
        self.handshake_trust.apply();
        Ok(())
    }

    /// `workspace_id` is served here no more: its sessions admit nobody, and the
    /// people only it had are trusted no more.
    pub(crate) async fn stop(&self, workspace_id: WorkspaceId) {
        let workspace = self.workspaces.lock().await.get(&workspace_id).cloned();
        if let Some(workspace) = workspace {
            workspace.lock().await.settle_members(
                tilo_peer_state::GovernedAccess::default(),
                [],
                None,
            );
        }
        self.handshake_trust.apply();
    }
}

#[derive(Clone)]
pub(crate) struct HandshakeTrust {
    base_roots: Arc<HashSet<[u8; 32]>>,
    pub(crate) live_policy: Arc<std::sync::RwLock<TrustPolicy>>,
    pub(crate) allow_any: bool,
    /// The people each workspace here has as members: the allow list is the operator's own
    /// together with all of them, read from this index rather than by walking and locking
    /// every workspace, which at hundreds of workspaces would be most of the cost of
    /// admitting one more person.
    memberships: Arc<crate::workspaces::memberships::Memberships>,
}

impl HandshakeTrust {
    pub(crate) fn new(operator_policy: &TrustPolicy, own_user: [u8; 32]) -> Self {
        let allow_any = matches!(operator_policy, TrustPolicy::AllowAny);
        let trust = Self {
            base_roots: Arc::new(policy_roots(operator_policy, own_user)),
            live_policy: Arc::new(std::sync::RwLock::new(if allow_any {
                TrustPolicy::AllowAny
            } else {
                TrustPolicy::AllowList { users: Vec::new() }
            })),
            allow_any,
            memberships: Arc::default(),
        };
        trust.apply();
        trust
    }

    /// The live allow list: the operator's own roots and every workspace's members. Written
    /// under the index's people as they stand, so a list read from older people never lands
    /// after a newer one.
    fn apply(&self) {
        if self.allow_any {
            return;
        }
        let mut live = self.live_policy.write_recover();
        let mut roots = self.base_roots.as_ref().clone();
        roots.extend(self.memberships.people());
        let mut users = roots.into_iter().map(hex::encode).collect::<Vec<_>>();
        users.sort_unstable();
        let next = TrustPolicy::AllowList { users };
        let narrowed = narrows(&live, &next);
        *live = next;
        drop(live);
        if narrowed {
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
pub(crate) const OWN_KEY_DIR: &str = tilo_paths::desktop_service::KEY_DIR;

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
        let key_path = dir.join(tilo_paths::desktop_service::KEY_FILE);
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

/// The service's own identity and admission, as its embedder's handlers and its
/// links read them.
#[derive(Clone)]
pub struct ServerProtocol {
    pub workspaces: WorkspaceRegistry,
    pub trust_policy: Arc<std::sync::RwLock<TrustPolicy>>,
    /// The devices and people this host's operator blocks ([`crate::workspaces::blocked`]),
    /// shared with the workspaces it serves.
    pub blocked: Arc<std::sync::RwLock<crate::workspaces::blocked::BlockedKeys>>,
    pub identity: LiveIdentity,
    pub metrics: Arc<Metrics>,
    pub(crate) app_serving: Option<Arc<dyn crate::app_serving::AppActionServer>>,
    /// Set once when the service governs workspaces: what its links answer a
    /// device's workspace administration with.
    pub admin: Arc<std::sync::OnceLock<Arc<crate::workspaces::Workspaces>>>,
    pub agents: crate::registry::AgentRegistry,
    /// The devices this service certified for its browser environments, see
    /// [`crate::environment`].
    pub environments: crate::environment::EnvironmentRegistry,
    /// The tokens this host's fetch proxy takes, minted when a page asks its
    /// host for one: the listener that serves the proxy takes the same ones.
    pub fetch_grants: Arc<crate::fetch_grants::Grants>,
}

impl ServerProtocol {
    /// This server's certificate and agent trust.
    pub fn trust(&self) -> crate::workspace_access::ServerTrust<'_> {
        crate::workspace_access::ServerTrust {
            revocations: &self.identity.revocations,
            policy: &self.trust_policy,
            blocked: &self.blocked,
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
}

impl std::fmt::Debug for ServerProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServerProtocol")
    }
}

impl ServerProtocol {
    /// What shows its workspaces' apps as web pages, where the apps it serves
    /// do ([`crate::app_serving::AppActionServer::pages`]).
    #[must_use]
    pub fn pages(&self) -> Option<&dyn crate::app_serving::AppPages> {
        self.app_serving.as_deref()?.pages()
    }

    /// The direct links of the workspaces this server governs, when it governs
    /// any: what a device's link to them is served by
    /// ([`tilo_peer_state::direct::Serving::serve_link`]) over the browser's
    /// transport as much as over iroh.
    #[must_use]
    pub fn direct(&self) -> Option<tilo_peer_state::direct::Serving> {
        Some(self.admin.get()?.direct())
    }

    /// The action service this server's workspaces answer their devices'
    /// requests on, when it governs any: what the same service serves on a
    /// device's link ([`tilo_peer_state::actions::ACTIONS_ALPN`]).
    #[must_use]
    pub fn actions(&self) -> Option<tilo_peer_state::actions::ActionService> {
        Some(self.admin.get()?.action_service())
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

    #[test]
    fn the_handshake_keeps_the_operators_pinned_roots() {
        let own_user = [1; 32];
        let operator_pin = [2; 32];
        let trust = HandshakeTrust::new(
            &TrustPolicy::AllowList {
                users: vec![hex::encode(operator_pin)],
            },
            own_user,
        );
        assert_eq!(
            trust.live_policy.read_recover().clone(),
            TrustPolicy::AllowList {
                users: vec![hex::encode(own_user), hex::encode(operator_pin)]
            }
        );
    }

    /// The people one workspace settles join the handshake's allow list, and a
    /// workspace forgotten leaves it, without reading any other workspace:
    /// the cost of admitting one more workspace does not grow with how many
    /// the service keeps.
    #[test]
    fn a_settled_workspace_joins_the_allow_list_and_a_forgotten_one_leaves_it() {
        let own_user = [1; 32];
        let trust = HandshakeTrust::new(&TrustPolicy::AllowList { users: Vec::new() }, own_user);
        let listed = |trust: &HandshakeTrust| -> Vec<String> {
            let policy = trust.live_policy.read_recover().clone();
            match policy {
                TrustPolicy::AllowList { users } => users,
                // A service that lists nobody, or trusts anyone, reads as an
                // empty list; the assertions below are what notice.
                TrustPolicy::AllowMatchingUser | TrustPolicy::AllowAny => Vec::new(),
            }
        };

        trust.memberships.record("first", [[9; 32]]);
        trust.apply();
        assert_eq!(
            listed(&trust),
            vec![hex::encode(own_user), hex::encode([9; 32])],
            "the settled workspace's reader is trusted with the operator"
        );

        trust.memberships.record("second", [[8; 32]]);
        trust.apply();
        assert_eq!(
            listed(&trust),
            vec![
                hex::encode(own_user),
                hex::encode([8; 32]),
                hex::encode([9; 32])
            ],
            "a second workspace adds its reader and keeps the first's"
        );

        trust.memberships.forget("first");
        trust.apply();
        assert_eq!(
            listed(&trust),
            vec![hex::encode(own_user), hex::encode([8; 32])],
            "a forgotten workspace's reader is trusted no more"
        );
    }
}
