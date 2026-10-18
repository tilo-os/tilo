//! Publication privacy depends on retained payloads, not task-map presence.
use super::has_private_task_history;
use automerge::{AutoCommit, ObjType, ROOT, ScalarValue, transaction::Transactable as _};
use serde_json::{Value, json};

fn needs_migration(room: &AutoCommit) -> bool {
    has_private_task_history(room, &std::collections::BTreeMap::default()).unwrap()
}

fn put(room: &mut AutoCommit, field: &str, value: &Value) -> automerge::ObjId {
    let map = room.put_object(ROOT, field, ObjType::Map).unwrap();
    room.put(
        &map,
        "task",
        ScalarValue::Bytes(serde_json::to_vec(value).unwrap()),
    )
    .unwrap();
    room.commit();
    map
}

#[test]
fn ordinary_manual_history_publishes_and_recovers_without_governor_key() {
    use crate::{RoomId, open_file, testing};
    use neighbour_wire::NeighbourMsg;
    use tilo_peer_state::TransportOrigin;

    for private in [false, true] {
        let directory = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
        let path = directory.path().join("rooms.json");
        let owner = open_file(&path).unwrap();
        // The enrollment fixture returns only the governor's public identity.
        let (intent, bootstrap, messages, governor, now) = testing::raw_fixture();
        let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
        owner
            .install(&testing::finalize(intent, pending, bootstrap).unwrap(), now)
            .unwrap();
        let room = RoomId::parse(testing::ROOM).unwrap();
        for message in messages.into_iter().skip(1) {
            owner
                .receive(
                    room,
                    TransportOrigin::RetainedRelayDelta(governor),
                    message,
                    now,
                )
                .unwrap();
        }
        let mut document = owner
            .with_current(room, |view| {
                Ok(view.peer().admitted_room().unwrap().clone())
            })
            .unwrap();
        let before = document.get_heads();
        let task = if private {
            json!({"route": "model", "resource": {
                "agent_request": {"instruction": "Private managed-model goal"}
            }})
        } else {
            json!({"route": "manual", "title": "Plan the meeting", "audit": []})
        };
        let tasks = put(
            &mut document,
            tilo_desktop_model::projection::ROOM_TASKS_KEY,
            &task,
        );
        // The current projection is empty in both cases; retained history differs.
        document.delete(&tasks, "task").unwrap();
        document.commit();
        let changes = document
            .get_changes(&before)
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        owner
            .receive(
                room,
                TransportOrigin::RetainedRelayDelta(governor),
                NeighbourMsg::SyncResp {
                    room: testing::ROOM.into(),
                    changes,
                },
                now,
            )
            .unwrap();
        let member = ed25519_dalek::SigningKey::from_bytes(&[63; 32]);
        let result = owner.reconcile_outgoing(room, &member, now);
        if private {
            assert!(result.is_err());
        } else {
            result.unwrap();
        }
        for current in [&owner, &open_file(&path).unwrap()] {
            let outgoing = current.with_outgoing(room, now, |view, _| {
                assert_ne!(view.device().device_public_key, governor);
                assert!(view.record.sanitized_baseline.is_none());
                Ok(())
            });
            assert_eq!(outgoing.is_err(), private);
        }
    }
}

#[test]
fn task_free_history_needs_no_migration() {
    let mut room = AutoCommit::new();
    room.put(ROOT, "topic", "before").unwrap();
    room.commit();
    room.put(ROOT, "topic", "after").unwrap();
    room.commit();
    assert!(!needs_migration(&room));
}

#[test]
fn public_manual_controls_and_managed_metadata_need_no_migration() {
    let mut room = AutoCommit::new();
    put(
        &mut room,
        tilo_desktop_model::projection::ROOM_TASKS_KEY,
        &json!({"route": "manual", "title": "Public task", "audit": []}),
    );
    put(
        &mut room,
        tilo_peer_state::workspace_task::TASK_CONTROL_KEY,
        &json!({
            "instruction": "Public human instruction",
            "directions": [{"revision": 1, "text": "Public human direction"}],
            "presentations": {"public": {"text": "Human progress"}}
        }),
    );
    assert!(!needs_migration(&room));
    let mut metadata = AutoCommit::new();
    put(
        &mut metadata,
        tilo_desktop_model::projection::ROOM_TASKS_KEY,
        &json!({"route": "model", "title": "Explicit public label", "audit": []}),
    );
    assert!(!needs_migration(&metadata));
}

#[test]
fn deleted_private_control_history_still_requires_migration() {
    for value in [
        json!({"instruction": "Private instruction"}),
        json!({"directions": [{"revision": 1, "text": "Private direction"}]}),
        json!({"model_authority": {"contract": {"account": "Private account"}}}),
        json!({"presentations": {"run": {
            "run": "run", "sequence": 1, "text": "Private report",
            "reasoning": "Private reasoning", "detail": "Private detail"
        }}}),
    ] {
        let mut room = AutoCommit::new();
        let field = tilo_peer_state::workspace_task::TASK_CONTROL_KEY;
        let map = put(&mut room, field, &value);
        assert!(needs_migration(&room));
        room.delete(&map, "task").unwrap();
        room.commit();
        assert!(needs_migration(&room));
        room.delete(ROOT, field).unwrap();
        room.commit();
        let reopened = tilo_automerge_isolate::load(&room.save()).unwrap();
        assert!(needs_migration(&reopened));
    }
}

#[test]
fn historical_reports_and_nested_confirmation_audit_remain_private() {
    let report =
        tilo_desktop_model::WorkspaceTaskAuditEvent::new(7, "host", "abandon", std::iter::empty())
            .noting("Private model report");
    for task in [
        json!({"route": "model", "audit": [report]}),
        json!({"route": "manual", "audit": [{
            "what": {"action": {"action": "ask-confirmation"},
                "args": {"action": "Private draft label"}},
            "note": "Private review"
        }]}),
        json!({"route": "rules_only", "audit": [{
            "what": {"members": [{"action": {"action": "confirm-action"}, "args": {}}]},
            "note": "Private review reason"
        }]}),
    ] {
        let mut room = AutoCommit::new();
        let tasks = put(
            &mut room,
            tilo_desktop_model::projection::ROOM_TASKS_KEY,
            &task,
        );
        room.delete(&tasks, "task").unwrap();
        room.commit();
        assert!(needs_migration(&room));
    }
}

#[test]
fn confirmation_draft_and_decline_reason_remain_private_even_for_manual_tasks() {
    let mut confirmation = tilo_desktop_model::WorkspaceTaskConfirmation {
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
            description: Some("Private draft body".into()),
            ..Default::default()
        },
        arguments_digest: [5; 32],
        requested_at: 17,
        state: tilo_desktop_model::WorkspaceTaskConfirmationState::Waiting,
    };
    for decline in [false, true] {
        if decline {
            confirmation.draft = tilo_actions::Draft::default();
            confirmation.state = tilo_desktop_model::WorkspaceTaskConfirmationState::Declined {
                person: "person".into(),
                at: 18,
                reason: Some("Private reason".into()),
            };
        }
        let mut room = AutoCommit::new();
        put(
            &mut room,
            tilo_desktop_model::projection::ROOM_TASKS_KEY,
            &json!({"route": "manual"}),
        );
        let controls = put(
            &mut room,
            tilo_peer_state::workspace_task::TASK_CONTROL_KEY,
            &json!({"confirmation": confirmation}),
        );
        room.delete(&controls, "task").unwrap();
        room.commit();
        assert!(needs_migration(&room));
    }
}

#[test]
fn reference_shaped_direction_needs_authenticated_extraction_evidence() {
    let mut room = AutoCommit::new();
    let reference = tilo_peer_state::workspace_task::private_instruction_reference("private");
    put(
        &mut room,
        tilo_peer_state::workspace_task::TASK_CONTROL_KEY,
        &json!({"directions": [{"revision": 1, "text": reference}]}),
    );
    assert!(needs_migration(&room));
    let references = [("task".into(), [(1, reference)].into())].into();
    assert!(!has_private_task_history(&room, &references).unwrap());
}

#[test]
fn malformed_historical_roots_fail_closed() {
    let mut room = AutoCommit::new();
    room.put(
        ROOT,
        tilo_peer_state::workspace_task::TASK_CONTROL_KEY,
        "not a map",
    )
    .unwrap();
    room.commit();
    room.delete(ROOT, tilo_peer_state::workspace_task::TASK_CONTROL_KEY)
        .unwrap();
    room.commit();
    assert_eq!(
        has_private_task_history(&room, &std::collections::BTreeMap::default()).unwrap_err(),
        "task history root is not a map"
    );
}
