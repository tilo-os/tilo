//! Store-and-forward across a relay outage.
//!
//! Relay1 up, relay2 briefly down. A device on relay1 publishes
//! several deltas → relay1 enqueues them for relay2 in the SAME
//! pending path (`workspaces/<id>/pending/<hex(device_pubkey)>/`)
//! device peers use. When relay2 restarts and re-federates, its
//! subscribe drains the queue in order.
//!
//! To simulate "restart" cleanly, relay2's `state_root` and device
//! identity persist across the drop/re-`Relay::start` cycle. Its
//! iroh secret is `load_or_create_iroh_secret` — same file → same
//! iroh node id. Its device cert is built by the test.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard; the arithmetic \
              here is a synthetic per-delta timestamp counter bounded by a \
              3-item literal array, not real user data"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use std::path::PathBuf;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::{Envelope, RelayClient, RelayConfig, WorkspaceBinding};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, sign_device_cert,
};

use crate::common::{TestUser, client_endpoint, spawn_relay};

/// Build a fresh signing key + signed cert with `can_act_as_host`.
/// Returns the cert AND the signing key so callers can re-use them
/// across a simulated restart.
fn make_relay_device(user: &TestUser, label: &str) -> (SigningKey, DeviceCert) {
    let (sk, dev) = DeviceIdentity::generate(label, 100);
    let mut cert = DeviceCert {
        user_public_key: user.user_pk,
        device_public_key: dev.public_key,
        device_label: dev.label,
        capabilities: DeviceCapabilities::host(),
        issued_at_ns: 200,
        expires_at_ns: None,
        signature: [0u8; 64],
    };
    sign_device_cert(&mut cert, &user.user_sk).unwrap();
    (sk, cert)
}

async fn start_relay_at(
    user: &TestUser,
    sk: SigningKey,
    cert: DeviceCert,
    state_root: PathBuf,
    workspaces: &[u64],
    members: &[[u8; 32]],
) -> Relay {
    std::fs::create_dir_all(&state_root).unwrap();
    let mut cfg = RelayConfig {
        state_root: state_root.clone(),
        ..Default::default()
    };
    for id in workspaces {
        cfg.workspaces.push(WorkspaceBinding {
            id: *id,
            pin: true,
            replication: tilo_relay::ReplicationIntent::FullWorkspace,
            admitted_users: Vec::new(),
            shell_authority: None,
            checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::Disabled,
        });
    }
    let identity = RelayIdentity {
        user_public_key: user.user_pk,
        device_signing_key: sk,
        device_cert: cert,
        revocations: RevocationList::new(),
    };
    let relay = Relay::start(cfg, identity).await.unwrap();
    for id in workspaces {
        relay
            .set_workspace_membership(*id, true, members.to_vec(), state_root.clone())
            .await
            .unwrap();
    }
    relay
}

/// Bring relay2 up briefly so relay1 learns its device pubkey via
/// `PeerRelayHello`, then close it (rather than drop, so relay1 doesn't
/// have to wait out the QUIC idle timeout before noticing the
/// subscription drop — far longer than any outage this test can sit through).
async fn prime_then_close_relay2(
    user: &TestUser,
    relay1: &Relay,
    relay1_addr: iroh_base::EndpointAddr,
    r2_sk: SigningKey,
    r2_cert: DeviceCert,
    r2_state_root: PathBuf,
    workspaces: &[u64],
    members: &[[u8; 32]],
) {
    let r2_pubkey = r2_cert.device_public_key;
    let r2_first = Box::pin(start_relay_at(
        user,
        r2_sk,
        r2_cert,
        r2_state_root,
        workspaces,
        members,
    ))
    .await;
    // Peer-relay r1 ← r2 dial primes relay1's `known_peer_relays`.
    r2_first.add_peer_relay_at(relay1_addr, "r1".into(), 0);
    // Wait for the actual subscription rather than guessing how long the
    // handshake takes on the runner.
    tilo_test_env::within("relay2 subscribed to relay1", async {
        loop {
            if relay1.is_subscribed(workspaces[0], &r2_pubkey).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    r2_first.shutdown().await;
    // Wait until relay1 has removed the closed subscription before publishing
    // the outage deltas. Otherwise a loaded runner can accidentally classify
    // the dead QUIC stream as live and lose store-and-forward coverage.
    tilo_test_env::within("relay1 observed relay2 shutdown", async {
        loop {
            if !relay1.is_subscribed(workspaces[0], &r2_pubkey).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// p1 publishes three tagged deltas via relay1 while relay2 is down, and
/// this waits until all three have landed in relay1's pending queue for
/// r2's device pubkey (the same store path offline device peers use).
async fn publish_three_while_peer_is_down(
    relay1_addr: iroh_base::EndpointAddr,
    user: &TestUser,
    p1: &crate::common::TestDevice,
    pending_dir: &std::path::Path,
) {
    let c1 = RelayClient::connect(
        client_endpoint().await,
        relay1_addr,
        user.user_pk,
        p1.cert.clone(),
        p1.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    c1.send(Envelope::Subscribe { workspace_id: 11 }).unwrap();
    let _ = tilo_test_env::within("p1 Ready", c1.recv()).await;

    for (i, tag) in ["one", "two", "three"].iter().enumerate() {
        c1.send(Envelope::Delta {
            workspace_id: 11,
            author_device_public_key: p1.device_pubkey(),
            authored_at_ns: (i as u64) + 1,
            delta: tag.as_bytes().to_vec(),
            signature: [1u8; 64],
        })
        .unwrap();
    }

    let mut queued = 0;
    // Polls for up to 30 s; only a hang uses the whole budget.
    for _ in 0..600 {
        queued = if pending_dir.exists() {
            std::fs::read_dir(pending_dir)
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".delta"))
                .count()
        } else {
            0
        };
        if queued >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(queued, 3, "3 deltas should queue for offline peer relay");
}

/// p2 subscribes through `r2_addr` and collects delta payloads until 3
/// have arrived or the retry budget runs out (whichever first — the
/// caller's own assertion reports a short collection).
async fn drain_three_deltas(
    r2_addr: iroh_base::EndpointAddr,
    user: &TestUser,
    p2: &crate::common::TestDevice,
) -> Vec<Vec<u8>> {
    let c2 = RelayClient::connect(
        client_endpoint().await,
        r2_addr,
        user.user_pk,
        p2.cert.clone(),
        p2.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    c2.send(Envelope::Subscribe { workspace_id: 11 }).unwrap();

    let mut got: Vec<Vec<u8>> = Vec::new();
    for _ in 0..15 {
        let env = tokio::time::timeout(tilo_test_env::hang_bound(), c2.recv()).await;
        match env {
            Ok(Some(Envelope::Delta {
                delta,
                workspace_id: 11,
                ..
            })) => got.push(delta),
            Ok(Some(_)) => {} // Ready / other, ignore
            _ => break,
        }
        if got.len() >= 3 {
            break;
        }
    }
    got
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deltas_queued_during_outage_arrive_in_order_after_reconnect() {
    let user = TestUser::fresh("Alice");
    let p1 = user.enrol("laptop", DeviceCapabilities::primary());
    let p2 = user.enrol("phone", DeviceCapabilities::member());
    let members = vec![p1.device_pubkey(), p2.device_pubkey()];

    let relay1 = Box::pin(spawn_relay(
        &user,
        user.enrol("r1", DeviceCapabilities::host()),
        &[11],
        &members,
    ))
    .await;

    // Stable identity + state root for relay2 across the outage.
    let (r2_sk, r2_cert) = make_relay_device(&user, "r2");
    let r2_pubkey = r2_cert.device_public_key;
    let r2_tmp = tempfile::tempdir().unwrap();
    let r2_state_root = r2_tmp.path().to_path_buf();

    // Round 1: relay2 up briefly so relay1 learns its device pubkey
    // via PeerRelayHello.
    Box::pin(prime_then_close_relay2(
        &user,
        &relay1.relay,
        relay1.relay.node_addr(),
        r2_sk.clone(),
        r2_cert.clone(),
        r2_state_root.clone(),
        &[11],
        &members,
    ))
    .await;

    // Round 2: p1 publishes three deltas via relay1 while relay2 is
    // down. Relay1's fanout enqueues for the (known) r2 pubkey. Confirm
    // they land in the pending dir for r2's device pubkey — same store
    // path as offline device peers.
    let hex_r2 = hex::encode(r2_pubkey);
    let pending_dir = relay1
        .state_root
        .join("workspaces")
        .join("11")
        .join("pending")
        .join(&hex_r2);
    publish_three_while_peer_is_down(relay1.relay.node_addr(), &user, &p1, &pending_dir).await;

    // Round 3: relay2 restarts with the SAME device cert AND the
    // SAME state root → same device pubkey, same iroh secret →
    // same iroh node id (load_or_create_iroh_secret persists the
    // file). Its subscribe drains the queue.
    let r2_restart = Box::pin(start_relay_at(
        &user,
        r2_sk,
        r2_cert,
        r2_state_root.clone(),
        &[11],
        &members,
    ))
    .await;
    r2_restart.add_peer_relay_at(relay1.relay.node_addr(), "r1".into(), 0);

    // p2 → r2_restart. Once relay2 subscribes upstream to relay1
    // (which happens inside its peer-relay dial task), relay1
    // drains the queue → relay2 fans out to p2.
    let got = drain_three_deltas(r2_restart.node_addr(), &user, &p2).await;
    assert_eq!(
        got,
        vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()],
        "queued deltas must drain in order after relay2 reconnects",
    );

    drop(r2_restart);
    drop(r2_tmp);
}
