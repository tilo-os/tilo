//! Fixtures shared by the end-to-end tests: a host serving browsers over
//! HTTPS with a development certificate, and certified devices.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of these fixtures"
)]

pub mod https;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use collab_types::DocumentMutation;
use ed25519_dalek::{Signer as _, SigningKey};
use getrandom::{SysRng, rand_core::UnwrapErr};
use sha2::{Digest as _, Sha256};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay::certify::{ApproverDecision, ApproverReply};
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::web::{TlsSource, WebConfig, WebHandle};
use tilo_relay::workspaces::PreAdmitRequest;
use tilo_relay_client::Approver;
use tilo_relay_protocol::workspaces::{JoinReply, PreAdmission, WorkspaceRole};
use tilo_session_host::invite::ask_to_join_pre_admitted;
use tilo_session_host::{Host, MemoryPeerStore, PeerConfig, PeerEvent, PeerHandle, PeerSnapshot};
use tilo_user_identity::{
    CertificationPolicy, DeviceCapabilities, DeviceCert, RevocationList, issue_device_cert,
    sign_device_cert,
};

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

/// The test hosts' devices. A governed workspace's host writes the first state
/// of each record a member starts; the tests read what members wrote.
static HOSTS: std::sync::Mutex<std::collections::BTreeSet<[u8; 32]>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Whether `device` is a test host's.
pub fn is_host(device: &[u8; 32]) -> bool {
    HOSTS.lock().unwrap().contains(device)
}

/// Whether `event` is a document a member wrote, not one of a host's.
pub fn members_document(event: &PeerEvent) -> bool {
    matches!(event, PeerEvent::Document(document) if !is_host(&document.author))
}

pub fn workspace() -> u64 {
    tilo_relay_protocol::room_workspace_id(WORKSPACE).unwrap()
}

/// Have `relay` govern [`WORKSPACE`], the person's workspace, unless it does,
/// and admit `device` to it as its owner: what a page asks of its host
/// before it joins ([`PeerHandle::join_placed`]), done here for a peer
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

/// A host that keeps its iroh address across a restart: it listens for iroh
/// where `iroh_listen` names.
pub async fn start_host_at(
    root: &SigningKey,
    host: &Device,
    state: &Path,
    web_listen: SocketAddr,
    iroh_listen: SocketAddr,
) -> TestHost {
    start_host_serving(
        root,
        host,
        state,
        web_listen,
        HostSettings {
            iroh_listen: Some(iroh_listen),
            ..HostSettings::default()
        },
    )
    .await
}

/// What a test host serves beyond its secure listener.
#[derive(Default)]
pub struct HostSettings<'a> {
    /// Plain HTTP too, here.
    pub plain_listen: Option<SocketAddr>,
    /// Where its iroh endpoint listens; ephemeral unless a restarted host
    /// must keep its address.
    pub iroh_listen: Option<SocketAddr>,
    /// People (user roots) its operator trust policy lets connect.
    pub guests: &'a [[u8; 32]],
    /// The change and content budgets workspace guests are held to.
    pub limits: Option<tilo_relay::member_limits::MemberLimitsConfig>,
    /// The fetch proxy it serves its pages.
    pub fetch_proxy: Option<tilo_relay::web::FetchProxySetup>,
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
        iroh_listen,
        guests,
        limits,
        fetch_proxy,
    } = settings;
    let user = root.verifying_key().to_bytes();
    let mut cfg = tilo_relay::RelayConfig {
        state_root: state.to_path_buf(),
        member_limits: limits,
        ..Default::default()
    };
    if let Some(iroh_listen) = iroh_listen {
        // A stopped host's connection tasks let go of its socket shortly
        // after its shutdown returns; a restart on the same address waits
        // for that, as a restarted process waits for the old one to exit.
        if iroh_listen.port() != 0 {
            wait_for("the stopped host's iroh address to be free", || async {
                std::net::UdpSocket::bind(iroh_listen).ok().map(drop)
            })
            .await;
        }
        cfg.iroh_listen = iroh_listen;
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
        })
        .await
        .unwrap();
    HOSTS.lock().unwrap().insert(host.cert.device_public_key);
    TestHost { relay, web }
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
    let relay = Relay::start(
        tilo_relay::RelayConfig {
            state_root: state.to_path_buf(),
            ..Default::default()
        },
        RelayIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: host.key.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
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
        })
        .await
        .unwrap();
    HOSTS.lock().unwrap().insert(host.cert.device_public_key);
    TestHost { relay, web }
}

impl TestHost {
    pub fn https(&self) -> Arc<https::DevHttps> {
        https::DevHttps::new(self.web.addr, self.web.certificate_sha256.as_ref().unwrap())
    }

    /// The host as a session reaches it: its iroh endpoint.
    pub fn iroh(&self) -> Host {
        Host::at(self.relay.node_addr())
    }

    /// The address its iroh endpoint listens on, for a restart that keeps
    /// it: a page dials the address it was given, so a host restarted
    /// elsewhere is one it never finds again.
    pub fn iroh_listen(&self) -> SocketAddr {
        *self.relay.node_addr().ip_addrs().next().unwrap()
    }
}

/// Approve the one pending certification request as `laptop` (a root
/// holder), over the host's iroh certification ALPN.
pub async fn approve_one(host: &TestHost, root: &SigningKey, laptop: &Device) -> String {
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let mut approver = Approver::connect(
        &endpoint,
        host.relay.node_addr(),
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

pub fn start_peer(
    host: Host,
    cert: DeviceCert,
    host_cert: DeviceCert,
    key: SigningKey,
) -> PeerHandle {
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
) -> PeerHandle {
    PeerHandle::start(
        PeerConfig {
            host,
            cert,
            host_cert,
            revocations: RevocationList::new(),
        },
        Arc::new(key),
        store,
    )
}

/// A page's device key in custody: the session signs with it and never reads
/// it, as a browser's `WebCrypto` key is used. A page's endpoint therefore
/// speaks under a key of its own, which the device signs for.
pub struct PageCustody(SigningKey);

impl tilo_session_host::DeviceSigner for PageCustody {
    fn device_public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    fn sign(&self, message: Vec<u8>) -> tilo_session_host::SignFuture<'_> {
        let signature = ed25519_dalek::Signer::sign(&self.0, &message).to_bytes();
        Box::pin(std::future::ready(Ok(signature)))
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
) -> PeerHandle {
    PeerHandle::start(
        PeerConfig {
            host,
            cert,
            host_cert,
            revocations: RevocationList::new(),
        },
        Arc::new(PageCustody(key)),
        store,
    )
}

pub async fn ready(peers: &[&PeerHandle]) {
    wait_for("sessions ready", || async {
        peers
            .iter()
            .all(|peer| PeerSnapshot::from_status(&peer.status()).phase == "ready")
            .then_some(())
    })
    .await;
}

/// Wait until a participant of the workspace admitted `peer` to the note's channel.
pub async fn admitted(peer: &PeerHandle) {
    let note = format!("route {}/{} ", route().app_id, route().record);
    wait_for("a participant to admit the page", || {
        let ready = peer
            .status()
            .channels
            .lines()
            .any(|line| line.starts_with(&note) && line.contains(": ready;"));
        async move { ready.then_some(()) }
    })
    .await;
}

pub fn route() -> DocumentRoute {
    DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: WORKSPACE.into(),
        record: "note-1".into(),
    }
}

pub async fn next_document(peer: &PeerHandle, pending: &mut Vec<PeerEvent>) -> DocumentMutation {
    wait_for("a document", || {
        pending.extend(peer.drain());
        let found = pending.iter().position(members_document).and_then(|index| {
            match pending.remove(index) {
                PeerEvent::Document(document) => document.mutation(),
                _ => None,
            }
        });
        async move { found }
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

/// Redeem `door` for `cert`'s device over the host's join exchange, as a
/// page asks to join with its invitation: the workspace admits it as an editor.
pub async fn redeem(
    host: &TestHost,
    (invitation, token): (Vec<u8>, Vec<u8>),
    cert: &DeviceCert,
    key: &SigningKey,
) {
    let admitted = ask_to_join_pre_admitted(
        &*host.https(),
        &invitation,
        cert,
        key,
        random_bytes(),
        "Guest",
        &token,
    )
    .await
    .unwrap();
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
    workspace_id: u64,
) -> PeerHandle {
    redeem(host, door, cert, key).await;
    let peer = start_peer(host.iroh(), cert.clone(), host_cert.clone(), key.clone());
    peer.join(workspace_id).unwrap();
    in_workspace(&peer, workspace_id).await;
    peer
}

pub async fn in_workspace(peer: &PeerHandle, workspace_id: u64) {
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
pub fn ready_blocking(peers: &[&PeerHandle]) {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(ready(peers)));
}
