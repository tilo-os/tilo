//! Confirmation archives remain private across staged retirement and reopen.
use super::*;
use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use tilo_desktop_model::{
    WorkspaceTaskConfirmation as Confirmation, WorkspaceTaskConfirmationState as State,
};

const SECRET: &str = "historical private confirmation\n  exact relay custody";

fn full() -> Confirmation {
    Confirmation {
        id: "review".into(),
        asking: uuid::Uuid::from_u128(29),
        run_epoch: 1,
        agent_id: "scribe".into(),
        app: "mail".into(),
        resource_kind: "mailbox".into(),
        resource: "inbox".into(),
        action: "send".into(),
        kind: tilo_action_policy::ActionKind::Undoable,
        risk: Default::default(),
        draft: tilo_actions::Draft {
            action_label: SECRET.into(),
            description: Some(SECRET.into()),
            ..Default::default()
        },
        arguments_digest: [7; 32],
        requested_at: 17,
        state: State::Waiting,
    }
}

fn declined() -> Confirmation {
    let mut value = full();
    value.state = State::Declined {
        person: "person".into(),
        at: 23,
        reason: Some(SECRET.into()),
    };
    value
}

fn legacy_history(room: &mut GovernedRoom, key: &SigningKey, now: SystemTime) -> serde_json::Value {
    let mut doc = room.peer.admitted_room().unwrap().clone();
    let controls = doc
        .put_object(ROOT, "task_control_v1", ObjType::Map)
        .unwrap();
    let waiting = full();
    let mut control = tilo_desktop_model::WorkspaceTaskControl {
        run_epoch: 1,
        confirmation: Some(waiting.clone()),
        ..Default::default()
    };
    let bytes = ScalarValue::Bytes(serde_json::to_vec(&control).unwrap());
    doc.put(&controls, "current", bytes.clone()).unwrap();
    doc.put(&controls, "deleted", bytes).unwrap();
    doc.commit();
    let answered = declined();
    control.confirmation = Some(answered.clone());
    doc.put(
        &controls,
        "current",
        ScalarValue::Bytes(serde_json::to_vec(&control).unwrap()),
    )
    .unwrap();
    doc.delete(&controls, "deleted").unwrap();
    let tasks = doc.put_object(ROOT, "tasks_v1", ObjType::Map).unwrap();
    let mut event = tilo_desktop_model::WorkspaceTaskAuditEvent::new(
        23,
        "person",
        "decline-action",
        [("action", tilo_actions::Scalar::text(SECRET))],
    );
    event.note = Some(SECRET.into());
    let event = serde_json::to_value(event).unwrap();
    doc.put(
        &tasks,
        "current",
        ScalarValue::Bytes(
            serde_json::to_vec(&serde_json::json!({
                "route": "model", "title": "Public title", "audit": [event.clone()]
            }))
            .unwrap(),
        ),
    )
    .unwrap();
    doc.put(ROOT, "public-edit", "preserved").unwrap();
    let mut candidate = room.peer.clone();
    candidate
        .install_seed(
            tilo_peer_state::SeedState {
                room_changes: doc
                    .get_changes(&[])
                    .iter()
                    .map(|change| change.raw_bytes().to_vec())
                    .collect(),
                documents: vec![],
            },
            key,
            now,
        )
        .unwrap();
    let collaboration = room.collaboration().clone();
    room.commit(candidate, key, now, |_, _| Ok(collaboration))
        .unwrap();
    event
}

#[test]
fn current_and_retired_confirmation_custody_survives_staging_compaction_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = super::super::tests::governor();
    let now = SystemTime::now();
    let mut room =
        GovernedRoom::create(dir.path(), "Confirmation history", &cert, &key, now).unwrap();
    let initial = room
        .stage_private_task_baseline(&key, now)
        .unwrap()
        .unwrap();
    room.install_private_task_baseline(&initial).unwrap();
    let event = legacy_history(&mut room, &key, now);
    let waiting = full();
    let before = room.record.peer.clone();
    let baseline = room
        .stage_private_task_baseline(&key, now)
        .unwrap()
        .unwrap();
    assert_eq!(
        room.record.peer, before,
        "staging must not replace the predecessor"
    );
    let history_path = room
        .path
        .with_extension("private-task-confirmation-history-v1.json");
    let history: serde_json::Value = super::super::private_tasks::read_private(&history_path)
        .unwrap()
        .unwrap();
    assert!(
        history["confirmations"]["current"]
            .as_array()
            .unwrap()
            .contains(&serde_json::to_value(&waiting).unwrap())
    );
    assert!(
        history["confirmations"]["deleted"]
            .as_array()
            .unwrap()
            .contains(&serde_json::to_value(&waiting).unwrap())
    );
    assert!(
        history["audit"]["current"]
            .as_array()
            .unwrap()
            .contains(&event)
    );
    // Recovering after a crash before public installation sees the same private
    // versions and never invents a new current answer from the archive.
    let mut room = GovernedRoom::open(room.path.clone(), &cert, &key).unwrap();
    let retried = room
        .stage_private_task_baseline(&key, now)
        .unwrap()
        .unwrap();
    room.install_private_task_baseline(&retried).unwrap();
    room.compact_task_confirmations().unwrap();
    let room = GovernedRoom::open(room.path.clone(), &cert, &key).unwrap();
    assert_reopened(&room);
    let after: serde_json::Value = super::super::private_tasks::read_private(&history_path)
        .unwrap()
        .unwrap();
    assert_eq!(
        history, after,
        "compaction must not erase historical archives"
    );
    for envelope in [baseline, retried] {
        assert_public(&room, &envelope);
    }
    assert!(
        room.stage_private_task_baseline(&key, now)
            .unwrap()
            .is_none(),
        "scrubbed placeholders are not another privacy migration"
    );
}

fn assert_reopened(room: &GovernedRoom) {
    let state = room.task_policy_snapshot().unwrap();
    assert_eq!(
        state.controls["current"].confirmation.as_ref(),
        Some(&declined())
    );
    assert!(!state.controls.contains_key("deleted"));
    let public =
        tilo_peer_state::workspace_task::snapshot(room.peer.admitted_room().unwrap()).unwrap();
    let exposed = public.controls["current"].confirmation.as_ref().unwrap();
    assert_eq!(exposed.draft, tilo_actions::Draft::default());
    assert!(matches!(
        &exposed.state,
        State::Declined { reason: None, .. }
    ));
    assert_eq!(
        room.peer
            .admitted_room()
            .unwrap()
            .get(ROOT, "public-edit")
            .unwrap()
            .unwrap()
            .0
            .as_str(),
        Some("preserved")
    );
}

fn assert_public(room: &GovernedRoom, envelope: &tilo_peer_state::SanitizedRoomBaseline) {
    let mut doc = envelope
        .verify_anchored_document(
            room.room(),
            room.peer.authority().device_public_key,
            &envelope.anchor(),
        )
        .unwrap();
    for change in doc.get_changes(&[]) {
        assert!(
            !change
                .raw_bytes()
                .windows(SECRET.len())
                .any(|part| part == SECRET.as_bytes())
        );
    }
}
