//! Exact legacy scalar formatting, lending no owned native values.
use crate::native_footprint_vendor_tests::assert_precharged;
use automerge::resource_limits::{
    NativeAllocationSet, NativeAllowance, NativeScalar, NativeSelection, NativeValue,
};
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use std::fmt::Write;

struct HashText(blake3::Hasher);
impl std::fmt::Write for HashText {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.0.update(text.as_bytes());
        Ok(())
    }
}
fn allowance() -> NativeAllowance {
    NativeAllowance::new(64 * 1024 * 1024).unwrap()
}
fn expected(text: &str) -> [u8; 32] {
    *blake3::hash(text.as_bytes()).as_bytes()
}

#[test]
fn borrowed_scalar_writer_matches_literal_debug_shapes_without_allocating() {
    let bytes = [0, 1, 127, 255];
    let cases = [
        (NativeScalar::Bytes(&bytes), "Bytes([0, 1, 127, 255])"),
        (NativeScalar::Str("a\n\"\\☺"), "Str(\"a\\n\\\"\\\\☺\")"),
        (NativeScalar::Int(i64::MIN), "Int(-9223372036854775808)"),
        (NativeScalar::Uint(u64::MAX), "Uint(18446744073709551615)"),
        (NativeScalar::F64(-0.0), "F64(-0.0)"),
        (NativeScalar::F64(f64::INFINITY), "F64(inf)"),
        (NativeScalar::F64(f64::NAN), "F64(NaN)"),
        (
            NativeScalar::Counter(12),
            "Counter(Counter { start: 12, current: 12 })",
        ),
        (NativeScalar::Timestamp(-42), "Timestamp(-42)"),
        (NativeScalar::Boolean(true), "Boolean(true)"),
        (
            NativeScalar::Unknown {
                type_code: 15,
                bytes: &bytes,
            },
            "Unknown { type_code: 15, bytes: [0, 1, 127, 255] }",
        ),
        (NativeScalar::Null, "Null"),
    ];
    let current = allowance();
    for (value, text) in cases {
        let mut sink = HashText(blake3::Hasher::new());
        assert_precharged(&current, || value.write_legacy_debug(&mut sink)).unwrap();
        assert_eq!(*sink.0.finalize().as_bytes(), expected(text));
    }
    struct Refuse;
    impl std::fmt::Write for Refuse {
        fn write_str(&mut self, _: &str) -> std::fmt::Result {
            Err(std::fmt::Error)
        }
    }
    assert!(
        NativeScalar::Int(1)
            .write_legacy_debug(&mut Refuse)
            .is_err()
    );
}
#[test]
fn actual_malformed_policy_fingerprints_match_legacy_for_all_shapes_and_incremented_counter() {
    let values = vec![
        ScalarValue::Bytes(vec![0x17; 16000]),
        ScalarValue::Str("a\n\"\\☺".repeat(2000).into()),
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
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    for value in values {
        let counter = matches!(value, ScalarValue::Counter(_));
        let mut doc = AutoCommit::new();
        doc.put(ROOT, crate::room::media::FIELD, value).unwrap();
        if counter {
            doc.increment(ROOT, crate::room::media::FIELD, 5).unwrap();
        }
        doc.commit();
        let legacy = doc.get(ROOT, crate::room::media::FIELD).unwrap().unwrap().0;
        if counter {
            assert_eq!(
                format!("{:?}", legacy.to_scalar().unwrap()),
                "Counter(Counter { start: 12, current: 12 })"
            );
        }
        let expected = crate::room::media::governed_value(Some(&doc));
        let candidate = doc.clone_bounded(&mut allocations, &turn).unwrap();
        let read = candidate.read_unqualified(&turn).unwrap();
        let mut hash = HashText(blake3::Hasher::new());
        assert_precharged(&current, || {
            read.visit_property(
                read.root(),
                crate::room::media::FIELD,
                NativeSelection::All,
                |value, _| {
                    match value {
                        NativeValue::Scalar(NativeScalar::Bytes(bytes)) => {
                            hash.0.update(b"bytes\0");
                            hash.0.update(bytes);
                        }
                        NativeValue::Scalar(NativeScalar::Str(text)) => {
                            hash.0.update(b"string\0");
                            hash.0.update(text.as_bytes());
                        }
                        NativeValue::Scalar(value) => {
                            hash.0.update(b"invalid-scalar\0");
                            value.write_legacy_debug(&mut hash).unwrap();
                        }
                        _ => panic!("scalar fixture"),
                    }
                    Ok::<_, ()>(())
                },
            )
        })
        .unwrap();
        let crate::room::media::MediaGovernanceValue::Values(set) = expected else {
            panic!("value expected")
        };
        assert_eq!(
            set.into_iter().collect::<Vec<_>>(),
            vec![*hash.0.finalize().as_bytes()]
        );
    }
    for kind in [ObjType::Map, ObjType::Table, ObjType::List, ObjType::Text] {
        let mut doc = AutoCommit::new();
        let id = doc
            .put_object(ROOT, crate::room::media::FIELD, kind)
            .unwrap();
        doc.commit();
        let expected = crate::room::media::governed_value(Some(&doc));
        let mut encoded = vec![0; id.to_bytes().len()];
        let candidate = doc.clone_bounded(&mut allocations, &turn).unwrap();
        let read = candidate.read_unqualified(&turn).unwrap();
        let mut hash = HashText(blake3::Hasher::new());
        assert_precharged(&current, || {
            read.visit_property(
                read.root(),
                crate::room::media::FIELD,
                NativeSelection::All,
                |value, id| {
                    let NativeValue::Object { kind, .. } = value else {
                        panic!("object fixture")
                    };
                    hash.0.update(b"invalid-object\0");
                    write!(&mut hash, "{kind:?}").unwrap();
                    let len = read.write_id_bytes(id, &mut encoded).unwrap();
                    hash.0.update(&encoded[..len]);
                    Ok::<_, ()>(())
                },
            )
        })
        .unwrap();
        let crate::room::media::MediaGovernanceValue::Values(set) = expected else {
            panic!("value expected")
        };
        assert_eq!(
            set.into_iter().collect::<Vec<_>>(),
            vec![*hash.0.finalize().as_bytes()]
        );
    }
}
