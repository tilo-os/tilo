use super::*;
use tilo_action_broker::services::WorkspaceTaskExecution;

#[test]
fn joined_replica_never_substitutes_local_mailboxes_or_hydrates_private_text() {
    let (directory, owner, room, _, _) =
        super::super::super::native_workspace_owner::test_enrolled_owner();
    let caller = owner
        .with_current(room, |view| {
            assert_ne!(view.device(), view.governor());
            Ok(view.device().clone())
        })
        .unwrap();
    let service = NativeOwnerCommitService::new(
        super::super::super::native_workspace_owner::tree_of(&owner),
        None,
    );
    let room_id = room;
    let room = room.to_string();
    let binding = NativeTaskBinding {
        task_id: "task:claimed".into(),
        agent_id: "claimed-agent".into(),
        epoch: 1,
        revision: 0,
        starter: None,
    };
    assert!(
        agent_authority(&owner, room_id, &binding)
            .unwrap_err()
            .contains("local execution host custody"),
        "a pinned remote host certificate must never be borrowed as local agent authority"
    );
    let path = directory.path().join("state.json");
    let before = std::fs::read(&path).unwrap();
    let denied = "actual governing owner";
    assert!(
        messages(&service, &room, &binding)
            .unwrap_err()
            .contains(denied)
    );
    assert!(
        service
            .private_instructions(&room, &binding)
            .unwrap_err()
            .contains(denied)
    );
    assert!(
        acknowledge(
            &service,
            &room,
            &binding,
            &[uuid::Uuid::new_v4().to_string()]
        )
        .unwrap_err()
        .contains(denied)
    );
    assert!(
        person_messages(&service, &room, &binding.task_id, binding.epoch, &caller)
            .unwrap_err()
            .contains(denied)
    );
    assert!(
        reports(&service, &room, &binding)
            .unwrap_err()
            .contains(denied)
    );
    assert!(
        person_reports(&service, &room, &binding.task_id, binding.epoch, &caller)
            .unwrap_err()
            .contains(denied)
    );
    assert!(
        person_outputs(&service, &room, &binding.task_id, binding.epoch, &caller)
            .unwrap_err()
            .contains(denied)
    );
    use tilo_action_broker::services::WorkspaceMemberships;
    assert!(
        service
            .task_private_instructions(&room, &binding.task_id, 1, &caller)
            .unwrap_err()
            .contains(denied)
    );
    assert!(
        service
            .task_messages(&room, &binding.task_id, 1, &caller)
            .unwrap_err()
            .contains(denied)
    );
    assert!(
        service
            .task_outputs(&room, &binding.task_id, 1, &caller)
            .unwrap_err()
            .contains(denied)
    );
    assert!(
        service
            .task_report_transcript(&room, &binding.task_id, 1, &caller)
            .unwrap_err()
            .contains(denied)
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn report_conversion_preserves_full_presentation_and_refuses_oversize() {
    use tilo_actions::task_messaging::{
        TaskReportKind, TaskReportPresentation, TaskReportPresentationFinish,
    };
    use tilo_joined_room::{
        NativePrivateTaskReport, NativePrivateTaskReportKind, NativePrivateTaskReportRole,
    };
    let presentation = TaskReportPresentation {
        run: "model-run:exact".into(),
        sequence: 23,
        received_at: Some(1_700_000_000),
        text: "é".repeat(3072),
        reasoning: format!("  reasoning\n{}", "é".repeat(506)),
        finish: Some(TaskReportPresentationFinish::Truncated),
        detail: format!("  finish detail\n{}", "é".repeat(504)),
    };
    let original = NativePrivateTaskReport {
        invocation: uuid::Uuid::new_v4(),
        source_device: [7; 32],
        role: NativePrivateTaskReportRole::Assistant,
        accepted_at_unix_ns: 123,
        sequence: 9,
        task_run: TaskRunAddress {
            task_id: "task:exact".into(),
            run_epoch: 4,
            agent_id: "scribe".into(),
        },
        actor: "agent:scribe".into(),
        kind: NativePrivateTaskReportKind::Presentation,
        body: presentation.text.clone(),
        presentation: Some(presentation.clone()),
    };
    assert_eq!(
        presentation.text.len() + presentation.reasoning.len() + presentation.detail.len(),
        TaskReportPresentation::MAX_PROSE_BYTES
    );
    let reports = report_dtos(vec![original.clone()]).unwrap();
    let report = &reports[0];
    assert_eq!(report.kind, TaskReportKind::Presentation);
    assert_eq!(report.presentation.as_ref(), Some(&presentation));
    assert_eq!(report.body, presentation.text);
    assert_eq!(report.invocation, original.invocation);
    assert_eq!(report.source_device, original.source_device);
    assert_eq!(report.accepted_at_unix_ns, original.accepted_at_unix_ns);
    assert_eq!(report.sequence, original.sequence);
    assert_eq!(report.task_run, original.task_run);
    assert_eq!(report.actor, original.actor);
    for field in ["text", "reasoning", "detail"] {
        let mut oversized = original.clone();
        let presentation = oversized.presentation.as_mut().unwrap();
        match field {
            "text" => {
                presentation.text.push('x');
                oversized.body = presentation.text.clone();
            }
            "reasoning" => presentation.reasoning.push('x'),
            _ => presentation.detail.push('x'),
        }
        assert!(report_dtos(vec![oversized]).is_err());
    }
    let mut legacy = original;
    legacy.kind = NativePrivateTaskReportKind::Finish;
    legacy.body = " legacy report\n".into();
    legacy.presentation = None;
    let reports = report_dtos(vec![legacy]).unwrap();
    assert_eq!(reports[0].kind, TaskReportKind::Finish);
    assert_eq!(reports[0].body, " legacy report\n");
    assert!(reports[0].presentation.is_none());
}
