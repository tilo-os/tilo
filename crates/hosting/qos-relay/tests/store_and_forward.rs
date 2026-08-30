//! Store-and-forward: a delta authored while peer B is offline
//! must be queued on the relay's disk under
//! `workspaces/<id>/pending/<hex(pubkey_B)>/`, then delivered on
//! B's next Subscribe.
//!
//! Timeline:
//!   1. relay + peers A and B start; both are workspace 5 members.
//!   2. Peer A connects + subscribes.
//!   3. Peer A publishes delta D1. B is offline → D1 gets queued.
//!   4. Peer A disconnects, publishes nothing more.
//!   5. Peer B connects + subscribes.
//!   6. The relay's Ready reports `pending_deltas=1` and immediately
//!      delivers D1 to B.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::Duration;

use qos_relay::Envelope;
use qos_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_relay};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_peer_gets_queued_deltas_on_reconnect() {
    let user = TestUser::fresh("Alice");
    let relay_dev = user.enrol("home-server", DeviceCapabilities::host());
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("phone", DeviceCapabilities::member());

    let relay = spawn_relay(
        &user,
        relay_dev,
        &[5],
        &[a.device_pubkey(), b.device_pubkey()],
    )
    .await;
    let addr = relay.relay.node_addr();

    // Peer A comes up.
    let ep_a = client_endpoint().await;
    let a_client = qos_relay::RelayClient::connect(
        ep_a,
        addr.clone(),
        user.user_pk,
        a.cert.clone(),
        a.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    a_client
        .send(Envelope::Subscribe { workspace_id: 5 })
        .unwrap();
    // Drain Ready.
    let _ = tokio::time::timeout(Duration::from_secs(2), a_client.recv())
        .await
        .expect("A ready timeout");

    // A publishes two deltas while B is offline.
    for (seq, payload) in [(1u64, "first-delta"), (2u64, "second-delta")] {
        a_client
            .send(Envelope::Delta {
                workspace_id: 5,
                author_device_public_key: a.device_pubkey(),
                authored_at_ns: seq,
                delta: payload.as_bytes().to_vec(),
                signature: [7u8; 64],
            })
            .unwrap();
    }
    // Give the relay a beat to persist.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Verify the deltas landed on disk under
    // <state>/workspaces/5/pending/<hex(b)>/.
    let b_hex = hex::encode(b.device_pubkey());
    let pending_dir = relay
        .state_root
        .join("workspaces")
        .join("5")
        .join("pending")
        .join(&b_hex);
    let mut queued = std::fs::read_dir(&pending_dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".delta"))
        .count();
    // Retry a couple of times: with iroh transport the write may
    // land asynchronously.
    for _ in 0..20 {
        if queued >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        queued = std::fs::read_dir(&pending_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".delta"))
            .count();
    }
    assert_eq!(queued, 2, "both deltas should be queued to disk for B");

    // B comes online + subscribes.
    let ep_b = client_endpoint().await;
    let b_client = qos_relay::RelayClient::connect(
        ep_b,
        addr,
        user.user_pk,
        b.cert.clone(),
        b.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    b_client
        .send(Envelope::Subscribe { workspace_id: 5 })
        .unwrap();

    // The next N messages B receives are the two queued deltas
    // (order preserved) followed by Ready. Iteration order is:
    // the relay drains pending BEFORE it sends Ready, so B
    // receives Delta, Delta, then Ready.
    let mut got_payloads: Vec<Vec<u8>> = Vec::new();
    let mut got_ready = false;
    for _ in 0..3 {
        let env = tokio::time::timeout(Duration::from_secs(3), b_client.recv())
            .await
            .expect("timeout")
            .expect("channel closed");
        match env {
            Envelope::Delta {
                workspace_id,
                delta,
                ..
            } => {
                assert_eq!(workspace_id, 5);
                got_payloads.push(delta);
            }
            Envelope::Ready {
                workspace_id,
                pending_deltas,
            } => {
                assert_eq!(workspace_id, 5);
                assert_eq!(pending_deltas, 2);
                got_ready = true;
            }
            other => panic!("unexpected env: {other:?}"),
        }
    }
    assert!(got_ready, "Ready envelope should arrive");
    assert_eq!(
        got_payloads,
        vec![b"first-delta".to_vec(), b"second-delta".to_vec()],
        "queued deltas must be delivered in-order"
    );

    // Pending dir should be empty now.
    let after = std::fs::read_dir(&pending_dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".delta"))
        .count();
    assert_eq!(after, 0, "queue drains after Subscribe");
}
