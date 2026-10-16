//! Real document/UndoLog transaction tests; no permission admission is claimed here.
use super::owned_restore::*;
use super::*;
use crate::history_mode::restore::*;
use crate::permissions::PermissionContext;
use automerge::ROOT;
use std::cell::Cell;

thread_local! { static VALIDATION_MODE: Cell<u8> = const { Cell::new(0) }; }
fn validate_result(doc: &AutoCommit) -> Result<(), String> {
    if doc.get(ROOT, "body").unwrap().unwrap().0.as_str() == Some("old") {
        match VALIDATION_MODE.get() {
            1 => return Err("result refused".into()),
            2 => panic!("result validator unwound"),
            _ => {}
        }
    }
    Ok(())
}
pub(crate) static SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "owned-test",
    version: 1,
    roots: &[HistoryRestoreRoot {
        key: "body",
        required: true,
        expected_type: RestoreValueType::Scalar(ScalarType::String),
    }],
    selectors: &[],
    preserved_roots: &[],
    preserved: &[],
    validate: |_| Ok(()),
    validate_result,
};
pub(crate) fn context() -> tilo_actions::ActionContext {
    tilo_actions::ActionContext::for_resource("document:1", "document", "Draft")
        .with_app("dev.tilo.restore-test", "Restore test")
}
pub(crate) fn admission(authority: &PermissionContext) -> ConfirmedRestore<'_> {
    ConfirmedRestore::for_test(authority, UserId("ada".into()), context())
}
pub(crate) fn value(doc: &AutoCommit) -> String {
    doc.get(ROOT, "body")
        .unwrap()
        .unwrap()
        .0
        .as_str()
        .unwrap()
        .to_owned()
}
fn fixture() -> (State, PreparedContentRestore) {
    VALIDATION_MODE.set(0);
    let mut state = State::new();
    state.doc.put(ROOT, "body", "old").unwrap();
    state.commit_action(None, None);
    let target = state.doc.base_heads();
    state.doc.put(ROOT, "body", "live").unwrap();
    state.commit_action(None, None);
    let plan =
        prepare_content_restore(&state.doc, &SCHEMA, &state.doc.base_heads(), &target).unwrap();
    (state, plan)
}
#[derive(Debug, PartialEq)]
struct Snapshot {
    bytes: Vec<u8>,
    heads: Vec<ChangeHash>,
    pending: usize,
    undo: Vec<(Vec<ChangeHash>, Vec<ChangeHash>)>,
    redo: Vec<(Vec<ChangeHash>, Vec<ChangeHash>)>,
    baseline: Vec<ChangeHash>,
    revision: u64,
}
fn snapshot(state: &State) -> Snapshot {
    Snapshot {
        // Serialize a clone, never close the real pending transaction.
        bytes: state.doc.clone().save(),
        heads: state.doc.base_heads(),
        pending: state.doc.pending_ops(),
        undo: state.undo.brackets(&state.doc).0,
        redo: state.undo.brackets(&state.doc).1,
        baseline: state.undo.last_heads.clone(),
        revision: state.history_revision(),
    }
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn owned_legacy_restores_are_exact_guarded_and_roll_back_only_owned_writes() {
    // phase: owned_restore_legacy_one_stamp_exact_span_and_undo
    {
        let (mut state, plan) = fixture();
        let authority = PermissionContext::permissive();
        let before = state.doc.base_heads();
        let count = state.doc.get_changes(&[]).len();
        let revision = state.history_revision();
        let actor = state.doc.get_actor().clone();
        let root = state.root.clone();
        let receipt = state
            .restore_prepared_owned(&admission(&authority), &plan)
            .unwrap();
        assert!(receipt.changed);
        assert_eq!(receipt.after, state.doc.base_heads());
        assert_eq!(state.history_revision(), revision + 1);
        assert_eq!(state.doc.get_changes(&[]).len(), count + 1);
        let entry = state.peek_last_undo().unwrap();
        assert_eq!(entry.before, before);
        assert_eq!(entry.after, receipt.after);
        assert_eq!(entry.author, Some(UserId("ada".into())));
        assert_eq!(entry.instance_id, None);
        let change = &state.doc.get_changes(&before)[0];
        assert_eq!(change.deps(), before);
        assert!(change.timestamp() > 0);
        assert_eq!(
            change
                .message()
                .and_then(|s| tilo_actions::ActionContext::from_change_message(s))
                .map(unstamped),
            Some(context().with_invoker(tilo_actions::ActionInvoker::actor("ada")))
        );
        assert_eq!(value(&state.doc), "old");
        assert_eq!(state.doc.get_actor(), &actor);
        assert_eq!(state.root, root);
        state
            .undo_as(Some(&UserId("ada".into())), None, None, None)
            .unwrap();
        assert_eq!(value(&state.doc), "live");
    }
    // phase: owned_restore_legacy_refuses_pending_and_logically_unfinished_edits
    {
        let authority = PermissionContext::permissive();
        let (mut state, plan) = fixture();
        state.doc.put(ROOT, "body", "pending").unwrap();
        let before = snapshot(&state);
        assert!(matches!(
            state.restore_prepared_owned(&admission(&authority), &plan),
            Err(OwnedRestoreError::PendingEdits)
        ));
        assert_eq!(snapshot(&state), before);
        state.doc.commit();
        let plan = prepare_content_restore(
            &state.doc,
            &SCHEMA,
            &state.doc.base_heads(),
            plan.target_heads(),
        )
        .unwrap();
        let before = snapshot(&state);
        assert!(matches!(
            state.restore_prepared_owned(&admission(&authority), &plan),
            Err(OwnedRestoreError::UnfinishedActionBoundary)
        ));
        assert_eq!(snapshot(&state), before);
        // Its original owner completes the precommitted ordinary action explicitly.
        let baseline = state.undo.last_heads.clone();
        state
            .undo
            .record_committed_action(&mut state.doc, baseline, None, None);
        assert!(
            state
                .restore_prepared_owned(&admission(&authority), &plan)
                .unwrap()
                .changed
        );
    }
    // phase: owned_restore_legacy_stale_and_capacity_leave_everything_untouched
    {
        let authority = PermissionContext::permissive();
        let (mut state, plan) = fixture();
        state.doc.put(ROOT, "body", "new").unwrap();
        state.commit_action(None, None);
        let before = snapshot(&state);
        assert!(matches!(
            state.restore_prepared_owned(&admission(&authority), &plan),
            Err(OwnedRestoreError::StaleReview)
        ));
        assert_eq!(snapshot(&state), before);
        let plan = prepare_content_restore(
            &state.doc,
            &SCHEMA,
            &state.doc.base_heads(),
            plan.target_heads(),
        )
        .unwrap();
        state.undo.revision = u64::MAX;
        let before = snapshot(&state);
        assert!(matches!(
            state.restore_prepared_owned(&admission(&authority), &plan),
            Err(OwnedRestoreError::HistoryExhausted)
        ));
        assert_eq!(snapshot(&state), before);
    }
    // phase: owned_restore_legacy_noop_and_errors_preserve_redo
    {
        let authority = PermissionContext::permissive();
        let (mut state, plan) = fixture();
        state.doc.put(ROOT, "body", "later").unwrap();
        state.commit_action(None, None);
        state.undo(None, None).unwrap();
        assert!(state.can_redo_as(None));
        let current = state.doc.base_heads();
        let noop = prepare_content_restore(&state.doc, &SCHEMA, &current, &current).unwrap();
        let before = snapshot(&state);
        let receipt = state
            .restore_prepared_owned(&admission(&authority), &noop)
            .unwrap();
        assert!(!receipt.changed);
        assert_eq!(receipt.after, current);
        assert_eq!(snapshot(&state), before);
        let plan =
            prepare_content_restore(&state.doc, &SCHEMA, &current, plan.target_heads()).unwrap();
        VALIDATION_MODE.set(1);
        assert!(matches!(
            state.restore_prepared_owned(&admission(&authority), &plan),
            Err(OwnedRestoreError::Engine(RestoreError::SchemaInvalid(_)))
        ));
        VALIDATION_MODE.set(0);
        assert_eq!(snapshot(&state), before);
        // A later real restore succeeds, proving rollback kept actor/patch state usable.
        assert!(
            state
                .restore_prepared_owned(&admission(&authority), &plan)
                .unwrap()
                .changed
        );
    }
    // phase: owned_restore_legacy_unwind_rolls_back_only_owned_writes
    {
        let authority = PermissionContext::permissive();
        let (mut state, plan) = fixture();
        state.doc.update_diff_cursor();
        let before = snapshot(&state);
        VALIDATION_MODE.set(2);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state.restore_prepared_owned(&admission(&authority), &plan)
        }));
        VALIDATION_MODE.set(0);
        assert!(result.is_err());
        assert_eq!(snapshot(&state), before);
        assert!(
            state
                .restore_prepared_owned(&admission(&authority), &plan)
                .unwrap()
                .changed
        );
        assert_eq!(value(&state.doc), "old");
    }
}

#[test]
fn owned_restore_mixed_overlay_and_undo_preserve_live_remote_operational_state() {
    static MIXED: HistoryRestoreSchema = HistoryRestoreSchema {
        id: "mixed-owned-test",
        version: 1,
        roots: &[],
        selectors: &[HistoryRootSelector {
            pattern: HistoryRootPattern::Exact("mixed"),
            required: true,
            node: HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
                fields: &[HistoryMapField {
                    key: "body",
                    required: true,
                    node: HistoryRestoreNode::Whole {
                        expected_type: RestoreValueType::Scalar(ScalarType::String),
                    },
                }],
                entries: None,
            }),
        }],
        preserved_roots: &[],
        preserved: &[],
        validate: |_| Ok(()),
        validate_result: |_| Ok(()),
    };
    let mut state = State::new();
    let mixed = state
        .doc
        .put_object(ROOT, "mixed", automerge::ObjType::Map)
        .unwrap();
    let runtime = state
        .doc
        .put_object(&mixed, "runtime", automerge::ObjType::Map)
        .unwrap();
    state.doc.put(&mixed, "body", "old").unwrap();
    state.doc.put(&runtime, "session", "historical").unwrap();
    state.commit_action(None, None);
    let target = state.doc.base_heads();
    state.doc.put(&mixed, "body", "live").unwrap();
    state.commit_action(None, None);
    let mut remote = state.doc.fork();
    remote
        .put(&runtime, "session", "current remote session")
        .unwrap();
    remote.commit();
    let remote_heads = remote.base_heads();
    state.load_incremental(&remote.save()).unwrap();
    let reviewed = state.doc.base_heads();
    let plan = prepare_content_restore(&state.doc, &MIXED, &reviewed, &target).unwrap();
    let authority = PermissionContext::permissive();
    state
        .restore_prepared_owned(&admission(&authority), &plan)
        .unwrap();
    assert_eq!(
        state.doc.get(&mixed, "body").unwrap().unwrap().0.as_str(),
        Some("old")
    );
    state
        .undo_as(Some(&UserId("ada".into())), None, None, None)
        .unwrap();
    assert_eq!(
        state.doc.get(&mixed, "body").unwrap().unwrap().0.as_str(),
        Some("live")
    );
    assert_eq!(state.doc.get(ROOT, "mixed").unwrap().unwrap().1, mixed);
    assert_eq!(
        state.doc.get(&mixed, "runtime").unwrap().unwrap().1,
        runtime
    );
    assert_eq!(
        state
            .doc
            .get(&runtime, "session")
            .unwrap()
            .unwrap()
            .0
            .as_str(),
        Some("current remote session")
    );
    for head in remote_heads {
        assert!(state.doc.get_change_by_hash(&head).is_some());
    }
    assert_eq!(
        state
            .undo
            .next_redo_as(&state.doc, Some(&UserId("ada".into())))
            .unwrap()
            .author,
        Some(UserId("ada".into()))
    );
}

#[derive(Debug, PartialEq)]
pub(crate) struct HistorySnapshot {
    undo: Vec<(Vec<ChangeHash>, Vec<ChangeHash>)>,
    redo: Vec<(Vec<ChangeHash>, Vec<ChangeHash>)>,
    baseline: Vec<ChangeHash>,
    revision: u64,
}
pub(crate) fn history_snapshot(history: &UndoLog, doc: &AutoCommit) -> HistorySnapshot {
    let (undo, redo) = history.brackets(doc);
    HistorySnapshot {
        undo,
        redo,
        baseline: history.last_heads.clone(),
        revision: history.revision(),
    }
}
pub(crate) fn exhaust_history(history: &mut UndoLog) {
    history.revision = u64::MAX;
}
pub(crate) fn refuse_restored_result(refuse: bool) {
    VALIDATION_MODE.set(u8::from(refuse));
}

/// An entry as its caller gave it: without the undo step the history joined
/// it to, which it must have.
pub(crate) fn unstamped(mut context: tilo_actions::ActionContext) -> tilo_actions::ActionContext {
    assert!(context.gesture.is_some(), "the change joined an undo step");
    context.gesture = None;
    context
}
