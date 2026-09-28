//! N-peer end-to-end test across real Unix sockets speaking the production
//! Cap'n Proto collaboration protocol.
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
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::support::{document_mutation, typed_collab::TypedBroker};
use tilo_ui::collaboration::{CollabEvent, CollabRequest, PresencePayload};
use tilo_ui::presence::PresenceKey;

#[test]
fn n_peer_fanout_and_per_peer_attribution() {
    let broker = TypedBroker::new();
    let mut a = broker.manager("peer-a");
    let mut b = broker.manager("peer-b");
    let mut c = broker.manager("peer-c");

    let _ = a.poll();
    let _ = b.poll();
    let _ = c.poll();

    // --- 1. Fan-out excludes the sender -----------------------------
    a.send(CollabRequest::document_incremental(b"X".to_vec()));

    let count_incremental = |evs: &[CollabEvent]| {
        evs.iter()
            .filter(|event| {
                matches!(
                    document_mutation(event),
                    Some(collab_types::DocumentMutation::Incremental(data)) if data == b"X"
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

    // --- 2. Per-peer presence attribution ---------------------------
    // A announces on Workspace "1"; B announces on Workspace "2".
    a.send(CollabRequest::Presence {
        key: PresenceKey::Workspace { id: "1".into() },
        payload: PresencePayload::Pointer { x: 1.0, y: 1.0 },
    });
    b.send(CollabRequest::Presence {
        key: PresenceKey::Workspace { id: "2".into() },
        payload: PresencePayload::Pointer { x: 2.0, y: 2.0 },
    });
    // Extract the attributed user_id for each source by its distinct key.
    let uid_for = |evs: &[CollabEvent], ws: &str| -> Option<String> {
        evs.iter().find_map(|e| match e {
            CollabEvent::RemotePresenceUpdate {
                user_id,
                key: PresenceKey::Workspace { id },
                ..
            } if id == ws => Some(user_id.clone()),
            _ => None,
        })
    };
    let (mut c_evs, mut a_evs) = (Vec::new(), Vec::new());
    tilo_test_env::wait_until("both presences at C and B's at A", || {
        c_evs.extend(c.poll());
        a_evs.extend(a.poll());
        uid_for(&c_evs, "1").is_some()
            && uid_for(&c_evs, "2").is_some()
            && uid_for(&a_evs, "2").is_some()
    });
    let uid_a = uid_for(&c_evs, "1").expect("C sees A's Workspace-1 presence");
    let uid_b = uid_for(&c_evs, "2").expect("C sees B's Workspace-2 presence");
    // Asserts the exact announced ids, not just `uid_a != uid_b` — that
    // weaker check would still pass if the broker swapped them.
    assert_eq!(uid_a, "peer-a", "A's presence must be attributed to A");
    assert_eq!(uid_b, "peer-b", "B's presence must be attributed to B");
    assert_ne!(uid_a, uid_b, "A and B must be attributed distinct user ids");

    // A must NOT receive its own Workspace-1 presence, but SHOULD see
    // B's Workspace-2 presence.
    assert!(
        uid_for(&a_evs, "1").is_none(),
        "A must not receive its own presence back: {a_evs:?}"
    );
    assert!(
        uid_for(&a_evs, "2").is_some(),
        "A should receive B's presence: {a_evs:?}"
    );

    drop(a);
    drop(b);
    drop(c);
}
