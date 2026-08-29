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

use std::time::{Duration, Instant};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
use qos_relay::{Envelope, RelayClient, apply_sync_response, send_sync_request};
use qos_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_relay};

fn read_str(doc: &AutoCommit, key: &str) -> Option<String> {
    match doc.get(ROOT, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Str(v) => Some(v.to_string()),
            _ => None,
        },
        _ => None,
    }
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

    let r1 = spawn_relay(&user, r1_dev, &[WS], &members).await;
    let r2 = spawn_relay(&user, r2_dev, &[WS], &members).await;
    let r3 = spawn_relay(&user, r3_dev, &[WS], &members).await;

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

    // Let the peer-relay handshakes + subscriptions settle.
    tokio::time::sleep(Duration::from_millis(2500)).await;

    // ---- Author on R1. ----
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
    let _ = tokio::time::timeout(Duration::from_secs(3), a_client.recv())
        .await
        .expect("A Ready");

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
    let b_client = RelayClient::connect(
        client_endpoint().await,
        r3.relay.node_addr(),
        user.user_pk,
        b.cert.clone(),
        b.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("B dial R3");
    b_client
        .send(Envelope::Subscribe { workspace_id: WS })
        .unwrap();

    // Drain everything B receives, applying live Deltas fanned out by R3
    // (once R3 has ingested the change) and any SyncResponse; nudge a
    // device-level SyncRequest against R3 on each idle tick as a backstop.
    let mut doc_b = AutoCommit::new();
    doc_b.set_actor(automerge::ActorId::from(b"bob".as_slice()));
    let _ = send_sync_request(&b_client, WS, &mut doc_b);

    let mut converged = false;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        match tokio::time::timeout(Duration::from_millis(400), b_client.recv()).await {
            Ok(Some(Envelope::Delta { delta, .. })) => {
                let _ = doc_b.load_incremental(&delta);
            }
            Ok(Some(Envelope::SyncResponse { delta_since, .. })) => {
                let _ = apply_sync_response(&mut doc_b, &delta_since);
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {
                // Idle — nudge anti-entropy against R3 again.
                let _ = send_sync_request(&b_client, WS, &mut doc_b);
            }
        }
        if !doc_b.get_heads().is_empty() && doc_b.get_heads() == doc_a.get_heads() {
            converged = true;
            break;
        }
    }

    assert!(
        converged,
        "device on R3 must converge with the author on R1 across the two-hop chain"
    );
    assert_eq!(
        read_str(&doc_b, "note").as_deref(),
        Some("across-the-chain")
    );
    assert_eq!(
        doc_b.get_heads(),
        doc_a.get_heads(),
        "B (on R3) converges with A (on R1) — the chain is healed by materialize + anti-entropy"
    );

    drop(a_client);
    drop(b_client);
    drop(r1);
    drop(r2);
    drop(r3);
}
