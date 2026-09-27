//! Federation echo suppression — the "one filter line" claim.
//!
//! Two relays with bidirectional peer-relay federation. A peer
//! publishes a delta on relay1. Relay1 fans to relay2 → relay2 must
//! NOT bounce it back to relay1 via the reverse peer-relay session,
//! and the author on relay1 must NOT receive its own delta echoed
//! back through the federation loop.
//!
//! Concretely: after the author publishes, the author's inbound
//! channel receives ZERO envelopes over the window.

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

use tilo_relay::{Envelope, RelayClient};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_relay, wait_until_subscribed};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn author_never_receives_echo_through_federation() {
    let user = TestUser::fresh("Alice");
    let relay_dev_1 = user.enrol("relay-1", DeviceCapabilities::host());
    let relay_dev_2 = user.enrol("relay-2", DeviceCapabilities::host());
    let p1 = user.enrol("laptop", DeviceCapabilities::primary());

    let (relay_key_1, relay_key_2) = (relay_dev_1.device_pubkey(), relay_dev_2.device_pubkey());
    let relay1 = Box::pin(spawn_relay(&user, relay_dev_1, &[9], &[p1.device_pubkey()])).await;
    let relay2 = Box::pin(spawn_relay(&user, relay_dev_2, &[9], &[p1.device_pubkey()])).await;

    // Bidirectional federation — the ideal echo scenario. Use the
    // full EndpointAddr so tests skip iroh discovery.
    relay1
        .relay
        .add_peer_relay_at(relay2.relay.node_addr(), "r2".into(), 0);
    relay2
        .relay
        .add_peer_relay_at(relay1.relay.node_addr(), "r1".into(), 0);

    let ep = client_endpoint().await;
    let c = RelayClient::connect(
        ep,
        relay1.relay.node_addr(),
        user.user_pk,
        p1.cert.clone(),
        p1.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    c.send(Envelope::Subscribe { workspace_id: 9 }).unwrap();
    let _ = tilo_test_env::within("ready timeout", c.recv()).await;

    // Both directions of the federation are live, so an echo has a path.
    wait_until_subscribed(&relay1.relay, 9, &relay_key_2).await;
    wait_until_subscribed(&relay2.relay, 9, &relay_key_1).await;

    c.send(Envelope::Delta {
        workspace_id: 9,
        author_device_public_key: p1.device_pubkey(),
        authored_at_ns: 100,
        delta: b"once".to_vec(),
        signature: [1u8; 64],
    })
    .unwrap();

    // Watch for ANY echo of the author's own delta. Author exclusion
    // in fanout on relay1 already prevents the local echo; the
    // federation loop is what we're checking here — must not send
    // it back via relay2.
    let mut echoes = 0usize;
    for _ in 0..5 {
        if let Ok(Some(env)) = tokio::time::timeout(Duration::from_millis(300), c.recv()).await
            && matches!(env, Envelope::Delta { .. })
        {
            echoes += 1;
        }
    }
    assert_eq!(
        echoes, 0,
        "author received {echoes} echo(es) via federation"
    );
}
