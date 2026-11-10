//! A browser peer over HTTPS and a native peer over iroh commit concurrently
//! to one workspace: the host keeps every commit of both at a position of its
//! own, and a third observer peer holds every change. One authority orders
//! both carriers.

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

use automerge::AutoCommit;
use tilo_automerge::ApplyIncoming as _;
use tilo_session_host::PeerHandle;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{enrol, holds, ready, route, say, start_host, start_peer};

const EACH: usize = 12;

/// Wait until everything `peer` published is durable, and answer the host's acknowledgement of
/// its writes to the note: how many it kept, and its order of the last.
async fn durable(peer: &PeerHandle) -> (u64, u64) {
    support::durable(peer).await;
    let note = peer
        .snapshot()
        .health
        .records
        .into_iter()
        .find(|record| record.record.record_id == route().record)
        .expect("the note's health");
    (
        note.outgoing.durable_operations,
        note.acknowledged_seq.expect("an acknowledged write"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn https_and_iroh_commits_share_one_host_order() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let observer = enrol(&root, "phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _laptop = participant::Participant::start(&host.relay, &root).await;

    let browser_peer = start_peer(
        host.iroh(),
        browser.cert.clone(),
        host_device.cert.clone(),
        browser.key.clone(),
    );
    support::place(&host.relay, &laptop.cert).await;
    support::place(&host.relay, &observer.cert).await;
    let laptop_peer = start_peer(
        host.iroh(),
        laptop.cert.clone(),
        host_device.cert.clone(),
        laptop.key.clone(),
    );
    let observer_peer = start_peer(
        host.iroh(),
        observer.cert.clone(),
        host_device.cert.clone(),
        observer.key.clone(),
    );
    for peer in [&browser_peer, &laptop_peer, &observer_peer] {
        peer.join_placed(support::WORKSPACE, "Workspace").unwrap();
    }
    ready(&[&browser_peer, &laptop_peer, &observer_peer]).await;

    // Interleave the two authors' commits.
    let (mut over_https, mut over_iroh) = (AutoCommit::new(), AutoCommit::new());
    for index in 0..EACH {
        browser_peer
            .publish(route(), &say(&mut over_https, &format!("https-{index}")))
            .unwrap();
        laptop_peer
            .publish(route(), &say(&mut over_iroh, &format!("iroh-{index}")))
            .unwrap();
    }
    let (browser_kept, browser_last) = durable(&browser_peer).await;
    let (laptop_kept, laptop_last) = durable(&laptop_peer).await;
    assert_eq!((browser_kept, laptop_kept), (EACH as u64, EACH as u64));
    assert_ne!(
        browser_last, laptop_last,
        "every commit has its own host position"
    );

    // The observer holds every change of both.
    let mut both = over_https.fork();
    both.merge_incoming(&mut over_iroh).unwrap();
    holds(&observer_peer, &route(), &both).await;
    for peer in [&browser_peer, &laptop_peer, &observer_peer] {
        peer.close();
    }
    host.relay.shutdown().await;
}
