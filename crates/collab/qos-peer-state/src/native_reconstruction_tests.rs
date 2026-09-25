use crate::native_footprint_vendor_tests::assert_precharged;
use automerge::AutoCommit;
use automerge::marks::{ExpandMark, Mark};
use automerge::resource_limits::{
    NativeAllocationSet, NativeAllowance, NativeLimits, NativeScalar, NativeSelection, NativeValue,
};
use automerge::resource_limits::{NativeOwned, NativeResourceError, NativeWorkTurn};
use automerge::transaction::Transactable;
use automerge::{ActorId, ObjType, ROOT, ReadDoc, SaveOptions, ScalarValue};

fn reconstruct(
    bytes: &[u8],
    credit: &NativeAllowance,
) -> (NativeOwned<AutoCommit>, NativeAllocationSet, NativeWorkTurn) {
    let result = assert_precharged(credit, || {
        let mut registry = NativeAllocationSet::new(credit.clone())?;
        let turn = registry.work_turn(credit.clone())?;
        let checked =
            AutoCommit::inspect_bounded(bytes, &NativeLimits::default(), &mut credit.clone())?;
        let normalized = checked.normalize(credit)?;
        let owned = normalized.reconstruct(&mut registry, &turn)?;
        Ok::<_, NativeResourceError>((owned, registry, turn))
    });
    result.unwrap()
}

fn same_history(mut source: AutoCommit, bytes: &[u8]) {
    let heads = source.get_heads();
    let changes: Vec<_> = source
        .get_changes(&[])
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    let credit = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let (owned, _, turn) = reconstruct(bytes, &credit);
    let saved = assert_precharged(&credit, || {
        owned.save_bounded(
            &turn,
            SaveOptions {
                deflate: false,
                retain_orphans: true,
            },
        )
    })
    .unwrap();
    let mut restored = AutoCommit::load(saved.bytes()).unwrap();
    assert_eq!(restored.get_heads(), heads);
    let actual: Vec<_> = restored
        .get_changes(&[])
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    assert_eq!(actual, changes);
}

#[test]
fn checked_native_reconstruct_valid_map_retains_charge() {
    let mut original = AutoCommit::new();
    original.put(automerge::ROOT, "message", "hello").unwrap();
    let bytes = original.save();
    let mut allowance = NativeAllowance::new(4 * 1024 * 1024).unwrap();
    let mut allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = allocations.work_turn(allowance.clone()).unwrap();
    let checked =
        AutoCommit::inspect_bounded(&bytes, &NativeLimits::default(), &mut allowance).unwrap();
    let normalized = checked.normalize(&allowance).unwrap();
    assert_eq!(normalized.original_bytes(), bytes);
    let owned = normalized.reconstruct(&mut allocations, &turn).unwrap();
    let retained = allowance.retained_bytes();
    let read = owned.read(&turn).unwrap();
    let mut observed = false;
    read.visit_property(
        read.root(),
        "message",
        NativeSelection::Winner,
        |value, _| {
            assert_eq!(value, NativeValue::Scalar(NativeScalar::Str("hello")));
            observed = true;
            Ok::<_, ()>(())
        },
    )
    .unwrap();
    assert!(observed);
    assert_eq!(allowance.retained_bytes(), retained);
    assert!(retained > 0);
    drop(read);
    drop(owned);
    allocations.reconcile();
    assert!(allowance.retained_bytes() < retained);
}

#[test]
fn checked_native_reconstruct_merged_text_marks_and_counter_history() {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(&[1_u8][..]));
    let text = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
    doc.splice_text(&text, 0, 0, "A normal collaborative text document.")
        .unwrap();
    let list = doc.put_object(ROOT, "items", ObjType::List).unwrap();
    doc.insert(&list, 0, "first").unwrap();
    doc.put(ROOT, "counter", ScalarValue::Counter(2.into()))
        .unwrap();
    doc.commit();
    let mut peer = doc.fork().with_actor(ActorId::from(&[2_u8][..]));
    doc.mark(
        &text,
        Mark::new("emphasis".into(), true, 2, 8),
        ExpandMark::Both,
    )
    .unwrap();
    doc.increment(ROOT, "counter", 3).unwrap();
    peer.splice_text(&text, 1, 0, " shared").unwrap();
    peer.insert(&list, 1, "second").unwrap();
    peer.commit();
    doc.merge(&mut peer).unwrap();
    doc.delete(&list, 0).unwrap();
    doc.commit();
    let bytes = doc.save();
    same_history(doc, &bytes);
}

#[test]
fn checked_native_reconstruct_normal_long_text_and_empty_history() {
    let mut empty = AutoCommit::new();
    let bytes = empty.save();
    same_history(empty, &bytes);
    let credit = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let _ = reconstruct(&[], &credit);
    let mut doc = AutoCommit::new();
    let text = doc.put_object(ROOT, "notes", ObjType::Text).unwrap();
    doc.splice_text(&text, 0, 0, &"ordinary text ".repeat(384))
        .unwrap();
    doc.commit();
    doc.mark(
        &text,
        Mark::new("emphasis".into(), "normal", 10, 100),
        ExpandMark::Both,
    )
    .unwrap();
    doc.commit();
    let bytes = doc.save();
    same_history(doc, &bytes);
}

#[test]
fn checked_native_reconstruct_change_bundle_and_document_tail() {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(&[3_u8][..]));
    doc.put(ROOT, "a", 1_i64).unwrap();
    doc.commit();
    let prefix = doc.save();
    let before = doc.get_heads();
    doc.put(ROOT, "b", 2_i64).unwrap();
    doc.commit();
    let mut full_document_tail = prefix.clone();
    full_document_tail.extend(doc.save());
    same_history(doc.clone(), &full_document_tail);
    let mut combined = prefix;
    combined.extend(doc.save_after(&before));
    same_history(doc.clone(), &combined);
    let raw: Vec<u8> = doc
        .get_changes(&[])
        .iter()
        .flat_map(|change| change.raw_bytes().iter().copied())
        .collect();
    same_history(doc.clone(), &raw);
    let hashes: Vec<_> = doc
        .get_changes(&[])
        .iter()
        .map(|change| change.hash())
        .collect();
    let bundle = doc.bundle(hashes).unwrap();
    same_history(doc, bundle.bytes());
}

#[test]
fn checked_native_reconstruct_compressed_changes_keep_raw_history() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "notes", "normal repeated sentence. ".repeat(256))
        .unwrap();
    doc.commit();
    let mut change = doc.get_last_local_change().unwrap().clone();
    let incremental = change.bytes().into_owned();
    assert_eq!(
        incremental[8], 2,
        "normal incremental save uses compressed change storage"
    );
    same_history(doc, &incremental);
}

#[test]
fn checked_native_reconstruct_aggregate_refusal_preserves_existing_document() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "value", "existing").unwrap();
    let bytes = doc.save();
    let credit = NativeAllowance::new(4 * 1024 * 1024).unwrap();
    let (owned, mut registry, turn) = reconstruct(&bytes, &credit);
    let before = credit.retained_bytes();
    let other_work = credit
        .reserve(credit.available_bytes() - 16 * 1024)
        .unwrap();
    let result = assert_precharged(&credit, || {
        AutoCommit::inspect_bounded(&bytes, &NativeLimits::default(), &mut credit.clone())?
            .normalize(&credit)?
            .reconstruct(&mut registry, &turn)
    });
    assert!(matches!(result, Err(NativeResourceError::WorkBytes)));
    drop(other_work);
    assert_eq!(credit.retained_bytes(), before);
    let read = owned.read(&turn).unwrap();
    let mut found = false;
    read.visit_property(read.root(), "value", NativeSelection::Winner, |value, _| {
        assert_eq!(value, NativeValue::Scalar(NativeScalar::Str("existing")));
        found = true;
        Ok::<_, ()>(())
    })
    .unwrap();
    assert!(found);
}

#[test]
fn checked_native_reconstruct_preserves_legitimately_queued_change() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "value", "initial").unwrap();
    doc.commit();
    let mut peer = doc.fork();
    peer.put(ROOT, "value", "middle").unwrap();
    let missing = peer.get_last_local_change().unwrap().clone();
    peer.put(ROOT, "value", "final").unwrap();
    let queued = peer.get_last_local_change().unwrap().clone();
    doc.apply_changes([queued.clone()]).unwrap();
    let bytes = doc.save();
    let credit = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let (owned, _, turn) = reconstruct(&bytes, &credit);
    let saved = assert_precharged(&credit, || {
        owned.save_bounded(
            &turn,
            SaveOptions {
                deflate: false,
                retain_orphans: true,
            },
        )
    })
    .unwrap();
    assert!(saved.bytes().ends_with(queued.raw_bytes()));
    let mut restored = AutoCommit::load(saved.bytes()).unwrap();
    restored.apply_changes([missing]).unwrap();
    assert_eq!(
        restored.get(ROOT, "value").unwrap().unwrap().0,
        automerge::Value::from("final")
    );
}

#[test]
fn checked_native_reconstruct_small_credit_refuses_without_retention() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "valid", "ordinary").unwrap();
    let bytes = doc.save();
    let mut credit = NativeAllowance::new(32 * 1024).unwrap();
    let mut registry = NativeAllocationSet::new(credit.clone()).unwrap();
    let turn = registry.work_turn(credit.clone()).unwrap();
    let before = credit.retained_bytes();
    let result = assert_precharged(&credit, || {
        AutoCommit::inspect_bounded(&bytes, &NativeLimits::default(), &mut credit.clone())?
            .normalize(&credit)?
            .reconstruct(&mut registry, &turn)
    });
    assert!(matches!(result, Err(NativeResourceError::WorkBytes)));
    registry.reconcile();
    assert_eq!(credit.retained_bytes(), before);
    let mut limits = NativeLimits::default();
    limits.max_operations = 0;
    assert!(matches!(
        AutoCommit::inspect_bounded(&bytes, &limits, &mut credit),
        Err(NativeResourceError::Operations)
    ));
}

#[test]
fn checked_native_reconstruct_rejects_wrong_turn_and_survives_original_turn_end() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "value", "retained").unwrap();
    let bytes = doc.save();
    let credit = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let (owned, mut registry, old_turn) = reconstruct(&bytes, &credit);
    let retained = credit.retained_bytes();
    credit.finish_turn();
    drop(old_turn);
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let turn = registry.work_turn(current.clone()).unwrap();
    let read = owned.read(&turn).unwrap();
    let mut found = false;
    read.visit_property(read.root(), "value", NativeSelection::Winner, |value, _| {
        assert_eq!(value, NativeValue::Scalar(NativeScalar::Str("retained")));
        found = true;
        Ok::<_, ()>(())
    })
    .unwrap();
    assert!(found);
    drop(read);
    let wrong = NativeAllocationSet::new(current.clone()).unwrap();
    let wrong_turn = wrong.work_turn(current.clone()).unwrap();
    let normalized =
        AutoCommit::inspect_bounded(&bytes, &NativeLimits::default(), &mut current.clone())
            .unwrap()
            .normalize(&current)
            .unwrap();
    assert!(matches!(
        normalized.reconstruct(&mut registry, &wrong_turn),
        Err(NativeResourceError::Unaccounted)
    ));
    drop(owned);
    registry.reconcile();
    assert!(credit.retained_bytes() < retained);
}

#[test]
fn checked_native_reconstruct_rejects_different_turn_ledger() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "value", "ordinary").unwrap();
    let bytes = doc.save();
    let source = NativeAllowance::new(4 * 1024 * 1024).unwrap();
    let source_before = source.retained_bytes();
    let current = NativeAllowance::new(4 * 1024 * 1024).unwrap();
    let mut registry = NativeAllocationSet::new(current.clone()).unwrap();
    let turn = registry.work_turn(current.clone()).unwrap();
    let normalized =
        AutoCommit::inspect_bounded(&bytes, &NativeLimits::default(), &mut source.clone())
            .unwrap()
            .normalize(&source)
            .unwrap();
    let before = current.retained_bytes();
    assert!(matches!(
        normalized.reconstruct(&mut registry, &turn),
        Err(NativeResourceError::Unaccounted)
    ));
    assert_eq!(source.retained_bytes(), source_before);
    assert_eq!(current.retained_bytes(), before);
}

#[test]
fn checked_native_reconstruct_ordinary_branch_merge_clock_frontiers() {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(&[1_u8][..]));
    doc.put(ROOT, "shared", "base").unwrap();
    doc.commit();
    let base = doc.get_heads();
    let mut branches = Vec::new();
    for actor in 2_u8..64 {
        let mut branch = doc
            .fork_at(&base)
            .unwrap()
            .with_actor(ActorId::from(&[actor][..]));
        branch
            .put(ROOT, format!("ordinary-{actor}"), u64::from(actor))
            .unwrap();
        branch.commit();
        branches.push(branch);
    }
    for mut branch in branches {
        doc.merge(&mut branch).unwrap();
    }
    doc.put(ROOT, "merged", true).unwrap();
    doc.commit();
    assert_eq!(doc.get_changes(&[]).len(), 64);
    let bytes = doc.save();
    same_history(doc, &bytes);
}
