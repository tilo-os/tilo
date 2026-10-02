//! A browser that reconnects — a new session, or a restarted host — resumes
//! each room from the host-authenticated position it last received: the host
//! replays only what it missed instead of the room's retained history.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the host lives as long as the phase that uses it"
)]

#[path = "support/steward.rs"]
mod steward;
mod support;

use std::sync::atomic::Ordering;
use std::time::Duration;

use collab_types::DocumentMutation;
use tilo_browser_peer::{Host, PeerEvent, PeerHandle, PeerSnapshot};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{Recording, enrol, ready, route, start_host, start_peer, wait_for, workspace};

fn documents(peer: &PeerHandle) -> Vec<DocumentMutation> {
    peer.drain()
        .into_iter()
        .filter_map(|event| match event {
            PeerEvent::Document(document) => document.mutation(),
            _ => None,
        })
        .collect()
}

async fn receive(peer: &PeerHandle, count: usize) -> Vec<DocumentMutation> {
    let mut received = Vec::new();
    wait_for("documents", || {
        received.extend(documents(peer));
        let done = received.len() >= count;
        async move { done.then_some(()) }
    })
    .await;
    received
}

/// Wait for `peer` to be ready on a fresh session and settle; return the
/// documents it received meanwhile.
async fn resumed(peer: &PeerHandle) -> Vec<DocumentMutation> {
    wait_for("the new session", || {
        let ready = PeerSnapshot::from_status(&peer.status()).phase == "ready"
            && peer.status().rooms.values().all(|ready| *ready);
        async move { ready.then_some(()) }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    documents(peer)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reconnect_resumes_after_the_last_position_instead_of_replaying_history() {
    const HISTORY: usize = 100;
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let author = enrol(&root, "browser", DeviceCapabilities::member());
    let observer = enrol(&root, "phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let mut steward = steward::Steward::start(&host, &root).await;
    let listen = host.web.addr;

    let author = start_peer(
        Host::Https(host.https()),
        author.cert,
        host_device.cert.clone(),
        author.key,
    );
    let recording = Recording::new(host.https());
    let traffic = recording.total.clone();
    let observer = start_peer(
        Host::Https(recording),
        observer.cert,
        host_device.cert.clone(),
        observer.key,
    );
    author.join(workspace()).unwrap();
    observer.join(workspace()).unwrap();
    observer.open_route(route()).unwrap();
    ready(&[&author, &observer]).await;

    author
        .publish(route(), &DocumentMutation::Snapshot(vec![1; 4096]))
        .unwrap();
    for index in 0..HISTORY {
        author
            .publish(
                route(),
                &DocumentMutation::Incremental(vec![u8::try_from(index % 250).unwrap(); 4096]),
            )
            .unwrap();
    }
    assert_eq!(receive(&observer, HISTORY + 1).await.len(), HISTORY + 1);

    // A new session: nothing retained is replayed.
    let before = traffic.load(Ordering::Relaxed);
    observer.reconnect().unwrap();
    let replayed = resumed(&observer).await;
    let resume_bytes = traffic.load(Ordering::Relaxed) - before;
    assert!(replayed.is_empty(), "replayed {} documents", replayed.len());
    author
        .publish(route(), &DocumentMutation::Incremental(b"after".to_vec()))
        .unwrap();
    assert_eq!(
        receive(&observer, 1).await,
        vec![DocumentMutation::Incremental(b"after".to_vec())]
    );

    // A restarted host honours positions it issued before, and replays what
    // was committed while the observer was away.
    drop(host.web);
    host.relay.shutdown().await;
    let host = start_host(&root, &host_device, state.path(), listen).await;
    steward.reconnect(&host, &root).await;
    author.reconnect().unwrap();
    ready(&[&author]).await;
    author
        .publish(route(), &DocumentMutation::Incremental(b"missed".to_vec()))
        .unwrap();
    let receipt = author.barrier().unwrap();
    wait_for("the missed edit to be durable", || async {
        receipt.try_recv().ok()
    })
    .await
    .unwrap();
    observer.reconnect().unwrap();
    let replayed = resumed(&observer).await;
    assert_eq!(
        replayed,
        vec![DocumentMutation::Incremental(b"missed".to_vec())],
        "only the edit made while away is replayed"
    );

    // A peer without a position (a fresh page) still gets everything.
    let fresh = enrol(&root, "tablet", DeviceCapabilities::member());
    let recording = Recording::new(host.https());
    let full_replay = recording.total.clone();
    let fresh = start_peer(
        Host::Https(recording),
        fresh.cert,
        host_device.cert.clone(),
        fresh.key,
    );
    fresh.join(workspace()).unwrap();
    fresh.open_route(route()).unwrap();
    assert_eq!(receive(&fresh, HISTORY + 3).await.len(), HISTORY + 3);
    eprintln!(
        "reconnect: {resume_bytes} bytes of /events resuming, {} bytes replaying the history",
        full_replay.load(Ordering::Relaxed)
    );
}
