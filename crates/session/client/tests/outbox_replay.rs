//! Edits a browser makes over HTTPS while its host is down stay in its
//! replica's persisted custody and are replayed once the host is back on the
//! same address; a peer that joins afterwards receives them from the host.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "support/participant.rs"]
mod participant;
mod support;

// Placing the workspace has the host build its governance in an automerge worker.
tilo_automerge_isolate::test_worker!();

use std::sync::Arc;
use tilo_session_client::{Cmd, Link, Outbox, TransportPhase};

use automerge::AutoCommit;
use tilo_session_client::{MemoryPeerStore, PeerConfig};
use tilo_user_identity::{DeviceCapabilities, RevocationList, UserIdentity};

use support::{
    admitted, enrol, holds, ready, route, say, start_host, start_host_at, start_peer, wait_for,
};

/// How many of `peer`'s writes to the workspace its replica's custody still holds.
fn unadmitted(peer: &Link) -> usize {
    let workspace = tilo_desktop_model::WorkspaceId::parse(support::WORKSPACE).unwrap();
    peer.engine()
        .owner()
        .unadmitted_writes(workspace)
        .unwrap_or_default()
}

/// An edit published before the page holds its route's keys waits for
/// them; a barrier called meanwhile waits with it until the host has made it
/// durable, however late a participant admits the page.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_barrier_covers_an_edit_held_for_the_workspaces_replay() {
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
    let _laptop = participant::Participant::start(&host.relay, &root).await;
    let peer = start_peer(
        host.iroh(),
        browser.cert.clone(),
        host_device.cert.clone(),
        browser.key.clone(),
    );
    // Before the page joins, it holds no keys.
    peer.write(
        route(),
        &say(&mut AutoCommit::new(), "written before the replay"),
    )
    .unwrap();
    let receipt = peer.barrier().unwrap();
    peer.join_placed(support::WORKSPACE, "Workspace").unwrap();
    wait_for("the barrier", || async { receipt.try_recv().ok() })
        .await
        .unwrap();
    let durable = peer
        .snapshot()
        .health
        .records
        .into_iter()
        .find(|record| record.record.record_id == route().record)
        .map_or(0, |note| note.outgoing.durable_operations);
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
    // The restarted host answers at the same web and iroh addresses, as a
    // host that kept its place does.
    let iroh_listen = "127.0.0.1:0".parse().unwrap();
    let host = start_host_at(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        iroh_listen,
    )
    .await;
    let mut laptop = participant::Participant::start(&host.relay, &root).await;
    let listen = host.web.addr;
    let iroh_listen = host.iroh_listen();

    let store = Arc::new(MemoryPeerStore::default());
    let peer = tilo_session_client::start_session(
        PeerConfig {
            host: host.iroh(),
            cert: browser.cert.clone(),
            host_cert: host_device.cert.clone(),
            revocations: RevocationList::new(),
            entering: None,
        },
        Arc::new(browser.key.clone()),
        store.clone(),
    );
    peer.join_placed(support::WORKSPACE, "Workspace").unwrap();
    peer.send(Cmd::OpenRoute(route())).unwrap();
    ready(&[&peer]).await;
    admitted(&peer).await;

    drop(host.web);
    host.relay.shutdown().await;
    let mut offline = AutoCommit::new();
    for text in ["written while offline", "typed while offline"] {
        peer.write(route(), &say(&mut offline, text)).unwrap();
    }
    // The outbox persists an edit before the link to the stopped host has
    // reported its end, so wait for the peer to see the host gone as well.
    wait_for("both edits in the persisted outbox, the host gone", || {
        let queued = unadmitted(&peer) == 2 && peer.status().phase != TransportPhase::Connected;
        async move { queued.then_some(()) }
    })
    .await;
    assert_ne!(peer.status().phase, TransportPhase::Connected);
    assert_eq!(peer.snapshot().synchronization, "pending");

    let host = start_host_at(&root, &host_device, state.path(), listen, iroh_listen).await;
    laptop.reconnect(&host.relay, &root).await;
    peer.send(Cmd::Reconnect).unwrap();
    let receipt = peer.barrier().unwrap();
    wait_for("the replayed commits to be durable", || async {
        receipt.try_recv().ok()
    })
    .await
    .unwrap();
    assert_eq!(unadmitted(&peer), 0);

    support::place(&host.relay, &observer.cert).await;
    let observer_peer = start_peer(
        host.iroh(),
        observer.cert.clone(),
        host_device.cert.clone(),
        observer.key.clone(),
    );
    observer_peer
        .join_placed(support::WORKSPACE, "Workspace")
        .unwrap();
    observer_peer.send(Cmd::OpenRoute(route())).unwrap();
    holds(&observer_peer, &route(), &offline).await;
    peer.close();
    observer_peer.close();
    host.relay.shutdown().await;
}
