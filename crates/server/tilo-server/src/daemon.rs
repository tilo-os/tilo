//! The long-running daemon: binds an iroh endpoint, accepts on
//! `tilo-server/1`, runs the auth handshake, hands off to
//! [`crate::peer::run_peer`].
//!
//! Also exposes the `Server` struct so integration tests can spin up
//! two servers in the same process and let them merge.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use iroh::endpoint::{Endpoint, presets};
use iroh::protocol::{DynProtocolHandler, Router};
use iroh_base::EndpointAddr;
use tokio::task::JoinHandle;

use tilo_user_identity::RevocationList;

use crate::config::ServerConfig;

use crate::handshake::LiveIdentity;
use crate::metrics::Metrics;
use crate::policy::TrustPolicy;
use crate::service::ServerProtocol;
use crate::transport::ALPN;
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_server_protocol::WorkspaceId;
use tilo_workspace_service::Reaching;

/// Failure modes for `Server::start`.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// Iroh endpoint bind / accept.
    #[error("iroh: {0}")]
    Iroh(String),
    /// The host's files beside the device store.
    #[error("files: {0}")]
    Files(#[from] crate::workspace_files::FilesError),
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
    /// An additional endpoint protocol conflicts with the server router.
    #[error("protocol registration: {0}")]
    ProtocolRegistration(String),
}

impl From<crate::service::ServiceError> for ServerError {
    fn from(error: crate::service::ServiceError) -> Self {
        use crate::service::ServiceError;
        match error {
            ServiceError::Identity(reason) => Self::Identity(reason),
            ServiceError::Files(error) => Self::Files(error),
            ServiceError::Io(error) => Self::Io(error),
            ServiceError::GovernedAccess(reason) => Self::GovernedAccess(reason),
            ServiceError::Workspaces(reason) => {
                Self::Identity(format!("governed workspaces: {reason}"))
            }
            ServiceError::Handshake(reason) => Self::Iroh(reason),
        }
    }
}

/// The identity a server runs with: a host's.
pub use crate::service::ServiceIdentity as ServerIdentity;

/// `identity` with the root-signed revocations persisted at `state_root`
/// applied, each verified: what a host reads when it starts, so a device its
/// root revoked is refused as it connects instead of only after it is in.
///
/// # Errors
///
/// Returns an error if the log cannot be read, or holds an entry that does
/// not verify or contradicts one already in `identity`.
pub fn with_persisted_revocations(
    identity: ServerIdentity,
    state_root: &std::path::Path,
) -> anyhow::Result<ServerIdentity> {
    let ServerIdentity {
        user_public_key,
        device_signing_key,
        device_cert,
        mut revocations,
    } = identity;
    for entry in crate::workspace_files::load_revocations(state_root)? {
        revocations
            .insert(entry)
            .context("verify persisted server revocation")?;
    }
    Ok(ServerIdentity {
        user_public_key,
        device_signing_key,
        device_cert,
        revocations,
    })
}

/// What an embedder plugs into a server it starts.
struct Servers {
    /// Answers remote-action frames addressed to this host, see
    /// [`Server::start_with_app_serving`].
    app_serving: Option<Arc<dyn crate::app_serving::AppActionServer>>,
    /// Classifies co-edit bodies.
    shell_payloads: Arc<dyn crate::payloads::ShellPayloads>,
}

impl Servers {
    /// The desktop's payload adapter, with `app_serving` if any.
    fn desktop(app_serving: Option<Arc<dyn crate::app_serving::AppActionServer>>) -> Self {
        Self {
            app_serving,
            shell_payloads: crate::payloads::neighbour(),
        }
    }
}

/// A running server. Prefer [`Server::shutdown`]; dropping the handle also
/// stops the daemon and its background tasks, but leaves peers to
/// notice by timeout.
pub struct Server {
    pub endpoint: Endpoint,
    /// Iroh router that wires the ALPN → protocol handler.
    router: Router,
    /// The workspace host this server is: its workspaces, the handshake that
    /// admits their members and every member's session.
    sessions: crate::service::ServiceSessions,
    /// Only the browser listener publishes this public navigation origin.
    #[cfg(feature = "web")]
    pub(crate) browser_origin: Arc<std::sync::RwLock<Option<Arc<String>>>>,
    /// The server's live session, which its browsers serve and its devices dial.
    #[cfg(feature = "web")]
    pub(crate) live_origin: Option<crate::web::live::LiveOrigin>,
    /// Background tasks (revocation refresh, metrics persistence). Aborted
    /// (per-handle) when the `Server` drops.
    background_tasks: Arc<std::sync::Mutex<Vec<AbortOnDrop>>>,
    /// Effective same-identity replication and serving policy, safe to expose
    /// to the shell/fs orchestrators because service claims were intersected
    /// with the verified certificate at startup.
    mesh_profile: Arc<crate::mesh::DeviceMeshProfile>,
    /// What the routes a page asks before its session answer with.
    web: Box<WebContext>,
    /// The workspaces this server governs; absent when it hosts no workspace authority.
    workspaces: Option<crate::workspaces::Workspaces>,
    /// What this host keeps for each workspace it serves.
    content: Arc<tilo_content::WorkspaceStores>,
    /// The links to its workspaces' files its web origin serves.
    #[cfg(feature = "web")]
    content_links: Arc<crate::content_links::ContentLinks>,
    /// This device's part in the channels of the workspaces it governs:
    /// absent when its certificate does not let it own workspaces, and so it
    /// holds no channel keys.
    workspace_keys: Option<Arc<tilo_workspace_service::own_device::KeyService>>,
    /// What puts its workspaces' questions about running commands unattended
    /// to people: the server's confirmations, held here so that its workspaces
    /// hold them only weakly and nothing of the server outlives it.
    _asks_people: Arc<crate::certification::CertificationProtocol>,
    /// The packet relay this host runs for the pages it serves, when it runs
    /// one ([`ServerConfig::packet_relay`]).
    iroh_packet_relay: Option<iroh_relay::server::Server>,
    /// The shells whose terminal streams this host's devices dial it for, once they run.
    #[cfg(feature = "agents")]
    terminal_shells: crate::agents::TerminalShells,
}

/// The server state the page's pre-session routes answer with.
#[derive(Clone)]
pub(crate) struct WebContext {
    pub(crate) server: ServerProtocol,
    #[cfg(feature = "web")]
    pub(crate) certification: crate::certification::CertificationProtocol,
    /// The device of the session beside this node, once named.
    pub(crate) node_session: Arc<std::sync::OnceLock<[u8; 32]>>,
    /// What this server was told at its start about Apple's push service,
    /// once named.
    #[cfg(feature = "agents")]
    pub(crate) closed_apps: Arc<std::sync::OnceLock<crate::apple_push::ApplePushSetup>>,
}

impl Server {
    #[cfg(feature = "web")]
    pub(crate) fn web_context(&self) -> &WebContext {
        &self.web
    }

    /// This host's channel to the person's approving devices: where a
    /// question about an action it runs for one of their agents is answered.
    #[cfg(feature = "web")]
    #[must_use]
    pub fn confirmations(&self) -> crate::certification::CertificationProtocol {
        self.web.certification.clone()
    }

    /// The stand-in certificate and key this host keeps for `user`, when it
    /// keeps a live one: what it approves for them with.
    #[cfg(feature = "web")]
    #[must_use]
    pub fn stand_in(
        &self,
        user: &[u8; 32],
    ) -> Option<(ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert)> {
        crate::web::stand_in_for(self.state_root(), user)
    }

    /// Name the device of the session beside this node (the shell that runs
    /// the person's apps), which pages address to open apps there. Named
    /// once; a later call keeps the first.
    pub fn set_node_session(&self, device: [u8; 32]) {
        let _ = self.web.node_session.set(device);
    }

    /// Name the key this server signs in to Apple's push service with and
    /// the apps it pushes for: what its people's closed apps are told
    /// through. Named once, before browsers are served; a later call keeps
    /// the first.
    #[cfg(feature = "agents")]
    pub fn set_apple_push(&self, setup: crate::apple_push::ApplePushSetup) {
        if self.web.closed_apps.set(setup).is_err() {
            tracing::warn!("the key for Apple's push service was named twice: the first is kept");
        }
    }
}

/// A tokio `JoinHandle` that aborts on drop — used so that dropping
/// `Server` tears down every outbound attach reconnect task.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Stop reconnect loops before closing the shared endpoint.  An
        // explicit close also tells connected peers about a host shutdown
        // immediately instead of making them wait for QUIC's idle timeout.
        if let Ok(mut tasks) = self.background_tasks.lock() {
            tasks.clear();
        }
        // `Server` is normally dropped inside Tokio, but Drop must remain safe
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
pub struct ServerHandle {
    /// Reachability info a peer can dial.
    pub node_addr: EndpointAddr,
    /// Metrics view — read-only.
    pub metrics: Arc<Metrics>,
    /// Effective device-mesh policy for routing and replication consumers.
    mesh_profile: Arc<crate::mesh::DeviceMeshProfile>,
}

/// Verify `cert`'s device's delegation against the person the agent acts for,
/// the root its certificate names, and put it in the server's agent register,
/// replacing an earlier one: what confines an agent to its delegated workspaces
/// from its next envelope on. Both paths that tell a server about an agent come
/// here ([`Server::register_agent`], `ServerAgentLink::register`), so neither
/// can register one the other leaves unconfined.
///
/// # Errors
///
/// The delegation is not the agent's person's, or names another device.
pub(crate) fn register_agent_in(
    agents: &crate::peer::AgentRegistry,
    cert: &tilo_user_identity::DeviceCert,
    delegation: &tilo_server_protocol::delegation::Delegation,
) -> Result<(), String> {
    delegation
        .verify(&cert.user_public_key, tilo_clock::unix_now_ns())
        .map_err(|error| error.to_string())?;
    if cert.device_public_key != delegation.device_public_key {
        return Err("the delegation names another device".into());
    }
    let mut agents = agents.write_recover();
    if matches!(agents.get(&delegation.device_public_key), Some(None)) {
        return Err("the agent device was withdrawn".into());
    }
    agents.insert(delegation.device_public_key, Some(delegation.clone()));
    drop(agents);
    // A replacement may narrow an earlier delegation's workspaces.
    crate::authority_epoch::bump();
    Ok(())
}

impl Server {
    /// Start on an explicitly selected packet relay without public address lookup.
    ///
    /// Device certificate validation, workspace membership and relay/1 framing
    /// remain identical to [`Self::start`]. The default constructor retains N0.
    ///
    /// # Errors
    /// Returns the same identity, storage and endpoint errors as [`Self::start`].
    pub async fn start_with_relay_mode(
        cfg: ServerConfig,
        identity: ServerIdentity,
        relay_mode: iroh::RelayMode,
    ) -> Result<Self, ServerError> {
        Box::pin(Self::start_inner(
            cfg,
            identity,
            Some(relay_mode.clone()),
            None,
            reaching_of(&relay_mode),
            Vec::new(),
            Servers::desktop(None),
        ))
        .await
    }

    /// Start server services on an already-bound endpoint and compose any
    /// number of caller-owned protocols into the same router. This is the
    /// production shell path: server state/action traffic, workspace joining,
    /// surfaces, and personal-fabric protocols retain one stable node address
    /// and one shutdown lifecycle.
    ///
    /// Ownership of `endpoint` transfers to the returned server. Calling
    /// [`Self::shutdown`] closes the router and invokes every additional
    /// handler's shutdown hook.
    ///
    /// `reaching` says whether that endpoint names where it is reached: the
    /// workspaces this server opens wait for that name, and an endpoint whose
    /// relays are off ([`Reaching::OnItsOwn`]) names none.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::ProtocolRegistration`] if an ALPN is empty,
    /// duplicates another additional protocol, or collides with a protocol
    /// owned by the server.
    pub async fn start_on_endpoint_with_protocols(
        cfg: ServerConfig,
        identity: ServerIdentity,
        endpoint: Endpoint,
        reaching: Reaching,
        protocols: Vec<(Vec<u8>, Box<dyn DynProtocolHandler>)>,
    ) -> Result<Self, ServerError> {
        Box::pin(Self::start_inner(
            cfg,
            identity,
            None,
            Some(endpoint),
            reaching,
            protocols,
            Servers::desktop(None),
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
    /// Returns [`ServerError::Identity`] if `identity` fails validation
    /// (bad signature, wrong chain, revoked, or not host-capable),
    /// [`ServerError::Io`] if the state root can't be created,
    /// [`ServerError::Files`] if the persistent iroh secret can't be
    /// loaded, and [`ServerError::Iroh`] if
    /// the listen address is invalid or the endpoint fails to bind.
    pub async fn start(cfg: ServerConfig, identity: ServerIdentity) -> Result<Self, ServerError> {
        Self::start_with_app_serving(cfg, identity, None).await
    }

    /// [`Server::start`], with an optional app action server that answers remote-action
    /// frames addressed to this host.
    ///
    /// The server links no apps; a process that hosts them passes an app action server
    /// here. Supplying one makes `app_serving` available, still subject to
    /// the configuration requesting it and the certificate authorising it.
    /// When the effective services leave `app_serving` off, the app action server is
    /// dropped and such frames are refused as undeliverable.
    ///
    /// # Errors
    ///
    /// The same as [`Server::start`].
    pub async fn start_with_app_serving(
        cfg: ServerConfig,
        identity: ServerIdentity,
        app_serving: Option<Arc<dyn crate::app_serving::AppActionServer>>,
    ) -> Result<Self, ServerError> {
        Box::pin(Self::start_inner(
            cfg,
            identity,
            None,
            None,
            Reaching::ByRelay,
            Vec::new(),
            Servers::desktop(app_serving),
        ))
        .await
    }

    /// Start an app-serving host with an explicit transport relay policy.
    ///
    /// # Errors
    /// The same as [`Server::start`].
    pub async fn start_with_app_serving_and_relay_mode(
        cfg: ServerConfig,
        identity: ServerIdentity,
        app_serving: Option<Arc<dyn crate::app_serving::AppActionServer>>,
        relay_mode: iroh::RelayMode,
    ) -> Result<Self, ServerError> {
        Box::pin(Self::start_inner(
            cfg,
            identity,
            Some(relay_mode.clone()),
            None,
            reaching_of(&relay_mode),
            Vec::new(),
            Servers::desktop(app_serving),
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
        mut cfg: ServerConfig,
        identity: ServerIdentity,
        relay_mode: Option<iroh::RelayMode>,
        endpoint: Option<Endpoint>,
        reaching: Reaching,
        additional_protocols: Vec<(Vec<u8>, Box<dyn DynProtocolHandler>)>,
        servers: Servers,
    ) -> Result<Self, ServerError> {
        let Servers {
            app_serving,
            shell_payloads,
        } = servers;
        // The host's one content store per workspace: the app action server's
        // when it brings one (its uploads and app workers keep there), so
        // everything this host keeps for a workspace is in one place.
        let content = app_serving
            .as_ref()
            .and_then(|server| server.content())
            .unwrap_or_else(|| crate::content::stores(&cfg.state_root));
        validate_additional_protocols(&additional_protocols)?;
        validate_server_identity(&identity)?;
        if cfg.work.takes_nothing() {
            return Err(ServerError::Identity(
                "a device that takes no work serves no workspace".into(),
            ));
        }
        // Live workspace control uses server's non-retained presence track, and app
        // serving needs an app action server supplied by an embedding process.
        let mut available_services = crate::mesh::ServiceCapabilities::relay_only();
        available_services.app_serving = app_serving.is_some();
        cfg.services = cfg.services.limited_to(&available_services);
        let app_serving = app_serving.filter(|_| cfg.services.app_serving);
        // Reject unavailable local backends before binding endpoints, publishing
        // capabilities or starting background tasks.
        let mesh_profile = Arc::new(crate::mesh::DeviceMeshProfile {
            device: identity.device_cert.device_public_key,
            services: cfg.services.clone(),
            filesystem_endpoints: Vec::new(),
        });
        std::fs::create_dir_all(&cfg.state_root)?;
        // The packet relay this server is reached through when it starts none of its own: one it
        // cannot reach leaves it reachable by nobody, which its start says rather than serving.
        let mut reached_through = None;
        let (packet_relay, relay_mode) = match cfg.packet_relay {
            Some(addr) if endpoint.is_none() && relay_mode.is_none() => {
                let (server, url) = spawn_packet_relay(addr).await?;
                let url = match &cfg.packet_relay_url {
                    Some(published) => published.parse().map_err(|error| {
                        ServerError::Iroh(format!("packet relay URL {published}: {error}"))
                    })?,
                    None => url,
                };
                tracing::info!(%addr, %url, "running this host's own packet relay");
                (Some(server), Some(iroh::RelayMode::Custom(url.into())))
            }
            Some(_) => {
                return Err(ServerError::Iroh(
                    "a host runs its own packet relay or is given one, not both".into(),
                ));
            }
            // Given a packet relay's URL and none of its own, the server is
            // reached through that one: a packet relay on the public internet
            // when the server sits behind a router nobody can dial into.
            None => match (&cfg.packet_relay_url, relay_mode) {
                (Some(published), None) if endpoint.is_none() => {
                    let url: iroh::RelayUrl = published.parse().map_err(|error| {
                        ServerError::Iroh(format!("packet relay URL {published}: {error}"))
                    })?;
                    tracing::info!(%url, "reached through another packet relay");
                    reached_through = Some(url.clone());
                    (None, Some(iroh::RelayMode::Custom(url.into())))
                }
                (_, relay_mode) => (None, relay_mode),
            },
        };
        let endpoint = match endpoint {
            Some(endpoint) => endpoint,
            None => bind_endpoint(&cfg, &identity.device_signing_key, relay_mode).await?,
        };
        // A certified host governs the workspaces it creates, unless its embedder
        // serves workspace joining itself, and only when its device may own
        // workspaces: governing one is holding its keys, so a device that may not
        // keeps no workspace. It answers their governance as actions itself, beside
        // whatever apps its embedder serves, and it runs the app transforms its
        // embedder gives it a job runner for.
        let governs_workspaces = !matches!(cfg.trust_policy, TrustPolicy::AllowAny)
            && identity.device_cert.capabilities.holds_workspace_keys()
            && !additional_protocols.iter().any(|(alpn, _)| {
                alpn.as_slice() == tilo_delivery_protocol::workspace_join::WORKSPACE_JOIN_ALPN
            });
        // A server that governs workspaces takes part in every channel of them, as
        // one key holder beside their members.
        let workspace_keys = governs_workspaces
            .then(|| {
                crate::workspace_keys::open(&cfg.state_root, identity.device_signing_key.clone())
            })
            .transpose()?
            .map(Arc::new);
        // Where this host is dialled — what each workspace's governance names of
        // it, and what a link it hands out carries — is the relay, which the
        // endpoint knows only once it has connected to one. The workspaces opened
        // below name it straight away, and a host that has none yet names it when
        // it makes its first, so wait for the relay before serving either.
        reaching.awaited(&endpoint).await;
        if let Some(url) = &reached_through
            && endpoint.addr().relay_urls().next().is_none()
        {
            return Err(ServerError::Iroh(format!(
                "the packet relay at {url}, which this server is to be reached through, did \
                 not answer: no one could reach the server. Check that the URL names a packet \
                 relay this computer reaches (tilo.sh's is https://live-eu1.tilo.sh)"
            )));
        }
        let (sessions, governed) = crate::service::ServiceSessions::open(
            crate::service::ServiceConfig {
                state_root: cfg.state_root.clone(),
                work: cfg.work.clone(),
                // A keeper that serves pages is a site, and goes by its
                // site's name among the holders of the workspaces it serves;
                // a server of someone's own goes by the name their admin
                // workspace gives it, applied below once it is open.
                label: cfg.web_entry.as_deref().map_or_else(String::new, site_name),
                rented: cfg.rented,
                trust_policy: cfg.trust_policy.clone(),
                max_workspace_grants: cfg.max_workspace_grants,
                reaching,
                // A server's endpoint keeps its relays from the start, so nothing is held back for
                // an invitation to wake.
                reachability: tilo_endpoint_reachability::Reachability::default(),
                apps: app_serving.clone(),
                shell_payloads,
                // A server serves other people's workspaces; it joins none as
                // a member.
                member: None,
                surfaces: std::sync::Arc::clone(collab_types::app_surface::Surfaces::process()),
            },
            identity.clone(),
            endpoint.clone(),
            governs_workspaces,
            workspace_keys
                .as_ref()
                .map(|workspace_keys| workspace_keys.groups()),
            |_workspaces, rest| rest,
        )
        .await?;
        let governed = governed.map(|governed| governed.workspaces);
        if let (Some(workspace_keys), Some(governed)) = (&workspace_keys, &governed) {
            workspace_keys.attach(governed);
        }
        // The links its invitations hand over open the server's web entry. A stack on this
        // machine alone (`TILO_WORKSPACES_LOCAL`) keeps its own site: its addresses are
        // loopback, which no public page dials.
        if let (Some(entry), Some(governed)) = (&cfg.web_entry, &governed) {
            let local = std::env::var_os("TILO_WORKSPACES_LOCAL").is_some();
            let _ = governed.invite_at(entry, local);
        }
        let workspaces = sessions.workspaces.clone();
        let metrics = sessions.metrics.clone();
        let live = sessions.identity.clone();
        let handler = sessions.handler.clone();
        // The live session belongs to the server: every key holder, a page
        // included, reaches it over the relay's own iroh endpoint, and all
        // land in one origin whether or not browsers are served.
        #[cfg(feature = "web")]
        let mut live_origin = Some(crate::web::live::LiveOrigin::start(handler.clone()));
        // A server enrolled into its person's identity: a host of theirs alone.
        let enrolled_server = matches!(cfg.trust_policy, TrustPolicy::AllowMatchingUser);
        let certification = crate::certification::CertificationProtocol::new(
            live.clone(),
            Some(workspaces.clone()),
        );
        let asks_people = Arc::new(certification.clone());
        if let Some(governed) = &governed {
            certification.keep_issued_in(governed.clone());
            // Whether a workspace of theirs runs commands unattended is the
            // person's to answer, on their approving devices. The workspaces
            // hold the confirmations weakly: they hold the workspaces.
            governed.ask_people(Arc::new(crate::certification::AsksThroughConfirmations(
                Arc::downgrade(&asks_people),
            )));
        }
        let web = Box::new(WebContext {
            server: handler.clone(),
            #[cfg(feature = "web")]
            certification: certification.clone(),
            node_session: Arc::default(),
            #[cfg(feature = "agents")]
            closed_apps: Arc::default(),
        });
        let mut router = Router::builder(endpoint.clone())
            .accept(ALPN, handler)
            .accept(crate::certify::CERTIFY_ALPN, certification);
        if let Some(governed) = &governed {
            for (alpn, handler) in crate::service::workspaces_protocols(&sessions, governed) {
                router = router.accept(alpn, handler);
            }
        }
        for (alpn, handler) in additional_protocols {
            router = router.accept(alpn, handler);
        }
        // A person's device takes the terminal of a hosted workspace's session here, once
        // the host's shells are told where they run.
        #[cfg(feature = "agents")]
        let terminal_shells = crate::agents::TerminalShells::default();
        #[cfg(feature = "agents")]
        {
            router = router.accept(ush::environment_terminals::ALPN, terminal_shells.route());
        }
        #[cfg(feature = "web")]
        if let Some(origin) = live_origin.as_ref() {
            let (alpn, handler) = origin.protocol();
            router = router.accept(alpn, handler);
        }
        let router = router.spawn();

        let background_tasks = spawn_background_tasks(&cfg, &identity, &metrics, &live);
        #[cfg(feature = "web")]
        if let Some(origin) = live_origin.as_mut() {
            let mut tasks = lock_background_tasks(&background_tasks);
            tasks.extend(origin.tasks().into_iter().map(AbortOnDrop));
        }
        // Members read what this host keeps for their workspace from its
        // content, and the files of the workspaces it governs are kept there
        // so a member that comes up after the one that wrote them left reads
        // the same bytes from this host alone. What no record of a workspace
        // names any more is let go of, so replaced files do not pile up.
        if let Some(governed) = &governed {
            // A server of the person's own takes part in their Servers
            // workspace, whose record of their servers says what it should be.
            if enrolled_server {
                // One that has the workspace keeps its entry's address current.
                // One that has it not yet is handed it by a device that has it,
                // once the ticket it came in with entered it in the document.
                if crate::servers::holds_person_workspace(governed).await {
                    crate::servers::enroll(governed, &endpoint.addr())
                        .await
                        .map_err(|error| {
                            ServerError::Identity(format!("Servers workspace: {error}"))
                        })?;
                }
                // Its OS updates wait their turn among the person's servers.
                let rollout = crate::rollout::serve(crate::rollout::ServerMachine::new(
                    governed.clone(),
                    cfg.state_root.clone(),
                ))?;
                push_background_task(&background_tasks, rollout);
            }
            // What this device is, and what the person's servers are told to
            // be, is its record in the Servers workspace's document: from the
            // moment it has the workspace it applies the document, after each
            // change to it, and finishes the installation its plan describes if
            // it came in from one.
            push_background_task(
                &background_tasks,
                tokio::spawn(install_and_follow(
                    governed.clone(),
                    endpoint.clone(),
                    cfg.state_root.clone(),
                    cfg.identity_root.clone(),
                    identity.device_cert.clone(),
                )),
            );
            // A server's name is its entry's in its person's admin workspace:
            // the workspaces it holds show it by that name from now on.
            if let Some(label) =
                crate::servers::name(governed, &identity.device_cert.device_public_key).await
            {
                governed.name_holder(&label).await;
            }
            governed.serve_content(Arc::new(crate::content::StoresContent(Arc::clone(
                &content,
            ))));
            // A branch of a workspace takes the content its records name.
            governed.branch_content(Arc::new(crate::content::branch::StoresBranches(
                Arc::clone(&content),
            )));
            let kept = crate::kept_content::KeptContent::new(
                governed.clone(),
                identity.device_cert.device_public_key,
                Arc::clone(&content),
            );
            push_background_task(&background_tasks, tokio::spawn(kept.keep_content()));
            push_background_task(
                &background_tasks,
                tokio::spawn(crate::content::collection::run(
                    governed.clone(),
                    Arc::clone(&content),
                )),
            );
            // A workspace its owner deleted takes its content and its app
            // workers with it.
            push_background_task(
                &background_tasks,
                tokio::spawn(crate::content::forget_deleted(
                    governed.clone(),
                    Arc::clone(&content),
                    app_serving,
                )),
            );
        }
        // The host is a member of every workspace it holds: its own device is on
        // those rosters, so it reads and serves them as any key holder does.
        for workspace in workspaces.lock().await.values() {
            workspace
                .lock()
                .await
                .admit_device(identity.device_cert.device_public_key)?;
        }
        Ok(Self {
            endpoint,
            router,
            sessions,
            #[cfg(feature = "web")]
            browser_origin: Arc::new(std::sync::RwLock::new(None)),
            #[cfg(feature = "web")]
            live_origin,
            background_tasks,
            mesh_profile,
            web,
            workspaces: governed,
            content,
            #[cfg(feature = "web")]
            content_links: Arc::default(),
            workspace_keys,
            _asks_people: asks_people,
            iroh_packet_relay: packet_relay,
            #[cfg(feature = "agents")]
            terminal_shells,
        })
    }

    /// The relay this server's live session runs on: what a part of the host
    /// that takes part in a workspace's calls joins, as the server's own device.
    #[cfg(feature = "web")]
    #[must_use]
    pub fn live_relay(&self) -> Option<tilo_live::relay::Relay> {
        self.live_origin
            .as_ref()
            .map(crate::web::live::LiveOrigin::origin)
    }

    /// The shells whose terminal streams this host serves, once they run.
    #[cfg(feature = "agents")]
    pub(crate) const fn terminal_shells(&self) -> &crate::agents::TerminalShells {
        &self.terminal_shells
    }

    /// Whether this server currently counts `device` as subscribed to
    /// `workspace_id`.
    ///
    /// This is the server's own belief, which is what routing acts on: a
    /// device is here from the moment its `Subscribe` is accepted until
    /// the connection closes, and a delta published in between is fanned
    /// out to it rather than queued. There is no way to learn a peer has
    /// gone before its close arrives, so anything that depends on the
    /// departure having been noticed has to ask.
    pub async fn is_subscribed(&self, workspace_id: WorkspaceId, device: &[u8; 32]) -> bool {
        self.sessions.is_subscribed(workspace_id, device).await
    }

    /// Stop the server and tell every peer it has gone.
    ///
    /// Dropping the handle also stops it, but abandons the QUIC
    /// connections rather than closing them: each peer keeps this server
    /// as a live subscriber until its own idle timeout, and for that
    /// whole window fans out to a connection nobody reads instead of
    /// queueing for store-and-forward. Deltas in that window are neither
    /// delivered nor queued. Closing the endpoint sends `CONNECTION_CLOSE`
    /// straight away, so peers drop the subscription and queue instead.
    ///
    /// What its workspaces took in of their members' writes is written
    /// through to their files first
    /// ([`crate::workspaces::Workspaces::keep_what_was_followed`]): that
    /// write-through is deferred, so a server stopped without it would start
    /// again on documents that lack what was written last, and owe a turn its
    /// agent had already answered.
    pub async fn shutdown(mut self) {
        // Stop reconnecting first: the dial loop would otherwise open a
        // fresh connection while the endpoint is closing.
        lock_background_tasks(&self.background_tasks).clear();
        if let Some(workspaces) = &self.workspaces {
            workspaces.keep_what_was_followed().await;
        }
        if let Err(error) = self.router.shutdown().await {
            tracing::debug!(%error, "server router shutdown task failed");
        }
        self.sessions.shutdown();
        if let Some(packet_relay) = std::mem::take(&mut self.iroh_packet_relay)
            && let Err(error) = packet_relay.shutdown().await
        {
            tracing::debug!(%error, "packet relay shutdown task failed");
        }
    }

    /// Cheap external handle.
    pub fn handle(&self) -> ServerHandle {
        ServerHandle {
            node_addr: self.endpoint.addr(),
            metrics: self.sessions.metrics.clone(),
            mesh_profile: self.mesh_profile.clone(),
        }
    }

    /// Tell the host that the device `cert` certifies is an agent: verify
    /// its root-signed delegation against the person it acts for and confine the
    /// device to the delegated workspaces from its next envelope on, then put it
    /// on the roster of each workspace this server governs that the delegation
    /// names, as an approved grant does ([`crate::workspaces::Workspaces::reconcile_agent`]).
    /// Once this returns, those workspaces admit it. Replaces an earlier
    /// delegation of the same device.
    ///
    /// # Errors
    ///
    /// The delegation is not the agent's person's, names another device, or
    /// does not verify.
    #[cfg_attr(
        not(feature = "agents"),
        allow(
            clippy::unused_async,
            reason = "only a build with agents keeps them on governed rosters, which it awaits"
        )
    )]
    pub async fn register_agent(
        &self,
        cert: &tilo_user_identity::DeviceCert,
        delegation: tilo_server_protocol::delegation::Delegation,
    ) -> Result<(), String> {
        register_agent_in(&self.web.server.agents, cert, &delegation)?;
        // The governed workspaces it names put it on their rosters.
        #[cfg(feature = "agents")]
        if let Some(workspaces) = self.workspaces() {
            workspaces
                .reconcile_agent(cert.device_public_key, Some(cert))
                .await;
        }
        Ok(())
    }

    /// This device's key service: the one place the device holds in each key group of the
    /// workspaces it governs, which every session of this device reads and asks with. `None` when
    /// its certificate does not let it own workspaces.
    #[must_use]
    pub fn workspace_keys(&self) -> Option<tilo_workspace_keys::SharedKeys> {
        self.workspace_keys.as_ref()?.keys().get()
    }

    /// The iroh endpoint address (node id + reachable addresses) —
    /// what a peer needs to dial this server.
    pub fn node_addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// The iroh endpoint this server's device dials from.
    #[must_use]
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }

    /// The local sockets the server's endpoint is bound to.
    #[must_use]
    pub fn bound_sockets(&self) -> Vec<std::net::SocketAddr> {
        self.endpoint.bound_sockets()
    }

    /// Where the app action server hands a frame to a device of one of its workspaces after
    /// answering its caller itself (a page action the person allowed, running
    /// on the window that refused it): this host's own delivery, stamped as
    /// every frame it forwards is.
    ///
    /// # Panics
    ///
    /// Outside a runtime, where there is nothing to deliver on.
    #[must_use]
    pub fn hand(&self) -> crate::app_serving::Hand {
        crate::app_serving::Hand::new(
            self.sessions.workspaces.clone(),
            tokio::runtime::Handle::current(),
        )
    }

    /// The workspaces this server governs, when it hosts the workspace authority.
    #[must_use]
    pub const fn workspaces(&self) -> Option<&crate::workspaces::Workspaces> {
        self.workspaces.as_ref()
    }

    /// The devices this server certified for its browser environments, and the
    /// one record each reaches ([`crate::environment`]): what the server
    /// confines each environment to when it places it, and what every app action server of
    /// its workspaces asks before it lets one in.
    #[must_use]
    pub fn environments(&self) -> crate::environment::EnvironmentRegistry {
        self.sessions.handler.environments.clone()
    }

    /// What this host keeps for each workspace it serves: its one content
    /// store per workspace.
    #[must_use]
    pub fn content(&self) -> Arc<tilo_content::WorkspaceStores> {
        Arc::clone(&self.content)
    }

    /// The links to its workspaces' files this host's web origin serves.
    #[cfg(feature = "web")]
    #[must_use]
    pub fn content_links(&self) -> Arc<crate::content_links::ContentLinks> {
        Arc::clone(&self.content_links)
    }

    /// Where this server keeps its state.
    #[cfg(feature = "web")]
    pub(crate) fn state_root(&self) -> &std::path::Path {
        &self.sessions.state_root
    }

    /// Own user identity — for tests.
    pub fn own_user_public_key(&self) -> [u8; 32] {
        self.sessions.identity.user_public_key
    }

    /// In-process access to the records of the workspaces this server hosts,
    /// for a service embedded in the host process.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "records are read only where the server keeps workspaces"
    )]
    pub fn workspace_documents(&self) -> crate::workspace_documents::WorkspaceDocuments {
        self.workspaces()
            .expect("records are read only where the server keeps workspaces")
            .workspace_documents()
    }

    /// This server's own device key.
    #[must_use]
    pub fn device_signing_key(&self) -> &ed25519_dalek::SigningKey {
        &self.sessions.identity.signing_key
    }
}

/// The site name a keeper's web entry names: `tilo.sh` for
/// `https://tilo.sh/app/`. One without a web entry is no site and goes by its
/// server's name alone.
fn site_name(web_entry: &str) -> String {
    let rest = web_entry
        .split_once("://")
        .map_or(web_entry, |(_, rest)| rest);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// From the moment this device has its person's Servers workspace, however
/// it came to (it had it at start, or a device that has it handed it over),
/// apply the workspace's document, and finish the installation its plan
/// describes if this machine came in from one.
async fn install_and_follow(
    workspaces: crate::workspaces::Workspaces,
    endpoint: Endpoint,
    state_root: PathBuf,
    identity_root: PathBuf,
    cert: tilo_user_identity::DeviceCert,
) {
    use tokio::sync::broadcast::error::RecvError;
    let mut listed = workspaces.listing_changes();
    let mut governed = workspaces.governance_changes();
    while !crate::servers::holds_person_workspace(&workspaces).await {
        // A slow receiver re-reads; a closed one means the service is gone.
        let changed = tokio::select! {
            change = listed.recv() => change.map(drop),
            change = governed.recv() => change.map(drop),
        };
        if changed == Err(RecvError::Closed) {
            return;
        }
    }
    // The document is applied while the installation waits on the person's
    // answers.
    let following = crate::servers::applied::follow(
        &workspaces,
        crate::servers::applied::Requests::default(),
        Some(endpoint),
        state_root,
    );
    if crate::servers::first_install::joined(&identity_root) {
        match crate::servers::first_install::install(
            &workspaces,
            &cert,
            &crate::servers::first_install::real_drives(),
            &crate::servers::applied::Requests::default(),
        )
        .await
        {
            Ok(()) => tracing::info!("this machine installed from its plan"),
            Err(error) => tracing::warn!(%error, "this machine did not install from its plan"),
        }
    }
    if let Err(error) = following.await {
        tracing::warn!(%error, "the Servers workspace's document is no longer applied here");
    }
}

impl ServerHandle {
    /// The services this server offers: its configured ones, limited to what it started.
    pub fn advertised_services(&self) -> crate::mesh::ServiceCapabilities {
        self.mesh_profile.services.clone()
    }

    /// Current profile safe to publish to placement/discovery consumers.
    pub fn mesh_profile(&self) -> crate::mesh::DeviceMeshProfile {
        (*self.mesh_profile).clone()
    }
}

/// Derive the server transport key from the enrolled device secret. The server
/// application handshake still proves possession of the certified device key;
/// this domain-separated transport key prevents the server and filesystem
/// processes from publishing competing address records for one Iroh `NodeId`.
pub fn server_node_id(device_key: &ed25519_dalek::SigningKey) -> [u8; 32] {
    tilo_server_protocol::server_node_id(&device_key.to_bytes())
}

/// Run this host's own iroh packet relay on `addr`: the relay, and the URL
/// its peers dial. A page has no UDP socket, so it reaches this host only
/// through a relay, and the host advertises this one in its signed address.
async fn spawn_packet_relay(
    addr: std::net::SocketAddr,
) -> Result<(iroh_relay::server::Server, iroh_base::RelayUrl), ServerError> {
    let mut config = iroh_relay::server::ServerConfig::default();
    config.relay = Some(iroh_relay::server::RelayConfig::new(addr));
    let server = iroh_relay::server::Server::spawn(config)
        .await
        .map_err(|error| ServerError::Iroh(format!("packet relay: {error}")))?;
    let serving = server
        .http_addr()
        .ok_or_else(|| ServerError::Iroh("the packet relay serves no address".into()))?;
    let url: iroh_base::RelayUrl = format!("http://{serving}")
        .parse()
        .map_err(|error| ServerError::Iroh(format!("packet relay URL: {error}")))?;
    Ok((server, url))
}

/// Whether an endpoint bound with `relay_mode` names where it is reached: one
/// told to use no relay never does, and everything else is expected to.
fn reaching_of(relay_mode: &iroh::RelayMode) -> Reaching {
    if matches!(relay_mode, iroh::RelayMode::Disabled) {
        Reaching::OnItsOwn
    } else {
        Reaching::ByRelay
    }
}

/// Bind the server protocol with a domain-separated transport key. This is one
/// physical certified device, not a second user identity: protocol auth uses
/// `device_key` and its device certificate after QUIC connects.
async fn bind_endpoint(
    cfg: &ServerConfig,
    device_key: &ed25519_dalek::SigningKey,
    relay_mode: Option<iroh::RelayMode>,
) -> Result<Endpoint, ServerError> {
    let secret = iroh::SecretKey::from_bytes(&tilo_server_protocol::server_node_secret(
        &device_key.to_bytes(),
    ));
    let builder = match relay_mode {
        Some(mode) => Endpoint::builder(presets::Minimal).relay_mode(mode),
        None => Endpoint::builder(presets::N0),
    };
    let builder = builder.transport_config(tilo_server_client::host::transport());
    let builder = if cfg.asks_router_for_port() {
        builder
    } else {
        builder.portmapper_config(iroh::endpoint::PortmapperConfig::Disabled)
    };
    builder
        .secret_key(secret)
        .bind_addr(cfg.iroh_listen)
        .map_err(|e| ServerError::Iroh(format!("invalid iroh listen address: {e}")))?
        .bind()
        .await
        .map_err(|e| ServerError::Iroh(e.to_string()))
}

fn validate_additional_alpn(alpn: &[u8]) -> Result<(), ServerError> {
    let reserved: &[&[u8]] = &[ALPN];
    if alpn.is_empty() {
        return Err(ServerError::ProtocolRegistration(
            "additional ALPN must not be empty".into(),
        ));
    }
    if reserved.contains(&alpn) {
        return Err(ServerError::ProtocolRegistration(format!(
            "{} is reserved by tilo-server",
            String::from_utf8_lossy(alpn)
        )));
    }
    Ok(())
}

fn validate_additional_protocols(
    protocols: &[(Vec<u8>, Box<dyn DynProtocolHandler>)],
) -> Result<(), ServerError> {
    validate_additional_alpns(protocols.iter().map(|(alpn, _)| alpn.as_slice()))
}

fn validate_additional_alpns<'a>(
    alpns: impl IntoIterator<Item = &'a [u8]>,
) -> Result<(), ServerError> {
    let mut seen = HashSet::new();
    for alpn in alpns {
        validate_additional_alpn(alpn)?;
        if !seen.insert(alpn) {
            return Err(ServerError::ProtocolRegistration(format!(
                "{} is registered more than once",
                String::from_utf8_lossy(alpn)
            )));
        }
    }
    Ok(())
}

/// Spawn the revocation-refresh loop and the metrics-persist loop,
/// returning the abort-on-drop handle list `Server` owns.
fn spawn_background_tasks(
    cfg: &ServerConfig,
    identity: &ServerIdentity,
    metrics: &Arc<Metrics>,
    live: &LiveIdentity,
) -> Arc<std::sync::Mutex<Vec<AbortOnDrop>>> {
    let background_tasks: Arc<std::sync::Mutex<Vec<AbortOnDrop>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    let revocation_task = spawn_revocation_refresh_task(
        cfg.state_root.clone(),
        identity.revocations.clone(),
        live.revocations.clone(),
    );
    push_background_task(&background_tasks, revocation_task);

    let metrics_task = spawn_metrics_persist_task(cfg.state_root.clone(), metrics.clone());
    push_background_task(&background_tasks, metrics_task);
    background_tasks
}

/// How often the revocation log is re-read while its directory cannot be
/// watched: the cadence `tilo-server-admin` promises ("within one second").
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
                || change.path.as_str() == crate::workspace_files::REVOCATIONS_FILE
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
        let Ok(entries) = crate::workspace_files::load_revocations(&self.state_root) else {
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
            if let Err(error) =
                crate::workspace_files::save_metrics(&state_root, &live_metrics.snapshot())
            {
                tracing::warn!(%error, "could not persist server metrics snapshot");
            }
        }
    })
}

/// Lock the attach task list, recovering the inner `Vec` if the
/// mutex was poisoned by an earlier panic rather than propagating one
/// here — losing track of a reconnect task would leak it past the
/// owning `Server`'s lifetime.
fn lock_background_tasks(
    tasks: &Arc<std::sync::Mutex<Vec<AbortOnDrop>>>,
) -> std::sync::MutexGuard<'_, Vec<AbortOnDrop>> {
    tasks.lock_recover()
}

/// Register a spawned task on the attach task list (see
/// [`lock_background_tasks`]).
fn push_background_task(tasks: &Arc<std::sync::Mutex<Vec<AbortOnDrop>>>, task: JoinHandle<()>) {
    lock_background_tasks(tasks).push(AbortOnDrop(task));
}

fn validate_server_identity(identity: &ServerIdentity) -> Result<(), ServerError> {
    crate::service::validate_service_device_identity(identity)?;
    if !identity.device_cert.capabilities.can_own_workspaces {
        return Err(ServerError::Identity(
            "the certificate is not a device of the person".into(),
        ));
    }
    Ok(())
}

/// The server frame codec, re-exported under the daemon path older callers use.
pub use crate::frame::{read_frame, write_frame};

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
mod tests {
    use super::*;
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, DeviceIdentity, UserIdentity, sign_device_cert,
    };

    fn identity(capabilities: DeviceCapabilities) -> ServerIdentity {
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
            issuer: None,
        };
        sign_device_cert(&mut device_cert, &root).unwrap();
        ServerIdentity {
            user_public_key: user.public_key,
            device_signing_key,
            device_cert,
            revocations: RevocationList::new(),
        }
    }

    tilo_test_env::scenario! {
        fn startup_validation_and_workspace_authority_admit_only_signed_hosts;

        phase valid_host_identity_passes_startup_validation;
        {
            validate_server_identity(&identity(DeviceCapabilities::member())).unwrap();
        }

        phase an_agent_grant_cannot_start_a_server;
        {
            let error =
                validate_server_identity(&identity(DeviceCapabilities::nothing())).unwrap_err();
            assert!(error.to_string().contains("not a device"));
        }

        phase mismatched_device_key_is_rejected_at_startup;
        {
            let mut server = identity(DeviceCapabilities::member());
            server.device_signing_key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
            let error = validate_server_identity(&server).unwrap_err();
            assert!(error.to_string().contains("does not match"));
        }
    }

    /// The packet relay sits behind a front that pools its connections (a
    /// Caddy, as tilo.sh and a local stack run it), and every iroh endpoint
    /// homed on it probes its latency there now and then. The relay keeps a
    /// connection only once it is a relay WebSocket, aborting any other 30 s
    /// after it opened, so a probe's connection the front kept for its next
    /// request was aborted under it, logged as a connection that "did not
    /// reach established state", every half minute or so. The relay now
    /// closes a probe's connection behind its answer: the front pools nothing
    /// for it to abort.
    #[tokio::test]
    async fn a_probe_leaves_the_packet_relay_no_connection_to_abort() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let (relay, _url) = spawn_packet_relay((std::net::Ipv4Addr::LOCALHOST, 0).into())
            .await
            .unwrap();
        let addr = relay.http_addr().unwrap();
        let mut front = tokio::net::TcpStream::connect(addr).await.unwrap();
        front
            .write_all(
                format!(
                    "GET {} HTTP/1.1\r\n{}: {addr}\r\nConnection: keep-alive\r\n\r\n",
                    iroh_relay::http::RELAY_PROBE_PATH,
                    hyper::header::HOST,
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        // The answer, and then the end of the connection: well before the
        // 30 s at which an open one would be aborted.
        let mut answer = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), front.read_to_end(&mut answer))
            .await
            .expect("the relay closes a probe's connection once it answered")
            .unwrap();
        let answer = String::from_utf8_lossy(&answer).to_ascii_lowercase();
        assert!(answer.starts_with("http/1.1 200"), "{answer}");
        assert!(answer.contains("connection: close"), "{answer}");

        let metrics = &relay.metrics().server;
        tokio::time::timeout(Duration::from_secs(5), async {
            while metrics.http_connections_closed.get() < 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the relay counts the probe's connection closed");
        assert_eq!(
            metrics.http_connections_errored.get(),
            0,
            "a probe's connection ends cleanly"
        );
        relay.shutdown().await.unwrap();
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the end-to-end discovery regression keeps host, client, and filesystem assertions in one readable protocol scenario"
    )]
    async fn advertised_endpoints_are_the_hosted_services_own() {
        // Phase `composite_server_clamps_only_unhosted_service_advertisements`.
        {
            let state = tempfile::tempdir().unwrap();
            let server = Server::start(
                ServerConfig {
                    state_root: state.path().to_path_buf(),
                    services: crate::mesh::ServiceCapabilities::personal_server(),
                    ..ServerConfig::default()
                }
                .on_loopback(),
                identity(DeviceCapabilities::member()),
            )
            .await
            .unwrap();

            let services = server.handle().mesh_profile().services;
            assert!(services.workspace_replication);
            assert!(services.inbound_server);
            assert!(!services.app_serving);
            assert!(!services.compute);
            assert!(services.remote_control_server);
            server.shutdown().await;
        }
        // Phase `an_enrolled_servers_updates_take_turns_among_its_persons_servers`.
        {
            let temp = tempfile::tempdir().unwrap();
            let (root, user) = UserIdentity::generate("owner", 1);
            let (host_key, host) = DeviceIdentity::generate("home", 2);
            let (_, client) = DeviceIdentity::generate("laptop", 3);
            let (admin_key, admin) = DeviceIdentity::generate("primary", 5);
            let mut server_cert = DeviceCert {
                user_public_key: user.public_key,
                device_public_key: host.public_key,
                device_label: host.label,
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 4,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            let mut client_cert = DeviceCert {
                user_public_key: user.public_key,
                device_public_key: client.public_key,
                device_label: client.label,
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 4,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            let mut admin_cert = DeviceCert {
                user_public_key: user.public_key,
                device_public_key: admin.public_key,
                device_label: admin.label,
                capabilities: DeviceCapabilities::primary(),
                issued_at_ns: 4,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            sign_device_cert(&mut server_cert, &root).unwrap();
            sign_device_cert(&mut client_cert, &root).unwrap();
            sign_device_cert(&mut admin_cert, &root).unwrap();
            // The workspace this server holds, and the one it does not.
            let given = WorkspaceId::from_u128(7);
            let server = Server::start(
                ServerConfig {
                    state_root: temp.path().join("server"),
                    trust_policy: TrustPolicy::AllowMatchingUser,
                    services: crate::mesh::ServiceCapabilities::relay_only(),
                    // The relay runs its own packet relay on this machine's loopback, as the
                    // tests' hosts do: nothing here reaches number 0.
                    packet_relay: Some((std::net::Ipv4Addr::LOCALHOST, 0).into()),
                    ..ServerConfig::default()
                }
                .on_loopback(),
                ServerIdentity {
                    user_public_key: user.public_key,
                    device_signing_key: host_key,
                    device_cert: server_cert.clone(),
                    revocations: RevocationList::new(),
                },
            )
            .await
            .unwrap();
            let server_workspaces = server.workspaces().expect("the server holds workspaces");
            // The person's primary device makes their Servers workspace, enters
            // this server among their servers there and hands it the workspace,
            // as it hands every server of theirs: no server founds it.
            let primary_state = tempfile::tempdir().unwrap();
            let primary = Server::start(
                ServerConfig {
                    state_root: primary_state.path().to_path_buf(),
                    services: crate::mesh::ServiceCapabilities::personal_server(),
                    ..ServerConfig::default()
                }
                .on_loopback(),
                ServerIdentity {
                    user_public_key: user.public_key,
                    device_signing_key: admin_key,
                    device_cert: admin_cert.clone(),
                    revocations: RevocationList::new(),
                },
            )
            .await
            .unwrap();
            let primary_workspaces = primary.workspaces().expect("the device holds workspaces");
            crate::servers::found(primary_workspaces).await.unwrap();
            crate::servers::admit(primary_workspaces, &server_cert, &server.node_addr())
                .await
                .unwrap();
            crate::servers::take_in(primary_workspaces, &server_cert, &server.node_addr())
                .await
                .unwrap();
            tilo_test_env::within(
                "the server to hold its entry in the Servers workspace",
                async {
                    while !crate::servers::read(server_workspaces)
                        .await
                        .is_ok_and(|servers| servers.active(&host.public_key).is_some())
                    {
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                },
            )
            .await;
            // The workspace enters the server the way production does it: the
            // person makes it on their server and their device enters it.
            let server_workspace = given.to_string();
            server_workspaces
                .create_workspace("Workspace", Some(&server_workspace))
                .await
                .unwrap();
            server_workspaces
                .enter(&server_workspace, &client_cert)
                .await
                .unwrap();
            let governed = server.workspaces().unwrap();
            // Its OS update waits its turn in that admin workspace: the
            // update service asks on the server's socket, the server takes
            // the update lease and drains before it answers that the machine
            // may restart, and lets the lease go once the update booted.
            let state_root = temp.path().join("server");
            let ask = |request| {
                let state_root = state_root.clone();
                tokio::task::spawn_blocking(move || crate::rollout::ask(&state_root, &request))
            };
            assert_eq!(
                ask(crate::rollout::Request::Prepare {
                    target: "sha256:next".into()
                })
                .await
                .unwrap()
                .unwrap(),
                crate::rollout::Answer::Restart
            );
            let admin = crate::servers::person_workspace_id(&user.public_key);
            let lease = |state: tilo_peer_state::StatePeer| {
                state
                    .workspace()
                    .lease(crate::rollout::UPDATE_LEASE, std::time::SystemTime::now())
                    .holder
                    .map(|holder| holder.device)
            };
            assert_eq!(
                lease(governed.state(&admin).await.unwrap()),
                Some(governed.serving_certificate().device_public_key)
            );
            assert!(
                governed.draining(),
                "it takes no work under leases any more"
            );
            assert_eq!(
                ask(crate::rollout::Request::Booted {
                    image: "sha256:next".into()
                })
                .await
                .unwrap()
                .unwrap(),
                crate::rollout::Answer::Released
            );
            assert_eq!(lease(governed.state(&admin).await.unwrap()), None);
            // After the healthy boot its rollback floor may move: it asks the
            // person's other devices in the admin workspace, says which key its
            // floor takes, and never answers its request itself.
            let secret_sync = tilo_seal::SecretKey::from_bytes([3; 32]);
            let made =
                crate::servers::floor::make_key(&root, &secret_sync.public_key().unwrap()).unwrap();
            crate::servers::floor::apply(governed, made.action)
                .await
                .unwrap();
            let floor_request = tilo_rollback::approval::build_request(
                0x01D9_1100,
                &[0, 11, 1],
                tilo_rollback::counter::FloorState {
                    counter: 9,
                    base: 4,
                },
                &[5, 6],
                3,
            )
            .unwrap();
            let asked = ask(crate::rollout::Request::Floor(
                crate::servers::floor::Asked {
                    generation: 6,
                    image: "sha256:next".into(),
                    updates: "stable".into(),
                    authority: made.public.clone(),
                    request: serde_json::to_string(&floor_request).unwrap(),
                },
            ))
            .await
            .unwrap()
            .unwrap();
            assert!(
                matches!(&asked, crate::rollout::Answer::Floor { approvals, why }
                    if approvals.is_empty() && why.contains("waiting")),
                "{asked:?}"
            );
            assert_eq!(
                crate::servers::read(governed)
                    .await
                    .unwrap()
                    .active(&host.public_key)
                    .unwrap()
                    .floor_key,
                made.public
            );
            assert_eq!(
                crate::servers::floor::answer(governed, &secret_sync)
                    .await
                    .unwrap(),
                0,
                "a server never answers its own floor request"
            );
            let own = crate::servers::device_id(&host.public_key);
            let id = crate::servers::floor::request_id(own.as_str(), 6);
            assert!(
                crate::servers::floor::apply(
                    governed,
                    crate::servers::floor::Floor::APPROVE_FLOOR_ADVANCE.with((
                        id.clone(),
                        own.as_str().to_owned(),
                        "[]".into()
                    )),
                )
                .await
                .is_err(),
                "nor may it approve it by hand"
            );
            let pending = crate::servers::floor::read(governed).await.unwrap();
            assert_eq!(
                pending.requests.get(&id).unwrap().answer,
                "",
                "it stays pending"
            );
            server.shutdown().await;
        }
    }

    #[test]
    fn additional_protocol_rejects_server_owned_alpns() {
        assert!(matches!(
            validate_additional_alpn(ALPN),
            Err(ServerError::ProtocolRegistration(_))
        ));
        validate_additional_alpn(b"tilo-workspace-join/1").unwrap();
        assert!(matches!(
            validate_additional_alpns([
                b"tilo-workspace-join/1".as_slice(),
                b"tilo-workspace-join/1".as_slice(),
            ]),
            Err(ServerError::ProtocolRegistration(_))
        ));
    }
}
