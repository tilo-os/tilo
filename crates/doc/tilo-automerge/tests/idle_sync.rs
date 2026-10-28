//! An idle sync exchange between two peers in sync, each resuming from the
//! state it persisted after their last exchange, costs the same however long
//! the history behind them is (vendor/automerge/TILO-PATCH.md).

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::sync::{Message, State, SyncDoc};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT};
use tilo_automerge::ApplyIncoming;

/// A document of `changes` changes: a map entry per hundred changes, each
/// rewritten in a change of its own.
fn history(changes: u64) -> AutoCommit {
    let mut doc = AutoCommit::new();
    let files = doc.put_object(ROOT, "files", ObjType::Map).unwrap();
    let mut file = files.clone();
    for change in 0..changes {
        if change.is_multiple_of(100) {
            file = doc
                .put_object(&files, format!("file-{change}"), ObjType::Map)
                .unwrap();
        }
        doc.put(&file, "size", change).unwrap();
        doc.commit();
    }
    doc
}

/// Runs one exchange to completion; returns the bytes both sides sent.
fn exchange(
    (a, a_state): (&mut AutoCommit, &mut State),
    (b, b_state): (&mut AutoCommit, &mut State),
) -> usize {
    let mut bytes = 0;
    for _ in 0..64 {
        let message = a.sync().generate_sync_message(a_state);
        if let Some(message) = &message {
            let encoded = message.clone().encode();
            bytes += encoded.len();
            b.receive_incoming_sync(b_state, Message::decode(&encoded).unwrap())
                .unwrap();
        }
        let reply = b.sync().generate_sync_message(b_state);
        if let Some(reply) = &reply {
            let encoded = reply.clone().encode();
            bytes += encoded.len();
            a.receive_incoming_sync(a_state, Message::decode(&encoded).unwrap())
                .unwrap();
        }
        if message.is_none() && reply.is_none() {
            return bytes;
        }
    }
    panic!("the exchange did not complete");
}

/// The most changes of the history any of several idle exchanges between
/// two copies of a document of `changes` changes visited, each resuming from
/// the persisted state of the last, and the bytes one of them sent.
fn idle_exchange(changes: u64) -> (u64, usize) {
    let mut a = history(changes);
    let mut b = AutoCommit::load(&a.save()).unwrap();
    let (mut a_state, mut b_state) = (State::new(), State::new());
    exchange((&mut a, &mut a_state), (&mut b, &mut b_state));
    let mut visited = 0;
    let mut bytes = 0;
    for _ in 0..20 {
        let mut a_resumed = State::decode(&a_state.encode()).unwrap();
        let mut b_resumed = State::decode(&b_state.encode()).unwrap();
        let before = automerge::work().visited_changes;
        bytes = exchange((&mut a, &mut a_resumed), (&mut b, &mut b_resumed));
        visited = visited.max(automerge::work().visited_changes - before);
        (a_state, b_state) = (a_resumed, b_resumed);
    }
    (visited, bytes)
}

#[test]
fn an_idle_resumed_exchange_does_not_walk_the_history() {
    let (short, short_bytes) = idle_exchange(200);
    let (long, long_bytes) = idle_exchange(20_000);
    assert_eq!(
        short_bytes, long_bytes,
        "an idle exchange sends heads, not a summary of the history"
    );
    // Walking 20,000 ancestors for every message took most of a
    // millisecond; the exchange itself, a few microseconds at either size.
    assert!(
        long <= short * 2 + 16,
        "an idle exchange visited {long} changes over 20,000 and {short} over 200"
    );
}
