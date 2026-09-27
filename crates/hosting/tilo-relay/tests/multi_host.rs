//! Multi-host: two relays are bound to the same workspace.
//!
//! Multiple hosts are just more replicas of the same automerge doc
//! and merge like any other peers. The relays here don't implement
//! inter-relay CRDT federation (that lives in the federation module,
//! tested separately); this test targets the routing choice a peer
//! makes: **prefer the nearest reachable host**.
//!
//! The concrete assertion is a peer's failover:
//!
//! 1. Spawn relay1 + relay2 both bound to workspace 3, membership
//!    {peer, peer2}.
//! 2. Peer1 connects to relay1; peer2 connects to relay1 (they
//!    both agree on the "nearest" host).
//! 3. Peer1 publishes a delta — peer2 receives it via relay1.
//! 4. Peer2 fails over: reconnects to relay2 instead. Peer1
//!    publishes another delta. Peer2 does NOT receive it via
//!    relay2 (no inter-relay CRDT federation yet) — but the
//!    delta is queued on relay1 for peer2, so if peer2 comes
//!    back to relay1 it drains.
//!
//! The test therefore confirms:
//!   (a) the peer can dial either relay, opportunistically,
//!   (b) each relay independently maintains its own pending
//!       queue for offline members it knows about (so a fleet of
//!       relays does not "leak" a peer's messages onto a relay
//!       the peer never talks to).

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

use crate::common::{TestUser, client_endpoint, spawn_relay, wait_until};

/// Connect p1 and p2 to relay1, subscribe both to workspace 3, and drain
/// their initial Ready acks.
async fn connect_both_to_relay1(
    user: &TestUser,
    relay1_addr: iroh_base::EndpointAddr,
    p1: &crate::common::TestDevice,
    p2: &crate::common::TestDevice,
) -> (RelayClient, RelayClient) {
    let ep1 = client_endpoint().await;
    let ep2 = client_endpoint().await;
    let c1 = RelayClient::connect(
        ep1,
        relay1_addr.clone(),
        user.user_pk,
        p1.cert.clone(),
        p1.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    let c2 = RelayClient::connect(
        ep2,
        relay1_addr,
        user.user_pk,
        p2.cert.clone(),
        p2.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();

    c1.send(Envelope::Subscribe { workspace_id: 3 }).unwrap();
    c2.send(Envelope::Subscribe { workspace_id: 3 }).unwrap();
    for cli in [&c1, &c2] {
        let env = tilo_test_env::within("Ready timeout", cli.recv())
            .await
            .expect("closed");
        match env {
            Envelope::Ready { .. } => {}
            other => panic!("expected Ready, got {other:?}"),
        }
    }
    (c1, c2)
}

/// Close p2's relay1 session (rather than drop it — a dropped handle may
/// never get `CONNECTION_CLOSE` onto the wire, leaving relay1 to notice
/// only by idle timeout) and wait until relay1 has actually noticed the
/// departure. Nothing makes that atomic with the caller's next delta, so
/// this polls rather than assumes it — a fixed sleep is a race that only
/// shows up under load.
async fn fail_p2_over_from_relay1(
    c2_r1: RelayClient,
    relay1: &crate::common::TestRelay,
    p2_pubkey: [u8; 32],
) {
    c2_r1.close().await;
    // Polls for up to 30 s; only a hang uses the whole budget.
    for _ in 0..1500 {
        if !relay1.relay.is_subscribed(3, &p2_pubkey).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !relay1.relay.is_subscribed(3, &p2_pubkey).await,
        "relay1 never noticed p2 disconnect"
    );
}

/// p2 reconnects to relay1 and drains until it has received the delta
/// that was queued for it while it was away.
async fn p2_returns_and_drains(
    relay1_addr: iroh_base::EndpointAddr,
    user: &TestUser,
    p2: &crate::common::TestDevice,
) {
    let c2_r1_again = RelayClient::connect(
        client_endpoint().await,
        relay1_addr,
        user.user_pk,
        p2.cert.clone(),
        p2.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    c2_r1_again
        .send(Envelope::Subscribe { workspace_id: 3 })
        .unwrap();
    // Pending delivery is at-least-once: a queued delta is acknowledged only
    // after wire delivery (WorkspaceStore::load_pending), so the delta p2
    // received live just before closing may be redelivered once. That
    // duplicate is harmless (signed deltas are idempotent); the queued delta
    // must still arrive exactly once.
    let mut got_delta = false;
    let mut ready = false;
    let mut redelivered = 0_u32;
    while !(ready && got_delta) {
        let env = tilo_test_env::within("a message on c2_r1_again", c2_r1_again.recv())
            .await
            .unwrap();
        match env {
            Envelope::Delta { delta, .. } if delta == b"queued-on-relay1" => {
                assert!(!got_delta, "the queued delta was delivered twice");
                got_delta = true;
            }
            Envelope::Delta { delta, .. } if delta == b"via-relay1" => {
                redelivered = redelivered.saturating_add(1);
                assert_eq!(redelivered, 1, "an already-delivered delta repeated");
            }
            Envelope::Ready {
                workspace_id: 3,
                pending_deltas,
            } if pending_deltas == 1 || pending_deltas == 2 => ready = true,
            other => panic!("unexpected: {other:?}"),
        }
    }
    assert!(got_delta, "p2 should drain the queued delta on reconnect");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn peers_use_several_relays_and_fail_over() {
    // Phase `peer_can_dial_multiple_relays_and_pending_stays_local`.
    {
        let user = TestUser::fresh("Alice");
        let relay_dev_1 = user.enrol("home-server-1", DeviceCapabilities::host());
        let relay_dev_2 = user.enrol("home-server-2", DeviceCapabilities::host());
        let p1 = user.enrol("laptop", DeviceCapabilities::primary());
        let p2 = user.enrol("phone", DeviceCapabilities::member());

        let members = vec![p1.device_pubkey(), p2.device_pubkey()];

        let relay1 = Box::pin(spawn_relay(&user, relay_dev_1, &[3], &members)).await;
        let relay2 = Box::pin(spawn_relay(&user, relay_dev_2, &[3], &members)).await;

        // Peers dial relay1 first — verify they can both dial and
        // fan-out works via relay1.
        let (c1_r1, c2_r1) =
            connect_both_to_relay1(&user, relay1.relay.node_addr(), &p1, &p2).await;

        // p1 publishes a delta via relay1; p2 receives via relay1.
        let d1 = Envelope::Delta {
            workspace_id: 3,
            author_device_public_key: p1.device_pubkey(),
            authored_at_ns: 100,
            delta: b"via-relay1".to_vec(),
            signature: [1u8; 64],
        };
        c1_r1.send(d1).unwrap();
        let got = tilo_test_env::within("a message on c2_r1", c2_r1.recv())
            .await
            .unwrap();
        match got {
            Envelope::Delta { delta, .. } => {
                assert_eq!(delta, b"via-relay1".to_vec());
            }
            other => panic!("expected Delta, got {other:?}"),
        }

        // Now p2 fails over to relay2.
        fail_p2_over_from_relay1(c2_r1, &relay1, p2.device_pubkey()).await;
        let ep3 = client_endpoint().await;
        let c2_r2 = RelayClient::connect(
            ep3,
            relay2.relay.node_addr(),
            user.user_pk,
            p2.cert.clone(),
            p2.sk.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap();
        c2_r2.send(Envelope::Subscribe { workspace_id: 3 }).unwrap();
        let ready = tilo_test_env::within("a message on c2_r2", c2_r2.recv())
            .await
            .unwrap();
        match ready {
            Envelope::Ready {
                workspace_id: 3,
                pending_deltas: 0,
            } => {}
            other => panic!("expected empty Ready on relay2, got {other:?}"),
        }

        // p1 publishes another delta via relay1. Since p2 is on
        // relay2 (no inter-relay CRDT federation), relay1 will
        // enqueue for p2 on ITS disk; relay2 sees nothing.
        let d2 = Envelope::Delta {
            workspace_id: 3,
            author_device_public_key: p1.device_pubkey(),
            authored_at_ns: 200,
            delta: b"queued-on-relay1".to_vec(),
            signature: [2u8; 64],
        };
        c1_r1.send(d2).unwrap();

        // relay1's pending dir for p2 has 1 delta; relay2 has none.
        let hex_p2 = hex::encode(p2.device_pubkey());
        let pending_r1 = relay1.state_root.join("workspaces/3/pending").join(&hex_p2);
        let pending_r2 = relay2.state_root.join("workspaces/3/pending").join(&hex_p2);
        let count = |dir: &std::path::Path| {
            std::fs::read_dir(dir).map_or(0, |rd| {
                rd.flatten()
                    .filter(|e| e.file_name().to_string_lossy().ends_with(".delta"))
                    .count()
            })
        };
        // Wait for the write rather than guessing how long it takes: a fixed sleep
        // is a race that fails on a loaded machine and proves nothing on a fast one.
        wait_until("relay1 to queue the delta for offline p2", || {
            count(&pending_r1) == 1
        })
        .await;
        let count_r1 = count(&pending_r1);
        let count_r2 = count(&pending_r2);
        assert_eq!(count_r1, 1, "relay1 should hold the delta for offline p2");
        assert_eq!(count_r2, 0, "relay2 should not have seen the delta");

        // p2 returns to relay1 and drains the queue.
        drop(c2_r2);
        p2_returns_and_drains(relay1.relay.node_addr(), &user, &p2).await;
    }
    // Phase `client_falls_over_to_reachable_relay`.
    // Failover latency: if the first host doesn't respond, the client
    // tries the next. We simulate an unresponsive host by dialing an
    // unknown iroh node id — the connect should time out quickly and
    // the next dial should succeed within the same time budget.
    {
        let user = TestUser::fresh("Alice");
        let relay_dev = user.enrol("home-server", DeviceCapabilities::host());
        let a = user.enrol("laptop", DeviceCapabilities::primary());
        let relay = Box::pin(spawn_relay(&user, relay_dev, &[7], &[a.device_pubkey()])).await;
        let real_addr = relay.relay.node_addr();

        // Ghost address: a randomly-generated node id nobody listens on.
        let ghost_secret = iroh::SecretKey::from_bytes(&[42u8; 32]);
        let ghost_id = ghost_secret.public();
        let ghost_addr = iroh_base::EndpointAddr::new(ghost_id);

        // Dial the ghost with a short timeout — expect it to fail.
        let ep = client_endpoint().await;
        let ghost_res = tokio::time::timeout(
            Duration::from_secs(3),
            RelayClient::connect(
                ep,
                ghost_addr,
                user.user_pk,
                a.cert.clone(),
                a.sk.clone(),
                RevocationList::new(),
            ),
        )
        .await;
        // Either the connect itself errored, or our outer timeout
        // fired. Both are "the ghost is unreachable within the
        // budget"; keep the assertion permissive.
        assert!(
            matches!(ghost_res, Err(_) | Ok(Err(_))),
            "ghost host must not appear reachable",
        );

        // Dial the real relay — succeeds.
        let ep2 = client_endpoint().await;
        let real = tilo_test_env::within(
            "real dial timed out",
            RelayClient::connect(
                ep2,
                real_addr,
                user.user_pk,
                a.cert,
                a.sk,
                RevocationList::new(),
            ),
        )
        .await
        .expect("real dial failed");
        drop(real);
    }
}
