use super::*;
use serde_json::json;

fn event(action: &str, body: &str) -> serde_json::Value {
    serde_json::to_value(
        tilo_desktop_model::WorkspaceTaskAuditEvent::new(
            7,
            if action == "abandon" {
                "host"
            } else {
                "agent:scribe"
            },
            action,
            [("outcome", tilo_actions::Scalar::text("done"))],
        )
        .noting(body),
    )
    .unwrap()
}

#[test]
fn fresh_history_neutralizes_all_automatic_reports_and_recovers_retired_bodies() {
    let mut room = AutoCommit::new();
    let tasks = room
        .put_object(
            ROOT,
            tilo_desktop_model::projection::ROOM_TASKS_KEY,
            ObjType::Map,
        )
        .unwrap();
    let resource = json!({"app_id": "files", "resource_id": "verified-resource", "future": true});
    let mut automatic = json!({"route": "model", "title": "Public title", "resource": resource, "audit": [event("finish", "retired exact body\n  with spaces")]});
    room.put(&tasks, "automatic", serde_json::to_vec(&automatic).unwrap())
        .unwrap();
    room.commit();
    automatic["audit"] = json!([
        event("finish", "private finish"),
        event("abandon", "private abandonment"),
        event("public-progress", "private progress"),
        event("note", "older automatic audit summary"),
    ]);
    room.put(&tasks, "automatic", serde_json::to_vec(&automatic).unwrap())
        .unwrap();
    let manual = json!({"route": "manual", "title": "Public human task", "resource": resource, "audit": [event("finish", "Explicit public manual report")]});
    let manual_bytes = serde_json::to_vec_pretty(&manual).unwrap();
    room.put(&tasks, "manual", manual_bytes.clone()).unwrap();
    let rules =
        json!({"route": "rules_only", "audit": [event("abandon", "private rules failure")]});
    room.put(&tasks, "rules", serde_json::to_vec(&rules).unwrap())
        .unwrap();
    room.put(&tasks, "deleted", serde_json::to_vec(&rules).unwrap())
        .unwrap();
    room.commit();
    room.delete(&tasks, "deleted").unwrap();
    room.commit();
    // Round-trip the historical source: this is persisted CRDT history, not
    // ephemeral mutation input that could disappear before migration.
    let source = AutoCommit::load(&room.save()).unwrap();
    let (mut sanitized, private) = sanitize_task_room(&source, &BTreeMap::new()).unwrap();
    assert_eq!(private["automatic"].historical_reports.len(), 5);
    assert!(
        private["automatic"]
            .historical_reports
            .iter()
            .any(|entry| entry["note"] == "retired exact body\n  with spaces")
    );
    assert_eq!(
        private["deleted"].historical_reports[0]["note"],
        "private rules failure"
    );
    assert!(!private.contains_key("manual"));
    assert_eq!(sanitized.get_changes(&[]).len(), 1);
    validate_reports(&sanitized).unwrap();
    assert!(validate_reports(&source).is_err());
    let visible = blobs(&sanitized, tilo_desktop_model::projection::ROOM_TASKS_KEY).unwrap();
    assert_eq!(visible["manual"], manual_bytes);
    let public: serde_json::Value = serde_json::from_slice(&visible["automatic"]).unwrap();
    assert_eq!(public["resource"], resource);
    assert_eq!(public["title"], "Public title");
    assert_eq!(public["audit"][0]["note"], "done");
    assert_eq!(public["audit"][1]["note"], "abandoned");
    assert_eq!(public["audit"][2]["note"], "progress");
    assert_eq!(public["audit"][3]["note"], "progress");
    assert_eq!(public["audit"][0]["what"], automatic["audit"][0]["what"]);
    let mut transport = vec![sanitized.save()];
    transport.extend(
        sanitized
            .get_changes(&[])
            .iter()
            .map(|change| change.raw_bytes().to_vec()),
    );
    for secret in [
        "retired exact body",
        "private finish",
        "private abandonment",
        "private progress",
        "older automatic audit summary",
        "private rules failure",
    ] {
        for bytes in &transport {
            assert!(
                !bytes
                    .windows(secret.len())
                    .any(|part| part == secret.as_bytes())
            );
        }
    }
    // Also inspect materialized values after loading the outgoing checkpoint;
    // compressed serialization alone is not proof of absence.
    let outgoing = AutoCommit::load(&sanitized.save()).unwrap();
    let recovered = reports::historical_reports(&outgoing).unwrap();
    for task in recovered.values() {
        for entry in &task.historical_reports {
            assert!(matches!(
                entry["note"].as_str(),
                Some("done" | "abandoned" | "progress")
            ));
        }
    }
}
