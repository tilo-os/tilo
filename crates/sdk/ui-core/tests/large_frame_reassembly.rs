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

use crate::support::typed_collab::TypedBroker;
use tilo_ui::collaboration::CollabRequest;

#[test]
fn large_frame_survives_split_reads() {
    let broker = TypedBroker::new();
    let mut a = broker.manager("alice");
    let mut b = broker.manager("bob");
    let _ = a.poll();
    let _ = b.poll();

    // 256 KB — far larger than a single socket read, so it MUST span
    // multiple reads on B's side.
    let big = vec![0xABu8; 256 * 1024];
    a.send(CollabRequest::document_snapshot(big.clone()));

    // Poll B until the frame arrives, accumulating it across reads.
    let mut got: Option<Vec<u8>> = None;
    tilo_test_env::wait_until("the large snapshot at B", || {
        for e in b.poll() {
            if let Some(Ok(collab_types::DocumentMutation::Snapshot(data))) = e.document_mutation()
            {
                got = Some(data);
            }
        }
        got.is_some()
    });

    let data = got.expect("B must receive the large document snapshot");
    assert_eq!(data.len(), big.len(), "payload length must match");
    assert!(data == big, "payload must round-trip byte-for-byte");

    drop(a);
    drop(b);
}
