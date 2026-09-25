use super::*;
use automerge::resource_limits::{
    NativeAllocationSet, NativeAllowance, NativeCloneCandidate, NativeResourceError,
};
use automerge::{
    ObjType, ReadDoc,
    marks::{ExpandMark, Mark},
};
fn allowance() -> NativeAllowance {
    NativeAllowance::new(64 * 1024 * 1024).unwrap()
}
fn cloned(doc: &AutoCommit) -> (NativeCloneCandidate<AutoCommit>, NativeAllocationSet) {
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let turn = allocations.work_turn(allowance()).unwrap();
    let candidate = doc.clone_bounded(&mut allocations, &turn).unwrap();
    (candidate, allocations)
}

#[test]
fn semantic_native_writer_matches_real_marked_pages_and_full_capacity() {
    for large in [false, true] {
        let content = "\"\n界🦀".repeat(700);
        let mut doc = if large {
            fixture(&[&content, "tail"])
        } else {
            fixture(&["short"; 70])
        };
        if large {
            let c = qdoc::model::open(&doc, &ROOT).unwrap();
            let id = qdoc::model::blocks_in_order(&doc, &c)[0].clone();
            let block = doc.get(&c.blocks, id).unwrap().unwrap().1;
            let text = doc.get(&block, "text").unwrap().unwrap().1;
            doc.split_block(&text, 17).unwrap();
            doc.mark(
                &text,
                Mark::new("bold".into(), true, 8, 100),
                ExpandMark::Both,
            )
            .unwrap();
            doc.mark(
                &text,
                Mark::new("font".into(), "NOT CONTENT", 50, 200),
                ExpandMark::Both,
            )
            .unwrap();
            doc.commit();
        }
        let heads = doc.base_heads();
        let (candidate, allocations) = cloned(&doc);
        let current = allowance();
        let turn = allocations.work_turn(current.clone()).unwrap();
        let read = candidate.read_unqualified(&turn).unwrap();
        let before = current.available_bytes();
        let limits = Limits {
            items: 64,
            json_bytes: if large { 4096 } else { 65536 },
            scan_entries: 100_000,
        };
        let mut cursor = None;
        for page_index in 0..100 {
            let expected = read_text(&doc, &heads, cursor.as_ref(), limits).unwrap();
            let (actual, peak, largest) = allocation_meter::precharged(&current, || {
                read_text_native(&read, read.root(), cursor.as_ref(), limits)
            });
            let actual = actual.unwrap();
            assert!(peak < 512 * 1024 && largest < 64 * 1024);
            assert_eq!(
                serde_json::to_value(actual.page()).unwrap(),
                serde_json::to_value(&expected).unwrap()
            );
            assert!(current.available_bytes() < before);
            if !large && page_index == 0 {
                assert_eq!(actual.page().segments.len(), 64);
            }
            cursor = expected.next;
            drop(actual);
            assert_eq!(current.available_bytes(), before);
            if cursor.is_none() {
                break;
            }
        }
        assert!(cursor.is_none());
        assert_eq!(doc.base_heads(), heads);
    }
}
#[test]
fn semantic_native_writer_refuses_work_memory_pending_and_foreign_scope() {
    let mut doc = fixture(&["hello"]);
    let (candidate, allocations) = cloned(&doc);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let hold = current.reserve(current.available_bytes() - 1).unwrap();
    assert!(matches!(
        read_text_native(&read, read.root(), None, limits(1)),
        Err(ReadError::Resource(NativeResourceError::WorkBytes))
    ));
    drop(hold);
    let short = Limits {
        scan_entries: 1,
        ..limits(1)
    };
    assert!(read_text_native(&read, read.root(), None, short).is_err());
    let (foreign, _) = cloned(&doc);
    // A different registry cannot even mint a session for this current turn.
    assert!(foreign.read_unqualified(&turn).is_err());
    doc.put(ROOT, "pending", true).unwrap();
    let count = doc.pending_ops();
    let heads = doc.base_heads();
    let (pending, allocations) = cloned(&doc);
    let pending_turn = allocations.work_turn(allowance()).unwrap();
    let read = pending.read_unqualified(&pending_turn).unwrap();
    assert_eq!(
        read_text_native(&read, read.root(), None, limits(1)).unwrap_err(),
        ReadError::Pending
    );
    assert_eq!(doc.pending_ops(), count);
    assert_eq!(doc.base_heads(), heads);
}

#[test]
fn semantic_native_writer_preserves_invalid_row_skip_semantics() {
    let mut doc = fixture(&["kept"]);
    let blocks = qdoc::model::open(&doc, &ROOT).unwrap().blocks;
    doc.put(&blocks, "x".repeat(1024), "invalid scalar row")
        .unwrap();
    doc.put_object(&blocks, "y".repeat(1024), ObjType::Map)
        .unwrap();
    doc.commit();
    let expected = read_text(&doc, &doc.base_heads(), None, limits(64)).unwrap();
    assert_eq!(expected.segments.len(), 1);
    let (candidate, allocations) = cloned(&doc);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let (actual, _, _) = allocation_meter::precharged(&current, || {
        read_text_native(&read, read.root(), None, limits(64))
    });
    assert_eq!(
        serde_json::to_value(actual.unwrap().page()).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}

#[test]
fn semantic_native_writer_binds_nested_roots_and_complete_frontier() {
    let mut doc = AutoCommit::new();
    let left = doc.put_object(ROOT, "left", ObjType::Map).unwrap();
    let right = doc.put_object(ROOT, "right", ObjType::Map).unwrap();
    for root in [&left, &right] {
        let c = qdoc::model::init(&mut doc, root);
        let mut ids = qdoc::IdGen::new("same-ids".into());
        let mut dm = qdoc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let first = dm.insert_block(None, "paragraph");
        dm.splice_text(&first, 0, 0, "first");
        let second = dm.insert_block(Some(&first), "paragraph");
        dm.splice_text(&second, 0, 0, "second");
    }
    doc.commit();
    let (candidate, allocations) = cloned(&doc);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let mut roots = [None, None];
    for (index, key) in ["left", "right"].into_iter().enumerate() {
        read.visit_property(
            read.root(),
            key,
            automerge::resource_limits::NativeSelection::Winner,
            |value, _| {
                if let automerge::resource_limits::NativeValue::Object { object, .. } = value {
                    roots[index] = Some(object);
                }
                Ok::<_, ()>(())
            },
        )
        .unwrap();
    }
    let page = read_text_native(&read, roots[0].unwrap(), None, limits(1)).unwrap();
    let expected = read_text_at(&doc, &doc.base_heads(), &left, None, limits(1)).unwrap();
    assert_eq!(
        serde_json::to_value(page.page()).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(
        read_text_native(
            &read,
            roots[1].unwrap(),
            page.page().next.as_ref(),
            limits(1)
        )
        .unwrap_err(),
        ReadError::InvalidCursor
    );
    let base = fixture(&["many heads"]);
    let mut joined = base.clone();
    for actor in 1u8..=65 {
        let mut fork = base
            .clone()
            .with_actor(automerge::ActorId::from(vec![actor]));
        fork.put(ROOT, "branch", actor as u64).unwrap();
        fork.commit();
        joined.merge(&mut fork).unwrap();
    }
    assert_eq!(joined.base_heads().len(), 65);
    let (candidate, allocations) = cloned(&joined);
    let turn = allocations.work_turn(allowance()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    assert!(matches!(
        read_text_native(&read, read.root(), None, limits(1)),
        Err(ReadError::Resource(NativeResourceError::Collection))
    ));
}

#[test]
fn semantic_native_large_borrowed_atom_never_materializes_whole_text() {
    let mut doc = fixture(&["seed"]);
    let c = qdoc::model::open(&doc, &ROOT).unwrap();
    let id = qdoc::model::blocks_in_order(&doc, &c)[0].clone();
    let block = doc.get(&c.blocks, id).unwrap().unwrap().1;
    let text = doc.get(&block, "text").unwrap().unwrap().1;
    doc.splice_text(&text, 0, 4, "").unwrap();
    doc.insert(&text, 0, "x".repeat(200_000)).unwrap();
    doc.commit();
    assert_eq!(doc.text(&text).unwrap().len(), 200_000);
    let expected = read_text(&doc, &doc.base_heads(), None, limits(64)).unwrap();
    let (candidate, allocations) = cloned(&doc);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let (actual, _, largest) = allocation_meter::precharged(&current, || {
        read_text_native(&read, read.root(), None, limits(64))
    });
    assert!(largest < 32 * 1024, "copied whole borrowed atom: {largest}");
    assert_eq!(
        serde_json::to_value(actual.unwrap().page()).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}
