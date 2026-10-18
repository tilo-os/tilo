use super::*;

#[test]
fn legacy_control_absence_defaults_without_erasing_frontier_or_presentation() {
    let control = WorkspaceTaskControl {
        run_epoch: 7,
        instruction: Some("legacy private custody".into()),
        confirmation: Some(WorkspaceTaskConfirmation {
            id: "review".into(),
            asking: uuid::Uuid::from_u128(1),
            run_epoch: 7,
            agent_id: "scribe".into(),
            app: "so.tilo.mail".into(),
            resource_kind: "mail.mailbox".into(),
            resource: "inbox".into(),
            action: "send_message".into(),
            kind: tilo_action_policy::ActionKind::Outside,
            risk: tilo_actions::RiskLevel::Destructive,
            draft: tilo_actions::Draft::default(),
            arguments_digest: [7; 32],
            requested_at: 11,
            state: WorkspaceTaskConfirmationState::Waiting,
        }),
        receipts: vec![WorkspaceTaskMutationReceipt {
            device: [8; 32],
            invocation: uuid::Uuid::from_u128(2),
            request_digest: [9; 32],
            result: WorkspaceTaskMutationResult::Queued {
                task_id: "task:one".into(),
            },
        }],
        presentations: std::collections::BTreeMap::from([(
            "turn:1".into(),
            collab_types::task_presentation::TaskPresentation {
                run: "turn:1".into(),
                sequence: 3,
                received_at: Some(12),
                text: "answer".into(),
                reasoning: "reasoning".into(),
                finish: Some(collab_types::task_presentation::TaskPresentationFinish::Complete),
                detail: "detail".into(),
            },
        )]),
        ..Default::default()
    };
    let legacy = serde_json::to_value(&control).unwrap();
    assert!(legacy.get("private_instruction_ref").is_none());
    let mut restored: WorkspaceTaskControl = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(restored, control);
    assert!(restored.private_instruction_ref.is_none());
    restored.private_instruction_ref = Some(format!("private-instruction:{}", "a".repeat(64)));
    let encoded = serde_json::to_value(&restored).unwrap();
    assert_eq!(encoded["run_epoch"], legacy["run_epoch"]);
    assert_eq!(encoded["presentations"], legacy["presentations"]);
    assert_eq!(encoded["instruction"], legacy["instruction"]);
    assert_eq!(encoded["confirmation"], legacy["confirmation"]);
    assert_eq!(encoded["receipts"], legacy["receipts"]);
    assert_eq!(
        serde_json::from_value::<WorkspaceTaskControl>(encoded).unwrap(),
        restored
    );
}
