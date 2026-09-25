//! Real dependent-crate reader tests. Measured candidates remain unqualified;
//! these tests do not claim checked-load or NativeOwned construction readiness.
//!
//! The public reader API resolves without constructing a qualified document:
//! ```
//! use automerge::{AutoCommit, resource_limits::*};
//! fn valid(owned: &NativeOwned<AutoCommit>, turn: &NativeWorkTurn) {
//!     let read = owned.read(turn).unwrap();
//!     let mut object = None;
//!     read.visit_property(read.root(), "key", NativeSelection::Winner, |value, _| {
//!         if let NativeValue::Object { object: value, .. } = value { object = Some(value); }
//!         Ok::<_, ()>(())
//!     }).unwrap();
//!     if let Some(object) = object {
//!         read.visit_keys(object, |_| Ok::<_, ()>(())).unwrap();
//!     }
//! }
//! ```
//! A scalar loan cannot escape its higher-ranked callback:
//! ```compile_fail,E0521
//! use automerge::{AutoCommit, resource_limits::*};
//! fn leak(owned: &NativeOwned<AutoCommit>, turn: &NativeWorkTurn) {
//!     let read = owned.read(turn).unwrap();
//!     let mut escaped = None;
//!     read.visit_property(read.root(), "key", NativeSelection::Winner, |value, _| {
//!         if let NativeValue::Scalar(NativeScalar::Str(value)) = value { escaped = Some(value); }
//!         Ok::<_, ()>(())
//!     }).unwrap();
//!     println!("{:?}", escaped);
//! }
//! ```
//! An opaque identity cannot outlive its retained document:
//! ```compile_fail,E0515
//! use automerge::{AutoCommit, resource_limits::*};
//! fn leak(owned: NativeOwned<AutoCommit>, turn: &NativeWorkTurn) -> NativeObject<'static> {
//!     owned.read(turn).unwrap().root()
//! }
//! ```
//! There is no dereference or coercion to raw AutoCommit:
//! ```compile_fail,E0308
//! use automerge::{AutoCommit, resource_limits::*};
//! fn raw(owned: &NativeOwned<AutoCommit>) -> &AutoCommit { owned }
//! ```
use automerge::resource_limits::{
    NativeAllocationSet, NativeAllowance, NativeCloneCandidate, NativeReadError,
    NativeResourceError, NativeScalar, NativeSelection, NativeValue,
};
use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ObjType, ROOT};
mod sequence;

fn allowance() -> NativeAllowance {
    NativeAllowance::new(8 * 1024 * 1024).unwrap()
}

fn measured_candidate(
    doc: &AutoCommit,
    allocations: &mut NativeAllocationSet,
) -> NativeCloneCandidate<AutoCommit> {
    let turn = allocations.work_turn(allowance()).unwrap();
    doc.clone_bounded(allocations, &turn).unwrap()
}

#[test]
fn native_read_nested_keys_property_and_canonical_identity() {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![0xab, 0x01]));
    let object = doc.put_object(ROOT, "config", ObjType::Map).unwrap();
    doc.put(
        &object,
        "bytes",
        automerge::ScalarValue::Bytes(vec![1, 2, 3]),
    )
    .unwrap();
    doc.put(&object, "title", "borrowed title").unwrap();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let candidate = measured_candidate(&doc, &mut allocations);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let available = current.available_bytes();
    let mut visited = 0;
    assert_eq!(
        read.visit_property(
            read.root(),
            "config",
            NativeSelection::Winner,
            |value, id| {
                let NativeValue::Object {
                    kind: ObjType::Map,
                    object,
                } = value
                else {
                    panic!("expected map")
                };
                assert_eq!(object.id(), id);
                assert_eq!(read.id_len(id).unwrap(), 6);
                let mut text = [0; 6];
                assert_eq!(read.write_id(id, &mut text).unwrap(), 6);
                assert_eq!(&text, b"1@ab01");
                assert_eq!(
                    read.visit_keys(object, |key| {
                        assert_eq!(
                            read.visit_property(object, key, NativeSelection::All, |value, _| {
                                match (key, value) {
                                    ("bytes", NativeValue::Scalar(NativeScalar::Bytes(bytes))) => {
                                        assert_eq!(bytes, &[1, 2, 3])
                                    }
                                    ("title", NativeValue::Scalar(NativeScalar::Str(text))) => {
                                        assert_eq!(text, "borrowed title")
                                    }
                                    other => panic!("unexpected borrowed value {other:?}"),
                                }
                                visited += 1;
                                Ok::<_, ()>(())
                            })
                            .unwrap(),
                            1
                        );
                        Ok::<_, ()>(())
                    })
                    .unwrap(),
                    2
                );
                Ok::<_, ()>(())
            }
        )
        .unwrap(),
        1
    );
    assert_eq!(visited, 2);
    assert_eq!(current.available_bytes(), available);
    assert!(
        doc.pending_ops() > 0,
        "reading does not close the source transaction"
    );
}

#[test]
fn native_read_winner_all_deleted_and_visitor_stop() {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![1]));
    doc.put(ROOT, "title", "base").unwrap();
    doc.put(ROOT, "removed", true).unwrap();
    doc.commit();
    let mut left = doc.fork().with_actor(ActorId::from(vec![2]));
    let mut right = doc.fork().with_actor(ActorId::from(vec![3]));
    left.put(ROOT, "title", "left").unwrap();
    right.put(ROOT, "title", "right").unwrap();
    left.delete(ROOT, "removed").unwrap();
    doc.merge(&mut left).unwrap();
    doc.merge(&mut right).unwrap();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let candidate = measured_candidate(&doc, &mut allocations);
    let turn = allocations.work_turn(allowance()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let mut count = 0;
    assert_eq!(
        read.visit_property(read.root(), "title", NativeSelection::All, |value, _| {
            let expected = if count == 0 { "left" } else { "right" };
            assert_eq!(value, NativeValue::Scalar(NativeScalar::Str(expected)));
            count += 1;
            Ok::<_, ()>(())
        })
        .unwrap(),
        2
    );
    assert_eq!(
        read.visit_property(read.root(), "title", NativeSelection::Winner, |value, _| {
            assert_eq!(value, NativeValue::Scalar(NativeScalar::Str("right")));
            Ok::<_, ()>(())
        })
        .unwrap(),
        1
    );
    assert_eq!(
        read.visit_property(
            read.root(),
            "removed",
            NativeSelection::All,
            |_, _| -> Result<(), ()> { panic!("deleted property returned") }
        )
        .unwrap(),
        0
    );
    let mut calls = 0;
    assert_eq!(
        read.visit_property(read.root(), "title", NativeSelection::All, |_, _| {
            calls += 1;
            Err("visitor stopped")
        }),
        Err(NativeReadError::Visitor("visitor stopped"))
    );
    assert_eq!(calls, 1);
}

#[test]
fn native_read_refuses_foreign_domain_document_and_short_id_sink() {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![9; 128]));
    doc.put_object(ROOT, "map", ObjType::Map).unwrap();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let candidate = measured_candidate(&doc, &mut allocations);
    let other = measured_candidate(&doc, &mut allocations);
    let turn = allocations.work_turn(allowance()).unwrap();
    let foreign_domain = NativeAllocationSet::new(allowance()).unwrap();
    let foreign_turn = foreign_domain.work_turn(allowance()).unwrap();
    assert!(matches!(
        candidate.read_unqualified(&foreign_turn),
        Err(NativeResourceError::Unaccounted)
    ));
    let read = candidate.read_unqualified(&turn).unwrap();
    let other_read = other.read_unqualified(&turn).unwrap();
    assert_ne!(read.root(), other_read.root());
    assert_eq!(
        read.visit_keys(other_read.root(), |_| Ok::<_, ()>(())),
        Err(NativeReadError::Resource(NativeResourceError::Invalid))
    );
    assert_eq!(
        read.id_len(other_read.root().id()),
        Err(NativeResourceError::Invalid)
    );
    read.visit_property(read.root(), "map", NativeSelection::Winner, |_, id| {
        assert_eq!(read.id_len(id).unwrap(), 258);
        let mut too_short = [0x55; 257];
        assert_eq!(
            read.write_id(id, &mut too_short),
            Err(NativeResourceError::WorkBytes)
        );
        assert_eq!(too_short, [0x55; 257]);
        let mut bytes = [0; 258];
        assert_eq!(read.write_id(id, &mut bytes).unwrap(), 258);
        assert_eq!(&bytes[..2], b"1@");
        assert!(bytes[2..].chunks_exact(2).all(|pair| pair == b"09"));
        Ok::<_, ()>(())
    })
    .unwrap();
    let mut root = [0; 5];
    read.write_id(read.root().id(), &mut root).unwrap();
    assert_eq!(&root, b"_root");
}

#[test]
fn native_read_current_turn_refuses_before_visit_and_releases_nested_work() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "value", "nonempty").unwrap();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let candidate = measured_candidate(&doc, &mut allocations);
    // Residence has ample credit; only the distinct current turn is exhausted.
    let current = NativeAllowance::new(64).unwrap();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let mut calls = 0;
    assert_eq!(
        read.visit_property(read.root(), "value", NativeSelection::All, |_, _| {
            calls += 1;
            Ok::<_, ()>(())
        }),
        Err(NativeReadError::Resource(NativeResourceError::WorkBytes))
    );
    assert_eq!(
        read.visit_keys(read.root(), |_| {
            calls += 1;
            Ok::<_, ()>(())
        }),
        Err(NativeReadError::Resource(NativeResourceError::WorkBytes))
    );
    assert_eq!(calls, 0);

    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let initial = current.available_bytes();
    read.visit_keys(read.root(), |key| {
        assert!(
            current.available_bytes() < initial,
            "outer cursor must retain its charge during the callback"
        );
        let remainder = current.reserve(current.available_bytes()).unwrap();
        assert_eq!(
            read.visit_property(
                read.root(),
                key,
                NativeSelection::Winner,
                |_, _| Ok::<_, ()>(())
            ),
            Err(NativeReadError::Resource(NativeResourceError::WorkBytes))
        );
        drop(remainder);
        read.visit_property(read.root(), key, NativeSelection::Winner, |value, _| {
            assert_eq!(value, NativeValue::Scalar(NativeScalar::Str("nonempty")));
            Ok::<_, ()>(())
        })
        .unwrap();
        Ok::<_, ()>(())
    })
    .unwrap();
    assert_eq!(current.available_bytes(), initial);
    assert_eq!(
        read.visit_keys(read.root(), |_| Err("stop")),
        Err(NativeReadError::Visitor("stop"))
    );
    assert_eq!(current.available_bytes(), initial);
}

#[test]
fn native_read_scalars_counters_and_isolation_refusal() {
    let mut doc = AutoCommit::new();
    let fields = [
        ("boolean", automerge::ScalarValue::Boolean(true)),
        ("counter", automerge::ScalarValue::counter(7)),
        ("float", automerge::ScalarValue::F64(1.5)),
        ("int", automerge::ScalarValue::Int(-3)),
        ("null", automerge::ScalarValue::Null),
        ("time", automerge::ScalarValue::Timestamp(42)),
        ("uint", automerge::ScalarValue::Uint(9)),
        (
            "unknown",
            automerge::ScalarValue::Unknown {
                type_code: 13,
                bytes: vec![7, 8],
            },
        ),
    ];
    for (key, value) in fields {
        doc.put(ROOT, key, value).unwrap();
    }
    doc.increment(ROOT, "counter", 5).unwrap();
    let old_heads = doc.get_heads();
    doc.put(ROOT, "later", false).unwrap();
    doc.commit();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let candidate = measured_candidate(&doc, &mut allocations);
    let turn = allocations.work_turn(allowance()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let expected = [
        ("boolean", NativeScalar::Boolean(true)),
        ("counter", NativeScalar::Counter(12)),
        ("float", NativeScalar::F64(1.5)),
        ("int", NativeScalar::Int(-3)),
        ("null", NativeScalar::Null),
        ("time", NativeScalar::Timestamp(42)),
        ("uint", NativeScalar::Uint(9)),
        (
            "unknown",
            NativeScalar::Unknown {
                type_code: 13,
                bytes: &[7, 8],
            },
        ),
    ];
    for (key, expected) in expected {
        assert_eq!(
            read.visit_property(read.root(), key, NativeSelection::Winner, |value, _| {
                assert_eq!(value, NativeValue::Scalar(expected));
                Ok::<_, ()>(())
            })
            .unwrap(),
            1
        );
    }
    doc.isolate(&old_heads);
    let isolated = measured_candidate(&doc, &mut allocations);
    assert!(matches!(
        isolated.read_unqualified(&turn),
        Err(NativeResourceError::Unaccounted)
    ));
}

#[test]
fn native_read_large_borrowed_values_and_cursor_allocations_are_precharged() {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![1; 256]));
    for i in 0..256 {
        doc.put(
            ROOT,
            format!("key-{i:04}-{}", "long-key".repeat(16)),
            i as u64,
        )
        .unwrap();
    }
    doc.put(
        ROOT,
        "large-bytes",
        automerge::ScalarValue::Bytes(vec![0xa5; 128 * 1024]),
    )
    .unwrap();
    doc.put(ROOT, "large-string", "borrowed-".repeat(16384))
        .unwrap();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let candidate = measured_candidate(&doc, &mut allocations);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let initial = current.available_bytes();
    let mut count = 0;
    crate::native_footprint_vendor_tests::assert_precharged(&current, || {
        read.visit_keys(read.root(), |key| {
            read.visit_property(read.root(), key, NativeSelection::All, |value, id| {
                match value {
                    NativeValue::Scalar(NativeScalar::Bytes(bytes)) => {
                        assert_eq!(bytes.len(), 128 * 1024);
                        assert!(bytes.iter().all(|b| *b == 0xa5));
                    }
                    NativeValue::Scalar(NativeScalar::Str(text)) => {
                        assert_eq!(text.len(), 9 * 16384)
                    }
                    NativeValue::Scalar(NativeScalar::Uint(value)) => assert!(value < 256),
                    value => panic!("unexpected scalar {value:?}"),
                }
                let mut id_bytes = [0; 536];
                let len = read.write_id(id, &mut id_bytes).unwrap();
                assert_eq!(read.id_len(id).unwrap(), len);
                count += 1;
                Ok::<_, ()>(())
            })
            .unwrap();
            Ok::<_, ()>(())
        })
        .unwrap();
    });
    assert_eq!(count, 258);
    assert_eq!(current.available_bytes(), initial);
}

#[test]
fn native_read_counter_successor_seeks_are_precharged_while_workspace_is_live() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "counter", automerge::ScalarValue::counter(0))
        .unwrap();
    // Different increments prevent one RLE run from hiding a successor seek.
    // The IntCursor uses 64-byte slabs; 256 distinct values span several.
    for increment in 1..=256 {
        doc.increment(ROOT, "counter", increment).unwrap();
    }
    doc.commit();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let candidate = measured_candidate(&doc, &mut allocations);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let initial = current.available_bytes();
    crate::native_footprint_vendor_tests::assert_precharged(&current, || {
        for selection in [NativeSelection::Winner, NativeSelection::All] {
            assert_eq!(
                read.visit_property(read.root(), "counter", selection, |value, _| {
                    assert_eq!(value, NativeValue::Scalar(NativeScalar::Counter(32896)));
                    Ok::<_, ()>(())
                })
                .unwrap(),
                1
            );
        }
        assert_eq!(
            read.visit_keys(read.root(), |key| {
                assert_eq!(key, "counter");
                Ok::<_, ()>(())
            })
            .unwrap(),
            1
        );
    });
    assert_eq!(current.available_bytes(), initial);
}
