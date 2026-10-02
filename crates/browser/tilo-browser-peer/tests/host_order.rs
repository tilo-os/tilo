//! A browser peer over HTTPS and a native peer over iroh commit concurrently
//! to one room; a third observer peer sees every change in exactly the
//! host's order — the order of the `Committed` sequence numbers both authors
//! received. One authority orders both carriers.

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
use tilo_browser_peer::{Host, PeerEvent, PeerHandle};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{enrol, ready, route, start_host, start_peer, wait_for, workspace};

const EACH: usize = 12;

fn mutation(author: &str, index: usize) -> DocumentMutation {
    DocumentMutation::Incremental(format!("{author}-{index}").into_bytes())
}

/// Every `Committed` sequence `peer` received, in arrival order.
async fn committed(peer: &PeerHandle, count: usize) -> Vec<u64> {
    let mut seqs = Vec::new();
    wait_for("every commit acknowledged", || {
        for event in peer.drain() {
            if let PeerEvent::Committed { seq, .. } = event {
                seqs.push(seq);
            }
        }
        let done = (seqs.len() >= count).then(|| seqs.clone());
        async move { done }
    })
    .await
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
    let _steward = steward::Steward::start(&host, &root).await;

    let browser_peer = start_peer(
        Host::Https(host.https()),
        browser.cert.clone(),
        host_device.cert.clone(),
        browser.key.clone(),
    );
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
    let observer_events = observer_peer.events();
    for peer in [&browser_peer, &laptop_peer, &observer_peer] {
        peer.join(workspace()).unwrap();
    }
    ready(&[&browser_peer, &laptop_peer, &observer_peer]).await;

    // Interleave the two authors' commits.
    for index in 0..EACH {
        browser_peer
            .publish(route(), &mutation("https", index))
            .unwrap();
        laptop_peer
            .publish(route(), &mutation("iroh", index))
            .unwrap();
    }
    let browser_seqs = committed(&browser_peer, EACH).await;
    let laptop_seqs = committed(&laptop_peer, EACH).await;

    // The host order of every change, from the authors' acknowledgements.
    let mut host_order: Vec<(u64, DocumentMutation)> = browser_seqs
        .iter()
        .enumerate()
        .map(|(index, seq)| (*seq, mutation("https", index)))
        .chain(
            laptop_seqs
                .iter()
                .enumerate()
                .map(|(index, seq)| (*seq, mutation("iroh", index))),
        )
        .collect();
    host_order.sort_by_key(|(seq, _)| *seq);
    let seqs: Vec<u64> = host_order.iter().map(|(seq, _)| *seq).collect();
    let mut distinct = seqs.clone();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        2 * EACH,
        "every commit has its own host position"
    );
    assert!(
        browser_seqs.windows(2).all(|pair| pair[0] < pair[1])
            && laptop_seqs.windows(2).all(|pair| pair[0] < pair[1]),
        "each author's commits keep their order"
    );

    // The observer sees exactly that order.
    let mut seen = Vec::new();
    wait_for("the observer to see every change", || {
        for event in observer_events.drain() {
            if let PeerEvent::Document(document) = event
                && let Some(mutation) = document.mutation()
            {
                seen.push(mutation);
            }
        }
        let done = (seen.len() >= 2 * EACH).then_some(());
        async move { done }
    })
    .await;
    let expected: Vec<DocumentMutation> = host_order.into_iter().map(|(_, m)| m).collect();
    assert_eq!(seen, expected, "the observer's order is the host order");
    for peer in [&browser_peer, &laptop_peer, &observer_peer] {
        peer.close();
    }
    host.relay.shutdown().await;
}
