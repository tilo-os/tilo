use super::*;
use serde_json::json;

#[test]
fn presentation_history_is_archived_exactly_and_only_prose_leaves_are_scrubbed() {
    let mut room = AutoCommit::new();
    let tasks = room
        .put_object(
            ROOT,
            tilo_desktop_model::projection::ROOM_TASKS_KEY,
            ObjType::Map,
        )
        .unwrap();
    let controls = room
        .put_object(ROOT, TASK_CONTROL_KEY, ObjType::Map)
        .unwrap();
    for (id, route) in [
        ("automatic", "model"),
        ("deleted", "rules_only"),
        ("manual", "manual"),
    ] {
        room.put(
            &tasks,
            id,
            serde_json::to_vec(&json!({"route": route})).unwrap(),
        )
        .unwrap();
    }
    let presentation = json!({
        "run": "turn:1", "sequence": 4, "received_at": 1234,
        "text": "  exact private presentation answer\n  ",
        "reasoning": "private reasoning\n", "detail": "private detail",
        "finish": "complete"
    });
    let original = json!({
        "presentations": {"turn:1": presentation},
        "execution": {"sequence": 7, "phase": "executing"},
        "confirmation": null,
        "future": {"preserved": true}
    });
    for id in ["automatic", "deleted", "manual"] {
        room.put(&controls, id, serde_json::to_vec(&original).unwrap())
            .unwrap();
    }
    room.commit();
    let mut current = original.clone();
    current["presentations"]["turn:1"]["sequence"] = 5.into();
    current["presentations"]["turn:1"]["text"] = "replacement private answer".into();
    room.put(
        &controls,
        "automatic",
        serde_json::to_vec(&current).unwrap(),
    )
    .unwrap();
    room.delete(&tasks, "deleted").unwrap();
    room.delete(&controls, "deleted").unwrap();
    room.commit();
    assert!(validate_reports(&room).is_err());
    let mut malformed = room.clone();
    let mut invalid = current.clone();
    invalid["confirmation"] =
        json!({"future_confirmation_fields": [1, 2, 3], "draft": "unchanged"});
    malformed
        .put(
            &controls,
            "automatic",
            serde_json::to_vec(&invalid).unwrap(),
        )
        .unwrap();
    malformed.commit();
    assert!(sanitize_task_room(&malformed, &BTreeMap::new()).is_err());
    let (mut sanitized, archive) = sanitize_task_room(&room, &BTreeMap::new()).unwrap();
    validate_reports(&sanitized).unwrap();
    for id in ["automatic", "deleted"] {
        assert!(
            archive[id]
                .historical_reports
                .iter()
                .any(|entry| entry["presentation"] == presentation
                    && entry["note"] == presentation["text"])
        );
    }
    assert!(!archive.contains_key("manual"));
    let visible = blobs(&sanitized, TASK_CONTROL_KEY).unwrap();
    let public: serde_json::Value = serde_json::from_slice(&visible["automatic"]).unwrap();
    let mut expected = current;
    for field in ["text", "reasoning", "detail"] {
        expected["presentations"]["turn:1"][field] = "".into();
    }
    assert_eq!(public, expected);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&visible["manual"]).unwrap(),
        original
    );
    assert_eq!(sanitized.get_changes(&[]).len(), 1);
    // The outgoing fresh history contains no retired automatic turn snapshots.
    let restored = AutoCommit::load(&sanitized.save()).unwrap();
    let outgoing = reports::historical_reports(&restored).unwrap();
    assert!(!outgoing.contains_key("deleted"));
    assert!(outgoing["automatic"].historical_reports.iter().all(
        |event| event["presentation"]["text"] == ""
            && event["presentation"]["reasoning"] == ""
            && event["presentation"]["detail"] == ""
    ));
}
