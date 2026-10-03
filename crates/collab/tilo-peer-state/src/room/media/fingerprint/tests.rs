use super::*;
use crate::resources::native::NativeBatch;
use automerge::{
    ActorId, AutoCommit, ObjType, ROOT, ScalarValue, Value, transaction::Transactable,
};
use std::collections::BTreeSet;
use tilo_automerge::ApplyIncoming;

// Literal prior comparison algorithm, independent of the shared implementation.
fn old(doc: &AutoCommit) -> super::super::MediaGovernanceValue {
    let values = doc.get_all(ROOT, super::super::FIELD).unwrap();
    if values.is_empty() {
        return super::super::MediaGovernanceValue::Absent;
    }
    let mut fingerprints = BTreeSet::new();
    for (value, object) in values {
        let mut hash = blake3::Hasher::new();
        match value {
            Value::Scalar(value) => match value.as_ref() {
                ScalarValue::Bytes(bytes) => {
                    hash.update(b"bytes\0");
                    hash.update(bytes);
                }
                ScalarValue::Str(value) => {
                    hash.update(b"string\0");
                    hash.update(value.as_bytes());
                }
                value => {
                    hash.update(b"invalid-scalar\0");
                    hash.update(format!("{value:?}").as_bytes());
                }
            },
            Value::Object(kind) => {
                hash.update(b"invalid-object\0");
                hash.update(format!("{kind:?}").as_bytes());
                hash.update(&object.to_bytes());
            }
        }
        fingerprints.insert(*hash.finalize().as_bytes());
    }
    super::super::MediaGovernanceValue::Values(fingerprints)
}
fn compare(doc: &AutoCommit) {
    let expected = old(doc);
    assert_eq!(super::super::governed_value(Some(doc)), expected);
    let mut legacy = BTreeSet::new();
    let count = LegacyRead(doc)
        .visit_media_fingerprints(|hash| {
            legacy.insert(hash);
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(
        if count == 0 {
            super::super::MediaGovernanceValue::Absent
        } else {
            super::super::MediaGovernanceValue::Values(legacy)
        },
        expected
    );
    // The same fingerprints from the document as the isolated loader admits it.
    let admitted = NativeBatch::new(usize::MAX)
        .load(&doc.clone().save())
        .unwrap();
    let mut hashes = [[0; 32]; 8];
    let mut index = 0;
    let count = admitted
        .read()
        .visit_media_fingerprints(|hash| {
            hashes[index] = hash;
            index = index.checked_add(1).unwrap();
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(count, index);
    let result = if count == 0 {
        super::super::MediaGovernanceValue::Absent
    } else {
        super::super::MediaGovernanceValue::Values(hashes[..count].iter().copied().collect())
    };
    assert_eq!(result, expected);
}
#[test]
fn shared_fingerprint_matches_literal_all_scalar_object_and_absent_shapes() {
    compare(&AutoCommit::new());
    let values = vec![
        ScalarValue::Bytes(vec![0x17; 16000]),
        ScalarValue::Str("x\n☺".repeat(4000).into()),
        ScalarValue::Int(i64::MIN),
        ScalarValue::Uint(u64::MAX),
        ScalarValue::F64(-0.0),
        ScalarValue::F64(f64::INFINITY),
        ScalarValue::F64(f64::NAN),
        ScalarValue::Counter(7.into()),
        ScalarValue::Timestamp(-42),
        ScalarValue::Boolean(true),
        ScalarValue::Unknown {
            type_code: 15,
            bytes: vec![255; 16000],
        },
        ScalarValue::Null,
    ];
    for value in values {
        let counter = matches!(value, ScalarValue::Counter(_));
        let mut doc = AutoCommit::new();
        doc.put(ROOT, super::super::FIELD, value).unwrap();
        if counter {
            doc.increment(ROOT, super::super::FIELD, 5).unwrap();
        }
        doc.commit();
        compare(&doc);
    }
    for kind in [ObjType::Map, ObjType::Table, ObjType::List, ObjType::Text] {
        let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![4; 16000]));
        doc.put_object(ROOT, super::super::FIELD, kind).unwrap();
        doc.commit();
        compare(&doc);
    }
}
#[test]
fn shared_fingerprint_preserves_actual_conflicts_and_visitor_refusal() {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![1]));
    doc.put(ROOT, "ordinary", "basis").unwrap();
    doc.commit();
    let mut a = doc.fork().with_actor(ActorId::from(vec![2]));
    let mut b = doc.fork().with_actor(ActorId::from(vec![3]));
    a.put(ROOT, super::super::FIELD, ScalarValue::Counter(9.into()))
        .unwrap();
    a.commit();
    b.put_object(ROOT, super::super::FIELD, ObjType::List)
        .unwrap();
    b.commit();
    doc.merge_incoming(&mut a).unwrap();
    doc.merge_incoming(&mut b).unwrap();
    compare(&doc);
    let admitted = NativeBatch::new(usize::MAX)
        .load(&doc.clone().save())
        .unwrap();
    assert_eq!(
        admitted.read().visit_media_fingerprints(|_| Err(7u8)),
        Err(ReadError::Visitor(7))
    );
}
