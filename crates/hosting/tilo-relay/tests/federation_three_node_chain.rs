//! Three-node relay chain: R1 — R2 — R3, where R1 and R3 are NOT
//! directly federated; R2 is the only bridge.
//!
//! This is the adversarial multi-host case the pure fan-out tier cannot
//! serve on its own. A delta authored on R1 reaches R2 via live
//! fan-out, but R2 must NOT re-forward a peer-relay-origin delta to
//! another peer relay (echo suppression — the "one filter line" that
//! stops a federated mesh from looping). So the delta never reaches R3
//! via live fan-out, and a device connected to R3 would be stranded.
//!
//! Anti-entropy heals the chain. R2 *materializes* the delta into its
//! pinned doc when it arrives; R3's federated session periodically
//! reconciles against R2 (`SyncRequest` → `SyncResponse`), ingests the
//! missing change, and fans it out to its local devices. The device on
//! R3 therefore converges with the author on R1 across two hops — the
//! guarantee the materialize + anti-entropy tier adds on top of live
//! fan-out.
//!
//! Asserted on genuine Automerge docs (identical `get_heads()`) over the
//! real `Relay` / `RelayClient` path — no mocks. Everything runs
//! in-process over iroh's loopback; genuine cross-machine NAT traversal
//! is out of scope for a headless test (noted in the module docs).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};
use tilo_automerge::ApplyIncoming;
use tilo_automerge::ReadTyped as _;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT};
use tilo_relay::{Envelope, RelayClient, apply_sync_response, send_sync_request};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_relay, wait_until_subscribed};

/// Connect B to `r3_addr`, subscribe, and drain until it converges with
/// `doc_a` — applying live `Delta`s R3 fans out plus any `SyncResponse`,
/// nudging a `SyncRequest` on each idle tick as a backstop. Returns B's
/// document and whether it actually converged within the 15s budget.
async fn drain_until_converged(
    r3_addr: iroh_base::EndpointAddr,
    user: &TestUser,
    b: &crate::common::TestDevice,
    ws: u64,
    doc_a: &mut AutoCommit,
) -> (AutoCommit, bool) {
    let b_client = RelayClient::connect(
        client_endpoint().await,
        r3_addr,
        user.user_pk,
        b.cert.clone(),
        b.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("B dial R3");
    b_client
        .send(Envelope::Subscribe { workspace_id: ws })
        .unwrap();

    let mut doc_b = AutoCommit::new();
    doc_b.set_actor(automerge::ActorId::from(b"bob".as_slice()));
    let _ = send_sync_request(&b_client, ws, &mut doc_b);

    let mut converged = false;
    let deadline = tilo_test_env::hang_deadline();
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(400), b_client.recv()).await {
            Ok(Some(Envelope::Delta { delta, .. })) => {
                let _ = doc_b.load_incoming(&delta);
            }
            Ok(Some(Envelope::SyncResponse { delta_since, .. })) => {
                let _ = apply_sync_response(&mut doc_b, &delta_since);
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {
                // Idle — nudge anti-entropy against R3 again.
                let _ = send_sync_request(&b_client, ws, &mut doc_b);
            }
        }
        if !doc_b.get_heads().is_empty() && doc_b.get_heads() == doc_a.get_heads() {
            converged = true;
            break;
        }
    }
    drop(b_client);
    (doc_b, converged)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delta_traverses_relay_chain_via_anti_entropy() {
    const WS: u64 = 41;

    let user = TestUser::fresh("Alice");
    let r1_dev = user.enrol("relay-1", DeviceCapabilities::host());
    let r2_dev = user.enrol("relay-2", DeviceCapabilities::host());
    let r3_dev = user.enrol("relay-3", DeviceCapabilities::host());
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("phone", DeviceCapabilities::member());
    let members = vec![a.device_pubkey(), b.device_pubkey()];

    let (r1_key, r2_key, r3_key) = (
        r1_dev.device_pubkey(),
        r2_dev.device_pubkey(),
        r3_dev.device_pubkey(),
    );
    let r1 = Box::pin(spawn_relay(&user, r1_dev, &[WS], &members)).await;
    let r2 = Box::pin(spawn_relay(&user, r2_dev, &[WS], &members)).await;
    let r3 = Box::pin(spawn_relay(&user, r3_dev, &[WS], &members)).await;

    // Wire the CHAIN only: R1<->R2 and R2<->R3. R1 and R3 never federate
    // directly, so R2 is the sole bridge.
    r1.relay
        .add_peer_relay_at(r2.relay.node_addr(), "r2".into(), 0);
    r2.relay
        .add_peer_relay_at(r1.relay.node_addr(), "r1".into(), 0);
    r2.relay
        .add_peer_relay_at(r3.relay.node_addr(), "r3".into(), 0);
    r3.relay
        .add_peer_relay_at(r2.relay.node_addr(), "r2".into(), 0);

    // Every link of the chain has subscribed in both directions.
    wait_until_subscribed(&r1.relay, WS, &r2_key).await;
    wait_until_subscribed(&r2.relay, WS, &r1_key).await;
    wait_until_subscribed(&r2.relay, WS, &r3_key).await;
    wait_until_subscribed(&r3.relay, WS, &r2_key).await;

    let a_client = RelayClient::connect(
        client_endpoint().await,
        r1.relay.node_addr(),
        user.user_pk,
        a.cert.clone(),
        a.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("A dial R1");
    a_client
        .send(Envelope::Subscribe { workspace_id: WS })
        .unwrap();
    let _ = tilo_test_env::within("A Ready", a_client.recv()).await;

    let mut doc_a = AutoCommit::new();
    doc_a.set_actor(automerge::ActorId::from(b"alice".as_slice()));
    doc_a.put(ROOT, "note", "across-the-chain").unwrap();
    let d1 = doc_a.save_incremental();
    a_client
        .send(Envelope::Delta {
            workspace_id: WS,
            author_device_public_key: a.device_pubkey(),
            authored_at_ns: 1,
            delta: d1.clone(),
            signature: [0u8; 64],
        })
        .unwrap();

    // ---- Device on R3 must converge — only possible through the healed
    //      chain (R1 -> R2 materialize -> R3 anti-entropy). ----
    let (mut doc_b, converged) =
        drain_until_converged(r3.relay.node_addr(), &user, &b, WS, &mut doc_a).await;

    assert!(
        converged,
        "device on R3 must converge with the author on R1 across the two-hop chain"
    );
    assert_eq!(
        doc_b.str_at(&ROOT, "note").as_deref(),
        Some("across-the-chain")
    );
    assert_eq!(
        doc_b.get_heads(),
        doc_a.get_heads(),
        "B (on R3) converges with A (on R1) — the chain is healed by materialize + anti-entropy"
    );

    drop(a_client);
    drop(r1);
    drop(r2);
    drop(r3);
}
