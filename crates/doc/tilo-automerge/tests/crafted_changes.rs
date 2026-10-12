//! Changes a peer builds by hand, which no Automerge makes, are checked
//! before anything of them is applied (vendor/automerge/TILO-PATCH.md). One
//! the apply cannot place, or that would leave a document whose save no
//! longer loads, is refused with an error and the document stays byte for
//! byte as it was, where crates.io automerge 0.12.0 panics or takes it in
//! silently. A change Automerge makes is applied as upstream applies it.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::num::NonZeroU64;
use std::panic::{AssertUnwindSafe, catch_unwind};

use automerge::legacy::{ElementId, Key, ObjectId, Op, OpId, OpType, SortedVec};
use automerge::transaction::Transactable;
use automerge::{
    ActorId, AutoCommit, Change, ExpandedChange, LoadOptions, ObjId, ObjType, Patch, PatchAction,
    ROOT, ReadDoc, ScalarValue, TextEncoding,
};
use tilo_automerge::ApplyIncoming;

mod upstream;
use upstream::{History, Replica, Rng, agrees_with_upstream, refuses_only_what_upstream_corrupts};

/// A list `[1]` by actor `aa`: the list is op 1, its item op 2.
const LIST_BASE: &str = "856f4a83ae6ec86200760101aa0127bb838ff217b4f57fe5bfffa75af586b3da7e148ef1e872591780a55722fcc2060102030213022302400256020b01040204130415052102230234024203560357018001027f007f017f027f007f007f0700017f0000017f0100017f007f016c00010200020101017e02017e001401020000";
/// On `LIST_BASE`: a put on element 99@aa, which the list never held.
const UPDATE_OF_UNKNOWN: &str = "856f4a836ecbd837014f0127bb838ff217b4f57fe5bfffa75af586b3da7e148ef1e872591780a55722fcc202f001010300000101aa090102020211021303340142025602570170027f017f017f017fe300017f017f14057f00";
/// On `LIST_BASE`: an insert after element 99@aa.
const INSERT_AFTER_UNKNOWN: &str = "856f4a83d8e9acbb01500127bb838ff217b4f57fe5bfffa75af586b3da7e148ef1e872591780a55722fcc202f001010300000101aa090102020211021303340242025602570170027f017f017f017fe30000017f017f14057f00";
/// A text `"ab"` by actor `aa`: the text is op 1, `a` op 2, `b` op 3.
const TEXT_BASE: &str = "856f4a83ac2187e60080010101aa011a190725c9e01787574caaf60bf64d2dcea17d67eac3bb9aa2c9ca1318e3c015060102030213022302400256020c010402041104130515052102230234024204560457028001027f007f017f037f007f007f07000102000001020100027f0000017e00027f017400020300030101027f0402017f0002166162030000";
/// On `TEXT_BASE`: a second value `"Q"` on `a`, overwriting nothing.
const FORGED_VALUE: &str = "856f4a837108c8fa014e011a190725c9e01787574caaf60bf64d2dcea17d67eac3bb9aa2c9ca1318e3c01502f001010400000101aa090102020211021302340142025602570170027f017f017f017f02017f017f16517f00";
/// On `TEXT_BASE`: a delete of `a`, as a peer that has not seen
/// `FORGED_VALUE` makes it.
const HONEST_DELETE: &str = "856f4a838cdee0860153011a190725c9e01787574caaf60bf64d2dcea17d67eac3bb9aa2c9ca1318e3c01502f002010400000101aa0a01020202110213023401420256027002710273027f017f017f017f02017f037f007f017f017f02";

fn hex(text: &str) -> Vec<u8> {
    let digit = |c: u8| char::from(c).to_digit(16).unwrap() as u8;
    (text.as_bytes().chunks(2))
        .map(|pair| digit(pair[0]) << 4 | digit(pair[1]))
        .collect()
}

fn aa() -> ActorId {
    ActorId::from(&[0xaa][..])
}

fn id(counter: u64) -> OpId {
    OpId(counter, aa())
}

fn legacy(id: &ObjId) -> ObjectId {
    match id {
        ObjId::Id(counter, actor, _) => ObjectId::Id(OpId(*counter, actor.clone())),
        ObjId::Root => ObjectId::Root,
    }
}

fn elem(id: OpId) -> Key {
    Key::Seq(ElementId::Id(id))
}

fn op(obj: ObjectId, key: Key, action: OpType, pred: Vec<OpId>, insert: bool) -> Op {
    Op {
        action,
        obj,
        key,
        pred: SortedVec::from(pred),
        insert,
    }
}

fn put(obj: ObjectId, key: Key, pred: Vec<OpId>) -> Op {
    op(obj, key, OpType::Put(ScalarValue::Int(5)), pred, false)
}

/// A change by `actor`, the `seq`th of that actor, on top of `doc`.
fn change_at(doc: &mut AutoCommit, actor: &[u8], seq: u64, start: u64, ops: Vec<Op>) -> Vec<u8> {
    Change::from(ExpandedChange {
        operations: ops,
        actor_id: ActorId::from(actor),
        hash: None,
        seq,
        start_op: NonZeroU64::new(start).unwrap(),
        time: 0,
        message: None,
        deps: doc.get_heads(),
        extra_bytes: Vec::new(),
        author: None,
    })
    .raw_bytes()
    .to_vec()
}

/// The first change of the new actor `f0 n`, numbered from `start`.
fn change(doc: &mut AutoCommit, n: u8, start: u64, ops: Vec<Op>) -> Vec<u8> {
    change_at(doc, &[0xf0, n], 1, start, ops)
}

/// `bytes`, applied to a replica of `base`, are refused: an error, no
/// panic, and the document byte for byte as it was, its save loading. The
/// replica then takes a change Automerge makes exactly as upstream does.
#[expect(
    clippy::disallowed_methods,
    reason = "upstream automerge is the reference here, called as it ships"
)]
fn refused(base: &[u8], bytes: &[u8]) {
    let mut doc = AutoCommit::load(base).unwrap();
    let (before, heads) = (doc.save(), doc.get_heads());
    let result = catch_unwind(AssertUnwindSafe(|| doc.load_incoming(bytes)));
    assert!(
        matches!(result, Ok(Err(_))),
        "expected a refusal, got {result:?}"
    );
    assert_eq!(doc.get_heads(), heads);
    assert_eq!(doc.save(), before);
    AutoCommit::load(&doc.save()).unwrap();

    let mut source = AutoCommit::load(base)
        .unwrap()
        .with_actor(ActorId::from(&[0xee][..]));
    source.put(ROOT, "after", 1).unwrap();
    source.commit();
    let honest = source.save_after(&heads);
    let mut theirs = upstream_automerge::AutoCommit::load(base).unwrap();
    assert_eq!(
        doc.load_incoming(&honest).unwrap(),
        theirs.load_incremental(&honest).unwrap()
    );
    assert_eq!(doc.save(), theirs.save());
}

/// The list `[1]` of `LIST_BASE`, with the document it is in.
fn list_doc() -> (AutoCommit, ObjectId) {
    let mut doc = AutoCommit::load(&hex(LIST_BASE)).unwrap();
    let list = legacy(&doc.get(ROOT, "l").unwrap().unwrap().1);
    (doc.fork(), list)
}

#[test]
fn an_insert_after_an_unknown_element_is_refused() {
    let base = hex(LIST_BASE);
    refused(&base, &hex(INSERT_AFTER_UNKNOWN));
    let (mut doc, list) = list_doc();
    let insert_after = |parent| op(list.clone(), parent, OpType::Put(5.into()), vec![], true);
    // The list object itself, an op the change makes after it, and itself.
    for parent in [elem(id(1)), elem(OpId(4, ActorId::from(&[0xf0, 1][..])))] {
        refused(&base, &change(&mut doc, 1, 3, vec![insert_after(parent)]));
    }
    let own = elem(OpId(3, ActorId::from(&[0xf0, 1][..])));
    refused(&base, &change(&mut doc, 1, 3, vec![insert_after(own)]));
    // An insert after one the same change makes later.
    let later = elem(OpId(4, ActorId::from(&[0xf0, 1][..])));
    let head = op(
        list.clone(),
        Key::Seq(ElementId::Head),
        OpType::Put(1.into()),
        vec![],
        true,
    );
    refused(
        &base,
        &change(&mut doc, 1, 3, vec![insert_after(later), head]),
    );
}

#[test]
fn an_update_of_an_unknown_element_is_refused() {
    let base = hex(LIST_BASE);
    refused(&base, &hex(UPDATE_OF_UNKNOWN));
    let (mut doc, list) = list_doc();
    let ghost = put(list.clone(), elem(id(99)), vec![]);
    // Alone, and beside a put on the list's real item in the same change.
    let item = put(list.clone(), elem(id(2)), vec![id(2)]);
    refused(&base, &change(&mut doc, 1, 3, vec![item, ghost]));
    for key in [Key::Seq(ElementId::Head), elem(id(1))] {
        refused(
            &base,
            &change(&mut doc, 1, 3, vec![put(list.clone(), key, vec![])]),
        );
    }
}

/// `text` as UTF-16 code units, the unit patches count in here.
fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// Apply the text patches on `obj` to `text`.
fn replay(text: &mut Vec<u16>, patches: &[Patch], obj: &ObjId) {
    for patch in patches.iter().filter(|p| &p.obj == obj) {
        match &patch.action {
            PatchAction::SpliceText { index, value, .. } => {
                let inserted = units(&value.make_string());
                text.splice(*index..*index, inserted);
            }
            PatchAction::DeleteSeq { index, length } => {
                text.drain(*index..*index + *length);
            }
            PatchAction::Conflict { .. } | PatchAction::Mark { .. } => {}
            other => panic!("unexpected text patch {other:?}"),
        }
    }
}

fn utf16(base: &[u8]) -> AutoCommit {
    let options = LoadOptions::new().text_encoding(TextEncoding::Utf16CodeUnit);
    AutoCommit::load_with_options(base, options).unwrap()
}

/// A peer puts `value` as a second value on the `a` of `"ab"`, overwriting
/// nothing, and another, who has not seen it, deletes the `a`.
fn forged_value_then_delete(value: &str) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut doc = AutoCommit::new().with_actor(aa());
    let text = doc.put_object(ROOT, "t", ObjType::Text).unwrap();
    doc.splice_text(&text, 0, 0, "ab").unwrap();
    doc.commit();
    let base = doc.save();
    let text = legacy(&text);
    let set = OpType::Put(ScalarValue::Str(value.into()));
    let forged = change(
        &mut doc,
        1,
        4,
        vec![op(text.clone(), elem(id(2)), set, vec![], false)],
    );
    let delete = op(text, elem(id(2)), OpType::Delete, vec![id(2)], false);
    let honest = change(&mut doc, 2, 4, vec![delete]);
    (base, forged, honest)
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "upstream automerge is the reference here, called as it ships"
)]
fn a_concurrent_delete_after_a_forged_text_value_applies() {
    let reproduced = (hex(TEXT_BASE), hex(FORGED_VALUE), hex(HONEST_DELETE));
    let built = ["Q", "😀"].map(forged_value_then_delete);
    for (base, forged, honest) in std::iter::once(reproduced).chain(built) {
        // The other order, in which upstream applies both.
        let mut fresh = utf16(&base);
        fresh.load_incoming(&honest).unwrap();
        fresh.load_incoming(&forged).unwrap();
        let mut theirs = upstream_automerge::AutoCommit::load(&base).unwrap();
        theirs.load_incremental(&honest).unwrap();
        theirs.load_incremental(&forged).unwrap();
        assert_eq!(fresh.save(), theirs.save());
        let text = fresh.get(ROOT, "t").unwrap().unwrap().1;
        let expected = fresh.text(&text).unwrap();

        for diff in [false, true] {
            let mut doc = utf16(&base);
            doc.load_incoming(&forged).unwrap();
            let mut shown = units(&doc.text(&text).unwrap());
            if diff {
                doc.update_diff_cursor();
            }
            doc.load_incoming(&honest).unwrap();
            assert_eq!(doc.text(&text).unwrap(), expected);
            assert_eq!(doc.get_heads(), fresh.get_heads());
            AutoCommit::load(&doc.save()).unwrap();
            if diff {
                replay(&mut shown, &doc.diff_incremental(), &text);
                assert_eq!(String::from_utf16(&shown).unwrap(), expected);
            }
        }
    }
}

#[test]
fn an_op_outside_any_object_or_keyed_against_its_object_is_refused() {
    let (mut doc, list) = list_doc();
    let base = doc.save();
    let map = legacy(&doc.put_object(ROOT, "m", ObjType::Map).unwrap());
    doc.put(ROOT, "k", 1).unwrap();
    doc.commit();
    let with_map = doc.save();
    let cases = [
        (
            &base,
            put(ObjectId::Id(id(50)), Key::Map("x".into()), vec![]),
        ),
        (
            &base,
            put(ObjectId::Id(id(2)), Key::Map("x".into()), vec![]),
        ),
        (&base, put(list, Key::Map("x".into()), vec![])),
        (&with_map, put(map, elem(id(2)), vec![])),
        (
            &with_map,
            put(ObjectId::Root, Key::Seq(ElementId::Head), vec![]),
        ),
    ];
    for (base, op) in cases {
        let mut doc = AutoCommit::load(base).unwrap();
        let start = doc
            .get_changes(&[])
            .iter()
            .map(Change::max_op)
            .max()
            .unwrap()
            + 1;
        refused(base, &change(&mut doc, 1, start, vec![op]));
    }
}

#[test]
fn an_overwrite_outside_the_op_s_own_key_or_element_is_refused() {
    let mut doc = AutoCommit::new().with_actor(aa());
    let list = doc.put_object(ROOT, "l", ObjType::List).unwrap(); // 1
    doc.insert(&list, 0, 1).unwrap(); // 2
    doc.insert(&list, 1, 2).unwrap(); // 3
    let map = doc.put_object(ROOT, "m", ObjType::Map).unwrap(); // 4
    doc.put(&map, "k", 1).unwrap(); // 5
    doc.commit();
    let base = doc.save();
    let (list, map) = (legacy(&list), legacy(&map));
    let delete = |pred| op(list.clone(), elem(id(2)), OpType::Delete, pred, false);
    let cases = [
        put(map.clone(), Key::Map("z".into()), vec![id(5)]),
        put(map.clone(), Key::Map("a".into()), vec![id(5)]),
        put(ObjectId::Root, Key::Map("zz".into()), vec![id(1)]),
        put(map.clone(), Key::Map("k".into()), vec![id(2)]),
        put(list.clone(), elem(id(2)), vec![id(3)]),
        put(list.clone(), elem(id(3)), vec![id(2)]),
        put(
            map.clone(),
            Key::Map("k".into()),
            vec![OpId(5, ActorId::from(&[0xf0, 1][..]))],
        ),
        put(
            map,
            Key::Map("k".into()),
            vec![OpId(6, ActorId::from(&[0xf0, 1][..]))],
        ),
        delete(vec![]),
        op(
            list.clone(),
            Key::Seq(ElementId::Head),
            OpType::Delete,
            vec![],
            true,
        ),
    ];
    for op in cases {
        refused(&base, &change(&mut doc, 1, 6, vec![op]));
    }
}

#[test]
fn a_change_numbered_out_of_its_actor_s_order_is_refused() {
    let (mut doc, _) = list_doc();
    let base = doc.save();
    let map_put = || put(ObjectId::Root, Key::Map("q".into()), vec![]);
    // A gap in aa's sequence, a second change of an actor without a first,
    // ops numbered at or below what the change follows, and an op naming an
    // actor the document has never seen.
    refused(&base, &change_at(&mut doc, &[0xaa], 3, 3, vec![map_put()]));
    refused(
        &base,
        &change_at(&mut doc, &[0xf0, 1], 2, 3, vec![map_put()]),
    );
    refused(&base, &change_at(&mut doc, &[0xaa], 2, 2, vec![map_put()]));
    refused(&base, &change(&mut doc, 1, 1, vec![map_put()]));
    let stranger = OpId(2, ActorId::from(&[0xbb][..]));
    let list = legacy(&doc.get(ROOT, "l").unwrap().unwrap().1);
    refused(
        &base,
        &change(&mut doc, 1, 3, vec![put(list, elem(stranger), vec![])]),
    );
}

/// A peer's honest edit of `base` and a refused change, delivered together:
/// neither is applied. The honest one, delivered alone, then is.
#[test]
fn a_refused_change_takes_its_whole_delivery_with_it() {
    let base = hex(LIST_BASE);
    let mut peer = AutoCommit::load(&base)
        .unwrap()
        .with_actor(ActorId::from(&[0xee][..]));
    let heads = peer.get_heads();
    peer.put(ROOT, "x", 1).unwrap();
    peer.commit();
    let honest = peer.save_after(&heads);
    let both = [honest.clone(), hex(INSERT_AFTER_UNKNOWN)].concat();
    let mut doc = AutoCommit::load(&base).unwrap();
    assert!(doc.load_incoming(&both).is_err());
    assert_eq!(doc.save(), base);
    doc.load_incoming(&honest).unwrap();
    assert_eq!(doc.save(), peer.save());
}

/// A refused change that arrived before its dependency waits in the queue:
/// once the dependency arrives it is dropped, with whatever depends on it,
/// and the dependency is applied. One that arrives with the dependency
/// refuses the delivery and leaves the queue as it was.
#[test]
fn a_refused_change_waiting_for_its_dependency_holds_back_nothing() {
    let base = hex(LIST_BASE);
    let mut peer = AutoCommit::load(&base)
        .unwrap()
        .with_actor(ActorId::from(&[0xee][..]));
    let heads = peer.get_heads();
    peer.put(ROOT, "x", 1).unwrap();
    peer.commit();
    let dependency = peer.save_after(&heads);
    let list = legacy(&peer.get(ROOT, "l").unwrap().unwrap().1);
    let ghost = put(list, elem(id(1)), vec![]);
    let waiting = change(&mut peer, 1, 4, vec![ghost]);

    let mut doc = AutoCommit::load(&base).unwrap();
    assert_eq!(doc.load_incoming(&waiting).unwrap(), 0);
    let missing = doc.get_missing_deps(&[]);
    assert!(!missing.is_empty());
    let mut queued = doc.clone();
    queued.load_incoming(&dependency).unwrap();
    let mut honest = AutoCommit::load(&base).unwrap();
    honest.load_incoming(&dependency).unwrap();
    assert_eq!(queued.save(), honest.save());
    assert!(queued.get_missing_deps(&[]).is_empty());
    AutoCommit::load(&queued.save()).unwrap();

    let mut together = doc.clone();
    let delivery = [dependency, hex(INSERT_AFTER_UNKNOWN)].concat();
    assert!(together.load_incoming(&delivery).is_err());
    // A save carries the changes still waiting.
    assert_eq!(together.save(), doc.save());
    assert_eq!(together.get_missing_deps(&[]), missing);
}

/// Three actors edit a map, a list and a text, merging now and then.
fn small_history(seed: u64) -> (Vec<u8>, Vec<Change>) {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut base = AutoCommit::new().with_actor(aa());
    base.put_object(ROOT, "m", ObjType::Map).unwrap();
    let list = base.put_object(ROOT, "l", ObjType::List).unwrap();
    base.insert(&list, 0, 0).unwrap();
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
    for step in 0..60i64 {
        let a = rng.below(3) as usize;
        if rng.below(6) == 0 {
            let mut other = actors[(a + 1) % 3].fork();
            actors[a].merge_incoming(&mut other).unwrap();
            continue;
        }
        let doc = &mut actors[a];
        let heads = doc.get_heads();
        let get = |doc: &AutoCommit, key| doc.get(ROOT, key).unwrap().unwrap().1;
        let (map, list, text) = (get(doc, "m"), get(doc, "l"), get(doc, "t"));
        let (list_len, text_len) = (doc.length(&list) as u64, doc.length(&text) as u64);
        let _ = match rng.below(8) {
            0 => doc.put(&map, *rng.pick(&["a", "b", "c"]), step),
            1 => doc.delete(&map, *rng.pick(&["a", "b", "c"])),
            2 => doc.insert(&list, rng.below(list_len + 1) as usize, step),
            3 if list_len > 0 => doc.put(&list, rng.below(list_len) as usize, -step),
            4 if list_len > 0 => doc.delete(&list, rng.below(list_len) as usize),
            5 => doc.splice_text(&text, rng.below(text_len + 1) as usize, 0, "xy"),
            6 if text_len > 0 => doc.splice_text(&text, rng.below(text_len) as usize, 1, ""),
            _ => doc.put(&map, "n", ScalarValue::counter(step)),
        };
        doc.commit();
        changes.extend(doc.get_changes(&heads));
    }
    (base_bytes, changes)
}

/// Every op id `changes` make, every actor they come from, and a few of
/// neither.
fn names(changes: &[Change]) -> (Vec<OpId>, Vec<ActorId>) {
    let mut ids = vec![id(1), id(2), id(3)];
    let mut actors = vec![aa(), ActorId::from(&[0xf0, 9][..])];
    for c in changes {
        let c = c.decode();
        for i in 0..c.operations.len() as u64 {
            ids.push(OpId(c.start_op.get() + i, c.actor_id.clone()));
        }
        actors.push(c.actor_id.clone());
        ids.push(OpId(c.start_op.get() + 500, c.actor_id));
    }
    (ids, actors)
}

/// One or two random edits of a change's fields.
fn mutate(c: &mut ExpandedChange, rng: &mut Rng, ids: &[OpId], actors: &[ActorId]) {
    for _ in 0..=rng.below(2) {
        let n = c.operations.len();
        let pick = rng.below(n.max(1) as u64) as usize;
        let random_id = rng.pick(ids).clone();
        let what = rng.below(16);
        if n == 0 && what < 10 {
            continue;
        }
        match what {
            0 => c.operations[pick].obj = ObjectId::Id(random_id),
            1 => c.operations[pick].obj = ObjectId::Root,
            2 => c.operations[pick].key = elem(random_id),
            3 => c.operations[pick].key = Key::Seq(ElementId::Head),
            4 => c.operations[pick].key = Key::Map((*rng.pick(&["a", "k", "n"])).into()),
            5 => c.operations[pick].insert = !c.operations[pick].insert,
            6 => {
                c.operations[pick].action = match rng.below(5) {
                    0 => OpType::Delete,
                    1 => OpType::Increment(1),
                    2 => OpType::Make(*rng.pick(&[ObjType::Map, ObjType::List, ObjType::Text])),
                    3 => OpType::Put(ScalarValue::Str("z".into())),
                    _ => OpType::Put(ScalarValue::counter(1)),
                }
            }
            7 => {
                let mut pred: Vec<OpId> = c.operations[pick].pred.iter().cloned().collect();
                pred.push(random_id);
                c.operations[pick].pred = SortedVec::from(pred);
            }
            8 => c.operations[pick].pred = SortedVec::from(vec![]),
            9 => {
                let copy = c.operations[pick].clone();
                c.operations.insert(pick, copy);
            }
            10 => {
                if n > 0 {
                    c.operations.remove(pick);
                }
            }
            11 => {
                let start = c.start_op.get() + rng.below(5);
                c.start_op = NonZeroU64::new(start.saturating_sub(2).max(1)).unwrap();
            }
            12 => c.seq = (c.seq + rng.below(3)).saturating_sub(1).max(1),
            13 => {
                c.deps.pop();
            }
            14 => c.actor_id = rng.pick(actors).clone(),
            _ => c.time += 1,
        }
    }
}

/// `count` valid changes of `changes`, each with a field or two changed at
/// random, and the index of the change each was made from.
fn mutated(seed: u64, changes: &[Change], count: usize) -> Vec<(usize, Vec<u8>)> {
    let (ids, actors) = names(changes);
    let mut rng = Rng(seed ^ 0xfeed);
    let mut out = Vec::new();
    while out.len() < count {
        let k = rng.below(changes.len() as u64) as usize;
        let mut expanded = changes[k].decode();
        mutate(&mut expanded, &mut rng, &ids, &actors);
        expanded.hash = None;
        // Some edits make a change the encoder itself cannot write.
        if let Ok(change) = catch_unwind(AssertUnwindSafe(|| Change::from(expanded))) {
            out.push((k, change.raw_bytes().to_vec()));
        }
    }
    out
}

/// Valid changes with a field or two changed at random. The valid ones are
/// applied as upstream applies them. Each mutated one, delivered to the
/// replica that has everything before the change it was made from, with
/// and without a diff cursor, never panics the fork, and leaves a document
/// that saves and loads; without a cursor, the fork takes it as upstream
/// does, or refuses it and keeps its document byte for byte, where upstream
/// panics or takes it into a document that no longer loads.
#[test]
fn a_mutated_change_never_panics_or_corrupts_the_document() {
    let (mut refused, mut total) = (0, 0);
    for seed in 1..=8u64 {
        let (base, changes) = small_history(seed);
        let valid: Vec<Vec<u8>> = changes.iter().map(|c| c.raw_bytes().to_vec()).collect();
        for diff in [false, true] {
            let encoding = TextEncoding::UnicodeCodePoint;
            agrees_with_upstream(seed, &base, &valid, Replica { diff, encoding });
        }
        let mut replica = AutoCommit::load(&base).unwrap();
        let mut before = Vec::new();
        for change in &changes {
            before.push(replica.clone());
            replica.apply_incoming([change.clone()]).unwrap();
        }
        let forged = mutated(seed, &changes, 300);
        for (n, (k, bytes)) in forged.iter().enumerate() {
            let diff = n % 2 == 0;
            let mut doc = before[*k].clone();
            if diff {
                doc.update_diff_cursor();
            }
            let saved = catch_unwind(AssertUnwindSafe(|| {
                let _ = doc.load_incoming(bytes);
                if diff {
                    doc.diff_incremental();
                }
                doc.save()
            }));
            let Ok(saved) = saved else {
                panic!(
                    "seed {seed}: the fork panicked on {:?}, made from {:?}",
                    Change::try_from(bytes.as_slice()).map(|c| c.decode()),
                    changes[*k].decode()
                );
            };
            let reload = catch_unwind(AssertUnwindSafe(|| {
                AutoCommit::load(&saved)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }));
            assert!(
                matches!(reload, Ok(Ok(()))),
                "seed {seed}: took {:?} into a document that does not load: {reload:?}",
                Change::try_from(bytes.as_slice()).map(|c| c.decode())
            );
        }
        for (k, bytes) in forged {
            let history = History {
                base: base.clone(),
                changes: valid[..k].to_vec(),
                forged: vec![bytes],
            };
            total += 1;
            refused += refuses_only_what_upstream_corrupts(seed, &history);
        }
    }
    assert!(
        refused > 100,
        "only {refused} of {total} mutated changes refused"
    );
    assert!(
        total - refused > 100,
        "only {} of {total} mutated changes taken",
        total - refused
    );
}

/// The reproducers' bytes are the changes the tests above build.
#[test]
fn the_reproducer_bytes_are_well_formed() {
    let base = hex(LIST_BASE);
    for change in [UPDATE_OF_UNKNOWN, INSERT_AFTER_UNKNOWN] {
        let change = Change::try_from(hex(change).as_slice()).unwrap();
        let mut doc = AutoCommit::load(&base).unwrap();
        assert_eq!(change.deps(), doc.get_heads().as_slice());
    }
    let base = hex(TEXT_BASE);
    for change in [FORGED_VALUE, HONEST_DELETE] {
        let change = Change::try_from(hex(change).as_slice()).unwrap();
        let mut doc = AutoCommit::load(&base).unwrap();
        assert_eq!(change.deps(), doc.get_heads().as_slice());
    }
}
