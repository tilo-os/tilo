//! A large record change does not fit in one socket read, so it arrives
//! split across reads. The typed RPC transport must reassemble it without
//! truncation.

use crate::support_collab::TypedBroker;
use crate::support_collab::{change_carrying, changes_of};
use ui_core::collaboration::CollabRequest;

#[test]
fn large_frame_survives_split_reads() {
    let broker = TypedBroker::new();
    let mut a = broker.manager("alice");
    let mut b = broker.manager("bob");
    let _ = a.poll();
    let _ = b.poll();

    // 256 KB — far larger than a single socket read, so it MUST span
    // multiple reads on B's side.
    let big = change_carrying(vec![0xABu8; 256 * 1024]);
    a.send(CollabRequest::document_snapshot(big.clone()));

    // Poll B until the frame arrives, accumulating it across reads.
    let mut got: Option<Vec<u8>> = None;
    tilo_test_env::wait_until("the large snapshot at B", || {
        for e in b.poll() {
            if let Some(mutation) = e.document_mutation() {
                got = Some(mutation.data().to_vec());
            }
        }
        got.is_some()
    });

    let data = got.expect("B must receive the large change");
    assert_eq!(data.len(), big.len(), "payload length must match");
    assert!(
        changes_of(&data) == changes_of(&big),
        "payload must round-trip byte-for-byte"
    );

    drop(a);
    drop(b);
}
