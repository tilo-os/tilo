//! A page's input on another device's window reaches that device: the page has no direct route to
//! it, so its session says the control message through the workspace's host, which hands it to
//! the hosting device alone, stamped as the page's.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "in a test these are the assertion, not a hazard; the arithmetic is over a bounded \
              sample of delivery times"
)]

use std::time::{Duration, Instant};

use neighbour_wire::NeighbourMsg;
use neighbour_wire::input::InputEvent;
use tilo_session_client::{Cmd, Event, Outbox};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{enrol, ready, start_host, start_peer, wait_for};

/// How many key presses the page sends, each timed from the page to the hosting device.
const PRESSES: u32 = 40;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pages_input_on_a_window_reaches_the_device_hosting_it() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let page = enrol(&root, "browser", DeviceCapabilities::member());
    let window_host = enrol(&root, "Linux apps", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    crate::support::place(&host.relay, &page.cert).await;
    let start = |device: &crate::support::Device| {
        start_peer(
            host.iroh(),
            device.cert.clone(),
            host_device.cert.clone(),
            device.key.clone(),
        )
    };
    let (page_peer, window_peer) = (start(&page), start(&window_host));
    let heard = window_peer.events();
    for peer in [&page_peer, &window_peer] {
        peer.join_placed(crate::support::WORKSPACE, "Workspace")
            .unwrap();
    }
    ready(&[&page_peer, &window_peer]).await;
    crate::support::admitted_to(&page_peer, crate::support::workspace()).await;
    crate::support::admitted_to(&window_peer, crate::support::workspace()).await;

    let target = iroh_base::EndpointId::from_bytes(&window_host.cert.device_public_key).unwrap();
    let press = |n: u32| NeighbourMsg::RemoteInput {
        peer: 0,
        surface: 0x1e7,
        event: postcard::to_allocvec(&InputEvent::Key {
            keysym: 30 + n % 10,
            pressed: true,
            modifiers: 0,
        })
        .unwrap(),
    };
    let mut took = Vec::new();
    for n in 0..PRESSES {
        let sent = Instant::now();
        page_peer
            .send(Cmd::SendControl {
                ws: crate::support::workspace(),
                msg: Box::new(press(n)),
                target,
            })
            .unwrap();
        let arrived = wait_for("the page's key press at the window's device", || {
            let arrived = heard.drain().into_iter().find_map(|event| match event {
                Event::RecordContent { from, frame, .. } => {
                    Some((from, postcard::from_bytes::<NeighbourMsg>(&frame).ok()?))
                }
                _ => None,
            });
            async move { arrived }
        })
        .await;
        took.push(sent.elapsed());
        assert_eq!(
            arrived,
            (page.cert.device_public_key, press(n)),
            "the press arrives whole, as the page's own"
        );
    }
    took.sort();
    let at = |share: f64| took[((took.len() as f64 - 1.0) * share) as usize];
    eprintln!(
        "input from a page to the device hosting the window: p50 {:?}, p95 {:?}",
        at(0.5),
        at(0.95)
    );
    assert!(
        at(0.95) < Duration::from_secs(1),
        "input lands within a moment"
    );
}
