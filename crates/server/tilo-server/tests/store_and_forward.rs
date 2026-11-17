//! Store-and-forward: a delta authored while peer B is offline
//! must be queued on the server's disk under
//! `workspaces/<workspace id>/pending/<hex(pubkey_B)>/`, then delivered on
//! B's next Subscribe.
//!
//! Timeline:
//!   1. server + peers A and B start; both are workspace 5 members.
//!   2. Peer A connects + subscribes.
//!   3. Peer A publishes delta D1. B is offline → D1 gets queued.
//!   4. Peer A disconnects, publishes nothing more.
//!   5. Peer B connects + subscribes.
//!   6. B's replay carries the workspace's own state and both queued deltas
//!      in order before the server's Ready.

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

use tilo_server::Envelope;
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_server};
use tilo_server::WorkspaceId;

const WORKSPACE: WorkspaceId = WorkspaceId::from_u128(5);

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
    let a_client = tilo_server::ServerClient::connect(
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
        .send(Envelope::Subscribe {
            workspace_id: WORKSPACE,
        })
        .unwrap();
    // A held workspace sends its own state before it is ready.
    crate::common::next(&a_client, |env| matches!(env, Envelope::Ready { .. })).await;

    // A publishes two deltas while B is offline.
    for (seq, payload) in [(1u64, "first-delta"), (2u64, "second-delta")] {
        a_client
            .send(Envelope::Delta {
                workspace_id: WORKSPACE,
                author_device_public_key: a.device_pubkey(),
                authored_at_ns: seq,
                delta: payload.as_bytes().to_vec(),
                signature: [7u8; 64],
            })
            .unwrap();
    }
    // The writes land asynchronously on the server's persistence turn: wait
    // for them rather than for a fixed time. The held workspace queues state
    // of its own beside them, so each delta is found by its bytes.
    let deadline = tilo_test_env::hang_deadline();
    while !(crate::common::queued_carries(pending_dir, b"first-delta")
        && crate::common::queued_carries(pending_dir, b"second-delta"))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "deltas should reach B's queue on disk: {} queued",
            queued_deltas(pending_dir)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// B connects, subscribes, and drains the two queued deltas plus the
/// Ready marker (the server drains pending before it sends Ready, so B
/// receives Delta, Delta, then Ready).
async fn b_connects_and_drains_queue(
    addr: iroh_base::EndpointAddr,
    user: &TestUser,
    b: &crate::common::TestDevice,
) {
    let b_client = tilo_server::ServerClient::connect(
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
        .send(Envelope::Subscribe {
            workspace_id: WORKSPACE,
        })
        .unwrap();

    // The server drains pending BEFORE it sends Ready, so the two queued
    // deltas arrive in order before it, interleaved with the state the
    // workspace sends first.
    let mut got_payloads: Vec<Vec<u8>> = Vec::new();
    loop {
        match crate::common::next(&b_client, |env| {
            matches!(env, Envelope::Delta { .. } | Envelope::Ready { .. })
        })
        .await
        {
            Envelope::Delta {
                workspace_id,
                delta,
                ..
            } => {
                assert_eq!(workspace_id, WORKSPACE);
                if delta == b"first-delta" || delta == b"second-delta" {
                    got_payloads.push(delta);
                }
            }
            Envelope::Ready {
                workspace_id,
                pending_deltas,
                ..
            } => {
                assert_eq!(workspace_id, WORKSPACE);
                assert!(pending_deltas >= 2, "both queued deltas were replayed");
                break;
            }
            other => panic!("unexpected env: {other:?}"),
        }
    }
    assert_eq!(
        got_payloads,
        vec![b"first-delta".to_vec(), b"second-delta".to_vec()],
        "queued deltas must be delivered in-order"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_peer_gets_queued_deltas_on_reconnect() {
    let user = TestUser::fresh("Alice");
    let server_dev = user.enrol("home-server", DeviceCapabilities::member());
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("phone", DeviceCapabilities::member());

    let server = Box::pin(spawn_server(
        &user,
        server_dev,
        &[WORKSPACE],
        &[&a.cert, &b.cert],
    ))
    .await;
    let addr = server.server.node_addr();

    // Verify the deltas landed on disk under
    // <state>/workspaces/<id>/pending/<hex(b)>/.
    let b_hex = hex::encode(b.device_pubkey());
    let pending_dir = server
        .state_root
        .join("workspaces")
        .join(WORKSPACE.to_string())
        .join("pending")
        .join(&b_hex);
    a_publishes_two_offline_deltas(addr.clone(), &user, &a, &pending_dir).await;

    // B comes online + subscribes, and drains the queue.
    b_connects_and_drains_queue(addr, &user, &b).await;

    // The pending entries retire on the server's persistence turn after
    // delivery, so they may still be on disk as B reads Ready. Wait for the
    // queue to drain, naming what stayed if it does not.
    let deadline = tilo_test_env::hang_deadline();
    while queued_deltas(&pending_dir) != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "delivered deltas should retire from the queue: {} left",
            queued_deltas(&pending_dir)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
