//! Two-peer fan-out through a single relay.
//!
//! Spins up:
//! - one relay bound to workspace 42, membership = {A, B}
//! - two peer devices A + B, each dials the relay
//!
//! Both subscribe to 42; A publishes a Delta envelope. Assert B
//! receives the same delta bytes via the relay's fan-out.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use std::time::Duration;

use tilo_relay::{CoeditPayload, Envelope, Relay, RelayClient, RelayCoeditSession, RelayConfig};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_relay};

/// Upper bound for a frame the relay owes a client. Live fan-out waits for the
/// delta's durable commit (pending file plus directory fsyncs) and the relay's
/// blocking-worker pool, so a disk-contended CI host can take seconds; this
/// only bounds a hang.
const HANG: Duration = Duration::from_secs(30);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn relay_daemon_bootstraps_remembers_and_fans_out() {
    // Phase `first_time_home_device_bootstraps_from_retained_checkpoint`.
    {
        let user = TestUser::fresh("Alice");
        let relay_dev = user.enrol("home-server", DeviceCapabilities::host());
        let a = user.enrol("laptop", DeviceCapabilities::primary());
        let b = user.enrol("new-phone", DeviceCapabilities::member());
        let state = tempfile::tempdir().unwrap();
        let relay = Relay::start(
            RelayConfig {
                state_root: state.path().to_path_buf(),
                ..RelayConfig::default()
            },
            tilo_relay::daemon::RelayIdentity {
                user_public_key: user.user_pk,
                device_signing_key: relay_dev.sk,
                device_cert: relay_dev.cert,
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        let addr = relay.node_addr();
        let a_key = a.device_pubkey();
        let a_client = RelayClient::connect(
            client_endpoint().await,
            addr.clone(),
            user.user_pk,
            a.cert,
            a.sk,
            RevocationList::new(),
        )
        .await
        .unwrap();
        let a_session = RelayCoeditSession::join(a_client, 19, a_key).unwrap();
        assert!(matches!(
            tilo_test_env::within("a message on a_session", a_session.recv()).await,
            Some(tilo_relay::Inbound::Ready { pending: 0 })
        ));
        let metrics = relay.handle().metrics;
        a_session
            .publish_checkpoint([7u8; 32], b"baseline-before-phone".to_vec())
            .unwrap();
        // A and B use separate connections, so B's Subscribe can reach the
        // relay before A's checkpoint does. Wait for the relay to commit it.
        tokio::time::timeout(HANG, async {
            while metrics.snapshot().deltas_received == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("relay committed A's checkpoint");

        // B was not a workspace member when A published, so no per-device
        // pending queue could have existed. Its first subscribe must use retained
        // state instead.
        let b_client = RelayClient::connect(
            client_endpoint().await,
            addr,
            user.user_pk,
            b.cert,
            b.sk,
            RevocationList::new(),
        )
        .await
        .unwrap();
        b_client
            .send(Envelope::Subscribe { workspace_id: 19 })
            .unwrap();
        let received = tilo_test_env::within("a message on b_client", b_client.recv())
            .await
            .unwrap();
        let Envelope::Delta { delta, .. } = received else {
            panic!("expected retained checkpoint, got {received:?}");
        };
        assert!(matches!(
            CoeditPayload::decode(&delta),
            Some(CoeditPayload::Checkpoint { body, .. }) if body == b"baseline-before-phone"
        ));
        assert!(matches!(
            tilo_test_env::within("a message on b_client", b_client.recv()).await,
            Some(Envelope::Ready {
                workspace_id: 19,
                pending_deltas: 1
            })
        ));
    }
    // Phase `home_host_remembers_authenticated_devices_for_offline_delivery`.
    {
        let user = TestUser::fresh("Alice");
        let relay_dev = user.enrol("home-server", DeviceCapabilities::host());
        let a = user.enrol("laptop", DeviceCapabilities::primary());
        let b = user.enrol("phone", DeviceCapabilities::member());
        let a_key = a.device_pubkey();
        let state = tempfile::tempdir().unwrap();
        let relay = Relay::start(
            RelayConfig {
                state_root: state.path().to_path_buf(),
                ..RelayConfig::default()
            },
            tilo_relay::daemon::RelayIdentity {
                user_public_key: user.user_pk,
                device_signing_key: relay_dev.sk,
                device_cert: relay_dev.cert,
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        let addr = relay.node_addr();
        let a_client = RelayClient::connect(
            client_endpoint().await,
            addr.clone(),
            user.user_pk,
            a.cert,
            a.sk,
            RevocationList::new(),
        )
        .await
        .unwrap();
        let b_client = RelayClient::connect(
            client_endpoint().await,
            addr.clone(),
            user.user_pk,
            b.cert.clone(),
            b.sk.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap();
        for client in [&a_client, &b_client] {
            client
                .send(Envelope::Subscribe { workspace_id: 9 })
                .unwrap();
            let _ = tilo_test_env::within("a message on client", client.recv()).await;
        }
        drop(b_client);
        let pending_dir = state
            .path()
            .join("workspaces/9/pending")
            .join(hex::encode(b.device_pubkey()));
        publish_until_queued(&a_client, a_key, &pending_dir).await;
        assert!(
            pending_dir
                .read_dir()
                .is_ok_and(|mut files| files.next().is_some()),
            "relay noticed the closed session and queued an offline change"
        );

        let b_again = RelayClient::connect(
            client_endpoint().await,
            addr,
            user.user_pk,
            b.cert,
            b.sk,
            RevocationList::new(),
        )
        .await
        .unwrap();
        b_again
            .send(Envelope::Subscribe { workspace_id: 9 })
            .unwrap();
        assert!(matches!(
        tilo_test_env::within("a message on b_again", b_again.recv()).await,
        Some(Envelope::Delta { ref delta, .. }) if delta == b"offline-change"
            ));
        wait_for_ready_with_pending(&b_again).await;

        let members: Vec<String> = serde_json::from_slice(
            &std::fs::read(state.path().join("workspaces/9/membership.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            members.len(),
            2,
            "both same-owner devices persist as members"
        );
    }
    // Phase `two_peers_fan_out_through_relay`.
    {
        let user = TestUser::fresh("Alice");
        let relay_dev = user.enrol("home-server", DeviceCapabilities::host());
        let a = user.enrol("laptop", DeviceCapabilities::primary());
        let b = user.enrol("phone", DeviceCapabilities::member());

        let relay = Box::pin(spawn_relay(
            &user,
            relay_dev,
            &[42],
            &[a.device_pubkey(), b.device_pubkey()],
        ))
        .await;
        let relay_addr = relay.relay.node_addr();

        // Peer A dials the relay.
        let ep_a = client_endpoint().await;
        let a_client = RelayClient::connect(
            ep_a,
            relay_addr.clone(),
            user.user_pk,
            a.cert.clone(),
            a.sk.clone(),
            RevocationList::new(),
        )
        .await
        .expect("A connect");

        // Peer B dials the relay.
        let ep_b = client_endpoint().await;
        let b_client = RelayClient::connect(
            ep_b,
            relay_addr.clone(),
            user.user_pk,
            b.cert.clone(),
            b.sk.clone(),
            RevocationList::new(),
        )
        .await
        .expect("B connect");

        a_client
            .send(Envelope::Subscribe { workspace_id: 42 })
            .unwrap();
        b_client
            .send(Envelope::Subscribe { workspace_id: 42 })
            .unwrap();

        // Drain the Ready acks so we don't confuse them for the delta
        // later.
        for cli in [&a_client, &b_client] {
            let env = tilo_test_env::within("Ready timeout", cli.recv())
                .await
                .expect("Ready channel closed");
            match env {
                Envelope::Ready {
                    workspace_id: 42,
                    pending_deltas: 0,
                } => {}
                other => panic!("expected Ready(42, 0), got {other:?}"),
            }
        }

        // A publishes a delta. `delta` bytes are opaque to the relay.
        let payload = b"the-crdt-delta".to_vec();
        let delta_env = Envelope::Delta {
            workspace_id: 42,
            author_device_public_key: a.device_pubkey(),
            authored_at_ns: 1000,
            delta: payload.clone(),
            signature: [1u8; 64],
        };
        a_client.send(delta_env.clone()).unwrap();

        // B receives it.
        let received = tilo_test_env::within("delta timeout", b_client.recv())
            .await
            .expect("delta channel closed");
        match received {
            Envelope::Delta {
                workspace_id,
                delta,
                ..
            } => {
                assert_eq!(workspace_id, 42);
                assert_eq!(delta, payload);
            }
            other => panic!("expected Delta, got {other:?}"),
        }

        // A does NOT receive its own delta back (author-exclusion).
        // We wait a short window; anything within it is a bug.
        let ghost = tokio::time::timeout(Duration::from_millis(200), a_client.recv()).await;
        assert!(ghost.is_err(), "A should not receive its own delta");

        // Metrics reflect activity.
        let snap = relay.relay.handle().metrics.snapshot();
        assert!(snap.conns_authed >= 2, "at least both peers authed");
    }
}

/// Publish synthetic deltas from `a_client` until `pending_dir` shows a
/// queued file (the relay noticed the closed session), or the hang bound
/// runs out — the caller's own `assert!` reports the timeout.
async fn publish_until_queued(
    a_client: &RelayClient,
    a_key: [u8; 32],
    pending_dir: &std::path::Path,
) {
    let _ = tokio::time::timeout(HANG, async {
        for authored_at_ns in 1.. {
            a_client
                .send(Envelope::Delta {
                    workspace_id: 9,
                    author_device_public_key: a_key,
                    authored_at_ns,
                    delta: b"offline-change".to_vec(),
                    signature: [0; 64],
                })
                .unwrap();
            if pending_dir
                .read_dir()
                .is_ok_and(|mut files| files.next().is_some())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
}

/// Drain replayed `Delta`s until the `Ready` marker reports at least one
/// pending delta was queued for this reconnect.
async fn wait_for_ready_with_pending(b_again: &RelayClient) {
    loop {
        match tokio::time::timeout(tilo_test_env::hang_bound(), b_again.recv()).await {
            Ok(Some(Envelope::Ready {
                workspace_id: 9,
                pending_deltas,
            })) => {
                assert!(pending_deltas >= 1);
                break;
            }
            Ok(Some(Envelope::Delta { .. })) => {}
            other => panic!("expected queued delta or Ready, got {other:?}"),
        }
    }
}
