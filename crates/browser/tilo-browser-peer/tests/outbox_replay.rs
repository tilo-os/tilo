//! Edits a browser makes over HTTPS while its host is down stay in the
//! persisted outbox and are replayed, in order, once the host is back on the
//! same address; a peer that joins afterwards receives them from the host.

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

mod support;

use std::sync::Arc;

use collab_types::DocumentMutation;
use tilo_browser_peer::{
    Host, MemoryPeerStore, PeerConfig, PeerHandle, PeerSnapshot, PeerStore, TransportPhase,
};
use tilo_user_identity::{DeviceCapabilities, RevocationList, UserIdentity};

use support::{enrol, next_document, ready, route, start_host, start_peer, wait_for, workspace};

/// An edit published before the page knows whether its room is governed
/// waits for the room's replay; a barrier called meanwhile waits with it
/// until the host has made it durable, however late the replay arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_barrier_covers_an_edit_held_for_the_rooms_replay() {
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
    // Before the page joins, no replay has said whether the room is governed.
    peer.publish(
        route(),
        &DocumentMutation::Snapshot(b"written before the replay".to_vec()),
    )
    .unwrap();
    let receipt = peer.barrier().unwrap();
    peer.join(workspace()).unwrap();
    wait_for("the barrier", || async { receipt.try_recv().ok() })
        .await
        .unwrap();
    let durable = peer
        .status()
        .routes
        .get(&route())
        .map_or(0, |route| route.durable_operations);
    assert_eq!(
        durable, 1,
        "a barrier completes only once what was published before it is durable"
    );
    peer.close();
    host.relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbox_survives_a_host_restart_and_is_replayed() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let observer = enrol(&root, "phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let listen = host.web.addr;
    let https = host.https();

    let store = Arc::new(MemoryPeerStore::default());
    let peer = PeerHandle::start(
        PeerConfig {
            host: Host::Https(https),
            cert: browser.cert.clone(),
            host_cert: host_device.cert.clone(),
            revocations: RevocationList::new(),
        },
        Arc::new(browser.key.clone()),
        store.clone(),
    );
    peer.join(workspace()).unwrap();
    ready(&[&peer]).await;

    drop(host.web);
    host.relay.shutdown().await;
    let offline = [
        DocumentMutation::Snapshot(b"snapshot while offline".to_vec()),
        DocumentMutation::Incremental(b"typed while offline".to_vec()),
    ];
    for mutation in &offline {
        peer.publish(route(), mutation).unwrap();
    }
    // The outbox persists an edit before the link to the stopped host has
    // reported its end, so wait for the peer to see the host gone as well.
    wait_for("both edits in the persisted outbox, the host gone", || {
        let queued = store.load().is_some_and(|record| record.outbox.len() == 2)
            && peer.status().phase != TransportPhase::Connected;
        async move { queued.then_some(()) }
    })
    .await;
    assert_ne!(peer.status().phase, TransportPhase::Connected);
    assert_eq!(
        PeerSnapshot::from_status(&peer.status()).synchronization,
        "pending"
    );

    let host = start_host(&root, &host_device, state.path(), listen).await;
    peer.reconnect().unwrap();
    let receipt = peer.barrier().unwrap();
    wait_for("the replayed commits to be durable", || async {
        receipt.try_recv().ok()
    })
    .await
    .unwrap();
    assert!(store.load().unwrap().outbox.is_empty());

    let observer_peer = start_peer(
        host.iroh(),
        observer.cert.clone(),
        host_device.cert.clone(),
        observer.key.clone(),
    );
    observer_peer.join(workspace()).unwrap();
    let mut events = Vec::new();
    for mutation in &offline {
        assert_eq!(&next_document(&observer_peer, &mut events).await, mutation);
    }
    peer.close();
    observer_peer.close();
    host.relay.shutdown().await;
}
