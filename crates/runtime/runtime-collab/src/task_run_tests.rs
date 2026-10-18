use super::{
    INSTRUCTION_CONTEXT, INSTRUCTION_CONVERSATION, TaskRunEnvelope, TaskRunPlanning, TaskRunUpdate,
};

#[test]
fn task_control_mailbox_is_optional_bounded_and_exact_run() {
    let legacy = serde_json::json!({
        "version": 1, "task_id": "task:recipient", "run_epoch": 2,
        "from_revision": 0, "applied_revision": 0, "latest_revision": 0,
        "directions": [], "has_more": false
    });
    let mut control: super::TaskRunControl = serde_json::from_value(legacy).unwrap();
    assert!(control.messages.is_empty());
    control.validate().unwrap();
    let message = super::TaskMessage {
        version: 1,
        message_id: "00000000-0000-4000-8000-000000000001".into(),
        source: super::TaskRunAddress {
            task_id: "task:source".into(),
            run_epoch: 1,
            agent_id: "source-agent".into(),
        },
        recipient: super::TaskRunAddress {
            task_id: "task:recipient".into(),
            run_epoch: 2,
            agent_id: "recipient-agent".into(),
        },
        received_at: 1,
        sequence: 1,
        text: "Context, not instructions".into(),
        status: super::TaskMessageStatus::Accepted,
    };
    control.messages = vec![message.clone(); 4];
    control.validate().unwrap();
    control.messages.push(message.clone());
    assert!(control.validate().is_err());
    control.messages = vec![message];
    control.messages[0].recipient.run_epoch = 3;
    assert!(control.validate().is_err());
    control.messages[0].recipient.run_epoch = 2;
    control.messages[0].recipient.task_id = "task:other".into();
    assert!(control.validate().is_err());
}

#[test]
fn inbox_incorporation_update_preserves_message_ids() {
    let update = TaskRunUpdate::InboxIncorporated {
        message_ids: vec!["00000000-0000-4000-8000-000000000001".into()],
    };
    let value = serde_json::to_value(&update).unwrap();
    assert_eq!(value["kind"], "inbox_incorporated");
    assert_eq!(
        serde_json::from_value::<TaskRunUpdate>(value).unwrap(),
        update
    );
}

#[test]
fn task_run_wire_is_typed_and_rejects_unknown_fields() {
    let envelope = TaskRunEnvelope {
        paused: None,
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
        paused: None,
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
        paused: None,
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
        paused: None,
        version: 1,
        task_id: "task:3".into(),
        presentation_version: None,
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
