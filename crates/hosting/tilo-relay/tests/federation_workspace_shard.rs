//! Sharded topology: relay1 hosts workspace A, relay2 hosts B, they
//! are peer-relay-federated with each other. An op on A stays on
//! relay1 (relay2 doesn't hold A → no traffic). An op on B stays on
//! relay2 (relay1 doesn't hold B → no traffic).
//!
//! The `PeerRelayHello` handshake computes the intersection of both
//! sides' hosted workspaces — an empty intersection means the pair
//! federates over no workspace at all. That's the whole shard
//! isolation mechanism: hosted-set intersection.

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

use tilo_relay::{Envelope, RelayClient};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_relay, wait_until};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharded_topology_isolates_per_workspace() {
    let user = TestUser::fresh("Alice");
    let relay_dev_1 = user.enrol("r1", DeviceCapabilities::host());
    let relay_dev_2 = user.enrol("r2", DeviceCapabilities::host());
    let author = user.enrol("laptop", DeviceCapabilities::primary());
    let members = vec![author.device_pubkey()];

    // Relay1 hosts workspace 20 only; relay2 hosts workspace 21
    // only — non-overlapping shards.
    let relay1 = Box::pin(spawn_relay(&user, relay_dev_1, &[20], &members)).await;
    let relay2 = Box::pin(spawn_relay(&user, relay_dev_2, &[21], &members)).await;

    // Federate anyway — the intersection is empty.
    relay1
        .relay
        .add_peer_relay_at(relay2.relay.node_addr(), "r2".into(), 0);
    relay2
        .relay
        .add_peer_relay_at(relay1.relay.node_addr(), "r1".into(), 0);
    // Both peer-relay sessions authenticated, so the relays have met and
    // found nothing in common rather than not having met yet.
    let (metrics_1, metrics_2) = (relay1.relay.handle().metrics, relay2.relay.handle().metrics);
    wait_until("the relays to complete a peer-relay handshake", || {
        metrics_1.snapshot().conns_authed > 0 && metrics_2.snapshot().conns_authed > 0
    })
    .await;

    // The author dials relay1, subscribes to 20, publishes on 20.
    let ep = client_endpoint().await;
    let a = RelayClient::connect(
        ep,
        relay1.relay.node_addr(),
        user.user_pk,
        author.cert.clone(),
        author.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    a.send(Envelope::Subscribe { workspace_id: 20 }).unwrap();
    let _ = tilo_test_env::within("Ready(20)", a.recv()).await;

    a.send(Envelope::Delta {
        workspace_id: 20,
        author_device_public_key: author.device_pubkey(),
        authored_at_ns: 100,
        delta: b"on-20".to_vec(),
        signature: [1u8; 64],
    })
    .unwrap();
    // Wait until the host that *should* record the delta has done so —
    // then the isolation check is about the production write path, not a
    // guess that 300 ms was enough for a leak to show up.
    let log20 = relay1
        .state_root
        .join("workspaces")
        .join("20")
        .join("log.jsonl");
    wait_until("relay1 persisted the delta for workspace 20", || {
        std::fs::read(&log20).is_ok_and(|b| !b.is_empty())
    })
    .await;

    // Assertion: relay2 must not have persisted anything for ws20.
    // Its state root has no `workspaces/20/` dir at all.
    let ws20_on_r2 = relay2.state_root.join("workspaces").join("20");
    assert!(
        !ws20_on_r2.exists(),
        "relay2 must not touch a workspace it doesn't host: {ws20_on_r2:?}",
    );

    // Symmetric: an op on 21 dialled through relay2 stays on relay2.
    let ep2 = client_endpoint().await;
    let b = RelayClient::connect(
        ep2,
        relay2.relay.node_addr(),
        user.user_pk,
        author.cert.clone(),
        author.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    b.send(Envelope::Subscribe { workspace_id: 21 }).unwrap();
    let _ = tilo_test_env::within("Ready(21)", b.recv()).await;

    b.send(Envelope::Delta {
        workspace_id: 21,
        author_device_public_key: author.device_pubkey(),
        authored_at_ns: 200,
        delta: b"on-21".to_vec(),
        signature: [1u8; 64],
    })
    .unwrap();
    let log21 = relay2
        .state_root
        .join("workspaces")
        .join("21")
        .join("log.jsonl");
    wait_until("relay2 persisted the delta for workspace 21", || {
        std::fs::read(&log21).is_ok_and(|b| !b.is_empty())
    })
    .await;

    let ws21_on_r1 = relay1.state_root.join("workspaces").join("21");
    assert!(
        !ws21_on_r1.exists(),
        "relay1 must not touch a workspace it doesn't host: {ws21_on_r1:?}",
    );
}
