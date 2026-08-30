//! Federation smoke test — two relays bound to the same workspace,
//! each dialing the other. An op published by a device on relay1
//! reaches a device connected to relay2 via the peer-relay session.
//!
//! The whole delivery hits the SAME `WorkspaceRuntime::fanout_delta`
//! device peers use — the only new code is a one-line filter that
//! suppresses `PeerRelay` → `PeerRelay` echoes. This test proves the
//! reuse: no new fan-out, no new transport, no new store path.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use std::time::Duration;

use qos_relay::{Envelope, RelayClient};
use qos_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_relay};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn op_on_relay1_reaches_device_on_relay2() {
    let user = TestUser::fresh("Alice");
    let relay_dev_1 = user.enrol("home-server-1", DeviceCapabilities::host());
    let relay_dev_2 = user.enrol("home-server-2", DeviceCapabilities::host());
    let p1 = user.enrol("laptop", DeviceCapabilities::primary());
    let p2 = user.enrol("phone", DeviceCapabilities::member());

    let members = vec![p1.device_pubkey(), p2.device_pubkey()];
    let relay1 = spawn_relay(&user, relay_dev_1, &[3], &members).await;
    let relay2 = spawn_relay(&user, relay_dev_2, &[3], &members).await;

    // Wire them together — each dials the other. Same config option
    // a real deployment uses. Two directions means both A→B and B→A
    // flow through their own peer-relay session (no bidirectional
    // subscribe on one connection). Use the full EndpointAddr in
    // tests so we don't wait on iroh discovery.
    relay1
        .relay
        .add_peer_relay_at(relay2.relay.node_addr(), "relay2".into(), 0);
    relay2
        .relay
        .add_peer_relay_at(relay1.relay.node_addr(), "relay1".into(), 0);

    // Peer p1 → relay1, peer p2 → relay2.
    let ep1 = client_endpoint().await;
    let c1 = RelayClient::connect(
        ep1,
        relay1.relay.node_addr(),
        user.user_pk,
        p1.cert.clone(),
        p1.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("p1 dial relay1");
    let ep2 = client_endpoint().await;
    let c2 = RelayClient::connect(
        ep2,
        relay2.relay.node_addr(),
        user.user_pk,
        p2.cert.clone(),
        p2.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("p2 dial relay2");

    c1.send(Envelope::Subscribe { workspace_id: 3 }).unwrap();
    c2.send(Envelope::Subscribe { workspace_id: 3 }).unwrap();

    // Drain the Ready acks.
    for cli in [&c1, &c2] {
        let env = tokio::time::timeout(Duration::from_secs(3), cli.recv())
            .await
            .expect("Ready timeout")
            .expect("closed");
        assert!(matches!(env, Envelope::Ready { .. }));
    }

    // Give the peer-relay dial tasks a beat to complete their
    // handshake + PeerRelayHello exchange + Subscribe. Reconnect
    // starts with a 1s backoff on failed initial dial, but on a
    // clean localhost dial this is normally under 500ms.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // p1 publishes on relay1.
    let payload = b"federated-delta".to_vec();
    c1.send(Envelope::Delta {
        workspace_id: 3,
        author_device_public_key: p1.device_pubkey(),
        authored_at_ns: 100,
        delta: payload.clone(),
        signature: [1u8; 64],
    })
    .unwrap();

    // p2, connected only to relay2, must still see the delta —
    // the ONLY path is relay1's fanout → peer-relay session → relay2
    // fanout → p2.
    let mut got = false;
    for _ in 0..5 {
        let env = tokio::time::timeout(Duration::from_secs(3), c2.recv())
            .await
            .expect("federation delta timeout")
            .expect("closed");
        if let Envelope::Delta {
            delta,
            workspace_id: 3,
            ..
        } = env
        {
            assert_eq!(delta, payload);
            got = true;
            break;
        }
        // Ignore stray Ready envelopes.
    }
    assert!(
        got,
        "p2 must receive the delta via the federated peer relay"
    );
}
