//! A large document snapshot payload (the late-join
//! reply) does not fit in one socket read, so it arrives split across
//! reads. The typed RPC transport must reassemble it without truncation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::thread;
use std::time::Duration;

use crate::support::typed_collab::TypedBroker;
use tilo_ui::collaboration::CollabRequest;

/// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
const HANG_BOUND: Duration = Duration::from_secs(30);

#[test]
fn large_frame_survives_split_reads() {
    let broker = TypedBroker::new();
    let mut a = broker.manager("alice");
    let mut b = broker.manager("bob");
    thread::sleep(Duration::from_millis(150));
    let _ = a.poll();
    let _ = b.poll();

    // 256 KB — far larger than a single socket read, so it MUST span
    // multiple reads on B's side.
    let big = vec![0xABu8; 256 * 1024];
    a.send(CollabRequest::document_snapshot(big.clone()));

    // Poll B until the frame arrives, accumulating it across reads.
    let mut got: Option<Vec<u8>> = None;
    let deadline = std::time::Instant::now() + HANG_BOUND;
    while std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
        for e in b.poll() {
            if let Some(Ok(collab_types::DocumentMutation::Snapshot(data))) = e.document_mutation()
            {
                got = Some(data);
            }
        }
        if got.is_some() {
            break;
        }
    }

    let data = got.expect("B must receive the large document snapshot");
    assert_eq!(data.len(), big.len(), "payload length must match");
    assert!(data == big, "payload must round-trip byte-for-byte");

    drop(a);
    drop(b);
}
