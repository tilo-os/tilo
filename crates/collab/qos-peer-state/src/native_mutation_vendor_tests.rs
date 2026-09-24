//! Public bounded operation tests using the shared actual allocation meter.
mod target_validation;

use crate::native_footprint_vendor_tests::assert_precharged;
use automerge::resource_limits::{
    NativeAllocationSet, NativeAllowance, NativeLimits, NativeOwned, NativeResourceError,
    NativeWorkTurn,
};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc};

fn normal_document() -> (Vec<u8>, Vec<automerge::ChangeHash>) {
    let mut document = AutoCommit::new();
    document
        .put(ROOT, "message", "ordinary shared text ".repeat(256))
        .unwrap();
    document.put(ROOT, "revision", 7_i64).unwrap();
    let bytes = document.save();
    (bytes, document.get_heads())
}

#[test]
fn normalize_input_charges_current_turn_and_preserves_document() {
    let (bytes, heads) = normal_document();
    let residence = NativeAllowance::new(1024 * 1024).unwrap();
    let registry = NativeAllocationSet::new(residence.clone()).unwrap();
    let work = NativeAllowance::new(4 * 1024 * 1024).unwrap();
    let work_before_turn = work.available_bytes();
    let turn = registry.work_turn(work.clone()).unwrap();
    let residence_before = residence.available_bytes();
    let work_before = work.available_bytes();

    let normalized = assert_precharged(&work, || {
        turn.normalize_input(&bytes, &NativeLimits::default())
    })
    .unwrap();
    assert_eq!(normalized.original_bytes(), bytes);
    assert!(
        normalized.bytes().len() > bytes.len(),
        "ordinary compressed input expands"
    );
    assert_eq!(residence.available_bytes(), residence_before);
    assert_eq!(
        work_before - work.available_bytes(),
        normalized.bytes().len()
    );
    let mut restored = AutoCommit::load(normalized.bytes()).unwrap();
    assert_eq!(restored.get_heads(), heads);
    assert_eq!(
        restored.get(ROOT, "revision").unwrap().unwrap().0.to_i64(),
        Some(7)
    );
    drop(restored);
    drop(turn);
    drop(registry);
    assert_eq!(
        work_before_turn - work.available_bytes(),
        normalized.bytes().len()
    );
    drop(normalized);
    assert_eq!(work.available_bytes(), work_before_turn);
}

#[test]
fn normalize_input_respects_current_turn_seal() {
    let (bytes, _) = normal_document();
    let residence = NativeAllowance::new(1024 * 1024).unwrap();
    let registry = NativeAllocationSet::new(residence.clone()).unwrap();
    let work = NativeAllowance::new(4 * 1024 * 1024).unwrap();
    let turn = registry.work_turn(work.clone()).unwrap();
    work.finish_turn();
    let result = assert_precharged(&work, || {
        turn.normalize_input(&bytes, &NativeLimits::default())
    });
    assert!(matches!(result, Err(NativeResourceError::WorkBytes)));
}

#[test]
fn normalize_input_preserves_caller_structural_quota() {
    let (bytes, _) = normal_document();
    let work = NativeAllowance::new(4 * 1024 * 1024).unwrap();
    let registry = NativeAllocationSet::new(work.clone()).unwrap();
    let turn = registry.work_turn(work.clone()).unwrap();
    let before = work.available_bytes();
    let limits = NativeLimits {
        max_normalized_bytes: 0,
        ..NativeLimits::default()
    };
    let result = assert_precharged(&work, || turn.normalize_input(&bytes, &limits));
    assert!(matches!(result, Err(NativeResourceError::NormalizedBytes)));
    assert_eq!(work.available_bytes(), before);
}

fn retained_document(
    source: &mut AutoCommit,
    allowance: &NativeAllowance,
) -> (NativeOwned<AutoCommit>, NativeAllocationSet, NativeWorkTurn) {
    let bytes = source.save();
    assert_precharged(allowance, || {
        let mut registry = NativeAllocationSet::new(allowance.clone()).unwrap();
        let turn = registry.work_turn(allowance.clone()).unwrap();
        let normalized = turn
            .normalize_input(&bytes, &NativeLimits::default())
            .unwrap();
        let document = normalized.reconstruct(&mut registry, &turn).unwrap();
        (document, registry, turn)
    })
}

fn concurrent_document() -> AutoCommit {
    let mut source = AutoCommit::new();
    source.put(ROOT, "base", "shared").unwrap();
    source.commit();
    let mut other = source.fork();
    source.put(ROOT, "left", "one").unwrap();
    other.put(ROOT, "right", "two").unwrap();
    source.merge(&mut other).unwrap();
    assert_eq!(source.get_heads().len(), 2);
    source
}

#[test]
fn history_metadata_heads_retain_exact_output_after_source_drop() {
    let mut source = concurrent_document();
    let expected = source.get_heads();
    let allowance = NativeAllowance::new(4 * 1024 * 1024).unwrap();
    let before_fixture = allowance.available_bytes();
    let (document, registry, turn) = retained_document(&mut source, &allowance);
    let before_heads = allowance.available_bytes();
    let heads = assert_precharged(&allowance, || document.base_heads_bounded(&turn)).unwrap();
    assert_eq!(heads.as_slice(), expected);
    let hash_bytes = 2 * std::mem::size_of::<automerge::ChangeHash>();
    assert_eq!(before_heads - allowance.available_bytes(), hash_bytes);
    drop(document);
    drop(registry);
    drop(turn);
    assert_eq!(heads.as_slice(), expected);
    assert_eq!(before_fixture - allowance.available_bytes(), hash_bytes);
    drop(heads);
    assert_eq!(allowance.available_bytes(), before_fixture);
}

#[test]
fn history_metadata_heads_refuse_wrong_domain_and_short_current_credit() {
    let mut source = concurrent_document();
    let residence = NativeAllowance::new(4 * 1024 * 1024).unwrap();
    let (document, registry, _) = retained_document(&mut source, &residence);
    let work = NativeAllowance::new(1024).unwrap();
    let turn = registry.work_turn(work.clone()).unwrap();
    let blocker = work.reserve(work.available_bytes() - 63).unwrap();
    let result = assert_precharged(&work, || document.base_heads_bounded(&turn));
    assert!(matches!(result, Err(NativeResourceError::WorkBytes)));
    drop(blocker);
    let foreign = NativeAllowance::new(1024).unwrap();
    let foreign_registry = NativeAllocationSet::new(foreign.clone()).unwrap();
    let foreign_turn = foreign_registry.work_turn(foreign.clone()).unwrap();
    let result = assert_precharged(&foreign, || document.base_heads_bounded(&foreign_turn));
    assert!(matches!(result, Err(NativeResourceError::Unaccounted)));
    let heads = assert_precharged(&work, || document.base_heads_bounded(&turn)).unwrap();
    assert_eq!(heads.as_slice(), source.get_heads());
}

#[test]
fn history_metadata_missing_dependencies_tracks_real_queued_changes() {
    let mut source = AutoCommit::new();
    source.put(ROOT, "value", "initial").unwrap();
    source.commit();
    let mut peer = source.fork();
    peer.put(ROOT, "value", "middle").unwrap();
    let missing = peer.get_last_local_change().unwrap();
    peer.put(ROOT, "value", "final").unwrap();
    let queued = peer.get_last_local_change().unwrap();
    for expected in [false, true, false] {
        if expected {
            source.apply_changes([queued.clone()]).unwrap();
        }
        let allowance = NativeAllowance::new(4 * 1024 * 1024).unwrap();
        let (document, _, _) = retained_document(&mut source, &allowance);
        assert_eq!(
            assert_precharged(&allowance, || document.has_missing_dependencies()),
            expected,
        );
        if expected {
            source.apply_changes([missing.clone()]).unwrap();
        }
    }
}

fn ordinary_history() -> (AutoCommit, Vec<automerge::ChangeHash>) {
    use automerge::marks::{ExpandMark, Mark};
    use automerge::transaction::CommitOptions;
    use automerge::{ObjType, ScalarValue};
    let mut document = AutoCommit::new();
    let text = document.put_object(ROOT, "text", ObjType::Text).unwrap();
    document
        .splice_text(&text, 0, 0, "An ordinary collaborative note.")
        .unwrap();
    document
        .put(ROOT, "counter", ScalarValue::Counter(3.into()))
        .unwrap();
    document.commit_with(CommitOptions::default().with_message("initial ordinary note"));
    let before = document.get_heads();
    let mut peer = document.fork();
    for index in 0..256 {
        document
            .put(ROOT, format!("entry_{index}"), index as i64)
            .unwrap();
    }
    document
        .put(ROOT, "attachment", ScalarValue::Bytes(vec![17; 4096]))
        .unwrap();
    document.commit_with(CommitOptions::default().with_message("normal multi-operation change"));
    peer.increment(ROOT, "counter", 4).unwrap();
    peer.mark(
        &text,
        Mark::new("emphasis".into(), true, 3, 12),
        ExpandMark::Both,
    )
    .unwrap();
    peer.commit();
    document.merge(&mut peer).unwrap();
    document.delete(ROOT, "entry_0").unwrap();
    document.commit();
    (document, before)
}

#[test]
fn canonical_export_matches_native_raw_changes_and_retains_them() {
    let (mut source, before) = ordinary_history();
    let expected_all: Vec<_> = source
        .get_changes(&[])
        .into_iter()
        .map(|change| (change.hash(), change.raw_bytes().to_vec()))
        .collect();
    let expected_since: Vec<_> = source
        .get_changes(&before)
        .into_iter()
        .map(|change| (change.hash(), change.raw_bytes().to_vec()))
        .collect();
    let allowance = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let before_fixture = allowance.available_bytes();
    let (document, registry, turn) = retained_document(&mut source, &allowance);
    let all = assert_precharged(&allowance, || document.changes_since_bounded(&[], &turn)).unwrap();
    let since = assert_precharged(&allowance, || {
        document.changes_since_bounded(&before, &turn)
    })
    .unwrap();
    let heads = source.get_heads();
    let empty =
        assert_precharged(&allowance, || document.changes_since_bounded(&heads, &turn)).unwrap();
    assert!(empty.is_empty());
    drop(empty);
    drop(document);
    drop(registry);
    drop(turn);
    for (actual, expected) in [(&all, &expected_all), (&since, &expected_since)] {
        assert_eq!(actual.len(), expected.len());
        for (index, (hash, bytes)) in expected.iter().enumerate() {
            assert_eq!(actual.hash(index), Some(*hash));
            assert_eq!(actual.raw_bytes(index), Some(bytes.as_slice()));
        }
        assert_eq!(actual.raw_bytes(actual.len()), None);
    }
    assert!(allowance.available_bytes() < before_fixture);
    drop(all);
    drop(since);
    assert_eq!(allowance.available_bytes(), before_fixture);
}

#[test]
fn canonical_export_refuses_foreign_or_insufficient_current_work() {
    let (mut source, _) = ordinary_history();
    let residence = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let (document, registry, resident_turn) = retained_document(&mut source, &residence);
    let work = NativeAllowance::new(1024).unwrap();
    let turn = registry.work_turn(work.clone()).unwrap();
    let before = work.available_bytes();
    let result = assert_precharged(&work, || document.changes_since_bounded(&[], &turn));
    assert!(matches!(result, Err(NativeResourceError::WorkBytes)));
    assert_eq!(work.available_bytes(), before);
    let foreign = NativeAllowance::new(1024 * 1024).unwrap();
    let foreign_registry = NativeAllocationSet::new(foreign.clone()).unwrap();
    let foreign_turn = foreign_registry.work_turn(foreign.clone()).unwrap();
    let result = assert_precharged(&foreign, || {
        document.changes_since_bounded(&[], &foreign_turn)
    });
    assert!(matches!(result, Err(NativeResourceError::Unaccounted)));
    assert_eq!(
        document
            .base_heads_bounded(&resident_turn)
            .unwrap()
            .as_slice(),
        source.get_heads()
    );
}

#[test]
fn canonical_export_duplicate_and_unknown_heads_keep_native_selection() {
    let (mut source, before) = ordinary_history();
    let mut unrelated = AutoCommit::new();
    unrelated
        .put(ROOT, "separate_document", "ordinary history")
        .unwrap();
    let foreign_head = unrelated.get_heads()[0];
    let mut heads = before.repeat(8);
    heads.push(foreign_head);
    let expected: Vec<_> = source
        .get_changes(&heads)
        .into_iter()
        .map(|change| (change.hash(), change.raw_bytes().to_vec()))
        .collect();
    let allowance = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let (document, _, turn) = retained_document(&mut source, &allowance);
    let actual =
        assert_precharged(&allowance, || document.changes_since_bounded(&heads, &turn)).unwrap();
    assert_eq!(actual.len(), expected.len());
    for (index, (hash, bytes)) in expected.iter().enumerate() {
        assert_eq!(actual.hash(index), Some(*hash));
        assert_eq!(actual.raw_bytes(index), Some(bytes.as_slice()));
    }
}

#[test]
fn incoming_apply_unions_concurrent_history_and_keeps_original_view() {
    use automerge::marks::{ExpandMark, Mark};
    use automerge::resource_limits::{
        NativeApplyResult, NativeDependencyPolicy, NativeMutationError,
    };
    use automerge::{ObjType, SaveOptions};
    let mut source = AutoCommit::new();
    let text = source.put_object(ROOT, "note", ObjType::Text).unwrap();
    source
        .splice_text(&text, 0, 0, "Shared ordinary text.")
        .unwrap();
    source.commit();
    let mut incoming = source.fork();
    source.put(ROOT, "local_branch", "retained").unwrap();
    source.commit();
    incoming.put(ROOT, "remote_branch", "merged").unwrap();
    incoming
        .mark(
            &text,
            Mark::new("bold".into(), true, 0, 6),
            ExpandMark::Both,
        )
        .unwrap();
    let bytes = incoming.save();
    let original_heads = source.get_heads();
    let source_bytes = source.save();
    let mut expected = source.clone();
    expected.merge(&mut incoming).unwrap();
    let expected_heads = expected.get_heads();
    let allowance = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    // Every disposable native allocation is born in this measurement, so a
    // consuming refusal or registry replacement can safely free its old owner.
    let saved = assert_precharged(&allowance, || -> Result<_, NativeMutationError> {
        let mut registry = NativeAllocationSet::new(allowance.clone())?;
        let turn = registry.work_turn(allowance.clone())?;
        let original = turn
            .normalize_input(&source_bytes, &NativeLimits::default())?
            .reconstruct(&mut registry, &turn)?;
        let candidate = original.clone_bounded(&mut registry, &turn)?;
        let input = turn.normalize_input(&bytes, &NativeLimits::default())?;
        let result = candidate.apply_checked_bounded(
            input,
            NativeDependencyPolicy::RequireComplete,
            &mut registry,
            &turn,
        )?;
        let NativeApplyResult::Applied {
            document,
            changed,
            missing_dependencies,
        } = result
        else {
            panic!("ordinary concurrent history must apply")
        };
        assert!(changed);
        assert!(!missing_dependencies);
        assert_eq!(
            document.base_heads_bounded(&turn)?.as_slice(),
            expected_heads
        );
        assert_eq!(
            original.base_heads_bounded(&turn)?.as_slice(),
            original_heads
        );
        let input = turn.normalize_input(&bytes, &NativeLimits::default())?;
        let result = document.apply_checked_bounded(
            input,
            NativeDependencyPolicy::RequireComplete,
            &mut registry,
            &turn,
        )?;
        let NativeApplyResult::Applied {
            document,
            changed,
            missing_dependencies,
        } = result
        else {
            panic!("known ordinary snapshot must validate and remain complete")
        };
        assert!(!changed);
        assert!(!missing_dependencies);
        assert_eq!(
            document.base_heads_bounded(&turn)?.as_slice(),
            expected_heads
        );
        assert_eq!(
            original.base_heads_bounded(&turn)?.as_slice(),
            original_heads
        );
        Ok(document.save_bounded(&turn, SaveOptions::default())?)
    })
    .unwrap();
    let restored = AutoCommit::load(saved.bytes()).unwrap();
    assert_eq!(
        restored
            .get(ROOT, "local_branch")
            .unwrap()
            .unwrap()
            .0
            .to_str(),
        Some("retained")
    );
    assert_eq!(
        restored
            .get(ROOT, "remote_branch")
            .unwrap()
            .unwrap()
            .0
            .to_str(),
        Some("merged")
    );
}

#[test]
fn incoming_apply_preserves_explicit_missing_dependency_policy_and_raw_orphan() {
    use automerge::SaveOptions;
    use automerge::resource_limits::{
        NativeApplyResult, NativeDependencyPolicy, NativeMutationError,
    };
    let mut source = AutoCommit::new();
    source.put(ROOT, "value", "initial").unwrap();
    source.commit();
    let mut peer = source.fork();
    peer.put(ROOT, "value", "middle").unwrap();
    peer.commit();
    peer.put(ROOT, "value", "final").unwrap();
    let queued = peer.get_last_local_change().unwrap();
    let bytes = queued.raw_bytes();
    let original_heads = source.get_heads();
    let source_bytes = source.save();
    for policy in [
        NativeDependencyPolicy::RequireComplete,
        NativeDependencyPolicy::RetainOrphans,
    ] {
        let allowance = NativeAllowance::new(64 * 1024 * 1024).unwrap();
        assert_precharged(&allowance, || -> Result<(), NativeMutationError> {
            let mut registry = NativeAllocationSet::new(allowance.clone())?;
            let turn = registry.work_turn(allowance.clone())?;
            let original = turn
                .normalize_input(&source_bytes, &NativeLimits::default())?
                .reconstruct(&mut registry, &turn)?;
            let candidate = original.clone_bounded(&mut registry, &turn)?;
            let input = turn.normalize_input(bytes, &NativeLimits::default())?;
            let result = candidate.apply_checked_bounded(input, policy, &mut registry, &turn)?;
            match (policy, result) {
                (
                    NativeDependencyPolicy::RequireComplete,
                    NativeApplyResult::MissingDependencies,
                ) => {}
                (
                    NativeDependencyPolicy::RetainOrphans,
                    NativeApplyResult::Applied {
                        document,
                        changed,
                        missing_dependencies,
                    },
                ) => {
                    assert!(!changed, "queued changes do not change integrated heads");
                    assert!(missing_dependencies);
                    let saved = document.save_bounded(&turn, SaveOptions::default())?;
                    assert!(
                        saved.bytes().ends_with(bytes),
                        "queued canonical raw bytes remain exact"
                    );
                }
                _ => panic!("dependency policy must be explicit"),
            }
            assert_eq!(
                original.base_heads_bounded(&turn)?.as_slice(),
                original_heads
            );
            assert!(!original.has_missing_dependencies());
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn incoming_apply_discards_patch_history_from_many_mark_snapshots() {
    use automerge::marks::{ExpandMark, Mark};
    use automerge::resource_limits::{
        NativeApplyResult, NativeDependencyPolicy, NativeMutationError,
    };
    use automerge::{Automerge, ObjType, PatchLog, SaveOptions};

    let mut source = AutoCommit::new();
    let text = source.put_object(ROOT, "text", ObjType::Text).unwrap();
    source
        .splice_text(&text, 0, 0, &"abcdefghijklmnop".repeat(8))
        .unwrap();
    for index in 0..12 {
        source
            .mark(
                &text,
                Mark::new(
                    format!("ordinary-mark-{index}"),
                    "retained mark value",
                    index,
                    128 - index,
                ),
                ExpandMark::Both,
            )
            .unwrap();
    }
    source.commit();
    let base = source.save();
    let mut remote = source.fork();
    for index in 0..32 {
        remote.splice_text(&text, index * 3, 0, "x").unwrap();
    }
    remote.put(ROOT, "revision", "incoming branch").unwrap();
    remote.commit();
    let changes = remote.get_changes(&[]);
    let incoming = remote.save();
    let mut expected = Automerge::load(&base).unwrap();
    let mut legacy_log = PatchLog::inactive();
    expected
        .apply_changes_batch_log_patches(changes, &mut legacy_log)
        .unwrap();
    assert!(
        !expected.make_patches(&mut legacy_log).is_empty(),
        "legacy inactive batch recording is an intentional compatibility control"
    );
    let expected_heads = expected.get_heads();
    let expected_changes = expected.get_changes(&[]);
    let allowance = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let initial = allowance.available_bytes();
    let output = assert_precharged(&allowance, || -> Result<_, NativeMutationError> {
        let mut registry = NativeAllocationSet::new(allowance.clone())?;
        let turn = registry.work_turn(allowance.clone())?;
        let candidate = turn
            .normalize_input(&base, &NativeLimits::default())?
            .reconstruct(&mut registry, &turn)?;
        let input = turn.normalize_input(&incoming, &NativeLimits::default())?;
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
            panic!("ordinary complete marked history must apply")
        };
        assert!(changed);
        assert!(!missing_dependencies);
        assert_eq!(
            document.base_heads_bounded(&turn)?.as_slice(),
            expected_heads
        );
        let canonical = document.changes_since_bounded(&[], &turn)?;
        assert_eq!(canonical.len(), expected_changes.len());
        for (index, expected) in expected_changes.iter().enumerate() {
            assert_eq!(canonical.hash(index), Some(expected.hash()));
            assert_eq!(canonical.raw_bytes(index), Some(expected.raw_bytes()));
        }
        Ok(document.save_bounded(&turn, SaveOptions::default())?)
    })
    .unwrap();
    let mut restored = AutoCommit::load(output.bytes()).unwrap();
    assert_eq!(restored.get_heads(), expected_heads);
    assert_eq!(restored.text(&text).unwrap(), remote.text(&text).unwrap());
    drop(output);
    // The allowance's own ledger metadata stays charged while it is alive.
    assert_eq!(allowance.available_bytes(), initial);
}

fn generous_limits() -> NativeLimits {
    NativeLimits {
        max_normalized_bytes: 64 * 1024 * 1024,
        max_operations: 4 * 1024 * 1024,
        max_operation_counter: u32::MAX as u64,
        max_changes: 1024 * 1024,
        max_edges: 16 * 1024 * 1024,
        max_actors: 65_536,
        max_heads: 65_536,
        max_columns: 128,
        max_chunks: 65_536,
        max_value_bytes: 64 * 1024 * 1024,
    }
}

#[test]
fn checked_incoming_applies_large_typed_history_within_one_work_allowance() {
    use automerge::ObjType;
    use automerge::resource_limits::NativeIncomingOutcome;
    let mut author = AutoCommit::new();
    let text = author.put_object(ROOT, "text", ObjType::Text).unwrap();
    // Ordinary typing: one small change per few characters.
    for index in 0..4_000 {
        let at = author.length(&text);
        author.splice_text(&text, at, 0, "typed words ").unwrap();
        if index % 3 == 0 {
            author.commit();
        }
    }
    author.commit();
    let snapshot = author.save();
    let started = std::time::Instant::now();
    let allowance = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut fresh = AutoCommit::new();
    let outcome = fresh
        .apply_incoming_checked(
            &snapshot,
            &generous_limits(),
            &allowance,
            automerge::resource_limits::NativeDependencyPolicy::RequireComplete,
        )
        .unwrap();
    assert_eq!(outcome, NativeIncomingOutcome::Applied { changed: true });
    assert_eq!(fresh.get_heads(), author.get_heads());
    assert_eq!(fresh.text(&text).unwrap(), author.text(&text).unwrap());
    let heads = author.get_heads();
    author.splice_text(&text, 0, 0, "Edited: ").unwrap();
    author.commit();
    let incremental = author.save_after(&heads);
    let outcome = fresh
        .apply_incoming_checked(
            &incremental,
            &generous_limits(),
            &allowance,
            automerge::resource_limits::NativeDependencyPolicy::RequireComplete,
        )
        .unwrap();
    assert_eq!(outcome, NativeIncomingOutcome::Applied { changed: true });
    assert_eq!(fresh.text(&text).unwrap(), author.text(&text).unwrap());
    let outcome = fresh
        .apply_incoming_checked(
            &incremental,
            &generous_limits(),
            &allowance,
            automerge::resource_limits::NativeDependencyPolicy::RequireComplete,
        )
        .unwrap();
    assert_eq!(outcome, NativeIncomingOutcome::Applied { changed: false });
    eprintln!("checked large apply took {:?}", started.elapsed());
    assert_eq!(
        allowance.retained_bytes(),
        NativeAllowance::new(64 * 1024 * 1024)
            .unwrap()
            .retained_bytes()
    );
}

#[test]
fn checked_incoming_refuses_missing_dependencies_without_mutation() {
    use automerge::resource_limits::NativeIncomingOutcome;
    let mut source = AutoCommit::new();
    source.put(ROOT, "value", "initial").unwrap();
    source.commit();
    let mut peer = source.fork();
    peer.put(ROOT, "value", "middle").unwrap();
    peer.commit();
    peer.put(ROOT, "value", "final").unwrap();
    let orphan = peer.get_last_local_change().unwrap();
    let heads = source.get_heads();
    let allowance = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let outcome = source
        .apply_incoming_checked(
            orphan.raw_bytes(),
            &generous_limits(),
            &allowance,
            automerge::resource_limits::NativeDependencyPolicy::RequireComplete,
        )
        .unwrap();
    assert_eq!(outcome, NativeIncomingOutcome::MissingDependencies);
    assert_eq!(source.get_heads(), heads);
    assert!(source.get_missing_deps(&[]).is_empty());
}
