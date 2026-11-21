//! `SessionHealth` recovery against a real host: refreshing the shared state
//! re-requests the workspace's checkpoints (also after the host restarted), and
//! failures the page reports for its store, surfaces and assets are scoped
//! and cleared.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]

use tilo_session_client::{Cmd, Event, FailureKind, LocalState, Outbox, TransportPhase};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{
    HostSettings, enrol, holds, ready, restart_host_relayed, route, say, start_host,
    start_host_serving, start_peer, wait_for, workspace,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refresh_asks_the_restarted_host_for_the_workspace_again() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    // The restarted host is found at the server the page knew, whichever port
    // it came back on.
    let host = start_host_serving(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        HostSettings {
            relayed: true,
            ..HostSettings::default()
        },
    )
    .await;
    let peer = start_peer(
        host.iroh(),
        browser.cert.clone(),
        host_device.cert.clone(),
        browser.key.clone(),
    );
    peer.join_placed(crate::support::WORKSPACE, "Workspace")
        .unwrap();
    ready(&[&peer]).await;
    let mut tree = automerge::AutoCommit::new();
    peer.write(route(), &say(&mut tree, "desktop tree"))
        .unwrap();
    let receipt = peer.barrier().unwrap();
    wait_for("the snapshot to be durable", || async {
        receipt.try_recv().ok()
    })
    .await
    .unwrap();

    drop(host.web);
    host.server.shutdown().await;
    let host = restart_host_relayed(&root, &host_device, state.path()).await;
    // A reconnect resumes: the page already holds what it published.
    peer.send(Cmd::Reconnect).unwrap();
    ready(&[&peer]).await;

    let before = peer.status().generation;
    peer.send(Cmd::Refresh).unwrap();
    wait_for("the refreshed session", || {
        let refreshed = peer.status().generation > before;
        async move { refreshed.then_some(()) }
    })
    .await;
    ready(&[&peer]).await;
    holds(&peer, &route(), &tree).await;
    let status = peer.status();
    assert!(status.generation > before);
    assert!(status.workspaces.values().all(|ready| *ready));
    assert!(
        status
            .failures
            .iter()
            .all(|failure| failure.kind != FailureKind::Admission)
    );
    peer.close();
    host.server.shutdown().await;
}

/// Back online after a long outage, a session waiting out its backoff
/// reconnects when told the network is back, not up to half a minute
/// later; a connected session is left as it is.
#[test]
fn a_session_told_the_network_is_back_reconnects_now_instead_of_after_its_backoff() {
    // The backoff waits on the clock `tilo_clock` lends, and this test holds that clock an hour
    // ahead and the wall clock still: the wait cannot end by itself, so the dial after the retry
    // is the session's answer to it, not a timer's, and the deadline its status names stays in
    // the future for the assertion below.
    let held = tilo_clock::now() + std::time::Duration::from_secs(3600);
    let wall = tilo_clock::wall();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tilo_clock::testing::with_monotonic_time(held, || {
        tilo_clock::testing::with_wall(wall, || {
            runtime.block_on(async {
                let (root, _) = UserIdentity::generate("Ada", 1);
                let host_device = enrol(&root, "home-server", DeviceCapabilities::member());
                let browser = enrol(&root, "browser", DeviceCapabilities::member());
                let state = tempfile::tempdir().unwrap();
                // Back online, the page reaches the host through the server it knew,
                // whichever port the host came back on.
                let host = start_host_serving(
                    &root,
                    &host_device,
                    state.path(),
                    "127.0.0.1:0".parse().unwrap(),
                    HostSettings {
                        relayed: true,
                        ..HostSettings::default()
                    },
                )
                .await;
                let peer = start_peer(
                    host.iroh(),
                    browser.cert.clone(),
                    host_device.cert.clone(),
                    browser.key.clone(),
                );
                let events = peer.events();
                peer.join_placed(crate::support::WORKSPACE, "Workspace")
                    .unwrap();
                // Wait for the join's own `Ready` event, not a ready phase: the join
                // is a command the session takes up later, so the phase can read
                // ready (connected, no workspace waiting) before the workspace is even asked
                // for, and the session marks the workspace ready before it emits the
                // event. Either way that `Ready` would land after the drain below.
                wait_for("the workspace's Ready", || async {
                    events
                        .drain()
                        .iter()
                        .any(|event| {
                            matches!(event, Event::HostReady { ws: workspace_id, .. } if *workspace_id == workspace())
                        })
                        .then_some(())
                })
                .await;
                ready(&[&peer]).await;
                // Any status change bumps the generation (each envelope's contact
                // time does); a reconnect is what subscribes the workspace anew.
                let _ = events.drain();
                peer.send(Cmd::Retry).unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                assert!(
                    !events
                        .drain()
                        .iter()
                        .any(|event| matches!(event, Event::HostReady { .. })),
                    "a connected session is left as it is"
                );

                drop(host.web);
                host.server.shutdown().await;
                // The session waits out a backoff before its next attempt, and its
                // status names when that is.
                let due = wait_for("the session to wait out a backoff", || async {
                    let status = peer.status();
                    (status.phase == TransportPhase::Offline)
                        .then_some(status.next_attempt_at_ms)
                        .flatten()
                })
                .await;
                peer.send(Cmd::Retry).unwrap();
                // The session dials at once, not when the backoff runs out.
                wait_for("the session to dial again", || async {
                    (peer.status().phase == TransportPhase::Reconnecting).then_some(())
                })
                .await;
                assert!(
                    tilo_clock::unix_now_ms() < due,
                    "back before the backoff ran out"
                );
                let host = restart_host_relayed(&root, &host_device, state.path()).await;
                ready(&[&peer]).await;
                peer.close();
                host.server.shutdown().await;
            });
        });
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn page_failures_are_scoped_offer_their_recovery_and_clear() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let peer = start_peer(
        host.iroh(),
        browser.cert.clone(),
        host_device.cert.clone(),
        browser.key.clone(),
    );
    // Page-owned failures only; the session may be connecting meanwhile.
    let page_failures = || {
        let health = serde_json::to_value(peer.snapshot().health).unwrap();
        health["failures"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|failure| failure["scope"] != "transport")
            .cloned()
            .collect::<Vec<_>>()
    };

    peer.set_local_state(route(), LocalState::Failed);
    peer.set_local_state(route(), LocalState::Failed);
    peer.report_failure(
        FailureKind::Surface("tilo-desktop-1".into()),
        "gpu-device-lost",
        "lost",
    )
    .unwrap();
    peer.report_failure(
        FailureKind::Asset {
            surface: "tilo-desktop-1".into(),
            asset: "4".into(),
        },
        "asset-unavailable",
        "missing",
    )
    .unwrap();
    assert!(
        peer.report_failure(FailureKind::Session, "gpu-device-lost", "x")
            .is_err()
    );
    assert!(
        peer.report_failure(FailureKind::Storage, "certificate-expired", "x")
            .is_err()
    );
    let recovery: Vec<_> = page_failures()
        .iter()
        .map(|failure| failure["recovery"][0].clone())
        .collect();
    assert_eq!(
        recovery,
        [
            serde_json::json!({"kind": "retry-saving"}),
            serde_json::json!({"kind": "retry-desktop", "surfaceId": "tilo-desktop-1"}),
            serde_json::json!({"kind": "retry-asset", "surfaceId": "tilo-desktop-1", "assetId": "4"}),
        ],
        "one storage failure however often a write fails"
    );

    peer.set_local_state(route(), LocalState::Durable);
    peer.clear_surface("tilo-desktop-1");
    assert!(page_failures().is_empty());
    peer.report_failure(FailureKind::Storage, "storage-quota", "full")
        .unwrap();
    peer.clear_failure(&FailureKind::Storage);
    assert!(page_failures().is_empty());
    peer.close();
    host.server.shutdown().await;
}
