//! Task coordination and owner-configured execution policy through the existing invocation service.
//! Inputs may select a planning mode and recipient; never actor, custody, status or lease.
use super::{InvocationEnvelope, PublishedAction, PublishedInput};
use tilo_desktop_model::{
    WorkspaceTask, WorkspaceTaskMutationResult, WorkspaceTaskResource, WorkspaceTaskRoute,
};
use tilo_peer_state::workspace_task::{
    WorkspaceTaskCommand, WorkspaceTaskSnapshot, validate_manual_resource,
};

/// Legacy task ceilings; a named task only narrows authority and need not have a record.
/// Native runs instead use the canonical owner's guarded `check_native_task_action`,
/// which enforces the exact run's task grants together with its frozen authority.
#[derive(Clone, Copy)]
pub(crate) enum TaskAdmission<'a> {
    Legacy(Option<&'a str>),
}

impl TaskAdmission<'_> {
    pub(crate) fn answer(
        &self,
        workspace: &tilo_desktop_model::Workspace,
        agent: Option<&str>,
        node: &tilo_action_policy::TreePath,
        kind: tilo_actions::ActionKind,
        now: u64,
    ) -> Option<tilo_action_policy::Answer> {
        let cx = tilo_desktop_model::context_at(now);
        match self {
            Self::Legacy(named) => {
                let running = workspace.tasks.iter().filter(|task| {
                    matches!(
                        task.status,
                        tilo_desktop_model::WorkspaceTaskStatus::InProgress
                            | tilo_desktop_model::WorkspaceTaskStatus::Waiting
                    ) && agent.is_some_and(|agent| task.assigned_agent.as_deref() == Some(agent))
                });
                named
                    .iter()
                    .copied()
                    .chain(running.map(|task| task.id.as_str()))
                    .filter_map(|task| workspace.policy.task_answer(task, node, kind, &cx))
                    .min()
            }
        }
    }
}

#[path = "workspace_tasks/compute.rs"]
pub(super) mod compute;

#[path = "workspace_tasks/messaging.rs"]
pub(super) mod messaging;
#[path = "workspace_tasks/metadata.rs"]
mod metadata;
#[path = "workspace_tasks/private_subtask.rs"]
mod private_subtask;
pub use private_subtask::queue_private_subtask;

const WORKSPACE: &str = "desktop:workspace";
const COLLECTION: &str = "desktop:tasks";
const TASK_PREFIX: &str = "desktop:task:";

/// Resource identity published by the real workspace task provider. It is not
/// an application window and remains available for windowless semantic tasks.
pub fn task_resource_id(task_id: &str) -> String {
    format!("desktop:{task_id}")
}

/// Resolve an existing semantic task, never invent a source for an absent id.
/// MODEL admission still requires the separate trusted person-start ceiling.
///
/// # Errors
/// The snapshot does not publish a task with this id.
pub fn model_source(
    snapshot: &WorkspaceTaskSnapshot,
    room: &str,
    task_id: &str,
) -> Result<tilo_joined_room::task_admission::model::NativeModelSource, String> {
    if !snapshot.tasks.iter().any(|task| task.id == task_id) {
        return Err("native MODEL task resource is not published".into());
    }
    Ok(
        tilo_joined_room::task_admission::model::NativeModelSource::Task {
            task_id: task_id.into(),
            resource: tilo_joined_room::NativeModelContract::task_resource(room, task_id),
        },
    )
}
const MAX_ARGUMENT_BYTES: usize = 8192;
const MAX_QUEUE_ARGUMENT_BYTES: usize =
    tilo_desktop_model::MAX_WORKSPACE_TASK_INSTRUCTION_BYTES * 6 + 4096;
const MAX_CURRENT_TASKS: usize = 256;
/// A selection's subject: a resource, up to the selected items' places and
/// a bounded excerpt.
const MAX_SUBJECT_BYTES: usize = 48 * 1024;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceInput {
    app_id: String,
    app_label: String,
    id: String,
    kind: String,
    label: String,
}

fn resource(encoded: &str) -> Result<WorkspaceTaskResource, String> {
    if encoded.len() > MAX_ARGUMENT_BYTES {
        return Err("task resource exceeds bounded input".into());
    }
    let input: ResourceInput = serde_json::from_str(encoded).map_err(|error| error.to_string())?;
    let value = WorkspaceTaskResource {
        app_id: Some(input.app_id),
        app_label: Some(input.app_label),
        id: input.id,
        kind: input.kind,
        label: input.label,
        agent_request: None,
    };
    validate_manual_resource(&value)?;
    Ok(value)
}

/// What a queued request is about, as the person's desktop sent it: bounded
/// and well formed, and never more than the request's subject.
fn subject(encoded: &str) -> Result<tilo_actions::SelectionSubject, String> {
    if encoded.len() > MAX_SUBJECT_BYTES {
        return Err("task subject exceeds bounded input".into());
    }
    let subject: tilo_actions::SelectionSubject =
        serde_json::from_str(encoded).map_err(|error| error.to_string())?;
    subject.validate()?;
    Ok(subject)
}

fn argument(envelope: &InvocationEnvelope) -> Result<&str, String> {
    let encoded = envelope
        .argument
        .as_deref()
        .ok_or("task arguments missing")?;
    let maximum = if envelope.action == "send-message" {
        tilo_actions::task_messaging::TaskMessageSend::MAX_TEXT_BYTES * 6 + 4096
    } else if (envelope.resource == COLLECTION && envelope.action == "queue")
        || (canonical_task_id(envelope).is_some() && envelope.action == "queue-private-subtask")
    {
        MAX_QUEUE_ARGUMENT_BYTES
    } else {
        MAX_ARGUMENT_BYTES
    };
    if encoded.len() > maximum {
        return Err("task arguments exceed bounded input".into());
    }
    Ok(encoded
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .unwrap_or(encoded))
}

fn queue_command(envelope: &InvocationEnvelope) -> Result<WorkspaceTaskCommand, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        title: String,
        assigned_agent: String,
        planning: String,
        #[serde(default)]
        resource_json: Option<String>,
    }
    let input: Input =
        serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
    let route = match input.planning.as_str() {
        "model" => WorkspaceTaskRoute::Model,
        "rules_only" => WorkspaceTaskRoute::RulesOnly,
        _ => return Err("managed task planning must be model or rules_only".into()),
    };
    Ok(WorkspaceTaskCommand::Queue {
        title: input.title,
        assigned_agent: input.assigned_agent,
        route,
        resource: input
            .resource_json
            .as_deref()
            .map(resource)
            .transpose()?
            .map(Box::new),
    })
}

fn queue_instruction_command(
    envelope: &InvocationEnvelope,
) -> Result<WorkspaceTaskCommand, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        title: String,
        instruction: String,
        #[serde(default)]
        assigned_agent: String,
        planning: String,
        #[serde(default)]
        subject_json: Option<String>,
    }
    let input: Input =
        serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
    let route = match input.planning.as_str() {
        "model" => WorkspaceTaskRoute::Model,
        "rules_only" => WorkspaceTaskRoute::RulesOnly,
        _ => return Err("managed task planning must be model or rules_only".into()),
    };
    Ok(match input.subject_json.as_deref() {
        Some(encoded) => WorkspaceTaskCommand::QueueInstructionAbout {
            title: input.title,
            instruction: input.instruction,
            assigned_agent: input.assigned_agent,
            route,
            subject: Box::new(subject(encoded)?),
        },
        None => WorkspaceTaskCommand::QueueInstruction {
            title: input.title,
            instruction: input.instruction,
            assigned_agent: input.assigned_agent,
            route,
        },
    })
}

fn run_command(envelope: &InvocationEnvelope) -> Result<Option<WorkspaceTaskCommand>, String> {
    if let Some(task_id) = canonical_task_id(envelope)
        && matches!(envelope.action.as_str(), "steer" | "direct")
    {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Direction {
            run_epoch: u64,
            after_revision: u64,
            text: String,
        }
        let input: Direction =
            serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
        return Ok(Some(WorkspaceTaskCommand::DirectionForRun {
            task_id,
            run_epoch: input.run_epoch,
            after_revision: input.after_revision,
            text: input.text,
        }));
    }
    if let Some(task_id) = canonical_task_id(envelope)
        && envelope.action == "stop"
    {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Stop {
            run_epoch: u64,
            after_revision: u64,
        }
        let input: Stop =
            serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
        return Ok(Some(WorkspaceTaskCommand::CancelForRun {
            task_id,
            run_epoch: input.run_epoch,
            after_revision: input.after_revision,
        }));
    }
    Ok(None)
}

pub(super) fn command(
    envelope: &InvocationEnvelope,
) -> Result<Option<WorkspaceTaskCommand>, String> {
    if let Some(command) = metadata::command(envelope)? {
        return Ok(Some(command));
    }
    if envelope.resource == COLLECTION && envelope.action == "queue" {
        return queue_instruction_command(envelope).map(Some);
    }
    if let Some(command) = run_command(envelope)? {
        return Ok(Some(command));
    }
    if let Some(id) = envelope.resource.strip_prefix(TASK_PREFIX)
        && envelope.action == "approve-run"
    {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            #[serde(default)]
            agent_id: String,
            expected_run_epoch: u64,
            after_revision: u64,
        }
        let input: Input =
            serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
        return Ok(Some(WorkspaceTaskCommand::ApproveRun {
            task_id: format!("task:{id}"),
            // An unassigned conscious Start selects a host-private actor, not
            // a roster entry. The invocation makes retry selection stable.
            agent_id: if input.agent_id.is_empty() {
                format!("native-{}", envelope.invocation.0)
            } else {
                input.agent_id
            },
            expected_run_epoch: input.expected_run_epoch,
            after_revision: input.after_revision,
        }));
    }
    if envelope.action == "answer-confirmation" {
        return answer_confirmation_command(envelope).map(Some);
    }
    if let Some(id) = envelope.resource.strip_prefix(TASK_PREFIX)
        && envelope.action == "set-execution-budget"
    {
        return execution_budget_command(envelope, id).map(Some);
    }
    if envelope.resource == WORKSPACE && envelope.action == "queue-agent-task" {
        return queue_command(envelope).map(Some);
    }
    if envelope.resource == WORKSPACE && envelope.action == "create-task" {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            title: String,
            #[serde(default)]
            resource_json: Option<String>,
        }
        let input: Input =
            serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
        Ok(Some(WorkspaceTaskCommand::CreateManual {
            title: input.title,
            resource: input
                .resource_json
                .as_deref()
                .map(resource)
                .transpose()?
                .map(Box::new),
        }))
    } else if let Some(id) = envelope.resource.strip_prefix(TASK_PREFIX)
        && envelope.action == "attach-resource"
    {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            resource_json: String,
            #[serde(default)]
            expected_resource_id: Option<String>,
        }
        let input: Input =
            serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
        if input
            .expected_resource_id
            .as_ref()
            .is_some_and(|value| value.len() > 2048 || value.chars().any(char::is_control))
        {
            return Err("invalid observed attachment id".into());
        }
        Ok(Some(WorkspaceTaskCommand::AttachResource {
            task_id: format!("task:{id}"),
            expected_resource_id: input.expected_resource_id,
            resource: Box::new(resource(&input.resource_json)?),
        }))
    } else {
        Ok(None)
    }
}

fn answer_confirmation_command(
    envelope: &InvocationEnvelope,
) -> Result<WorkspaceTaskCommand, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        run_epoch: u64,
        confirmation_id: String,
        answer: String,
        #[serde(default)]
        edits: Vec<tilo_actions::DraftEdit>,
        #[serde(default)]
        reason: Option<String>,
    }
    let task_id = canonical_task_id(envelope).ok_or("confirmation target is invalid")?;
    let input: Input =
        serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
    let answer = match input.answer.as_str() {
        "confirm" => {
            tilo_desktop_model::WorkspaceTaskConfirmationAnswer::Confirm { edits: input.edits }
        }
        "decline" if input.edits.is_empty() => {
            tilo_desktop_model::WorkspaceTaskConfirmationAnswer::Decline {
                reason: input.reason,
            }
        }
        _ => return Err("answer is confirm (with any edits) or decline".into()),
    };
    Ok(WorkspaceTaskCommand::AnswerConfirmation {
        task_id,
        run_epoch: input.run_epoch,
        confirmation_id: input.confirmation_id,
        answer,
    })
}

fn execution_budget_command(
    envelope: &InvocationEnvelope,
    id: &str,
) -> Result<WorkspaceTaskCommand, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        expected_run_epoch: u64,
        #[serde(default)]
        expected_budget: Option<collab_types::task_budget::TaskExecutionBudget>,
        #[serde(default)]
        budget: Option<collab_types::task_budget::TaskExecutionBudget>,
    }
    let input: Input =
        serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
    if let Some(budget) = input.budget {
        budget.validate()?;
    }
    if let Some(budget) = input.expected_budget {
        budget.validate()?;
    }
    Ok(WorkspaceTaskCommand::SetExecutionBudget {
        task_id: format!("task:{id}"),
        expected_run_epoch: input.expected_run_epoch,
        expected_budget: input.expected_budget,
        budget: input.budget,
    })
}

fn manual(task: &WorkspaceTask) -> bool {
    task.route == WorkspaceTaskRoute::Manual && task.assigned_agent.is_none()
}

fn task_target(envelope: &InvocationEnvelope) -> Option<&str> {
    envelope
        .resource
        .strip_prefix(TASK_PREFIX)
        .filter(|task| !task.is_empty() && task.len() <= 256)
}

fn canonical_task_id(envelope: &InvocationEnvelope) -> Option<String> {
    task_target(envelope).map(|task| format!("task:{task}"))
}

fn queue_action(workspace_label: &str) -> PublishedAction {
    use tilo_actions::{ActionParam, ParamType};
    let mut queue = action(
        WORKSPACE,
        "queue-agent-task",
        workspace_label,
        "Queue agent task",
        vec![
            ActionParam::required(
                "title",
                ParamType::Text,
                "Task title; at most 240 characters",
            ),
            ActionParam::required(
                "assigned_agent",
                ParamType::Text,
                "Exact current agent identity in this workspace",
            ),
            ActionParam::required(
                "planning",
                ParamType::Text,
                "model or rules_only; never manual",
            ),
            ActionParam::optional(
                "resource_json",
                ParamType::Text,
                "Ordinary resource JSON: app_id, app_label, id, kind, label; grants no authority",
            ),
        ],
        false,
    );
    queue.description = Some("An admitted person queues work for a current workspace agent. This does not launch the task, mint an execution lease or grant resource authority.".into());
    queue
}

pub(super) fn actions(
    snapshot: &WorkspaceTaskSnapshot,
    workspace_label: &str,
    current_agents: &std::collections::BTreeSet<String>,
) -> Vec<PublishedAction> {
    use tilo_actions::{ActionParam, ParamType};
    let mut result = vec![
        current_tasks_action(),
        compute::action(),
        queue_instruction_action(),
        create_manual_task_action(workspace_label),
    ];
    result.push(queue_action(workspace_label));
    result.push(metadata::list_action());
    for task in &snapshot.tasks {
        // The resource belongs to the workspace, not to the assigned worker.
        // Keep its read publication even when that worker is offline/removed.
        result.push(task_read_action(task));
        if task
            .assigned_agent
            .as_ref()
            .is_some_and(|agent| !current_agents.contains(agent))
        {
            continue;
        }
        let target = task_resource_id(&task.id);
        result.extend(metadata::actions(task));
        if !manual(task) {
            result.push(conversation_read_action(&target, task));
        }
        result.push(action(
            &target,
            "inspect",
            &task.title,
            "Inspect task execution policy",
            Vec::new(),
            true,
        ));
        if !task.status.is_finished() {
            result.extend(task_grant_actions(&target, &task.title));
        }
        if task.status == tilo_desktop_model::WorkspaceTaskStatus::InProgress
            && snapshot
                .controls
                .get(&task.id)
                .is_some_and(|control| control.active_run.is_some())
        {
            if task.route == WorkspaceTaskRoute::Model {
                result.push(direction_action(&target, &task.title, "steer"));
                result.push(direction_action(&target, &task.title, "direct"));
            }
            result.push(stop_action(&target, &task.title));
            result.push(messaging::send_action(&target, &task.title));
            result.push(messaging::wait_action(&target, &task.title));
            if snapshot
                .controls
                .get(&task.id)
                .and_then(|control| control.confirmation.as_ref())
                .is_some_and(tilo_desktop_model::WorkspaceTaskConfirmation::is_waiting)
            {
                result.push(answer_confirmation_action(&target, &task.title));
            }
        }
        if task.status == tilo_desktop_model::WorkspaceTaskStatus::Queued
            && !manual(task)
            && (task.assigned_agent.is_some() || task.route == WorkspaceTaskRoute::Model)
            && task
                .resource
                .as_ref()
                .is_none_or(|resource| resource.agent_request.is_none())
            && snapshot
                .controls
                .get(&task.id)
                .is_some_and(|value| value.active_run.is_none())
        {
            result.push(approve_action(&target, &task.title));
            // Enrollment may be pending while the task remains queued. Stop
            // must stay callable so its changed frontier defeats late approval.
            result.push(stop_action(&target, &task.title));
        }
        if !manual(task)
            && !task.status.is_finished()
            && snapshot
                .controls
                .get(&task.id)
                .is_some_and(|control| control.active_run.is_none())
        {
            result.push(execution_budget_action(&target, &task.title));
        }
        if manual(task)
            && !task.status.is_finished()
            && snapshot
                .controls
                .get(&task.id)
                .is_some_and(|control| control.active_run.is_none() && control.run_epoch == 0)
        {
            result.push(action(&target, "attach-resource", &task.title, "Attach resource", vec![
                ActionParam::required("resource_json", ParamType::Text, "Ordinary resource JSON: app_id, app_label, id, kind, label; grants no authority"),
                ActionParam::optional("expected_resource_id", ParamType::Text, "Exact current attachment id observed through inspect; omit only when no attachment exists"),
            ], false));
        }
    }
    result
}

fn conversation_read_action(target: &str, task: &WorkspaceTask) -> PublishedAction {
    use tilo_actions::{ActionParam, ParamType};
    action(
        target,
        "read-conversation",
        &task.title,
        "Read private task conversation",
        vec![
            ActionParam::required(
                "run_epoch",
                ParamType::Integer,
                "Exact conversation run epoch",
            ),
            ActionParam::optional(
                "after_message_sequence",
                ParamType::Integer,
                "Private inbox page cursor; reading does not acknowledge mail",
            ),
        ],
        true,
    )
}

fn execution_budget_action(target: &str, title: &str) -> PublishedAction {
    use tilo_actions::{ActionParam, ParamType};
    let mut operation = action(
        target,
        "set-execution-budget",
        title,
        "Configure bounded task execution",
        vec![
            ActionParam::required(
                "expected_run_epoch",
                ParamType::Integer,
                "Exact epoch observed through inspect",
            ),
            ActionParam::optional(
                "expected_budget",
                ParamType::Object,
                "Exact observed budget, or null when absent",
            ),
            ActionParam::optional(
                "budget",
                ParamType::Object,
                "Owner policy: turn_ms up to 900000, total_ms up to7200000, turn <= total; null restores defaults",
            ),
        ],
        false,
    );
    operation.description = Some("Only the current workspace owner can configure the next task run. This grants no action, model, check or resource authority.".into());
    operation
}

fn create_manual_task_action(workspace_label: &str) -> PublishedAction {
    use tilo_actions::{ActionParam, ParamType};
    action(
        WORKSPACE,
        "create-task",
        workspace_label,
        "Create manual task",
        vec![
            ActionParam::required(
                "title",
                ParamType::Text,
                "Manual coordination title; at most 240 characters",
            ),
            ActionParam::optional(
                "resource_json",
                ParamType::Text,
                "Ordinary resource JSON: app_id, app_label, id, kind, label; grants no authority",
            ),
        ],
        false,
    )
}

fn current_tasks_action() -> PublishedAction {
    action(
        COLLECTION,
        "read-current-tasks",
        "Tasks",
        "Read current agent tasks",
        vec![tilo_actions::ActionParam::required(
            "agent",
            tilo_actions::ParamType::Text,
            "Exact admitted workspace agent",
        )],
        true,
    )
}

fn queue_instruction_action() -> PublishedAction {
    use tilo_actions::{ActionParam, ParamType};
    action(
        COLLECTION,
        "queue",
        "Tasks",
        "Queue task",
        vec![
            ActionParam::required("title", ParamType::Text, "Bounded public task label"),
            ActionParam::required(
                "instruction",
                ParamType::Text,
                "Full private instruction; retained by the task owner",
            ),
            ActionParam::optional(
                "assigned_agent",
                ParamType::Text,
                "Configured executor, or empty to save an unassigned model task",
            ),
            ActionParam::required(
                "planning",
                ParamType::Text,
                "model or rules_only; never manual",
            ),
            ActionParam::optional(
                "subject_json",
                ParamType::Text,
                "What the request is about, as JSON: app_id, app_label, resource (id, kind, label) and anchor (place, items, excerpt) naming what the person selected; the subject only, never a scope or an authority",
            ),
        ],
        false,
    )
}

fn task_read_action(task: &WorkspaceTask) -> PublishedAction {
    action(
        &format!("desktop:{}", task.id),
        "read-task",
        &task.title,
        "Read current task",
        Vec::new(),
        true,
    )
}

fn direction_action(target: &str, title: &str, operation: &str) -> PublishedAction {
    use tilo_actions::{ActionParam, ParamType};
    action(
        target,
        operation,
        title,
        "Steer task",
        vec![
            ActionParam::required("run_epoch", ParamType::Integer, "Exact current run epoch"),
            ActionParam::required(
                "after_revision",
                ParamType::Integer,
                "Exact current direction revision",
            ),
            ActionParam::required("text", ParamType::Text, "Direction for this exact run"),
        ],
        false,
    )
}

fn stop_action(target: &str, title: &str) -> PublishedAction {
    use tilo_actions::{ActionParam, ParamType};
    action(
        target,
        "stop",
        title,
        "Stop task",
        vec![
            ActionParam::required("run_epoch", ParamType::Integer, "Exact current run epoch"),
            ActionParam::required(
                "after_revision",
                ParamType::Integer,
                "Exact current direction revision",
            ),
        ],
        false,
    )
}

/// The actions that edit an open task's grant set.
fn task_grant_actions(target: &str, title: &str) -> [PublishedAction; 2] {
    use tilo_actions::{ActionParam, ParamType};
    let mut grant = action(
        target,
        "grant",
        title,
        "Allow in this task",
        vec![
            ActionParam::required(
                "node",
                ParamType::Text,
                "Where, as a path below the room, e.g. app:so.tilo.mail",
            ),
            ActionParam::required(
                "kinds",
                ParamType::Array,
                "The kinds of action it covers: read, undoable, outside",
            ),
            ActionParam::required("answer", ParamType::Text, "allow, ask or deny"),
        ],
        false,
    );
    grant.description = Some(
        "Work done in this task gets this answer there while the task is open. A task's grants only bound its work: they give nobody more than they hold.".into(),
    );
    let mut revoke = action(
        target,
        "revoke",
        title,
        "Remove from this task's grants",
        vec![ActionParam::required(
            "grant",
            ParamType::Text,
            "The grant's id, as inspect lists it",
        )],
        false,
    );
    revoke.description = Some("Take one grant out of this task's grant set.".into());
    [grant, revoke]
}

/// The person's answer to the action a run paused at: what Task Center's
/// confirmation card does, for anyone driving the desktop without it.
fn answer_confirmation_action(target: &str, title: &str) -> PublishedAction {
    use tilo_actions::{ActionParam, ParamType};
    let mut answer = action(
        target,
        "answer-confirmation",
        title,
        "Answer the paused action",
        vec![
            ActionParam::required("run_epoch", ParamType::Integer, "Exact current run epoch"),
            ActionParam::required(
                "confirmation_id",
                ParamType::Text,
                "The paused action's confirmation id, as read-task shows it",
            ),
            ActionParam::required("answer", ParamType::Text, "confirm or decline")
                .with_choices(["confirm", "decline"]),
            ActionParam::optional(
                "edits",
                ParamType::Array,
                "Changes to the draft's fields before confirming: {path, text} each",
            ),
            ActionParam::optional("reason", ParamType::Text, "Why it is declined"),
        ],
        false,
    );
    answer.description = Some(
        "Confirm the action this task's run paused at, optionally with changed fields, or decline it with a reason. Only the person the run acts for answers.".into(),
    );
    answer
}

fn approve_action(target: &str, title: &str) -> PublishedAction {
    use tilo_actions::{ActionParam, ParamType};
    action(
        target,
        "approve-run",
        title,
        "Approve this task run",
        vec![
            ActionParam::optional(
                "agent_id",
                ParamType::Text,
                "Exact assigned agent; omit for protected first-use model setup",
            ),
            ActionParam::required(
                "expected_run_epoch",
                ParamType::Integer,
                "Observed run epoch",
            ),
            ActionParam::required(
                "after_revision",
                ParamType::Integer,
                "Observed instruction frontier",
            ),
        ],
        false,
    )
}

fn action(
    target: &str,
    operation: &str,
    label: &str,
    operation_label: &str,
    params: Vec<tilo_actions::ActionParam>,
    inspect: bool,
) -> PublishedAction {
    PublishedAction {
        semantic_identity: None,
        resource: target.into(), action: operation.into(), resource_kind: if target == WORKSPACE { "workspace-membership" } else { "workspace-task" }.into(), resource_label: label.into(), action_label: operation_label.into(),
        description: Some(if inspect { "Inspect current task coordination and execution policy; no execution result is asserted." } else if operation == "approve-run" { "Approve the observed queued task for its assigned agent. Existing workspace and action permissions still apply; approval does not assert execution or completion." } else if matches!(operation, "queue" | "steer" | "direct" | "stop") { "Change owner-backed managed task state. Exact task, run, revision, membership and caller authority are revalidated before the mutation." } else { "Manual coordination only. Attachments are shared resource references; no agent run, execution, integration or verification is asserted." }.into()),
        operation_kind: if inspect { tilo_actions::OperationKind::View } else { tilo_actions::OperationKind::Document }, params,
        contract: tilo_actions::ActionContract {
            kind: if inspect { tilo_actions::ActionKind::Read } else { tilo_actions::ActionKind::Undoable },
            result: tilo_actions::ResultSpec::Json { schema: serde_json::json!({"type":"object"}) },
            ..Default::default()
        }, view: None, input: PublishedInput::None,
    }
}

pub(super) fn is_inspect(envelope: &InvocationEnvelope) -> bool {
    (envelope.resource == COLLECTION
        && matches!(
            envelope.action.as_str(),
            "read-current-tasks" | "list-tasks" | "read-task-compute"
        ))
        || (envelope.resource.starts_with(TASK_PREFIX)
            && matches!(
                envelope.action.as_str(),
                "inspect" | "read-task" | "read-conversation"
            ))
}

pub(super) fn inspect(
    snapshot: &WorkspaceTaskSnapshot,
    envelope: &InvocationEnvelope,
) -> Result<serde_json::Value, String> {
    if envelope.resource == COLLECTION && envelope.action == "list-tasks" {
        return metadata::list(snapshot, envelope);
    }
    if envelope.resource == COLLECTION && envelope.action == "read-current-tasks" {
        return read_current_tasks(snapshot, envelope);
    }
    if envelope.action == "read-task" {
        return read_task(snapshot, envelope);
    }
    if envelope.action == "read-conversation" {
        return read_conversation(snapshot, envelope);
    }
    if envelope.argument.is_some() {
        return Err("task inspection takes no arguments".into());
    }
    let task_id = envelope
        .resource
        .strip_prefix("desktop:")
        .ok_or("invalid task resource")?;
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .ok_or("task is absent")?;
    let control = snapshot
        .controls
        .get(task_id)
        .ok_or("task control is absent")?;
    let subject = tilo_action_policy::Subject::Task(task.id.clone());
    let held: Vec<tilo_action_policy::Grant> = snapshot
        .grants
        .iter()
        .filter(|grant| grant.subject == subject)
        .cloned()
        .collect();
    let grants: Vec<serde_json::Value> = held
        .iter()
        .map(|grant| {
            serde_json::json!({
                "id": grant.id,
                "node": grant.node,
                "kinds": grant.kinds,
                "answer": grant.answer,
            })
        })
        .collect();
    let mut public_task = task.clone();
    if let Some(resource) = &mut public_task.resource {
        resource.agent_request = None;
    }
    // What was selected is the request's content, private as its
    // instruction is; where it is stays readable.
    if let Some(subject) = &mut public_task.subject {
        subject.anchor.excerpt = None;
    }
    let mut output = serde_json::json!({"task":public_task,"grants":grants,
        "grants_read":tilo_action_policy::describe(&held, &tilo_action_policy::Ids),"coordination_only":manual(task),
        "managed_route":!manual(task), "run_epoch":control.run_epoch,
        "execution_budget":control.execution_budget,
        "active_run":control.active_run.is_some(),
        "instruction_revision":control.directions.last().map_or(0, |direction| direction.revision),
        "default_turn_ms":30_000,"default_total_ms":300_000});
    if manual(task) {
        output
            .as_object_mut()
            .ok_or("invalid task inspection object")?
            .insert("managed_execution".into(), serde_json::json!(false));
    }
    Ok(output)
}

fn active_execution<'a>(
    control: &'a tilo_desktop_model::WorkspaceTaskControl,
    active: &tilo_desktop_model::WorkspaceTaskActiveRun,
) -> Option<&'a tilo_desktop_model::WorkspaceTaskExecution> {
    control
        .execution
        .as_ref()
        .filter(|execution| execution.run_epoch == active.epoch)
}

fn read_task(
    snapshot: &WorkspaceTaskSnapshot,
    envelope: &InvocationEnvelope,
) -> Result<serde_json::Value, String> {
    if envelope.argument.is_some() {
        return Err("task read accepts no arguments".into());
    }
    let task_id = canonical_task_id(envelope).ok_or("task read target is invalid")?;
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .ok_or("task is absent from the current owner projection")?;
    Ok(task_frontier(snapshot, task))
}

/// Only called behind the authenticated owner-person inspection custody gate.
/// Public frontier/list/inspect projections deliberately never call this.
fn read_conversation(
    snapshot: &WorkspaceTaskSnapshot,
    envelope: &InvocationEnvelope,
) -> Result<serde_json::Value, String> {
    let (task_id, run_epoch) = conversation_target(envelope)?;
    if snapshot
        .controls
        .get(&task_id)
        .is_none_or(|control| control.run_epoch != run_epoch)
    {
        return Err("conversation run changed".into());
    }
    private_conversation(snapshot, envelope, None)
}

/// Select exactly one private run; never accept an authority from arguments.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationQuery {
    run_epoch: u64,
    #[serde(default)]
    after_message_sequence: u64,
}
fn conversation_query(envelope: &InvocationEnvelope) -> Result<ConversationQuery, String> {
    serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())
}
pub(super) fn conversation_target(envelope: &InvocationEnvelope) -> Result<(String, u64), String> {
    Ok((
        canonical_task_id(envelope).ok_or("conversation target is invalid")?,
        conversation_query(envelope)?.run_epoch,
    ))
}
pub(super) fn conversation_message_cursor(envelope: &InvocationEnvelope) -> Result<u64, String> {
    Ok(conversation_query(envelope)?.after_message_sequence)
}

pub(super) fn private_conversation(
    snapshot: &WorkspaceTaskSnapshot,
    envelope: &InvocationEnvelope,
    private: Option<&crate::services::NativeTaskPrivateInstructions>,
) -> Result<serde_json::Value, String> {
    let (task_id, run_epoch) = conversation_target(envelope)?;
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .ok_or("task is absent")?;
    if manual(task) {
        return Err("manual task has no private agent conversation".into());
    }
    let control = snapshot
        .controls
        .get(&task_id)
        .ok_or("task control is absent")?;
    if run_epoch > control.run_epoch {
        return Err("conversation run has not existed".into());
    }
    let mut turns = Vec::new();
    if let Some(instruction) = private.and_then(|text| text.instruction.as_ref()) {
        turns.push(serde_json::json!({
            "task_id": task.id, "run_epoch": 0, "revision": 0,
            "text": instruction, "author": task.created_by, "received_at": task.created_at,
        }));
    }
    // Directions are task-level revisions. For a replaced run, the ordered
    // audit is the durable boundary: never relabel a later run's directions
    // as turns in the selected historical conversation.
    let next_run = run_epoch
        .checked_add(1)
        .and_then(|epoch| tilo_actions::Scalar::of(&epoch));
    let historical_events = task
        .audit
        .iter()
        .take_while(|event| {
            !matches!(&event.what, tilo_actions::Did::Verb(verb)
            if matches!(event.action(), "start-run" | "approve-run")
                && verb.args.get("run") == next_run.as_ref())
        })
        .collect::<Vec<_>>();
    for direction in &control.directions {
        if run_epoch != control.run_epoch
            && direction.applied.as_ref().is_none_or(|ack| ack.run_epoch != run_epoch)
            && !historical_events.iter().any(|event| {
                matches!(&event.what, tilo_actions::Did::Verb(verb)
                    if event.action() == "add-direction"
                        && verb.args.get("revision") == tilo_actions::Scalar::of(&direction.revision).as_ref())
            })
        {
            continue;
        }
        let text = if direction.text.starts_with("private-instruction:") {
            let Some(private) = private else {
                continue;
            };
            let text = private
                .directions
                .get(&direction.revision)
                .ok_or("private task direction unavailable")?;
            if tilo_peer_state::workspace_task::private_instruction_reference(text)
                != direction.text
            {
                return Err("private task direction binding changed".into());
            }
            text
        } else {
            &direction.text
        };
        turns.push(serde_json::json!({
            "task_id": task.id, "run_epoch": run_epoch, "revision": direction.revision,
            "text": text, "author": direction.author, "received_at": direction.created_at,
            "applied": direction.applied.as_ref().filter(|ack| ack.run_epoch == run_epoch),
        }));
    }
    append_public_conversation_turns(task, run_epoch, &mut turns);
    if control.run_epoch == run_epoch {
        for presentation in control.presentations.values() {
            turns.push(serde_json::json!({
                "task_id": task.id, "run_epoch": run_epoch,
                "revision": presentation.sequence,
                "id": format!("{}:{}:{}", task.id, run_epoch, presentation.run),
                "kind": "agent-output", "text": presentation.text,
                "presentation": presentation,
                "received_at": presentation.received_at.unwrap_or(task.updated_at),
                "author": format!("agent:{}", task.assigned_agent.as_deref().unwrap_or_default()),
            }));
        }
    }

    // The selected view belongs to the requested run, not the Agent's latest
    // assignment. Keep the live frontier separate for mutation admission.
    let mut frontier = task_frontier(snapshot, task);
    let fields = frontier
        .as_object_mut()
        .ok_or("task frontier is not an object")?;
    fields.insert("run_epoch".into(), serde_json::json!(run_epoch));
    if control.run_epoch != run_epoch
        || (control.active_run.is_none()
            && task.status != tilo_desktop_model::WorkspaceTaskStatus::Queued)
    {
        fields.insert("active_agent".into(), serde_json::Value::Null);
        fields.insert("execution".into(), serde_json::Value::Null);
        fields.insert("status".into(), serde_json::json!("ended"));
    }
    Ok(
        serde_json::json!({"task_id": task.id, "run_epoch": run_epoch,
        "frontier": frontier, "turns": turns}),
    )
}

fn append_public_conversation_turns(
    task: &WorkspaceTask,
    run_epoch: u64,
    turns: &mut Vec<serde_json::Value>,
) {
    for (ordinal, event) in task
        .audit
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            event.for_run(run_epoch)
                && matches!(event.action(), "public-progress" | "finish" | "abandon")
        })
        .take(tilo_desktop_model::task::MAX_WORKSPACE_TASK_PUBLIC_UPDATES + 1)
    {
        if let Some(text) = event
            .note
            .as_ref()
            .filter(|text| text.len() <= tilo_desktop_model::MAX_WORKSPACE_TASK_SUMMARY_BYTES)
        {
            turns.push(serde_json::json!({
                "task_id": task.id, "run_epoch": run_epoch, "revision": ordinal,
                "kind": "agent-output", "text": text,
                "author": event.actor,
                "received_at": event.at,
            }));
        }
    }
}

fn read_current_tasks(
    snapshot: &WorkspaceTaskSnapshot,
    envelope: &InvocationEnvelope,
) -> Result<serde_json::Value, String> {
    let agent = inspection_agent_query(envelope)?;
    let mut tasks = snapshot
        .tasks
        .iter()
        .filter(|task| {
            let active = snapshot
                .controls
                .get(&task.id)
                .and_then(|control| control.active_run.as_ref());
            task.status == tilo_desktop_model::WorkspaceTaskStatus::InProgress
                && task.assigned_agent.as_deref() == Some(agent.as_str())
                && active.is_some_and(|run| run.agent_id == agent)
        })
        .map(|task| task_frontier(snapshot, task))
        .collect::<Vec<_>>();
    tasks.sort_by(|left, right| left["task_id"].as_str().cmp(&right["task_id"].as_str()));
    let total = u64::try_from(tasks.len()).map_err(|_| "current task count exceeds u64")?;
    let complete = tasks.len() <= MAX_CURRENT_TASKS;
    tasks.truncate(MAX_CURRENT_TASKS);
    Ok(serde_json::json!({
        "complete": complete,
        "total": total,
        "tasks": tasks,
    }))
}

fn inspection_agent_query(envelope: &InvocationEnvelope) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Query {
        agent: String,
    }
    let query: Query =
        serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
    if query.agent.is_empty()
        || query.agent.len() > 128
        || query.agent.chars().any(char::is_control)
    {
        return Err("current task query agent is invalid".into());
    }
    Ok(query.agent)
}

pub(super) fn inspection_agent(
    snapshot: &WorkspaceTaskSnapshot,
    envelope: &InvocationEnvelope,
) -> Result<Option<String>, String> {
    if envelope.resource == COLLECTION && envelope.action == "read-current-tasks" {
        return inspection_agent_query(envelope).map(Some);
    }
    if matches!(envelope.action.as_str(), "read-task" | "read-conversation") {
        let task_id = canonical_task_id(envelope).ok_or("task read target is invalid")?;
        return snapshot
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .ok_or_else(|| "task is absent from the current owner projection".into())
            .map(|task| task.assigned_agent.clone());
    }
    Ok(None)
}

fn task_frontier(snapshot: &WorkspaceTaskSnapshot, task: &WorkspaceTask) -> serde_json::Value {
    let control = snapshot.controls.get(&task.id);
    let active = control.and_then(|control| control.active_run.as_ref());
    let execution =
        control.and_then(|control| active.and_then(|active| active_execution(control, active)));
    serde_json::json!({
        "task_id": task.id,
        "title": task.title,
        "assigned_agent": task.assigned_agent,
        "parent_task_id": task.parent_task_id,
        "child_order": task.child_order,
        "labels": task.labels,
        "responsible_assignees": task.responsible_assignees,
        "child_progress": task.child_progress(&snapshot.tasks),
        "route": task.route,
        "status": task.status,
        "run_epoch": control.map(|control| control.run_epoch),
        "active_agent": active.map(|run| &run.agent_id),
        "after_revision": control
            .and_then(|control| control.directions.last())
            .map_or(0, |direction| direction.revision),
        "applied_revision": active.map(|run| run.applied_direction_revision),
        "execution": execution,
        // The action the run paused at, by its label: the draft itself stays
        // with the task's owner.
        "confirmation": control
            .and_then(|control| control.confirmation.as_ref())
            .map(|held| serde_json::json!({
                "confirmation_id": held.id,
                "action": held.draft.action_label,
                "waiting": held.is_waiting(),
            })),
    })
}

fn stop_result(
    snapshot: &WorkspaceTaskSnapshot,
    task_id: &str,
    result: &WorkspaceTaskMutationResult,
    generation: u64,
) -> Result<serde_json::Value, String> {
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .ok_or("owner stop receipt has no canonical task")?;
    if task.status != tilo_desktop_model::WorkspaceTaskStatus::Cancelled
        || snapshot
            .controls
            .get(task_id)
            .is_some_and(|control| control.active_run.is_some())
    {
        return Err("owner stop receipt did not cancel the exact task".into());
    }
    Ok(serde_json::json!({
        "generation": generation,
        "result": result,
        "resource": format!("desktop:{task_id}"),
        "task": task,
        "cancelled": true,
    }))
}

/// The answer a person gave to one task's confirmation request, read back
/// against the canonical task it was recorded for.
fn confirmation_result(
    snapshot: &WorkspaceTaskSnapshot,
    task_id: &str,
    result: &WorkspaceTaskMutationResult,
    generation: u64,
) -> Result<serde_json::Value, String> {
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .ok_or("owner confirmation receipt has no canonical task")?;
    Ok(serde_json::json!({
        "generation": generation,
        "result": result,
        "resource": format!("desktop:{task_id}"),
        "task": task,
        "answered": true,
    }))
}

/// A saved steer or direct, with whether the active run has already applied it.
fn direction_result(
    snapshot: &WorkspaceTaskSnapshot,
    task_id: &str,
    result: &WorkspaceTaskMutationResult,
    revision: u64,
    generation: u64,
) -> Result<serde_json::Value, String> {
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .ok_or("owner direction receipt has no canonical task")?;
    let control = snapshot
        .controls
        .get(task_id)
        .ok_or("owner direction receipt has no task control")?;
    let run_epoch = control
        .active_run
        .as_ref()
        .map(|run| run.epoch)
        .ok_or("owner direction receipt has no active run")?;
    Ok(serde_json::json!({
        "generation": generation,
        "result": result,
        "resource": format!("desktop:{task_id}"),
        "task": task,
        "run_epoch": run_epoch,
        "revision": revision,
        "saved": true,
        "applied": control.active_run.as_ref().is_some_and(|run| {
            run.epoch == run_epoch && run.applied_direction_revision >= revision
        }),
    }))
}

pub(super) fn result(
    snapshot: &WorkspaceTaskSnapshot,
    envelope: &InvocationEnvelope,
    device: [u8; 32],
    generation: u64,
) -> Result<serde_json::Value, String> {
    let result = snapshot
        .controls
        .values()
        .flat_map(|control| &control.receipts)
        .find(|receipt| receipt.device == device && receipt.invocation == envelope.invocation.0)
        .ok_or("owner did not retain the exact task command receipt")?;
    let (task_id, budget_change) = match &result.result {
        WorkspaceTaskMutationResult::Queued { task_id }
        | WorkspaceTaskMutationResult::ResourceAttached { task_id }
        | WorkspaceTaskMutationResult::RunStarted { task_id, .. }
        | WorkspaceTaskMutationResult::MetadataUpdated { task_id } => (task_id, false),
        WorkspaceTaskMutationResult::ExecutionBudgetSet { task_id } => (task_id, true),
        WorkspaceTaskMutationResult::ConfirmationRecorded { task_id }
            if envelope.action == "answer-confirmation"
                && task_target(envelope) == task_id.strip_prefix("task:") =>
        {
            return confirmation_result(snapshot, task_id, &result.result, generation);
        }
        WorkspaceTaskMutationResult::DirectionAdded { task_id, revision }
            if matches!(envelope.action.as_str(), "steer" | "direct")
                && task_target(envelope) == task_id.strip_prefix("task:") =>
        {
            return direction_result(snapshot, task_id, &result.result, *revision, generation);
        }
        WorkspaceTaskMutationResult::Finished { task_id }
            if envelope.action == "stop"
                && task_target(envelope) == task_id.strip_prefix("task:") =>
        {
            return stop_result(snapshot, task_id, &result.result, generation);
        }
        _ => return Err("task command returned an unexpected managed execution result".into()),
    };
    let task = snapshot
        .tasks
        .iter()
        .find(|task| &task.id == task_id)
        .ok_or("owner task receipt has no canonical task")?;
    let queued_agent = envelope.resource == WORKSPACE && envelope.action == "queue-agent-task";
    let queued_instruction = envelope.resource == COLLECTION && envelope.action == "queue";
    let approved = envelope.action == "approve-run";
    let metadata_change = matches!(
        result.result,
        WorkspaceTaskMutationResult::MetadataUpdated { .. }
    ) && matches!(
        envelope.action.as_str(),
        "move-task" | "set-labels" | "set-assignees" | "bind-executor"
    ) && envelope.resource == format!("desktop:{task_id}");
    let created_child = matches!(
        envelope.action.as_str(),
        "create-subtask" | "queue-private-subtask"
    ) && matches!(result.result, WorkspaceTaskMutationResult::Queued { .. })
        && task
            .parent_task_id
            .as_ref()
            .is_some_and(|parent| envelope.resource == format!("desktop:{parent}"));
    if (!approved
        && !budget_change
        && !queued_agent
        && !queued_instruction
        && !metadata_change
        && !created_child
        && !manual(task))
        || (queued_agent
            && (manual(task)
                || !matches!(result.result, WorkspaceTaskMutationResult::Queued { .. })))
        || (queued_instruction
            && (manual(task)
                || !matches!(result.result, WorkspaceTaskMutationResult::Queued { .. })))
    {
        return Err("task command receipt disagrees with its declared coordination route".into());
    }
    if approved
        && (envelope.resource != format!("desktop:{task_id}")
            || !matches!(
                result.result,
                WorkspaceTaskMutationResult::RunStarted { .. }
            ))
    {
        return Err("approval receipt has another task or command".into());
    }
    Ok(
        serde_json::json!({"generation":generation,"result":result.result,
        "resource":format!("desktop:{task_id}"),"task":task,
        "coordination_only":manual(task),"managed_execution":false,
        "managed_route":!manual(task),"run_approved":approved,"execution_started":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(arguments: &serde_json::Value) -> InvocationEnvelope {
        InvocationEnvelope {
            id: 1,
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(9)),
            resource: WORKSPACE.into(),
            action: "create-task".into(),
            argument: Some(format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                arguments
            )),
            actor: "supplied-actor-is-not-authority".into(),
            context: None,
        }
    }

    #[test]
    fn the_paused_action_is_answered_through_a_published_action() {
        let mut request = envelope(&serde_json::json!({
            "run_epoch": 3,
            "confirmation_id": "c1",
            "answer": "confirm",
            "edits": [{"path": ["message", "to"], "text": "carol@example.org"}],
        }));
        request.resource = "desktop:task:abc".into();
        request.action = "answer-confirmation".into();
        let Some(WorkspaceTaskCommand::AnswerConfirmation {
            task_id,
            run_epoch,
            confirmation_id,
            answer: tilo_desktop_model::WorkspaceTaskConfirmationAnswer::Confirm { edits },
        }) = command(&request).unwrap()
        else {
            panic!("an answer command");
        };
        assert_eq!(
            (task_id.as_str(), run_epoch, confirmation_id.as_str()),
            ("task:abc", 3, "c1")
        );
        assert_eq!(edits[0].text, "carol@example.org");

        let mut decline = envelope(&serde_json::json!({
            "run_epoch": 3, "confirmation_id": "c1", "answer": "decline", "reason": "Not now",
        }));
        decline.resource = "desktop:task:abc".into();
        decline.action = "answer-confirmation".into();
        assert!(matches!(
            command(&decline).unwrap(),
            Some(WorkspaceTaskCommand::AnswerConfirmation {
                answer: tilo_desktop_model::WorkspaceTaskConfirmationAnswer::Decline {
                    reason: Some(_)
                },
                ..
            })
        ));
        let mut edited_decline = envelope(&serde_json::json!({
            "run_epoch": 3, "confirmation_id": "c1", "answer": "decline",
            "edits": [{"path": ["x"], "text": "y"}],
        }));
        edited_decline.resource = "desktop:task:abc".into();
        edited_decline.action = "answer-confirmation".into();
        assert!(
            command(&edited_decline).is_err(),
            "a decline changes nothing"
        );
    }

    #[test]
    fn conversation_projects_only_exact_run_public_notes_and_real_outcome() {
        let event = |epoch: u64, action: &str, text: &str| {
            tilo_desktop_model::WorkspaceTaskAuditEvent::new(
                2,
                "agent:scribe",
                action,
                [("run_epoch", tilo_actions::Scalar::text(&epoch.to_string()))],
            )
            .noting(text)
        };
        let snapshot = WorkspaceTaskSnapshot {
            tasks: vec![WorkspaceTask {
                parent_task_id: None,
                child_order: None,
                labels: Vec::new(),
                responsible_assignees: Vec::new(),
                id: "task:output".into(),
                title: "Update".into(),
                created_by: "person".into(),
                assigned_agent: Some("scribe".into()),
                route: WorkspaceTaskRoute::Model,
                status: tilo_desktop_model::WorkspaceTaskStatus::Done,
                resource: None,
                subject: None,
                created_at: 1,
                updated_at: 3,
                audit: vec![
                    event(1, "public-progress", "Old run"),
                    event(2, "public-progress", "Appended the paragraph."),
                    event(2, "finish", "The document is updated."),
                ],
            }],
            controls: std::collections::BTreeMap::from([(
                "task:output".into(),
                tilo_desktop_model::WorkspaceTaskControl {
                    run_epoch: 2,
                    instruction: Some("Private full instruction".into()),
                    ..Default::default()
                },
            )]),
            grants: Vec::new(),
        };
        let mut request = envelope(&serde_json::json!({"run_epoch":2}));
        request.resource = "desktop:task:output".into();
        request.action = "read-conversation".into();
        assert_eq!(
            inspection_agent(&snapshot, &request).unwrap().as_deref(),
            Some("scribe")
        );
        let private = crate::services::NativeTaskPrivateInstructions {
            instruction: Some("Private full instruction".into()),
            ..Default::default()
        };
        let read = private_conversation(&snapshot, &request, Some(&private)).unwrap();
        assert!(
            !inspect(&snapshot, &request)
                .unwrap()
                .to_string()
                .contains("Private full instruction")
        );
        assert_eq!(read["turns"].as_array().unwrap().len(), 3);
        assert_eq!(read["turns"][1]["text"], "Appended the paragraph.");
        assert_eq!(read["turns"][2]["text"], "The document is updated.");
        request.argument = envelope(&serde_json::json!({"run_epoch":1})).argument;
        assert!(inspect(&snapshot, &request).is_err());
        let historical = private_conversation(&snapshot, &request, Some(&private)).unwrap();
        assert_eq!(historical["turns"][1]["text"], "Old run");
        assert_eq!(historical["turns"].as_array().unwrap().len(), 2);
        assert_eq!(historical["frontier"]["status"], "ended");
        request.action = "inspect".into();
        request.argument = None;
        assert!(
            !inspect(&snapshot, &request)
                .unwrap()
                .to_string()
                .contains("Private full instruction")
        );
    }

    fn enrolled_queue_owner() -> (
        tempfile::TempDir,
        tilo_joined_room::NativeWorkspaceOwner,
        tilo_user_identity::DeviceCert,
        ed25519_dalek::SigningKey,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let owner = tilo_joined_room::open_file(&directory.path().join("native.json")).unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[92; 32]);
        let certificate = tilo_joined_room::testing::certificate(
            &ed25519_dalek::SigningKey::from_bytes(&[91; 32]),
            &key,
            "Governor",
        );
        let _governor =
            tilo_joined_room::testing::enroll_interactive_governor(&owner, &key, &certificate)
                .unwrap();
        (directory, owner, certificate, key)
    }

    fn assert_non_person_queue_refused(
        owner: &tilo_joined_room::NativeWorkspaceOwner,
        request: &tilo_owner_commit::OwnerCommitRequest,
        certificate: &tilo_user_identity::DeviceCert,
        snapshot: &WorkspaceTaskSnapshot,
        context: (tilo_desktop_model::RoomId, std::time::SystemTime),
    ) {
        use tilo_owner_commit::CommitOperation;
        use tilo_peer_state::workspace_task::WorkspaceTaskAuthority;
        let (room, now) = context;
        for authority in [
            WorkspaceTaskAuthority::Agent {
                attestor: certificate.clone(),
                agent_id: "scribe".into(),
            },
            WorkspaceTaskAuthority::Host {
                attestor: certificate.clone(),
            },
        ] {
            let mut impersonated = request.clone();
            let CommitOperation::WorkspaceTaskCommand {
                invocation,
                authority: current,
                ..
            } = &mut impersonated.operation
            else {
                panic!("expected task command");
            };
            *invocation = uuid::Uuid::new_v4();
            *current = authority;
            assert!(
                owner
                    .commit_workspace_task_command(impersonated, now)
                    .is_err()
            );
            assert_eq!(owner.workspace_task_snapshot(room).unwrap(), *snapshot);
        }
    }

    #[test]
    fn public_queue_commits_an_admitted_agent_and_retains_a_receipt_without_a_run() {
        use tilo_action_policy::roles::AgentPreset;
        use tilo_owner_commit::{CommitOperation, OwnerCommitRequest};
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
        use tilo_peer_state::workspace_task::WorkspaceTaskAuthority;
        let (_directory, owner, certificate, key) = enrolled_queue_owner();
        let now = std::time::SystemTime::now();
        let room = tilo_desktop_model::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
        let membership = |command| OwnerCommitRequest {
            room: room.to_string(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: certificate.clone(),
                command,
            },
        };
        let enrolled = membership(WorkspaceMembershipCommand::AddAgent {
            agent: "scribe".into(),
            preset: AgentPreset::Contributor,
        });
        owner
            .commit_workspace_membership_command(enrolled.clone(), &certificate, &key, now)
            .unwrap()
            .validate_for(&enrolled)
            .unwrap();
        let before = owner.workspace_task_snapshot(room).unwrap();
        let published = actions(
            &before,
            "Workspace",
            &std::collections::BTreeSet::from(["scribe".into()]),
        )
        .into_iter()
        .find(|row| row.action == "queue-agent-task")
        .unwrap();
        assert_eq!(published.contract.kind, tilo_actions::ActionKind::Undoable);
        let mut public = envelope(
            &serde_json::json!({"title":"Inspect chip proof", "assigned_agent":"scribe", "planning":"model"}),
        );
        public.action = published.action;
        public.resource = published.resource;
        let request = OwnerCommitRequest {
            room: room.to_string(),
            operation: CommitOperation::WorkspaceTaskCommand {
                invocation: public.invocation.0,
                authority: WorkspaceTaskAuthority::Person {
                    caller: certificate.clone(),
                },
                command: command(&public).unwrap().unwrap(),
            },
        };
        let receipt = owner
            .commit_workspace_task_command(request.clone(), now)
            .unwrap();
        receipt.validate_for(&request).unwrap();
        let snapshot = owner.workspace_task_snapshot(room).unwrap();
        let output = result(
            &snapshot,
            &public,
            certificate.device_public_key,
            receipt.generation,
        )
        .unwrap();
        assert_eq!(output["task"]["assigned_agent"], "scribe");
        assert_eq!(output["task"]["status"], "queued");
        assert_eq!(output["managed_route"], true);
        assert_eq!(output["managed_execution"], false);
        assert_eq!(output["execution_started"], false);
        assert!(
            snapshot
                .controls
                .values()
                .all(|control| control.active_run.is_none() && control.run_epoch == 0)
        );
        owner
            .commit_workspace_task_command(request.clone(), now)
            .unwrap()
            .validate_for(&request)
            .unwrap();
        assert_eq!(owner.workspace_task_snapshot(room).unwrap(), snapshot);
        assert_non_person_queue_refused(&owner, &request, &certificate, &snapshot, (room, now));
        let removed = membership(WorkspaceMembershipCommand::RemoveAgent {
            agent: "scribe".into(),
        });
        owner
            .commit_workspace_membership_command(removed.clone(), &certificate, &key, now)
            .unwrap()
            .validate_for(&removed)
            .unwrap();
        let mut stale = request;
        let CommitOperation::WorkspaceTaskCommand { invocation, .. } = &mut stale.operation else {
            panic!("expected task command");
        };
        *invocation = uuid::Uuid::new_v4();
        assert!(owner.commit_workspace_task_command(stale, now).is_err());
        assert_eq!(owner.workspace_task_snapshot(room).unwrap(), snapshot);
    }

    #[test]
    fn managed_queue_arguments_cannot_choose_authority_or_run_state() {
        for field in [
            "actor",
            "caller",
            "certificate",
            "room",
            "authority",
            "status",
            "run_epoch",
            "lease",
        ] {
            let mut input = serde_json::json!({"title":"Review RTL", "assigned_agent":"scribe", "planning":"model"});
            input[field] = serde_json::json!("forged");
            let mut public = envelope(&input);
            public.action = "queue-agent-task".into();
            assert!(command(&public).is_err(), "{field}");
        }
        for planning in ["manual", "Model", "", "host"] {
            let mut public = envelope(
                &serde_json::json!({"title":"Review RTL", "assigned_agent":"scribe", "planning":planning}),
            );
            public.action = "queue-agent-task".into();
            assert!(command(&public).is_err());
        }
    }

    #[test]
    fn budget_public_arguments_cannot_select_authority_or_unbounded_deadlines() {
        let mut request = envelope(&serde_json::json!({"expected_run_epoch":0,
            "budget":{"turn_ms":900_000,"total_ms":7_200_000}}));
        request.resource = "desktop:task:one".into();
        request.action = "set-execution-budget".into();
        assert!(
            matches!(command(&request).unwrap(), Some(WorkspaceTaskCommand::SetExecutionBudget { task_id, .. }) if task_id == "task:one")
        );
        for input in [
            serde_json::json!({"expected_run_epoch":0,"actor":"owner"}),
            serde_json::json!({"expected_run_epoch":0,"budget":{"turn_ms":900_001,"total_ms":7_200_000}}),
            serde_json::json!({"expected_run_epoch":false}),
            serde_json::json!({"expected_run_epoch":0,"budget":{"turn_ms":100,"total_ms":99}}),
        ] {
            request.argument = Some(format!("{}{}", tilo_actions::ARGUMENTS_JSON_PREFIX, input));
            assert!(command(&request).is_err());
        }
    }

    #[test]
    fn manual_public_contract_refuses_execution_authority_and_instruction_carriers() {
        assert!(matches!(
            command(&envelope(&serde_json::json!({"title":"Review RTL"}))).unwrap(),
            Some(WorkspaceTaskCommand::CreateManual { resource: None, .. })
        ));
        for field in [
            "actor",
            "caller",
            "certificate",
            "room",
            "route",
            "assigned_agent",
            "status",
            "run_epoch",
        ] {
            let mut input = serde_json::json!({"title":"Review RTL"});
            input[field] = serde_json::json!("forged");
            assert!(command(&envelope(&input)).is_err(), "{field}");
        }
        let mut reference = serde_json::json!({"app_id":"so.tilo.chip-design","app_label":"Chip Design","id":"chip_design.verification:hash","kind":"verification","label":"Report"});
        reference["agent_request"] = serde_json::json!({"instruction":"run something"});
        assert!(
            command(&envelope(
                &serde_json::json!({"title":"Review RTL", "resource_json":reference.to_string()})
            ))
            .is_err()
        );
        reference.as_object_mut().unwrap().remove("agent_request");
        for id in ["file:///device/private", "FILE:///device/private"] {
            reference["id"] = serde_json::json!(id);
            assert!(
                command(&envelope(&serde_json::json!({
                    "title":"Review RTL", "resource_json":reference.to_string()
                })))
                .is_err()
            );
        }
        assert!(command(&envelope(&serde_json::json!({"title":"x".repeat(8192)}))).is_err());
    }

    fn catalogue_task() -> WorkspaceTask {
        WorkspaceTask {
            parent_task_id: None,
            child_order: None,
            labels: Vec::new(),
            responsible_assignees: Vec::new(),
            id: "task:00000000-0000-4000-8000-000000000001".into(),
            title: "Review report".into(),
            created_by: "person".into(),
            assigned_agent: None,
            route: WorkspaceTaskRoute::Manual,
            status: tilo_desktop_model::WorkspaceTaskStatus::Queued,
            resource: None,
            subject: None,
            created_at: 1,
            updated_at: 1,
            audit: Vec::new(),
        }
    }

    /// The exact actions a manual task publishes: the original nine and all
    /// seven metadata/private-task additions. Count and exact names also
    /// reject duplicate publications.
    fn manual_catalogue_actions() -> std::collections::BTreeSet<&'static str> {
        std::collections::BTreeSet::from([
            "read-current-tasks",
            "read-task-compute",
            "queue",
            "create-task",
            "queue-agent-task",
            "read-task",
            "inspect",
            "grant",
            "revoke",
            "attach-resource",
            "list-tasks",
            "queue-private-subtask",
            "create-subtask",
            "move-task",
            "set-labels",
            "set-assignees",
        ])
    }

    #[test]
    fn task_catalog_exposes_managed_policy_without_execution_authority() {
        let mut document = automerge::AutoCommit::new();
        // Project the real typed production document shape, then read through
        // the same canonical snapshot function used by the installed service.
        let mut task = catalogue_task();
        tilo_desktop_model::projection::project_workspace_tasks(&mut document, &[task.clone()])
            .unwrap();
        let mut snapshot = tilo_peer_state::workspace_task::snapshot(&document).unwrap();
        snapshot.controls.insert(
            task.id.clone(),
            tilo_desktop_model::WorkspaceTaskControl::default(),
        );
        let catalogue = actions(&snapshot, "Workspace", &std::collections::BTreeSet::new());
        let expected = manual_catalogue_actions();
        assert_eq!(catalogue.len(), expected.len());
        assert_eq!(
            catalogue
                .iter()
                .map(|row| row.action.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            expected,
        );
        assert!(
            ["grant", "revoke"]
                .iter()
                .all(|id| catalogue.iter().any(|row| row.action == *id))
        );
        let create = catalogue
            .iter()
            .find(|row| row.action == "create-task")
            .unwrap();
        assert_eq!(create.resource_label, "Workspace");
        assert_eq!(create.resource_kind, "workspace-membership");
        let inspect_action = catalogue
            .iter()
            .find(|row| row.action == "inspect")
            .unwrap();
        assert_eq!(inspect_action.contract.kind, tilo_actions::ActionKind::Read);
        let mut request = envelope(&serde_json::json!({}));
        request.resource = format!("desktop:{}", task.id);
        request.action = "inspect".into();
        request.argument = None;
        assert_eq!(inspect(&snapshot, &request).unwrap()["task"]["id"], task.id);
        task.assigned_agent = Some("scribe".into());
        task.route = WorkspaceTaskRoute::Model;
        snapshot.tasks = vec![task];
        let managed = actions(
            &snapshot,
            "Workspace",
            &std::collections::BTreeSet::from(["scribe".into()]),
        );
        let mut managed_expected = expected;
        assert!(managed_expected.remove("attach-resource"));
        managed_expected.extend([
            "read-conversation",
            "approve-run",
            "stop",
            "set-execution-budget",
        ]);
        assert_eq!(managed.len(), managed_expected.len());
        assert_eq!(
            managed
                .iter()
                .map(|row| row.action.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            managed_expected,
        );
        assert!(managed.iter().any(|row| row.action == "approve-run"));
        assert!(
            managed
                .iter()
                .any(|row| row.action == "set-execution-budget")
        );
        assert!(!managed.iter().any(|row| row.action == "attach-resource"));
        let observed = inspect(&snapshot, &request).unwrap();
        assert_eq!(observed["managed_route"], true);
        assert!(observed.get("managed_execution").is_none());
        assert_eq!(observed["active_run"], false);
        assert_eq!(observed["run_epoch"], 0);
        assert_eq!(observed["execution_budget"], serde_json::Value::Null);
        assert!(observed.get("active_run_token").is_none());
        assert!(observed.get("instruction").is_none());
    }
}

#[cfg(test)]
#[path = "workspace_tasks/modern_control_tests.rs"]
mod modern_control_tests;
