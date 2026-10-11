//! Taking a change into a map seeks over the ops the change cannot affect
//! instead of walking every op of the map (vendor/automerge/TILO-PATCH.md):
//! the document, its heads and its patches are byte for byte what crates.io
//! automerge 0.12.0 produces from the same bytes, and the cost of a change
//! does not grow with the history behind it.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::num::NonZeroU64;
use std::time::{Duration, Instant};

use automerge::legacy::{Key, ObjectId, Op, OpId, OpType, SortedVec};
use automerge::transaction::Transactable;
use automerge::{
    ActorId, AutoCommit, Change, ChangeHash, ExpandedChange, ObjId, ObjType, ROOT, ReadDoc,
    ScalarValue, TextEncoding,
};
use tilo_automerge::ApplyIncoming;

mod upstream;
use upstream::{Replica, Rng, agrees_with_upstream};

const KEYS: &[&str] = &["a", "b", "desktop", "k1", "k2", "k3", "zz"];

/// A base document and the changes three actors make on top of it, in the
/// order they were made: puts that pile history onto a few keys, deletes,
/// counters and increments, nested maps, text, merges between the actors
/// (so writes conflict and later changes resolve them), and now and then a
/// hand-built change whose op overwrites an op under a different key.
fn history(seed: u64, steps: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut base = AutoCommit::new().with_actor(ActorId::from(&[0xaa][..]));
    base.put_object(ROOT, "m1", ObjType::Map).unwrap();
    base.put_object(ROOT, "m2", ObjType::Map).unwrap();
    base.put(ROOT, "c", ScalarValue::counter(0)).unwrap();
    let text = base.put_object(ROOT, "t", ObjType::Text).unwrap();
    base.splice_text(&text, 0, 0, "hello").unwrap();
    base.commit();
    let base_bytes = base.save();

    let mut actors: Vec<AutoCommit> = (1..=3u8)
        .map(|i| {
            AutoCommit::load(&base_bytes)
                .unwrap()
                .with_actor(ActorId::from(&[i][..]))
        })
        .collect();
    let mut changes = Vec::new();
    let mut forgers = 0u8;
    for step in 0..steps {
        let a = rng.below(actors.len() as u64) as usize;
        if rng.below(10) == 0 {
            let b = (a + 1 + rng.below(2) as usize) % actors.len();
            let mut other = actors[b].fork();
            actors[a].merge_incoming(&mut other).unwrap();
            continue;
        }
        // Only the replicas under test see a forged change: Automerge
        // rebuilds the changes it sends from its ops, and cannot rebuild
        // this one, so a peer that took it could not merge into another.
        if rng.below(40) == 0
            && let Some(change) = forged(&mut actors[a], &mut forgers)
        {
            changes.push(change.raw_bytes().to_vec());
            continue;
        }
        let heads = actors[a].get_heads();
        for _ in 0..=rng.below(3) {
            edit(&mut actors[a], &mut rng, step);
        }
        actors[a].commit();
        let bytes = actors[a].save_after(&heads);
        if !bytes.is_empty() {
            changes.push(bytes);
        }
    }
    (base_bytes, changes)
}

/// One local edit; edits the document cannot take (a counter that is not
/// one, an object another actor made) are simply not made.
fn edit(doc: &mut AutoCommit, rng: &mut Rng, step: usize) {
    let mut maps = vec![ROOT];
    for key in ["m1", "m2", "nested"] {
        if let Ok(Some((_, id))) = doc.get(ROOT, key) {
            maps.push(id);
        }
    }
    let obj = rng.pick(&maps).clone();
    let key = *rng.pick(KEYS);
    let _ = match rng.below(20) {
        0..=10 => doc.put(&obj, key, i64::try_from(step).unwrap()),
        11 => doc.put(&obj, key, format!("{{\"records\":[{step}]}}")),
        12 | 13 => doc.delete(&obj, key),
        14 => doc.put(
            &obj,
            key,
            ScalarValue::counter(i64::try_from(step).unwrap()),
        ),
        15 | 16 => doc.increment(&obj, if rng.below(2) == 0 { "c" } else { key }, 3),
        17 => doc.put_object(ROOT, "nested", ObjType::Map).map(|_| ()),
        18 => doc.put_object(&obj, key, ObjType::Map).map(|_| ()),
        _ => match doc.get(ROOT, "t") {
            Ok(Some((_, text))) => {
                let len = doc.length(&text);
                doc.splice_text(&text, rng.below(len as u64 + 1) as usize, 0, "x")
            }
            _ => Ok(()),
        },
    };
}

/// A change by a new actor, on top of `doc`, that puts a value at one
/// root key with the op at another root key as its predecessor: not a
/// change Automerge makes, but one a peer can send.
fn forged(doc: &mut AutoCommit, forgers: &mut u8) -> Option<Change> {
    let (key, target) = KEYS.iter().find_map(|&target| {
        let key = KEYS.iter().find(|&&key| key != target)?;
        match doc.get(ROOT, target).ok()?? {
            (_, ObjId::Id(counter, actor, _)) => Some((*key, OpId(counter, actor))),
            (_, ObjId::Root) => None,
        }
    })?;
    let deps: Vec<ChangeHash> = doc.get_heads();
    let max_op = deps
        .iter()
        .filter_map(|hash| doc.get_change_by_hash(hash).map(|c| c.max_op()))
        .max()?;
    *forgers += 1;
    let change = ExpandedChange {
        operations: vec![Op {
            action: OpType::Put(ScalarValue::Int(-1)),
            obj: ObjectId::Root,
            key: Key::Map(key.into()),
            pred: SortedVec::from(vec![target]),
            insert: false,
        }],
        actor_id: ActorId::from(&[0xf0, *forgers][..]),
        hash: None,
        seq: 1,
        start_op: NonZeroU64::new(max_op + 1)?,
        time: 0,
        message: None,
        deps,
        extra_bytes: Vec::new(),
        author: None,
    };
    Some(Change::from(change))
}

fn agrees(seed: u64, steps: usize, diff: bool) {
    let (base, changes) = history(seed, steps);
    let encoding = TextEncoding::UnicodeCodePoint;
    agrees_with_upstream(seed, &base, &changes, Replica { diff, encoding });
}

#[test]
fn incoming_map_changes_match_upstream() {
    for seed in 1..=6 {
        agrees(seed, 1_500, false);
    }
}

#[test]
fn incoming_map_patches_match_upstream() {
    for seed in 101..=104 {
        agrees(seed, 1_500, true);
    }
}

/// The fastest time of taking in a change that rewrites `"desktop"`, which
/// 20,000 changes rewrote before it, and of one that rewrites `"a"`, which
/// sorts before it and has next to no history, into the same replica.
fn overwrite_costs() -> (Duration, Duration) {
    let mut source = AutoCommit::new();
    let mut change = |key: &str, i: usize| {
        let heads = source.get_heads();
        source.put(ROOT, key, format!("{{\"rev\":{i}}}")).unwrap();
        source.commit();
        source.save_after(&heads)
    };
    let history: Vec<u8> = (0..20_000).flat_map(|i| change("desktop", i)).collect();
    let probes: Vec<(Vec<u8>, Vec<u8>)> = (0..60)
        .map(|i| (change("desktop", i), change("a", i)))
        .collect();
    let mut replica = AutoCommit::new();
    replica.load_incoming(&history).unwrap();
    let (mut busy, mut fresh) = (Duration::MAX, Duration::MAX);
    for (desktop, a) in &probes {
        let started = Instant::now();
        replica.load_incoming(desktop).unwrap();
        busy = busy.min(started.elapsed());
        let started = Instant::now();
        replica.load_incoming(a).unwrap();
        fresh = fresh.min(started.elapsed());
    }
    assert_eq!(replica.save(), source.save());
    (busy, fresh)
}

#[test]
fn a_map_overwrite_costs_no_more_after_a_long_history() {
    let (busy, fresh) = overwrite_costs();
    // The two changes cost the same but for walking the 20,000 overwritten
    // values of "desktop": about 1.7 ms in an optimised build. With debug
    // assertions on, Automerge also re-checks the order of every op after
    // each apply, which both pay alike.
    assert!(
        busy <= fresh * 3 / 2 + Duration::from_micros(50),
        "rewriting a key with 20,000 changes of history took {busy:?}, one without {fresh:?}"
    );
}
