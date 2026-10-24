//! The room host: the governance of the rooms placed on a device and the
//! session stack their members connect through, embeddable in any process
//! that owns an iroh endpoint.
//!
//! A relay runs one as a certified host ([`HostMode::CertifiedHost`]); a
//! person's device runs the same one for the rooms placed on it
//! ([`HostMode::Device`]). [`RoomHost::open`] spawns nothing periodic: with
//! no peers connected the host wakes only on I/O, and a connected peer's
//! session is audited when its rooms' membership files change, which the
//! host watches. Its embedder registers
//! [`RoomHost::protocols`] on its own router, or, when the router is built
//! before any room is placed here, [`deferred_protocols`], which refuse
//! connections until [`HostSlot::install`] gives them a host.
//!
//! A member in the same process connects through [`RoomHost::connect_local`]:
//! the same handshake and session as a remote member, over an in-process
//! stream.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use iroh::endpoint::{Endpoint, RecvStream, SendStream};
use iroh::protocol::{AcceptError, DynProtocolHandler, ProtocolHandler};
use tilo_clock::unix_now_ns;
use tilo_lock::RwLockRecover as _;
use tilo_user_identity::{DeviceCert, RevocationList, verify_device_cert_at};
use tokio::sync::Mutex as AsyncMutex;

use crate::app_serving::AppServingPlane;
use crate::client::{ClientError, RelayClient};
use crate::handshake::{EndpointTrust, HandshakeError, LiveIdentity};
use crate::metrics::Metrics;
use crate::peer::{PeerContext, WorkspaceAutobind, WorkspaceRegistry, run_peer_until};
use crate::policy::TrustPolicy;
use crate::transport::{ALPN, MAX_FRAME_BYTES};

/// How much an in-process session buffers each way before its writer waits.
const LOCAL_SESSION_BUFFER: usize = 1 << 20;

/// How a host proves itself to the members that dial it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostMode {
    /// An always-on host certified with host capability (a relay).
    CertifiedHost,
    /// A person's device hosting the rooms placed on it: no host capability
    /// needed. The governing device itself serves its rooms, and its join
    /// answers name it as such
    /// ([`WorkspaceHostMode::NativeInteractiveWorkspaceOwner`]).
    ///
    /// [`WorkspaceHostMode::NativeInteractiveWorkspaceOwner`]:
    /// tilo_delivery_protocol::workspace_join::WorkspaceHostMode::NativeInteractiveWorkspaceOwner
    Device,
}

impl HostMode {
    /// What the host's join answers say it is.
    pub(crate) const fn join_hint_mode(
        self,
    ) -> tilo_delivery_protocol::workspace_join::WorkspaceHostMode {
        match self {
            Self::CertifiedHost => {
                tilo_delivery_protocol::workspace_join::WorkspaceHostMode::CertifiedHost
            }
            Self::Device => {
                tilo_delivery_protocol::workspace_join::WorkspaceHostMode::NativeInteractiveWorkspaceOwner
            }
        }
    }
}

/// What a host is opened with.
#[derive(Clone)]
pub struct HostConfig {
    /// Where the host keeps its rooms and their retained state.
    pub state_root: PathBuf,
    pub mode: HostMode,
    /// Who the handshake admits besides the person's own devices and the
    /// rooms' members.
    pub trust_policy: TrustPolicy,
    /// The change and content budgets guests are held to.
    pub member_limits: Option<crate::member_limits::MemberLimitsConfig>,
    /// Pre-admissions each room keeps, spent ones included.
    pub max_room_grants: Option<usize>,
    /// Answers remote actions addressed to this host beyond its own
    /// governance (`tilo.host`, `tilo.workspace`).
    pub apps: Option<Arc<dyn AppServingPlane>>,
    /// Classifies co-edit bodies for every workspace the host opens.
    pub shell_payloads: Arc<dyn crate::payloads::ShellPayloads>,
}

impl HostConfig {
    /// A host keeping its state under `state_root`, admitting the person's
    /// own devices and its rooms' members, with the desktop's payloads.
    #[must_use]
    pub fn new(state_root: PathBuf, mode: HostMode) -> Self {
        Self {
            state_root,
            mode,
            trust_policy: TrustPolicy::AllowMatchingUser,
            member_limits: None,
            max_room_grants: None,
            apps: None,
            shell_payloads: crate::payloads::neighbour(),
        }
    }
}

/// Why a host could not open or change.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// The identity is invalid, or not one this mode may host with.
    #[error("identity: {0}")]
    Identity(String),
    /// On-disk persistence.
    #[error("store: {0}")]
    Store(#[from] crate::store::StoreError),
    /// Filesystem I/O.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Governed workspace admission could not be reconciled safely.
    #[error("governed access: {0}")]
    GovernedAccess(String),
    /// The rooms could not be opened.
    #[error("governed rooms: {0}")]
    Rooms(String),
    /// A member's handshake failed.
    #[error("handshake: {0}")]
    Handshake(String),
}

/// ALPN handlers to register on a router, each with its ALPN.
pub type Protocols = Vec<(Vec<u8>, Box<dyn DynProtocolHandler>)>;

/// The ALPNs a room host answers on.
const HOST_ALPNS: [&[u8]; 4] = [
    ALPN,
    tilo_delivery_protocol::workspace_join::WORKSPACE_JOIN_ALPN,
    crate::rooms::ROOM_ADMIN_ALPN,
    tilo_relay_session::rooms::private::PRIVATE_ALPN,
];

/// A host's session stack: its workspaces, the handshake that admits their
/// members, and the session every member runs, whatever its carrier.
#[derive(Clone)]
pub struct HostSessions {
    pub(crate) endpoint: Endpoint,
    pub workspaces: WorkspaceRegistry,
    pub metrics: Arc<Metrics>,
    pub identity: LiveIdentity,
    pub(crate) handshake_trust: HandshakeTrust,
    governed_access_update: Arc<AsyncMutex<()>>,
    pub state_root: PathBuf,
    pub(crate) shell_payloads: Arc<dyn crate::payloads::ShellPayloads>,
    /// The relay ALPN's handler, and every session's context.
    pub handler: RelayProtocol,
    /// Ends the in-process sessions when the host shuts down.
    closing: Arc<tokio::sync::watch::Sender<bool>>,
    /// The watch that lets its rooms' membership be read from memory until
    /// a file changes, so a live session is audited when something changed
    /// rather than every second.
    membership_watch: Arc<crate::membership_watch::WatchTask>,
}

/// A host's rooms, when it governs them: the governance and the plane
/// answering its actions.
pub struct Governed {
    pub rooms: crate::rooms::Rooms,
    pub(crate) actions: Arc<dyn AppServingPlane>,
}

impl HostSessions {
    /// Open a host's sessions and, when `governs`, its rooms. `wrap` gives
    /// the plane every session answers remote actions with, from the
    /// workspaces and the host's own plane (its governance and apps): a
    /// relay answers compute placement around it.
    ///
    /// # Errors
    ///
    /// The identity is invalid or, for a certified host, not host-capable;
    /// the state root cannot be created; or the rooms cannot be opened.
    pub async fn open(
        cfg: HostConfig,
        identity: HostIdentity,
        endpoint: Endpoint,
        governs: bool,
        wrap: impl FnOnce(
            &WorkspaceRegistry,
            Option<Arc<dyn AppServingPlane>>,
        ) -> Option<Arc<dyn AppServingPlane>>,
    ) -> Result<(Self, Option<Governed>), HostError> {
        validate_host_device_identity(&identity)?;
        if cfg.mode == HostMode::CertifiedHost && !identity.device_cert.capabilities.can_act_as_host
        {
            return Err(HostError::Identity(
                "device certificate is not host-capable".into(),
            ));
        }
        say_when_unsealed(governs, &identity.device_cert);
        std::fs::create_dir_all(&cfg.state_root)?;
        let workspaces: WorkspaceRegistry =
            Arc::new(AsyncMutex::new(std::collections::HashMap::new()));
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
        // The host answers its rooms' governance as actions itself, beside
        // whatever apps its embedder serves.
        let governance = governs.then(crate::rooms::HostRoomActions::new);
        let own_plane = match &governance {
            Some(governance) => Some(Arc::new(crate::rooms::GovernedPlane {
                governance: governance.clone(),
                apps: cfg.apps.clone(),
            }) as Arc<dyn AppServingPlane>),
            None => cfg.apps.clone(),
        };
        let autobind =
            matches!(cfg.trust_policy, TrustPolicy::AllowMatchingUser).then(|| WorkspaceAutobind {
                state_root: cfg.state_root.clone(),
                shell_payloads: cfg.shell_payloads.clone(),
            });
        let handler = RelayProtocol {
            workspaces: workspaces.clone(),
            trust_policy: handshake_trust.live_policy.clone(),
            identity: live.clone(),
            metrics: metrics.clone(),
            autobind,
            app_serving: wrap(&workspaces, own_plane.clone()),
            live_bridge: Arc::default(),
            agents: Arc::default(),
            compactor: crate::compaction::Compactor::new(
                identity.device_signing_key.clone(),
                identity.device_cert.clone(),
                cfg.shell_payloads.clone(),
            ),
            member_limits: cfg
                .member_limits
                .map(|limits| Arc::new(crate::member_limits::MemberLimits::new(limits))),
        };
        let sessions = Self {
            endpoint: endpoint.clone(),
            workspaces: workspaces.clone(),
            metrics: metrics.clone(),
            identity: live,
            handshake_trust,
            governed_access_update: Arc::new(AsyncMutex::new(())),
            state_root: cfg.state_root.clone(),
            shell_payloads: cfg.shell_payloads.clone(),
            handler,
            closing: Arc::new(tokio::sync::watch::channel(false).0),
            membership_watch: Arc::new(crate::membership_watch::WatchTask::spawn(&cfg.state_root)),
        };
        let (Some(governance), Some(actions)) = (governance, own_plane) else {
            return Ok((sessions, None));
        };
        let rooms = crate::rooms::Rooms::open(
            crate::rooms::RoomServices {
                workspaces: workspaces.clone(),
                access: sessions.access_writer(),
                records: sessions.room_records(),
                endpoint,
                cert: identity.device_cert.clone(),
                key: identity.device_signing_key.clone(),
                revocations,
                metrics: metrics.clone(),
                max_pre_admissions: cfg
                    .max_room_grants
                    .unwrap_or(crate::rooms::DEFAULT_MAX_PRE_ADMISSIONS),
                trust_policy: sessions.handler.trust_policy.clone(),
                agents: sessions.handler.agents.clone(),
                mode: cfg.mode.join_hint_mode(),
            },
            cfg.state_root.join("rooms"),
        )
        .await
        .map_err(HostError::Rooms)?;
        governance.attach(rooms.clone());
        #[cfg(any(feature = "agents", feature = "web"))]
        sessions
            .handler
            .compactor
            .steward_with(Arc::new(rooms.clone()));
        Ok((sessions, Some(Governed { rooms, actions })))
    }

    /// The writer of the host's governed workspace access.
    pub(crate) fn access_writer(&self) -> GovernedAccessWriter {
        GovernedAccessWriter {
            workspaces: self.workspaces.clone(),
            handshake_trust: self.handshake_trust.clone(),
            update: Arc::clone(&self.governed_access_update),
            state_root: self.state_root.clone(),
            shell_payloads: self.shell_payloads.clone(),
        }
    }

    /// In-process access to the records of the workspaces this host holds.
    pub fn room_records(&self) -> crate::room_records::RoomRecords {
        crate::room_records::RoomRecords::new(
            self.workspaces.clone(),
            self.identity.cert.device_public_key,
            self.identity.user_public_key,
            self.identity.signing_key.clone(),
            self.metrics.clone(),
        )
    }

    /// Atomically replace a governed workspace's exact member devices and
    /// the user roots those devices need at the handshake.
    ///
    /// # Errors
    ///
    /// [`HostError::GovernedAccess`] when the host admits anyone or
    /// `state_root` is not its own, and [`HostError::Store`] when workspace
    /// initialization or durable replacement fails.
    pub async fn replace_governed_workspace_access(
        &self,
        workspace_id: u64,
        pinned: bool,
        access: tilo_peer_state::GovernedAccess,
        state_root: PathBuf,
    ) -> Result<(), HostError> {
        if self.handshake_trust.allow_any {
            return Err(HostError::GovernedAccess(
                "allow_any cannot host governed workspace admission".into(),
            ));
        }
        if state_root != self.state_root {
            return Err(HostError::GovernedAccess(
                "state root does not match the running relay".into(),
            ));
        }
        self.access_writer()
            .replace(workspace_id, pinned, access)
            .await
    }

    /// Whether the host counts `device` as subscribed to `workspace_id`.
    pub async fn is_subscribed(&self, workspace_id: u64, device: &[u8; 32]) -> bool {
        let ws = self.workspaces.lock().await.get(&workspace_id).cloned();
        match ws {
            Some(ws) => ws.lock().await.has_subscriber(device),
            None => false,
        }
    }

    /// A member session of this device's own person to this host, over an
    /// in-process stream: the handshake and the session a remote member
    /// runs. It lasts until either side drops it or the host shuts down.
    ///
    /// # Errors
    ///
    /// As [`RelayClient::connect_v2_over_stream`]: the host refused the
    /// device, or the handshake failed.
    pub(crate) async fn connect_local(
        &self,
        cert: DeviceCert,
        key: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<RelayClient, ClientError> {
        let (client, server) = tokio::io::duplex(LOCAL_SESSION_BUFFER);
        let (server_read, server_write) = tokio::io::split(server);
        let handler = self.handler.clone();
        let mut closing = self.closing.subscribe();
        tokio::spawn(async move {
            let closed = async move {
                // The sender lives as long as the host; its end is one too.
                let _ = closing.wait_for(|closing| *closing).await;
            };
            if let Err(error) = serve_session(handler, server_read, server_write, closed).await {
                tracing::debug!(%error, "in-process session ended");
            }
        });
        let (client_read, client_write) = tokio::io::split(client);
        RelayClient::connect_v2_over_stream(
            client_read,
            client_write,
            self.identity.cert.device_public_key,
            cert,
            key,
            revocations,
        )
        .await
    }

    /// End every in-process session and stop compacting.
    pub async fn shutdown(&self) {
        self.closing.send_replace(true);
        self.membership_watch.stop();
        self.handler.compactor.stop().await;
    }
}

/// Say loudly when a host that `governs` rooms under `cert` stewards no
/// channels: its certificate may not own workspaces, so the rooms it governs
/// keep their records in the clear in the room document instead of sealed
/// channels.
fn say_when_unsealed(governs: bool, cert: &DeviceCert) {
    if governs && !cert.capabilities.stewards_room_channels() {
        tracing::error!(
            device = %cert.device_label,
            "this host's certificate cannot own workspaces, so the rooms it governs are not sealed; re-enroll it with the own capability"
        );
    }
}

/// The handlers of a governing host's ALPNs, shared between the routers
/// that register them.
fn host_handlers(
    sessions: &HostSessions,
    rooms: &crate::rooms::Rooms,
) -> Vec<(&'static [u8], Arc<dyn DynProtocolHandler>)> {
    vec![
        (
            ALPN,
            Arc::new(sessions.handler.clone()) as Arc<dyn DynProtocolHandler>,
        ),
        (
            tilo_delivery_protocol::workspace_join::WORKSPACE_JOIN_ALPN,
            Arc::new(
                tilo_relay_session::workspace_join::WorkspaceJoinProtocol::new(
                    rooms.join_service(),
                ),
            ),
        ),
        (
            crate::rooms::ROOM_ADMIN_ALPN,
            Arc::new(crate::rooms::RoomAdminProtocol::new(rooms.clone())),
        ),
        (
            tilo_relay_session::rooms::private::PRIVATE_ALPN,
            Arc::new(crate::rooms::PrivateProtocol::new(rooms.clone())),
        ),
    ]
}

/// The rooms' own ALPNs on a relay's router, beside its session ALPN.
pub fn rooms_protocols(
    sessions: &HostSessions,
    rooms: &crate::rooms::Rooms,
) -> Vec<(&'static [u8], Box<dyn DynProtocolHandler>)> {
    host_handlers(sessions, rooms)
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

/// The governance of the rooms placed on this device and the sessions of
/// their members.
pub struct RoomHost {
    sessions: HostSessions,
    rooms: crate::rooms::Rooms,
    actions: Arc<dyn AppServingPlane>,
    handlers: Vec<(&'static [u8], Arc<dyn DynProtocolHandler>)>,
}

impl std::fmt::Debug for RoomHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomHost")
            .field(
                "device",
                &hex::encode(self.sessions.identity.cert.device_public_key),
            )
            .finish_non_exhaustive()
    }
}

impl RoomHost {
    /// Open the host on `endpoint`, which the embedder owns and routes.
    /// Spawns nothing periodic: with no peers connected it wakes only on
    /// I/O.
    ///
    /// # Errors
    ///
    /// The identity is invalid or, for [`HostMode::CertifiedHost`], not
    /// host-capable; `cfg` admits anyone, under which no room is governed;
    /// the state root cannot be created; or the rooms kept there cannot be
    /// opened.
    pub async fn open(
        cfg: HostConfig,
        identity: HostIdentity,
        endpoint: Endpoint,
    ) -> Result<Self, HostError> {
        if matches!(cfg.trust_policy, TrustPolicy::AllowAny) {
            return Err(HostError::Identity(
                "a host admitting anyone governs no rooms".into(),
            ));
        }
        let (sessions, governed) =
            HostSessions::open(cfg, identity, endpoint, true, |_, plane| plane).await?;
        let Some(Governed { rooms, actions }) = governed else {
            return Err(HostError::Rooms("the host governs no rooms".into()));
        };
        let handlers = host_handlers(&sessions, &rooms);
        Ok(Self {
            sessions,
            rooms,
            actions,
            handlers,
        })
    }

    /// The ALPN handlers to register on the embedder's router: the session,
    /// workspace join, room administration and private protocols.
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

    /// The governance of the rooms hosted here.
    #[must_use]
    pub const fn rooms(&self) -> &crate::rooms::Rooms {
        &self.rooms
    }

    /// The plane answering the host's own actions (`tilo.host`:
    /// creating, listing and entering rooms; `tilo.workspace`: each room's
    /// governance) and its embedder's apps, for in-process callers. A
    /// caller is named by [`local_caller`] and its certificate.
    #[must_use]
    pub fn actions(&self) -> Arc<dyn AppServingPlane> {
        Arc::clone(&self.actions)
    }

    /// The workspace join service of the rooms hosted here, for a joining
    /// device in the same process
    /// ([`WorkspaceJoinClient::local`](tilo_relay_session::workspace_join::WorkspaceJoinClient::local)).
    #[must_use]
    pub fn join_service(&self) -> Arc<dyn tilo_relay_session::workspace_join::LocalJoinService> {
        Arc::new(self.rooms.join_service())
    }

    /// An in-process member session of this device's own person: the same
    /// handshake and session as a remote member, with its private action
    /// channel, over an in-process stream.
    ///
    /// # Errors
    ///
    /// The host refused `cert`, or the handshake failed.
    pub async fn connect_local(
        &self,
        cert: DeviceCert,
        key: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<RelayClient, ClientError> {
        self.sessions.connect_local(cert, key, revocations).await
    }

    /// This host's endpoint address: what a member dials.
    #[must_use]
    pub fn node_addr(&self) -> iroh_base::EndpointAddr {
        self.sessions.endpoint.addr()
    }

    /// The compactor of the workspaces hosted here.
    #[cfg(all(test, feature = "agents"))]
    pub(crate) const fn compactor(&self) -> &crate::compaction::Compactor {
        &self.sessions.handler.compactor
    }

    /// The durable records of the rooms hosted here, for a service the
    /// embedder runs beside them.
    #[must_use]
    pub fn room_records(&self) -> crate::room_records::RoomRecords {
        self.sessions.room_records()
    }

    /// The certificate this host proves itself with.
    #[must_use]
    pub fn device_cert(&self) -> &DeviceCert {
        &self.sessions.identity.cert
    }

    /// Replace the revocations the host checks every handshake against.
    /// Narrowing them ends the sessions of the devices revoked.
    pub fn set_revocations(&self, revocations: RevocationList) {
        *self.sessions.identity.revocations.write_recover() = revocations;
        crate::authority_epoch::bump();
    }

    /// Whether this host counts `device` as subscribed to `workspace_id`.
    pub async fn is_subscribed(&self, workspace_id: u64, device: &[u8; 32]) -> bool {
        self.sessions.is_subscribed(workspace_id, device).await
    }

    /// Every counter of this host's sessions now.
    #[must_use]
    pub fn metrics(&self) -> crate::metrics::MetricsSnapshot {
        self.sessions.metrics.snapshot()
    }

    /// End the in-process sessions and stop the host's work. The embedder
    /// closes its own router and endpoint.
    pub async fn shutdown(self) {
        self.sessions.shutdown().await;
    }
}

/// The caller an in-process device is to the host's [`RoomHost::actions`].
#[must_use]
pub fn local_caller(cert: &DeviceCert) -> tilo_actions::remote::Caller {
    tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key)
}

/// Where an embedder whose router is built before any room is placed on it
/// installs the host its [`deferred_protocols`] forward to.
#[derive(Clone, Default)]
pub struct HostSlot(Arc<std::sync::RwLock<Option<Arc<RoomHost>>>>);

impl std::fmt::Debug for HostSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("HostSlot")
            .field(&self.0.read_recover().is_some())
            .finish()
    }
}

impl HostSlot {
    /// Forward this slot's connections to `host` from now on.
    pub fn install(&self, host: Arc<RoomHost>) {
        *self.0.write_recover() = Some(host);
    }

    /// Refuse this slot's connections again, handing back the host it
    /// forwarded to.
    pub fn clear(&self) -> Option<Arc<RoomHost>> {
        self.0.write_recover().take()
    }

    /// The host this slot forwards to.
    #[must_use]
    pub fn host(&self) -> Option<Arc<RoomHost>> {
        self.0.read_recover().clone()
    }
}

/// Handlers for every ALPN a room host answers on, for a router built
/// before any host opens: each refuses connections until its slot holds a
/// host ([`HostSlot::install`]), then forwards to that host. Before then
/// they cost nothing.
#[must_use]
pub fn deferred_protocols() -> (HostSlot, Protocols) {
    let slot = HostSlot::default();
    let handlers = HOST_ALPNS
        .iter()
        .map(|alpn| {
            (
                alpn.to_vec(),
                Box::new(Deferred {
                    slot: slot.clone(),
                    alpn,
                }) as Box<dyn DynProtocolHandler>,
            )
        })
        .collect();
    (slot, handlers)
}

/// One ALPN of a [`HostSlot`].
#[derive(Debug)]
struct Deferred {
    slot: HostSlot,
    alpn: &'static [u8],
}

impl ProtocolHandler for Deferred {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let Some(host) = self.slot.host() else {
            return Err(AcceptError::from_err(std::io::Error::other(
                "no room is hosted here",
            )));
        };
        let Some(handler) = host.handler(self.alpn).cloned() else {
            return Err(AcceptError::from_err(std::io::Error::other(
                "the host does not answer this protocol",
            )));
        };
        handler.accept(connection).await
    }
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
    ) -> Result<(), HostError> {
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
    ) -> Result<(), HostError> {
        if self.handshake_trust.allow_any {
            return Err(HostError::GovernedAccess(
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
pub(crate) struct HandshakeTrust {
    base_roots: Arc<HashSet<[u8; 32]>>,
    pub(crate) live_policy: Arc<std::sync::RwLock<TrustPolicy>>,
    pub(crate) allow_any: bool,
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
        }
    }

    pub(crate) async fn refresh(&self, workspaces: &WorkspaceRegistry) {
        if self.allow_any {
            return;
        }
        let mut roots = self.base_roots.as_ref().clone();
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

/// Bundle of identity material a host needs to run. The relay daemon's
/// `main.rs` loads it from
/// [`tilo_user_identity::persist`] and passed in explicitly so the
/// integration tests can wire a `tempdir`-based identity.
#[derive(Clone)]
pub struct HostIdentity {
    /// The user root pubkey the relay chains to.
    pub user_public_key: [u8; 32],
    pub device_signing_key: ed25519_dalek::SigningKey,
    /// This device's signed cert (issued by the user root).
    pub device_cert: DeviceCert,
    /// Current revocation list (may be empty).
    pub revocations: RevocationList,
}

pub fn validate_host_device_identity(identity: &HostIdentity) -> Result<(), HostError> {
    verify_device_cert_at(&identity.device_cert, unix_now_ns())
        .map_err(|error| HostError::Identity(format!("invalid device certificate: {error}")))?;
    if identity.device_cert.user_public_key != identity.user_public_key {
        return Err(HostError::Identity(
            "device certificate does not chain to the configured user public key".into(),
        ));
    }
    if identity.device_cert.device_public_key
        != identity.device_signing_key.verifying_key().to_bytes()
    {
        return Err(HostError::Identity(
            "device certificate does not match the configured device signing key".into(),
        ));
    }
    if identity
        .revocations
        .is_revoked(&identity.device_cert.device_public_key)
    {
        return Err(HostError::Identity("device certificate is revoked".into()));
    }
    Ok(())
}

/// The `ProtocolHandler` iroh wires to the ALPN.
#[derive(Clone)]
pub struct RelayProtocol {
    pub workspaces: WorkspaceRegistry,
    pub trust_policy: Arc<std::sync::RwLock<TrustPolicy>>,
    pub identity: LiveIdentity,
    pub metrics: Arc<Metrics>,
    pub autobind: Option<WorkspaceAutobind>,
    pub(crate) app_serving: Option<Arc<dyn crate::app_serving::AppServingPlane>>,
    /// Set once when the host starts serving browsers.
    pub live_bridge: Arc<std::sync::OnceLock<crate::peer::LiveBridge>>,
    pub agents: crate::peer::AgentRegistry,
    pub compactor: crate::compaction::Compactor,
    pub(crate) member_limits: Option<Arc<crate::member_limits::MemberLimits>>,
}

impl RelayProtocol {
    /// This relay's certificate and agent trust.
    #[cfg(feature = "web")]
    pub fn trust(&self) -> crate::room_access::RelayTrust<'_> {
        crate::room_access::RelayTrust {
            revocations: &self.identity.revocations,
            policy: &self.trust_policy,
            own_user: &self.identity.user_public_key,
            agents: &self.agents,
        }
    }

    /// The peer context a session of this relay runs with.
    pub fn peer_context(&self) -> PeerContext {
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
    send: SendStream,
    recv: RecvStream,
) -> Result<(), HostError> {
    let closed = connection.clone();
    let result = serve_session(ctx, recv, send, async move {
        closed.closed().await;
    })
    .await;
    connection.close(0u32.into(), b"relay session ended");
    result
}

/// One member session over `recv`/`send`: the bounded shared handshake
/// under the live policy (the owner and every user the host currently
/// admits), then the peer loop until the stream ends or `closed` resolves.
/// Every carrier runs this: an iroh connection, and an in-process stream.
async fn serve_session<R, W>(
    ctx: RelayProtocol,
    mut recv: R,
    mut send: W,
    closed: impl std::future::Future<Output = ()> + Send,
) -> Result<(), HostError>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    ctx.metrics.inc_conns_accepted();
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
            return Err(HostError::Handshake(error.to_string()));
        }
    };
    ctx.metrics.inc_conns_authed();
    let context = ctx.peer_context();
    if let Err(e) = Box::pin(run_peer_until(client_peer, closed, recv, send, context)).await {
        tracing::debug!("peer session ended with error: {e}");
    }
    Ok(())
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
}
