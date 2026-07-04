//! Two-peer fan-out through a single relay.
//!
//! Spins up:
//! - one relay bound to workspace 42, membership = {A, B}
//! - two peer devices A + B, each dials the relay
//! Both subscribe to 42; A publishes a Delta envelope. Assert B
//! receives the same delta bytes via the relay's fan-out.

mod common;

use std::time::Duration;

use qos_relay::{Envelope, RelayClient};
use qos_user_identity::{DeviceCapabilities, RevocationList};

use common::{TestUser, client_endpoint, spawn_relay, wait_for};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_peers_fan_out_through_relay() {
    let user = TestUser::fresh("Alice");
    let relay_dev = user.enrol("home-server", DeviceCapabilities::host());
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("phone", DeviceCapabilities::member());

    let relay = spawn_relay(
        &user,
        relay_dev,
        &[42],
        &[a.device_pubkey(), b.device_pubkey()],
    )
    .await;
    let relay_addr = relay.relay.node_addr();

    // Peer A dials the relay.
    let ep_a = client_endpoint().await;
    let a_client = RelayClient::connect(
        ep_a,
        relay_addr.clone(),
        user.user_pk,
        a.cert.clone(),
        a.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("A connect");

    // Peer B dials the relay.
    let ep_b = client_endpoint().await;
    let b_client = RelayClient::connect(
        ep_b,
        relay_addr.clone(),
        user.user_pk,
        b.cert.clone(),
        b.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("B connect");

    a_client
        .send(Envelope::Subscribe { workspace_id: 42 })
        .unwrap();
    b_client
        .send(Envelope::Subscribe { workspace_id: 42 })
        .unwrap();

    // Drain the Ready acks so we don't confuse them for the delta
    // later.
    for cli in [&a_client, &b_client] {
        let env = tokio::time::timeout(Duration::from_secs(2), cli.recv())
            .await
            .expect("Ready timeout")
            .expect("Ready channel closed");
        match env {
            Envelope::Ready {
                workspace_id: 42,
                pending_deltas: 0,
            } => {}
            other => panic!("expected Ready(42, 0), got {other:?}"),
        }
    }

    // A publishes a delta. `delta` bytes are opaque to the relay.
    let payload = b"the-crdt-delta".to_vec();
    let delta_env = Envelope::Delta {
        workspace_id: 42,
        author_device_public_key: a.device_pubkey(),
        authored_at_ns: 1000,
        delta: payload.clone(),
        signature: [1u8; 64],
    };
    a_client.send(delta_env.clone()).unwrap();

    // B receives it.
    let received = tokio::time::timeout(Duration::from_secs(2), b_client.recv())
        .await
        .expect("delta timeout")
        .expect("delta channel closed");
    match received {
        Envelope::Delta { workspace_id, delta, .. } => {
            assert_eq!(workspace_id, 42);
            assert_eq!(delta, payload);
        }
        other => panic!("expected Delta, got {other:?}"),
    }

    // A does NOT receive its own delta back (author-exclusion).
    // We wait a short window; anything within it is a bug.
    let ghost = tokio::time::timeout(Duration::from_millis(200), a_client.recv()).await;
    assert!(ghost.is_err(), "A should not receive its own delta");

    // Metrics reflect activity.
    let snap = relay.relay.handle().metrics.snapshot();
    assert!(snap.conns_authed >= 2, "at least both peers authed");

    // Squelch clippy's "unused" for the awaiter helper.
    let _ = wait_for(|| true, 1).await;
}
