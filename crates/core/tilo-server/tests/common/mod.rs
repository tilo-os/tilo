#![allow(
    dead_code,
    reason = "each integration module uses a different subset of these fixtures"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::sync::Arc;
use tilo_test_env::SpawnUnderEnvLock as _;

use ed25519_dalek::SigningKey;
use iroh::endpoint::{Endpoint, presets};
use iroh::protocol::Router;
use tempfile::TempDir;

use tilo_peer_state::actions::RemoteDevice;
use tilo_peer_state::direct::{DirectSync, Heard, Reader};
use tilo_server_client::LinkError;
use tilo_server_client::link::{Dialer, Purpose};
use tilo_server_protocol::refusal::Refusal;

use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity, sign_device_cert,
};

use tilo_peer_state::join::{Role, decode_and_verify_invitation};
use tilo_server::{
    ServerConfig, TrustPolicy, WorkspaceId,
    daemon::{Server, ServerIdentity},
    service::{ServiceConfig, ServiceIdentity, WorkspaceService},
    workspace_join::WorkspaceJoinClient,
    workspaces::{IssueInvitationRequest, IssuedInvitation, Workspaces},
};
use tilo_server_protocol::WorkSetting;

/// A "user" (root keypair) + on-disk identity root usable by the
/// tests.
pub struct TestUser {
    pub user_sk: SigningKey,
    pub user_pk: [u8; 32],
}

impl TestUser {
    pub fn fresh(label: &str) -> Self {
        let (sk, u) = UserIdentity::generate(label, 1);
        Self {
            user_sk: sk,
            user_pk: u.public_key,
        }
    }

    /// Enrol a device (mints a keypair + signed cert).
    pub fn enrol(&self, label: &str, caps: DeviceCapabilities) -> TestDevice {
        let (dev_sk, dev) = DeviceIdentity::generate(label, 100);
        let mut cert = DeviceCert {
            user_public_key: self.user_pk,
            device_public_key: dev.public_key,
            device_label: dev.label,
            capabilities: caps,
            issued_at_ns: 200,
            expires_at_ns: None,
            signature: [0u8; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &self.user_sk).unwrap();
        TestDevice { sk: dev_sk, cert }
    }
}

/// A "device" — signing key + signed cert.
#[derive(Clone)]
pub struct TestDevice {
    pub sk: SigningKey,
    pub cert: DeviceCert,
}

impl TestDevice {
    pub fn device_pubkey(&self) -> [u8; 32] {
        self.cert.device_public_key
    }
}

/// Spin up a server bound to `workspace_ids`. Every listed
/// workspace is created pinned + with `members` as the membership
/// set.
pub struct TestServer {
    pub server: Server,
    pub state_root: PathBuf,
    #[allow(
        dead_code,
        reason = "keeps the temporary state root alive for the server's lifetime"
    )]
    tmp: TempDir,
}

pub async fn spawn_server(
    user: &TestUser,
    server_dev: TestDevice,
    workspaces: &[WorkspaceId],
    members: &[&TestDevice],
) -> TestServer {
    let tmp = tempfile::tempdir().unwrap();
    let state_root = tmp.path().to_path_buf();
    let server = Box::pin(spawn_server_on(
        &state_root,
        user,
        server_dev,
        workspaces,
        members,
    ))
    .await;
    TestServer {
        server,
        state_root,
        tmp,
    }
}

/// Start a server bound to `workspaces` (all pinned) with `members`,
/// persisting to a caller-owned `state_root`. Unlike [`spawn_server`]
/// the caller keeps the directory alive, so it can drop the returned
/// `Relay` and call this again on the same `state_root` to model a
/// host that restarts — the on-disk pending queues + iroh secret
/// survive the restart. The server device cert is minted fresh on
/// each call (only its *user* identity must be stable), mirroring a
/// host that re-enrols a device key on reinstall.
pub async fn spawn_server_on(
    state_root: &std::path::Path,
    user: &TestUser,
    server_dev: TestDevice,
    workspaces: &[WorkspaceId],
    members: &[&TestDevice],
) -> Server {
    Box::pin(spawn_server_with(
        state_root, user, server_dev, workspaces, members, None,
    ))
    .await
}

/// [`spawn_server`] for a host that serves apps through `server` and asks to
/// advertise `app_serving`.
pub async fn spawn_server_serving_apps(
    user: &TestUser,
    server_dev: TestDevice,
    workspaces: &[WorkspaceId],
    members: &[&TestDevice],
    server: std::sync::Arc<dyn tilo_server::AppActionServer>,
) -> TestServer {
    let tmp = tempfile::tempdir().unwrap();
    let state_root = tmp.path().to_path_buf();
    let server = Box::pin(spawn_server_with(
        &state_root,
        user,
        server_dev,
        workspaces,
        members,
        Some(server),
    ))
    .await;
    TestServer {
        server,
        state_root,
        tmp,
    }
}

async fn spawn_server_with(
    state_root: &std::path::Path,
    user: &TestUser,
    server_dev: TestDevice,
    workspaces: &[WorkspaceId],
    members: &[&TestDevice],
    server: Option<std::sync::Arc<dyn tilo_server::AppActionServer>>,
) -> Server {
    if std::env::var_os("TILO_SERVER_CORRELATED_DIAGNOSTICS").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_env_filter(
                "tilo_server::peer=info,tilo_server::client=info,tilo_server::workspace_io=info",
            )
            .with_writer(|| PhaseWriter)
            .try_init();
    }
    let mut cfg = ServerConfig::loopback(state_root.to_path_buf());
    let identity = ServerIdentity {
        user_public_key: user.user_pk,
        device_signing_key: server_dev.sk.clone(),
        device_cert: server_dev.cert.clone(),
        revocations: RevocationList::new(),
    };
    cfg.services.app_serving = server.is_some();
    let server = start_server_serving(cfg, identity, server).await.unwrap();
    for id in workspaces {
        hold_workspace(&server, *id, members).await;
    }
    server
}

/// A host under test on this machine's loopback packet relay: the one way these tests start a
/// host, so none of them relays through number 0's relays or publishes to its directory.
///
/// Boxed: a test's setup future carries its host's config and identity, and a whole scenario's
/// would grow past what the lint allows without this on the heap.
///
/// # Errors
///
/// As [`Server::start`].
pub fn start_server(
    cfg: ServerConfig,
    identity: ServerIdentity,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Server, tilo_server::ServerError>> + Send>,
> {
    start_server_serving(cfg, identity, None)
}

/// [`start_server`], with the app action server `server` when a test hosts apps.
///
/// # Errors
///
/// As [`Server::start`].
pub fn start_server_serving(
    cfg: ServerConfig,
    identity: ServerIdentity,
    server: Option<std::sync::Arc<dyn tilo_server::AppActionServer>>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Server, tilo_server::ServerError>> + Send>,
> {
    Box::pin(async move {
        Server::start_with_app_serving_and_relay_mode(
            cfg.on_loopback(),
            identity,
            server,
            test_server::relay_mode(),
        )
        .await
    })
}

/// The devices `workspace`'s signed member list has, as `server` holds it now: none when it
/// holds no such workspace.
pub async fn member_devices(server: &Server, workspace: WorkspaceId) -> Vec<[u8; 32]> {
    let Some(state) = server
        .workspaces()
        .expect("the server holds workspaces")
        .state(&workspace.to_string())
        .await
    else {
        return Vec::new();
    };
    state
        .governance()
        .governed_access(std::time::SystemTime::now())
        .expect("the workspace's member list")
        .member_devices
}

/// The server's person makes `workspace` on their server, and it holds it;
/// `members`, devices of theirs, enter it. These tests enter a workspace the
/// way production does — a server holds what is made on it, and a member
/// joins — never by handing the server a member list from outside.
pub async fn hold_workspace(server: &Server, workspace: WorkspaceId, members: &[&TestDevice]) {
    let workspaces = server.workspaces().expect("the server holds workspaces");
    let workspace = workspace.to_string();
    if workspaces.summary(&workspace).await.is_ok() {
        // Held already, as a server restart finds it: its governance names the
        // members it admitted, and they enter it no more.
        return;
    }
    workspaces
        .create_workspace("Workspace", Some(&workspace))
        .await
        .expect("the person's server makes the workspace");
    for member in members {
        workspaces
            .enter(&workspace, (&member.cert, &member.sk))
            .await
            .expect("a device of the person lists itself in the workspace");
    }
}

/// [`spawn_server`] on a server whose handshake admits `others`' people too,
/// as a hosted one's operator allow-list does: its workspaces are still made
/// by its own person, and others join them as members.
pub async fn spawn_server_admitting(
    user: &TestUser,
    server_dev: &TestDevice,
    others: &[&TestUser],
    workspaces: &[WorkspaceId],
    members: &[&TestDevice],
) -> TestServer {
    Box::pin(spawn_server_admitting_with(
        user, server_dev, others, workspaces, members, None,
    ))
    .await
}

/// [`spawn_server_admitting`] for a server that serves apps through `server`.
pub async fn spawn_server_admitting_with(
    user: &TestUser,
    server_dev: &TestDevice,
    others: &[&TestUser],
    workspaces: &[WorkspaceId],
    members: &[&TestDevice],
    server: Option<std::sync::Arc<dyn tilo_server::AppActionServer>>,
) -> TestServer {
    let tmp = tempfile::tempdir().unwrap();
    let state_root = tmp.path().to_path_buf();
    let mut users = vec![hex::encode(user.user_pk)];
    users.extend(others.iter().map(|other| hex::encode(other.user_pk)));
    let cfg = ServerConfig {
        state_root: state_root.clone(),
        trust_policy: TrustPolicy::AllowList { users },
        ..Default::default()
    };
    let identity = ServerIdentity {
        user_public_key: user.user_pk,
        device_signing_key: server_dev.sk.clone(),
        device_cert: server_dev.cert.clone(),
        revocations: RevocationList::new(),
    };
    let server = start_server_serving(cfg, identity, server).await.unwrap();
    for id in workspaces {
        hold_workspace(&server, *id, members).await;
    }
    TestServer {
        server,
        state_root,
        tmp,
    }
}

/// An invitation to `workspace` for `recipient`'s person, issued by `issuer`.
pub async fn invitation(
    workspaces: &Workspaces,
    issuer: &TestDevice,
    workspace: &str,
    role: Role,
    recipient: &TestDevice,
) -> IssuedInvitation {
    let mut request_id = [0; 32];
    getrandom::fill(&mut request_id).unwrap();
    workspaces
        .issue(
            workspace,
            &issuer.cert,
            IssueInvitationRequest {
                request_id,
                role,
                recipient_user_public_key: Some(recipient.cert.user_public_key),
                invitee_name: None,
                inviter_name: None,
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(600_000_000_000)
                    .unwrap(),
            },
        )
        .await
        .unwrap()
}

/// `device` redeems `issued` through the address the invitation carries.
pub async fn redeem(device: &TestDevice, issued: &IssuedInvitation) -> Result<(), String> {
    let verified = decode_and_verify_invitation(
        &issued.signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )?;
    let mut request_id = [0; 32];
    getrandom::fill(&mut request_id).unwrap();
    WorkspaceJoinClient::new(device_endpoint(device).await)
        .redeem_hint(
            &verified.grant().transport[0],
            issued.signed_invitation.clone(),
            device.cert.clone(),
            &device.sk,
            request_id,
        )
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Bind a plain iroh endpoint the tests can use to dial a relay. It carries no relay and no
/// directory of its own: every address it dials is one a test holds ([`test_server::loopback`] is
/// what a device's own relays are in these tests).
pub async fn client_endpoint() -> Endpoint {
    Endpoint::builder(presets::Minimal).bind().await.unwrap()
}

/// An endpoint whose key is the device's own, as a device dials with: relays on, as a page and a
/// shell dial a host they were handed a link to (`tilo-server-client`'s `Host::bind` reaches the
/// host through the relay it names). Its relay is this machine's loopback one
/// ([`test_server::loopback`]), so a link's relay URL is one this machine answers on.
pub async fn device_endpoint(device: &TestDevice) -> Endpoint {
    test_server::endpoint(iroh::SecretKey::from_bytes(&device.sk.to_bytes())).await
}

/// A host in `mode` on `device`'s own endpoint, routed as its embedder
/// routes it.
pub struct Hosted {
    pub host: WorkspaceService,
    pub router: Router,
    state: TempDir,
}

impl Hosted {
    pub async fn stop(self) {
        drop(self.stop_keeping().await);
    }

    /// Stop the host, keeping its state for [`serve_in`] to open again.
    pub async fn stop_keeping(self) -> TempDir {
        self.router.shutdown().await.unwrap();
        self.host.shutdown().await;
        self.state
    }
}

pub async fn serve_on(user: &TestUser, device: &TestDevice, work: WorkSetting) -> Hosted {
    serve_in(user, device, work, tempfile::tempdir().unwrap()).await
}

/// `device` joins the person's own workspace as a device joining from a ticket
/// does: the device the ticket is answered on enters it among the person's
/// servers, and the copy's holders hand it the workspace once its entry names
/// where it is reached. Waits until `at` keeps the workspace.
pub async fn join_person_workspace(governor: &Hosted, device: &TestDevice, at: &Hosted) {
    // Boxed: the hand-over's own future is large, and every fixture that joins
    // a device would otherwise carry it in its own.
    Box::pin(async move {
        tilo_server::servers::admit(
            governor.host.workspaces(),
            &device.cert,
            &at.host.node_addr(),
        )
        .await
        .unwrap();
        tilo_server::servers::attach::attach_named(governor.host.workspaces())
            .await
            .unwrap();
        tilo_test_env::within(
            "the joining device to keep the person's own workspace",
            async {
                while !tilo_server::servers::holds_person_workspace(at.host.workspaces()).await {
                    tokio::task::yield_now().await;
                }
            },
        )
        .await;
    })
    .await;
}

/// A host as [`serve_on`] makes one, over the state `state` holds. Its device takes part in
/// its workspaces' channels, as every device that founds a workspace or seals its records does.
pub async fn serve_in(
    user: &TestUser,
    device: &TestDevice,
    work: WorkSetting,
    state: TempDir,
) -> Hosted {
    let channels = tilo_workspace_service::workspaces::DeviceKeyGroups {
        keys: Arc::<tilo_workspace_service::workspaces::fixtures::TestGroupKeys>::default(),
        sink: None,
    };
    serve_with_keys(user, device, work, state, Some(channels)).await
}

/// [`serve_in`], whose device takes part in its workspaces' channels through `channels`, as a
/// person's own device does: what opens a workspace's vault there.
pub async fn serve_with_keys(
    user: &TestUser,
    device: &TestDevice,
    work: WorkSetting,
    state: TempDir,
    channels: Option<tilo_workspace_service::workspaces::DeviceKeyGroups>,
) -> Hosted {
    let config = ServiceConfig::new(state.path().to_path_buf(), work);
    serve_configured(user, device, (config, state), channels).await
}

/// A host as [`serve_on`] makes one, opened with `config` over the state `state` holds: what an
/// operator's own settings (whom it trusts) make of it.
pub async fn serve_configured(
    user: &TestUser,
    device: &TestDevice,
    (config, state): (ServiceConfig, TempDir),
    channels: Option<tilo_workspace_service::workspaces::DeviceKeyGroups>,
) -> Hosted {
    let endpoint = device_endpoint(device).await;
    let host = WorkspaceService::open(
        config,
        ServiceIdentity {
            user_public_key: user.user_pk,
            device_signing_key: device.sk.clone(),
            device_cert: device.cert.clone(),
            revocations: RevocationList::new(),
        },
        endpoint.clone(),
        channels,
    )
    .await
    .unwrap();
    let mut router = Router::builder(endpoint);
    for (alpn, handler) in host.protocols() {
        router = router.accept(alpn, handler);
    }
    Hosted {
        host,
        router: router.spawn(),
        state,
    }
}

/// A member device's link to one workspace on a host: its own copy of the workspace, seeded
/// from what the host holds, the link it dialled and runs, and the host's actions asked from the
/// same endpoint.
///
/// It is the same set-up a device's session makes: a dialer to the
/// host, the member handshake on the link's first stream, and the workspace's records dialled
/// over it. Nothing here speaks a protocol of its own.
pub struct Linked {
    /// This device's copy of the workspace, as its links sync it.
    pub sync: DirectSync,
    /// The host's actions in this workspace, asked as this device.
    pub actions: RemoteDevice,
    /// The host's device key: who the link is pinned to and requests are addressed to.
    pub host: [u8; 32],
    /// This device's signing key, for the requests it asks beside the link's.
    sk: SigningKey,
    /// The device this link belongs to, for the links it opens beside this one.
    device: TestDevice,
    /// The endpoint every link of this device runs from: a server keeps one connection per
    /// endpoint id, so a device binds one and lends it. `None` over WebTransport, which has no
    /// endpoint.
    endpoint: Option<Endpoint>,
    reader: Reader,
    /// The endpoints the links run from: they live as long as this does.
    dialer: Arc<Dialer>,
    running: Option<tokio::task::JoinHandle<Result<(), Refusal>>>,
}

impl Linked {
    /// This device's key: who it asks and writes as.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.sync.device()
    }

    /// Read this device's copy of the workspace.
    pub fn read<R>(&self, read: impl FnOnce(&tilo_peer_state::StatePeer) -> R) -> R {
        self.sync.read(read)
    }

    /// Send `message` on the link, as this device.
    ///
    /// # Errors
    ///
    /// The link ended, or the message could not be signed or kept.
    pub fn send(&self, message: neighbour_wire::NeighbourMsg) -> Result<(), String> {
        self.sync.send(message)
    }

    /// Make and send `write`, keeping the message it returns as this device's write.
    ///
    /// # Errors
    ///
    /// The write was refused, or its frame could not be kept.
    pub fn publish(
        &self,
        write: impl FnOnce(
            &mut tilo_peer_state::StatePeer,
        ) -> Result<neighbour_wire::NeighbourMsg, String>,
    ) -> Result<neighbour_wire::NeighbourMsg, String> {
        self.sync.publish(write)
    }

    /// The caller a request made on this device carries: its person, and this device.
    #[must_use]
    pub fn caller(&self, user: [u8; 32]) -> tilo_actions::remote::Caller {
        tilo_actions::remote::Caller::authenticated(user, self.device())
    }

    /// The host's actions in `workspace`, asked over the actions link this device holds open to
    /// it, as its session asks them ([`tilo_session_client::session`]): the same link a device
    /// asks every action it makes on a host over.
    ///
    /// # Errors
    ///
    /// The host did not answer its actions link.
    pub async fn asking_in(&self, workspace: &str) -> Result<RemoteDevice, Refusal> {
        self.asking_device_in(workspace, self.host).await
    }

    /// [`Self::asking_in`], a request addressed to `target` rather than the host: a device of
    /// the workspace, which the host passes the request on to.
    ///
    /// # Errors
    ///
    /// The host did not answer its actions link.
    pub async fn asking_device_in(
        &self,
        workspace: &str,
        target: [u8; 32],
    ) -> Result<RemoteDevice, Refusal> {
        let link = self.dialer.link(Purpose::Actions).await.map_err(refused)?;
        let service = tilo_peer_state::actions::ActionService::new(
            self.sk.clone(),
            Arc::new(tilo_peer_state::actions::NoActions),
        );
        Ok(service.remote_over(Arc::new(link), target, workspace))
    }

    /// This device's link to `workspace` on `server`, from the endpoint its other links run
    /// from: a device opens a link for each workspace it holds, and one endpoint under its key.
    ///
    /// # Errors
    ///
    /// The host refused the device, or the link did not open.
    pub async fn reopen(&self, server: &Server, workspace: WorkspaceId) -> Result<Self, Refusal> {
        #[cfg(feature = "web")]
        let Some(endpoint) = &self.endpoint else {
            return Self::open_web(server, &self.device, workspace).await;
        };
        #[cfg(not(feature = "web"))]
        let Some(endpoint) = &self.endpoint else {
            return Err(Refusal::Other("this device's link has no endpoint".into()));
        };
        let dialer = Arc::new(Dialer::from_endpoint(
            tilo_server_client::ServerAddress::at(server.node_addr()),
            endpoint.clone(),
        ));
        let workspaces = server.workspaces().expect("the server holds workspaces");
        let asking = tilo_peer_state::actions::ActionService::new(
            self.sk.clone(),
            Arc::new(tilo_peer_state::actions::NoActions),
        )
        .remote(endpoint, &server.node_addr(), &workspace.to_string());
        Self::joined(
            workspaces,
            &self.device,
            workspace,
            dialer,
            asking,
            Some(endpoint.clone()),
            None,
        )
        .await
    }

    /// [`Self::reopen`] for a host a test runs itself ([`Hosted`]).
    ///
    /// # Errors
    ///
    /// The host refused the device, or the link did not open.
    pub async fn reopen_hosted(
        &self,
        hosted: &Hosted,
        workspace: WorkspaceId,
    ) -> Result<Self, Refusal> {
        #[cfg(feature = "web")]
        let Some(endpoint) = &self.endpoint else {
            return Self::open_hosted(hosted, &self.device, workspace).await;
        };
        #[cfg(not(feature = "web"))]
        let Some(endpoint) = &self.endpoint else {
            return Err(Refusal::Other("this device's link has no endpoint".into()));
        };
        let dialer = Arc::new(Dialer::from_endpoint(
            tilo_server_client::ServerAddress::at(hosted.host.node_addr()),
            endpoint.clone(),
        ));
        let workspaces = hosted.host.workspaces();
        let asking = tilo_peer_state::actions::ActionService::new(
            self.sk.clone(),
            Arc::new(tilo_peer_state::actions::NoActions),
        )
        .remote(endpoint, &hosted.host.node_addr(), &workspace.to_string());
        Self::joined(
            workspaces,
            &self.device,
            workspace,
            dialer,
            asking,
            Some(endpoint.clone()),
            None,
        )
        .await
    }

    /// The next frame a link brought this device's copy, or `None` once the link ended.
    pub async fn next(&mut self) -> Option<Heard> {
        self.reader.recv().await
    }

    /// Ticks whenever this device's copy changed, by its own write or a link's.
    #[must_use]
    pub fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.sync.changes()
    }

    /// What the host made of this device's own writes, on this link.
    #[must_use]
    pub fn answers(&self) -> tokio::sync::broadcast::Receiver<tilo_peer_state::direct::Answered> {
        self.sync.answers()
    }

    /// Wait until the link ends, from either end: the host refused the device, or closed the
    /// link, or this device let it go.
    ///
    /// # Errors
    ///
    /// The host's refusal, when it ended the link with one.
    pub async fn closed(&mut self) -> Result<(), Refusal> {
        let Some(running) = self.running.take() else {
            return Ok(());
        };
        match running.await {
            Ok(answer) => answer,
            Err(_) => Ok(()),
        }
    }

    /// A device of `workspace` on the server `server`, dialling its records link as it holds the
    /// workspace in the host's roster.
    ///
    /// # Errors
    ///
    /// The host refused the device, or the link did not open.
    pub async fn open(
        server: &Server,
        device: &TestDevice,
        workspace: WorkspaceId,
    ) -> Result<Self, Refusal> {
        let endpoint = device_endpoint(device).await;
        let dialer = Arc::new(Dialer::from_endpoint(
            tilo_server_client::ServerAddress::at(server.node_addr()),
            endpoint.clone(),
        ));
        let workspaces = server.workspaces().expect("the server holds workspaces");
        let asking = tilo_peer_state::actions::ActionService::new(
            device.sk.clone(),
            Arc::new(tilo_peer_state::actions::NoActions),
        )
        .remote(&endpoint, &server.node_addr(), &workspace.to_string());
        Self::joined(
            workspaces,
            device,
            workspace,
            dialer,
            asking,
            Some(endpoint),
            None,
        )
        .await
    }

    /// [`Self::open`], starting this device's copy from `seed`: what a device holds of the
    /// workspace before its link opens, so what it lacks reaches it over the link.
    ///
    /// # Errors
    ///
    /// The host refused the device, or the link did not open.
    pub async fn open_seeded(
        server: &Server,
        device: &TestDevice,
        workspace: WorkspaceId,
        seed: tilo_peer_state::StatePeer,
    ) -> Result<Self, Refusal> {
        let endpoint = device_endpoint(device).await;
        let dialer = Arc::new(Dialer::from_endpoint(
            tilo_server_client::ServerAddress::at(server.node_addr()),
            endpoint.clone(),
        ));
        let workspaces = server.workspaces().expect("the server holds workspaces");
        let asking = tilo_peer_state::actions::ActionService::new(
            device.sk.clone(),
            Arc::new(tilo_peer_state::actions::NoActions),
        )
        .remote(&endpoint, &server.node_addr(), &workspace.to_string());
        Self::joined(
            workspaces,
            device,
            workspace,
            dialer,
            asking,
            Some(endpoint),
            Some(seed),
        )
        .await
    }

    /// [`Self::open`] over the packet relay the host names, and nothing else: the shape a page's
    /// transport has, which no socket address shortcuts.
    ///
    /// # Errors
    ///
    /// The host refused the device, or the link did not open.
    pub async fn open_relayed(
        server: &Server,
        device: &TestDevice,
        workspace: WorkspaceId,
    ) -> Result<Self, Refusal> {
        let relay = server
            .node_addr()
            .relay_urls()
            .next()
            .cloned()
            .expect("the host names its packet relay");
        let addr = iroh_base::EndpointAddr::new(server.node_addr().id).with_relay_url(relay);
        let address = tilo_server_client::ServerAddress::at(addr.clone())
            .over(tilo_server_client::Carrier::Relay);
        let dialer = Arc::new(Dialer::new(
            address,
            Some(device.sk.verifying_key().to_bytes()),
        ));
        let endpoint = dialer.endpoint().await.map_err(refused)?;
        let workspaces = server.workspaces().expect("the server holds workspaces");
        let asking = tilo_peer_state::actions::ActionService::new(
            device.sk.clone(),
            Arc::new(tilo_peer_state::actions::NoActions),
        )
        .remote(&endpoint, &addr, &workspace.to_string());
        Self::joined(
            workspaces,
            device,
            workspace,
            dialer,
            asking,
            Some(endpoint),
            None,
        )
        .await
    }

    /// [`Self::open`] over the browser's WebTransport: the page's way, which a native device
    /// can dial too. The device proves itself on each link's first stream.
    ///
    /// # Errors
    ///
    /// The host refused the device, or the link did not open.
    #[cfg(feature = "web")]
    pub async fn open_web(
        server: &Server,
        device: &TestDevice,
        workspace: WorkspaceId,
    ) -> Result<Self, Refusal> {
        let workspaces = server.workspaces().expect("the server holds workspaces");
        let address = tilo_server_client::ServerAddress::at(server.node_addr())
            .with_web(server.web_link())
            .over(tilo_server_client::Carrier::WebTransport);
        let dialer = Arc::new(Dialer::new(
            address,
            Some(device.sk.verifying_key().to_bytes()),
        ));
        let asking = tilo_peer_state::actions::ActionService::new(
            device.sk.clone(),
            Arc::new(tilo_peer_state::actions::NoActions),
        )
        .remote_over(
            Arc::new(dialer.link(Purpose::Actions).await.map_err(refused)?),
            workspaces.host_device(),
            &workspace.to_string(),
        );
        Self::joined(workspaces, device, workspace, dialer, asking, None, None).await
    }

    /// [`Self::open`] for a host a test runs itself ([`Hosted`]).
    ///
    /// # Errors
    ///
    /// The host refused the device, or the link did not open.
    pub async fn open_hosted(
        hosted: &Hosted,
        device: &TestDevice,
        workspace: WorkspaceId,
    ) -> Result<Self, Refusal> {
        let endpoint = device_endpoint(device).await;
        let dialer = Arc::new(Dialer::from_endpoint(
            tilo_server_client::ServerAddress::at(hosted.host.node_addr()),
            endpoint.clone(),
        ));
        let workspaces = hosted.host.workspaces();
        let asking = tilo_peer_state::actions::ActionService::new(
            device.sk.clone(),
            Arc::new(tilo_peer_state::actions::NoActions),
        )
        .remote(&endpoint, &hosted.host.node_addr(), &workspace.to_string());
        Self::joined(
            workspaces,
            device,
            workspace,
            dialer,
            asking,
            Some(endpoint),
            None,
        )
        .await
    }

    async fn joined(
        workspaces: &Workspaces,
        device: &TestDevice,
        workspace: WorkspaceId,
        dialer: Arc<Dialer>,
        actions: RemoteDevice,
        endpoint: Option<Endpoint>,
        seed: Option<tilo_peer_state::StatePeer>,
    ) -> Result<Self, Refusal> {
        let host = workspaces.host_device();
        let host_cert = workspaces.device_cert().clone();
        let state = match seed {
            Some(seed) => seed,
            None => match Box::pin(workspaces.state(&workspace.to_string())).await {
                Some(state) => state,
                // The host holds no such workspace, and would refuse the dial the same way once
                // it named it.
                None => {
                    return Err(Refusal::Other("the host holds no such workspace".into()));
                }
            },
        };
        // A device holds its own certificate, whether or not the workspace's roster names it: a
        // link dialled from that copy proves the device by it, and the host decides its
        // admission.
        let listed = state
            .certificates()
            .iter()
            .any(|certificate| certificate.device_public_key == device.cert.device_public_key);
        let sync = DirectSync::new(state, device.sk.clone());
        let sync = if listed {
            sync
        } else {
            sync.placed(device.cert.clone()).map_err(Refusal::Other)?
        };
        let link = dialer.link(Purpose::Records).await.map_err(refused)?;
        link.prove_member(&device.cert, &device.sk, &host_cert)
            .await
            .map_err(refused)?;
        let running_session = sync.dial_over(Arc::new(link), host).await?;
        let reader = sync.reader();
        let running = sync.clone();
        let running = tokio::spawn(async move { running_session.run(&running).await });
        Ok(Self {
            sync,
            actions,
            host,
            sk: device.sk.clone(),
            device: device.clone(),
            endpoint,
            reader,
            dialer,
            running: Some(running),
        })
    }
}

impl std::fmt::Debug for Linked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Linked")
            .field("device", &hex::encode(self.device()))
            .finish_non_exhaustive()
    }
}

impl Drop for Linked {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            running.abort();
        }
    }
}

/// Why a link did not open, as the typed refusal the host gave or one of this side's own.
fn refused(error: LinkError) -> Refusal {
    match error {
        LinkError::Refused(reason) => Refusal::NotAMember(reason),
        LinkError::DeviceRevoked => Refusal::NotAMember("the device is revoked".into()),
        LinkError::CertificateRejected => Refusal::NotAdmitted,
        LinkError::ServerChanged => Refusal::Other("the host proved another device".into()),
        other => Refusal::Other(other.to_string()),
    }
}

/// A device of `workspace` that the host admits, or the typed refusal it gave.
pub async fn try_connect(
    server: &Server,
    device: &TestDevice,
    workspace: WorkspaceId,
) -> Result<Linked, Refusal> {
    Linked::open(server, device, workspace).await
}

/// A device of `workspace` that the host admits.
pub async fn connect(server: &Server, device: &TestDevice, workspace: WorkspaceId) -> Linked {
    try_connect(server, device, workspace)
        .await
        .expect("the workspace admits its own member")
}

/// The next frame `linked`'s copy takes that `want` accepts: the wait ends as soon as one
/// arrives, and only a hang reaches the bound.
pub async fn next(linked: &mut Linked, mut want: impl FnMut(&Heard) -> bool) -> Heard {
    tilo_test_env::within("the next frame", async {
        loop {
            let heard = linked.next().await.expect("the link stays open");
            if want(&heard) {
                return heard;
            }
        }
    })
    .await
}

/// Wait until `linked`'s copy holds what `holds` asks of it. The workspace's own changes
/// before it are passed over.
pub async fn receives(linked: &Linked, mut holds: impl FnMut(&tilo_peer_state::StatePeer) -> bool) {
    let mut changes = linked.changes();
    tilo_test_env::within("the change arrives", async move {
        loop {
            if linked.read(&mut holds) {
                return;
            }
            let _ = changes.changed().await;
        }
    })
    .await;
}

/// Whether `linked` hears nothing for `wait`.
pub async fn quiet(linked: &mut Linked, wait: std::time::Duration) -> bool {
    tokio::time::timeout(wait, linked.next()).await.is_err()
}

/// `author`'s documents at `routes`, as its own copy holds them, enter `workspace` as a member's
/// first write of a document does: whole, through `linked`. Returns once the host holds every
/// one.
pub async fn found_documents(
    server: &TestServer,
    linked: &Linked,
    author: &TestDevice,
    workspace: WorkspaceId,
    routes: &[(&str, &str)],
) {
    let workspaces = server
        .server
        .workspaces()
        .expect("the server holds workspaces");
    let mut changes = workspaces.governance_changes();
    for (app, id) in routes {
        let document = linked
            .read(|peer| peer.document(app, id))
            .expect("the device holds the document");
        let message = linked
            .publish(|peer| {
                peer.publication_mut().publish_document(
                    app,
                    id,
                    document,
                    &author.sk,
                    std::time::SystemTime::now(),
                )
            })
            .unwrap();
        let (_, kind) = neighbour_wire::retention(&message, false).unwrap();
        assert_eq!(
            kind,
            neighbour_wire::Retention::Checkpoint,
            "a whole document"
        );
    }
    let workspace = workspace.to_string();
    tilo_test_env::within("the server to hold the founded documents", async {
        loop {
            let held = workspaces
                .document_routes(&workspace)
                .await
                .unwrap_or_default();
            if routes
                .iter()
                .all(|(app, id)| held.iter().any(|(a, d)| a == app && d == id))
            {
                return;
            }
            let _ = changes.recv().await;
        }
    })
    .await;
}

/// Wait until `server` has accepted `peer`'s subscription to `workspace`.
///
/// From then on a delta published on `server` reaches `peer`; a test waits for
/// it instead of sleeping and hoping the handshake finished in time.
pub async fn wait_until_subscribed(server: &Server, workspace: WorkspaceId, peer: &[u8; 32]) {
    tilo_test_env::within("the peer subscribed to the workspace", async {
        while !server.is_subscribed(workspace, peer).await {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
}

/// Poll `cond` until it holds, failing the test naming `what` if it never
/// does.
pub async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    tilo_test_env::within(what, async {
        while !cond() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

fn phase_bytes() -> &'static std::sync::Mutex<std::collections::VecDeque<u8>> {
    static BUFFER: std::sync::OnceLock<std::sync::Mutex<std::collections::VecDeque<u8>>> =
        std::sync::OnceLock::new();
    BUFFER.get_or_init(|| {
        std::sync::Mutex::new(std::collections::VecDeque::with_capacity(128 * 1024))
    })
}
struct PhaseWriter;
impl std::io::Write for PhaseWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut buffer = phase_bytes().lock().unwrap();
        for byte in bytes {
            if buffer.len() == 128 * 1024 {
                buffer.pop_front();
            }
            buffer.push_back(*byte);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub fn print_phase_diagnostic() {
    let bytes = phase_bytes()
        .lock()
        .unwrap()
        .iter()
        .copied()
        .collect::<Vec<_>>();
    eprintln!("{}", String::from_utf8_lossy(&bytes));
}

/// The app surfaces a process decides received changes against
/// (`tilo_peer_state::decide_with_surfaces`) are one per process, and a
/// server or replica reads them when it starts. A test that installs some
/// runs in a process of its own ([`in_own_process`]), so no other test of
/// this binary ever runs under them.
pub fn install_process_surfaces(
    surfaces: collab_types::Surfaces,
    of: impl Fn(&str) -> Option<[u8; 32]> + Send + Sync + 'static,
) {
    tilo_peer_state::decide_with_surfaces(surfaces, of);
}

/// Names the one test a process of its own runs, see [`in_own_process`].
const OWN_PROCESS: &str = "TILO_SERVER_TEST_OWN_PROCESS";

/// Whether the test `test` (its path in this binary) is to run its body
/// here: in the process this binary started for it alone. Anywhere else the
/// test runs this binary again for itself, waits for it to pass, and answers
/// `false`, leaving nothing more to do.
pub fn in_own_process(test: &str) -> bool {
    if std::env::var_os(OWN_PROCESS).is_some_and(|own| own == test) {
        return true;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(OWN_PROCESS, test)
        .stderr(std::process::Stdio::inherit())
        .output_locked()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    eprintln!("{stdout}");
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed"),
        "{test} did not pass in its own process: {}",
        output.status
    );
    false
}

/// The laptop's own service under `state`, opened as a device opens its own, with its key
/// service beside it.
pub async fn own_service(
    state: &std::path::Path,
    laptop: &TestDevice,
) -> (
    tilo_server::service::WorkspaceService,
    tilo_workspace_service::own_device::KeyService,
) {
    let signer: std::sync::Arc<dyn collab_types::signer::DeviceSigner> =
        std::sync::Arc::new(laptop.sk.clone());
    let keys = tilo_workspace_service::own_device::KeyService::open(
        "laptop-groups",
        &state.join("groups"),
        key_group::VaultKey::new([3; 32]),
        std::sync::Arc::clone(&signer),
        std::sync::Arc::new(tilo_workspace_keys::NoSealChecks),
    )
    .expect("the laptop's key service opens");
    let service = tilo_workspace_service::own_device::open(
        state.join("service"),
        tilo_workspace_service::own_device::Device {
            cert: &laptop.cert,
            signer: &signer,
            itself: None,
        },
        device_endpoint(laptop).await,
        keys.groups(),
        |config| config,
    )
    .await
    .expect("the laptop's own service opens");
    (service, keys)
}

/// The keys of `workspace`'s group that reads `record` of `app`, as `at`'s device holds them.
/// Keys are shared only among one person's own devices: a member device of that person seals
/// with these, and a holder of anyone else carries what it seals without them.
pub async fn keys_of(
    at: &Hosted,
    workspace: WorkspaceId,
    (app, record): (&str, &str),
) -> key_group::ContentKeys {
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: app.into(),
        cluster_id: workspace.to_string(),
        document: record.into(),
    };
    tilo_test_env::within("the person's device holds the record's keys", async {
        loop {
            match at.host.workspaces().own_route_keys(&route, true).await {
                Some(Ok(keys)) => return keys,
                Some(Err(error)) => panic!("the record's keys do not open: {error}"),
                None => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
    })
    .await
}

/// Write `document` as the whole of `record` of `app`, sealed with `keys`, as `linked`'s device
/// writes it: a member device's first sealed write of a record is the record itself. Answers the
/// sealed write, the bytes every holder carries whether or not it reads them.
pub fn write_sealed(
    linked: &Linked,
    keys: &key_group::ContentKeys,
    (app, record): (&str, &str),
    document: Vec<u8>,
) -> collab_types::SignedOp {
    let workspace = linked.read(|peer| peer.workspace_id().to_string());
    let artifact = collab_types::document_artifact_id(app, &workspace, record);
    let mut seal = |plaintext: &[u8]| {
        keys.seal_content(
            artifact,
            collab_types::ContentRetention::Checkpoint,
            plaintext,
        )
        .map_err(|error| error.to_string())
    };
    let written = linked
        .publish(|peer| {
            peer.publication_mut().publish_sealed_checkpoint(
                app,
                record,
                document,
                &mut seal,
                &linked.sk,
                std::time::SystemTime::now(),
            )
        })
        .expect("the member's sealed write is kept");
    let neighbour_wire::NeighbourMsg::AppDocument { envelope, .. } = written else {
        panic!("a record write is a document frame: {written:?}")
    };
    *envelope
}

/// Whether `peer` holds every one of `writes` to `record` of `app` as itself: what a holder
/// carries of a sealed record, read or not.
#[must_use]
pub fn holds_writes(
    peer: &tilo_peer_state::StatePeer,
    (app, record): (&str, &str),
    writes: &[collab_types::SignedOp],
) -> bool {
    writes
        .iter()
        .all(|write| peer.publication().holds_sealed(app, record, write))
}

/// Wait until `at`'s own copy of `workspace` holds every one of `writes` to `record` of `app`.
pub async fn host_holds(
    at: &Hosted,
    workspace: WorkspaceId,
    route: (&str, &str),
    writes: &[collab_types::SignedOp],
) {
    let what = format!("the host holds the carried writes of {}", route.1);
    tilo_test_env::within(&what, async {
        loop {
            if at
                .host
                .workspaces()
                .state(&workspace.to_string())
                .await
                .is_some_and(|state| holds_writes(&state, route, writes))
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
}
