//! Fixtures shared by the end-to-end tests: a host serving browsers on
//! HTTPS + WebTransport with a development certificate, and certified
//! devices.

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
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_browser_peer::{
    DocumentRoute, Host, MemoryPeerStore, PeerConfig, PeerEvent, PeerHandle, PeerSnapshot,
};
use tilo_relay::certify::{ApproverDecision, ApproverReply};
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::web::{TlsSource, WebConfig, WebHandle};
use tilo_relay_client::Approver;
use tilo_user_identity::{
    CertificationPolicy, DeviceCapabilities, DeviceCert, RevocationList, issue_device_cert,
    sign_device_cert,
};

pub const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

pub fn workspace() -> u64 {
    tilo_relay_protocol::room_workspace_id(ROOM).unwrap()
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

/// [`start_host_plain`], whose workspace also admits the people `guests`
/// (user roots), as a room admits its guests.
pub async fn start_host_admitting(
    root: &SigningKey,
    host: &Device,
    state: &Path,
    web_listen: SocketAddr,
    plain_listen: Option<SocketAddr>,
    guests: &[[u8; 32]],
) -> TestHost {
    let user = root.verifying_key().to_bytes();
    let mut cfg = tilo_relay::RelayConfig {
        state_root: state.to_path_buf(),
        ..Default::default()
    };
    if !guests.is_empty() {
        // The host's handshake admits them, as it admits a room's members.
        cfg.trust_policy = tilo_relay::TrustPolicy::AllowList {
            users: guests.iter().map(hex::encode).collect(),
        };
    }
    cfg.workspaces.push(tilo_relay::WorkspaceBinding {
        id: workspace(),
        pin: true,
        replication: tilo_relay::ReplicationIntent::FullWorkspace,
        admitted_users: std::iter::once(&user)
            .chain(guests)
            .map(hex::encode)
            .collect(),
        checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::Disabled,
        shell_authority: None,
    });
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
            live_origin: None,
            limits: tilo_relay::web::WebLimits::default(),
        })
        .await
        .unwrap();
    TestHost { relay, web }
}

/// A host that governs rooms rather than one pinned workspace, serving
/// browsers on an ephemeral port. With `keep_web_identity` its development
/// certificate is stored under `state`, so a restarted host presents the
/// same one.
pub async fn start_room_host(
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
            live_origin: None,
            limits: tilo_relay::web::WebLimits::default(),
        })
        .await
        .unwrap();
    TestHost { relay, web }
}

impl TestHost {
    pub fn https(&self) -> Arc<https::DevHttps> {
        https::DevHttps::new(self.web.addr, self.web.certificate_sha256.as_ref().unwrap())
    }

    pub fn iroh(&self) -> Host {
        Host::Iroh {
            addr: self.relay.node_addr(),
            relay_url: None,
        }
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
            watching: false,
        },
        Arc::new(key),
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

/// Wait until the room's steward admitted `peer` to the note's channel.
pub async fn admitted(peer: &PeerHandle) {
    wait_for("the steward to admit the page", || {
        let ready = peer
            .status()
            .channels
            .lines()
            .any(|line| line.starts_with("route ") && line.contains(": ready;"));
        async move { ready.then_some(()) }
    })
    .await;
}

pub fn route() -> DocumentRoute {
    DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: ROOM.into(),
        record: "note-1".into(),
    }
}

/// An HTTPS client that counts the `/events` answers the host sent.
pub struct Recording {
    pub inner: Arc<https::DevHttps>,
    /// Largest single answer.
    pub largest_page: Arc<std::sync::atomic::AtomicUsize>,
    /// Every answer's bytes together.
    pub total: Arc<std::sync::atomic::AtomicUsize>,
}

impl Recording {
    pub fn new(inner: Arc<https::DevHttps>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            largest_page: Arc::default(),
            total: Arc::default(),
        })
    }
}

impl tilo_browser_peer::HttpClient for Recording {
    fn post(
        &self,
        path: &str,
        session: Option<&str>,
        body: Vec<u8>,
    ) -> tilo_browser_peer::HttpFuture<'_> {
        use std::sync::atomic::Ordering;
        let events = path.ends_with("/events");
        let answer = self.inner.post(path, session, body);
        Box::pin(async move {
            let answer = answer.await;
            if events && let Ok((_, bytes)) = &answer {
                self.largest_page.fetch_max(bytes.len(), Ordering::Relaxed);
                self.total.fetch_add(bytes.len(), Ordering::Relaxed);
            }
            answer
        })
    }
}

/// Wait for the next document event on `peer` and return its mutation.
pub async fn next_document(peer: &PeerHandle, pending: &mut Vec<PeerEvent>) -> DocumentMutation {
    wait_for("a document", || {
        pending.extend(peer.drain());
        let found = pending
            .iter()
            .position(|event| matches!(event, PeerEvent::Document(_)))
            .and_then(|index| match pending.remove(index) {
                PeerEvent::Document(document) => document.mutation(),
                _ => None,
            });
        async move { found }
    })
    .await
}
