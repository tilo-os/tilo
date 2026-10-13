//! Cross-user peer relay is rejected — federation adds ZERO new
//! trust surface. A peer relay whose device cert chains to a
//! different user identity is rejected by the SAME auth handshake
//! that already rejects cross-user device clients.
//!
//! We spin up two relays under two different users, put user A's
//! relay1 into a `peer_relays` config pointing at user B's relay2,
//! and assert:
//!  - the dial task's `RelayClient::connect` fails at the mutual-
//!    auth stage (user B's cert doesn't chain to user A's root);
//!  - no state for any workspace leaks from B → A.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use tilo_relay::{Envelope, RelayClient};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_relay, wait_until};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_user_peer_relay_is_rejected() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");

    let a_relay_dev = alice.enrol("alice-r1", DeviceCapabilities::host());
    let b_relay_dev = bob.enrol("bob-r2", DeviceCapabilities::host());
    let a_client_dev = alice.enrol("alice-laptop", DeviceCapabilities::primary());

    // Same workspace id on both — id collision on its own is not a
    // vulnerability because auth is per-user; this is the setup a
    // malicious operator would use to try to smuggle state across.
    let a_relay = Box::pin(spawn_relay(
        &alice,
        a_relay_dev,
        &[42],
        &[a_client_dev.device_pubkey()],
    ))
    .await;
    let b_relay = Box::pin(spawn_relay(&bob, b_relay_dev, &[42], &[])).await;

    // Alice's relay is told to federate with Bob's — the dial task
    // will run, but the mutual-auth check on the returning HelloAck
    // must reject Bob's cert (wrong user root) → connection tears
    // down before any envelope flows.
    a_relay
        .relay
        .add_peer_relay_at(b_relay.relay.node_addr(), "bob-r2".into(), 0);

    // Bob's relay has seen the federation attempt and refused it.
    let bob_metrics = b_relay.relay.handle().metrics;
    wait_until("Bob's relay to refuse Alice's peer-relay handshake", || {
        bob_metrics.snapshot().auth_rejected > 0
    })
    .await;

    // Alice's client publishes a delta on Alice's relay. If auth
    // had been bypassed, it would flow to Bob's relay and end up
    // through Bob's ingress and count as a delta Bob's relay accepted.
    let ep = client_endpoint().await;
    let a = RelayClient::connect(
        ep,
        a_relay.relay.node_addr(),
        alice.user_pk,
        a_client_dev.cert.clone(),
        a_client_dev.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    a.send(Envelope::Subscribe { workspace_id: 42 }).unwrap();
    let _ = tokio::time::timeout(tilo_test_env::hang_bound(), a.recv()).await;
    a.send(Envelope::Delta {
        workspace_id: 42,
        author_device_public_key: a_client_dev.device_pubkey(),
        authored_at_ns: 1,
        delta: b"alice-only".to_vec(),
        signature: [1u8; 64],
    })
    .unwrap();
    // Alice's host must have committed the delta before we judge that Bob
    // never saw it — otherwise a slow disk makes the isolation check a
    // race against the writer's own latency.
    let alice_metrics = a_relay.relay.handle().metrics;
    wait_until("Alice's relay committed the published delta", || {
        alice_metrics.snapshot().deltas_received == 1
    })
    .await;

    // Bob's relay must have accepted NO deltas, and never authenticated a
    // session at all: no ingress path (delta, sync, pending) had a peer.
    let bob = bob_metrics.snapshot();
    assert_eq!(
        bob.deltas_received, 0,
        "cross-user peer relay must not receive any delta — {} leaked",
        bob.deltas_received,
    );
    assert_eq!(
        bob.conns_authed, 0,
        "cross-user peer relay must not authenticate Alice's relay",
    );

    // Bob's pending dir for Alice's client must not exist either.
    let leak = b_relay
        .state_root
        .join("workspaces")
        .join("42")
        .join("pending")
        .join(hex::encode(a_client_dev.device_pubkey()));
    assert!(
        !leak.exists(),
        "cross-user peer relay must not queue: {leak:?}"
    );
}
