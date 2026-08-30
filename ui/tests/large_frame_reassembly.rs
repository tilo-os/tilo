//! A large `SyncCRDT` payload (a full-document snapshot — the late-join
//! reply) does not fit in one socket read, so it arrives split across
//! reads. The typed RPC transport must reassemble it without truncation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use std::thread;
use std::time::Duration;

use crate::support::typed_collab::TypedBroker;
use ui::collaboration::{CollabEvent, CollabRequest};

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
    a.send(CollabRequest::SyncCRDT { data: big.clone() });

    // Poll B for up to ~2s, accumulating the frame across reads.
    let mut got: Option<Vec<u8>> = None;
    for _ in 0..40 {
        thread::sleep(Duration::from_millis(50));
        for e in b.poll() {
            if let CollabEvent::CRDTUpdate { data } = e {
                got = Some(data);
            }
        }
        if got.is_some() {
            break;
        }
    }

    let data = got.expect("B must receive the large CRDTUpdate frame");
    assert_eq!(data.len(), big.len(), "payload length must match");
    assert!(data == big, "payload must round-trip byte-for-byte");

    drop(a);
    drop(b);
}
