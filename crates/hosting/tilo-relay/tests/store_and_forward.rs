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
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use std::time::Duration;

use tilo_relay::Envelope;
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_relay};

/// `.delta` files queued in `pending_dir`; none while it does not exist yet.
fn queued_deltas(pending_dir: &std::path::Path) -> usize {
    std::fs::read_dir(pending_dir).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".delta"))
            .count()
    })
}

/// A connects, subscribes, publishes two deltas while B is offline, and
/// waits until both have landed on disk in B's pending queue (retrying,
/// since with iroh transport the write may land asynchronously).
async fn a_publishes_two_offline_deltas(
    addr: iroh_base::EndpointAddr,
    user: &TestUser,
    a: &crate::common::TestDevice,
    pending_dir: &std::path::Path,
) {
    let a_client = tilo_relay::RelayClient::connect(
        client_endpoint().await,
        addr,
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
    let _ = a_client.recv().await.expect("A Ready channel closed");

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
    // The writes land asynchronously on the relay's persistence turn: wait
    // for them rather than for a fixed time.
    let mut queued = queued_deltas(pending_dir);
    while queued < 2 {
        tokio::time::sleep(Duration::from_millis(25)).await;
        queued = queued_deltas(pending_dir);
    }
    assert_eq!(queued, 2, "both deltas should be queued to disk for B");
}

/// B connects, subscribes, and drains the two queued deltas plus the
/// Ready marker (the relay drains pending before it sends Ready, so B
/// receives Delta, Delta, then Ready).
async fn b_connects_and_drains_queue(
    addr: iroh_base::EndpointAddr,
    user: &TestUser,
    b: &crate::common::TestDevice,
) {
    let b_client = tilo_relay::RelayClient::connect(
        client_endpoint().await,
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
        let env = b_client.recv().await.expect("channel closed");
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_peer_gets_queued_deltas_on_reconnect() {
    let user = TestUser::fresh("Alice");
    let relay_dev = user.enrol("home-server", DeviceCapabilities::host());
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("phone", DeviceCapabilities::member());

    let relay = Box::pin(spawn_relay(
        &user,
        relay_dev,
        &[5],
        &[a.device_pubkey(), b.device_pubkey()],
    ))
    .await;
    let addr = relay.relay.node_addr();

    // Verify the deltas landed on disk under
    // <state>/workspaces/5/pending/<hex(b)>/.
    let b_hex = hex::encode(b.device_pubkey());
    let pending_dir = relay
        .state_root
        .join("workspaces")
        .join("5")
        .join("pending")
        .join(&b_hex);
    a_publishes_two_offline_deltas(addr.clone(), &user, &a, &pending_dir).await;

    // B comes online + subscribes, and drains the queue.
    b_connects_and_drains_queue(addr, &user, &b).await;

    // The pending entries retire on the relay's persistence turn after
    // delivery, so they may still be on disk as B reads Ready. Wait for the
    // queue to drain; one that never drains hangs and nextest reports it.
    while queued_deltas(&pending_dir) != 0 {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
