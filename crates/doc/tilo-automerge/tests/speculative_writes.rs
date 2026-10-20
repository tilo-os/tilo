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

use std::time::{Duration, Instant};

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ObjId, ObjType, ROOT, ReadDoc};
use tilo_automerge::ApplyIncoming;

#[allow(
    dead_code,
    reason = "this file uses the shared random source only; it replays nothing against upstream"
)]
mod upstream;
use upstream::Rng;

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

/// Letters at a random place of `text`.
fn type_at_random(doc: &mut AutoCommit, text: &ObjId, rng: &mut Rng, letters: usize) {
    let at = rng.below(doc.length(text) as u64 + 1) as usize;
    let word: String = (0..letters)
        .map(|_| char::from(b'a' + rng.below(26) as u8))
        .collect();
    doc.splice_text(text, at, 0, &word).unwrap();
}

/// A text two collaborators wrote in `changes` changes of 16 letters at
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

/// What taking back a speculative letter adds, by the median, to a
/// replica's taking in a remote letter, in a text written in `changes`
/// scattered changes; and that the replica ends as the remote writer's
/// document. Remote letters are taken in alternately with and without a
/// letter of the replica's own to take back first, so what every apply
/// costs (such as the re-check of the whole document that debug assertions
/// add) drops out. The replica writes as an actor the document holds, as
/// `tilolang-tilo`'s store does: a transaction of an actor that has not
/// written adds it to the document and its rollback removes it, which
/// renumbers every op either way.
fn speculation_cost(changes: usize) -> Duration {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut base, text) = scattered(changes, &mut rng);
    let mut replica = AutoCommit::load(&base.save())
        .unwrap()
        .with_actor(ActorId::from(vec![0xa1; 16]));
    let mut remote = base.fork().with_actor(ActorId::from(vec![0xc3; 16]));
    let probes: Vec<Vec<u8>> = (0..80)
        .map(|_| {
            let heads = remote.get_heads();
            type_at_random(&mut remote, &text, &mut rng, 1);
            remote.commit();
            remote.save_after(&heads)
        })
        .collect();
    let (mut plain, mut speculated) = (Vec::new(), Vec::new());
    for (i, change) in probes.iter().enumerate() {
        let speculate = i % 2 == 1;
        if speculate {
            type_at_random(&mut replica, &text, &mut rng, 1);
        }
        let started = Instant::now();
        replica.rollback();
        replica.load_incoming(change).unwrap();
        let took = started.elapsed();
        if speculate {
            speculated.push(took);
        } else {
            plain.push(took);
        }
    }
    assert_eq!(replica.save(), remote.save());
    median(speculated).saturating_sub(median(plain))
}

#[test]
fn a_rolled_back_letter_does_not_make_the_next_remote_letter_search() {
    // 4,000 and 80,000 letters of history.
    let short = speculation_cost(250);
    let long = speculation_cost(5_000);
    // Dropping the index at every rollback had the next apply build it
    // again: about 2.5 ms at 80,000 ops in an optimised build.
    assert!(
        long <= short * 2 + Duration::from_micros(100),
        "taking back a letter added {long:?} to a remote letter after 80,000 letters of \
         scattered history, {short:?} after 4,000"
    );
}

/// The fastest of several empty commits, by an actor that has not written,
/// into a text two collaborators wrote in `changes` scattered changes (so
/// that the ops' actor column changes every few ops).
fn empty_commit(changes: usize) -> Duration {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let (mut doc, _) = scattered(changes, &mut rng);
    let mut reader = AutoCommit::load(&doc.save())
        .unwrap()
        .with_actor(ActorId::from(vec![0x01; 16]));
    let saved = reader.save();
    let mut fastest = Duration::MAX;
    for _ in 0..20 {
        let started = Instant::now();
        assert_eq!(reader.commit(), None);
        fastest = fastest.min(started.elapsed());
    }
    assert_eq!(
        reader.save(),
        saved,
        "an empty commit leaves the document as it was"
    );
    assert_eq!(reader.get_actor(), &ActorId::from(vec![0x01; 16]));
    fastest
}

#[test]
fn an_empty_commit_does_not_renumber_the_document() {
    // 4,000 and 80,000 letters of history.
    let short = empty_commit(250);
    let long = empty_commit(5_000);
    // Opening a transaction added the actor (rewriting every op's actor)
    // and the empty commit removed it again.
    assert!(
        long <= short * 10 + Duration::from_micros(50),
        "an empty commit cost {long:?} after 80,000 letters, {short:?} after 4,000"
    );
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
