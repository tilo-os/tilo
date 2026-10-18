use super::*;
use serde_json::json;
use tilo_desktop_model::{
    WorkspaceTaskConfirmation as Confirmation, WorkspaceTaskConfirmationState as State,
};

fn full() -> Confirmation {
    Confirmation {
        id: "request".into(),
        asking: uuid::Uuid::from_u128(9),
        run_epoch: 3,
        agent_id: "scribe".into(),
        app: "mail".into(),
        resource_kind: "mailbox".into(),
        resource: "inbox".into(),
        action: "send".into(),
        kind: tilo_action_policy::ActionKind::Undoable,
        risk: tilo_actions::RiskLevel::default(),
        draft: tilo_actions::Draft {
            action_label: "private old label".into(),
            resource_label: "private recipient".into(),
            description: Some("private draft body\n  exact".into()),
            ..Default::default()
        },
        arguments_digest: [5; 32],
        requested_at: 17,
        state: State::Waiting,
    }
}

fn audit(action: &str, note: Option<&str>) -> serde_json::Value {
    let mut event = tilo_desktop_model::WorkspaceTaskAuditEvent::new(
        18,
        "person",
        action,
        [("action", tilo_actions::Scalar::text("private old label"))],
    );
    event.note = note.map(str::to_owned);
    serde_json::to_value(event).unwrap()
}

fn confirmed(waiting: &Confirmation) -> Confirmation {
    let mut confirmed = waiting.clone();
    confirmed.state = State::Confirmed {
        person: "person".into(),
        at: 19,
        edits: vec![tilo_actions::DraftEdit {
            path: vec!["body".into()],
            text: "private edit\n  exact".into(),
        }],
    };
    confirmed
}

fn empty_room() -> (AutoCommit, ObjId, ObjId) {
    let mut room = AutoCommit::new();
    let controls = room
        .put_object(ROOT, TASK_CONTROL_KEY, ObjType::Map)
        .unwrap();
    let tasks = room
        .put_object(
            ROOT,
            tilo_desktop_model::projection::ROOM_TASKS_KEY,
            ObjType::Map,
        )
        .unwrap();
    (room, controls, tasks)
}

#[test]
fn archives_exact_current_and_retired_confirmations_and_scrubs_new_history() {
    let (mut room, controls, tasks) = empty_room();
    let waiting = full();
    let old_event = audit("ask-confirmation", None);
    room.put(
        &controls,
        "task",
        serde_json::to_vec(&json!({"confirmation": waiting})).unwrap(),
    )
    .unwrap();
    room.put(
        &tasks,
        "task",
        serde_json::to_vec(&json!({"route": "model", "audit": [old_event]})).unwrap(),
    )
    .unwrap();
    room.commit();
    let confirmed = confirmed(&waiting);
    room.put(
        &controls,
        "task",
        serde_json::to_vec(&json!({"confirmation": confirmed})).unwrap(),
    )
    .unwrap();
    room.put(
        &controls,
        "deleted",
        serde_json::to_vec(&json!({"confirmation": confirmed})).unwrap(),
    )
    .unwrap();
    room.commit();
    let mut declined = waiting.clone();
    declined.state = State::Declined {
        person: "person".into(),
        at: 20,
        reason: Some("private reason\n  exact".into()),
    };
    let declined_event = audit("decline-action", Some("private reason\n  exact"));
    room.put(
        &controls,
        "task",
        serde_json::to_vec(&json!({"confirmation": declined, "future": {"retain": true}})).unwrap(),
    )
    .unwrap();
    room.put(
        &tasks,
        "task",
        serde_json::to_vec(
            &json!({"route": "model", "audit": [old_event, declined_event], "future": 42}),
        )
        .unwrap(),
    )
    .unwrap();
    room.delete(&controls, "deleted").unwrap();
    room.commit();
    let source = AutoCommit::load(&room.save()).unwrap();
    assert!(validate_reports(&source).is_err());
    let (mut clean, recovered) = sanitize_task_room(&source, &BTreeMap::new()).unwrap();
    let private = &recovered["task"];
    assert_eq!(private.confirmation.as_ref(), Some(&declined));
    assert_eq!(private.historical_confirmations.len(), 3);
    for exact in [&waiting, &confirmed, &declined] {
        assert!(private.historical_confirmations.contains(exact));
    }
    assert_eq!(
        recovered["deleted"].historical_confirmations,
        vec![confirmed]
    );
    assert!(recovered["deleted"].confirmation.is_none());
    assert!(private.historical_confirmation_audit.contains(&old_event));
    assert!(
        private
            .historical_confirmation_audit
            .contains(&declined_event)
    );
    validate_reports(&clean).unwrap();
    assert_eq!(clean.get_changes(&[]).len(), 1);
    let restored = AutoCommit::load(&clean.save()).unwrap();
    let public: serde_json::Value =
        serde_json::from_slice(&blobs(&restored, TASK_CONTROL_KEY).unwrap()["task"]).unwrap();
    let mut expected = declined;
    expected.draft = tilo_actions::Draft::default();
    if let State::Declined { reason, .. } = &mut expected.state {
        *reason = None;
    }
    assert_eq!(
        public["confirmation"],
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(public["future"], json!({"retain": true}));
    let task: serde_json::Value = serde_json::from_slice(
        &blobs(&restored, tilo_desktop_model::projection::ROOM_TASKS_KEY).unwrap()["task"],
    )
    .unwrap();
    assert_eq!(task["future"], 42);
    let materialized = serde_json::to_string(&json!([public, task])).unwrap();
    assert!(!materialized.contains("private"));
    let (_, repeated) = sanitize_task_room(&restored, &BTreeMap::new()).unwrap();
    assert!(repeated.values().all(|entry| entry.confirmation.is_none()));
}

#[test]
fn nested_and_deleted_confirmation_audits_are_archived_without_public_fallback() {
    let mut room = AutoCommit::new();
    let tasks = room
        .put_object(
            ROOT,
            tilo_desktop_model::projection::ROOM_TASKS_KEY,
            ObjType::Map,
        )
        .unwrap();
    let nested = json!({"at": 22, "actor": "person", "note": "private nested reason", "what": {"did": "group", "members": [audit("ask-confirmation", None)["what"].clone(), {"did": "history", "of": {"what": audit("confirm-action", None)["what"].clone()}}]}});
    let task = json!({"route": "model", "audit": [nested]});
    room.put(&tasks, "deleted", serde_json::to_vec(&task).unwrap())
        .unwrap();
    room.put(&tasks, "retained", serde_json::to_vec(&task).unwrap())
        .unwrap();
    room.commit();
    room.delete(&tasks, "deleted").unwrap();
    room.commit();
    let (clean, recovered) = sanitize_task_room(&room, &BTreeMap::new()).unwrap();
    assert_eq!(
        recovered["deleted"].historical_confirmation_audit,
        vec![nested.clone()]
    );
    assert_eq!(
        recovered["retained"].historical_confirmation_audit,
        vec![nested]
    );
    let public = blobs(&clean, tilo_desktop_model::projection::ROOM_TASKS_KEY).unwrap();
    assert!(
        !String::from_utf8(public["retained"].clone())
            .unwrap()
            .contains("private")
    );
    validate_reports(&clean).unwrap();
}
