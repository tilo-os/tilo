//! End-to-end workspace sync through a host: two member devices + a
//! durable host, Automerge deltas, live fan-out, author
//! exclusion, and offline store-and-forward.
//!
//! This is the whole "workspace = unit of sync" contract in one
//! timeline, asserted on the `WorkspaceRuntime`/daemon path with
//! Automerge documents so convergence — not just byte-equality of an opaque
//! blob — is what gets proven:
//!
//!   1. Host + devices A and B come up; A and B are workspace
//!      members. A and B both subscribe.
//!   2. A edits its CRDT (`title = "hello from A"`) and publishes the
//!      incremental delta. B receives it via the host's fan-out and
//!      *converges* — B's document, started empty, ends with A's
//!      value and identical heads.
//!   3. A does NOT receive its own delta back (author exclusion).
//!   4. B goes offline (deterministically: it unsubscribes, waits for
//!      a Ping/Pong round-trip that proves the host tore its
//!      subscription down, then drops its connection).
//!   5. A edits again (`body = "second edit"`) while B is offline.
//!      The delta is store-and-forwarded to disk under
//!      `workspaces/<id>/pending/<hex(B)>/`.
//!   6. B reconnects, subscribes, and the host flushes the queued
//!      delta. B applies it and now fully converges with A across
//!      *both* edits.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use std::time::Duration;
use tilo_automerge::ApplyIncoming;
use tilo_automerge::ReadTyped as _;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT};
use tilo_relay::{Envelope, RelayClient};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_relay};

/// How long a device waits for an answer the host owes it. Every wait below
/// is for a reply the protocol guarantees, so this only turns a hang into a
/// named failure before the test runner's own timeout. It is not a latency
/// budget: each offline fan-out fsyncs the pending queue, and on a busy disk
/// one fsync has been measured at 30 s.
const REPLY: Duration = Duration::from_secs(180);

/// Receive the next envelope, failing with `what` on a hang or a closed
/// connection.
async fn recv_reply(client: &RelayClient, what: &str) -> Envelope {
    tokio::time::timeout(REPLY, client.recv())
        .await
        .unwrap_or_else(|_| panic!("{what}: no reply from the host"))
        .unwrap_or_else(|| panic!("{what}: channel closed"))
}

/// Round-trip a Ping/Pong on `client`. The host handles a peer's stream in
/// order and answers Ping only after the preceding envelope's handling has
/// completed, so Pong proves everything sent before it has been applied.
async fn round_trip(client: &RelayClient, what: &str) {
    client.send(Envelope::Ping).unwrap();
    loop {
        match recv_reply(client, what).await {
            Envelope::Pong => return,
            Envelope::Ready { .. } => {}
            other => panic!("{what}: unexpected envelope {other:?}"),
        }
    }
}

/// Receive envelopes until a `Delta` arrives (skipping any interleaved
/// `Ready`/`Pong`), returning its delta bytes. A delta that never arrives
/// hangs, and nextest's slow-timeout reports it.
async fn recv_delta(client: &RelayClient, what: &str) -> Vec<u8> {
    loop {
        match recv_reply(client, what).await {
            Envelope::Delta { delta, .. } => return delta,
            Envelope::Ready { .. } | Envelope::Pong => {}
            other => panic!("{what}: unexpected envelope {other:?}"),
        }
    }
}

/// Connect A and B to the relay at `addr`, subscribe both to `ws`, and
/// drain their initial `Ready(ws, 0)` acks (nothing queued yet).
async fn connect_and_subscribe(
    addr: iroh_base::EndpointAddr,
    user: &TestUser,
    a: &crate::common::TestDevice,
    b: &crate::common::TestDevice,
    ws: u64,
) -> (RelayClient, RelayClient) {
    let ep_a = client_endpoint().await;
    let a_client = RelayClient::connect(
        ep_a,
        addr.clone(),
        user.user_pk,
        a.cert.clone(),
        a.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("A connect");
    let ep_b = client_endpoint().await;
    let b_client = RelayClient::connect(
        ep_b,
        addr,
        user.user_pk,
        b.cert.clone(),
        b.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("B connect");

    a_client
        .send(Envelope::Subscribe { workspace_id: ws })
        .unwrap();
    b_client
        .send(Envelope::Subscribe { workspace_id: ws })
        .unwrap();
    // Drain both Ready(ws, 0) acks (nothing queued yet).
    for (cli, who) in [(&a_client, "A"), (&b_client, "B")] {
        let env = recv_reply(cli, &format!("{who} Ready")).await;
        assert!(
            matches!(
                env,
                Envelope::Ready {
                    workspace_id: w,
                    pending_deltas: 0
                } if w == ws
            ),
            "{who} expected Ready(WS,0), got {env:?}"
        );
    }
    (a_client, b_client)
}

/// Unsubscribe B, then round-trip a Ping/Pong: Pong proves the
/// Unsubscribe was fully applied — B is no longer a subscriber, so
/// the next delta is guaranteed to be store-and-forwarded rather
/// than racing a half-torn-down live channel.
async fn take_b_offline(b_client: &RelayClient, ws: u64) {
    b_client
        .send(Envelope::Unsubscribe { workspace_id: ws })
        .unwrap();
    round_trip(b_client, "B unsubscribe").await;
}

/// Reconnect B, subscribe, and let the flushed queued delta bring it back
/// in sync with A — including the convergence assertions.
async fn reconnect_b_and_catch_up(
    addr: iroh_base::EndpointAddr,
    user: &TestUser,
    b: &crate::common::TestDevice,
    ws: u64,
    doc_a: &mut AutoCommit,
    doc_b: &mut AutoCommit,
) {
    let ep_b2 = client_endpoint().await;
    let b2 = RelayClient::connect(
        ep_b2,
        addr,
        user.user_pk,
        b.cert.clone(),
        b.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("B reconnect");
    b2.send(Envelope::Subscribe { workspace_id: ws }).unwrap();

    // The queued delta is flushed on subscribe; apply it to B's doc.
    let got2 = recv_delta(&b2, "B catch-up delta").await;
    doc_b.load_incoming(&got2).expect("B applies d2");

    // The host writes the replay before it acknowledges the subscription, so
    // Ready follows the catch-up delta and counts it.
    let ready = recv_reply(&b2, "B catch-up Ready").await;
    assert!(
        matches!(
            ready,
            Envelope::Ready {
                workspace_id: w,
                pending_deltas,
            } if w == ws && pending_deltas >= 1
        ),
        "B expected Ready(WS) counting its catch-up replay, got {ready:?}"
    );

    assert_eq!(
        doc_b.str_at(&ROOT, "title").as_deref(),
        Some("hello from A")
    );
    assert_eq!(doc_b.str_at(&ROOT, "body").as_deref(), Some("second edit"));
    assert_eq!(
        doc_a.get_heads(),
        doc_b.get_heads(),
        "B must fully converge with A after catching up offline edits"
    );
}

/// Number of `.delta` files currently queued in `dir`.
fn count_delta_files(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |rd| {
        rd.flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".delta"))
            .count()
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_devices_converge_through_host_with_offline_store_forward() {
    const WS: u64 = 7;

    let user = TestUser::fresh("Alice");
    let host_dev = user.enrol("home-server", DeviceCapabilities::host());
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("phone", DeviceCapabilities::member());

    // A durable host that pins workspace 7 and knows A + B are members.
    let host = Box::pin(spawn_relay(
        &user,
        host_dev,
        &[WS],
        &[a.device_pubkey(), b.device_pubkey()],
    ))
    .await;
    let addr = host.relay.node_addr();

    // Both devices dial the host, subscribe, and drain their initial Ready acks.
    let (a_client, b_client) = connect_and_subscribe(addr.clone(), &user, &a, &b, WS).await;

    // ---- Live edit: A mutates its CRDT and publishes the delta. ----
    let mut doc_a = AutoCommit::new();
    doc_a.put(ROOT, "title", "hello from A").unwrap();
    let d1 = doc_a.save_incremental();
    assert!(!d1.is_empty(), "first incremental must carry the edit");
    a_client
        .send(Envelope::Delta {
            workspace_id: WS,
            author_device_public_key: a.device_pubkey(),
            authored_at_ns: 1,
            delta: d1.clone(),
            signature: [0u8; 64],
        })
        .unwrap();

    // B receives it via the host's fan-out and converges.
    let got1 = recv_delta(&b_client, "B live delta").await;
    let mut doc_b = AutoCommit::new();
    doc_b.load_incoming(&got1).expect("B applies d1");
    assert_eq!(
        doc_b.str_at(&ROOT, "title").as_deref(),
        Some("hello from A")
    );
    assert_eq!(
        doc_a.get_heads(),
        doc_b.get_heads(),
        "B must converge with A after the live delta"
    );

    // Author exclusion: A must NOT receive its own delta back.
    let ghost = tokio::time::timeout(Duration::from_millis(300), a_client.recv()).await;
    assert!(ghost.is_err(), "author must not receive its own delta");

    // ---- Take B offline, deterministically. ----
    take_b_offline(&b_client, WS).await;
    drop(b_client); // B's device is now fully offline.

    // ---- Offline edit: A edits again; delta must persist for B. ----
    doc_a.put(ROOT, "body", "second edit").unwrap();
    let d2 = doc_a.save_incremental();
    assert!(!d2.is_empty());
    a_client
        .send(Envelope::Delta {
            workspace_id: WS,
            author_device_public_key: a.device_pubkey(),
            authored_at_ns: 2,
            delta: d2.clone(),
            signature: [0u8; 64],
        })
        .unwrap();

    // The host accepts the delta on A's stream and writes B's pending queue
    // entry on its persistence turn, off that stream: A's Pong does not
    // order the write, so wait for the queue itself.
    let b_hex = hex::encode(b.device_pubkey());
    let pending_dir = host
        .state_root
        .join("workspaces")
        .join(WS.to_string())
        .join("pending")
        .join(&b_hex);
    let mut queued = count_delta_files(&pending_dir);
    while queued == 0 {
        tokio::time::sleep(Duration::from_millis(25)).await;
        queued = count_delta_files(&pending_dir);
    }
    assert_eq!(
        queued, 1,
        "A's offline edit must be store-and-forwarded to disk for B"
    );

    // ---- B reconnects and catches up. ----
    reconnect_b_and_catch_up(addr, &user, &b, WS, &mut doc_a, &mut doc_b).await;

    // Queue drains after delivery. The drain follows the delivered flush on
    // the host's persistence turn, so it may land just after B has applied
    // it; a queue that never drains hangs and nextest's slow-timeout reports it.
    while count_delta_files(&pending_dir) != 0 {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
