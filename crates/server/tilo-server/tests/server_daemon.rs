//! Two-peer fan-out through a single server.
//!
//! Spins up:
//! - one server bound to workspace 42, membership = {A, B}
//! - two peer devices A + B, each dials the server
//!
//! Both subscribe to 42; A publishes a Delta envelope. Assert B
//! receives the same delta bytes via the server's fan-out.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use std::time::Duration;

use tilo_server::{CoeditPayload, Envelope, ServerClient, ServerCoeditSession, ServerConfig};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_server};
use tilo_server::WorkspaceId;

const WORKSPACE: WorkspaceId = WorkspaceId::from_u128(9);
const COEDIT_WORKSPACE: WorkspaceId = WorkspaceId::from_u128(19);
const SECOND_WORKSPACE: WorkspaceId = WorkspaceId::from_u128(42);

/// Upper bound for a frame the server owes a client. Live fan-out waits for the
/// delta's durable commit (pending file plus directory fsyncs) and the server's
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
        let server_dev = user.enrol("home-server", DeviceCapabilities::member());
        let a = user.enrol("laptop", DeviceCapabilities::primary());
        let b = user.enrol("new-phone", DeviceCapabilities::member());
        let state = tempfile::tempdir().unwrap();
        let server = crate::common::start_server(
            ServerConfig::loopback(state.path().to_path_buf()),
            tilo_server::daemon::ServerIdentity {
                user_public_key: user.user_pk,
                device_signing_key: server_dev.sk,
                device_cert: server_dev.cert,
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        crate::common::hold_workspace(&server, COEDIT_WORKSPACE, &[&a.cert]).await;
        let addr = server.node_addr();
        let a_key = a.device_pubkey();
        let a_client = ServerClient::connect(
            client_endpoint().await,
            addr.clone(),
            user.user_pk,
            a.cert,
            a.sk,
            RevocationList::new(),
        )
        .await
        .unwrap();
        let a_session = ServerCoeditSession::join(a_client, COEDIT_WORKSPACE, a_key).unwrap();
        tilo_test_env::within("a message on a_session", async {
            loop {
                match a_session.recv().await {
                    Some(tilo_server::Inbound::Ready { .. }) => break,
                    Some(_) => {}
                    None => panic!("A's session ended before it was ready"),
                }
            }
        })
        .await;
        let metrics = server.handle().metrics;
        a_session
            .publish_checkpoint([7u8; 32], b"baseline-before-phone".to_vec())
            .unwrap();
        // A and B use separate connections, so B's Subscribe can reach the
        // server before A's checkpoint does. Wait for the server to commit it.
        tokio::time::timeout(HANG, async {
            while metrics.snapshot().deltas_received == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("server committed A's checkpoint");

        // B was not a workspace member when A published, so no per-device
        // pending queue could have existed. It enters now, and its first
        // subscribe must use retained state instead.
        crate::common::hold_workspace(&server, COEDIT_WORKSPACE, &[&b.cert]).await;
        let b_client = ServerClient::connect(
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
            .send(Envelope::Subscribe {
                workspace_id: COEDIT_WORKSPACE,
            })
            .unwrap();
        // The workspace's own state comes first; the checkpoint it retained
        // for B follows it.
        tilo_test_env::within("B's replay of the retained checkpoint", async {
            loop {
                match b_client.recv().await {
                    Some(Envelope::Delta { delta, .. }) => {
                        if matches!(
                            CoeditPayload::decode(&delta),
                            Some(CoeditPayload::Checkpoint { body, .. })
                                if body == b"baseline-before-phone"
                        ) {
                            break;
                        }
                    }
                    Some(Envelope::Ready { .. }) => {
                        panic!("B was ready without its retained checkpoint")
                    }
                    Some(_) => {}
                    None => panic!("B's session ended before its replay"),
                }
            }
        })
        .await;
        assert!(matches!(
            crate::common::next(&b_client, |env| matches!(env, Envelope::Ready { .. })).await,
            Envelope::Ready {
                workspace_id: COEDIT_WORKSPACE,
                pending_deltas,
                ..
            } if pending_deltas >= 1
        ));
    }
    // Phase `home_host_remembers_authenticated_devices_for_offline_delivery`.
    {
        let user = TestUser::fresh("Alice");
        let server_dev = user.enrol("home-server", DeviceCapabilities::member());
        let a = user.enrol("laptop", DeviceCapabilities::primary());
        let b = user.enrol("phone", DeviceCapabilities::member());
        let (a_key, b_key) = (a.device_pubkey(), b.device_pubkey());
        let state = tempfile::tempdir().unwrap();
        let server = crate::common::start_server(
            ServerConfig::loopback(state.path().to_path_buf()),
            tilo_server::daemon::ServerIdentity {
                user_public_key: user.user_pk,
                device_signing_key: server_dev.sk,
                device_cert: server_dev.cert,
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        crate::common::hold_workspace(&server, WORKSPACE, &[&a.cert, &b.cert]).await;
        let addr = server.node_addr();
        let a_client = ServerClient::connect(
            client_endpoint().await,
            addr.clone(),
            user.user_pk,
            a.cert,
            a.sk,
            RevocationList::new(),
        )
        .await
        .unwrap();
        let b_client = ServerClient::connect(
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
                .send(Envelope::Subscribe {
                    workspace_id: WORKSPACE,
                })
                .unwrap();
            crate::common::next(client, |env| matches!(env, Envelope::Ready { .. })).await;
        }
        drop(b_client);
        let pending_dir = state
            .path()
            .join(format!("workspaces/{}/pending", WorkspaceId::from_u128(9)))
            .join(hex::encode(b.device_pubkey()));
        publish_until_queued(&a_client, a_key, &pending_dir).await;
        assert!(
            pending_dir
                .read_dir()
                .is_ok_and(|mut files| files.next().is_some()),
            "server noticed the closed session and queued an offline change"
        );

        let b_again = ServerClient::connect(
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
            .send(Envelope::Subscribe {
                workspace_id: WORKSPACE,
            })
            .unwrap();
        assert!(matches!(
            crate::common::next(&b_again, |env| matches!(
                env,
                Envelope::Delta { delta, .. } if delta == b"offline-change"
            ))
            .await,
            Envelope::Delta { .. }
        ));
        wait_for_ready_with_pending(&b_again, &server.handle().metrics).await;

        // Both devices are on the workspace's signed member list, the one
        // record that admits them, so a restart admits them from it alone.
        let members = crate::common::member_devices(&server, WORKSPACE).await;
        assert!(
            members.contains(&a_key) && members.contains(&b_key),
            "both devices persist as members"
        );
    }
    // Phase `two_peers_fan_out_through_relay`.
    {
        let user = TestUser::fresh("Alice");
        let server_dev = user.enrol("home-server", DeviceCapabilities::member());
        let a = user.enrol("laptop", DeviceCapabilities::primary());
        let b = user.enrol("phone", DeviceCapabilities::member());

        let server = Box::pin(spawn_server(
            &user,
            server_dev,
            &[SECOND_WORKSPACE],
            &[&a.cert, &b.cert],
        ))
        .await;
        let server_addr = server.server.node_addr();

        let ep_a = client_endpoint().await;
        let a_client = ServerClient::connect(
            ep_a,
            server_addr.clone(),
            user.user_pk,
            a.cert.clone(),
            a.sk.clone(),
            RevocationList::new(),
        )
        .await
        .expect("A connect");

        let ep_b = client_endpoint().await;
        let b_client = ServerClient::connect(
            ep_b,
            server_addr.clone(),
            user.user_pk,
            b.cert.clone(),
            b.sk.clone(),
            RevocationList::new(),
        )
        .await
        .expect("B connect");

        a_client
            .send(Envelope::Subscribe {
                workspace_id: SECOND_WORKSPACE,
            })
            .unwrap();
        b_client
            .send(Envelope::Subscribe {
                workspace_id: SECOND_WORKSPACE,
            })
            .unwrap();

        // Drain the Ready acks so we don't confuse them for the delta
        // later.
        for cli in [&a_client, &b_client] {
            let env = crate::common::next(cli, |env| matches!(env, Envelope::Ready { .. })).await;
            match env {
                Envelope::Ready {
                    workspace_id: SECOND_WORKSPACE,
                    ..
                } => {}
                other => panic!("expected Ready(42, 0), got {other:?}"),
            }
        }

        let payload = b"the-crdt-delta".to_vec();
        let delta_env = Envelope::Delta {
            workspace_id: SECOND_WORKSPACE,
            author_device_public_key: a.device_pubkey(),
            authored_at_ns: 1000,
            delta: payload.clone(),
            signature: [1u8; 64],
        };
        a_client.send(delta_env.clone()).unwrap();

        match crate::common::next(&b_client, |env| {
            matches!(
                env,
                Envelope::Delta { delta, .. } if *delta == payload
            )
        })
        .await
        {
            Envelope::Delta {
                workspace_id,
                delta,
                ..
            } => {
                assert_eq!(workspace_id, SECOND_WORKSPACE);
                assert_eq!(delta, payload);
            }
            other => panic!("expected Delta, got {other:?}"),
        }

        // A does NOT receive its own delta back (author-exclusion). We wait a
        // short window; anything but the person's own workspace list, which the
        // server sends every session of theirs when it changes, is a bug.
        assert!(
            crate::common::quiet(&a_client, Duration::from_millis(200)).await,
            "A should not receive its own delta"
        );

        let snap = server.server.handle().metrics.snapshot();
        assert!(snap.conns_authed >= 2, "at least both peers authed");
    }
}

/// Publish synthetic deltas from `a_client` until `pending_dir` shows a
/// queued file (the server noticed the closed session), or the hang bound
/// runs out — the caller's own `assert!` reports the timeout.
async fn publish_until_queued(
    a_client: &ServerClient,
    a_key: [u8; 32],
    pending_dir: &std::path::Path,
) {
    let _ = tokio::time::timeout(HANG, async {
        for authored_at_ns in 1.. {
            a_client
                .send(Envelope::Delta {
                    workspace_id: WORKSPACE,
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
async fn wait_for_ready_with_pending(
    b_again: &ServerClient,
    metrics: &std::sync::Arc<tilo_server::metrics::Metrics>,
) {
    loop {
        match tokio::time::timeout(tilo_test_env::hang_bound(), b_again.recv()).await {
            Ok(Some(Envelope::Ready {
                workspace_id: WORKSPACE,
                pending_deltas,
                ..
            })) => {
                assert!(pending_deltas >= 1);
                break;
            }
            Ok(Some(Envelope::Delta { .. })) => {}
            Ok(None) => {
                let snapshot = metrics.snapshot();
                panic!(
                    "the server closed the reconnected session before Ready: {} sessions ended with an error, {} of them for backpressure",
                    snapshot.subscriber_dropped, snapshot.subscriber_dropped_backpressure
                );
            }
            other => panic!("expected queued delta or Ready, got {other:?}"),
        }
    }
}
