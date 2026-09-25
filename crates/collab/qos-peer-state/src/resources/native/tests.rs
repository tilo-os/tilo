use super::*;
use crate::native_footprint_vendor_tests::assert_precharged;
use automerge::resource_limits::NativeAllowance;
use automerge::transaction::Transactable;
use automerge::{ROOT, ReadDoc};

fn stored(value: &str) -> Vec<u8> {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "value", value).unwrap();
    doc.commit();
    doc.save_with_options(SaveOptions {
        deflate: false,
        retain_orphans: true,
    })
}
fn context() -> (NativeAllowance, NativeAllocationSet, NativeWorkTurn) {
    let allowance = NativeAllowance::new(16 * 1024 * 1024).unwrap();
    let allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = allocations.work_turn(allowance.clone()).unwrap();
    (allowance, allocations, turn)
}

#[test]
fn bounded_native_storage_aggregates_normalized_input_across_documents() {
    let first = stored("first");
    let second = stored("second");
    let (allowance, mut allocations, turn) = context();
    let mut batch =
        NativeBatch::new(&mut allocations, &turn, first.len() + second.len() - 1).unwrap();
    let document = assert_precharged(&allowance, || batch.load(&first)).unwrap();
    assert!(matches!(
        assert_precharged(&allowance, || batch.load(&second)),
        Err(NativeResourceError::NormalizedBytes)
    ));
    // Dropping a materialized document does not reset this turn's cumulative
    // normalization policy or let a later document borrow its old input quota.
    drop(document);
    assert!(matches!(
        batch.load(&second),
        Err(NativeResourceError::NormalizedBytes)
    ));
}

#[test]
fn bounded_native_storage_detaches_input_and_captures_under_exact_output_limit() {
    let input = stored("ordinary application value 🦀");
    let expected = AutoCommit::load(&input).unwrap().get_heads();
    let (allowance, mut allocations, turn) = context();
    let document = assert_precharged(&allowance, || {
        NativeBatch::new(&mut allocations, &turn, input.len())
            .unwrap()
            .load(&input)
    })
    .unwrap();
    drop(input);
    let capture = assert_precharged(&allowance, || document.capture(32 * 1024, &turn)).unwrap();
    assert_eq!(
        AutoCommit::load(capture.bytes()).unwrap().get_heads(),
        expected
    );
    let length = capture.bytes().len();
    let exact = assert_precharged(&allowance, || document.capture(length, &turn)).unwrap();
    assert_eq!(capture.bytes(), exact.bytes());
    let before = allowance.retained_bytes();
    assert!(matches!(
        assert_precharged(&allowance, || document.capture(length - 1, &turn)),
        Err(NativeResourceError::WorkBytes)
    ));
    assert_eq!(allowance.retained_bytes(), before);
    drop(document);
    drop(exact);
    assert_eq!(
        AutoCommit::load(capture.bytes()).unwrap().get_heads(),
        expected
    );
}

#[test]
fn bounded_native_storage_candidate_uses_same_domain_and_current_turn_credit() {
    let input = stored("immutable source");
    let (first_credit, mut allocations, first_turn) = context();
    let document = NativeBatch::new(&mut allocations, &first_turn, input.len())
        .unwrap()
        .load(&input)
        .unwrap();
    first_credit.finish_turn();
    let current = NativeAllowance::new(16 * 1024 * 1024).unwrap();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let clone =
        assert_precharged(&current, || document.candidate(&mut allocations, &turn)).unwrap();
    assert!(current.retained_bytes() > 0);
    let (_, mut foreign_registry, foreign_turn) = context();
    assert!(matches!(
        document.candidate(&mut foreign_registry, &foreign_turn),
        Err(NativeResourceError::Unaccounted)
    ));
    assert!(matches!(
        document.capture(1024, &foreign_turn),
        Err(NativeResourceError::Unaccounted)
    ));
    let saved = clone.capture(1024, &turn).unwrap();
    assert_eq!(
        AutoCommit::load(saved.bytes())
            .unwrap()
            .get(ROOT, "value")
            .unwrap()
            .unwrap()
            .0
            .to_str(),
        Some("immutable source")
    );
}

#[test]
fn bounded_native_storage_read_and_exports_keep_actual_document_custody() {
    use crate::room::read::{LegacyRead, RoomRead};
    let input = stored("borrowed application scalar");
    let mut legacy = AutoCommit::load(&input).unwrap();
    let expected_heads = legacy.get_heads();
    let expected_changes = legacy
        .get_changes(&[])
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect::<Vec<_>>();
    let (allowance, mut allocations, turn) = context();
    let document = NativeBatch::new(&mut allocations, &turn, input.len())
        .unwrap()
        .load(&input)
        .unwrap();
    let read = assert_precharged(&allowance, || document.read(&turn)).unwrap();
    assert!(!document.has_missing_dependencies());
    let root = RoomRead::root(&read);
    let mut keys = 0;
    assert_eq!(
        assert_precharged(&allowance, || RoomRead::visit_keys(&read, &root, |key| {
            assert_eq!(key, "value");
            keys += 1;
            Ok::<_, ()>(())
        }))
        .unwrap(),
        1
    );
    assert_eq!(keys, 1);
    let legacy_read = LegacyRead(&legacy);
    assert_eq!(
        RoomRead::visit_keys(&legacy_read, &RoomRead::root(&legacy_read), |key| {
            assert_eq!(key, "value");
            Ok::<_, ()>(())
        })
        .unwrap(),
        1
    );
    let heads = assert_precharged(&allowance, || document.heads(&turn)).unwrap();
    let changes = assert_precharged(&allowance, || document.changes_since(&[], &turn)).unwrap();
    drop(read);
    drop(document);
    assert_eq!(heads.as_slice(), expected_heads);
    assert_eq!(changes.len(), expected_changes.len());
    for (index, expected) in expected_changes.iter().enumerate() {
        assert_eq!(changes.raw_bytes(index).unwrap(), expected);
    }
}
