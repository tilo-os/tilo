use super::*;
use tilo_actions::task_messaging::{TaskMessageStatus, TaskRunAddress};

fn envelope(argument: &serde_json::Value) -> InvocationEnvelope {
    InvocationEnvelope {
        id: 1,
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(100)),
        resource: "desktop:task:recipient".into(),
        action: "send-message".into(),
        argument: Some(format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            argument
        )),
        actor: "not-authority".into(),
        context: None,
    }
}

fn message(sequence: u64) -> TaskMessage {
    TaskMessage {
        version: 1,
        message_id: uuid::Uuid::from_u128(u128::from(sequence)).to_string(),
        source: TaskRunAddress {
            task_id: "task:sender".into(),
            run_epoch: 2,
            agent_id: "sender".into(),
        },
        recipient: TaskRunAddress {
            task_id: "task:recipient".into(),
            run_epoch: 3,
            agent_id: "recipient".into(),
        },
        received_at: 1,
        sequence,
        text: format!("Context {sequence}"),
        status: TaskMessageStatus::Accepted,
    }
}

#[test]
fn send_arguments_cannot_claim_source_or_authority() {
    let request = envelope(&serde_json::json!({"run_epoch":3,"text":"Context"}));
    let (recipient, body) = send(&request).unwrap().unwrap();
    assert_eq!(recipient, "task:recipient");
    assert_eq!(body.run_epoch, 3);
    for injected in ["actor", "source", "agent_id", "grant", "role"] {
        let mut argument = serde_json::json!({"run_epoch":3,"text":"Context"});
        argument[injected] = "invented".into();
        assert!(send(&envelope(&argument)).is_err(), "{injected}");
    }
    for argument in [
        serde_json::json!({"run_epoch":0,"text":"Context"}),
        serde_json::json!({"run_epoch":3,"text":" "}),
        serde_json::json!({"run_epoch":3,"text":"x".repeat(TaskMessageSend::MAX_TEXT_BYTES + 1)}),
    ] {
        assert!(send(&envelope(&argument)).is_err());
    }
    let mut unrelated = request;
    unrelated.action = "inspect".into();
    assert!(send(&unrelated).unwrap().is_none());
}

#[test]
fn registered_wait_is_a_bounded_cancellable_read_not_a_delivery_ack() {
    let action = wait_action("desktop:task:recipient", "Recipient");
    assert_eq!(action.action, "wait-task-message");
    assert_eq!(action.contract.kind, tilo_actions::ActionKind::Read);
    assert_eq!(action.operation_kind, tilo_actions::OperationKind::View);
    assert_eq!(action.contract.field.as_deref(), Some("inbox"));
    assert_eq!(
        action.contract.execution,
        tilo_actions::ExecutionMode::Background { cancellable: true }
    );
    assert!(action.contract.validate_declaration().is_empty());
    let mut request =
        envelope(&serde_json::json!({"run_epoch":3,"after_sequence":9,"timeout_ms":250}));
    request.action = "wait-task-message".into();
    let (task, args) = wait(&request).unwrap().unwrap();
    assert_eq!(task, "task:recipient");
    assert_eq!(args.after_sequence, 9);
    for injected in ["text", "agent_id", "source", "grant", "recipient"] {
        let mut value = serde_json::json!({"run_epoch":3,"after_sequence":9,"timeout_ms":250});
        value[injected] = "invented".into();
        request.argument = Some(format!("{}{}", tilo_actions::ARGUMENTS_JSON_PREFIX, value));
        assert!(wait(&request).is_err(), "{injected}");
    }
    for (epoch, timeout) in [(0, 1), (3, 0), (3, TaskMessageWait::MAX_TIMEOUT_MS + 1)] {
        request.argument = Some(format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::json!({"run_epoch":epoch,"after_sequence":0,"timeout_ms":timeout})
        ));
        assert!(wait(&request).is_err());
    }
}

#[test]
fn wait_page_uses_owner_recipient_and_cursor_without_acknowledging() {
    let run = message(1).recipient;
    let wait = TaskMessageWait {
        run_epoch: 3,
        after_sequence: 2,
        timeout_ms: 100,
    };
    let messages = (1..=7).rev().map(message).collect::<Vec<_>>();
    let page = wait_page(run.clone(), wait, messages, false).unwrap();
    assert_eq!(page.task_run, run);
    assert_eq!(
        page.messages
            .iter()
            .map(|mail| mail.sequence)
            .collect::<Vec<_>>(),
        [3, 4, 5, 6]
    );
    assert!(page.has_more);
    assert!(!page.timed_out);
    assert_eq!(page.next_sequence, 6);
    assert!(
        page.messages
            .iter()
            .all(|mail| mail.status == TaskMessageStatus::Accepted)
    );
    assert_eq!(
        format!("{page:?}"),
        "TaskMessageWaitResult { private: redacted }"
    );
    let empty = wait_page(run, wait, vec![message(1)], true).unwrap();
    assert!(empty.timed_out);
    assert!(empty.messages.is_empty());
    assert_eq!(empty.next_sequence, 2);
}

#[test]
fn wait_rejects_wrong_owner_recipient_and_duplicate_envelopes_even_before_cursor() {
    let run = message(1).recipient;
    let wait = TaskMessageWait {
        run_epoch: 3,
        after_sequence: 2,
        timeout_ms: 100,
    };
    for field in ["task", "epoch", "agent"] {
        let mut mail = message(1);
        match field {
            "task" => mail.recipient.task_id = "task:other".into(),
            "epoch" => mail.recipient.run_epoch = 4,
            _ => mail.recipient.agent_id = "other".into(),
        }
        assert!(wait_page(run.clone(), wait, vec![mail], false).is_err());
    }
    assert!(wait_page(run.clone(), wait, vec![message(1), message(1)], false).is_err());
    let mut reused = message(2);
    reused.message_id = message(1).message_id;
    assert!(wait_page(run, wait, vec![message(1), reused], false).is_err());
}

#[test]
fn conversation_pages_are_sorted_bounded_and_do_not_acknowledge() {
    let mut conversation = serde_json::json!({"task_id":"task:recipient","run_epoch":3,"turns":[]});
    let messages = (1..=6).rev().map(message).collect::<Vec<_>>();
    append_conversation(&mut conversation, messages.clone(), 0).unwrap();
    let turns = conversation["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 4);
    assert_eq!(turns[0]["sequence"], 1);
    assert_eq!(turns[3]["sequence"], 4);
    assert_eq!(turns[0]["kind"], "MessageFromTask");
    assert_eq!(turns[0]["status"], "Accepted");
    assert_eq!(conversation["messages_has_more"], true);
    assert_eq!(conversation["next_message_sequence"], 4);
    let mut next = serde_json::json!({"task_id":"task:recipient","run_epoch":3,"turns":[]});
    append_conversation(&mut next, messages, 4).unwrap();
    assert_eq!(next["turns"].as_array().unwrap().len(), 2);
    assert_eq!(next["turns"][0]["sequence"], 5);
    assert_eq!(next["messages_has_more"], false);
    assert_eq!(next["next_message_sequence"], 6);
}

#[test]
fn conversation_refuses_mail_for_another_task_or_run() {
    for (task, epoch) in [("task:other", 3), ("task:recipient", 4)] {
        let mut conversation = serde_json::json!({"task_id":task,"run_epoch":epoch,"turns":[]});
        assert!(append_conversation(&mut conversation, vec![message(1)], 0).is_err());
        assert!(conversation["turns"].as_array().unwrap().is_empty());
    }
}
