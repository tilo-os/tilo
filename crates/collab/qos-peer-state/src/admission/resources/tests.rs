use super::*;
use crate::native_footprint_vendor_tests::assert_precharged;

fn actual_index() -> (ExactAdmissions, Vec<DocumentAdmission>) {
    let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
    peer.select_admission_v5().unwrap();
    let mut index = ExactAdmissions::default();
    let mut admissions = Vec::new();
    for _ in 0..2 {
        let crate::NeighbourMsg::AppDocument { envelope, .. } = peer
            .publish_document(
                "test.app",
                "tab:1",
                peer.document("test.app", "tab:1").unwrap(),
                &key,
                now,
            )
            .unwrap()
        else {
            panic!("actual signed publication required")
        };
        let admission = peer
            .lookup_exact_local_document("test.app", "tab:1", &envelope, now)
            .unwrap()
            .expect("actual stored exact admission");
        index.insert(&admission).unwrap();
        admissions.push(admission);
    }
    assert_eq!(index.iter().count(), 2);
    (index, admissions)
}
fn limits() -> OuterLimits {
    OuterLimits {
        allocation_bytes: 256 * 1024,
        collection_entries: MAX_EXACT_ADMISSIONS,
    }
}

#[test]
fn exact_resource_decode_preserves_actual_signed_admissions_and_retains_custody() {
    let (index, admissions) = actual_index();
    let bytes = index.encode().unwrap();
    let allowance = NativeAllowance::new(1024 * 1024).unwrap();
    let baseline = allowance.retained_bytes();
    let decoded = assert_precharged(&allowance, || {
        BoundedExactAdmissions::decode(&bytes, limits(), &allowance)
    })
    .unwrap();
    for admission in &admissions {
        assert_eq!(decoded.get(&admission.key), Some(&admission.operation_id));
    }
    drop(bytes);
    allowance.finish_turn();
    assert!(allowance.retained_bytes() >= baseline + limits().allocation_bytes);
    assert_eq!(
        decoded.get(&admissions[0].key),
        Some(&admissions[0].operation_id)
    );
    drop(decoded);
    assert_eq!(allowance.retained_bytes(), baseline);
}

#[test]
fn exact_resource_rejects_duplicate_unsorted_trailing_and_announced_count_forms() {
    let (index, admissions) = actual_index();
    let first = postcard::to_allocvec(&(&admissions[0].key, admissions[0].operation_id)).unwrap();
    let second = postcard::to_allocvec(&(&admissions[1].key, admissions[1].operation_id)).unwrap();
    let mut duplicate = vec![2];
    duplicate.extend(&first);
    duplicate.extend(&first);
    let mut reversed = vec![2];
    reversed.extend(&second);
    reversed.extend(&first);
    let mut trailing = index.encode().unwrap();
    trailing.push(0);
    let allowance = NativeAllowance::new(1024 * 1024).unwrap();
    for bytes in [
        duplicate,
        reversed,
        trailing,
        vec![0x81, 0x80, 0x04],
        vec![1, 0xff, 0xff, 0xff, 0xff, 0x7f],
    ] {
        assert!(ExactAdmissions::decode(&bytes).is_err());
        let before = allowance.retained_bytes();
        assert!(matches!(
            assert_precharged(&allowance, || {
                BoundedExactAdmissions::decode(&bytes, limits(), &allowance)
            }),
            Err(PeerResourceError::Malformed) | Err(PeerResourceError::Capacity)
        ));
        assert_eq!(allowance.retained_bytes(), before);
    }
}

#[test]
fn exact_resource_small_output_budget_refuses_before_owned_key_growth() {
    let (index, _) = actual_index();
    let bytes = index.encode().unwrap();
    let allowance = NativeAllowance::new(1024 * 1024).unwrap();
    assert!(matches!(
        assert_precharged(&allowance, || {
            BoundedExactAdmissions::decode(
                &bytes,
                OuterLimits {
                    allocation_bytes: 128,
                    ..limits()
                },
                &allowance,
            )
        }),
        Err(PeerResourceError::Capacity)
    ));
}

#[test]
fn exact_resource_canonical_entry_size_uses_no_temporary_encoded_allocation() {
    let (_, admissions) = actual_index();
    let index = ExactAdmissions::default();
    let expected = postcard::to_allocvec(&(&admissions[0].key, admissions[0].operation_id))
        .unwrap()
        .len();
    let allowance = NativeAllowance::new(4096).unwrap();
    assert_eq!(
        assert_precharged(&allowance, || index.check(&admissions[0])).unwrap(),
        expected
    );
}
