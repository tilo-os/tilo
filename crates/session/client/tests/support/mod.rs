//! Fixtures shared by the end-to-end tests: a host serving browsers over
//! HTTPS with a development certificate, and certified devices.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of these fixtures"
)]

pub mod https;

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tilo_session_client::{Cmd, Link, Outbox, TransportPhase};

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable as _};
use collab_types::DocumentMutation;
use ed25519_dalek::{Signer as _, SigningKey};
use getrandom::{SysRng, rand_core::UnwrapErr};
use sha2::{Digest as _, Sha256};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay::certify::{ApproverDecision, ApproverReply};
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::web::{TlsSource, WebConfig, WebHandle};
use tilo_relay::workspaces::PreAdmitRequest;
use tilo_relay_client::Host;
use tilo_relay_client::{Approver, Carrier};
use tilo_relay_protocol::workspaces::{JoinReply, PreAdmission, WorkspaceRole};
use tilo_session_client::{MemoryPeerStore, PeerConfig};
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::{
    CertificationPolicy, DeviceCapabilities, DeviceCert, RevocationList, issue_device_cert,
    sign_device_cert,
};
use collab_types::WorkspaceId;

pub const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

/// A test's scratch directory for a host's or a participant's state, removed
/// with it. It is in memory where the system has one
/// ([`tilo_paths::memory_temp_dir`], `/dev/shm` on Linux), as `tilo-app`'s
/// test profiles are. Every write still syncs as it does for a person, but a
/// sync in memory costs nothing, where on a busy build node's shared disk one
/// waited up to seconds for every other writer's data: a host's channel
/// admission is a chain of synced writes (request, admission, acknowledgement),
/// and a page's keys then took longer than the host waits for them. These
/// tests prove what a page and a host do, not that a disk keeps data; the
/// relay's durability tests keep the real disk.
pub fn scratch_dir() -> tempfile::TempDir {
    if let Some(memory) = tilo_paths::memory_temp_dir()
        && let Ok(directory) = tempfile::Builder::new()
            .prefix("browser-peer-test-")
            .tempdir_in(memory)
    {
        return directory;
    }
    tempfile::tempdir().unwrap()
}

/// The directory holding the runtime module (`pkg/`) a page test's driver
/// serves beside the page's own sources.
///
/// nextest builds it once for a run and hands it over as `TILO_PAGE_BUNDLE`
/// (`.config/nextest.toml`, `tools/build-page-bundle.sh`). A run that names
/// none — a plain `cargo test` — builds it here, once, before any driver
/// starts: built inside the driver, the wasm compile happened inside the
/// time the test gives the page to create its identity, and waited on
/// cargo's build lock behind the run's own compiles, so the page looked
/// hung and the test failed on a phase the page had not reached.
#[expect(
    clippy::expect_used,
    reason = "a fixture's module that cannot be built or handed over is the failure it reports"
)]
pub fn page_bundle() -> std::path::PathBuf {
    static BUNDLE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BUNDLE
        .get_or_init(|| {
            if let Some(named) = std::env::var_os("TILO_PAGE_BUNDLE") {
                return std::path::PathBuf::from(named);
            }
            // The one build path, which hands the directory it built to a
            // nextest run through NEXTEST_ENV: a test reads it the same way.
            let handed = tempfile::NamedTempFile::new().unwrap();
            let mut build = std::process::Command::new("bash");
            build
                .arg(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../../tools/build-page-bundle.sh"),
                )
                .env("NEXTEST_ENV", handed.path());
            let exit = build
                .status_locked()
                .expect("the page's runtime module build starts");
            assert!(
                exit.success(),
                "the page's runtime module did not build ({exit})"
            );
            let named = std::fs::read_to_string(handed.path()).unwrap();
            let named = named
                .trim()
                .strip_prefix("TILO_PAGE_BUNDLE=")
                .expect("the build hands over the module it built");
            std::path::PathBuf::from(named)
        })
        .clone()
}

/// The test hosts' devices. A governed workspace's host writes the first state
/// of each record a member starts; the tests read what members wrote.
static HOSTS: std::sync::Mutex<std::collections::BTreeSet<[u8; 32]>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Whether `device` is a test host's.
pub fn is_host(device: &[u8; 32]) -> bool {
    HOSTS.lock().unwrap().contains(device)
}

/// Whether `event` is a document a member wrote, not one of a host's.
pub fn workspace() -> u64 {
    tilo_relay_protocol::WorkspaceId::parse(WORKSPACE).unwrap()
}

/// Have `relay` govern [`WORKSPACE`], the person's workspace, unless it does,
/// and admit `device` to it as its owner: what a page asks of its host
/// before it joins ([`Outbox::join_placed`](tilo_session_client::Outbox::join_placed)), done here for a peer
/// that reaches the host without the route that administers them.
pub async fn place(relay: &Relay, device: &DeviceCert) {
    let workspaces = relay.workspaces().unwrap();
    if workspaces.summary(WORKSPACE).await.is_err()
        && let Err(error) = workspaces
            .create_workspace("Workspace", Some(WORKSPACE))
            .await
    {
        // Another peer of the test may have placed it meanwhile.
        assert!(workspaces.summary(WORKSPACE).await.is_ok(), "{error}");
    }
    workspaces.enter(WORKSPACE, device).await.unwrap();
}

pub struct Device {
    pub key: SigningKey,
    pub cert: DeviceCert,
}

pub fn enrol(root: &SigningKey, label: &str, capabilities: DeviceCapabilities) -> Device {
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    Device { key, cert }
}

pub struct TestHost {
    pub relay: Relay,
    pub web: WebHandle,
}

pub async fn start_host(
    root: &SigningKey,
    host: &Device,
    state: &Path,
    web_listen: SocketAddr,
) -> TestHost {
    start_host_plain(root, host, state, web_listen, None).await
}

/// [`start_host`], also serving plain HTTP at `plain_listen`.
pub async fn start_host_plain(
    root: &SigningKey,
    host: &Device,
    state: &Path,
    web_listen: SocketAddr,
    plain_listen: Option<SocketAddr>,
) -> TestHost {
    start_host_admitting(root, host, state, web_listen, plain_listen, &[]).await
}

/// [`start_host_plain`], whose operator trust policy lets the people
/// `guests` (user roots) connect. Connecting is not membership: a guest
/// enters a workspace only through its invitation ([`door`], [`redeem`]).
pub async fn start_host_admitting(
    root: &SigningKey,
    host: &Device,
    state: &Path,
    web_listen: SocketAddr,
    plain_listen: Option<SocketAddr>,
    guests: &[[u8; 32]],
) -> TestHost {
    start_host_limiting(root, host, state, web_listen, plain_listen, guests, None).await
}

/// [`start_host_admitting`], holding workspace guests to the change and content
/// budgets `limits`, as a public host does.
pub async fn start_host_limiting(
    root: &SigningKey,
    host: &Device,
    state: &Path,
    web_listen: SocketAddr,
    plain_listen: Option<SocketAddr>,
    guests: &[[u8; 32]],
    limits: Option<tilo_relay::member_limits::MemberLimitsConfig>,
) -> TestHost {
    start_host_serving(
        root,
        host,
        state,
        web_listen,
        HostSettings {
            plain_listen,
            guests,
            limits,
            fetch_proxy: None,
            ..HostSettings::default()
        },
    )
    .await
}

/// A host restarted with its state reset, at the ports the kernel picks: the
/// same identity, and so the same endpoint its pages dial, which reaches its
/// relay as it comes up, as a host process restarted does. Nothing of the
/// stopped host's address is waited for or taken back.
pub async fn restart_host(root: &SigningKey, host: &Device, state: &Path) -> TestHost {
    start_host(root, host, state, "127.0.0.1:0".parse().unwrap()).await
}

/// What a test host serves beyond its secure listener.
#[derive(Default)]
pub struct HostSettings<'a> {
    /// Plain HTTP too, here.
    pub plain_listen: Option<SocketAddr>,
    /// People (user roots) its operator trust policy lets connect.
    pub guests: &'a [[u8; 32]],
    /// The change and content budgets workspace guests are held to.
    pub limits: Option<tilo_relay::member_limits::MemberLimitsConfig>,
    /// The fetch proxy it serves its pages.
    pub fetch_proxy: Option<tilo_relay::web::FetchProxySetup>,
    /// Run its own packet relay, as a host serving pages does: a page holds
    /// no UDP socket and reaches it only through one.
    pub packet_relay: bool,
}

/// A host serving what `settings` names.
pub async fn start_host_serving(
    root: &SigningKey,
    host: &Device,
    state: &Path,
    web_listen: SocketAddr,
    settings: HostSettings<'_>,
) -> TestHost {
    let HostSettings {
        plain_listen,
        guests,
        limits,
        fetch_proxy,
        packet_relay,
    } = settings;
    let user = root.verifying_key().to_bytes();
    let mut cfg = tilo_relay::RelayConfig {
        state_root: state.to_path_buf(),
        member_limits: limits,
        ..Default::default()
    };
    if packet_relay {
        cfg.packet_relay = Some((std::net::Ipv4Addr::LOCALHOST, 0).into());
    }
    if !guests.is_empty() {
        cfg.trust_policy = tilo_relay::TrustPolicy::AllowList {
            users: guests.iter().map(hex::encode).collect(),
        };
    }
    let relay = Relay::start(
        cfg,
        RelayIdentity {
            user_public_key: user,
            device_signing_key: host.key.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let web = relay
        .serve_web(WebConfig {
            listen: web_listen,
            tls: TlsSource::Development {
                hosts: vec!["localhost".into(), "127.0.0.1".into()],
                store: Some(state.join("web-dev-identity")),
            },
            allowed_origins: Vec::new(),
            plain_listen,
            public_origin: None,
            fetch_proxy,
            door: None,
            link: None,
        })
        .await
        .unwrap();
    if packet_relay {
        relayed(&relay);
    }
    HOSTS.lock().unwrap().insert(host.cert.device_public_key);
    TestHost { relay, web }
}

/// A host that serves its own packet relay names it in its address from the start: what
/// `Relay::start` waits for before it answers anything.
fn relayed(relay: &Relay) {
    assert!(
        relay.node_addr().relay_urls().next().is_some(),
        "the host names its packet relay"
    );
}

/// A host that governs workspaces rather than one pinned workspace, serving
/// browsers on an ephemeral port. With `keep_web_identity` its development
/// certificate is stored under `state`, so a restarted host presents the
/// same one.
pub async fn start_workspace_host(
    root: &SigningKey,
    host: &Device,
    state: &Path,
    keep_web_identity: bool,
) -> TestHost {
    start_workspace_host_relaying(root, host, state, keep_web_identity, false).await
}

/// [`start_workspace_host`], running its own packet relay when
/// `packet_relay` says so.
pub async fn start_workspace_host_relaying(
    root: &SigningKey,
    host: &Device,
    state: &Path,
    keep_web_identity: bool,
    packet_relay: bool,
) -> TestHost {
    let config = tilo_relay::RelayConfig {
        state_root: state.to_path_buf(),
        packet_relay: packet_relay.then(|| (std::net::Ipv4Addr::LOCALHOST, 0).into()),
        ..Default::default()
    };
    let identity = RelayIdentity {
        user_public_key: root.verifying_key().to_bytes(),
        device_signing_key: host.key.clone(),
        device_cert: host.cert.clone(),
        revocations: RevocationList::new(),
    };
    // A host running its own packet relay is reached through it. One that
    // runs none is reached on the network the test runs in, and looks up no
    // public relay or address: an invitation names what a stranger can dial
    // of its host, which would otherwise be a relay and an address out on
    // the internet, and the test would stand on whether those answer.
    let relay = if packet_relay {
        Relay::start(config, identity).await
    } else {
        Relay::start_with_relay_mode(config, identity, iroh::RelayMode::Disabled).await
    }
    .unwrap();
    let web = relay
        .serve_web(WebConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            tls: TlsSource::Development {
                hosts: vec!["localhost".into(), "127.0.0.1".into()],
                store: keep_web_identity.then(|| state.join("web-dev-identity")),
            },
            allowed_origins: Vec::new(),
            plain_listen: None,
            public_origin: None,
            fetch_proxy: None,
            door: None,
            link: None,
        })
        .await
        .unwrap();
    if packet_relay {
        relayed(&relay);
    }
    HOSTS.lock().unwrap().insert(host.cert.device_public_key);
    TestHost { relay, web }
}

impl TestHost {
    pub fn https(&self) -> Arc<https::DevHttps> {
        https::DevHttps::new(self.web.addr, self.web.certificate_sha256.as_ref().unwrap())
    }

    /// The host as a session reaches it: its iroh endpoint, and the
    /// WebTransport listener pages reach it at.
    pub fn iroh(&self) -> Host {
        Host::at(self.relay.node_addr()).with_web(Some(self.web.link.clone()))
    }

    /// The host as a device reaches it over `carrier`.
    pub fn over(&self, carrier: Carrier) -> Host {
        self.iroh().over(carrier)
    }
}

/// Approve the one pending certification request as `laptop` (a root
/// holder), over the host's iroh certification ALPN.
pub async fn approve_one(host: &TestHost, root: &SigningKey, laptop: &Device) -> String {
    approve_one_over(&host.iroh(), root, laptop).await
}

/// [`approve_one`], with `laptop` reaching the host as `host` says.
pub async fn approve_one_over(host: &Host, root: &SigningKey, laptop: &Device) -> String {
    // The laptop approves over iroh, whichever way the new device came.
    let host = match host.carrier() {
        Carrier::WebTransport => host.clone().over(Carrier::Direct),
        _ => host.clone(),
    };
    let endpoint = host.bind(None).await.unwrap();
    let mut approver = Approver::connect(
        &endpoint,
        host.addr(),
        laptop.cert.clone(),
        &laptop.key,
        &RevocationList::new(),
    )
    .await
    .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    let pending = loop {
        if let Some(pending) = approver.list_pending().await.unwrap().pop() {
            break pending;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no pending certification request"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let cert = issue_device_cert(
        &pending.request,
        root,
        &CertificationPolicy::browser(),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    assert_eq!(
        approver
            .decide(pending.request_digest, ApproverDecision::Issue(cert))
            .await
            .unwrap(),
        ApproverReply::Decided
    );
    approver.close();
    pending.pairing_code
}

pub async fn wait_for<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    // A loaded runner makes reconnects and replays slow, not wrong: wait on
    // the shared hang bound rather than a count of polls.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub fn start_peer(host: Host, cert: DeviceCert, host_cert: DeviceCert, key: SigningKey) -> Link {
    start_peer_with_store(
        host,
        cert,
        host_cert,
        key,
        Arc::new(MemoryPeerStore::default()),
    )
}

/// [`start_peer`] over `store`, as a page reloaded from its profile.
pub fn start_peer_with_store(
    host: Host,
    cert: DeviceCert,
    host_cert: DeviceCert,
    key: SigningKey,
    store: Arc<MemoryPeerStore>,
) -> Link {
    tilo_session_client::start_session(
        PeerConfig {
            host,
            cert,
            host_cert,
            revocations: RevocationList::new(),
            entering: None,
        },
        Arc::new(key),
        store,
    )
}

/// A page's device key in custody: the session signs with it and never reads
/// it, and each signature answers on a later poll, as a browser's `WebCrypto`
/// key does. A page's endpoint therefore speaks under a key of its own, which
/// the device signs for.
pub struct PageCustody(SigningKey);

impl tilo_relay_client::DeviceSigner for PageCustody {
    fn device_public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    fn sign(&self, message: Vec<u8>) -> tilo_relay_client::SignFuture<'_> {
        let signature = ed25519_dalek::Signer::sign(&self.0, &message).to_bytes();
        let mut answered = false;
        Box::pin(std::future::poll_fn(move |context| {
            if std::mem::replace(&mut answered, true) {
                std::task::Poll::Ready(Ok(signature))
            } else {
                context.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        }))
    }
}

/// [`start_peer`] for a page: over `store`, its custody keeps the device key,
/// so its session dials from an endpoint the device delegated.
pub fn start_page(
    host: Host,
    cert: DeviceCert,
    host_cert: DeviceCert,
    key: SigningKey,
    store: Arc<MemoryPeerStore>,
) -> Link {
    tilo_session_client::start_session(
        PeerConfig {
            host,
            cert,
            host_cert,
            revocations: RevocationList::new(),
            entering: None,
        },
        Arc::new(PageCustody(key)),
        store,
    )
}

/// [`start_page`], its session entering a workspace through its door as it
/// opens, as a new visitor's page does.
pub fn enter_page(
    host: Host,
    cert: DeviceCert,
    host_cert: DeviceCert,
    key: SigningKey,
    store: Arc<MemoryPeerStore>,
    entering: tilo_relay_protocol::entering::Entering,
) -> Link {
    tilo_session_client::start_session(
        PeerConfig {
            host,
            cert,
            host_cert,
            revocations: RevocationList::new(),
            entering: Some(entering),
        },
        Arc::new(PageCustody(key)),
        store,
    )
}

pub async fn ready(peers: &[&Link]) {
    wait_for("sessions ready", || async {
        peers
            .iter()
            .all(|peer| peer.snapshot().phase == "ready")
            .then_some(())
    })
    .await;
}

/// Wait until the host admitted `peer` to `workspace`: connected, subscribed
/// and replayed. A session that has joined no workspace yet is
/// [`ready`] too, so a page that is about to act in one waits on that one
/// rather than on the session: an action sent before the host subscribed the
/// page is refused by the relay and never answered.
pub async fn admitted_to(peer: &Link, workspace: u64) {
    wait_for("the session admitted to its workspace", || {
        let status = peer.status();
        async move {
            (status.phase == TransportPhase::Connected
                && status.workspaces.get(&workspace) == Some(&true))
            .then_some(())
        }
    })
    .await;
}

/// Wait until a participant of the workspace admitted `peer` to the note's channel.
pub async fn admitted(peer: &Link) {
    wait_keyed(peer, &route()).await;
}

/// Whether a participant of the workspace admitted `peer` to `route`'s channel: it holds its keys.
pub fn keyed(peer: &Link, route: &DocumentRoute) -> bool {
    let prefix = format!("route {}/{} ", route.app_id, route.record);
    peer.status()
        .channels
        .lines()
        .any(|line| line.starts_with(&prefix) && line.contains(": ready;"))
}

/// Wait until a participant of the workspace admitted `peer` to `route`'s channel.
pub async fn wait_keyed(peer: &Link, route: &DocumentRoute) {
    wait_for("a participant to admit the page", || {
        let ready = keyed(peer, route);
        async move { ready.then_some(()) }
    })
    .await;
}

/// Write `mutation` to `route` from `peer`'s device, once a participant admitted it to the
/// record's channel, and wait until the host has it: what the workspace holds of that record from
/// then on.
pub async fn written(peer: &Link, route: &DocumentRoute, mutation: &DocumentMutation) {
    peer.send(Cmd::OpenRoute(route.clone())).unwrap();
    wait_keyed(peer, route).await;
    peer.write(route.clone(), mutation).unwrap();
    durable(peer).await;
}

pub fn route() -> DocumentRoute {
    DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: WORKSPACE.into(),
        record: "note-1".into(),
    }
}

/// A change to `doc` noting `text`, as the mutation a peer publishes of it: a real
/// Automerge change, since a replica applies what it keeps. The first change of a
/// document is the whole record, which founds it.
pub fn say(doc: &mut AutoCommit, text: &str) -> DocumentMutation {
    let founds = doc.get_heads().is_empty();
    doc.put(ROOT, text, true).unwrap();
    if founds {
        DocumentMutation::Snapshot(doc.save())
    } else {
        DocumentMutation::Incremental(doc.save_incremental())
    }
}

/// What `peer`'s replica of its workspace holds of `route` now, when it reads.
pub fn record_of(peer: &Link, route: &DocumentRoute) -> Option<AutoCommit> {
    let bytes = peer
        .engine()
        .record(&route.cluster_id, &route.app_id, &route.record)
        .ok()??;
    AutoCommit::load(&bytes).ok()
}

/// What `peer`'s replica says in `route`: every text [`say`] noted in it.
pub fn said(peer: &Link, route: &DocumentRoute) -> BTreeSet<String> {
    record_of(peer, route)
        .map(|doc| doc.keys(ROOT).collect())
        .unwrap_or_default()
}

/// Wait until `peer`'s replica says `text` in `route`.
pub async fn hears(peer: &Link, route: &DocumentRoute, text: &str) {
    in_replica(peer, route, &format!("the replica to say {text:?}"), || {
        said(peer, route).contains(text).then_some(())
    })
    .await;
}

/// Wait until `probe` finds what it reads in `peer`'s replica; a timeout says what the
/// session's channels and failures were.
async fn in_replica<T>(
    peer: &Link,
    route: &DocumentRoute,
    what: &str,
    mut probe: impl FnMut() -> Option<T>,
) -> T {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(found) = probe() {
            return found;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}; the replica reads {:?}; channels:\n{}\nfailures: {:?}",
            peer.engine()
                .record(&route.cluster_id, &route.app_id, &route.record)
                .map(|bytes| bytes.map(|bytes| bytes.len())),
            peer.status().channels,
            peer.status().failures
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Wait until every write `peer` published is durable on its host; a timeout says what the
/// session's channels, failures and custody were.
pub async fn durable(peer: &Link) {
    let receipt = peer.barrier().unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Ok(answer) = receipt.try_recv() {
            answer.unwrap();
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the writes to be durable; pending {}; channels:\n{}\nfailures: {:?}",
            peer.status().outgoing_pending,
            peer.status().channels,
            peer.status().failures
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// What `peer`'s replica last refused to keep and how many writes it refused, as its status
/// says it: `None` while it refused nothing.
pub fn refused(peer: &Link) -> Option<String> {
    peer.status()
        .channels
        .lines()
        .find(|line| line.starts_with("refused "))
        .map(str::to_owned)
}

/// Wait until `peer`'s replica of `route` holds every change `doc` holds, and answer it.
pub async fn holds(peer: &Link, route: &DocumentRoute, doc: &AutoCommit) -> AutoCommit {
    let heads = doc.clone().get_heads();
    in_replica(peer, route, "the replica to hold the record", || {
        record_of(peer, route).filter(|held| {
            let mut held = held.clone();
            heads
                .iter()
                .all(|head| held.get_change_meta_by_hash(head).is_some())
        })
    })
    .await
}

/// The workspace's one-time pre-admission for `person`, as the workspace service's
/// door asks the workspace for it: the owner's grant for a token's digest,
/// issued as an invitation only `person` redeems. Returns the invitation
/// and the token.
pub async fn door(
    host: &TestHost,
    owner: &SigningKey,
    owner_host: &Device,
    workspace: &str,
    person: [u8; 32],
) -> (Vec<u8>, Vec<u8>) {
    let token = random_bytes();
    let token_sha256: [u8; 32] = Sha256::digest(token).into();
    let mut grant = PreAdmission {
        workspace: workspace.to_owned(),
        role: WorkspaceRole::Editor,
        token_sha256,
        label: "Guest".into(),
        expires_at_ns: tilo_clock::unix_now_ns()
            .checked_add(600_000_000_000)
            .unwrap(),
        owner_user_public_key: owner.verifying_key().to_bytes(),
        signature: [0; 64],
    };
    grant.signature = owner.sign(&grant.digest().unwrap()).to_bytes();
    let issued = host
        .relay
        .workspaces()
        .unwrap()
        .pre_admit(
            &owner_host.cert,
            PreAdmitRequest {
                request_id: *blake3::hash(&token_sha256).as_bytes(),
                grant,
                recipient_user_public_key: Some(person),
                inviter_name: Some("Tilo".into()),
            },
        )
        .await
        .unwrap();
    (issued.signed_invitation, token.to_vec())
}

/// Redeem `door` for `cert`'s device in the host's process, as its session's entry has the
/// workspace admit it once the device proved its key on the connection
/// (`invitation_entry.rs` walks that entry itself): the workspace admits it as an editor.
pub async fn redeem(
    host: &TestHost,
    (invitation, token): (Vec<u8>, Vec<u8>),
    cert: &DeviceCert,
    _key: &SigningKey,
) {
    let admitted = host
        .relay
        .workspaces()
        .unwrap()
        .admit_proven(
            invitation,
            cert.clone(),
            random_bytes(),
            "Guest",
            Some(&token),
        )
        .await;
    assert!(
        matches!(
            &admitted,
            JoinReply::Admitted {
                role: WorkspaceRole::Editor,
                ..
            }
        ),
        "{admitted:?}"
    );
}

/// Let `cert`'s device in through `door`, then start its page in the workspace.
pub async fn enter(
    host: &TestHost,
    door: (Vec<u8>, Vec<u8>),
    cert: &DeviceCert,
    key: &SigningKey,
    host_cert: &DeviceCert,
    workspace_id: WorkspaceId,
) -> Link {
    redeem(host, door, cert, key).await;
    let peer = start_peer(host.iroh(), cert.clone(), host_cert.clone(), key.clone());
    peer.send(Cmd::Join(workspace_id)).unwrap();
    in_workspace(&peer, workspace_id).await;
    peer
}

pub async fn in_workspace(peer: &Link, workspace_id: WorkspaceId) {
    ready(&[peer]).await;
    wait_for("the governed workspace to let the page in", || async {
        (peer.status().workspaces.get(&workspace_id) == Some(&true)).then_some(())
    })
    .await;
}

/// 32 bytes from the operating system's randomness.
fn random_bytes() -> [u8; 32] {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).unwrap();
    bytes
}

/// [`ready`], blocking the calling thread, for a step that runs inside a
/// fixture's synchronous setup: the caller must be on a multi-thread runtime,
/// where blocking one worker leaves the others driving the sessions.
pub fn ready_blocking(peers: &[&Link]) {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(ready(peers)));
}
