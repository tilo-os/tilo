use super::*;
use crate::application::{LaunchTarget, RestoreInput};
use crate::application_test_support::{ProbeHost, instantiate_probe};
use crate::history_mode::restore::{PreparedContentRestore, prepare_content_restore};
use crate::state::owned_restore::{ConfirmedRestore, OwnedRestoreError};
use crate::state::owned_restore_tests::{SCHEMA, context, value};
use automerge::transaction::Transactable;

fn setup(binding: &CallbackBinding) -> PreparedContentRestore {
    binding
        .records
        .with_action_state(&binding.checkout.id, |doc, history, _| {
            // Preserve the real canonical tilo:record identity and framework roots.
            doc.put(ROOT, "body", "old").unwrap();
            history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
            let target = doc.base_heads();
            doc.put(ROOT, "body", "live").unwrap();
            history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
            prepare_content_restore(doc, &SCHEMA, &doc.base_heads(), &target).unwrap()
        })
        .unwrap()
}
fn admitted<'a>(
    binding: &CallbackBinding,
    authority: &'a PermissionContext,
) -> ConfirmedRestore<'a> {
    ConfirmedRestore::for_test(authority, binding.author.clone(), context())
}
#[derive(Debug, PartialEq)]
struct Snapshot {
    bytes: Vec<u8>,
    heads: Vec<ChangeHash>,
    pending_ops: usize,
    revision: u64,
    generation: Option<u64>,
    can_redo: bool,
    pending_before: Option<Vec<ChangeHash>>,
    history: crate::state::owned_restore_tests::HistorySnapshot,
}
fn snapshot(binding: &CallbackBinding) -> Snapshot {
    binding
        .records
        .with_action_state(&binding.checkout.id, |doc, history, state| Snapshot {
            bytes: doc.clone().save(),
            heads: doc.base_heads(),
            pending_ops: doc.pending_ops(),
            revision: history.revision(),
            generation: state.generation,
            can_redo: history.can_redo_as(None),
            pending_before: state.pending.as_ref().map(|p| p.before.clone()),
            history: crate::state::owned_restore_tests::history_snapshot(history),
        })
        .unwrap()
}
#[test]
fn owned_restore_bound_applies_exact_spans_once_with_one_undo_boundary() {
    // Case `owned_restore_bound_exact_span_instance_stamp_and_undo_without_duplicate_boundary`.
    {
        let host = ProbeHost::new();
        let component =
            instantiate_probe(&host, 77, LaunchTarget::New, RestoreInput::None).unwrap();
        let binding = component.callback_binding();
        let plan = setup(&binding);
        let authority = binding.permissions.borrow();
        let before = snapshot(&binding);
        let mut outer = AutoCommit::new();
        let receipt = binding
            .restore_prepared(&mut outer, &admitted(&binding, &authority), &plan)
            .unwrap();
        assert!(receipt.changed);
        assert_eq!(
            binding.generation().unwrap(),
            before.generation.map(|v| v + 1)
        );
        binding
            .records
            .with_action_state(&binding.checkout.id, |doc, history, state| {
                assert_eq!(value(doc), "old");
                assert_eq!(crate::record::identity(doc), Some(binding.checkout.clone()));
                assert_eq!(doc.get_changes(plan.current_heads()).len(), 1);
                assert_eq!(history.revision(), before.revision + 1);
                let entry = history.peek_last_undo().unwrap();
                assert_eq!(entry.before, plan.current_heads());
                assert_eq!(entry.after, receipt.after);
                assert_eq!(entry.author, Some(binding.author.clone()));
                assert_eq!(entry.instance_id, Some(binding.instance_id));
                assert_eq!(
                    doc.get_changes(plan.current_heads())[0]
                        .message()
                        .and_then(|s| tilo_actions::ActionContext::from_change_message(s)),
                    Some(context())
                );
                assert_eq!(state.pending.as_ref().unwrap().before, receipt.after);
            })
            .unwrap();
        binding
            .history_request(&mut outer, crate::undo::UndoRequest::Undo)
            .unwrap()
            .unwrap();
        binding
            .records
            .with_action_state(&binding.checkout.id, |doc, history, _| {
                assert_eq!(value(doc), "live");
                assert_eq!(crate::record::identity(doc), Some(binding.checkout.clone()));
                assert_eq!(history.revision(), before.revision + 2);
            })
            .unwrap();
        assert_eq!(outer.pending_ops(), 0);
        assert!(outer.base_heads().is_empty());
    }

    // Case `owned_restore_bound_noop_and_stale_preserve_redo_and_empty_continuation`.
    {
        let host = ProbeHost::new();
        let component =
            instantiate_probe(&host, 77, LaunchTarget::New, RestoreInput::None).unwrap();
        let binding = component.callback_binding();
        let stale = setup(&binding);
        let plan = binding
            .records
            .with_action_state(&binding.checkout.id, |doc, history, state| {
                doc.put(ROOT, "body", "later").unwrap();
                history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
                history
                    .undo_as(doc, Some(&binding.author), None, None)
                    .unwrap();
                state.pending = Some(PendingRecordAction::new(&binding, doc.base_heads()));
                prepare_content_restore(doc, &SCHEMA, &doc.base_heads(), &doc.base_heads()).unwrap()
            })
            .unwrap();
        let before = snapshot(&binding);
        assert!(before.can_redo);
        let authority = binding.permissions.borrow();
        let receipt = binding
            .restore_prepared(
                &mut AutoCommit::new(),
                &admitted(&binding, &authority),
                &plan,
            )
            .unwrap();
        assert!(!receipt.changed);
        assert_eq!(snapshot(&binding), before);
        assert!(matches!(
            binding.restore_prepared(
                &mut AutoCommit::new(),
                &admitted(&binding, &authority),
                &stale
            ),
            Err(OwnedRestoreError::StaleReview)
        ));
        assert_eq!(snapshot(&binding), before);
    }

    // Case `owned_restore_bound_engine_rollback_preserves_redo_and_generation`.
    {
        let host = ProbeHost::new();
        let component =
            instantiate_probe(&host, 77, LaunchTarget::New, RestoreInput::None).unwrap();
        let binding = component.callback_binding();
        let target = setup(&binding).target_heads().to_vec();
        let plan = binding
            .records
            .with_action_state(&binding.checkout.id, |doc, history, state| {
                doc.put(ROOT, "body", "later").unwrap();
                history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
                history
                    .undo_as(doc, Some(&binding.author), None, None)
                    .unwrap();
                state.pending = Some(PendingRecordAction::new(&binding, doc.base_heads()));
                prepare_content_restore(doc, &SCHEMA, &doc.base_heads(), &target).unwrap()
            })
            .unwrap();
        let before = snapshot(&binding);
        let authority = binding.permissions.borrow();
        crate::state::owned_restore_tests::refuse_restored_result(true);
        let result = binding.restore_prepared(
            &mut AutoCommit::new(),
            &admitted(&binding, &authority),
            &plan,
        );
        crate::state::owned_restore_tests::refuse_restored_result(false);
        assert!(matches!(
            result,
            Err(OwnedRestoreError::Engine(
                crate::history_mode::restore::RestoreError::SchemaInvalid(_)
            ))
        ));
        assert_eq!(snapshot(&binding), before);
        assert!(
            binding
                .restore_prepared(
                    &mut AutoCommit::new(),
                    &admitted(&binding, &authority),
                    &plan
                )
                .unwrap()
                .changed
        );
    }
}
#[test]
fn owned_restore_bound_refuses_mismatched_or_active_targets_without_bookkeeping() {
    // Case `owned_restore_bound_refuses_physical_logical_and_generation_without_bookkeeping`.
    {
        for case in 0..5 {
            let host = ProbeHost::new();
            let component =
                instantiate_probe(&host, 77, LaunchTarget::New, RestoreInput::None).unwrap();
            let binding = component.callback_binding();
            let mut plan = setup(&binding);
            binding
                .records
                .with_action_state(&binding.checkout.id, |doc, history, state| {
                    match case {
                        0 => {
                            doc.put(ROOT, "body", "pending").unwrap();
                        }
                        1 => {
                            state.pending =
                                Some(PendingRecordAction::new(&binding, doc.base_heads()));
                            doc.put(ROOT, "body", "closed").unwrap();
                            doc.commit();
                            // A logical pending span must be refused independently of UndoLog baseline.
                            history.rebase(doc);
                            plan = prepare_content_restore(
                                doc,
                                &SCHEMA,
                                &doc.base_heads(),
                                plan.target_heads(),
                            )
                            .unwrap();
                        }
                        2 => state.generation = Some(u64::MAX),
                        3 => state.generation = None,
                        4 => crate::state::owned_restore_tests::exhaust_history(history),
                        _ => unreachable!(),
                    }
                })
                .unwrap();
            let before = snapshot(&binding);
            let authority = binding.permissions.borrow();
            let error = binding
                .restore_prepared(
                    &mut AutoCommit::new(),
                    &admitted(&binding, &authority),
                    &plan,
                )
                .unwrap_err();
            match case {
                0 => assert!(matches!(error, OwnedRestoreError::PendingEdits)),
                1 => assert!(matches!(error, OwnedRestoreError::UnfinishedActionBoundary)),
                4 => assert!(matches!(error, OwnedRestoreError::HistoryExhausted)),
                _ => assert!(matches!(error, OwnedRestoreError::GenerationExhausted)),
            }
            assert_eq!(snapshot(&binding), before);
        }
    }

    // Case `owned_restore_bound_active_mutation_and_nested_operation_refused`.
    {
        let host = ProbeHost::new();
        let component =
            instantiate_probe(&host, 77, LaunchTarget::New, RestoreInput::None).unwrap();
        let binding = component.callback_binding();
        let plan = setup(&binding);
        let authority = binding.permissions.borrow();
        let admission = admitted(&binding, &authority);
        binding
            .with_document(&mut AutoCommit::new(), |doc, _| {
                let bytes = doc.clone().save();
                let generation = binding.generation().unwrap();
                assert!(matches!(
                    binding.restore_prepared(doc, &admission, &plan),
                    Err(OwnedRestoreError::ActiveAction)
                ));
                assert_eq!(doc.clone().save(), bytes);
                assert_eq!(binding.generation().unwrap(), generation);
            })
            .unwrap();
        binding
            .with_record_frame(&mut AutoCommit::new(), false, |doc, _| {
                let operation =
                    BoundOperation::enter(&binding, doc, tilo_actions::OperationKind::View)
                        .unwrap();
                let before = operation.doc.clone().save();
                assert!(matches!(
                    binding.restore_prepared(operation.doc, &admission, &plan),
                    Err(OwnedRestoreError::ActiveAction)
                ));
                assert_eq!(operation.doc.clone().save(), before);
            })
            .unwrap();
    }
}
