#![allow(
    dead_code,
    reason = "each integration module uses a different subset of these fixtures"
)]
#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

pub mod compute;

use std::path::PathBuf;
use tilo_test_env::SpawnUnderEnvLock as _;

use ed25519_dalek::SigningKey;
use iroh::endpoint::{Endpoint, presets};
use iroh::protocol::Router;
use tempfile::TempDir;

use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity, sign_device_cert,
};

use tilo_server::{
    Envelope, ServerClient, ServerConfig, WorkspaceId,
    daemon::{Server, ServerIdentity},
    service::{HostConfig, HostIdentity, WorkspaceService},
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
    members: &[[u8; 32]],
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
    members: &[[u8; 32]],
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
    members: &[[u8; 32]],
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
    members: &[[u8; 32]],
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
    let mut cfg = ServerConfig {
        state_root: state_root.to_path_buf(),
        ..Default::default()
    };
    let identity = ServerIdentity {
        user_public_key: user.user_pk,
        device_signing_key: server_dev.sk.clone(),
        device_cert: server_dev.cert.clone(),
        revocations: RevocationList::new(),
    };
    cfg.services.app_serving = server.is_some();
    let server = start_server_serving(cfg, identity, server).await.unwrap();
    for id in workspaces {
        admit_members(&server, *id, members, state_root).await;
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
            cfg,
            identity,
            server,
            test_server::relay_mode(),
        )
        .await
    })
}

/// Make `members` the workspace's member devices, as the workspace's governor does:
/// the devices are all the server owner's.
pub async fn admit_members(
    server: &Server,
    workspace: WorkspaceId,
    members: &[[u8; 32]],
    state_root: &std::path::Path,
) {
    server
        .replace_governed_workspace_access(
            workspace,
            true,
            tilo_peer_state::GovernedAccess {
                member_devices: members.to_vec(),
                member_user_roots: vec![server.own_user_public_key()],
            },
            state_root.to_path_buf(),
        )
        .await
        .unwrap();
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

    /// Stop the host, keeping its state for [`host_in`] to open again.
    pub async fn stop_keeping(self) -> TempDir {
        self.router.shutdown().await.unwrap();
        self.host.shutdown().await;
        self.state
    }
}

pub async fn host_on(user: &TestUser, device: &TestDevice, work: WorkSetting) -> Hosted {
    host_in(user, device, work, tempfile::tempdir().unwrap()).await
}

/// A host as [`host_on`] makes one, over the state `state` holds.
pub async fn host_in(
    user: &TestUser,
    device: &TestDevice,
    work: WorkSetting,
    state: TempDir,
) -> Hosted {
    let endpoint = device_endpoint(device).await;
    let host = WorkspaceService::open(
        HostConfig::new(state.path().to_path_buf(), work),
        HostIdentity {
            user_public_key: user.user_pk,
            device_signing_key: device.sk.clone(),
            device_cert: device.cert.clone(),
            revocations: RevocationList::new(),
        },
        endpoint.clone(),
        None,
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

/// A session of `device` to the certified host `host` of its own person.
pub async fn session_to(user: &TestUser, device: &TestDevice, host: &Hosted) -> ServerClient {
    ServerClient::connect_v2_to_enrolled_home_peer(
        device_endpoint(device).await,
        host.host.node_addr(),
        user.user_pk,
        host.host.device_cert().device_public_key,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap()
}

/// Wait until `client` receives the change `body`. The workspace's own
/// changes before it (what its holders say of their copies, among others)
/// are passed over.
pub async fn receives(client: &ServerClient, body: &[u8]) {
    tilo_test_env::within("the change arrives", async {
        loop {
            if let Envelope::Delta { delta, .. } =
                client.recv().await.expect("the session stays open")
                && delta == body
            {
                break;
            }
        }
    })
    .await;
}

/// The publisher a shell is: a coedit session of `author` to `relay`, dialing
/// as the shell dials an enrolled home peer, once the server has it.
pub async fn publisher(
    server: &TestServer,
    user: &TestUser,
    author: &TestDevice,
    workspace: WorkspaceId,
) -> tilo_server::ServerCoeditSession {
    let publisher = tilo_server::ServerCoeditSession::join(
        ServerClient::connect_v2_to_enrolled_home_peer(
            client_endpoint().await,
            server.server.node_addr(),
            user.user_pk,
            server.server.handle().mesh_profile().device,
            author.cert.clone(),
            author.sk.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap(),
        workspace,
        author.device_pubkey(),
    )
    .unwrap();
    tilo_test_env::within("the publisher's subscription", async {
        while !matches!(
            publisher.recv().await,
            Some(tilo_server::Inbound::Ready { .. })
        ) {}
    })
    .await;
    publisher
}

/// A client of `device` subscribed to `workspace` on the server at `addr`, once
/// the server admits it.
pub async fn connect(
    user: &TestUser,
    device: &TestDevice,
    addr: iroh_base::EndpointAddr,
    workspace: WorkspaceId,
) -> ServerClient {
    let client = ServerClient::connect(
        client_endpoint().await,
        addr,
        user.user_pk,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    client
        .send(Envelope::Subscribe {
            workspace_id: workspace,
        })
        .unwrap();
    next(&client, |env| matches!(env, Envelope::Ready { .. })).await;
    client
}

/// How many queued deltas a server's state root holds in `dir`.
pub fn pending_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |rd| {
        rd.flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".delta"))
            .count()
    })
}

/// The next envelope from `client` that `want` accepts: the wait ends as soon
/// as one arrives, and only a hang reaches the bound.
pub async fn next(client: &ServerClient, mut want: impl FnMut(&Envelope) -> bool) -> Envelope {
    tilo_test_env::within("the next envelope", async {
        loop {
            let env = client.recv().await.expect("session open");
            if want(&env) {
                return env;
            }
        }
    })
    .await
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
