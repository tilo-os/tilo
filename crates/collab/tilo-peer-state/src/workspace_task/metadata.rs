//! Coordination metadata never changes an active run, instruction or grant.
use super::{
    AdmittedTaskActor, CommandContext, WorkspaceTask, WorkspaceTaskAuditEvent,
    WorkspaceTaskCommand, WorkspaceTaskControl, WorkspaceTaskMutationResult, WorkspaceTaskRoute,
    WorkspaceTaskStatus, dispatch_queue, manual_command, task_mut,
};

pub(super) fn metadata_command(
    tasks: &mut Vec<WorkspaceTask>,
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    command: &WorkspaceTaskCommand,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let AdmittedTaskActor::Person { id: author, .. } = context.actor else {
        return Err("only a person may change task hierarchy or responsibility".into());
    };
    if matches!(command, WorkspaceTaskCommand::QueueWithMetadata { .. }) {
        return queue_with_metadata(tasks, controls, context, command);
    }
    if matches!(command, WorkspaceTaskCommand::CreateSubtask { .. }) {
        return create_subtask(tasks, controls, context, command);
    }
    if let WorkspaceTaskCommand::BindExecutor {
        task_id,
        agent_id,
        expected_run_epoch,
        after_revision,
    } = command
    {
        return bind_executor(
            tasks,
            controls,
            context,
            task_id,
            agent_id,
            (*expected_run_epoch, *after_revision),
        );
    }
    let (task_id, verb) = match command {
        WorkspaceTaskCommand::SetMetadata { task_id, metadata } => {
            validate_metadata(tasks, task_id, metadata)?;
            (task_id, "set-metadata")
        }
        WorkspaceTaskCommand::MoveTask {
            task_id,
            parent_task_id,
            ..
        } => {
            validate_parent(tasks, task_id, parent_task_id.as_deref())?;
            (task_id, "move-task")
        }
        WorkspaceTaskCommand::SetLabels { task_id, labels } => {
            validate_values(labels)?;
            (task_id, "set-labels")
        }
        WorkspaceTaskCommand::SetAssignees {
            task_id,
            responsible_assignees,
        } => {
            validate_values(responsible_assignees)?;
            (task_id, "set-assignees")
        }
        _ => return Err("not a task metadata command".into()),
    };
    let task = task_mut(tasks, task_id)?;
    match command {
        WorkspaceTaskCommand::SetMetadata { metadata, .. } => {
            set_metadata(task, metadata);
        }
        WorkspaceTaskCommand::MoveTask {
            parent_task_id,
            child_order,
            ..
        } => {
            task.parent_task_id.clone_from(parent_task_id);
            task.child_order = *child_order;
        }
        WorkspaceTaskCommand::SetLabels { labels, .. } => task.labels.clone_from(labels),
        WorkspaceTaskCommand::SetAssignees {
            responsible_assignees,
            ..
        } => {
            task.responsible_assignees.clone_from(responsible_assignees);
        }
        _ => return Err("not a task metadata command".into()),
    }
    task.updated_at = context.now;
    task.audit
        .push(WorkspaceTaskAuditEvent::new(context.now, author, verb, []));
    Ok((
        task_id.clone(),
        WorkspaceTaskMutationResult::MetadataUpdated {
            task_id: task_id.clone(),
        },
    ))
}

fn validate_metadata(
    tasks: &[WorkspaceTask],
    task_id: &str,
    metadata: &tilo_desktop_model::WorkspaceTaskMetadata,
) -> Result<(), String> {
    validate_parent(tasks, task_id, metadata.parent_task_id.as_deref())?;
    validate_values(&metadata.labels)?;
    validate_values(&metadata.responsible_assignees)
}

fn set_metadata(task: &mut WorkspaceTask, metadata: &tilo_desktop_model::WorkspaceTaskMetadata) {
    task.parent_task_id.clone_from(&metadata.parent_task_id);
    task.child_order = metadata.child_order;
    task.labels.clone_from(&metadata.labels);
    task.responsible_assignees
        .clone_from(&metadata.responsible_assignees);
}

fn queue_with_metadata(
    tasks: &mut Vec<WorkspaceTask>,
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    command: &WorkspaceTaskCommand,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let WorkspaceTaskCommand::QueueWithMetadata {
        title,
        assigned_agent,
        route,
        metadata,
    } = command
    else {
        return Err("not a queue metadata command".into());
    };
    validate_metadata(tasks, "", metadata)?;
    let created = dispatch_queue(
        tasks,
        controls,
        context,
        &WorkspaceTaskCommand::Queue {
            title: title.clone(),
            assigned_agent: assigned_agent.clone().unwrap_or_default(),
            route: *route,
            resource: None,
        },
    )?;
    set_metadata(task_mut(tasks, &created.0)?, metadata);
    Ok(created)
}

fn create_subtask(
    tasks: &mut Vec<WorkspaceTask>,
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    command: &WorkspaceTaskCommand,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let WorkspaceTaskCommand::CreateSubtask {
        parent_task_id,
        title,
        assigned_agent,
        route,
    } = command
    else {
        return Err("not a subtask creation command".into());
    };
    validate_parent(tasks, "", Some(parent_task_id))?;
    let created = match (assigned_agent, route) {
        (None, None | Some(WorkspaceTaskRoute::Manual)) => manual_command(
            tasks,
            controls,
            context,
            &WorkspaceTaskCommand::CreateManual {
                title: title.clone(),
                resource: None,
            },
        )?,
        (None, Some(WorkspaceTaskRoute::Model)) => dispatch_queue(
            tasks,
            controls,
            context,
            &WorkspaceTaskCommand::Queue {
                title: title.clone(),
                assigned_agent: String::new(),
                route: WorkspaceTaskRoute::Model,
                resource: None,
            },
        )?,
        (
            Some(agent),
            Some(route @ (WorkspaceTaskRoute::RulesOnly | WorkspaceTaskRoute::Model)),
        ) => dispatch_queue(
            tasks,
            controls,
            context,
            &WorkspaceTaskCommand::Queue {
                title: title.clone(),
                assigned_agent: agent.clone(),
                route: *route,
                resource: None,
            },
        )?,
        _ => {
            return Err(
                "subtask execution route requires a model queue or an explicit agent and non-manual route"
                    .into(),
            );
        }
    };
    task_mut(tasks, &created.0)?.parent_task_id = Some(parent_task_id.clone());
    Ok(created)
}

fn bind_executor(
    tasks: &mut [WorkspaceTask],
    controls: &std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    task_id: &str,
    agent_id: &str,
    frontier: (u64, u64),
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let AdmittedTaskActor::Person { id: author, .. } = context.actor else {
        return Err("only a person may bind a task executor".into());
    };
    if agent_id.trim().is_empty() || agent_id.trim() != agent_id {
        return Err("task executor must be a nonempty agent identity".into());
    }
    let task = task_mut(tasks, task_id)?;
    let control = controls
        .get(task_id)
        .ok_or("task has no execution control")?;
    if task.route != WorkspaceTaskRoute::Model
        || task.status != WorkspaceTaskStatus::Queued
        || task.assigned_agent.is_some()
        || control.active_run.is_some()
        || control.run_epoch != frontier.0
        || control
            .directions
            .last()
            .map_or(0, |direction| direction.revision)
            != frontier.1
    {
        return Err("unbound queued task frontier changed; review before binding".into());
    }
    task.assigned_agent = Some(agent_id.to_owned());
    task.updated_at = context.now;
    task.audit.push(WorkspaceTaskAuditEvent::new(
        context.now,
        author,
        "bind-executor",
        [],
    ));
    Ok((
        task_id.to_owned(),
        WorkspaceTaskMutationResult::MetadataUpdated {
            task_id: task_id.to_owned(),
        },
    ))
}

fn validate_values(values: &[String]) -> Result<(), String> {
    let unique: std::collections::BTreeSet<_> = values.iter().collect();
    if values.len() > 32
        || unique.len() != values.len()
        || values.iter().any(|value| {
            value.trim().is_empty()
                || value.trim() != value
                || value.len() > 128
                || value.chars().any(char::is_control)
        })
    {
        return Err("task metadata must contain at most 32 distinct bounded values".into());
    }
    Ok(())
}

fn validate_parent(
    tasks: &[WorkspaceTask],
    task_id: &str,
    parent: Option<&str>,
) -> Result<(), String> {
    let mut next = parent;
    let mut seen = std::collections::BTreeSet::new();
    while let Some(id) = next {
        if id == task_id || !seen.insert(id) {
            return Err("task hierarchy contains a cycle".into());
        }
        let task = tasks
            .iter()
            .find(|task| task.id == id)
            .ok_or("parent task is absent from this workspace")?;
        next = task.parent_task_id.as_deref();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{
        AutoCommit, WorkspaceTaskActiveRun, WorkspaceTaskFinish, apply, project_controls, snapshot,
    };
    use super::*;
    use tilo_desktop_model::WorkspaceTaskMetadata;

    fn apply_command(
        doc: &mut AutoCommit,
        command: impl std::borrow::Borrow<WorkspaceTaskCommand>,
    ) -> Result<WorkspaceTaskMutationResult, String> {
        let command = command.borrow();
        let id = uuid::Uuid::new_v4();
        let digest = *blake3::hash(&postcard::to_stdvec(&command).unwrap()).as_bytes();
        apply(
            doc,
            id,
            digest,
            &AdmittedTaskActor::Person {
                id: "owner".into(),
                device: [1; 32],
            },
            command,
            1,
        )
        .map(|value| value.result)
    }

    #[test]
    fn unbound_model_queue_is_not_a_manual_task_or_a_runnable_agent() {
        let mut doc = AutoCommit::new();
        let parent = create(
            &mut doc,
            WorkspaceTaskCommand::Queue {
                title: "Model request".into(),
                assigned_agent: String::new(),
                route: WorkspaceTaskRoute::Model,
                resource: None,
            },
        );
        let child = create(
            &mut doc,
            WorkspaceTaskCommand::CreateSubtask {
                parent_task_id: parent.clone(),
                title: "Child request".into(),
                assigned_agent: None,
                route: Some(WorkspaceTaskRoute::Model),
            },
        );
        let state = snapshot(&doc).unwrap();
        for task in &state.tasks {
            assert_eq!(task.route, WorkspaceTaskRoute::Model);
            assert_eq!(task.status, WorkspaceTaskStatus::Queued);
            assert!(task.assigned_agent.is_none());
            assert!(state.controls[&task.id].active_run.is_none());
            assert_eq!(state.controls[&task.id].run_epoch, 0);
        }
        assert_eq!(
            state
                .tasks
                .iter()
                .find(|task| task.id == child)
                .unwrap()
                .parent_task_id,
            Some(parent.clone())
        );
        for agent_id in ["", "scribe"] {
            let command = WorkspaceTaskCommand::StartRun {
                task_id: parent.clone(),
                agent_id: agent_id.into(),
            };
            assert!(
                apply(
                    &mut doc,
                    uuid::Uuid::new_v4(),
                    [0; 32],
                    &AdmittedTaskActor::Host { device: [2; 32] },
                    &command,
                    2,
                )
                .is_err()
            );
            assert_eq!(snapshot(&doc).unwrap(), state);
        }
        for route in [WorkspaceTaskRoute::RulesOnly, WorkspaceTaskRoute::Manual] {
            assert!(
                apply_command(
                    &mut doc,
                    WorkspaceTaskCommand::Queue {
                        title: "Invalid unbound route".into(),
                        assigned_agent: String::new(),
                        route,
                        resource: None,
                    }
                )
                .is_err()
            );
            assert_eq!(snapshot(&doc).unwrap(), state);
        }
    }

    fn create(doc: &mut AutoCommit, command: WorkspaceTaskCommand) -> String {
        let WorkspaceTaskMutationResult::Queued { task_id } = apply_command(doc, command).unwrap()
        else {
            panic!("creation result")
        };
        task_id
    }

    #[test]
    fn raw_private_subtask_requires_owner_preparation_without_a_public_write() {
        let mut doc = AutoCommit::new();
        let parent = create(
            &mut doc,
            WorkspaceTaskCommand::CreateManual {
                title: "Parent".into(),
                resource: None,
            },
        );
        let before = snapshot(&doc).unwrap();
        let heads = doc.get_heads();
        assert!(
            apply_command(
                &mut doc,
                WorkspaceTaskCommand::QueuePrivateSubtask {
                    parent_task_id: parent,
                    title: "Child".into(),
                    instruction: "Never publish this private goal".into(),
                    assigned_agent: "scribe".into(),
                    route: WorkspaceTaskRoute::Model,
                },
            )
            .unwrap_err()
            .contains("owner-private preparation")
        );
        assert_eq!(snapshot(&doc).unwrap(), before);
        assert_eq!(doc.get_heads(), heads);
    }

    #[test]
    fn hierarchy_survives_serialization_and_refuses_cycles_missing_and_foreign_parents() {
        let mut doc = AutoCommit::new();
        let parent = create(
            &mut doc,
            WorkspaceTaskCommand::CreateManual {
                title: "Parent".into(),
                resource: None,
            },
        );
        let child = create(
            &mut doc,
            WorkspaceTaskCommand::CreateSubtask {
                parent_task_id: parent.clone(),
                title: "Child".into(),
                assigned_agent: None,
                route: None,
            },
        );
        for (task_id, parent_task_id) in [
            (parent.clone(), child.clone()),
            (child.clone(), child.clone()),
            (child.clone(), "task:foreign".into()),
        ] {
            assert!(
                apply_command(
                    &mut doc,
                    WorkspaceTaskCommand::MoveTask {
                        task_id,
                        parent_task_id: Some(parent_task_id),
                        child_order: None
                    }
                )
                .is_err()
            );
        }
        apply_command(
            &mut doc,
            WorkspaceTaskCommand::SetLabels {
                task_id: child.clone(),
                labels: vec!["review".into()],
            },
        )
        .unwrap();
        apply_command(
            &mut doc,
            WorkspaceTaskCommand::SetAssignees {
                task_id: child.clone(),
                responsible_assignees: vec!["person".into()],
            },
        )
        .unwrap();
        let restored = AutoCommit::load(&doc.save()).unwrap();
        let state = snapshot(&restored).unwrap();
        let task = state.tasks.iter().find(|task| task.id == child).unwrap();
        assert_eq!(task.parent_task_id.as_deref(), Some(parent.as_str()));
        assert_eq!(task.labels, ["review"]);
        assert_eq!(task.responsible_assignees, ["person"]);
        assert!(task.assigned_agent.is_none());
        assert!(
            state
                .controls
                .values()
                .all(|control| control.active_run.is_none())
        );
        assert_eq!(
            state
                .tasks
                .iter()
                .find(|task| task.id == parent)
                .unwrap()
                .child_progress(&state.tasks)
                .queued,
            1
        );
    }

    #[test]
    fn parent_and_child_runs_remain_independent_when_parent_finishes() {
        let mut doc = AutoCommit::new();
        let parent = create(
            &mut doc,
            WorkspaceTaskCommand::Queue {
                title: "Parent".into(),
                assigned_agent: "worker".into(),
                route: WorkspaceTaskRoute::RulesOnly,
                resource: None,
            },
        );
        let child = create(
            &mut doc,
            WorkspaceTaskCommand::CreateSubtask {
                parent_task_id: parent.clone(),
                title: "Child".into(),
                assigned_agent: Some("worker".into()),
                route: Some(WorkspaceTaskRoute::RulesOnly),
            },
        );
        for task_id in [&parent, &child] {
            apply_command(
                &mut doc,
                WorkspaceTaskCommand::ApproveRun {
                    task_id: task_id.clone(),
                    agent_id: "worker".into(),
                    expected_run_epoch: 0,
                    after_revision: 0,
                },
            )
            .unwrap();
        }
        let actor = AdmittedTaskActor::Agent {
            id: "worker".into(),
            device: [2; 32],
        };
        let command = WorkspaceTaskCommand::Finish {
            task_id: parent.clone(),
            run_epoch: 1,
            outcome: WorkspaceTaskFinish::Done,
            summary: "Finished parent work".into(),
        };
        apply(&mut doc, uuid::Uuid::new_v4(), [3; 32], &actor, &command, 2).unwrap();
        let state = snapshot(&doc).unwrap();
        let parent_task = state.tasks.iter().find(|task| task.id == parent).unwrap();
        let child_task = state.tasks.iter().find(|task| task.id == child).unwrap();
        assert_eq!(parent_task.status, WorkspaceTaskStatus::Done);
        assert_eq!(child_task.status, WorkspaceTaskStatus::InProgress);
        assert!(state.controls[&parent].active_run.is_none());
        assert_eq!(state.controls[&child].active_run.as_ref().unwrap().epoch, 1);
        assert_eq!(parent_task.child_progress(&state.tasks).in_progress, 1);
    }

    #[test]
    fn metadata_defaults_decode_legacy_task_records() {
        let mut doc = AutoCommit::new();
        let id = create(
            &mut doc,
            WorkspaceTaskCommand::CreateManual {
                title: "Legacy".into(),
                resource: None,
            },
        );
        let state = snapshot(&doc).unwrap();
        let task = state.tasks.iter().find(|task| task.id == id).unwrap();
        let mut json = serde_json::to_value(task).unwrap();
        for key in [
            "parent_task_id",
            "child_order",
            "labels",
            "responsible_assignees",
        ] {
            json.as_object_mut().unwrap().remove(key);
        }
        let legacy: WorkspaceTask = serde_json::from_value(json).unwrap();
        assert_eq!(&legacy, task);
    }

    #[test]
    fn metadata_retries_are_idempotent_and_reused_ids_refuse_changed_data() {
        let mut doc = AutoCommit::new();
        let task_id = create(
            &mut doc,
            WorkspaceTaskCommand::CreateManual {
                title: "Review".into(),
                resource: None,
            },
        );
        let actor = AdmittedTaskActor::Person {
            id: "owner".into(),
            device: [1; 32],
        };
        let invocation = uuid::Uuid::new_v4();
        let command = WorkspaceTaskCommand::SetLabels {
            task_id: task_id.clone(),
            labels: vec!["review".into()],
        };
        let first = apply(&mut doc, invocation, [4; 32], &actor, &command, 1).unwrap();
        let retry = apply(&mut doc, invocation, [4; 32], &actor, &command, 2).unwrap();
        assert!(first.changed);
        assert!(!retry.changed);
        assert_eq!(first.result, retry.result);
        assert!(apply(&mut doc, invocation, [5; 32], &actor, &command, 3).is_err());
        let before = snapshot(&doc).unwrap();
        for labels in [
            vec!["review".into(), "review".into()],
            vec![" ".into()],
            vec!["x".repeat(129)],
        ] {
            assert!(
                apply_command(
                    &mut doc,
                    WorkspaceTaskCommand::SetLabels {
                        task_id: task_id.clone(),
                        labels
                    }
                )
                .is_err()
            );
        }
        assert_eq!(snapshot(&doc).unwrap(), before);
    }

    fn unbound_queue(doc: &mut AutoCommit) -> String {
        create(
            doc,
            WorkspaceTaskCommand::Queue {
                title: "Pending model work".into(),
                assigned_agent: String::new(),
                route: WorkspaceTaskRoute::Model,
                resource: None,
            },
        )
    }

    fn bind(task_id: &str, epoch: u64, revision: u64) -> WorkspaceTaskCommand {
        WorkspaceTaskCommand::BindExecutor {
            task_id: task_id.into(),
            agent_id: "scribe".into(),
            expected_run_epoch: epoch,
            after_revision: revision,
        }
    }

    #[test]
    fn binding_is_replay_safe_metadata_without_run_or_grants() {
        let mut doc = AutoCommit::new();
        let task_id = unbound_queue(&mut doc);
        let before = snapshot(&doc).unwrap();
        let command = bind(&task_id, 0, 0);
        let bytes = postcard::to_stdvec(&command).unwrap();
        assert_eq!(bytes[0], 26, "new commands must be appended");
        let command: WorkspaceTaskCommand = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(command.agent(), Some("scribe"));
        let digest = *blake3::hash(&bytes).as_bytes();
        let invocation = uuid::Uuid::new_v4();
        let actor = AdmittedTaskActor::Person {
            id: "owner".into(),
            device: [1; 32],
        };
        let result = apply(&mut doc, invocation, digest, &actor, &command, 2).unwrap();
        assert!(result.changed);
        assert_eq!(
            result.result,
            WorkspaceTaskMutationResult::MetadataUpdated {
                task_id: task_id.clone()
            }
        );
        let bound = snapshot(&doc).unwrap();
        assert_eq!(bound.tasks[0].assigned_agent.as_deref(), Some("scribe"));
        assert_eq!(bound.tasks[0].status, WorkspaceTaskStatus::Queued);
        assert_eq!(
            bound.tasks[0].responsible_assignees,
            before.tasks[0].responsible_assignees
        );
        assert_eq!(
            bound.tasks[0].audit.last().unwrap().action(),
            "bind-executor"
        );
        assert_eq!(bound.grants, before.grants);
        let mut control = bound.controls[&task_id].clone();
        control.receipts = before.controls[&task_id].receipts.clone();
        assert_eq!(control, before.controls[&task_id]);
        let retry = apply(&mut doc, invocation, digest, &actor, &command, 3).unwrap();
        assert!(!retry.changed);
        assert_eq!(retry.result, result.result);
        let changed = bind(&task_id, 0, 1);
        let changed_digest = *blake3::hash(&postcard::to_stdvec(&changed).unwrap()).as_bytes();
        assert!(apply(&mut doc, invocation, changed_digest, &actor, &changed, 3).is_err());
        assert!(apply_command(&mut doc, command).is_err(), "cannot rebind");
        assert_eq!(snapshot(&doc).unwrap(), bound);
        let restored = AutoCommit::load(&doc.save()).unwrap();
        assert_eq!(snapshot(&restored).unwrap(), bound);
        let started = apply(
            &mut doc,
            uuid::Uuid::new_v4(),
            [9; 32],
            &AdmittedTaskActor::Host { device: [2; 32] },
            &WorkspaceTaskCommand::StartRun {
                task_id: task_id.clone(),
                agent_id: "scribe".into(),
            },
            4,
        )
        .unwrap();
        assert_eq!(
            started.result,
            WorkspaceTaskMutationResult::RunStarted { task_id, epoch: 1 }
        );
    }

    #[test]
    fn binding_requires_person_and_exact_unbound_model_queue() {
        let mut doc = AutoCommit::new();
        let task_id = unbound_queue(&mut doc);
        let before = snapshot(&doc).unwrap();
        for actor in [
            AdmittedTaskActor::Host { device: [2; 32] },
            AdmittedTaskActor::Agent {
                id: "scribe".into(),
                device: [2; 32],
            },
        ] {
            assert!(
                apply(
                    &mut doc,
                    uuid::Uuid::new_v4(),
                    [4; 32],
                    &actor,
                    &bind(&task_id, 0, 0),
                    2
                )
                .is_err()
            );
        }
        for command in [
            bind(&task_id, 1, 0),
            bind(&task_id, 0, 1),
            bind("task:absent", 0, 0),
            WorkspaceTaskCommand::BindExecutor {
                task_id: task_id.clone(),
                agent_id: String::new(),
                expected_run_epoch: 0,
                after_revision: 0,
            },
        ] {
            assert!(apply_command(&mut doc, command).is_err());
            assert_eq!(snapshot(&doc).unwrap(), before);
        }
        let mut active = doc.clone();
        let mut controls = before.controls.clone();
        controls.get_mut(&task_id).unwrap().active_run = Some(WorkspaceTaskActiveRun {
            epoch: 1,
            agent_id: "scribe".into(),
            started_at: 1,
            applied_direction_revision: 0,
            approval: None,
        });
        project_controls(&mut active, &controls).unwrap();
        let active_before = snapshot(&active).unwrap();
        assert!(apply_command(&mut active, bind(&task_id, 0, 0)).is_err());
        assert_eq!(snapshot(&active).unwrap(), active_before);
        // Exercise invalid stored frontiers through the same production reducer.
        for (route, status, assigned) in [
            (
                WorkspaceTaskRoute::Manual,
                WorkspaceTaskStatus::Queued,
                None,
            ),
            (
                WorkspaceTaskRoute::RulesOnly,
                WorkspaceTaskStatus::Queued,
                None,
            ),
            (
                WorkspaceTaskRoute::Model,
                WorkspaceTaskStatus::Waiting,
                None,
            ),
            (WorkspaceTaskRoute::Model, WorkspaceTaskStatus::Done, None),
            (
                WorkspaceTaskRoute::Model,
                WorkspaceTaskStatus::Queued,
                Some("other".into()),
            ),
        ] {
            let mut candidate = doc.clone();
            let mut tasks = before.tasks.clone();
            tasks[0].route = route;
            tasks[0].status = status;
            tasks[0].assigned_agent = assigned;
            tilo_desktop_model::projection::project_workspace_tasks(&mut candidate, &tasks)
                .unwrap();
            let invalid = snapshot(&candidate).unwrap();
            assert!(apply_command(&mut candidate, bind(&task_id, 0, 0)).is_err());
            assert_eq!(snapshot(&candidate).unwrap(), invalid);
        }
    }

    #[test]
    fn queued_metadata_is_atomic_and_never_mints_execution_authority() {
        let mut doc = AutoCommit::new();
        let parent = create(
            &mut doc,
            WorkspaceTaskCommand::CreateManual {
                title: "Parent".into(),
                resource: None,
            },
        );
        let metadata = tilo_desktop_model::WorkspaceTaskMetadata {
            parent_task_id: Some(parent),
            child_order: Some(4),
            labels: vec!["review".into()],
            responsible_assignees: vec!["ada".into()],
        };
        let child = create(
            &mut doc,
            WorkspaceTaskCommand::QueueWithMetadata {
                title: "Task".into(),
                assigned_agent: None,
                route: WorkspaceTaskRoute::Model,
                metadata: metadata.clone(),
            },
        );
        let before = snapshot(&doc).unwrap();
        let task = before.tasks.iter().find(|task| task.id == child).unwrap();
        assert_eq!(task.parent_task_id, metadata.parent_task_id);
        assert_eq!(task.labels, metadata.labels);
        assert_eq!(task.responsible_assignees, metadata.responsible_assignees);
        let control = &before.controls[&child];
        assert!(control.active_run.is_none());
        assert!(control.execution_authority.is_none());
        assert!(control.instruction.is_none());
        assert!(before.grants.is_empty());
        let heads = doc.get_heads();
        let mut invalid = metadata.clone();
        invalid.labels = vec!["duplicate".into(), "duplicate".into()];
        for command in [
            WorkspaceTaskCommand::QueueWithMetadata {
                title: "Task".into(),
                assigned_agent: None,
                route: WorkspaceTaskRoute::Model,
                metadata: invalid.clone(),
            },
            WorkspaceTaskCommand::SetMetadata {
                task_id: child.clone(),
                metadata: invalid,
            },
            WorkspaceTaskCommand::SetMetadata {
                task_id: child,
                metadata: tilo_desktop_model::WorkspaceTaskMetadata {
                    parent_task_id: Some("foreign-task".into()),
                    ..metadata
                },
            },
        ] {
            assert!(apply_command(&mut doc, command).is_err());
            assert_eq!(snapshot(&doc).unwrap(), before);
            assert_eq!(doc.get_heads(), heads);
        }
    }

    #[test]
    fn atomic_metadata_replacement_preserves_active_control_and_instruction_frontiers() {
        let mut doc = AutoCommit::new();
        let id = create(
            &mut doc,
            WorkspaceTaskCommand::QueueInstruction {
                title: "Task".into(),
                instruction: "Private execution goal".into(),
                assigned_agent: "scribe".into(),
                route: WorkspaceTaskRoute::Model,
            },
        );
        apply_command(
            &mut doc,
            WorkspaceTaskCommand::ApproveRun {
                task_id: id.clone(),
                agent_id: "scribe".into(),
                expected_run_epoch: 0,
                after_revision: 0,
            },
        )
        .unwrap();
        let before = snapshot(&doc).unwrap();
        apply_command(
            &mut doc,
            WorkspaceTaskCommand::SetMetadata {
                task_id: id.clone(),
                metadata: tilo_desktop_model::WorkspaceTaskMetadata {
                    labels: vec!["review".into()],
                    responsible_assignees: vec!["ada".into()],
                    ..Default::default()
                },
            },
        )
        .unwrap();
        let after = snapshot(&doc).unwrap();
        let mut control = after.controls[&id].clone();
        control.receipts.clone_from(&before.controls[&id].receipts);
        assert_eq!(control, before.controls[&id]);
        assert_eq!(after.tasks[0].status, WorkspaceTaskStatus::InProgress);
        assert_eq!(after.grants, before.grants);
        let public = serde_json::to_string(&after.tasks).unwrap();
        assert!(!public.contains("Private execution goal"));
        assert_eq!(
            after.tasks[0].audit.last().unwrap().action(),
            "set-metadata"
        );
    }

    #[test]
    fn appended_metadata_commands_keep_existing_postcard_indices() {
        let settle = WorkspaceTaskCommand::SettleConfirmation {
            task_id: "task:one".into(),
            run_epoch: 1,
            confirmation_id: "confirmation".into(),
        };
        assert_eq!(postcard::to_stdvec(&settle).unwrap()[0], 19);
        let presentation = WorkspaceTaskCommand::Presentation {
            task_id: "task:one".into(),
            run_epoch: 1,
            sequence: 1,
            presentation: collab_types::task_presentation::TaskPresentation {
                run: "one".into(),
                sequence: 1,
                received_at: None,
                text: String::new(),
                reasoning: String::new(),
                finish: None,
                detail: String::new(),
            },
        };
        assert_eq!(postcard::to_stdvec(&presentation).unwrap()[0], 20);
        let metadata = WorkspaceTaskCommand::SetMetadata {
            task_id: "task:one".into(),
            metadata: WorkspaceTaskMetadata::default(),
        };
        assert_eq!(postcard::to_stdvec(&metadata).unwrap()[0], 27);
        let queue = WorkspaceTaskCommand::QueueWithMetadata {
            title: "Task".into(),
            assigned_agent: None,
            route: WorkspaceTaskRoute::Model,
            metadata: WorkspaceTaskMetadata::default(),
        };
        assert_eq!(postcard::to_stdvec(&queue).unwrap()[0], 28);
        let result = WorkspaceTaskMutationResult::MetadataUpdated {
            task_id: "task:one".into(),
        };
        assert_eq!(postcard::to_stdvec(&result).unwrap()[0], 10);
        let about = WorkspaceTaskCommand::QueueInstructionAbout {
            title: "Selected work".into(),
            instruction: "Inspect the selection".into(),
            assigned_agent: "scribe".into(),
            route: WorkspaceTaskRoute::Model,
            subject: Box::new(tilo_actions::SelectionSubject {
                app_id: "so.tilo.notes".into(),
                app_label: "Notes".into(),
                resource: tilo_actions::ActionResource {
                    id: "notes:proposal".into(),
                    kind: "document".into(),
                    label: "Proposal".into(),
                },
                anchor: tilo_actions::SelectionAnchor::of_text("opening sentence"),
            }),
        };
        assert_eq!(postcard::to_stdvec(&about).unwrap()[0], 16);
        let command = WorkspaceTaskCommand::CreateSubtask {
            parent_task_id: "task:parent".into(),
            title: "Child".into(),
            assigned_agent: None,
            route: None,
        };
        assert_eq!(postcard::to_stdvec(&command).unwrap()[0], 21);
        assert_eq!(
            postcard::to_stdvec(&WorkspaceTaskCommand::SetAssignees {
                task_id: "task:child".into(),
                responsible_assignees: vec![]
            })
            .unwrap()[0],
            24
        );
    }
}
