//! What a collaborating document holds after merging a long run of remote deltas.
//!
//! A peer types and writes settings in 20 rounds of 1000 ops; the local [`State`] merges each
//! round as the collaboration manager does with an incremental document envelope. Its growth is
//! compared with the same document loaded from a full save.

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT};
use tilo_automerge::heap::{self, ThreadCounting};
use tilo_ui::state::{DocumentReplica, State};

#[global_allocator]
static HEAP: ThreadCounting = ThreadCounting;

const ROUNDS: i64 = 20;
const OPS_PER_ROUND: usize = 1000;
const SETTINGS_EVERY: usize = 10;
const TYPED_RUN: &str = "xxxxxxxxx";
// Every round is still 1000 ops: 100 settings writes and 900 typed characters.
const _: () = assert!(TYPED_RUN.len() == SETTINGS_EVERY - 1);

/// A peer's incremental saves, one per round, after the one that forks it from `base`.
fn remote_rounds(base: &mut AutoCommit) -> Vec<Vec<u8>> {
    let mut remote = base.fork();
    let text = remote.put_object(ROOT, "text", ObjType::Text).unwrap();
    let settings = remote.put_object(ROOT, "settings", ObjType::Map).unwrap();
    let mut rounds = vec![remote.save_after(&base.get_heads())];
    let mut typed = 0usize;
    for round in 0..ROUNDS {
        let heads = remote.get_heads();
        // Each settings write is followed by nine typed characters. One splice of the
        // nine records the same nine single-character inserts, each after the last, as
        // nine one-character splices do, without locating the insertion point nine times.
        for op in (0..OPS_PER_ROUND).step_by(SETTINGS_EVERY) {
            remote
                .put(&settings, format!("k{}", op % 50), round)
                .unwrap();
            remote.splice_text(&text, typed, 0, TYPED_RUN).unwrap();
            typed += TYPED_RUN.len();
        }
        remote.commit();
        rounds.push(remote.save_after(&heads));
    }
    rounds
}

#[test]
fn an_app_document_holds_what_its_saved_document_holds() {
    let mut state = State::new();
    let rounds = remote_rounds(&mut state.doc);
    let mut grown = 0isize;
    for round in &rounds {
        let before = heap::held();
        state.load_incremental(round).unwrap();
        grown += heap::held() - before;
    }
    let saved = state.doc.save();
    let loaded = heap::held_by(|| State::from_doc(AutoCommit::load(&saved).unwrap()));
    eprintln!("app document: grown {grown} bytes, loaded {loaded} bytes");
    assert!(
        grown < loaded * 2,
        "a document fed {} remote ops holds {grown} bytes against {loaded} loaded",
        rounds.len() * OPS_PER_ROUND
    );
}

#[test]
fn a_metadata_replica_holds_what_its_saved_document_holds() {
    let mut replica = DocumentReplica::new(AutoCommit::new());
    let rounds = remote_rounds(replica.document_mut());
    let mut grown = 0isize;
    for round in &rounds {
        let before = heap::held();
        replica.load_incremental(round).unwrap();
        grown += heap::held() - before;
    }
    let saved = replica.document_mut().save();
    let loaded = heap::held_by(|| DocumentReplica::new(AutoCommit::load(&saved).unwrap()));
    eprintln!("metadata replica: grown {grown} bytes, loaded {loaded} bytes");
    assert!(
        grown < loaded * 2,
        "a replica fed {} remote ops holds {grown} bytes against {loaded} loaded",
        rounds.len() * OPS_PER_ROUND
    );
}
