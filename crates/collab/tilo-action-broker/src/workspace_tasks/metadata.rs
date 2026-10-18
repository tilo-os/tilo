//! Registered task metadata operations used by native views and automation alike.
use super::{
    COLLECTION, InvocationEnvelope, MAX_CURRENT_TASKS, PublishedAction, WorkspaceTask,
    WorkspaceTaskCommand, WorkspaceTaskRoute, WorkspaceTaskSnapshot, action, argument,
    canonical_task_id, queue_private_subtask, task_frontier,
};
use tilo_actions::{ActionParam, ParamType};

pub(super) fn command(
    envelope: &InvocationEnvelope,
) -> Result<Option<WorkspaceTaskCommand>, String> {
    let Some(id) = canonical_task_id(envelope) else {
        return Ok(None);
    };
    let command = match envelope.action.as_str() {
        "bind-executor" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                agent_id: String,
                expected_run_epoch: u64,
                after_revision: u64,
            }
            let input: Input =
                serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
            WorkspaceTaskCommand::BindExecutor {
                task_id: id,
                agent_id: input.agent_id,
                expected_run_epoch: input.expected_run_epoch,
                after_revision: input.after_revision,
            }
        }
        "queue-private-subtask" => private_subtask_command(envelope, id)?,
        "create-subtask" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                title: String,
                #[serde(default)]
                assigned_agent: Option<String>,
                #[serde(default)]
                route: Option<WorkspaceTaskRoute>,
            }
            let input: Input =
                serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
            WorkspaceTaskCommand::CreateSubtask {
                parent_task_id: id,
                title: input.title,
                assigned_agent: input.assigned_agent,
                route: input.route,
            }
        }
        "move-task" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                #[serde(default)]
                parent_task_id: Option<String>,
                #[serde(default)]
                child_order: Option<i64>,
            }
            let input: Input =
                serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
            WorkspaceTaskCommand::MoveTask {
                task_id: id,
                parent_task_id: input.parent_task_id.filter(|id| !id.is_empty()),
                child_order: input.child_order,
            }
        }
        "set-labels" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                labels: String,
            }
            let input: Input =
                serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
            WorkspaceTaskCommand::SetLabels {
                task_id: id,
                labels: serde_json::from_str(&input.labels).map_err(|error| error.to_string())?,
            }
        }
        "set-assignees" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                responsible_assignees: String,
            }
            let input: Input =
                serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
            WorkspaceTaskCommand::SetAssignees {
                task_id: id,
                responsible_assignees: serde_json::from_str(&input.responsible_assignees)
                    .map_err(|error| error.to_string())?,
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(command))
}

fn private_subtask_command(
    envelope: &InvocationEnvelope,
    id: String,
) -> Result<WorkspaceTaskCommand, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        title: String,
        instruction: String,
        #[serde(default)]
        assigned_agent: String,
        route: WorkspaceTaskRoute,
    }
    let input: Input =
        serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
    if input.route != WorkspaceTaskRoute::Model {
        return Err("private child execution route must be model".into());
    }
    Ok(queue_private_subtask(
        id,
        input.title,
        input.instruction,
        input.assigned_agent,
        input.route,
    ))
}

pub(super) fn actions(task: &WorkspaceTask) -> Vec<PublishedAction> {
    let target = format!("desktop:{}", task.id);
    let mut actions = vec![
        private_subtask_action(&target, task),
        action(
            &target,
            "create-subtask",
            &task.title,
            "Create subtask",
            vec![
                ActionParam::required(
                    "title",
                    ParamType::Text,
                    "Child task title; never starts execution",
                ),
                ActionParam::optional(
                    "assigned_agent",
                    ParamType::Text,
                    "Internal execution route, not a persona or authorization grant",
                ),
                ActionParam::optional(
                    "route",
                    ParamType::Text,
                    "manual, rules_only or model; an agent route needs explicit approval before execution",
                ),
            ],
            false,
        ),
        action(
            &target,
            "move-task",
            &task.title,
            "Move task",
            vec![
                ActionParam::optional(
                    "parent_task_id",
                    ParamType::Text,
                    "Parent task ID in this workspace; empty means root",
                ),
                ActionParam::optional(
                    "child_order",
                    ParamType::Integer,
                    "Optional sibling ordering key",
                ),
            ],
            false,
        ),
        action(
            &target,
            "set-labels",
            &task.title,
            "Set labels",
            vec![ActionParam::required(
                "labels",
                ParamType::Text,
                "JSON array of distinct labels; at most 32 bounded strings",
            )],
            false,
        ),
        action(
            &target,
            "set-assignees",
            &task.title,
            "Set responsible people",
            vec![ActionParam::required(
                "responsible_assignees",
                ParamType::Text,
                "JSON array of responsible person IDs; grants no execution authority",
            )],
            false,
        ),
    ];
    if task.route == WorkspaceTaskRoute::Model
        && task.assigned_agent.is_none()
        && task.status == tilo_desktop_model::WorkspaceTaskStatus::Queued
    {
        actions.push(bind_executor_action(&target, task));
    }
    actions
}

fn private_subtask_action(target: &str, task: &WorkspaceTask) -> PublishedAction {
    action(
        target,
        "queue-private-subtask",
        &task.title,
        "Queue private subtask",
        vec![
            ActionParam::required("title", ParamType::Text, "Public child title"),
            ActionParam::required(
                "instruction",
                ParamType::Text,
                "Owner-private execution goal",
            ),
            ActionParam::optional(
                "assigned_agent",
                ParamType::Text,
                "Existing executor or empty for an unassigned private queue; grants no membership",
            ),
            ActionParam::required(
                "route",
                ParamType::Text,
                "model; queueing never approves or starts execution",
            ),
        ],
        false,
    )
}

fn bind_executor_action(target: &str, task: &WorkspaceTask) -> PublishedAction {
    action(
        target,
        "bind-executor",
        &task.title,
        "Select task executor",
        vec![
            ActionParam::required(
                "agent_id",
                ParamType::Text,
                "Authenticated configured native executor; grants no authority",
            ),
            ActionParam::required(
                "expected_run_epoch",
                ParamType::Integer,
                "Exact queued run epoch",
            ),
            ActionParam::required(
                "after_revision",
                ParamType::Integer,
                "Exact queued instruction revision",
            ),
        ],
        false,
    )
}

pub(super) fn list_action() -> PublishedAction {
    action(
        COLLECTION,
        "list-tasks",
        "Tasks",
        "List and filter tasks",
        vec![
            ActionParam::optional(
                "parent_task_id",
                ParamType::Text,
                "Exact parent ID; empty selects roots, omitted selects all",
            ),
            ActionParam::optional("label", ParamType::Text, "Required label"),
            ActionParam::optional("assignee", ParamType::Text, "Responsible person ID"),
        ],
        true,
    )
}

pub(super) fn list(
    snapshot: &WorkspaceTaskSnapshot,
    envelope: &InvocationEnvelope,
) -> Result<serde_json::Value, String> {
    #[derive(Default, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Query {
        parent_task_id: Option<String>,
        label: Option<String>,
        assignee: Option<String>,
    }
    let query: Query = if envelope.argument.is_some() {
        serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?
    } else {
        Query::default()
    };
    let mut tasks: Vec<_> =
        snapshot
            .tasks
            .iter()
            .filter(|task| {
                query.parent_task_id.as_ref().is_none_or(|parent| {
                    task.parent_task_id.as_deref().unwrap_or_default() == parent
                }) && query
                    .label
                    .as_ref()
                    .is_none_or(|label| task.labels.contains(label))
                    && query
                        .assignee
                        .as_ref()
                        .is_none_or(|assignee| task.responsible_assignees.contains(assignee))
            })
            .collect();
    tasks.sort_by(|left, right| {
        left.parent_task_id
            .cmp(&right.parent_task_id)
            .then_with(|| left.child_order.cmp(&right.child_order))
            .then_with(|| left.id.cmp(&right.id))
    });
    let total = tasks.len();
    let complete = total <= MAX_CURRENT_TASKS;
    let values: Vec<_> = tasks
        .into_iter()
        .take(MAX_CURRENT_TASKS)
        .map(|task| task_frontier(snapshot, task))
        .collect();
    Ok(serde_json::json!({"total": total, "complete": complete, "tasks": values}))
}
