//! The same public list API accepts a scoped scalar observer:
//! ```
//! use automerge::{AutoCommit, resource_limits::*};
//! fn observe(owned: &NativeOwned<AutoCommit>, turn: &NativeWorkTurn) {
//!     let read = owned.read(turn).unwrap();
//!     let steps = NativeReadSteps::new(100).unwrap();
//!     read.visit_property(read.root(), "list", NativeSelection::Winner, |value, _| {
//!         if let NativeValue::Object { object, .. } = value {
//!             read.visit_list(object, &steps, |entry| {
//!                 if let NativeValue::Scalar(NativeScalar::Str(text)) = entry.value {
//!                     assert!(text.len() <= usize::MAX);
//!                 }
//!                 Ok::<_, ()>(std::ops::ControlFlow::Continue(()))
//!             }).unwrap();
//!         }
//!         Ok::<_, ()>(())
//!     }).unwrap();
//! }
//! ```
//! List scalars are borrowed only inside the visitor:
//! ```compile_fail,E0521
//! use automerge::{AutoCommit, resource_limits::*};
//! fn leak(owned: &NativeOwned<AutoCommit>, turn: &NativeWorkTurn, list: NativeObject<'_>) {
//!     let read = owned.read(turn).unwrap();
//!     let steps = NativeReadSteps::new(100).unwrap();
//!     let mut escaped = None;
//!     read.visit_list(list, &steps, |entry| {
//!         if let NativeValue::Scalar(NativeScalar::Str(text)) = entry.value { escaped = Some(text); }
//!         Ok::<_, ()>(std::ops::ControlFlow::Continue(()))
//!     }).unwrap();
//!     println!("{:?}", escaped);
//! }
//! ```
use super::*;
use automerge::resource_limits::{NativeObject, NativeReadSession, NativeReadSteps};
use automerge::{
    ReadDoc, ScalarValueRef, ValueRef,
    marks::{ExpandMark, Mark},
};
use std::ops::ControlFlow;

fn object<'doc>(read: &NativeReadSession<'doc, '_>, key: &str) -> NativeObject<'doc> {
    let mut result = None;
    read.visit_property(read.root(), key, NativeSelection::Winner, |value, _| {
        if let NativeValue::Object { object, .. } = value {
            result = Some(object);
        }
        Ok::<_, ()>(())
    })
    .unwrap();
    result.unwrap()
}

#[test]
fn sequence_matches_real_list_winners_conflicts_deletes_and_counters() {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![1]));
    let list = doc.put_object(ROOT, "list", ObjType::List).unwrap();
    doc.insert(&list, 0, "base").unwrap();
    doc.insert(&list, 1, "deleted").unwrap();
    doc.insert(&list, 2, automerge::ScalarValue::counter(3))
        .unwrap();
    doc.commit();
    let mut other = doc.fork().with_actor(ActorId::from(vec![2]));
    doc.put(&list, 0, "left").unwrap();
    other.put(&list, 0, "right").unwrap();
    doc.delete(&list, 1).unwrap();
    doc.increment(&list, 1, 7).unwrap();
    doc.merge(&mut other).unwrap();
    doc.commit();
    let expected: Vec<_> = doc.list_range(&list, ..).collect();
    assert_eq!(expected.len(), 2);
    assert!(expected[0].conflict);
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let candidate = measured_candidate(&doc, &mut allocations);
    let turn = allocations.work_turn(allowance()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let list = object(&read, "list");
    let steps = NativeReadSteps::new(1000).unwrap();
    let mut seen = 0;
    read.visit_list(list, &steps, |entry| {
        let expected = &expected[seen];
        assert_eq!(
            (entry.index, entry.is_mark, entry.conflict),
            (expected.index, expected.is_mark(), expected.conflict)
        );
        let mut id = [0; 536];
        let len = read.write_id(entry.id, &mut id).unwrap();
        assert_eq!(&id[..len], expected.id().to_string().as_bytes());
        match (entry.value, &expected.value) {
            (
                NativeValue::Scalar(NativeScalar::Str(actual)),
                ValueRef::Scalar(ScalarValueRef::Str(expected)),
            ) => assert_eq!(actual, expected.as_ref()),
            (
                NativeValue::Scalar(NativeScalar::Counter(actual)),
                ValueRef::Scalar(ScalarValueRef::Counter(expected)),
            ) => {
                assert_eq!(actual, *expected);
                assert_eq!(actual, 10);
            }
            pair => panic!("unexpected list pair {pair:?}"),
        }
        seen += 1;
        Ok::<_, ()>(ControlFlow::Continue(()))
    })
    .unwrap();
    assert_eq!(seen, 2);
}

#[test]
fn sequence_marks_keep_true_indices_and_real_embedded_atoms() {
    let mut doc = AutoCommit::new();
    let text = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
    doc.splice_text(&text, 0, 0, "abcdefgh界🦀").unwrap();
    doc.split_block(&text, 3).unwrap();
    doc.mark(
        &text,
        Mark::new("bold".into(), true, 1, 6),
        ExpandMark::Both,
    )
    .unwrap();
    doc.mark(
        &text,
        Mark::new("font".into(), "NOT TEXT", 4, 9),
        ExpandMark::Both,
    )
    .unwrap();
    doc.commit();
    let plain = doc.text(&text).unwrap();
    assert_eq!(plain, "abc\u{fffc}defgh界🦀");
    let expected: Vec<_> = doc
        .list_range(&text, ..)
        .map(|e| (e.index, e.is_mark(), e.id()))
        .collect();
    assert_eq!(expected.iter().filter(|e| e.1).count(), 4);
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let candidate = measured_candidate(&doc, &mut allocations);
    let turn = allocations.work_turn(allowance()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let text = object(&read, "text");
    let mut actual = String::new();
    let mut seen = 0;
    read.visit_list(text, &NativeReadSteps::new(1000).unwrap(), |entry| {
        assert_eq!(
            (entry.index, entry.is_mark),
            (expected[seen].0, expected[seen].1)
        );
        if !entry.is_mark {
            match entry.value {
                NativeValue::Scalar(NativeScalar::Str(text)) => actual.push_str(text),
                _ => actual.push('\u{fffc}'),
            }
        }
        seen += 1;
        Ok::<_, ()>(ControlFlow::Continue(()))
    })
    .unwrap();
    assert_eq!(seen, expected.len());
    assert_eq!(actual, plain);
}

#[test]
fn sequence_and_nested_queries_keep_actual_allocations_precharged() {
    let mut doc = AutoCommit::new();
    let list = doc.put_object(ROOT, "list", ObjType::List).unwrap();
    doc.insert(&list, 0, automerge::ScalarValue::counter(0))
        .unwrap();
    for increment in 1..=256 {
        doc.increment(&list, 0, increment).unwrap();
    }
    let nested = doc.insert_object(&list, 1, ObjType::Map).unwrap();
    doc.put(&nested, "body", "borrowed".repeat(16_384)).unwrap();
    doc.commit();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let candidate = measured_candidate(&doc, &mut allocations);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let list = object(&read, "list");
    let steps = NativeReadSteps::new(5000).unwrap();
    let before = current.available_bytes();
    let mut seen = 0;
    crate::native_footprint_vendor_tests::assert_precharged(&current, || {
        read.visit_list(list, &steps, |entry| {
            match entry.value {
                NativeValue::Scalar(NativeScalar::Counter(value)) => assert_eq!(value, 32896),
                NativeValue::Object { object, .. } => {
                    read.visit_keys_with_steps(object, &steps, |key| {
                        assert_eq!(key, "body");
                        read.visit_property_with_steps(
                            object,
                            key,
                            NativeSelection::Winner,
                            &steps,
                            |value, _| {
                                let NativeValue::Scalar(NativeScalar::Str(text)) = value else {
                                    panic!("not text")
                                };
                                assert_eq!(text.len(), 8 * 16_384);
                                Ok::<_, ()>(())
                            },
                        )
                        .unwrap();
                        Ok::<_, ()>(ControlFlow::Continue(()))
                    })
                    .unwrap();
                }
                value => panic!("unexpected value {value:?}"),
            }
            seen += 1;
            Ok::<_, ()>(ControlFlow::Continue(()))
        })
        .unwrap();
    });
    assert_eq!(seen, 2);
    assert!(steps.remaining() < 5000 - 256);
    assert_eq!(current.available_bytes(), before);
}

#[test]
fn sequence_stops_and_shared_steps_do_not_refresh_for_nested_reads() {
    let mut doc = AutoCommit::new();
    let list = doc.put_object(ROOT, "list", ObjType::List).unwrap();
    for i in 0..100 {
        doc.insert(&list, i, i as u64).unwrap();
    }
    doc.commit();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let candidate = measured_candidate(&doc, &mut allocations);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let list = object(&read, "list");
    let steps = NativeReadSteps::new(10).unwrap();
    let before = current.available_bytes();
    let count = read
        .visit_list(list, &steps, |entry| {
            assert_eq!(entry.index, 0);
            Ok::<_, ()>(ControlFlow::Break(()))
        })
        .unwrap();
    assert_eq!(count, 1);
    assert!(steps.remaining() > 0 && steps.remaining() < 10);
    steps.charge(steps.remaining()).unwrap();
    assert!(matches!(
        read.visit_list(list, &steps, |_| -> Result<ControlFlow<()>, ()> {
            panic!("exhausted budget reached visitor")
        }),
        Err(NativeReadError::Resource(NativeResourceError::Operations))
    ));
    assert_eq!(current.available_bytes(), before);
    let enough = NativeReadSteps::new(1000).unwrap();
    assert_eq!(
        read.visit_list(list, &enough, |_| Err::<ControlFlow<()>, _>(17)),
        Err(NativeReadError::Visitor(17))
    );
    assert_eq!(current.available_bytes(), before);
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        read.visit_list(list, &enough, |_| -> Result<ControlFlow<()>, ()> {
            panic!("intentional visitor unwind")
        })
    }));
    assert!(unwind.is_err());
    assert_eq!(current.available_bytes(), before);
}

#[test]
fn sequence_refuses_foreign_handles_wrong_kinds_and_short_memory() {
    let mut doc = AutoCommit::new();
    let list = doc.put_object(ROOT, "list", ObjType::List).unwrap();
    doc.insert(&list, 0, "one").unwrap();
    doc.commit();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let a = measured_candidate(&doc, &mut allocations);
    let b = measured_candidate(&doc, &mut allocations);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = a.read_unqualified(&turn).unwrap();
    let foreign = b.read_unqualified(&turn).unwrap();
    let local_list = object(&read, "list");
    let other_list = object(&foreign, "list");
    let steps = NativeReadSteps::new(1000).unwrap();
    for object in [other_list, read.root()] {
        assert!(matches!(
            read.visit_list(object, &steps, |_| Ok::<_, ()>(ControlFlow::Continue(()))),
            Err(NativeReadError::Resource(NativeResourceError::Invalid))
        ));
    }
    let hold = current.reserve(current.available_bytes() - 1).unwrap();
    assert!(matches!(
        read.visit_list(local_list, &steps, |_| Ok::<_, ()>(ControlFlow::Continue(
            ()
        ))),
        Err(NativeReadError::Resource(NativeResourceError::WorkBytes))
    ));
    drop(hold);
    assert_eq!(
        read.visit_list(local_list, &steps, |_| Ok::<_, ()>(ControlFlow::Continue(
            ()
        )))
        .unwrap(),
        1
    );
}

#[test]
fn sequence_heads_are_complete_capped_before_callbacks_and_never_commit() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "base", true).unwrap();
    doc.commit();
    let mut branches = Vec::new();
    for i in 0..4 {
        let mut branch = doc.fork().with_actor(ActorId::from(vec![i + 1]));
        branch.put(ROOT, format!("branch{i}"), i as u64).unwrap();
        branch.commit();
        branches.push(branch);
    }
    for branch in &mut branches {
        doc.merge(branch).unwrap();
    }
    let mut expected = doc.base_heads();
    expected.sort_unstable();
    assert_eq!(expected.len(), 4);
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let candidate = measured_candidate(&doc, &mut allocations);
    let turn = allocations.work_turn(allowance()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    assert_eq!(read.pending_ops(), 0);
    assert_eq!(read.operation_count() as u64, doc.stats().num_ops);
    let mut got = Vec::new();
    assert_eq!(
        read.visit_heads(3, |_| panic!("head prefix escaped before complete-set cap")),
        Err(NativeResourceError::Collection)
    );
    assert_eq!(read.visit_heads(4, |hash| got.push(hash)).unwrap(), 4);
    got.sort_unstable();
    assert_eq!(got, expected);
    doc.put(ROOT, "pending", true).unwrap();
    let pending = measured_candidate(&doc, &mut allocations);
    let read = pending.read_unqualified(&turn).unwrap();
    let before = read.pending_ops();
    assert!(before > 0);
    assert_eq!(
        read.visit_heads(64, |_| panic!("pending read emitted heads")),
        Err(NativeResourceError::Unaccounted)
    );
    assert_eq!(read.pending_ops(), before);
    assert!(doc.pending_ops() > 0);
}

#[test]
fn sequence_output_credit_uses_current_ledger_and_survives_turn_sealing() {
    let doc = AutoCommit::new();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let candidate = measured_candidate(&doc, &mut allocations);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let before = current.available_bytes();
    let credit = read.reserve_output(1024).unwrap();
    assert_eq!(current.available_bytes(), before - 1024);
    assert!(matches!(
        read.reserve_output(before),
        Err(NativeResourceError::WorkBytes)
    ));
    current.finish_turn();
    assert!(matches!(
        read.reserve_output(1),
        Err(NativeResourceError::WorkBytes)
    ));
    drop(read);
    drop(turn);
    let retained = current.retained_bytes();
    assert!(retained >= 1024);
    drop(credit);
    assert_eq!(current.retained_bytes(), retained - 1024);
}
