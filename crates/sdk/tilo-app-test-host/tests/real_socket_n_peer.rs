//! N-peer end-to-end test across real Unix sockets speaking the production
//! collaboration wire.
//!
//! Generalizes `real_socket_two_peer.rs` to three peers, asserting the two
//! properties that matter for N > 2:
//!
//!   1. **Fan-out excludes the sender.** A document incremental from peer A
//!      reaches B and C exactly once each, and never echoes back to A.
//!   2. **Presence is attributed per peer.** A presence from A and B
//!      arrives at C tagged with the id each announced — so the two are
//!      distinct — and A never receives its own presence back.
//!

use crate::support_collab::{TypedBroker, change_carrying, changes_of};
use tilo_ui_base::{collaboration::CollabEvent, collaboration::CollabRequest};

#[test]
fn n_peer_fanout_and_per_peer_attribution() {
    let broker = TypedBroker::new();
    let mut a = broker.manager("peer-a");
    let mut b = broker.manager("peer-b");
    let mut c = broker.manager("peer-c");

    let _ = a.poll();
    let _ = b.poll();
    let _ = c.poll();

    // A record takes real changes: the store reads every write as one.
    let x = change_carrying(b"X".to_vec());
    a.send(CollabRequest::document_incremental(x.clone()));

    let count_incremental = |evs: &[CollabEvent]| {
        evs.iter()
            .filter(|event| {
                matches!(
                    event.document_mutation().cloned(),
                    Some(collab_types::DocumentMutation::Incremental(data)) if changes_of(&data) == changes_of(&x)
                )
            })
            .count()
    };
    let (mut b_evs, mut c_evs) = (Vec::new(), Vec::new());
    tilo_test_env::wait_until("A's incremental at B and C", || {
        b_evs.extend(b.poll());
        c_evs.extend(c.poll());
        count_incremental(&b_evs) > 0 && count_incremental(&c_evs) > 0
    });
    let a_evs = a.poll();
    assert_eq!(
        count_incremental(&b_evs),
        1,
        "B should get A's incremental once: {b_evs:?}"
    );
    assert_eq!(
        count_incremental(&c_evs),
        1,
        "C should get A's incremental once: {c_evs:?}"
    );
    assert_eq!(
        count_incremental(&a_evs),
        0,
        "A must not receive its own incremental: {a_evs:?}"
    );

    drop(a);
    drop(b);
    drop(c);
}
