//! A real page session — the one a browser runs — joins its workspace's live
//! plane: the page asks the host where the plane listens, joins the relay as
//! its own device with the workspace grant and the scope's channel keys, and
//! hears a native device's presence frame through the relay, opened with the
//! same keys. Nothing here calls the plane's join directly: the test only
//! serves `/live` and a relay listener, and the page's session does the rest.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::arithmetic_side_effects,
    clippy::items_after_statements,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

// Placing the workspace has the host build its governance in an automerge worker.
tilo_automerge_isolate::test_worker!();

use std::cell::Cell;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use channel_crypto::{ChannelGroup, MemberKey};
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_browser_peer::{ChannelContentKeys, Host, HttpClient, HttpFuture, PeerEvent, certify};
use tilo_live::live::{Namespace, Track};
use tilo_live::plane::{Join, Plane};
use tilo_live::relay::{
    Denied, Direction, LoopbackRelay, Peer, RelayAuthority, SessionAuthorization,
};
use tilo_live::roster::Roster;
use tilo_live::scope::Scope;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_protocol::web::{LIVE_INFO_PATH, LiveGrant, LiveInfo, LiveRequest};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{WORKSPACE, approve_one, enrol, start_host, start_peer, wait_for};

/// The workspace scope's record: its channel keys seal the plane's tracks.
fn plane_route() -> DocumentRoute {
    DocumentRoute {
        app_id: collab_types::scope::MEDIA_APP.to_owned(),
        cluster_id: WORKSPACE.to_owned(),
        record: collab_types::scope::WORKSPACE_RECORD.to_owned(),
    }
}

/// The scope's keys, as a channel's members derive them.
fn keys() -> ChannelContentKeys {
    ChannelGroup::create(
        collab_types::ChannelId::new(),
        MemberKey::generate("ada").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap()
}

/// The pinned certificate of a loopback listener, base64 as `/live` reports it.
fn certificate(relay: &LoopbackRelay) -> String {
    use base64::Engine as _;
    let hex = relay.fingerprints.first().unwrap();
    base64::engine::general_purpose::STANDARD.encode(hex::decode(hex).unwrap())
}

/// What the relay grants one participant: its own namespace to publish in and
/// the scope's subtree to read.
struct Permit {
    namespace: String,
    scope: String,
}

impl SessionAuthorization for Permit {
    fn publication(&self) -> Option<&str> {
        Some(&self.namespace)
    }
    fn subscriptions(&self) -> &[String] {
        &[]
    }
    fn subscription_prefix(&self) -> Option<&str> {
        Some(&self.scope)
    }
    fn current(&self, direction: Direction, namespace: &str) -> bool {
        match direction {
            Direction::Publish => namespace == self.namespace,
            Direction::Subscribe => namespace
                .strip_prefix(&self.scope)
                .is_some_and(|rest| rest.starts_with("/participant/")),
        }
    }
    fn changed(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        None
    }
}

/// Admits the page's grant — the one `/live` answered for its session token —
/// and the native device's key, which is the workspace's host device.
struct Admits {
    scope: Scope,
    page: [u8; 32],
    device: [u8; 32],
    token: Arc<Mutex<Option<[u8; 32]>>>,
}

impl RelayAuthority for Admits {
    fn admit(&self, _peer: Peer, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let device = if let Some(grant) = LiveGrant::decode(proof) {
            let token = *self.token.lock().unwrap();
            if Some(grant.token) != token || grant.workspace_id != support::workspace() {
                return Err(Denied);
            }
            self.page
        } else {
            let device = <[u8; 32]>::try_from(proof).map_err(|_| Denied)?;
            if device != self.device {
                return Err(Denied);
            }
            device
        };
        let namespace = Namespace::new(&self.scope, device);
        Ok(Arc::new(Permit {
            namespace: namespace.path(),
            scope: namespace.scope_path(),
        }))
    }
}

/// The host's answers, with `/live` answered by the loopback relay the native
/// device publishes on: the page's own code asks, joins and reads, as it does
/// against a host that serves a real live listener.
struct LiveAt {
    inner: Arc<support::https::DevHttps>,
    origin: String,
    port: u16,
    certificate: String,
    token: Arc<Mutex<Option<[u8; 32]>>>,
}

impl LiveAt {
    /// The answer to `/live` for `request`, or `None` when it asks for another
    /// workspace than this test's.
    fn answer(&self, session: Option<&str>, body: &[u8]) -> Option<Vec<u8>> {
        let request: LiveRequest = postcard::from_bytes(body).ok()?;
        if request.workspace != WORKSPACE || request.workspace_id != support::workspace() {
            return None;
        }
        let mut token = [0_u8; 32];
        hex::decode_to_slice(session?, &mut token).ok()?;
        *self.token.lock().unwrap() = Some(token);
        postcard::to_allocvec(&LiveInfo {
            webtransport_port: self.port,
            certificate_sha256: self.certificate.clone(),
            webtransport_origin: self.origin.clone(),
        })
        .ok()
    }
}

impl HttpClient for LiveAt {
    fn post(&self, path: &str, session: Option<&str>, body: Vec<u8>) -> HttpFuture<'_> {
        if !path.ends_with(LIVE_INFO_PATH) {
            return self.inner.post(path, session, body);
        }
        match self.answer(session, &body) {
            Some(bytes) => Box::pin(async move { Ok((200, bytes)) }),
            None => Box::pin(async move { Err("not this test's live route".into()) }),
        }
    }
}

/// A native device on the same relay: it joins the workspace's plane with its
/// own device key and publishes its presence, as every device's own live
/// session does.
struct Native {
    plane: Plane,
    sequence: Cell<u32>,
}

impl Native {
    async fn join(relay: &LoopbackRelay, device: [u8; 32], keys: ChannelContentKeys) -> Self {
        let carrier =
            tilo_live::carrier::WebTransport::new(&relay.endpoint, &relay.fingerprints).unwrap();
        let plane = Plane::join(
            &carrier,
            &device,
            Join {
                scope: Scope::workspace(WORKSPACE),
                device,
                keys,
                roster: Roster::new([device]),
                publish: true,
            },
        )
        .await
        .unwrap();
        Self {
            plane,
            sequence: Cell::new(0),
        }
    }

    fn publish_presence(&self, body: &[u8]) {
        let sequence = self.sequence.get();
        self.sequence.set(sequence.wrapping_add(1));
        self.plane.publish(Track::Presence, sequence, body).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the host's relay and web listener serve the page until the test ends"
)]
async fn a_page_joins_its_workspaces_plane_and_hears_a_device_through_the_relay() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let user = root.verifying_key().to_bytes();
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let state = support::scratch_dir();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;

    // The page is certified over the host's HTTPS and approved from the laptop.
    let browser_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let (code_tx, code_rx) = tokio::sync::oneshot::channel::<String>();
    let certifying = tokio::spawn({
        let https = Host::Https(host.https());
        let key = browser_key.clone();
        async move {
            certify(
                &https,
                user,
                "Browser on Ada's desk",
                [0; 16],
                &key,
                |code| {
                    let _ = code_tx.send(code.to_string());
                },
            )
            .await
        }
    });
    let shown = code_rx.await.unwrap();
    assert_eq!(approve_one(&host, &root, &laptop).await, shown);
    let (browser_cert, host_cert) = certifying.await.unwrap().unwrap();

    // The workspace is placed, and the plane's keys are the scope's.
    support::place(&host.relay, &laptop.cert).await;
    let keys = keys();

    // The workspace's live listener, shared by this page and a native device.
    let token = Arc::new(Mutex::new(None));
    let relay = LoopbackRelay::start(Arc::new(Admits {
        scope: Scope::workspace(WORKSPACE),
        page: browser_cert.device_public_key,
        device: host_device.cert.device_public_key,
        token: token.clone(),
    }))
    .unwrap();
    let origin = relay
        .endpoint
        .strip_suffix(tilo_live::preface::PATH)
        .unwrap()
        .to_owned();
    let port = url::Url::parse(&origin).unwrap().port().unwrap();
    let live = LiveAt {
        inner: host.https(),
        origin,
        port,
        certificate: certificate(&relay),
        token,
    };
    let page = start_peer(
        Host::Https(Arc::new(live)),
        browser_cert.clone(),
        host_cert,
        browser_key,
    );
    page.install_channel_keys(plane_route(), keys.clone())
        .unwrap();
    page.join_placed(WORKSPACE, "Workspace").unwrap();
    support::in_workspace(&page, support::workspace()).await;

    // A native device publishes its presence; the page hears it through the
    // relay, opened with the scope's keys.
    let native = Native::join(&relay, host_device.cert.device_public_key, keys.clone()).await;
    let said = b"Ada is at the desk".to_vec();
    let events = page.events();
    let (from, track, body) = wait_for("the page to hear the device on its plane", || {
        native.publish_presence(&said);
        let events = events.drain();
        async move {
            for event in events {
                if let PeerEvent::LivePlane {
                    workspace_id,
                    from,
                    track,
                    body,
                    ..
                } = event
                    && workspace_id == support::workspace()
                {
                    return Some((from, track, body));
                }
            }
            None
        }
    })
    .await;
    assert_eq!(from, host_device.cert.device_public_key);
    assert_eq!(track, tilo_live::live::Track::Presence);
    assert_eq!(body, said);
    assert_eq!(page.live_note(), None, "the page joined live");

    // A moving pointer reaches the page well inside a frame's worth of time:
    // presence travels the plane itself, so a stream of moves is never queued
    // behind the workspace's documents.
    let mut latencies = Vec::new();
    for step in 0..60_u32 {
        let said = format!("pointer {step}").into_bytes();
        let started = std::time::Instant::now();
        native.publish_presence(&said);
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if events
                .drain()
                .into_iter()
                .any(|event| matches!(event, PeerEvent::LivePlane { body, .. } if body == said))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the plane stopped carrying moves at move {step}"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        latencies.push(started.elapsed());
    }
    latencies.sort_unstable();
    let p95 = latencies[latencies.len() * 95 / 100];
    eprintln!(
        "presence latency over {} moves: p50 {:?}, p95 {p95:?}",
        latencies.len(),
        latencies[latencies.len() / 2]
    );
    assert!(
        p95 < Duration::from_millis(50),
        "pointer p95 {p95:?} over {} moves",
        latencies.len()
    );
}
