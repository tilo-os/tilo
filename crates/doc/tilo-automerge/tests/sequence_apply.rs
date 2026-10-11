//! Taking a change into a list or a text seeks over the elements the change
//! cannot affect instead of walking every op the sequence ever held
//! (vendor/automerge/TILO-PATCH.md): the document, its heads and its patches
//! (text patches with their marks, list patches with their conflicts) are
//! byte for byte what crates.io automerge 0.12.0 produces from the same
//! bytes, and the cost of a remote keystroke does not grow with the history
//! behind it.

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

use automerge::legacy::{ElementId, Key, ObjectId, Op, OpId, OpType, SortedVec};
use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable;
use automerge::{
    ActorId, AutoCommit, Change, ChangeHash, ExpandedChange, ObjId, ObjType, ROOT, ReadDoc,
    ScalarValue, TextEncoding,
};
use tilo_automerge::ApplyIncoming;

mod upstream;
use upstream::{Replica, Rng, agrees_with_upstream};

const SNIPPETS: &[&str] = &["x", "yz", "é", "😀", "\n", "e\u{301}", "abc"];
const MARKS: &[&str] = &["bold", "link"];
const EXPANDS: &[ExpandMark] = &[
    ExpandMark::Before,
    ExpandMark::After,
    ExpandMark::Both,
    ExpandMark::None,
];

/// A base document and the changes three actors make on top of it, in the
/// order they were made: text typed, deleted and replaced at random places,
/// with multi-unit characters, marks and unmarks, block markers; list items
/// inserted, set and deleted, counters incremented, maps nested in a list;
/// merges between the actors (so inserts at the same place race and later
/// changes resolve conflicts), and now and then a hand-built change whose op
/// overwrites an op of another element or another object.
fn history(seed: u64, steps: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut base = AutoCommit::new().with_actor(ActorId::from(&[0xaa][..]));
    let text = base.put_object(ROOT, "t", ObjType::Text).unwrap();
    base.splice_text(&text, 0, 0, "hello world").unwrap();
    let plain = base.put_object(ROOT, "t2", ObjType::Text).unwrap();
    base.splice_text(&plain, 0, 0, "ab").unwrap();
    let list = base.put_object(ROOT, "l", ObjType::List).unwrap();
    for i in 0..3 {
        base.insert(&list, i, i64::try_from(i).unwrap()).unwrap();
    }
    base.put_object(ROOT, "m", ObjType::Map).unwrap();
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
            && let Some(change) = forged(&mut actors[a], &mut rng, &mut forgers)
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

fn obj(doc: &AutoCommit, key: &str) -> ObjId {
    doc.get(ROOT, key).unwrap().unwrap().1
}

/// One local edit; edits the document cannot take (incrementing what is
/// not a counter, a range past the end) are simply not made.
fn edit(doc: &mut AutoCommit, rng: &mut Rng, step: usize) {
    // Marks go into "t" only: the fork keeps each text's mark changes in a
    // patch of its own where crates.io automerge 0.12.0 folds them into one
    // (TILO-PATCH.md), so the two agree on mark patches of one text only.
    let marked = rng.below(4) != 0;
    let text = obj(doc, if marked { "t" } else { "t2" });
    let list = obj(doc, "l");
    let (text_len, list_len) = (doc.length(&text) as u64, doc.length(&list) as u64);
    let at = |rng: &mut Rng, len: u64| rng.below(len + 1) as usize;
    let n = i64::try_from(step).unwrap();
    let _ = match rng.below(24) {
        0..=6 => doc.splice_text(&text, at(rng, text_len), 0, rng.pick(SNIPPETS)),
        7..=9 => {
            let pos = at(rng, text_len);
            let del = (1 + rng.below(3) as usize).min(text_len as usize - pos);
            doc.splice_text(&text, pos, isize::try_from(del).unwrap(), "")
        }
        10 => {
            let pos = at(rng, text_len);
            doc.splice_text(&text, pos, 1, rng.pick(SNIPPETS))
        }
        11 | 12 if marked && text_len > 0 => {
            let start = rng.below(text_len) as usize;
            let end = start + 1 + rng.below(text_len - start as u64) as usize;
            let name = rng.pick(MARKS).to_string();
            let expand = *rng.pick(EXPANDS);
            if rng.below(3) == 0 {
                doc.unmark(&text, &name, start, end, expand)
            } else {
                let value = if rng.below(2) == 0 {
                    ScalarValue::from(true)
                } else {
                    ScalarValue::from(format!("v{}", rng.below(3)))
                };
                doc.mark(&text, Mark::new(name, value, start, end), expand)
            }
        }
        13 => doc.split_block(&text, at(rng, text_len)).map(|_| ()),
        14..=16 => doc.insert(&list, at(rng, list_len), n),
        17 if list_len > 0 => doc.put(&list, rng.below(list_len) as usize, -n),
        18 if list_len > 0 => doc.delete(&list, rng.below(list_len) as usize),
        19 => doc.insert(&list, at(rng, list_len), ScalarValue::counter(n)),
        20 if list_len > 0 => doc.increment(&list, rng.below(list_len) as usize, 2),
        21 => doc
            .insert_object(&list, at(rng, list_len), ObjType::Map)
            .and_then(|map| doc.put(&map, "n", n)),
        22 => {
            let map = obj(doc, "m");
            doc.put(&map, "k", n)
        }
        _ => doc.splice_text(&text, text_len as usize, 0, "z"),
    };
}

fn legacy_id(id: &ObjId) -> Option<OpId> {
    match id {
        ObjId::Id(counter, actor, _) => Some(OpId(*counter, actor.clone())),
        ObjId::Root => None,
    }
}

/// Every item the list `seq` ever held, deleted ones too: the ids of the
/// insert ops in its history.
fn elements(doc: &mut AutoCommit, seq: &ObjId) -> Vec<OpId> {
    let Some(seq) = legacy_id(seq) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for change in doc.get_changes(&[]) {
        let change = change.decode();
        for (i, op) in change.operations.iter().enumerate() {
            if op.insert && op.obj == ObjectId::Id(seq.clone()) {
                let counter = change.start_op.get() + i as u64;
                ids.push(OpId(counter, change.actor_id.clone()));
            }
        }
    }
    ids
}

/// A change by a new actor, on top of `doc`, that sets an item of the list
/// (maybe a deleted one) with the op of a list item, or of a character of
/// the text, as its predecessor. Not a change Automerge makes, but one a
/// peer can send. (Setting a character this way makes crates.io automerge
/// 0.12.0 itself panic once one of the character's two values is deleted,
/// so the forged op sets a list item.)
fn forged(doc: &mut AutoCommit, rng: &mut Rng, forgers: &mut u8) -> Option<Change> {
    let list = obj(doc, "l");
    let pred_seq = if rng.below(2) == 0 {
        list.clone()
    } else {
        obj(doc, "t")
    };
    let len = doc.length(&pred_seq);
    if len == 0 {
        return None;
    }
    let pred = legacy_id(&doc.get(&pred_seq, rng.below(len as u64) as usize).ok()??.1)?;
    let target = rng.pick(&elements(doc, &list)).clone();
    let (seq, value) = (list, ScalarValue::Int(-1));
    let deps: Vec<ChangeHash> = doc.get_heads();
    let max_op = deps
        .iter()
        .filter_map(|hash| doc.get_change_by_hash(hash).map(|c| c.max_op()))
        .max()?;
    *forgers += 1;
    let change = ExpandedChange {
        operations: vec![Op {
            action: OpType::Put(value),
            obj: ObjectId::Id(legacy_id(&seq)?),
            key: Key::Seq(ElementId::Id(target)),
            pred: SortedVec::from(vec![pred]),
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

fn agrees(seed: u64, diff: bool, encoding: TextEncoding) {
    let (base, changes) = history(seed, 1_500);
    agrees_with_upstream(seed, &base, &changes, Replica { diff, encoding });
}

#[test]
fn incoming_sequence_changes_match_upstream() {
    for seed in 1..=6 {
        agrees(seed, false, TextEncoding::UnicodeCodePoint);
    }
}

#[test]
fn incoming_sequence_patches_match_upstream() {
    let encodings = [
        TextEncoding::UnicodeCodePoint,
        TextEncoding::Utf16CodeUnit,
        TextEncoding::Utf8CodeUnit,
        TextEncoding::GraphemeCluster,
    ];
    for (seed, encoding) in (101..).zip(encodings) {
        agrees(seed, true, encoding);
    }
}

/// The fastest time of taking in an edit into the middle of a sequence that
/// 20,000 edits (a quarter of them deletions) built before it, and of one
/// into a sequence with next to no history, into the same replica.
fn edit_costs(ty: ObjType) -> (Duration, Duration) {
    let mut source = AutoCommit::new();
    let busy = source.put_object(ROOT, "busy", ty).unwrap();
    let fresh = source.put_object(ROOT, "fresh", ty).unwrap();
    source.commit();
    let mut history = source.save();
    let mut change = |seq: &ObjId, i: usize| {
        let heads = source.get_heads();
        let len = source.length(seq);
        match ty {
            ObjType::List if i % 4 == 3 && len > 0 => source.delete(seq, len / 2).unwrap(),
            ObjType::List => source
                .insert(seq, len / 2, i64::try_from(i).unwrap())
                .unwrap(),
            _ if i % 4 == 3 && len > 0 => source.splice_text(seq, len / 2, 1, "").unwrap(),
            _ => source.splice_text(seq, len / 2, 0, "x").unwrap(),
        }
        source.commit();
        source.save_after(&heads)
    };
    history.extend((0..20_000).flat_map(|i| change(&busy, i)));
    let probes: Vec<(Vec<u8>, Vec<u8>)> = (0..60)
        .map(|i| (change(&busy, 4 * i), change(&fresh, 4 * i)))
        .collect();
    let mut replica = AutoCommit::new();
    replica.load_incoming(&history).unwrap();
    let (mut slow, mut quick) = (Duration::MAX, Duration::MAX);
    for (busy, fresh) in &probes {
        let started = Instant::now();
        replica.load_incoming(busy).unwrap();
        slow = slow.min(started.elapsed());
        let started = Instant::now();
        replica.load_incoming(fresh).unwrap();
        quick = quick.min(started.elapsed());
    }
    assert_eq!(replica.save(), source.save());
    (slow, quick)
}

#[test]
fn a_remote_keystroke_costs_no_more_after_a_long_history() {
    for ty in [ObjType::Text, ObjType::List] {
        let (busy, fresh) = edit_costs(ty);
        // The two edits cost the same but for walking the 20,000 ops of the
        // busy sequence: about 1.8 ms in an optimised build. With debug
        // assertions on, Automerge also re-checks the order of every op
        // after each apply, which both pay alike.
        assert!(
            busy <= fresh * 3 / 2 + Duration::from_micros(50),
            "an edit into a {ty:?} with 20,000 edits of history took {busy:?}, one without {fresh:?}"
        );
    }
}
