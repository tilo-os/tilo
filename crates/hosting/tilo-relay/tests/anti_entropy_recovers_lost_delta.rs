//! Anti-entropy recovers a delta that the store-and-forward tier lost.
//!
//! The per-peer pending queue is drain-then-delete: on `Subscribe` the
//! host flushes a peer's queued deltas to the wire and *removes them
//! from disk*. That flush is not transactional with the peer durably
//! applying them — if the peer's wire dies (or its process restarts)
//! after the host deleted the queued copy but before the peer applied
//! it, the delta is gone from both sides' recovery paths. The peer
//! reconnects, finds an empty queue, and stays permanently behind. This
//! is the exact gap the store-and-forward tier cannot close on its own.
//!
//! Anti-entropy closes it. The host materializes every delta into its
//! pinned Automerge doc, so the change survives even after the queued
//! copy was drained and deleted. The lagging peer advertises its heads
//! (`SyncRequest`); the host replies with the missing changes
//! (`SyncResponse`) computed against that materialized doc; the peer
//! applies them and converges — asserted on Automerge documents (identical
//! `get_heads()`) through the `Relay` / `RelayClient` path.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::Duration;
use tilo_automerge::ReadTyped as _;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT};
use tilo_relay::{Envelope, RelayClient, reconcile};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{HANG_BOUND, TestUser, client_endpoint, spawn_relay_on};

fn pending_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |rd| {
        rd.flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".delta"))
            .count()
    })
}

/// A connects, subscribes, publishes `note = "recover-me"`, and waits
/// until the host has queued it for the (still offline) B. Returns A's
/// client (kept alive so its actor state stays valid) and the document
/// holding the published edit.
async fn publish_offline_edit(
    addr: iroh_base::EndpointAddr,
    user: &TestUser,
    a: &crate::common::TestDevice,
    ws: u64,
    pending_dir: &std::path::Path,
) -> (RelayClient, AutoCommit) {
    let a_client = RelayClient::connect(
        client_endpoint().await,
        addr,
        user.user_pk,
        a.cert.clone(),
        a.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("A connect");
    a_client
        .send(Envelope::Subscribe { workspace_id: ws })
        .unwrap();
    // Drain A's Ready(0).
    match tilo_test_env::within("A Ready timeout", a_client.recv())
        .await
        .expect("A closed")
    {
        Envelope::Ready { pending_deltas, .. } => assert_eq!(pending_deltas, 0),
        other => panic!("expected Ready, got {other:?}"),
    }

    let mut doc_a = AutoCommit::new();
    doc_a.set_actor(automerge::ActorId::from(b"alice".as_slice()));
    doc_a.put(ROOT, "note", "recover-me").unwrap();
    let d1 = doc_a.save_incremental();
    a_client
        .send(Envelope::Delta {
            workspace_id: ws,
            author_device_public_key: a.device_pubkey(),
            authored_at_ns: 1,
            delta: d1.clone(),
            signature: [0u8; 64],
        })
        .unwrap();

    let mut queued = 0;
    // Polls for up to 30 s; only a hang uses the whole budget.
    for _ in 0..1200 {
        queued = pending_count(pending_dir);
        if queued >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(queued, 1, "A's edit is queued for the offline B");
    (a_client, doc_a)
}

/// Connect B, subscribe (draining + deleting its queued delta on the
/// host), then drop the session before it durably applies anything — the
/// disconnect race this whole test proves anti-entropy recovers from.
async fn drain_then_lose_session(
    addr: iroh_base::EndpointAddr,
    user: &TestUser,
    b: &crate::common::TestDevice,
    ws: u64,
    pending_dir: &std::path::Path,
) {
    let b1 = RelayClient::connect(
        client_endpoint().await,
        addr,
        user.user_pk,
        b.cert.clone(),
        b.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("B connect (1)");
    b1.send(Envelope::Subscribe { workspace_id: ws }).unwrap();

    // Wait until the host has flushed + deleted the queued delta from
    // disk (the drain is what removes it). B deliberately does NOT apply
    // what it received — the delta is lost in the disconnect race.
    let mut drained = false;
    // Polls for up to 30 s; only a hang uses the whole budget.
    for _ in 0..1200 {
        if pending_count(pending_dir) == 0 {
            drained = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        drained,
        "host drained + deleted B's queued delta on subscribe"
    );
    drop(b1); // B's wire dies; it never durably applied the drained delta.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anti_entropy_recovers_delta_drained_then_lost() {
    const WS: u64 = 31;

    let user = TestUser::fresh("Alice");
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("phone", DeviceCapabilities::member());
    let members = [a.device_pubkey(), b.device_pubkey()];

    let state = tempfile::tempdir().unwrap();
    let host_dev = user.enrol("home-server", DeviceCapabilities::host());
    let host = Box::pin(spawn_relay_on(
        state.path(),
        &user,
        host_dev,
        &[WS],
        &members,
    ))
    .await;
    let addr = host.node_addr();
    let b_hex = hex::encode(b.device_pubkey());
    let pending_dir = state
        .path()
        .join("workspaces")
        .join(WS.to_string())
        .join("pending")
        .join(&b_hex);

    // ---- A publishes a real edit while B is offline. The host queues it
    //      for B AND materializes it into its pinned doc. ----
    let (a_client, mut doc_a) =
        publish_offline_edit(addr.clone(), &user, &a, WS, &pending_dir).await;

    // ---- B connects (session 1) and drains its queue — the host DELETES
    //      the queued copy — but B's wire dies before it durably applies
    //      the drained delta. Model that by dropping the session right
    //      after the queue has been flushed. ----
    drain_then_lose_session(addr.clone(), &user, &b, WS, &pending_dir).await;

    // The store-and-forward tier is now exhausted for this delta: the
    // queued copy is gone, and a fresh reconnect will find nothing.
    assert_eq!(
        pending_count(&pending_dir),
        0,
        "nothing left in the queue to recover B from"
    );

    // ---- B reconnects (session 2). Its queue is empty, so a plain
    //      subscribe cannot catch it up — it is the previously
    //      unrecoverable state. ----
    let b2 = RelayClient::connect(
        client_endpoint().await,
        addr,
        user.user_pk,
        b.cert.clone(),
        b.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("B reconnect (2)");
    b2.send(Envelope::Subscribe { workspace_id: WS }).unwrap();
    match tilo_test_env::within("B Ready timeout", b2.recv())
        .await
        .expect("B closed")
    {
        // The queue is empty — no catch-up delta is delivered.
        Envelope::Ready { pending_deltas, .. } => {
            assert_eq!(
                pending_deltas, 0,
                "queue is empty; subscribe alone cannot recover d1"
            );
        }
        other => panic!("expected Ready(0), got {other:?}"),
    }

    // B starts from nothing and is divergent from A.
    let mut doc_b = AutoCommit::new();
    doc_b.set_actor(automerge::ActorId::from(b"bob".as_slice()));
    assert_ne!(
        doc_b.get_heads(),
        doc_a.get_heads(),
        "B is behind A before anti-entropy"
    );

    // ---- Anti-entropy: B reconciles against the host's MATERIALIZED
    //      doc — the only place d1 still lives — and converges. ----
    let applied = reconcile(&b2, WS, &mut doc_b, HANG_BOUND)
        .await
        .expect("reconcile round trip");
    assert!(
        applied > 0,
        "host backfilled the lost change via anti-entropy"
    );

    assert_eq!(doc_b.str_at(&ROOT, "note").as_deref(), Some("recover-me"));
    assert_eq!(
        doc_b.get_heads(),
        doc_a.get_heads(),
        "anti-entropy reconciles B to convergence despite the drained-and-lost delta"
    );

    drop(a_client);
    drop(host);
}
