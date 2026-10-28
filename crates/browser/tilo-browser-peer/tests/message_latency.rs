//! How long a message takes from one page to another in the same room when
//! both reach the host over HTTPS, as two browsers do: from the moment the
//! sender's page publishes a change to the moment the other page holds it.
//! The pages' own frames are not in it; this is the carrier and the host.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "support/steward.rs"]
mod steward;
mod support;

use std::time::{Duration, Instant};

use collab_types::DocumentMutation;
use tilo_browser_peer::{Host, PeerEvent};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{enrol, ready, route, start_host, start_peer, wait_for, workspace};

const MESSAGES: usize = 20;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_reaches_the_other_page_without_waiting_for_a_poll() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let sender = enrol(&root, "sender", DeviceCapabilities::member());
    let receiver = enrol(&root, "receiver", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _steward = steward::Steward::start(&host.relay, &root).await;
    let page = |device: &support::Device| {
        start_peer(
            Host::Https(host.https()),
            device.cert.clone(),
            host_device.cert.clone(),
            device.key.clone(),
        )
    };
    let (a, b) = (page(&sender), page(&receiver));
    let b_events = b.events();
    for peer in [&a, &b] {
        peer.join(workspace()).unwrap();
    }
    ready(&[&a, &b]).await;

    let mut took = Vec::new();
    for index in 0..=MESSAGES {
        let message = DocumentMutation::Incremental(format!("message-{index}").into_bytes());
        let sent = Instant::now();
        a.publish(route(), &message).unwrap();
        let arrived = wait_for("the other page to hold the message", || {
            let held = b_events.drain().into_iter().any(|event| {
                matches!(event, PeerEvent::Document(document)
                    if document.mutation().as_ref() == Some(&message))
            });
            async move { held.then(Instant::now) }
        })
        .await;
        // The first one also opens the record's channel.
        if index > 0 {
            took.push(arrived - sent);
        }
        // A pause, as between messages a person types, so each one finds
        // the receiver waiting rather than mid-answer.
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    took.sort();
    let p50 = took[took.len() / 2];
    let p95 = took[took.len() * 95 / 100];
    eprintln!("page to page: p50 {p50:?}, p95 {p95:?}, all {took:?}");
    // On loopback this is a few milliseconds of sealing and two HTTPS
    // exchanges. A receiver that only learned of it on its next poll would
    // take that poll's interval; the bound is far above the real cost and
    // far below any interval, so a loaded runner does not trip it.
    assert!(
        p50 < Duration::from_millis(150),
        "a message took {p50:?} (p50) from page to page: {took:?}"
    );
    for peer in [&a, &b] {
        peer.close();
    }
    host.relay.shutdown().await;
}
