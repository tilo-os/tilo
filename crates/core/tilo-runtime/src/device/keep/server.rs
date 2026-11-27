//! The long-running daemon: binds an iroh endpoint, and serves its workspaces'
//! links there and over WebTransport.
//!
//! A device's daemon keeps more on its own endpoint and workspace service
//! ([`Server::on_device`]); [`KeptServer`] starts that composition in a test, so
//! tests run two servers in one process and let them merge.

use std::path::PathBuf;
use std::sync::Arc;

use iroh::endpoint::{Endpoint, presets};
use iroh::protocol::DynProtocolHandler;
#[cfg(any(test, feature = "test-support"))]
use iroh::protocol::Router;
use iroh_base::EndpointAddr;
use tokio::task::JoinHandle;

use tilo_server_protocol::config::ServerConfig;

use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::policy::TrustPolicy;
use tilo_workspace_service::Reaching;
use tilo_workspace_service::service::ServerProtocol;

/// Failure modes for a server's start.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// Iroh endpoint bind / accept.
    #[error("iroh: {0}")]
    Iroh(String),
    /// The host's files beside the device store.
    #[error("files: {0}")]
    Files(#[from] tilo_workspace_service::workspace_files::FilesError),
    /// Config load / save.
    #[error("config: {0}")]
    Config(#[from] tilo_server_protocol::config::ConfigError),
    /// I/O — bubbled up from filesystem ops during startup.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Identity file missing / malformed.
    #[error("identity: {0}")]
    Identity(String),
    /// Member-list admission could not be reconciled safely.
    #[error("member list: {0}")]
    MemberListAccess(String),
}

impl From<tilo_workspace_service::service::ServiceError> for ServerError {
    fn from(error: tilo_workspace_service::service::ServiceError) -> Self {
        use tilo_workspace_service::service::ServiceError;
        match error {
            ServiceError::Identity(reason) => Self::Identity(reason),
            ServiceError::Files(error) => Self::Files(error),
            ServiceError::Io(error) => Self::Io(error),
            ServiceError::MemberListAccess(reason) => Self::MemberListAccess(reason),
            ServiceError::Workspaces(reason) => Self::Identity(format!("workspaces: {reason}")),
            ServiceError::Handshake(reason) => Self::Iroh(reason),
        }
    }
}

/// The identity a server runs with: a host's.
pub use tilo_workspace_service::service::ServiceIdentity as ServerIdentity;

/// `identity` with the root-signed revocations persisted at `state_root`
/// applied, each verified: what a host reads when it starts, so a device its
/// root revoked is refused as it connects instead of only after it is in.
///
/// The daemon applies the log itself before it opens the service; a
/// daemon that hands [`Server::on_device`] an identity it built reads it with
/// this first, so what it validates and what the server keeps agree.
///
/// # Errors
///
/// Returns an error if the log cannot be read, or holds an entry that does
/// not verify or contradicts one already in `identity`.
pub fn with_persisted_revocations(
    identity: ServerIdentity,
    state_root: &std::path::Path,
) -> anyhow::Result<ServerIdentity> {
    kept_revocations(identity, state_root).map_err(anyhow::Error::from)
}

/// [`with_persisted_revocations`], with a starting server's own errors.
fn kept_revocations(
    identity: ServerIdentity,
    state_root: &std::path::Path,
) -> Result<ServerIdentity, ServerError> {
    let ServerIdentity {
        user_public_key,
        device_signing_key,
        device_cert,
        mut revocations,
    } = identity;
    for entry in tilo_workspace_service::workspace_files::load_revocations(state_root)? {
        revocations
            .insert(entry)
            .map_err(|error| ServerError::Identity(format!("a kept revocation: {error}")))?;
    }
    Ok(ServerIdentity {
        user_public_key,
        device_signing_key,
        device_cert,
        revocations,
    })
}

/// A running server. Prefer [`Server::shutdown`]; dropping the handle also
/// stops the daemon and its background tasks, but leaves peers to
/// notice by timeout.
pub struct Server {
    pub endpoint: Endpoint,
    /// The workspace host this server is: its workspaces, the handshake that
    /// admits their members and every member's session.
    sessions: tilo_workspace_service::service::ServiceSessions,
    /// Only the browser listener publishes this public navigation origin.
    pub(crate) browser_origin: Arc<std::sync::RwLock<Option<Arc<String>>>>,
    /// The live session this server runs, which its browsers serve and its devices dial.
    pub(crate) live: Option<crate::device::live_relay::LiveSession>,
    /// Whether this device serves its own web UI, and how to serve it again: the
    /// setting `web.serve` switches, on by default.
    pub(crate) web_ui: Arc<crate::device::keep::web::control::Control>,
    /// Background tasks, aborted (per-handle) when the `Server` drops.
    background_tasks: Arc<std::sync::Mutex<Vec<AbortOnDrop>>>,
    /// What the routes a page asks before its session answer with.
    web: Box<WebContext>,
    /// The workspaces this server serves; absent when it hosts no workspace authority.
    workspaces: Option<tilo_workspace_service::workspaces::Workspaces>,
    /// The Browser this host places for its workspaces' pages, when it serves
    /// Browser ([`crate::device::keep::hosted_browser::HostedBrowsers::serving`]).
    #[cfg(feature = "agents")]
    browsers: Option<Arc<crate::device::keep::hosted_browser::HostedBrowsers>>,
    /// What this host keeps for each workspace it serves.
    content: Arc<tilo_content::WorkspaceStores>,
    /// This device's keys in the channels of the workspaces it holds, from the key service
    /// beside its workspace service.
    workspace_keys: tilo_workspace_keys::KeyProvider,
}

/// ALPN handlers to register on a router, each with its ALPN.
pub type Protocols = Vec<(Vec<u8>, Box<dyn DynProtocolHandler>)>;

/// What a device's daemon keeps more on: its workspace service, opened with
/// [`Server::keeping`], and what that service runs with.
pub struct OnDevice {
    /// What the device keeps, and where.
    pub cfg: ServerConfig,
    /// The device: its key and the certificate its person gave it.
    pub identity: ServerIdentity,
    /// The device's one workspace service.
    pub service: Arc<tilo_workspace_service::service::WorkspaceService>,
    /// The device's keys in its workspaces' channels.
    pub keys: tilo_workspace_keys::KeyProvider,
    /// The device's one endpoint.
    pub endpoint: Endpoint,
    /// Whether a ticket signed the device in, rather than its person founding it: only such a
    /// machine installs from the plan its person's Servers workspace has for it.
    pub joined: bool,
    /// What answers the apps' actions addressed to this device, when it serves apps.
    pub app_serving: Option<Arc<dyn tilo_workspace_service::app_serving::AppActionServer>>,
}

/// The server state the page's pre-session routes answer with.
#[derive(Clone)]
pub(crate) struct WebContext {
    pub(crate) server: ServerProtocol,
    /// Where pages reach this host over WebTransport, once it serves browsers: what
    /// [`Server::web_link`] answers, and what a test that stands in for a page dials.
    pub(crate) link: Arc<std::sync::OnceLock<crate::device::keep::web::LiveLink>>,
    /// The key for Apple's push service its configuration names, which its people's closed
    /// apps are told through.
    #[cfg(feature = "agents")]
    pub(crate) closed_apps: Option<tilo_server_protocol::config::ApplePushKey>,
    /// The stand-in for Apple's push service a test has the server push to.
    #[cfg(all(test, feature = "agents"))]
    pub(crate) apple_through: Arc<std::sync::OnceLock<crate::device::keep::apple_push::Service>>,
}

impl Server {
    pub(crate) fn web_context(&self) -> &WebContext {
        &self.web
    }

    /// Where pages reach this host over WebTransport, once it serves browsers
    /// ([`Self::serve_web`]): its endpoint and the listener pages dial.
    #[must_use]
    pub fn web_link(&self) -> Option<tilo_server_protocol::web::WebLink> {
        self.web
            .link
            .get()
            .map(crate::device::keep::web::LiveLink::now)
    }
}

/// A tokio `JoinHandle` that aborts on drop — used so that dropping
/// `Server` tears down every outbound attach reconnect task.
struct AbortOnDrop(Option<JoinHandle<()>>);

impl AbortOnDrop {
    /// Stop the task and answer its handle, so a stop can wait until what it held is let go.
    fn stop(mut self) -> Option<JoinHandle<()>> {
        let task = self.0.take()?;
        task.abort();
        Some(task)
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Stop reconnect loops before the daemon's endpoint goes. A server composed on a
        // device's daemon leaves the router, the endpoint and the key service to it.
        if let Ok(mut tasks) = self.background_tasks.lock() {
            tasks.clear();
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub struct KeptServer {
    /// The server the daemon's composition produced; its answers are this fixture's own.
    pub server: Server,
    /// Answers the workspace service's and the server's protocols while this lives.
    router: Option<Router>,
    /// The device's workspaces and the key service beside them, kept as a daemon keeps them:
    /// the groups' keys arrive through it, and it is shut down before the endpoint closes.
    workspaces: Arc<crate::device::workspaces::ServiceWorkspaces>,
    /// The device's endpoint, closed when this shuts down or goes.
    endpoint: Endpoint,
    /// The packet relay it runs or is reached through, when the configuration names one.
    relay: PacketRelay,
}

#[cfg(any(test, feature = "test-support"))]
impl std::ops::Deref for KeptServer {
    type Target = Server;

    fn deref(&self) -> &Server {
        &self.server
    }
}

#[cfg(any(test, feature = "test-support"))]
impl std::ops::DerefMut for KeptServer {
    fn deref_mut(&mut self) -> &mut Server {
        &mut self.server
    }
}

#[cfg(any(test, feature = "test-support"))]
impl KeptServer {
    /// The host `cfg` configures, started as a device's daemon starts one: the endpoint the
    /// configuration places, the device's workspace service opened in `cfg.state_root`, and
    /// [`Server::on_device`] on both.
    ///
    /// # Errors
    /// What [`Server::on_device`] refuses, and an identity or endpoint the daemon would refuse.
    pub async fn start(cfg: ServerConfig, identity: ServerIdentity) -> Result<Self, ServerError> {
        Box::pin(Self::compose(cfg, identity, None, None, Vec::new(), None)).await
    }

    /// [`Self::start`], on the relays `relay_mode` names instead of the configuration's: a relay
    /// on this machine, or none where nothing dials the host through one.
    ///
    /// # Errors
    /// As [`Self::start`].
    pub async fn with_relay_mode(
        cfg: ServerConfig,
        identity: ServerIdentity,
        relay_mode: iroh::RelayMode,
    ) -> Result<Self, ServerError> {
        Box::pin(Self::compose(
            cfg,
            identity,
            None,
            Some(relay_mode),
            Vec::new(),
            None,
        ))
        .await
    }

    /// [`Self::with_relay_mode`], with `app_serving` answering the workspaces' apps' calls.
    ///
    /// # Errors
    /// As [`Self::start`].
    pub async fn serving_apps(
        cfg: ServerConfig,
        identity: ServerIdentity,
        app_serving: Option<Arc<dyn tilo_workspace_service::app_serving::AppActionServer>>,
        relay_mode: iroh::RelayMode,
    ) -> Result<Self, ServerError> {
        Box::pin(Self::compose(
            cfg,
            identity,
            None,
            Some(relay_mode),
            Vec::new(),
            app_serving,
        ))
        .await
    }

    /// [`Self::start`] on an `endpoint` the caller bound: `extra` answer beside the service's
    /// own protocols, and `reaching` says whether the endpoint names where it is reached.
    ///
    /// # Errors
    /// As [`Self::start`].
    pub async fn on_endpoint(
        cfg: ServerConfig,
        identity: ServerIdentity,
        endpoint: Endpoint,
        reaching: Reaching,
        extra: Vec<(Vec<u8>, Box<dyn DynProtocolHandler>)>,
    ) -> Result<Self, ServerError> {
        Box::pin(Self::compose(
            cfg,
            identity,
            Some((endpoint, reaching)),
            None,
            extra,
            None,
        ))
        .await
    }

    /// Stop the host: what answers for it, then what it keeps, then its endpoint, whose address
    /// is free once this returns.
    pub async fn shutdown(self) {
        let Self {
            server,
            router,
            workspaces,
            endpoint,
            relay,
        } = self;
        if let Some(router) = router
            && let Err(error) = router.shutdown().await
        {
            tracing::debug!(%error, "server router shutdown task failed");
        }
        server.shutdown().await;
        workspaces.service().shutdown().await;
        endpoint.close().await;
        relay.shutdown().await;
    }

    async fn compose(
        cfg: ServerConfig,
        identity: ServerIdentity,
        endpoint: Option<(Endpoint, Reaching)>,
        relay_mode: Option<iroh::RelayMode>,
        extra: Vec<(Vec<u8>, Box<dyn DynProtocolHandler>)>,
        app_serving: Option<Arc<dyn tilo_workspace_service::app_serving::AppActionServer>>,
    ) -> Result<Self, ServerError> {
        validate_server_identity(&identity)?;
        std::fs::create_dir_all(&cfg.state_root)?;
        let identity = kept_revocations(identity, &cfg.state_root)?;
        let relay = PacketRelay::start(&cfg, endpoint.is_some() || relay_mode.is_some()).await?;
        let (endpoint, reaching) = if let Some((endpoint, reaching)) = endpoint {
            (endpoint, reaching)
        } else {
            let mode = relay_mode.or_else(|| relay.mode());
            let reaching = mode.as_ref().map_or(Reaching::ByRelay, reaching_of);
            let endpoint = bind_endpoint(&cfg, &identity.device_signing_key, mode).await?;
            (endpoint, reaching)
        };
        endpoints_agree(&identity, &endpoint)?;
        reaching.awaited(&endpoint).await;
        relay.reached(&endpoint)?;
        let device = tilo_identity::identity_store::DeviceIdentity::kept(
            identity.device_cert.clone(),
            identity.device_signing_key.clone(),
        );
        let keep = cfg.clone();
        let apps = app_serving.clone();
        let workspaces = crate::device::workspaces::ServiceWorkspaces::open(
            crate::device::workspaces::Opening {
                dir: &cfg.state_root,
                key: &identity.device_signing_key,
                identity: device,
                endpoint: endpoint.clone(),
                seal_checks: Arc::new(tilo_workspace_keys::NoSealChecks),
                apps: app_serving.clone(),
                configure: Some(Box::new(move |config| {
                    tilo_workspace_service::service::ServiceConfig {
                        reaching,
                        ..Server::keeping(&keep, config, apps)
                    }
                })),
                itself: Some(identity.clone()),
            },
        )
        .await
        .map_err(|error| {
            ServerError::Identity(format!("this device's workspace service: {error}"))
        })?;
        let service = Arc::clone(workspaces.service());
        let joined = crate::device::keep::servers::first_install::joined(&cfg.identity_root);
        let (server, protocols) = Server::on_device(OnDevice {
            cfg,
            identity,
            service: Arc::clone(&service),
            keys: workspaces.keys(),
            endpoint: endpoint.clone(),
            joined,
            app_serving,
        })
        .await?;
        let mut router = Router::builder(endpoint.clone());
        for (alpn, handler) in service.protocols() {
            router = router.accept(alpn, handler);
        }
        for (alpn, handler) in protocols.into_iter().chain(extra) {
            router = router.accept(alpn, handler);
        }
        Ok(Self {
            server,
            router: Some(router.spawn()),
            workspaces,
            endpoint,
            relay,
        })
    }
}

impl Server {
    /// `config`, a device's workspace service's, as a device keeping what `cfg` says opens it:
    /// the work it takes, who its handshake admits, the name it goes by among the holders of
    /// the workspaces it serves, whether its machine is rented, and the apps it answers for.
    #[must_use]
    pub fn keeping(
        cfg: &ServerConfig,
        config: tilo_workspace_service::service::ServiceConfig,
        apps: Option<Arc<dyn tilo_workspace_service::app_serving::AppActionServer>>,
    ) -> tilo_workspace_service::service::ServiceConfig {
        tilo_workspace_service::service::ServiceConfig {
            work: cfg.work.clone(),
            // A keeper that serves pages is a site, and goes by its site's name among the
            // holders of the workspaces it serves; a server of someone's own goes by the name
            // their admin workspace gives it, applied once it is open.
            label: cfg.web_entry.as_deref().map_or_else(String::new, site_name),
            rented: cfg.rented,
            // The address it signs carries its site's origin, so a member's device finds the
            // site's pages and API without guessing them from its name.
            origin: cfg.web_entry.as_deref().and_then(site_origin),
            trust_policy: cfg.trust_policy.clone(),
            max_workspace_grants: cfg.max_workspace_grants,
            apps,
            // Its workspaces open while the endpoint names its relay, and name it once it
            // answers, as a person's computer does: a net report's wait is not added to its start.
            reaching: Reaching::WhenNeeded,
            // Its member copies act as its own device, as every device's do: a workspace of
            // its person's that another device serves, it keeps as a member copy, entering it
            // as their device the way their other devices do.
            ..config
        }
    }

    /// What a device keeps beyond its own on `parts.service`, its workspace service as
    /// [`Self::keeping`] configured it: the certification and live session its people's devices
    /// and pages reach it through, the content it keeps for its workspaces, its place among its
    /// person's servers and their rollout, and its workspaces' terminals. The device's daemon
    /// registers the protocols it answers with on its own router, beside the service's.
    ///
    /// # Errors
    ///
    /// The device does not take the work a server takes, or its place among its person's
    /// servers cannot be kept.
    #[allow(
        clippy::too_many_lines,
        reason = "ordered startup preserves native identity, policy and task ownership"
    )]
    pub async fn on_device(parts: OnDevice) -> Result<(Self, Protocols), ServerError> {
        let OnDevice {
            cfg,
            identity,
            service,
            keys,
            endpoint,
            joined,
            app_serving,
        } = parts;
        endpoints_agree(&identity, &endpoint)?;
        if cfg.work.takes_nothing() {
            return Err(ServerError::Identity(
                "a device that takes no work serves no workspace".into(),
            ));
        }
        let content =
            crate::device::keep::content::of_device(&cfg.state_root, app_serving.as_ref());
        // App serving needs an app action server an embedding process supplies, and the operator's
        // consent to answer its workspaces' apps on this machine.
        let app_serving = app_serving.filter(|_| cfg.app_serving);
        let sessions = service.sessions().clone();
        let hosted = service.workspaces().clone();
        // The links its invitations hand over open the server's web entry. A stack on this
        // machine alone (`TILO_WORKSPACES_LOCAL`) keeps its own site: its addresses are
        // loopback, which no public page dials.
        if let Some(entry) = &cfg.web_entry {
            let local = std::env::var_os("TILO_WORKSPACES_LOCAL").is_some();
            let _ = hosted.invite_at(entry, local);
        }
        let live = sessions.identity.clone();
        let handler = sessions.handler.clone();
        // The live session belongs to the server: every key holder, a page
        // included, reaches it over the device's own iroh endpoint, and all
        // land in one origin whether or not browsers are served.
        let live_session =
            crate::device::live_relay::LiveSession::start(crate::device::live_relay::Shared::new(
                service.workspaces(),
                keys.clone(),
                identity.device_cert.clone(),
                handler.environments.clone(),
                Some(handler.clone()),
            ));
        // A server enrolled into its person's identity: a host of theirs alone.
        let enrolled_server = matches!(cfg.trust_policy, TrustPolicy::AllowMatchingUser);
        // The pages this host serves are served their tickets by the device's one set of
        // tickets, which keeps each device a page's ticket adds in its person's record here.
        hosted.action_service().tickets().serve_pages(Arc::new(
            crate::device::keep::page_tickets::PageTickets {
                identity: live.clone(),
                endpoint: endpoint.clone(),
                records: Some(hosted.downgrade()),
            },
        ));
        let web = Box::new(WebContext {
            server: handler.clone(),
            link: Arc::default(),
            #[cfg(feature = "agents")]
            closed_apps: cfg.apple_push.clone(),
            #[cfg(all(test, feature = "agents"))]
            apple_through: Arc::default(),
        });
        let protocols: Protocols = vec![live_session.protocol()];
        let background_tasks: Arc<std::sync::Mutex<Vec<AbortOnDrop>>> = Arc::default();
        {
            let mut tasks = lock_background_tasks(&background_tasks);
            tasks.extend(
                live_session
                    .tasks()
                    .into_iter()
                    .map(|task| AbortOnDrop(Some(task))),
            );
        }
        // A server of the person's own takes part in their Servers
        // workspace, whose record of their servers says what it should be.
        if enrolled_server {
            // One that has the workspace keeps its entry's address current.
            // One that has it not yet is handed it by a device that has it,
            // once the ticket it came in with entered it in the document.
            if tilo_workspace_service::workspaces::person_workspace::holds_person_workspace(&hosted)
                .await
            {
                // Its entry names the packet relay it is reached through, beside the address
                // it is dialled at, as its endpoint advertises it: a page has no UDP socket
                // and reaches it only through one.
                let relay = endpoint.addr().relay_urls().next().map(ToString::to_string);
                tilo_workspace_service::workspaces::servers::enroll(
                    &hosted,
                    &endpoint.addr(),
                    relay.as_deref(),
                )
                .await
                .map_err(|error| ServerError::Identity(format!("Servers workspace: {error}")))?;
            }
        }
        // What this device is, and what the person's servers are told to
        // be, is its record in the Servers workspace's document: from the
        // moment it has the workspace it applies the document, after each
        // change to it, and finishes the installation its plan describes if
        // it came in from one.
        push_background_task(
            &background_tasks,
            tokio::spawn(install_and_follow(
                hosted.clone(),
                endpoint.clone(),
                cfg.state_root.clone(),
                joined,
                identity.device_cert.clone(),
            )),
        );
        // A server's name is its entry's in its person's admin workspace:
        // the workspaces it holds show it by that name from now on.
        if let Some(label) = tilo_workspace_service::workspaces::servers::name(
            &hosted,
            &identity.device_cert.device_public_key,
        )
        .await
        {
            hosted.name_holder(&label).await;
        }
        // What every host keeps for its workspaces' members.
        push_background_task(
            &background_tasks,
            tokio::spawn(crate::device::keep::content::keep_for_members(
                &hosted,
                &content,
                app_serving,
            )),
        );
        // This device's own web UI, on until its person turns it off: the host operation
        // `web.serve` reaches it through the workspaces it serves.
        let web_ui = {
            let control = Arc::new(crate::device::keep::web::control::Control::default());
            hosted.serve_web_with(Arc::new(crate::device::keep::web::control::Serving(
                Arc::clone(&control),
            )));
            control
        };
        // A host that sells Linux machines places its workspaces' Browser in them: the
        // questions a window asks reach the person through the same store every other call on
        // this device waits in, and their answer goes back to the window that asked. The
        // device's Browser exists before this server's workspaces' service opens, which answers
        // their devices' app frames with it; its embedder wires it here
        // ([`Self::serve_browsers`]).
        Ok((
            Self {
                endpoint,
                sessions,
                browser_origin: Arc::new(std::sync::RwLock::new(None)),
                live: Some(live_session),
                web_ui,
                background_tasks,
                web,
                workspaces: Some(hosted),
                #[cfg(feature = "agents")]
                browsers: None,
                content,
                workspace_keys: keys,
            },
            protocols,
        ))
    }

    /// Wire the Browser `browsers` this host places to the workspaces it serves, on `runtime`:
    /// what answers a workspace's open of Browser and the page actions of the window it places,
    /// the ticket a session's VM joins with, and where a window's questions wait for the person.
    /// The Browser is made before the workspaces' service opens, which answers their devices'
    /// app frames with it ([`crate::device::keep::hosted_browser::app_serving`]); this is where it starts
    /// answering.
    ///
    /// # Errors
    ///
    /// The address its sessions reach this host at is not named.
    #[cfg(feature = "agents")]
    pub fn serve_browsers(
        &mut self,
        browsers: Arc<crate::device::keep::hosted_browser::HostedBrowsers>,
        runtime: tokio::runtime::Handle,
    ) -> Result<(), ServerError> {
        let Some(governed) = self.workspaces.as_ref() else {
            return Ok(());
        };
        let identity = &self.sessions.identity;
        // A workspace's VM joins with a ticket this host serves for it, and this host certifies
        // its daemon's key with its own for the VM's records alone.
        let server = crate::device::keep::hosted_browser::server_address()
            .map_err(|error| ServerError::Identity(format!("this host's Browser: {error}")))?;
        browsers.serve_from(crate::device::keep::hosted_browser::Authority {
            user_public_key: identity.user_public_key,
            host: identity.cert.clone(),
            host_key: identity.signing_key.clone(),
            server,
            workspaces: Some(governed.downgrade()),
            environments: self.sessions.handler.environments.clone(),
            runtime: runtime.clone(),
            hand: governed.hand(),
            actions: governed.action_service(),
        });
        browsers.ask_from(
            governed.action_service(),
            self.endpoint.clone(),
            governed.clone(),
            runtime,
        );
        if let Some(store) = governed.asked_store() {
            browsers.keep_asked_in(&store);
        }
        // The host's one dispatch answers the workspace's open of Browser and the page actions
        // of the window it places, as it answers every other action.
        browsers.provide_in(&governed.providers());
        self.browsers = Some(browsers);
        Ok(())
    }

    /// The live session this server runs, which what takes part in a workspace's calls, and the
    /// device's own daemon, join as the server's own device.
    #[must_use]
    pub(crate) fn live_session(&self) -> Option<crate::device::live_relay::LiveSession> {
        self.live.clone()
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

    /// Stop what this server started and tell every peer it has gone.
    ///
    /// Dropping the handle also stops it, but abandons the QUIC
    /// connections rather than closing them: each peer keeps this server
    /// as a live subscriber until its own idle timeout, and for that
    /// whole window fans out to a connection nobody reads instead of
    /// queueing for store-and-forward. Deltas in that window are neither
    /// delivered nor queued. The daemon that composed this server closes its
    /// endpoint right after, which sends `CONNECTION_CLOSE` straight away, so
    /// peers drop the subscription and queue instead.
    ///
    /// Every write its workspaces took in is already in their files: each is
    /// written through before the call that made it is answered, so a server
    /// stopped at any moment starts again on what it answered, and owes no turn
    /// its agent had answered.
    pub async fn shutdown(self) {
        // What this server started reaches the endpoint: stopped and awaited here, it lets go
        // of it before the daemon closes its endpoint. The dial loop is stopped first, so it
        // opens no fresh connection while the endpoint is closing.
        let stopped = std::mem::take(&mut *lock_background_tasks(&self.background_tasks));
        let serving = self.web_ui.stop_serving();
        for task in stopped
            .into_iter()
            .filter_map(AbortOnDrop::stop)
            .chain(serving)
        {
            let _ = task.await;
        }
    }

    /// What this server has carried so far: its counters, in total and per workspace.
    #[must_use]
    pub fn carried(&self) -> tilo_server_protocol::metrics::MetricsSnapshot {
        self.sessions.metrics.snapshot()
    }

    /// This device's key service: the one place the device holds in each key group of the
    /// workspaces it serves, which every session of this device reads and asks with. `None` when
    /// its certificate does not let it own workspaces.
    #[must_use]
    pub fn workspace_keys(&self) -> Option<tilo_workspace_keys::SharedKeys> {
        self.workspace_keys.get()
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

    /// Where the app action server hands a request to a device of one of its workspaces after
    /// answering its caller itself (a page action the person allowed, running
    /// on the window that refused it): this host's own delivery, through the
    /// action service, over the link the device holds open to this host.
    ///
    /// # Panics
    ///
    /// Outside a runtime, where there is nothing to deliver on.
    #[must_use]
    pub fn hand(&self) -> tilo_workspace_service::app_serving::Hand {
        match self.workspaces.as_ref() {
            Some(workspaces) => workspaces.hand(),
            // A host that serves no workspace of its own hands nothing on.
            None => tilo_workspace_service::app_serving::Hand::none(),
        }
    }

    /// The workspaces this server serves, when it hosts the workspace authority.
    #[must_use]
    pub const fn workspaces(&self) -> Option<&tilo_workspace_service::workspaces::Workspaces> {
        self.workspaces.as_ref()
    }

    /// The Browser this host places for its workspaces' pages, when it serves Browser.
    #[cfg(feature = "agents")]
    #[must_use]
    pub const fn browsers(
        &self,
    ) -> Option<&Arc<crate::device::keep::hosted_browser::HostedBrowsers>> {
        self.browsers.as_ref()
    }

    /// The devices this server certified for its browser environments, and the
    /// one record each reaches ([`tilo_workspace_service::environment`]): what the server
    /// confines each environment to when it places it, and what every app action server of
    /// its workspaces asks before it lets one in.
    #[must_use]
    pub fn environments(&self) -> tilo_workspace_service::environment::EnvironmentRegistry {
        self.sessions.handler.environments.clone()
    }

    /// What this host keeps for each workspace it serves: its one content
    /// store per workspace.
    #[must_use]
    pub fn content(&self) -> Arc<tilo_content::WorkspaceStores> {
        Arc::clone(&self.content)
    }

    /// Where this server keeps its state.
    pub(crate) fn state_root(&self) -> &std::path::Path {
        &self.sessions.state_root
    }

    /// In-process access to the records of the workspaces this server hosts,
    /// for a service embedded in the host process.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "records are read only where the server keeps workspaces"
    )]
    pub fn workspace_documents(
        &self,
    ) -> tilo_workspace_service::workspace_documents::WorkspaceDocuments {
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

/// The https origin of the site `web_entry` names (`https://eu1.tilo.sh`), which the address
/// this keeper signs carries: `None` for an entry that is no https address.
fn site_origin(web_entry: &str) -> Option<String> {
    let rest = web_entry.strip_prefix("https://")?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    (!host.is_empty()).then(|| format!("https://{host}"))
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
    workspaces: tilo_workspace_service::workspaces::Workspaces,
    endpoint: Endpoint,
    state_root: PathBuf,
    joined: bool,
    cert: tilo_user_identity::DeviceCert,
) {
    use tokio::sync::broadcast::error::RecvError;
    let mut listed = workspaces.listing_changes();
    let mut hosted = workspaces.governance_changes();
    while !tilo_workspace_service::workspaces::person_workspace::holds_person_workspace(&workspaces)
        .await
    {
        // A slow receiver re-reads; a closed one means the service is gone.
        let changed = tokio::select! {
            change = listed.recv() => change.map(drop),
            change = hosted.recv() => change.map(drop),
        };
        if changed == Err(RecvError::Closed) {
            return;
        }
    }
    // What the document asks of the machine is written in the server's own state, where the
    // image's root-run units read it (its state root is /var/lib/tilo/state there): a server that
    // is not the image's, one a stack or a container runs, can write nowhere else.
    let requests = crate::device::keep::servers::applied::Requests::at(
        state_root.join(crate::device::keep::servers::policy::POLICY_REQUEST_FILE),
    );
    // The document is applied while the installation waits on the person's
    // answers.
    let following = crate::device::keep::servers::applied::follow(
        &workspaces,
        requests.clone(),
        Some(endpoint),
        state_root,
    );
    if joined {
        match crate::device::keep::servers::first_install::install(
            &workspaces,
            &cert,
            &crate::device::keep::servers::first_install::real_drives(),
            &requests,
        )
        .await
        {
            Ok(()) => tracing::info!("this machine installed from its plan"),
            Err(error) => tracing::warn!(%error, "this machine did not install from its plan"),
        }
    }
    // Following ends when this does: it holds the endpoint, so a stop that ends this task ends
    // the following with it.
    following.wait().await;
}

/// The packet relay a server is reached through, as its config names it: one it runs itself
/// ([`ServerConfig::packet_relay`]), one it is given ([`ServerConfig::packet_relay_url`]), or
/// none, leaving its endpoint on the public relays. A page has no UDP socket, so it reaches a
/// server only through a relay, and the server advertises its relay in its signed address.
pub struct PacketRelay {
    /// The relay this server runs, while it runs.
    pub server: Option<iroh_relay::server::Server>,
    /// The URL its endpoint is reached through, when the config names one.
    url: Option<iroh::RelayUrl>,
    /// Whether that URL is another's relay rather than this server's own.
    given: bool,
}

impl PacketRelay {
    /// Start the relay `cfg` names, if it names one. `bound` says the endpoint is bound
    /// elsewhere, or its relays are chosen elsewhere: such a server runs no relay of its own.
    ///
    /// # Errors
    ///
    /// The relay does not start, its URL does not parse, or `bound` and a relay of its own are
    /// asked for together.
    pub async fn start(cfg: &ServerConfig, bound: bool) -> Result<Self, ServerError> {
        let published = cfg
            .packet_relay_url
            .as_deref()
            .map(|published| {
                published.parse::<iroh::RelayUrl>().map_err(|error| {
                    ServerError::Iroh(format!("packet relay URL {published}: {error}"))
                })
            })
            .transpose()?;
        match cfg.packet_relay {
            Some(_) if bound => Err(ServerError::Iroh(
                "a host runs its own packet relay or is given one, not both".into(),
            )),
            Some(addr) => {
                let (server, url) = spawn_packet_relay(addr).await?;
                let url = published.unwrap_or(url);
                tracing::info!(%addr, %url, "running this host's own packet relay");
                Ok(Self {
                    server: Some(server),
                    url: Some(url),
                    given: false,
                })
            }
            // Given a packet relay's URL and none of its own, the server is
            // reached through that one: a packet relay on the public internet
            // when the server sits behind a router nobody can dial into.
            None if !bound && published.is_some() => {
                tracing::info!(url = ?published, "reached through another packet relay");
                Ok(Self {
                    server: None,
                    url: published,
                    given: true,
                })
            }
            None => Ok(Self {
                server: None,
                url: None,
                given: false,
            }),
        }
    }

    /// The relay mode an endpoint reached through this relay binds with, when there is one.
    #[must_use]
    pub fn mode(&self) -> Option<iroh::RelayMode> {
        self.url
            .clone()
            .map(|url| iroh::RelayMode::Custom(url.into()))
    }

    /// Whether `endpoint` is reached through the relay it was given: one it cannot reach leaves
    /// it reachable by nobody, which its start says rather than serving.
    ///
    /// # Errors
    ///
    /// The endpoint reached no relay through the one it was given.
    pub fn reached(&self, endpoint: &Endpoint) -> Result<(), ServerError> {
        match &self.url {
            Some(url) if self.given && endpoint.addr().relay_urls().next().is_none() => {
                Err(ServerError::Iroh(format!(
                    "the packet relay at {url}, which this server is to be reached through, did \
                     not answer: no one could reach the server. Check that the URL names a packet \
                     relay this computer reaches (tilo.sh's is https://live-eu1.tilo.sh)"
                )))
            }
            _ => Ok(()),
        }
    }

    /// Stop the relay this server runs, if it runs one.
    pub async fn shutdown(self) {
        if let Some(server) = self.server
            && let Err(error) = server.shutdown().await
        {
            tracing::debug!(%error, "packet relay shutdown task failed");
        }
    }
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
    let url: iroh_base::RelayUrl = format!("http://{}", dialled_at(serving))
        .parse()
        .map_err(|error| ServerError::Iroh(format!("packet relay URL: {error}")))?;
    Ok((server, url))
}

/// `serving` as a device elsewhere dials it: an address bound on every interface is this
/// machine's own address on the network it routes to, learned without a packet being sent,
/// and loopback when it routes nowhere.
fn dialled_at(serving: std::net::SocketAddr) -> std::net::SocketAddr {
    if !serving.ip().is_unspecified() {
        return serving;
    }
    let ip = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))
        .and_then(|socket| {
            // Connecting a UDP socket picks the route and sends nothing.
            socket.connect((std::net::Ipv4Addr::new(192, 0, 2, 1), 9))?;
            socket.local_addr()
        })
        .map_or(
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            |local| local.ip(),
        );
    std::net::SocketAddr::new(ip, serving.port())
}

/// Whether an endpoint bound with `relay_mode` names where it is reached: one
/// told to use no relay never does, and everything else is expected to.
#[cfg(any(test, feature = "test-support"))]
fn reaching_of(relay_mode: &iroh::RelayMode) -> Reaching {
    if matches!(relay_mode, iroh::RelayMode::Disabled) {
        Reaching::OnItsOwn
    } else {
        Reaching::ByRelay
    }
}

/// Bind the one endpoint of a device that keeps what `cfg` names: on the address it names,
/// reached through the packet relay it runs or is given, else the public relays. The relay runs
/// for as long as the returned [`PacketRelay`] is kept. The endpoint names its relay while the
/// device opens its workspaces; [`PacketRelay::reached`] says whether it did.
///
/// # Errors
///
/// The relay does not start, or the endpoint does not bind.
pub async fn bind_keeping(
    cfg: &ServerConfig,
    device_key: &ed25519_dalek::SigningKey,
) -> Result<(Endpoint, PacketRelay), ServerError> {
    let relay = PacketRelay::start(cfg, false).await?;
    let endpoint = bind_endpoint(cfg, device_key, relay.mode()).await?;
    Ok((endpoint, relay))
}

/// Bind the device's one endpoint, keyed by its device key as every device's is: the key its
/// certificate names is the node its people's devices dial.
async fn bind_endpoint(
    cfg: &ServerConfig,
    device_key: &ed25519_dalek::SigningKey,
    relay_mode: Option<iroh::RelayMode>,
) -> Result<Endpoint, ServerError> {
    let secret = iroh::SecretKey::from_bytes(&device_key.to_bytes());
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
    lock_background_tasks(tasks).push(AbortOnDrop(Some(task)));
}

#[cfg(any(test, feature = "test-support"))]
fn validate_server_identity(identity: &ServerIdentity) -> Result<(), ServerError> {
    tilo_workspace_service::service::validate_service_device_identity(identity)?;
    if !identity.device_cert.capabilities.can_own_workspaces {
        return Err(ServerError::Identity(
            "the certificate is not a device of the person".into(),
        ));
    }
    // The evidence of the calls this server's apps have admitted is signed as this device: the
    // certificate its identity holds and the key that certificate names. A server without the
    // app host admits no app's call and signs no receipt.
    #[cfg(feature = "agents")]
    tilo_app_host::serve_receipts_as(
        identity.device_cert.clone(),
        std::sync::Arc::new(identity.device_signing_key.clone()),
    );
    Ok(())
}

/// Refuse a server whose `endpoint` is keyed by another device than `identity` is certified as:
/// one device has one key, so the node its people's devices dial is the one its links and
/// content asks name.
fn endpoints_agree(identity: &ServerIdentity, endpoint: &Endpoint) -> Result<(), ServerError> {
    let device = identity.device_cert.device_public_key;
    if endpoint.id().as_bytes() != &device {
        return Err(ServerError::Identity(format!(
            "this server's endpoint is keyed by {}, its device is certified as {}",
            hex::encode(endpoint.id().as_bytes()),
            hex::encode(device),
        )));
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
mod tests {
    use super::*;
    use crate::device::keep::servers::floor;
    use std::time::Duration;
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity,
        sign_device_cert,
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

    /// A server's endpoint is the device its certificate names: handed an endpoint keyed by
    /// another device, it starts not at all, so what it signs and what it is dialled at never
    /// disagree.
    #[tokio::test]
    async fn an_endpoint_keyed_by_another_device_is_refused() {
        let state = tempfile::tempdir().unwrap();
        let elsewhere = ed25519_dalek::SigningKey::from_bytes(&[77; 32]);
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(&elsewhere.to_bytes()))
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .expect("a loopback endpoint binds");
        let refused = KeptServer::on_endpoint(
            ServerConfig {
                identity_root: state.path().join("identity"),
                ..ServerConfig::loopback(state.path().to_path_buf())
            },
            identity(DeviceCapabilities::member()),
            endpoint.clone(),
            Reaching::OnItsOwn,
            Vec::new(),
        )
        .await;
        let Err(error) = refused else {
            panic!("a server whose endpoint is keyed by another device starts");
        };
        assert!(error.to_string().contains("keyed by"), "{error}");
        endpoint.close().await;
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
        // Phase `an_enrolled_servers_updates_take_turns_among_its_persons_servers`.
        {
            let temp = tempfile::tempdir().unwrap();
            let (root, user) = UserIdentity::generate("owner", 1);
            let (host_key, host) = DeviceIdentity::generate("home", 2);
            let (client_key, client) = DeviceIdentity::generate("laptop", 3);
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
            let server = KeptServer::start(
                ServerConfig {
                    state_root: temp.path().join("server"),
                    trust_policy: TrustPolicy::AllowMatchingUser,
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
            let primary = KeptServer::start(
                ServerConfig {
                    state_root: primary_state.path().to_path_buf(),
                    app_serving: true,
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
            tilo_workspace_service::workspaces::person_workspace::found(primary_workspaces)
                .await
                .unwrap();
            tilo_workspace_service::workspaces::servers::admit(
                primary_workspaces,
                &server_cert,
                &server.node_addr(),
            )
            .await
            .unwrap();
            let person_workspace =
                tilo_server_protocol::servers::person_workspace_id(&user.public_key);
            // The entry names where the server is reached, and the workspace's
            // holders hand it the workspace once it does.
            primary_workspaces
                .attach_server(
                    &person_workspace,
                    server_cert.device_public_key,
                    server.node_addr(),
                    tilo_peer_state::CopyRole::KeyHolder,
                )
                .await
                .unwrap();
            tilo_test_env::within(
                "the server to hold its entry in the Servers workspace",
                async {
                    while !tilo_workspace_service::workspaces::servers::read(server_workspaces)
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
                .enter(&server_workspace, (&client_cert, &client_key))
                .await
                .unwrap();
            let hosted = server.workspaces().unwrap();
            // After a healthy boot it asks its person's other devices to move its rollback
            // floor, saying which key the floor takes, and it never answers its own request.
            let secret_sync = tilo_seal::SecretKey::from_bytes([3; 32]);
            let made = floor::make_key(&root, &secret_sync.public_key().unwrap()).unwrap();
            floor::apply(hosted, made.action).await.unwrap();
            let state = tilo_rollback::counter::FloorState {
                counter: 9,
                base: 4,
            };
            let request =
                tilo_rollback::approval::build_request(0x01D9_1100, &[0, 11, 1], state, &[5, 6], 3);
            let asked = floor::Asked {
                generation: 6,
                image: "sha256:next".into(),
                updates: "stable".into(),
                authority: made.public.clone(),
                request: serde_json::to_string(&request.unwrap()).unwrap(),
            };
            let asked = floor::ask(hosted, asked).await.unwrap();
            assert!(asked.is_err_and(|why| why.contains("waiting")));
            let servers = tilo_workspace_service::workspaces::servers::read(hosted).await;
            let entry = servers.unwrap().active(&host.public_key).cloned().unwrap();
            assert_eq!(entry.floor_key, made.public);
            let answered = floor::answer(hosted, &secret_sync).await.unwrap();
            assert_eq!(answered, 0, "a server never answers its own floor request");
            server.shutdown().await;
        }
    }

    /// A server runs the packet relay its pages are reached through, and its entry among
    /// its person's servers names that relay beside the address it is dialled at, so every
    /// device of theirs knows where to reach it through one. A server that starts holding
    /// the Servers workspace writes it on that start; one handed it later writes it the
    /// next time it starts.
    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the whole hand-over, from the admitting device to the restarted server's entry"
    )]
    async fn a_server_serves_its_packet_relay_and_its_entry_names_it() {
        let temp = tempfile::tempdir().unwrap();
        let (root, user) = UserIdentity::generate("owner", 1);
        let (host_key, host) = DeviceIdentity::generate("home", 2);
        let (admin_key, admin) = DeviceIdentity::generate("primary", 5);
        let mut server_cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: host.public_key,
            device_label: host.label.clone(),
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
        sign_device_cert(&mut admin_cert, &root).unwrap();
        let primary_state = tempfile::tempdir().unwrap();
        let primary = KeptServer::start(
            ServerConfig {
                state_root: primary_state.path().to_path_buf(),
                app_serving: true,
                ..ServerConfig::default()
            }
            .on_loopback(),
            ServerIdentity {
                user_public_key: user.public_key,
                device_signing_key: admin_key,
                device_cert: admin_cert,
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        let primary_workspaces = primary.workspaces().expect("the device holds workspaces");
        tilo_workspace_service::workspaces::person_workspace::found(primary_workspaces)
            .await
            .unwrap();
        // The server runs the packet relay this machine's loopback is reached through:
        // nothing here asks number 0 for one.
        let config = ServerConfig {
            state_root: temp.path().join("server"),
            trust_policy: TrustPolicy::AllowMatchingUser,
            packet_relay: Some((std::net::Ipv4Addr::LOCALHOST, 0).into()),
            ..ServerConfig::default()
        }
        .on_loopback();
        let identity = ServerIdentity {
            user_public_key: user.public_key,
            device_signing_key: host_key,
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        };
        let server = KeptServer::start(config.clone(), identity.clone())
            .await
            .unwrap();
        let relay = server
            .node_addr()
            .relay_urls()
            .next()
            .expect("the server names its packet relay")
            .to_string();
        // It serves it: something answers on the address the URL names.
        let listening = relay
            .strip_prefix("http://")
            .expect("a plain URL")
            .trim_end_matches('/');
        let front = tokio::net::TcpStream::connect(
            listening
                .parse::<std::net::SocketAddr>()
                .expect("a relay URL's address"),
        )
        .await
        .expect("the packet relay answers");
        drop(front);
        let person_workspace = tilo_server_protocol::servers::person_workspace_id(&user.public_key);
        tilo_workspace_service::workspaces::servers::admit(
            primary_workspaces,
            &server_cert,
            &server.node_addr(),
        )
        .await
        .unwrap();
        primary_workspaces
            .attach_server(
                &person_workspace,
                server_cert.device_public_key,
                server.node_addr(),
                tilo_peer_state::CopyRole::KeyHolder,
            )
            .await
            .unwrap();
        let hosted = server.workspaces().expect("the server holds workspaces");
        tilo_test_env::within("the server to hold the Servers workspace", async {
            while !tilo_workspace_service::workspaces::person_workspace::holds_person_workspace(
                hosted,
            )
            .await
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        // Started again, it keeps the workspace from its own state and writes its entry
        // from where it is reached: its address, and the relay beside it.
        server.shutdown().await;
        let restarted = KeptServer::start(config, identity).await.unwrap();
        let hosted = restarted.workspaces().expect("the server holds workspaces");
        let endpoint = serde_json::to_string(&restarted.node_addr()).unwrap();
        let named = tilo_test_env::within("the entry to name the relay", async {
            loop {
                if let Ok(servers) = tilo_workspace_service::workspaces::servers::read(hosted).await
                    && let Some(entry) = servers.active(&host.public_key)
                    && entry.endpoint == endpoint
                    && !entry.relay.is_empty()
                {
                    break entry.relay.clone();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        let reached_by = restarted
            .node_addr()
            .relay_urls()
            .next()
            .expect("the restarted server names its packet relay")
            .to_string();
        assert_eq!(
            named, reached_by,
            "the entry names the packet relay the server is reached through"
        );
        restarted.shutdown().await;
        primary.shutdown().await;
    }

    /// A server of the person's, freshly started and handed their workspace, keeps a workspace
    /// their list gains that another device of theirs serves: it enters it as their device, a
    /// member copy of that device's, with nothing set up on it by hand.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario: the person, their two devices and the workspace the server comes to keep"
    )]
    async fn a_fresh_server_keeps_a_listed_workspace_another_device_serves() {
        let (root, user) = UserIdentity::generate("owner", 1);
        let certified = |label: &str, seed: u64, capabilities: DeviceCapabilities| {
            let (key, device) = DeviceIdentity::generate(label, seed);
            let mut cert = DeviceCert {
                user_public_key: user.public_key,
                device_public_key: device.public_key,
                device_label: device.label,
                capabilities,
                issued_at_ns: 4,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            sign_device_cert(&mut cert, &root).unwrap();
            ServerIdentity {
                user_public_key: user.public_key,
                device_signing_key: key,
                device_cert: cert,
                revocations: RevocationList::new(),
            }
        };
        let server_identity = certified("attic", 2, DeviceCapabilities::member());
        let server_cert = server_identity.device_cert.clone();
        let desk_identity = certified("desk", 3, DeviceCapabilities::primary());
        let desk_device = desk_identity.device_cert.device_public_key;
        let server_state = tempfile::tempdir().unwrap();
        let server = KeptServer::start(
            ServerConfig {
                state_root: server_state.path().to_path_buf(),
                trust_policy: TrustPolicy::AllowMatchingUser,
                app_serving: true,
                packet_relay: Some((std::net::Ipv4Addr::LOCALHOST, 0).into()),
                ..ServerConfig::default()
            }
            .on_loopback(),
            server_identity,
        )
        .await
        .unwrap();
        let desk_state = tempfile::tempdir().unwrap();
        let desk = KeptServer::start(
            ServerConfig {
                state_root: desk_state.path().to_path_buf(),
                app_serving: true,
                ..ServerConfig::default()
            }
            .on_loopback(),
            desk_identity,
        )
        .await
        .unwrap();
        let (kept, desk_workspaces) = (
            server.workspaces().expect("the server keeps workspaces"),
            desk.workspaces().expect("the desk keeps workspaces"),
        );

        // The desk founds the person's workspace, enters the server among their servers and
        // hands it the workspace, as the person's device hands every server of theirs.
        tilo_workspace_service::workspaces::person_workspace::found(desk_workspaces)
            .await
            .unwrap();
        tilo_workspace_service::workspaces::servers::admit(
            desk_workspaces,
            &server_cert,
            &server.node_addr(),
        )
        .await
        .unwrap();
        desk_workspaces
            .attach_server(
                &tilo_server_protocol::servers::person_workspace_id(&user.public_key),
                server_cert.device_public_key,
                server.node_addr(),
                tilo_peer_state::CopyRole::KeyHolder,
            )
            .await
            .unwrap();

        // A workspace the desk serves joins the person's list.
        let made = desk_workspaces.create("Desk").await.unwrap().workspace;
        let hint = desk_workspaces
            .hosts(&made)
            .await
            .unwrap()
            .into_iter()
            .find(|hint| hint.host.device_public_key == desk_device)
            .expect("the desk signs where it serves the workspace");
        tilo_workspace_service::workspaces::fixtures::list_workspace(
            desk_workspaces,
            &made,
            "Desk",
            &hint,
        )
        .await;

        let server_device = server_cert.device_public_key;
        waits_until_listed(kept, &made, server_device).await;
        // The server keeps it on its own, by the list's change alone.
        tilo_test_env::within(
            "the server keeps the workspace its person's list gained",
            async {
                while !kept.is_member(&made).await.unwrap_or(false) {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            },
        )
        .await;
        server.shutdown().await;
        desk.shutdown().await;
    }

    /// Wait until `kept`, a server's workspaces, keeps the person's workspace, reads its own
    /// entry there, and holds `made` as the person's list names it.
    async fn waits_until_listed(
        kept: &tilo_workspace_service::workspaces::Workspaces,
        made: &str,
        server_device: [u8; 32],
    ) {
        tilo_test_env::within(
            "the server keeps the person's workspace and reads its entry there",
            async {
                while
                    !(tilo_workspace_service::workspaces::person_workspace::holds_person_workspace(
                        kept,
                    )
                    .await
                        && tilo_workspace_service::workspaces::servers::read(kept)
                            .await
                            .is_ok_and(|servers| servers.active(&server_device).is_some()))
                {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            },
        )
        .await;
        tilo_test_env::within("the person's list reaches the server", async {
            while !tilo_workspace_service::workspaces::fixtures::lists(kept, made).await {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
    }
}
