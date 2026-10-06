//! `SessionHealth` recovery against a real host: refreshing the shared state
//! re-requests the room's checkpoints (also after the host restarted), and
//! failures the page reports for its store, surfaces and assets are scoped
//! and cleared.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "support/steward.rs"]
mod steward;
mod support;

use collab_types::DocumentMutation;
use tilo_browser_peer::{FailureSubject, Host, LocalState, PeerSnapshot};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{enrol, next_document, ready, route, start_host, start_peer, wait_for, workspace};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refresh_asks_the_restarted_host_for_the_room_again() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _steward = steward::Steward::start(&host.relay, &root).await;
    let listen = host.web.addr;
    let peer = start_peer(
        Host::Https(host.https()),
        browser.cert.clone(),
        host_device.cert.clone(),
        browser.key.clone(),
    );
    peer.join(workspace()).unwrap();
    ready(&[&peer]).await;
    let tree = DocumentMutation::Snapshot(b"desktop tree".to_vec());
    peer.publish(route(), &tree).unwrap();
    let receipt = peer.barrier().unwrap();
    wait_for("the snapshot to be durable", || async {
        receipt.try_recv().ok()
    })
    .await
    .unwrap();

    drop(host.web);
    host.relay.shutdown().await;
    let host = start_host(&root, &host_device, state.path(), listen).await;
    // A reconnect resumes: the page already holds what it published.
    peer.reconnect().unwrap();
    let mut events = Vec::new();
    ready(&[&peer]).await;
    events.clear();
    let _ = peer.drain();

    let before = peer.status().generation;
    peer.refresh().unwrap();
    assert_eq!(
        next_document(&peer, &mut events).await,
        tree,
        "the host sends the room's checkpoint again"
    );
    ready(&[&peer]).await;
    let status = peer.status();
    assert!(status.generation > before);
    assert!(status.rooms.values().all(|ready| *ready));
    assert!(
        status
            .failures
            .iter()
            .all(|failure| failure.subject != FailureSubject::Admission)
    );
    peer.close();
    host.relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn page_failures_are_scoped_offer_their_recovery_and_clear() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
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
        Host::Https(host.https()),
        browser.cert.clone(),
        host_device.cert.clone(),
        browser.key.clone(),
    );
    // Page-owned failures only; the session may be connecting meanwhile.
    let page_failures = || {
        let health =
            serde_json::to_value(PeerSnapshot::from_status(&peer.status()).health).unwrap();
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
        FailureSubject::Surface("tilo-desktop-1".into()),
        "gpu-device-lost",
        "lost",
    )
    .unwrap();
    peer.report_failure(
        FailureSubject::Asset {
            surface: "tilo-desktop-1".into(),
            asset: "4".into(),
        },
        "asset-unavailable",
        "missing",
    )
    .unwrap();
    assert!(
        peer.report_failure(FailureSubject::Session, "gpu-device-lost", "x")
            .is_err()
    );
    assert!(
        peer.report_failure(FailureSubject::Storage, "certificate-expired", "x")
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
    peer.report_failure(FailureSubject::Storage, "storage-quota", "full")
        .unwrap();
    peer.clear_failure(&FailureSubject::Storage);
    assert!(page_failures().is_empty());
    peer.close();
    host.relay.shutdown().await;
}
