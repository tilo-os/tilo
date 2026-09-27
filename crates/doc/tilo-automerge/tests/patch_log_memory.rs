//! What a long-lived replica holds after taking in many remote ops.
//!
//! A peer sends 20 rounds of 1000 ops (typed text and map writes); the receiver takes each
//! round in as an incremental save. Its growth is compared with the same document loaded from a
//! full save, which holds the op set, change graph and nothing else.

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT};
use tilo_automerge::ApplyIncoming;
use tilo_automerge::heap::{self, ThreadCounting};

#[global_allocator]
static HEAP: ThreadCounting = ThreadCounting;

const ROUNDS: usize = 20;
const OPS_PER_ROUND: usize = 1000;
const META_EVERY: usize = 10;
const TYPED_RUN: &str = "xxxxxxxxx";
// Every round is still 1000 ops: 100 map writes and 900 typed characters.
const _: () = assert!(TYPED_RUN.len() == META_EVERY - 1);

/// The incremental saves of a peer typing into a text and writing a map, one per round.
fn remote_rounds() -> Vec<Vec<u8>> {
    let mut remote = AutoCommit::new();
    let text = remote.put_object(ROOT, "text", ObjType::Text).unwrap();
    let meta = remote.put_object(ROOT, "meta", ObjType::Map).unwrap();
    let mut rounds = vec![remote.save_incremental()];
    let mut typed = 0usize;
    for round in 0..ROUNDS {
        // Each map write is followed by nine typed characters. One splice of the nine
        // records the same nine single-character inserts, each after the last, as nine
        // one-character splices do, without locating the insertion point nine times.
        for op in (0..OPS_PER_ROUND).step_by(META_EVERY) {
            remote
                .put(
                    &meta,
                    format!("k{}", op % 50),
                    i64::try_from(round).unwrap(),
                )
                .unwrap();
            remote.splice_text(&text, typed, 0, TYPED_RUN).unwrap();
            typed += TYPED_RUN.len();
        }
        remote.commit();
        rounds.push(remote.save_incremental());
    }
    rounds
}

/// Bytes the receiver grew by taking in every round through `apply`, and bytes the same
/// document holds when loaded from a full save.
fn take_in(rounds: &[Vec<u8>], mut apply: impl FnMut(&mut AutoCommit, &[u8])) -> (isize, isize) {
    let mut receiver = AutoCommit::new();
    let mut grown = 0isize;
    for round in rounds {
        let before = heap::held();
        apply(&mut receiver, round);
        grown += heap::held() - before;
    }
    let saved = receiver.save();
    let loaded = heap::held_by(|| AutoCommit::load(&saved).unwrap());
    (grown, loaded)
}

#[test]
fn a_replica_holds_what_its_saved_document_holds() {
    let rounds = remote_rounds();
    let (grown, loaded) = take_in(&rounds, |doc, bytes| {
        doc.load_incoming(bytes).unwrap();
    });
    eprintln!("load_incoming: grown {grown} bytes, loaded {loaded} bytes");
    assert!(
        grown < loaded * 2,
        "a replica fed {} remote ops holds {grown} bytes against {loaded} for the same document loaded",
        ROUNDS * OPS_PER_ROUND
    );
}

/// Pins the automerge behaviour `ApplyIncoming` exists for. Should automerge stop logging into
/// an inactive patch log, this fails, and the wrapper and its clippy ban can go.
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "demonstrates the raw call's growth"
)]
fn automerge_keeps_a_patch_event_per_applied_op() {
    let rounds = remote_rounds();
    let (grown, loaded) = take_in(&rounds, |doc, bytes| {
        doc.load_incremental(bytes).unwrap();
    });
    eprintln!("load_incremental: grown {grown} bytes, loaded {loaded} bytes");
    assert!(
        grown > loaded * 4,
        "raw load_incremental grew {grown} bytes against {loaded} loaded"
    );
}
