//! Taking in a remote edit costs the same however long the text's history,
//! also when that history was written at scattered places by collaborators
//! typing at once (vendor/automerge/TILO-PATCH.md, "find an op by its id
//! without searching the document").
//!
//! Such a text holds old and new op ids side by side all along it, so
//! finding the element a remote letter goes after, by its id, used to search
//! nearly the whole document: a one-letter change cost 0.04 ms at 1,000
//! changes of history and 0.5 ms at 20,000. The fork keeps an index of
//! where each op is. The documents, heads and patches it makes from such
//! histories are byte for byte crates.io automerge 0.12.0's.

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ObjId, ObjType, ROOT, ReadDoc, TextEncoding};
use tilo_automerge::ApplyIncoming;

#[allow(
    dead_code,
    reason = "this file replays histories against upstream; it forges no changes"
)]
mod upstream;
use upstream::{Replica, Rng, agrees_with_upstream};

/// Letters at a random place of `text`.
fn type_at_random(doc: &mut AutoCommit, text: &ObjId, rng: &mut Rng, letters: usize) {
    let at = rng.below(doc.length(text) as u64 + 1) as usize;
    let word: String = (0..letters)
        .map(|_| char::from(b'a' + rng.below(26) as u8))
        .collect();
    doc.splice_text(text, at, 0, &word).unwrap();
}

/// A document whose text `busy` two collaborators wrote in `changes` changes
/// of `letters` letters each, at random places, merging every 64 changes,
/// and whose text `fresh` holds a few letters.
fn scattered(changes: usize, letters: usize, rng: &mut Rng) -> (AutoCommit, ObjId, ObjId) {
    let mut ada = AutoCommit::new().with_actor(ActorId::from(vec![0xa1; 16]));
    let busy = ada.put_object(ROOT, "busy", ObjType::Text).unwrap();
    let fresh = ada.put_object(ROOT, "fresh", ObjType::Text).unwrap();
    ada.splice_text(&fresh, 0, 0, "fresh").unwrap();
    ada.commit();
    let mut bo = ada.fork().with_actor(ActorId::from(vec![0xb2; 16]));
    for i in 0..changes {
        let writer = if i % 2 == 0 { &mut ada } else { &mut bo };
        type_at_random(writer, &busy, rng, letters);
        writer.commit();
        if i % 64 == 63 {
            ada.merge_incoming(&mut bo).unwrap();
            bo.merge_incoming(&mut ada).unwrap();
        }
    }
    ada.merge_incoming(&mut bo).unwrap();
    (ada, busy, fresh)
}

/// Letters the larger probe change types, each at its own random place.
const PLACES: u32 = 16;

fn median(mut samples: Vec<u64>) -> u64 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

/// The slabs of the op id column that searches for ops by id may decode
/// for each letter of a third collaborator's change a replica takes in,
/// typed at a random place of a text that two collaborators wrote in
/// `changes` changes of 16 letters at scattered places: how many more (by
/// the median) a change typing [`PLACES`] letters at as many places searches
/// than one typing a single letter, per added letter. Whatever every apply
/// searches regardless of its letters is in both and drops out.
fn letter_cost(changes: usize) -> u64 {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut base, busy, _) = scattered(changes, 16, &mut rng);
    let mut replica = AutoCommit::load(&base.save()).unwrap();
    let mut remote = base.fork().with_actor(ActorId::from(vec![0xc3; 16]));
    let mut edit = |places: u32| {
        let heads = remote.get_heads();
        for _ in 0..places {
            type_at_random(&mut remote, &busy, &mut rng, 1);
        }
        remote.commit();
        remote.save_after(&heads)
    };
    let probes: Vec<(Vec<u8>, Vec<u8>)> = (0..60).map(|_| (edit(1), edit(PLACES))).collect();
    let (mut one, mut many) = (Vec::new(), Vec::new());
    let searched = |replica: &mut AutoCommit, change: &[u8]| {
        let before = automerge::work().searched_slabs;
        replica.load_incoming(change).unwrap();
        automerge::work().searched_slabs - before
    };
    for (single, spread) in &probes {
        one.push(searched(&mut replica, single));
        many.push(searched(&mut replica, spread));
    }
    assert_eq!(replica.save(), remote.save());
    median(many).saturating_sub(median(one)) / u64::from(PLACES - 1)
}

#[test]
fn a_remote_letter_costs_no_more_after_a_long_scattered_history() {
    // 4,000 and 80,000 letters of history.
    let short = letter_cost(250);
    let long = letter_cost(5_000);
    // Without the index, finding a letter's place searched nearly every slab
    // of the document: about 25 µs a letter in the short text and 0.5 ms in
    // the long one, in an optimised build. With it, a few µs in either.
    assert!(
        long <= short * 2 + 4,
        "a remote letter searched {long} slabs after 80,000 letters of scattered history, \
         {short} after 4,000"
    );
}

#[test]
fn scattered_histories_match_upstream() {
    for seed in 1..=4u64 {
        let mut rng = Rng(seed.wrapping_mul(0x2545_f491_4f6c_dd1d) | 1);
        let (mut base, busy, fresh) = scattered(2_000, 4, &mut rng);
        let base_bytes = base.save();
        // Three collaborators type at once at random places, now and then
        // merging what the others typed.
        let mut actors: Vec<AutoCommit> = (1..=3u8)
            .map(|i| base.fork().with_actor(ActorId::from(vec![i; 16])))
            .collect();
        let mut changes = Vec::new();
        for _ in 0..600 {
            let a = rng.below(3) as usize;
            if rng.below(8) == 0 {
                let mut other = actors[(a + 1) % 3].fork();
                actors[a].merge_incoming(&mut other).unwrap();
                continue;
            }
            let heads = actors[a].get_heads();
            let text = if rng.below(6) == 0 { &fresh } else { &busy };
            let len = actors[a].length(text);
            if len > 0 && rng.below(4) == 0 {
                let at = rng.below(len as u64) as usize;
                actors[a].splice_text(text, at, 1, "").unwrap();
            } else {
                let letters = 1 + rng.below(3) as usize;
                type_at_random(&mut actors[a], text, &mut rng, letters);
            }
            actors[a].commit();
            changes.push(actors[a].save_after(&heads));
        }
        for diff in [false, true] {
            let replica = Replica {
                diff,
                encoding: TextEncoding::UnicodeCodePoint,
            };
            agrees_with_upstream(seed, &base_bytes, &changes, replica);
        }
    }
}
