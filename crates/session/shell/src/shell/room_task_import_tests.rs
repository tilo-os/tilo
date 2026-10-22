#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion"
)]

use automerge::{AutoCommit, ObjType, ROOT, ScalarValue, transaction::Transactable};
use serde_json::json;
use tasks::model::Status;

use super::room_tasks;

fn put(doc: &mut AutoCommit, map: &str, key: &str, value: &serde_json::Value) {
    let map = match automerge::ReadDoc::get(doc, ROOT, map).unwrap() {
        Some((_, id)) => id,
        None => doc.put_object(ROOT, map, ObjType::Map).unwrap(),
    };
    doc.put(
        &map,
        key,
        ScalarValue::Bytes(serde_json::to_vec(value).unwrap()),
    )
    .unwrap();
}

fn said(action: &str) -> serde_json::Value {
    serde_json::to_value(tilo_actions::Did::verb(
        tilo_actions::ActionKey::new("so.tilo.desktop", "workspace-task", action),
        [("run_epoch".to_owned(), tilo_actions::Scalar::text("1"))],
    ))
    .unwrap()
}

#[test]
fn a_rooms_task_reads_as_the_issue_it_becomes_with_its_conversation() {
    let mut doc = AutoCommit::new();
    put(
        &mut doc,
        "tasks_v1",
        "t1",
        &json!({
            "id": "t1",
            "title": "Tidy the docs",
            "created_by": "ada",
            "assigned_agent": "claude",
            "route": "model",
            "status": "in_progress",
            "resource": null,
            "created_at": 100,
            "updated_at": 130,
            "audit": [
                {"at": 120, "actor": "agent:claude", "what": said("public-progress"), "note": "Reading them"},
                {"at": 125, "actor": "ada", "what": said("approve-run")},
            ],
        }),
    );
    put(
        &mut doc,
        "task_control_v1",
        "t1",
        &json!({
            "run_epoch": 1,
            "instruction": "Keep the headings",
            "directions": [
                {"revision": 1, "invocation": "00000000-0000-0000-0000-000000000001", "author": "ada", "text": "Short lines", "created_at": 110},
            ],
            "receipts": [],
        }),
    );
    put(
        &mut doc,
        "tasks_v1",
        "t2",
        &json!({"id": "t2", "title": "Written by hand", "created_by": "bo", "status": "done", "created_at": 50, "updated_at": 60}),
    );
    put(&mut doc, "tasks_v1", "broken", &json!({"title": 3}));

    let tasks = room_tasks(&doc);
    assert_eq!(
        tasks
            .iter()
            .map(|task| (task.task.as_str(), task.status))
            .collect::<Vec<_>>(),
        [("t2", Status::Done), ("t1", Status::Todo)]
    );
    let task = &tasks[1];
    assert_eq!(task.instruction, "Keep the headings");
    assert_eq!(task.assignee.as_deref(), Some("agent:claude"));
    assert_eq!(task.created_ms, 100_000);
    assert_eq!(
        task.messages
            .iter()
            .map(|message| (message.text.as_str(), message.by_agent))
            .collect::<Vec<_>>(),
        [("Short lines", false), ("Reading them", true)]
    );
    assert_eq!(tasks[0].assignee, None);
}
