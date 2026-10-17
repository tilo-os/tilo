use super::{
    INSTRUCTION_CONTEXT, INSTRUCTION_CONVERSATION, TaskRunEnvelope, TaskRunPlanning, TaskRunUpdate,
};

#[test]
fn task_run_wire_is_typed_and_rejects_unknown_fields() {
    let envelope = TaskRunEnvelope {
        version: 1,
        task_id: "task:1".into(),
        title: "Review proposal".into(),
        instruction: Some("Review the complete proposal and list every blocker".into()),
        subject: None,
        run_id: 7,
        lease: "broker-lease".into(),
        planning: TaskRunPlanning::RulesOnly,
        presentation_version: None,
        execution_budget: None,
        action_context: serde_json::json!({"purpose": "Review proposal"}),
    };
    let json = serde_json::to_string(&envelope).unwrap();
    assert_eq!(
        serde_json::from_str::<TaskRunEnvelope>(&json).unwrap(),
        envelope
    );
    assert_eq!(
        envelope.instruction(),
        "Review the complete proposal and list every blocker"
    );
    let legacy = serde_json::from_str::<TaskRunEnvelope>(
        r#"{"version":1,"task_id":"t","title":"legacy prompt","run_id":1,"lease":"l","planning":"rules_only","action_context":{}}"#,
    )
    .unwrap();
    assert_eq!(legacy.instruction(), "legacy prompt");
    assert!(serde_json::from_str::<TaskRunEnvelope>(
        r#"{"version":1,"task_id":"t","title":"x","run_id":1,"lease":"l","planning":"rules_only","action_context":{},"path":"/tmp"}"#
    )
    .is_err());
    assert!(serde_json::from_str::<TaskRunEnvelope>(
        r#"{"version":1,"task_id":"t","title":"x","run_id":1,"lease":"l","planning":"unattended","action_context":{}}"#
    )
    .is_err());
    assert_eq!(
        serde_json::from_str::<TaskRunUpdate>(r#"{"kind":"progress","message":"checking"}"#)
            .unwrap(),
        TaskRunUpdate::Progress {
            message: "checking".into(),
            presentation: None,
        }
    );
}

#[test]
fn task_run_carries_the_selection_it_is_about_without_breaking_older_runs() {
    let subject = tilo_actions::SelectionSubject {
        app_id: "so.tilo.notes".into(),
        app_label: "Notes".into(),
        resource: tilo_actions::ActionResource {
            id: "notes.notebook/pages/1/body".into(),
            kind: "rich-text".into(),
            label: "Ideas".into(),
        },
        anchor: tilo_actions::SelectionAnchor::of_text("the opening sentence")
            .at(tilo_actions::SelectionPlace::range("block-1", 0, 20)),
    };
    let envelope = TaskRunEnvelope {
        version: 1,
        task_id: "task:2".into(),
        presentation_version: None,
        title: "Tighten this".into(),
        instruction: Some("Tighten the opening sentence".into()),
        subject: Some(subject.clone()),
        run_id: 8,
        lease: "broker-lease".into(),
        planning: TaskRunPlanning::Model,
        execution_budget: None,
        action_context: serde_json::json!({"purpose": "Tighten this"}),
    };
    let json = serde_json::to_value(&envelope).unwrap();
    assert_eq!(json["subject"]["anchor"]["place"]["start"], 0);
    assert_eq!(json["subject"]["resource"]["label"], "Ideas");
    assert_eq!(
        serde_json::from_value::<TaskRunEnvelope>(json)
            .unwrap()
            .subject,
        Some(subject)
    );
    let without = TaskRunEnvelope {
        subject: None,
        ..envelope
    };
    assert!(
        serde_json::to_value(&without)
            .unwrap()
            .get("subject")
            .is_none(),
        "a run about no selection is the run an older agent reads"
    );
}

#[test]
fn the_request_is_the_persons_own_words_without_context_or_conversation() {
    let envelope = |instruction: &str| TaskRunEnvelope {
        version: 1,
        task_id: "task:3".into(),
        title: "Summarise".into(),
        instruction: Some(instruction.into()),
        subject: None,
        run_id: 9,
        lease: "broker-lease".into(),
        planning: TaskRunPlanning::Model,
        execution_budget: None,
        action_context: serde_json::json!({}),
    };
    let plain = envelope("make a summary note of this");
    assert_eq!(plain.request(), "make a summary note of this");
    let continued = envelope(&format!(
        "make a summary note of this{INSTRUCTION_CONTEXT}\n<<<\nWhich files?\n>>>\
         {INSTRUCTION_CONVERSATION}\nYou: apps and files share one history?"
    ));
    assert_eq!(continued.request(), "make a summary note of this");
    let recalled = envelope(&format!(
        "and in Writer?{INSTRUCTION_CONVERSATION}\nPerson: hi"
    ));
    assert_eq!(recalled.request(), "and in Writer?");
}
