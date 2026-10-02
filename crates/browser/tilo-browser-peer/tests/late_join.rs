//! A device that joins late receives a route's retained checkpoint and its
//! whole tail from the host: a checkpoint too large for one frame arrives as
//! one reassembled snapshot, and the host streams it in bounded pages.

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

use std::sync::atomic::Ordering;

use collab_types::DocumentMutation;
use tilo_browser_peer::{Host, PeerEvent};
use tilo_relay_protocol::web::MAX_PAGE_BYTES;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{Recording, enrol, ready, route, start_host, start_peer, wait_for, workspace};

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_joiner_receives_a_segmented_checkpoint_and_its_tail_in_bounded_pages() {
    const TAIL: usize = 200;
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let author = enrol(&root, "browser", DeviceCapabilities::member());
    let joiner = enrol(&root, "phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _steward = steward::Steward::start(&host, &root).await;

    let author = start_peer(
        Host::Https(host.https()),
        author.cert,
        host_device.cert.clone(),
        author.key,
    );
    author.join(workspace()).unwrap();
    ready(&[&author]).await;
    let checkpoint = DocumentMutation::Snapshot(noise(5 * 1024 * 1024 / 2, 1));
    author.publish(route(), &checkpoint).unwrap();
    let tail: Vec<DocumentMutation> = (0..TAIL)
        .map(|index| DocumentMutation::Incremental(noise(8 * 1024, index as u64 + 2)))
        .collect();
    for mutation in &tail {
        author.publish(route(), mutation).unwrap();
    }
    let receipt = author.barrier().unwrap();
    wait_for("the history to be durable", || async {
        receipt.try_recv().ok()
    })
    .await
    .unwrap();

    let recording = Recording::new(host.https());
    let largest_page = recording.largest_page.clone();
    let joined_at = std::time::Instant::now();
    let joiner = start_peer(
        Host::Https(recording),
        joiner.cert,
        host_device.cert.clone(),
        joiner.key,
    );
    joiner.join(workspace()).unwrap();
    let mut received: Vec<DocumentMutation> = Vec::new();
    wait_for("the checkpoint and its tail", || {
        received.extend(joiner.drain().into_iter().filter_map(|event| match event {
            PeerEvent::Document(document) => document.mutation(),
            _ => None,
        }));
        let done = received.len() > TAIL;
        async move { done.then_some(()) }
    })
    .await;
    eprintln!(
        "late join: 2.5 MiB checkpoint + {TAIL} x 8 KiB tail in {} ms, largest page {} bytes",
        joined_at.elapsed().as_millis(),
        largest_page.load(Ordering::Relaxed)
    );
    assert_eq!(received.len(), TAIL + 1, "no segment surfaces on its own");
    assert_eq!(
        received[0], checkpoint,
        "the checkpoint is reassembled first"
    );
    assert_eq!(&received[1..], tail.as_slice(), "the tail follows in order");
    assert!(
        largest_page.load(Ordering::Relaxed) <= MAX_PAGE_BYTES + 64 * 1024,
        "pages stay near the byte bound, one oversized envelope at most"
    );
}
