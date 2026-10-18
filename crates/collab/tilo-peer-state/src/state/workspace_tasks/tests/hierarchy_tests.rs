use super::*;
use tilo_desktop_model::{WorkspaceTaskMetadata, WorkspaceTaskMutationResult};

fn submit(
    fixture: &mut SteeringFixture,
    command: &WorkspaceTaskCommand,
) -> Result<WorkspaceTaskMutationResult, String> {
    fixture
        .peer
        .workspace_mut()
        .apply_workspace_task_command(
            uuid::Uuid::new_v4(),
            [14; 32],
            &fixture.caller,
            command,
            fixture.now,
        )
        .map(|(_, result)| result)
}

fn manual(fixture: &mut SteeringFixture, title: &str) -> String {
    let result = submit(
        fixture,
        &WorkspaceTaskCommand::CreateManual {
            title: title.into(),
            resource: None,
        },
    )
    .unwrap();
    let WorkspaceTaskMutationResult::Queued { task_id } = result else {
        panic!("expected queue result")
    };
    task_id
}

fn deny(fixture: &mut SteeringFixture, task_id: &str) {
    let node = TreePath::room(ROOM)
        .app(tilo_desktop_model::workspace::WORKSPACE_GOVERNANCE_APP_ID)
        .kind(tilo_desktop_model::TASK_KIND)
        .resource(format!("desktop:{task_id}"));
    fixture
        .peer
        .governance_mut()
        .put_governed_grants(
            &[Grant::new(
                "parent-write-denied",
                fixture.subject.clone(),
                node,
                KindSet::UNDOABLE,
                Answer::Deny,
                Subject::person("governor"),
            )],
            &SigningKey::from_bytes(&[2; 32]),
            fixture.now,
        )
        .unwrap();
}

#[test]
fn subtask_creation_rechecks_parent_resource_write_permission() {
    let mut fixture = SteeringFixture::new();
    let parent = manual(&mut fixture, "Parent");
    deny(&mut fixture, &parent);
    let before = fixture.peer.durable().unwrap();
    for command in [
        WorkspaceTaskCommand::CreateSubtask {
            parent_task_id: parent.clone(),
            title: "Child".into(),
            assigned_agent: None,
            route: None,
        },
        WorkspaceTaskCommand::QueueWithMetadata {
            title: "Child".into(),
            assigned_agent: None,
            route: WorkspaceTaskRoute::Model,
            metadata: WorkspaceTaskMetadata {
                parent_task_id: Some(parent.clone()),
                ..Default::default()
            },
        },
        WorkspaceTaskCommand::QueuePrivateSubtask {
            parent_task_id: parent,
            title: "Child".into(),
            instruction: "Private goal".into(),
            assigned_agent: String::new(),
            route: WorkspaceTaskRoute::Model,
        },
    ] {
        assert!(
            submit(&mut fixture, &command)
                .unwrap_err()
                .contains("not writable")
        );
        assert_eq!(fixture.peer.durable().unwrap(), before);
    }
}

#[test]
fn moving_or_editing_child_cannot_escape_revoked_parent_authority() {
    let mut fixture = SteeringFixture::new();
    let parent = manual(&mut fixture, "Parent");
    let WorkspaceTaskMutationResult::Queued { task_id: child } = submit(
        &mut fixture,
        &WorkspaceTaskCommand::CreateSubtask {
            parent_task_id: parent.clone(),
            title: "Child".into(),
            assigned_agent: None,
            route: None,
        },
    )
    .unwrap() else {
        panic!("expected child")
    };
    deny(&mut fixture, &parent);
    let before = fixture.peer.durable().unwrap();
    for command in [
        WorkspaceTaskCommand::MoveTask {
            task_id: child.clone(),
            parent_task_id: None,
            child_order: None,
        },
        WorkspaceTaskCommand::SetMetadata {
            task_id: child.clone(),
            metadata: WorkspaceTaskMetadata::default(),
        },
        WorkspaceTaskCommand::SetLabels {
            task_id: child,
            labels: vec!["review".into()],
        },
    ] {
        assert!(
            submit(&mut fixture, &command)
                .unwrap_err()
                .contains("not writable")
        );
        assert_eq!(fixture.peer.durable().unwrap(), before);
    }
}

#[test]
fn unassigned_model_queue_needs_no_model_configuration_or_agent_membership() {
    let (mut peer, governor, _, now) = peer();
    let before = crate::workspace_task::snapshot(peer.core.room_doc.as_deref().unwrap()).unwrap();
    let (_, result) = peer
        .workspace_mut()
        .apply_workspace_task_command(
            uuid::Uuid::new_v4(),
            [15; 32],
            &WorkspaceTaskAuthority::Person { caller: governor },
            &WorkspaceTaskCommand::QueueWithMetadata {
                title: "Task".into(),
                assigned_agent: None,
                route: WorkspaceTaskRoute::Model,
                metadata: WorkspaceTaskMetadata::default(),
            },
            now,
        )
        .unwrap();
    let WorkspaceTaskMutationResult::Queued { task_id } = result else {
        panic!("expected queue")
    };
    let after = crate::workspace_task::snapshot(peer.core.room_doc.as_deref().unwrap()).unwrap();
    let task = after.tasks.iter().find(|task| task.id == task_id).unwrap();
    assert!(task.assigned_agent.is_none());
    assert_eq!(task.status, WorkspaceTaskStatus::Queued);
    assert_eq!(after.grants, before.grants);
    assert!(after.controls[&task_id].active_run.is_none());
    assert!(after.controls[&task_id].execution_authority.is_none());
}
