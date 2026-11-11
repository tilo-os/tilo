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
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "support/participant.rs"]
mod participant;
mod support;

// Placing the workspace has the host build its governance in an automerge worker.
tilo_automerge_isolate::test_worker!();

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable as _};
use collab_types::DocumentMutation;
use tilo_session_client::Outbox;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{enrol, ready, route, start_host, start_peer};

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
    let _laptop = participant::Participant::start(&host.relay, &root).await;

    let author = start_peer(
        host.iroh(),
        author.cert,
        host_device.cert.clone(),
        author.key,
    );
    author.join_placed(support::WORKSPACE, "Workspace").unwrap();
    ready(&[&author]).await;
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "checkpoint", noise(5 * 1024 * 1024 / 2, 1))
        .unwrap();
    author
        .write(route(), &DocumentMutation::Snapshot(doc.save()))
        .unwrap();
    for index in 0..TAIL {
        doc.put(
            ROOT,
            format!("tail-{index}"),
            noise(8 * 1024, index as u64 + 2),
        )
        .unwrap();
        author
            .write(
                route(),
                &DocumentMutation::Incremental(doc.save_incremental()),
            )
            .unwrap();
    }
    support::durable(&author).await;

    let joined_at = std::time::Instant::now();
    let joiner = start_peer(
        host.iroh(),
        joiner.cert,
        host_device.cert.clone(),
        joiner.key,
    );
    joiner.join_placed(support::WORKSPACE, "Workspace").unwrap();
    let held = support::holds(&joiner, &route(), &doc).await;
    eprintln!(
        "late join: 2.5 MiB checkpoint + {TAIL} x 8 KiB tail in {} ms",
        joined_at.elapsed().as_millis()
    );
    assert_eq!(
        held.get(ROOT, "checkpoint")
            .unwrap()
            .map(|(value, _)| value),
        doc.get(ROOT, "checkpoint").unwrap().map(|(value, _)| value),
        "the segmented checkpoint is whole"
    );
}
