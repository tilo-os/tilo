//! Two `CollaborationManager`s co-editing through a real workspace host.
//!
//! This proves the `Backend::Relay` path end-to-end: the same host tier
//! the relay crate's own `coedit_through_host` test exercises, but driven
//! entirely through the toolkit's public `send` / `poll` surface — no raw
//! `Envelope`s, no mock of the host or the co-edit bridge.
//!
//! A single `qos_relay::Relay` is stood up in-process; two iroh clients
//! dial it, join the workspace's co-edit session, and each session is
//! wrapped in a `CollaborationManager::from_relay_session`. Manager A then
//! publishes a genuine Automerge co-edit delta and a presence pointer via
//! `send(..)`; Manager B `poll()`s, converges on the shared document
//! (identical `get_heads()` + identical rendered text), and sees the
//! remote cursor **attributed to A's device key** — the presence
//! attribution that rides inside the co-edit payload because the host's
//! presence envelope carries no author of its own.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ObjType, ROOT, ReadDoc, Value};

use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager, PresencePayload};
use ui::presence::PresenceKey;

use ed25519_dalek::SigningKey;
use iroh::endpoint::{Endpoint, presets};
use qos_relay::{
    RelayClient, RelayCoeditSession, RelayConfig, WorkspaceBinding,
    daemon::{Relay, RelayIdentity},
};
use qos_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity, sign_device_cert,
};

// --- minimal identity/host fixtures (public-API twins of the relay
//     crate's own `tests/common`, inlined so `ui` need not reach into
//     that test-only module) ------------------------------------------

struct TestUser {
    user_sk: SigningKey,
    user_pk: [u8; 32],
}

struct TestDevice {
    sk: SigningKey,
    cert: DeviceCert,
}

impl TestDevice {
    fn device_pubkey(&self) -> [u8; 32] {
        self.cert.device_public_key
    }
}

impl TestUser {
    fn fresh(label: &str) -> Self {
        let (sk, u) = UserIdentity::generate(label, 1);
        Self {
            user_sk: sk,
            user_pk: u.public_key,
        }
    }

    fn enrol(&self, label: &str, caps: DeviceCapabilities) -> TestDevice {
        let (dev_sk, dev) = DeviceIdentity::generate(label, 100);
        let mut cert = DeviceCert {
            user_public_key: self.user_pk,
            device_public_key: dev.public_key,
            device_label: dev.label,
            capabilities: caps,
            issued_at_ns: 200,
            expires_at_ns: None,
            signature: [0u8; 64],
        };
        sign_device_cert(&mut cert, &self.user_sk).unwrap();
        TestDevice { sk: dev_sk, cert }
    }
}

async fn spawn_host(
    state_root: &std::path::Path,
    user: &TestUser,
    host_dev: TestDevice,
    workspace: u64,
    members: &[[u8; 32]],
) -> Relay {
    let mut cfg = RelayConfig {
        state_root: state_root.to_path_buf(),
        ..Default::default()
    };
    cfg.workspaces.push(WorkspaceBinding {
        id: workspace,
        pin: true,
    });
    let identity = RelayIdentity {
        user_public_key: user.user_pk,
        device_signing_key: host_dev.sk.clone(),
        device_cert: host_dev.cert.clone(),
        revocations: RevocationList::new(),
    };
    let relay = Relay::start(cfg, identity).await.unwrap();
    relay
        .set_workspace_membership(workspace, true, members.to_vec(), state_root.to_path_buf())
        .await
        .unwrap();
    relay
}

async fn client_endpoint() -> Endpoint {
    Endpoint::builder(presets::N0).bind().await.unwrap()
}

async fn join_session(
    user: &TestUser,
    dev: &TestDevice,
    addr: iroh_base::EndpointAddr,
    workspace: u64,
) -> RelayCoeditSession {
    let client = RelayClient::connect(
        client_endpoint().await,
        addr,
        user.user_pk,
        dev.cert.clone(),
        dev.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("client connect");
    RelayCoeditSession::join(client, workspace, dev.device_pubkey()).unwrap()
}

// --- document helpers (mirror the relay crate's coedit test) ----------

fn doc_with_text(actor: &[u8], seed: &str) -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from(actor));
    let t = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
    doc.splice_text(&t, 0, 0, seed).unwrap();
    doc
}

fn text_of(doc: &AutoCommit) -> String {
    match doc.get(ROOT, "text") {
        Ok(Some((Value::Object(ObjType::Text), id))) => doc.text(&id).unwrap_or_default(),
        _ => String::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn two_managers_coedit_and_attribute_presence_through_host() {
    const WS: u64 = 71;

    let user = TestUser::fresh("Alice");
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("phone", DeviceCapabilities::member());
    let members = [a.device_pubkey(), b.device_pubkey()];

    let state = tempfile::tempdir().unwrap();
    let host_dev = user.enrol("home-server", DeviceCapabilities::host());
    let host = spawn_host(state.path(), &user, host_dev, WS, &members).await;
    let addr = host.node_addr();

    // Both peers join the co-edit session and wrap it in a manager. The
    // managers' bridge tasks pump the sessions on this test's runtime.
    let a_session = join_session(&user, &a, addr.clone(), WS).await;
    let b_session = join_session(&user, &b, addr, WS).await;

    let handle = tokio::runtime::Handle::current();
    let mut mgr_a = CollaborationManager::from_relay_session(a_session, &handle);
    let mut mgr_b = CollaborationManager::from_relay_session(b_session, &handle);

    assert!(
        mgr_a.is_connected(),
        "relay-backed manager reports connected"
    );
    assert!(
        mgr_b.is_connected(),
        "relay-backed manager reports connected"
    );

    // Let both subscriptions register at the host before A publishes,
    // so live presence fan-out (which, unlike deltas, is never queued)
    // reaches B.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // A produces a genuine Automerge co-edit delta and publishes it via
    // the toolkit's ordinary send path — no raw envelope in sight.
    let mut doc_a = doc_with_text(b"alice", "hello");
    let delta = doc_a.save_incremental();
    mgr_a.send(CollabRequest::SyncIncremental { data: delta });

    // Drain B until it has both converged on the document and seen A's
    // remote cursor. A's cursor is re-published each iteration because
    // presence is ephemeral (fanned to live subscribers, never queued),
    // exactly as a real editor streams pointer motion.
    let expected_author = format!("peer-{}", hex::encode(a.device_pubkey()));
    let mut doc_b = AutoCommit::new();
    doc_b.set_actor(ActorId::from(b"bob".as_slice()));
    let mut converged = false;
    let mut cursor_author: Option<String> = None;

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        mgr_a.send(CollabRequest::Presence {
            key: PresenceKey::Tab {
                id: "note-1".to_owned(),
            },
            payload: PresencePayload::Pointer { x: 12.5, y: 34.0 },
        });

        for evt in mgr_b.poll() {
            match evt {
                CollabEvent::CRDTIncremental { data } => {
                    doc_b
                        .load_incremental(&data)
                        .expect("B applies co-edit delta");
                    converged = true;
                }
                CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                    assert!((x - 12.5).abs() < 1e-3, "cursor x round-trips");
                    assert!((y - 34.0).abs() < 1e-3, "cursor y round-trips");
                    cursor_author = Some(user_id);
                }
                _ => {}
            }
        }

        if converged && cursor_author.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(
        converged,
        "B must receive A's co-edit delta fanned out through the host"
    );
    assert_eq!(
        text_of(&doc_b),
        "hello",
        "B renders A's co-edit locally after converging"
    );
    assert_eq!(
        doc_a.get_heads(),
        doc_b.get_heads(),
        "the two managers converge on the shared document through the host"
    );
    assert_eq!(
        cursor_author.as_deref(),
        Some(expected_author.as_str()),
        "presence over the relay is attributed to its author (A's device key)"
    );

    drop(mgr_a);
    drop(mgr_b);
    drop(host);
}
