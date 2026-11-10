//! A browser that reconnects — a new session, or a restarted host — resumes
//! each workspace from the host-authenticated position it last received: the host
//! replays only what it missed instead of the workspace's retained history.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the host lives as long as the phase that uses it"
)]

#[path = "support/participant.rs"]
mod participant;
mod support;

// Placing the workspace has the host build its governance in an automerge worker.
tilo_automerge_isolate::test_worker!();

use automerge::{AutoCommit, ROOT, transaction::Transactable as _};
use collab_types::DocumentMutation;
use tilo_relay_protocol::workspaces::WorkspacesRequest;
use tilo_session_host::{PeerEvent, PeerHandle};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{enrol, hears, holds, ready, route, say, start_host_at, start_peer, wait_for};

/// How many members' writes landed in `peer`'s replica since the last count.
fn landed(peer: &PeerHandle) -> usize {
    peer.drain()
        .into_iter()
        .filter(|event| {
            matches!(event, PeerEvent::Landed { author: Some(author), .. } if !support::is_host(author))
        })
        .count()
}

/// Wait until `peer` has taken in everything its host wrote it so far. The
/// host writes a position after the deltas it covers, and a page can surface
/// the last delta before it has read that position; a reconnect then would
/// present the position before it and be sent the delta again. The host
/// answers a request on the same stream, after what it wrote before.
async fn settled(peer: &PeerHandle) {
    let (reply, answer) = tokio::sync::oneshot::channel();
    peer.rooms_then(
        WorkspacesRequest::List,
        Box::new(move |result| {
            let _ = reply.send(result);
        }),
    )
    .unwrap();
    tilo_test_env::within("the host to answer the page", answer)
        .await
        .unwrap()
        .unwrap();
}

/// Wait for `peer` to be ready on a fresh session.
async fn resumed(peer: &PeerHandle) {
    wait_for("the new session", || {
        let ready = peer.snapshot().phase == "ready"
            && peer.status().workspaces.values().all(|ready| *ready);
        async move { ready.then_some(()) }
    })
    .await;
}

/// Have `author` publish `marker` into `doc` and return how many writes landed
/// in `peer`'s replica before it: the workspace delivers in commit order, so
/// anything replayed on the new session arrives ahead of the marker.
async fn before_marker(
    peer: &PeerHandle,
    author: &PeerHandle,
    doc: &mut AutoCommit,
    marker: &str,
) -> usize {
    author.publish(route(), &say(doc, marker)).unwrap();
    hears(peer, &route(), marker).await;
    settled(peer).await;
    landed(peer)
        .checked_sub(1)
        .expect("the marker itself landed")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reconnect_resumes_after_the_last_position_instead_of_replaying_history() {
    const HISTORY: usize = 100;
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let author = enrol(&root, "browser", DeviceCapabilities::member());
    let observer = enrol(&root, "phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    // The restarted host answers at the same web and iroh addresses, as a
    // host that kept its place does.
    let host = start_host_at(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let mut laptop = participant::Participant::start(&host.relay, &root).await;
    let listen = host.web.addr;
    let iroh_listen = host.iroh_listen();

    let author = start_peer(
        host.iroh(),
        author.cert,
        host_device.cert.clone(),
        author.key,
    );
    let observer = start_peer(
        host.iroh(),
        observer.cert,
        host_device.cert.clone(),
        observer.key,
    );
    author.join_placed(support::WORKSPACE, "Workspace").unwrap();
    observer
        .join_placed(support::WORKSPACE, "Workspace")
        .unwrap();
    observer.open_route(route()).unwrap();
    ready(&[&author, &observer]).await;

    let mut doc = AutoCommit::new();
    doc.put(ROOT, "seed", vec![1_u8; 4096]).unwrap();
    author
        .publish(route(), &DocumentMutation::Snapshot(doc.save()))
        .unwrap();
    for index in 0..HISTORY {
        doc.put(
            ROOT,
            format!("h{index}"),
            vec![u8::try_from(index % 250).unwrap(); 4096],
        )
        .unwrap();
        author
            .publish(
                route(),
                &DocumentMutation::Incremental(doc.save_incremental()),
            )
            .unwrap();
    }
    support::durable(&author).await;
    holds(&observer, &route(), &doc).await;
    settled(&observer).await;
    assert_eq!(landed(&observer), HISTORY + 1);

    // A new session: nothing retained is replayed.
    settled(&observer).await;
    observer.reconnect().unwrap();
    resumed(&observer).await;
    let replayed = before_marker(&observer, &author, &mut doc, "after").await;
    assert_eq!(replayed, 0, "replayed {replayed} documents");

    // A restarted host honours positions it issued before, and replays what
    // was committed while the observer was away.
    drop(host.web);
    host.relay.shutdown().await;
    let host = start_host_at(&root, &host_device, state.path(), listen, iroh_listen).await;
    laptop.reconnect(&host.relay, &root).await;
    author.reconnect().unwrap();
    ready(&[&author]).await;
    author.publish(route(), &say(&mut doc, "missed")).unwrap();
    let receipt = author.barrier().unwrap();
    wait_for("the missed edit to be durable", || async {
        receipt.try_recv().ok()
    })
    .await
    .unwrap();
    // The observer reconnects to the restarted host on its own; this only
    // cuts its backoff short. A forced reconnect instead could drop a session
    // that already resumed before it read the position covering the edit,
    // and have the edit sent twice.
    observer.retry_now().unwrap();
    resumed(&observer).await;
    let replayed = before_marker(&observer, &author, &mut doc, "back").await;
    assert_eq!(replayed, 1, "only the edit made while away is replayed");
    hears(&observer, &route(), "missed").await;

    // A peer without a position (a fresh page) still gets everything.
    let fresh = enrol(&root, "tablet", DeviceCapabilities::member());
    let fresh = start_peer(host.iroh(), fresh.cert, host_device.cert.clone(), fresh.key);
    fresh.join_placed(support::WORKSPACE, "Workspace").unwrap();
    fresh.open_route(route()).unwrap();
    holds(&fresh, &route(), &doc).await;
    settled(&fresh).await;
    assert_eq!(landed(&fresh), HISTORY + 4);
}
