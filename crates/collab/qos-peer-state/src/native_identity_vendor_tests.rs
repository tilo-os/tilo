//! Exact borrowed identity operations; measured candidates do not qualify owners.
//!
//! The borrowed callback API is usable without extracting native storage:
//! ```
//! use automerge::resource_limits::{NativeReadSession, NativeId};
//! fn actor_len<'doc>(read: &NativeReadSession<'doc, '_>, id: NativeId<'doc>) -> usize {
//!     read.visit_id_parts(id, |parts| parts.map_or(0, |(_, actor)| actor.len())).unwrap()
//! }
//! ```
//! Actor bytes cannot escape the higher-ranked callback:
//! ```compile_fail,E0521
//! use automerge::resource_limits::{NativeReadSession, NativeId};
//! fn leak<'doc>(read: &NativeReadSession<'doc, '_>, id: NativeId<'doc>) {
//!     let mut escaped = None;
//!     read.visit_id_parts(id, |parts| escaped = parts).unwrap();
//!     println!("{:?}", escaped);
//! }
//! ```
#[cfg(test)]
use crate::native_footprint_vendor_tests::assert_precharged;
use automerge::resource_limits::{
    NativeAllocationSet, NativeAllowance, NativeResourceError, NativeSelection, NativeValue,
};
use automerge::{ActorId, AutoCommit, ObjId, ObjType, ROOT, transaction::Transactable};

fn allowance() -> NativeAllowance {
    NativeAllowance::new(64 * 1024 * 1024).unwrap()
}

// Literal pre-factor encoder, including its original actor-index/counter order.
fn old_bytes(id: &ObjId) -> Vec<u8> {
    fn leb(mut value: u64, out: &mut Vec<u8>) {
        loop {
            let byte = (value & 127) as u8;
            value >>= 7;
            out.push(byte | if value > 0 { 128 } else { 0 });
            if value == 0 {
                break;
            }
        }
    }
    match id {
        ObjId::Root => vec![0],
        ObjId::Id(counter, actor, index) => {
            let mut out = vec![16];
            leb(actor.to_bytes().len() as u64, &mut out);
            out.extend_from_slice(actor.to_bytes());
            leb(*index as u64, &mut out);
            leb(*counter, &mut out);
            out
        }
    }
}
#[test]
fn existing_binary_encoder_preserves_literal_boundary_oracle() {
    for length in [0, 1, 127, 128, 4096] {
        let actor = ActorId::from(vec![0xa7; length]);
        for counter in [0, 1, 127, 128, 16383, 16384, u64::MAX] {
            for index in [0, 127, 128, usize::MAX] {
                let id = ObjId::Id(counter, actor.clone(), index);
                assert_eq!(id.to_bytes(), old_bytes(&id));
            }
        }
    }
    assert_eq!(ROOT.to_bytes(), [0]);
}
#[test]
fn borrowed_binary_and_semantic_identity_match_real_native_objects_without_allocating() {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![0xa7; 4096]));
    for _ in 0..128 {
        doc.put(ROOT, "padding", true).unwrap();
    }
    let first = doc.put_object(ROOT, "long", ObjType::Map).unwrap();
    doc.commit();
    doc.set_actor(ActorId::from(vec![0xb8; 128]));
    let second = doc.put_object(ROOT, "next", ObjType::Map).unwrap();
    doc.commit();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let candidate = doc.clone_bounded(&mut allocations, &turn).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let mut root = [9; 1];
    assert_eq!(
        assert_precharged(&current, || read
            .write_id_bytes(read.root().id(), &mut root))
        .unwrap(),
        1
    );
    assert_eq!(root, [0]);
    assert!(
        read.visit_id_parts(read.root().id(), |parts| parts.is_none())
            .unwrap()
    );
    for (key, expected) in [("long", first), ("next", second)] {
        let bytes = old_bytes(&expected);
        let mut sink = vec![0; bytes.len()];
        let mut short = vec![0x55; bytes.len() - 1];
        assert_precharged(&current, || {
            read.visit_property(read.root(), key, NativeSelection::Winner, |value, id| {
                assert!(matches!(value, NativeValue::Object { .. }));
                assert_eq!(read.id_bytes_len(id).unwrap(), bytes.len());
                assert_eq!(
                    read.write_id_bytes(id, &mut short),
                    Err(NativeResourceError::WorkBytes)
                );
                assert!(short.iter().all(|b| *b == 0x55));
                assert_eq!(read.write_id_bytes(id, &mut sink).unwrap(), bytes.len());
                assert_eq!(sink, bytes);
                read.visit_id_parts(id, |parts| {
                    let (counter, actor) = parts.unwrap();
                    let ObjId::Id(expected_counter, expected_actor, _) = &expected else {
                        unreachable!()
                    };
                    assert_eq!(counter, *expected_counter);
                    assert_eq!(actor, expected_actor.to_bytes());
                })
                .unwrap();
                Ok::<_, ()>(())
            })
        })
        .unwrap();
    }
}
#[test]
fn borrowed_identity_rejects_foreign_document_before_touching_sink_or_callback() {
    let doc = AutoCommit::new();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let first = doc.clone_bounded(&mut allocations, &turn).unwrap();
    let second = doc.clone_bounded(&mut allocations, &turn).unwrap();
    let a = first.read_unqualified(&turn).unwrap();
    let b = second.read_unqualified(&turn).unwrap();
    let mut sink = [0x33; 16];
    let mut called = false;
    assert_eq!(
        assert_precharged(&current, || a.id_bytes_len(b.root().id())),
        Err(NativeResourceError::Invalid)
    );
    assert_eq!(
        a.write_id_bytes(b.root().id(), &mut sink),
        Err(NativeResourceError::Invalid)
    );
    assert_eq!(sink, [0x33; 16]);
    assert_eq!(
        a.visit_id_parts(b.root().id(), |_| called = true),
        Err(NativeResourceError::Invalid)
    );
    assert!(!called);
}
