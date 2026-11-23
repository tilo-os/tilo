//! A replica that writes speculatively (into a transaction it rolls back
//! before taking in the next remote change) and commits when nothing is
//! pending pays for what it writes, not for the size of the document
//! (vendor/automerge/TILO-PATCH.md, "a rolled back transaction keeps the op
//! index" and "an empty commit opens no transaction").

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ObjId, ObjType, ROOT, ReadDoc};
use tilo_automerge::ApplyIncoming;

#[allow(
    dead_code,
    reason = "this file uses the shared random source only; it replays nothing against upstream"
)]
use crate::upstream::Rng;

fn type_at_random(doc: &mut AutoCommit, text: &ObjId, rng: &mut Rng, letters: usize) {
    let at = rng.below(doc.length(text) as u64 + 1) as usize;
    let word: String = (0..letters)
        .map(|_| char::from(b'a' + rng.below(26) as u8))
        .collect();
    doc.splice_text(text, at, 0, &word).unwrap();
}

/// A text two members wrote in `changes` changes of 16 letters at
/// random places: old and new op ids side by side all along it, the history
/// whose lookups the op index serves.
fn scattered(changes: usize, rng: &mut Rng) -> (AutoCommit, ObjId) {
    let mut ada = AutoCommit::new().with_actor(ActorId::from(vec![0xa1; 16]));
    let text = ada.put_object(ROOT, "text", ObjType::Text).unwrap();
    ada.commit();
    let mut bo = ada.fork().with_actor(ActorId::from(vec![0xb2; 16]));
    for i in 0..changes {
        let writer = if i % 2 == 0 { &mut ada } else { &mut bo };
        type_at_random(writer, &text, rng, 16);
        writer.commit();
        if i % 64 == 63 {
            ada.merge_incoming(&mut bo).unwrap();
            bo.merge_incoming(&mut ada).unwrap();
        }
    }
    ada.merge_incoming(&mut bo).unwrap();
    (ada, text)
}

/// The slabs of the op id column that taking in the remote letter after
/// each of 24 of the replica's own letters searched, in a text written in
/// `changes` scattered changes; and that the replica ends as the remote
/// writer's document. The replica writes as an actor the document holds, as
/// `tilolang-tilo`'s store does: a transaction of an actor that has not
/// written adds it to the document and its rollback removes it, which
/// renumbers every op either way.
///
/// A lookup the index answers searches nothing; one it cannot answer
/// searches every slab of the column, so a rolled back letter that dropped
/// the index shows up here as searches at every letter after it, however
/// the machine is loaded.
fn searched_by_rollback_applies(changes: usize) -> Vec<u64> {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut base, text) = scattered(changes, &mut rng);
    let mut replica = AutoCommit::load(&base.save())
        .unwrap()
        .with_actor(ActorId::from(vec![0xa1; 16]));
    let mut remote = base.fork().with_actor(ActorId::from(vec![0xc3; 16]));
    let mut searched = Vec::new();
    for _ in 0..24 {
        let heads = remote.get_heads();
        type_at_random(&mut remote, &text, &mut rng, 1);
        remote.commit();
        let change = remote.save_after(&heads);
        type_at_random(&mut replica, &text, &mut rng, 1);
        replica.rollback();
        let before = automerge::work().searched_slabs;
        replica.load_incoming(&change).unwrap();
        searched.push(automerge::work().searched_slabs - before);
    }
    assert_eq!(replica.save(), remote.save());
    searched
}

#[test]
fn a_rolled_back_letter_does_not_make_the_next_remote_letter_search() {
    // A short history is not indexed, so its letters are answered by
    // searching: the count below rises there, and is what a dropped index
    // would cost.
    let short = searched_by_rollback_applies(100);
    assert!(
        short.iter().any(|slabs| *slabs > 0),
        "a replica that indexed nothing searched nothing: {short:?}"
    );
    // 32,000 letters of history: a document whose op id column spans slabs
    // enough to be indexed when it is loaded, so the index is what answers
    // each letter after the rollback.
    let searched = searched_by_rollback_applies(2_000);
    assert!(
        searched.iter().all(|slabs| *slabs == 0),
        "the letter taken back before a remote one made it search the op id \
         column: {searched:?} slabs"
    );
}

/// What empty commits by an actor that has not written renumbered, whether
/// they left the document and the actor as they were, and what the count
/// makes of a write by that actor taken back (which does open a
/// transaction), into a text two members wrote in `changes` scattered
/// changes (so that the ops' actor column changes every few ops).
fn empty_commits(changes: usize) -> (u64, bool, bool, u64) {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let (mut doc, _) = scattered(changes, &mut rng);
    let actor = ActorId::from(vec![0x01; 16]);
    let mut reader = AutoCommit::load(&doc.save())
        .unwrap()
        .with_actor(actor.clone());
    let saved = reader.save();
    let before = automerge::work().renumbered_objects;
    for _ in 0..20 {
        assert_eq!(reader.commit(), None);
    }
    let renumbered = automerge::work().renumbered_objects - before;
    // A write by an actor that has not written opens a transaction, which
    // adds it and renumbers: what an empty commit used to do, and what the
    // count above would see.
    let before = automerge::work().renumbered_objects;
    reader.put(ROOT, "probe", 1).unwrap();
    reader.rollback();
    let opened = automerge::work().renumbered_objects - before;
    (
        renumbered,
        reader.save() == saved,
        reader.get_actor() == &actor,
        opened,
    )
}

#[test]
fn an_empty_commit_does_not_renumber_the_document() {
    // Renumbering is the document's objects, not its ops, so two sizes
    // prove it costs a document nothing.
    for changes in [250, 1_000] {
        let (renumbered, unchanged, same_actor, opened) = empty_commits(changes);
        assert_eq!(
            renumbered, 0,
            "an empty commit opened a transaction, which adds its actor and \
             renumbers the document, after {changes} changes"
        );
        assert!(
            unchanged,
            "an empty commit leaves the document as it was, after {changes} changes"
        );
        assert!(same_actor, "an empty commit takes on no actor");
        assert!(
            opened > 0,
            "a write by an actor that has not written renumbers, and the count must see it"
        );
    }
}

#[test]
fn a_write_after_an_empty_commit_commits_as_before() {
    let mut doc = AutoCommit::new();
    let map = doc.put_object(ROOT, "entries", ObjType::Map).unwrap();
    doc.commit();
    assert_eq!(doc.commit(), None);
    doc.put(&map, "late", 1).unwrap();
    let hash = doc.commit().unwrap();
    assert_eq!(doc.get_heads(), vec![hash]);
    assert_eq!(doc.get(&map, "late").unwrap().unwrap().0.as_i64(), Some(1));
}

fn loaded_replica_searches(changes: usize) -> Vec<u64> {
    let mut rng = Rng(0x6a09_e667_f3bc_c909);
    let (mut base, text) = scattered(changes, &mut rng);
    let saved = base.save();
    let mut replica = AutoCommit::load(&saved).unwrap();
    let mut peer = AutoCommit::load(&saved)
        .unwrap()
        .with_actor(ActorId::from(vec![0xf0; 16]));
    let mut searched = Vec::new();
    for _ in 0..24 {
        let heads = peer.get_heads();
        type_at_random(&mut peer, &text, &mut rng, 1);
        peer.commit();
        let change = peer.save_after(&heads);
        let before = automerge::work().searched_slabs;
        replica.load_incoming(&change).unwrap();
        searched.push(automerge::work().searched_slabs - before);
    }
    assert_eq!(replica.save(), peer.save());
    searched
}

#[test]
fn a_loaded_replica_pays_for_no_index_build_on_a_later_change() {
    // 32,000 letters of scattered history: enough for the index to be worth
    // building. A replica that did not build it at load searched the op id
    // column on its first changes until those searches would have paid for
    // the index, then built it on the change that crossed the line: 3.4 ms
    // more than the median here, 2.5 ms in a release build, 10 ms at 10,000
    // notes. Indexed at load, it finds every op its peer's changes name
    // without searching.
    let searched = loaded_replica_searches(2_000);
    assert!(
        searched.iter().all(|slabs| *slabs == 0),
        "a loaded replica's applies searched the op id column: {searched:?} slabs"
    );
}
