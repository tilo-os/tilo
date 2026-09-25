//! These cases enter through public bounded apply, including its ownership
//! accounting. They do not call a private validator or a production test shim.
use super::*;
use automerge::marks::{ExpandMark, Mark};
use automerge::resource_limits::{NativeApplyResult, NativeDependencyPolicy, NativeMutationError};
use automerge::{Change, ChangeHash, ExpandedChange, ObjType, SaveOptions, ScalarValue};

fn apply(source: &mut AutoCommit, incoming: &[u8], expected: &[ChangeHash]) -> AutoCommit {
    let bytes = source.save();
    let original_heads = source.get_heads();
    let allowance = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let before = allowance.available_bytes();
    let saved = assert_precharged(&allowance, || -> Result<_, NativeMutationError> {
        let mut registry = NativeAllocationSet::new(allowance.clone())?;
        let turn = registry.work_turn(allowance.clone())?;
        let original = turn
            .normalize_input(&bytes, &NativeLimits::default())?
            .reconstruct(&mut registry, &turn)?;
        let candidate = original.clone_bounded(&mut registry, &turn)?;
        let input = turn.normalize_input(incoming, &NativeLimits::default())?;
        let NativeApplyResult::Applied {
            document,
            changed,
            missing_dependencies,
        } = candidate.apply_checked_bounded(
            input,
            NativeDependencyPolicy::RequireComplete,
            &mut registry,
            &turn,
        )?
        else {
            panic!("complete ordinary history must apply")
        };
        assert!(changed);
        assert!(!missing_dependencies);
        assert_eq!(document.base_heads_bounded(&turn)?.as_slice(), expected);
        assert_eq!(
            original.base_heads_bounded(&turn)?.as_slice(),
            original_heads
        );
        Ok(document.save_bounded(&turn, SaveOptions::default())?)
    })
    .unwrap();
    let restored = AutoCommit::load(saved.bytes()).unwrap();
    drop(saved);
    assert_eq!(
        allowance.available_bytes(),
        before,
        "all disposable custody is released"
    );
    restored
}

fn refused(source: &mut AutoCommit, expanded: ExpandedChange) {
    let incoming = Change::from(expanded);
    let bytes = source.save();
    let original_heads = source.get_heads();
    let original_changes = source.get_changes(&[]);
    let allowance = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let before = allowance.available_bytes();
    assert_precharged(&allowance, || -> Result<(), NativeMutationError> {
        let mut registry = NativeAllocationSet::new(allowance.clone())?;
        let turn = registry.work_turn(allowance.clone())?;
        let original = turn
            .normalize_input(&bytes, &NativeLimits::default())?
            .reconstruct(&mut registry, &turn)?;
        let candidate = original.clone_bounded(&mut registry, &turn)?;
        let input = turn.normalize_input(incoming.raw_bytes(), &NativeLimits::default())?;
        let result = candidate.apply_checked_bounded(
            input,
            NativeDependencyPolicy::RequireComplete,
            &mut registry,
            &turn,
        );
        assert!(
            matches!(result, Err(NativeMutationError::InvalidOperation)),
            "semantic refusal must remain distinct from resource refusal: {result:?}"
        );
        assert_eq!(
            original.base_heads_bounded(&turn)?.as_slice(),
            original_heads
        );
        let changes = original.changes_since_bounded(&[], &turn)?;
        assert_eq!(changes.len(), original_changes.len());
        for (index, expected) in original_changes.iter().enumerate() {
            assert_eq!(changes.hash(index), Some(expected.hash()));
            assert_eq!(changes.raw_bytes(index), Some(expected.raw_bytes()));
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(
        allowance.available_bytes(),
        before,
        "refused candidate and scratch are released"
    );
}

#[test]
fn incoming_target_validation_preserves_nested_objects_and_list_edits() {
    let mut source = AutoCommit::new();
    let map = source.put_object(ROOT, "map", ObjType::Map).unwrap();
    source.put(&map, "value", "before").unwrap();
    let list = source.put_object(ROOT, "list", ObjType::List).unwrap();
    source.insert(&list, 0, "a").unwrap();
    source.insert(&list, 1, "b").unwrap();
    source.commit();
    let mut peer = source.fork();
    source.put(&map, "local", true).unwrap();
    source.commit();
    peer.put(&map, "value", "after").unwrap();
    peer.insert(&list, 1, "new").unwrap();
    peer.put(&list, 0, "updated").unwrap();
    peer.delete(&list, 2).unwrap();
    let child = peer.put_object(&map, "child", ObjType::Map).unwrap();
    peer.put(&child, "ready", true).unwrap();
    let incoming = peer.save();
    let mut control = source.clone();
    control.merge(&mut peer).unwrap();
    let restored = apply(&mut source, &incoming, &control.get_heads());
    assert_eq!(
        restored.get(&map, "value").unwrap().unwrap().0.to_str(),
        Some("after")
    );
    assert_eq!(
        restored.get(&map, "local").unwrap().unwrap().0.to_bool(),
        Some(true)
    );
    assert_eq!(
        restored.get(&child, "ready").unwrap().unwrap().0.to_bool(),
        Some(true)
    );
    assert_eq!(restored.length(&list), 2);
    assert_eq!(
        restored.get(&list, 0).unwrap().unwrap().0.to_str(),
        Some("updated")
    );
    assert_eq!(
        restored.get(&list, 1).unwrap().unwrap().0.to_str(),
        Some("new")
    );
}

#[test]
fn incoming_target_validation_preserves_existing_and_new_counters() {
    let mut source = AutoCommit::new();
    source.put(ROOT, "count", ScalarValue::counter(5)).unwrap();
    source.commit();
    let mut peer = source.fork();
    source.increment(ROOT, "count", 2).unwrap();
    source.commit();
    peer.increment(ROOT, "count", 3).unwrap();
    peer.put(ROOT, "new_count", ScalarValue::counter(7))
        .unwrap();
    peer.commit();
    peer.increment(ROOT, "new_count", 3).unwrap();
    peer.increment(ROOT, "new_count", -1).unwrap();
    let incoming = peer.save();
    let mut control = source.clone();
    control.merge(&mut peer).unwrap();
    let restored = apply(&mut source, &incoming, &control.get_heads());
    assert_eq!(
        restored.get(ROOT, "count").unwrap().unwrap().0.to_i64(),
        Some(10)
    );
    assert_eq!(
        restored.get(ROOT, "new_count").unwrap().unwrap().0.to_i64(),
        Some(9)
    );
}

#[test]
fn incoming_target_validation_preserves_increment_over_counter_scalar_conflict() {
    let mut source = AutoCommit::new();
    source.put(ROOT, "base", true).unwrap();
    source.commit();
    let mut other = source.fork();
    source.put(ROOT, "count", ScalarValue::counter(5)).unwrap();
    source.commit();
    other.put(ROOT, "count", "conflicting value").unwrap();
    other.commit();
    source.merge(&mut other).unwrap();
    assert_eq!(source.get_all(ROOT, "count").unwrap().len(), 2);
    let mut peer = source.fork();
    peer.increment(ROOT, "count", 2).unwrap();
    let incoming = peer.save();
    let expected = peer.get_heads();
    let restored = apply(&mut source, &incoming, &expected);
    assert_eq!(
        restored.get(ROOT, "count").unwrap().unwrap().0.to_i64(),
        Some(7)
    );
    assert_eq!(restored.get_all(ROOT, "count").unwrap().len(), 1);
}

#[test]
fn incoming_target_validation_rejects_skipped_existing_actor_sequence() {
    let mut source = AutoCommit::new();
    source.put(ROOT, "value", 1).unwrap();
    source.commit();
    let mut peer = source.clone();
    peer.put(ROOT, "value", 2).unwrap();
    let mut change = peer.get_last_local_change().unwrap().decode();
    change.seq += 1;
    refused(&mut source, change);
}

#[test]
fn incoming_target_validation_rejects_reused_operation_counter() {
    let mut source = AutoCommit::new();
    source.put(ROOT, "value", 1).unwrap();
    source.commit();
    let mut peer = source.clone();
    peer.put(ROOT, "value", 2).unwrap();
    let mut change = peer.get_last_local_change().unwrap().decode();
    change.start_op = std::num::NonZeroU64::new(1).unwrap();
    refused(&mut source, change);
}

#[test]
fn incoming_target_validation_requires_previous_actor_change_in_causal_history() {
    let mut source = AutoCommit::new();
    source.put(ROOT, "existing", 1).unwrap();
    source.commit();
    let mut peer = source.clone();
    peer.put(ROOT, "new_property", 2).unwrap();
    let mut change = peer.get_last_local_change().unwrap().decode();
    // ROOT and a new map key need no operation references. The author chain,
    // independently of object/predecessor checks, still needs its prior seq.
    change.deps.clear();
    refused(&mut source, change);
}

#[test]
fn incoming_target_validation_rejects_object_reference_outside_causal_history() {
    let mut source = AutoCommit::new();
    let object = source.put_object(ROOT, "map", ObjType::Map).unwrap();
    source.put(&object, "value", 1).unwrap();
    source.commit();
    let mut peer = source.fork();
    peer.put(&object, "value", 2).unwrap();
    let mut change = peer.get_last_local_change().unwrap().decode();
    change.deps.clear();
    refused(&mut source, change);
}

#[test]
fn incoming_target_validation_rejects_predecessor_from_another_property() {
    let mut source = AutoCommit::new();
    source.put(ROOT, "left", 1).unwrap();
    source.put(ROOT, "right", 2).unwrap();
    source.commit();
    let mut left = source.fork();
    left.put(ROOT, "left", 3).unwrap();
    let mut right = source.fork();
    right.put(ROOT, "right", 4).unwrap();
    let mut change = left.get_last_local_change().unwrap().decode();
    change.operations[0].key = right.get_last_local_change().unwrap().decode().operations[0]
        .key
        .clone();
    refused(&mut source, change);
}

#[test]
fn incoming_target_validation_rejects_sequence_anchor_from_another_object() {
    let mut source = AutoCommit::new();
    let a = source.put_object(ROOT, "a", ObjType::List).unwrap();
    let b = source.put_object(ROOT, "b", ObjType::List).unwrap();
    source.insert(&a, 0, "a").unwrap();
    source.insert(&b, 0, "b").unwrap();
    source.commit();
    let mut left = source.fork();
    left.insert(&a, 1, "new").unwrap();
    let mut right = source.fork();
    right.put(&b, 0, "updated").unwrap();
    let mut change = left.get_last_local_change().unwrap().decode();
    change.operations[0].obj = right.get_last_local_change().unwrap().decode().operations[0]
        .obj
        .clone();
    refused(&mut source, change);
}

#[test]
fn incoming_target_validation_rejects_mark_end_without_matching_begin() {
    let mut source = AutoCommit::new();
    let text = source.put_object(ROOT, "text", ObjType::Text).unwrap();
    source.splice_text(&text, 0, 0, "abcdef").unwrap();
    source.commit();
    let mut peer = source.fork();
    peer.mark(
        &text,
        Mark::new("bold".into(), true, 0, 3),
        ExpandMark::Both,
    )
    .unwrap();
    let mut change = peer.get_last_local_change().unwrap().decode();
    assert_eq!(change.operations.len(), 2);
    change.operations.remove(0);
    refused(&mut source, change);
}

#[test]
fn incoming_target_validation_rejects_unrepresentable_counter_sum() {
    let mut source = AutoCommit::new();
    source
        .put(ROOT, "count", ScalarValue::counter(i64::MAX))
        .unwrap();
    source.commit();
    let mut peer = source.fork();
    peer.increment(ROOT, "count", -1).unwrap();
    let mut value = serde_json::to_value(peer.get_last_local_change().unwrap().decode()).unwrap();
    // Use the existing typed ExpandedChange encoding. The only altered field
    // is an ordinary i64 increment; no byte parser fixture or unchecked native
    // apply is involved in constructing the case.
    value["ops"][0]["value"] = serde_json::json!(1);
    let change = serde_json::from_value(value).unwrap();
    refused(&mut source, change);
}

#[test]
fn incoming_target_validation_preserves_hidden_counter_at_representable_bound() {
    // Native control for the raw-base arithmetic concern: the merged counter
    // stays representable while raw base plus the fresh increment does not.
    let mut source = AutoCommit::new();
    source.put(ROOT, "base", true).unwrap();
    source.commit();
    let mut other = source.fork();
    source
        .put(ROOT, "count", ScalarValue::counter(i64::MAX))
        .unwrap();
    source.commit();
    source.increment(ROOT, "count", -1).unwrap();
    source.commit();
    // A later scalar with a larger operation counter conflicts with the counter.
    for index in 0..8 {
        other
            .put(ROOT, format!("filler_{index}"), index as i64)
            .unwrap();
    }
    other.put(ROOT, "count", "later scalar").unwrap();
    other.commit();
    source.merge(&mut other).unwrap();
    assert_eq!(source.get_all(ROOT, "count").unwrap().len(), 2);
    let mut peer = source.fork();
    peer.increment(ROOT, "count", 1).unwrap();
    let incoming = peer.save();
    let expected = peer.get_heads();
    let restored = apply(&mut source, &incoming, &expected);
    let values = restored.get_all(ROOT, "count").unwrap();
    assert!(
        values
            .iter()
            .any(|(value, _)| value.to_scalar() == Some(&ScalarValue::counter(i64::MAX))),
        "the merged counter keeps its representable value: {values:?}"
    );
}
