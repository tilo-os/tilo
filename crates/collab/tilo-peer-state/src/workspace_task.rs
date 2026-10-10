//! Typed, replay-safe mutations of the room-owned Task Center document.
//!
//! These are owner commands, not a second task store and not caller-authored
//! CRDT bytes. The native owner authenticates the authority before this pure
//! reduction changes `tasks_v1` and its execution-control sidecar together.

use automerge::{
    AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable,
};
use serde::{Deserialize, Serialize};
use tilo_desktop_model::{
    MAX_WORKSPACE_TASK_DIRECTION_BYTES, MAX_WORKSPACE_TASK_INSTRUCTION_BYTES,
    MAX_WORKSPACE_TASK_SUMMARY_BYTES, WorkspaceTask, WorkspaceTaskActiveRun,
    WorkspaceTaskAuditEvent, WorkspaceTaskCancellationRequest, WorkspaceTaskControl,
    WorkspaceTaskDirection, WorkspaceTaskDirectionApplied, WorkspaceTaskExecution,
    WorkspaceTaskExecutionPhase, WorkspaceTaskMutationReceipt, WorkspaceTaskMutationResult,
    WorkspaceTaskPendingFrontier, WorkspaceTaskResource, WorkspaceTaskRoute, WorkspaceTaskStatus,
};

pub const TASK_CONTROL_KEY: &str = "task_control_v1";
const MAX_TASK_TITLE_CHARS: usize = 240;
const MAX_TASK_INSTRUCTION_CHARS: usize = 8_000;
const MAX_TASK_RECEIPTS: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceTaskSnapshot {
    pub tasks: Vec<WorkspaceTask>,
    pub controls: std::collections::BTreeMap<String, WorkspaceTaskControl>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceTaskCommandId {
    pub device: [u8; 32],
    pub invocation: uuid::Uuid,
}

/// Read canonical tasks and execution controls from an admitted room document.
///
/// # Errors
/// A task-control entry is malformed.
pub fn snapshot(doc: &AutoCommit) -> Result<WorkspaceTaskSnapshot, String> {
    Ok(WorkspaceTaskSnapshot {
        tasks: tilo_desktop_model::projection::workspace_tasks_of(doc),
        controls: controls_of(doc)?,
    })
}

/// Read the retained result of one exact device-scoped invocation.
///
/// # Errors
/// A task-control entry is malformed.
pub fn result_of(
    doc: &AutoCommit,
    id: WorkspaceTaskCommandId,
) -> Result<Option<WorkspaceTaskMutationResult>, String> {
    Ok(controls_of(doc)?
        .values()
        .flat_map(|control| &control.receipts)
        .find(|receipt| receipt.device == id.device && receipt.invocation == id.invocation)
        .map(|receipt| receipt.result.clone()))
}

/// Authority retained by the host, never decoded from action arguments.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WorkspaceTaskAuthority {
    Person {
        caller: tilo_user_identity::DeviceCert,
    },
    Host {
        attestor: tilo_user_identity::DeviceCert,
    },
    Agent {
        attestor: tilo_user_identity::DeviceCert,
        agent_id: String,
    },
}

impl WorkspaceTaskAuthority {
    #[must_use]
    pub fn certificate(&self) -> &tilo_user_identity::DeviceCert {
        match self {
            Self::Person { caller } => caller,
            Self::Host { attestor } | Self::Agent { attestor, .. } => attestor,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTaskFinish {
    Done,
    Waiting,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WorkspaceTaskCommand {
    Queue {
        title: String,
        assigned_agent: String,
        route: WorkspaceTaskRoute,
        resource: Option<Box<WorkspaceTaskResource>>,
    },
    StartRun {
        task_id: String,
        agent_id: String,
    },
    Direction {
        task_id: String,
        text: String,
    },
    AppliedAck {
        task_id: String,
        run_epoch: u64,
        revision: u64,
    },
    Finish {
        task_id: String,
        run_epoch: u64,
        outcome: WorkspaceTaskFinish,
        summary: String,
    },
    Abandon {
        task_id: String,
        run_epoch: u64,
        reason: String,
    },
    /// A person steers the exact run and instruction frontier they observed.
    /// Appended to preserve the existing encoded command variant indices.
    DirectionForRun {
        task_id: String,
        run_epoch: u64,
        after_revision: u64,
        text: String,
    },
    /// A person explicitly returns a waiting task to the runnable queue.
    /// Appended to preserve every existing encoded command variant index.
    Retry {
        task_id: String,
        run_epoch: u64,
    },
    /// Public milestone for one exact run and authenticated report sequence.
    /// Appended to preserve existing encoded command variant indices. Private
    /// instructions, progress strings and model reasoning are not accepted.
    ExecutionPhase {
        task_id: String,
        run_epoch: u64,
        sequence: u64,
        phase: WorkspaceTaskExecutionPhase,
    },
    /// An admitted person cancels exactly the task/run frontier they reviewed.
    /// Keep the variant index already published on main.
    CancelForRun {
        task_id: String,
        run_epoch: u64,
        after_revision: u64,
    },
    /// Person-created coordination work; never mints an agent run lease.
    /// Appended to preserve every existing postcard command variant index.
    CreateManual {
        title: String,
        #[serde(default, deserialize_with = "deserialize_optional_manual_resource")]
        resource: Option<Box<WorkspaceTaskResource>>,
    },
    /// Replace one attachment only against the person's observed attachment.
    /// Manual coordination cannot alter a managed task or active run.
    AttachResource {
        task_id: String,
        expected_resource_id: Option<String>,
        #[serde(deserialize_with = "deserialize_manual_resource")]
        resource: Box<WorkspaceTaskResource>,
    },
    /// Owner-only policy update against the exact observed queued state.
    /// Appended to preserve all existing encoded command indices.
    SetExecutionBudget {
        task_id: String,
        expected_run_epoch: u64,
        expected_budget: Option<collab_types::task_budget::TaskExecutionBudget>,
        budget: Option<collab_types::task_budget::TaskExecutionBudget>,
    },
    /// Person approval of one exact queued task frontier; grants no app/resource authority.
    /// Keep this at the published command index before branch-only commands.
    ApproveRun {
        task_id: String,
        agent_id: String,
        expected_run_epoch: u64,
        after_revision: u64,
    },
    /// Queue a task whose complete instruction is independent of its short
    /// Task Center title. Appended to preserve encoded command indices.
    QueueTask {
        title: String,
        instruction: String,
        assigned_agent: String,
        route: WorkspaceTaskRoute,
        resource: Option<Box<WorkspaceTaskResource>>,
    },
    /// Request cooperative cancellation of one exact active run. Appended to
    /// preserve every prior encoded command variant index.
    CancelRun {
        task_id: String,
        run_epoch: u64,
        after_revision: u64,
    },
    /// Replace a pending instruction without retargeting an admitted run.
    /// Appended to preserve prior encoded command variant indices.
    AmendPending {
        task_id: String,
        frontier: WorkspaceTaskPendingFrontier,
        title: String,
        instruction: String,
    },
    /// Cancel pending execution, not an active-run stop or undo of prior effects.
    CancelPending {
        task_id: String,
        frontier: WorkspaceTaskPendingFrontier,
    },
}

// Ordinary resources already serialize to these five positional fields when
// agent_request is absent. Retain those published bytes rather than changing
// owner-commit digests by adding an Option tag. Only manual commands use this
// binary shape; their normal admission still validates every resource field.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManualResourceWire {
    app_id: Option<String>,
    app_label: Option<String>,
    id: String,
    kind: String,
    label: String,
}

impl From<ManualResourceWire> for WorkspaceTaskResource {
    fn from(value: ManualResourceWire) -> Self {
        Self {
            app_id: value.app_id,
            app_label: value.app_label,
            id: value.id,
            kind: value.kind,
            label: value.label,
            agent_request: None,
        }
    }
}

fn deserialize_manual_resource<'de, D>(
    deserializer: D,
) -> Result<Box<WorkspaceTaskResource>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    if deserializer.is_human_readable() {
        Box::<WorkspaceTaskResource>::deserialize(deserializer)
    } else {
        ManualResourceWire::deserialize(deserializer).map(|resource| Box::new(resource.into()))
    }
}

fn deserialize_optional_manual_resource<'de, D>(
    deserializer: D,
) -> Result<Option<Box<WorkspaceTaskResource>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    if deserializer.is_human_readable() {
        Option::<Box<WorkspaceTaskResource>>::deserialize(deserializer)
    } else {
        Option::<ManualResourceWire>::deserialize(deserializer)
            .map(|resource| resource.map(|resource| Box::new(resource.into())))
    }
}

impl WorkspaceTaskCommand {
    #[must_use]
    pub fn agent(&self) -> Option<&str> {
        match self {
            Self::Queue { assigned_agent, .. }
            | Self::QueueTask { assigned_agent, .. }
            | Self::StartRun {
                agent_id: assigned_agent,
                ..
            }
            | Self::ApproveRun {
                agent_id: assigned_agent,
                ..
            } => Some(assigned_agent),
            _ => None,
        }
    }
}

/// Already-authenticated actor passed from the owner's current membership check.
pub(crate) enum AdmittedTaskActor {
    Person { id: String, device: [u8; 32] },
    Host { device: [u8; 32] },
    Agent { id: String, device: [u8; 32] },
}

impl AdmittedTaskActor {
    fn device(&self) -> [u8; 32] {
        match self {
            Self::Person { device, .. } | Self::Host { device } | Self::Agent { device, .. } => {
                *device
            }
        }
    }
}

pub(crate) struct AppliedTaskCommand {
    pub changed: bool,
    pub result: WorkspaceTaskMutationResult,
}

/// Apply one owner-admitted command to the room document.
pub(crate) fn apply(
    doc: &mut AutoCommit,
    invocation: uuid::Uuid,
    request_digest: [u8; 32],
    actor: &AdmittedTaskActor,
    command: &WorkspaceTaskCommand,
    now: i64,
) -> Result<AppliedTaskCommand, String> {
    if invocation.is_nil() {
        return Err("task command invocation is nil".into());
    }
    let mut tasks = tilo_desktop_model::projection::workspace_tasks_of(doc);
    let mut controls = controls_of(doc)?;
    if let Some(receipt) = controls
        .values()
        .flat_map(|c| &c.receipts)
        .find(|receipt| receipt.device == actor.device() && receipt.invocation == invocation)
    {
        if receipt.request_digest != request_digest {
            return Err("task command invocation was already used for another request".into());
        }
        return Ok(AppliedTaskCommand {
            changed: false,
            result: receipt.result.clone(),
        });
    }

    let context = CommandContext {
        invocation,
        actor,
        now,
    };
    let (task_id, result) = dispatch(&mut tasks, &mut controls, &context, command)?;
    let control = controls
        .get_mut(&task_id)
        .ok_or("task command lost its execution control")?;
    if control.receipts.len() >= MAX_TASK_RECEIPTS {
        return Err("task command receipt capacity exhausted".into());
    }
    control.receipts.push(WorkspaceTaskMutationReceipt {
        device: actor.device(),
        invocation,
        request_digest,
        result: result.clone(),
    });
    tilo_desktop_model::projection::project_workspace_tasks(doc, &tasks)
        .map_err(|error| error.to_string())?;
    project_controls(doc, &controls)?;
    doc.commit();
    Ok(AppliedTaskCommand {
        changed: true,
        result,
    })
}

struct CommandContext<'a> {
    invocation: uuid::Uuid,
    actor: &'a AdmittedTaskActor,
    now: i64,
}

fn dispatch(
    tasks: &mut Vec<WorkspaceTask>,
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    command: &WorkspaceTaskCommand,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    match command {
        command @ (WorkspaceTaskCommand::CreateManual { .. }
        | WorkspaceTaskCommand::AttachResource { .. }) => {
            manual_command(tasks, controls, context, command)
        }
        command @ WorkspaceTaskCommand::SetExecutionBudget { .. } => {
            dispatch_budget(tasks, controls, context, command)
        }
        WorkspaceTaskCommand::Queue { .. } | WorkspaceTaskCommand::QueueTask { .. } => {
            dispatch_queue(tasks, controls, context, command)
        }
        command @ (WorkspaceTaskCommand::ApproveRun { .. }
        | WorkspaceTaskCommand::StartRun { .. }) => dispatch_run(tasks, controls, context, command),
        WorkspaceTaskCommand::Direction { task_id, text } => direct(
            tasks,
            controls,
            context.invocation,
            context.actor,
            task_id,
            text,
            context.now,
        ),
        WorkspaceTaskCommand::AppliedAck { .. } | WorkspaceTaskCommand::ExecutionPhase { .. } => {
            dispatch_agent_report(tasks, controls, context, command)
        }
        WorkspaceTaskCommand::Finish { .. } | WorkspaceTaskCommand::Abandon { .. } => {
            dispatch_terminal(tasks, controls, context, command)
        }
        WorkspaceTaskCommand::DirectionForRun {
            task_id,
            run_epoch,
            after_revision,
            text,
        } => direct_for_run(
            tasks,
            controls,
            context,
            task_id,
            text,
            (*run_epoch, *after_revision),
        ),
        WorkspaceTaskCommand::Retry { task_id, run_epoch } => {
            dispatch_retry(tasks, controls, context, task_id, *run_epoch)
        }
        WorkspaceTaskCommand::CancelRun {
            task_id,
            run_epoch,
            after_revision,
        } => cancel_run(
            tasks,
            controls,
            context.actor,
            task_id,
            (*run_epoch, *after_revision),
            context.now,
        ),
        WorkspaceTaskCommand::AmendPending { .. } | WorkspaceTaskCommand::CancelPending { .. } => {
            change_pending(tasks, controls, context, command)
        }
        WorkspaceTaskCommand::CancelForRun {
            task_id,
            run_epoch,
            after_revision,
        } => cancel_for_run(
            tasks,
            controls,
            context,
            task_id,
            (*run_epoch, *after_revision),
        ),
    }
}

fn dispatch_terminal(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    command: &WorkspaceTaskCommand,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    match command {
        WorkspaceTaskCommand::Finish {
            task_id,
            run_epoch,
            outcome,
            summary,
        } => finish(
            tasks,
            controls,
            context.actor,
            task_id,
            *run_epoch,
            *outcome,
            summary,
            context.now,
        ),
        WorkspaceTaskCommand::Abandon {
            task_id,
            run_epoch,
            reason,
        } => abandon(
            tasks,
            controls,
            context.actor,
            task_id,
            *run_epoch,
            reason,
            context.now,
        ),
        _ => Err("command is not a terminal task update".into()),
    }
}

fn change_pending(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    command: &WorkspaceTaskCommand,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let AdmittedTaskActor::Person { id: author, .. } = context.actor else {
        return Err("only a person may change pending work".into());
    };
    let (task_id, frontier, amendment) = match command {
        WorkspaceTaskCommand::AmendPending {
            task_id,
            frontier,
            title,
            instruction,
        } => {
            let title = title.trim();
            if title.is_empty() || title.chars().count() > MAX_TASK_TITLE_CHARS {
                return Err("task title is empty or too long".into());
            }
            bounded(
                instruction,
                MAX_WORKSPACE_TASK_INSTRUCTION_BYTES,
                "task instruction",
            )?;
            if instruction.chars().count() > MAX_TASK_INSTRUCTION_CHARS {
                return Err("task instruction exceeds 8000 characters".into());
            }
            (task_id, frontier, Some((title, instruction)))
        }
        WorkspaceTaskCommand::CancelPending { task_id, frontier } => (task_id, frontier, None),
        _ => return Err("command is not a pending task change".into()),
    };
    let task = task_mut(tasks, task_id)?;
    let control = controls
        .get_mut(task_id)
        .ok_or("pending task has no execution control")?;
    let current = WorkspaceTaskPendingFrontier {
        status: task.status,
        run_epoch: control.run_epoch,
        revision: control.pending_revision,
    };
    if !matches!(
        frontier.status,
        WorkspaceTaskStatus::Queued | WorkspaceTaskStatus::Waiting
    ) || control.active_run.is_some()
        || *frontier != current
    {
        return Err(
            "pending task changed; read its current frontier before submitting again".into(),
        );
    }
    let revision = control
        .pending_revision
        .checked_add(1)
        .ok_or("pending task revision exhausted")?;
    let (summary, result) = if let Some((title, instruction)) = amendment {
        title.clone_into(&mut task.title);
        control.instruction = Some(instruction.clone());
        control.instruction_required = true;
        // These are effective guidance for the old instruction, not the
        // historical ledger. A complete replacement must not reapply them
        // after the person's newer instruction on the next run. Exact
        // receipts, audit entries and prior CRDT versions remain intact.
        control.directions.clear();
        (
            "Updated pending task",
            WorkspaceTaskMutationResult::PendingAmended {
                task_id: task_id.clone(),
                revision,
            },
        )
    } else {
        task.status = WorkspaceTaskStatus::Cancelled;
        (
            "Cancelled pending task",
            WorkspaceTaskMutationResult::PendingCancelled {
                task_id: task_id.clone(),
                revision,
            },
        )
    };
    control.pending_revision = revision;
    task.updated_at = context.now;
    task.audit.push(WorkspaceTaskAuditEvent {
        at: context.now,
        actor: author.clone(),
        summary: summary.into(),
    });
    Ok((task_id.clone(), result))
}

fn dispatch_queue(
    tasks: &mut Vec<WorkspaceTask>,
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    command: &WorkspaceTaskCommand,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let (title, instruction, assigned_agent, route, resource) = match command {
        WorkspaceTaskCommand::Queue {
            title,
            assigned_agent,
            route,
            resource,
        } => (
            title,
            resource
                .as_deref()
                .and_then(|value| value.agent_request.as_ref())
                .map(|request| request.instruction.as_str()),
            assigned_agent,
            route,
            resource,
        ),
        WorkspaceTaskCommand::QueueTask {
            title,
            instruction,
            assigned_agent,
            route,
            resource,
        } => (
            title,
            Some(instruction.as_str()),
            assigned_agent,
            route,
            resource,
        ),
        _ => return Err("task queue command changed during dispatch".into()),
    };
    queue(
        tasks,
        controls,
        context,
        title,
        instruction,
        assigned_agent,
        *route,
        resource.as_deref().cloned(),
    )
}

fn dispatch_run(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    command: &WorkspaceTaskCommand,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    match command {
        WorkspaceTaskCommand::ApproveRun {
            task_id,
            agent_id,
            expected_run_epoch,
            after_revision,
        } => approve_run(
            tasks,
            controls,
            context,
            task_id,
            agent_id,
            (*expected_run_epoch, *after_revision),
        ),
        WorkspaceTaskCommand::StartRun { task_id, agent_id } => start_run(
            tasks,
            controls,
            context.actor,
            task_id,
            agent_id,
            context.now,
        ),
        _ => Err("not a run admission command".into()),
    }
}

fn dispatch_budget(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    command: &WorkspaceTaskCommand,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let WorkspaceTaskCommand::SetExecutionBudget {
        task_id,
        expected_run_epoch,
        expected_budget,
        budget,
    } = command
    else {
        return Err("command is not a task execution budget edit".into());
    };
    set_execution_budget(
        tasks,
        controls,
        context,
        task_id,
        (*expected_run_epoch, *expected_budget, *budget),
    )
}

fn dispatch_agent_report(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    command: &WorkspaceTaskCommand,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    match command {
        WorkspaceTaskCommand::AppliedAck {
            task_id,
            run_epoch,
            revision,
        } => applied(
            tasks,
            controls,
            context.actor,
            task_id,
            *run_epoch,
            *revision,
            context.now,
        ),
        WorkspaceTaskCommand::ExecutionPhase {
            task_id,
            run_epoch,
            sequence,
            phase,
        } => report_execution(
            tasks,
            controls,
            context,
            task_id,
            (*run_epoch, *sequence),
            *phase,
        ),
        _ => Err("command is not an agent report".into()),
    }
}

fn dispatch_retry(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    task_id: &str,
    run_epoch: u64,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    retry(
        tasks,
        controls,
        context.actor,
        task_id,
        run_epoch,
        context.now,
    )
}

/// Validate an ordinary resource reference, not an agent instruction carrier.
/// Labels are descriptive metadata; attachments grant no resource authority.
/// # Errors
/// Rejects oversized, malformed, path-like or managed-execution attachments.
pub fn validate_manual_resource(resource: &WorkspaceTaskResource) -> Result<(), String> {
    for (value, maximum, name) in [
        (
            resource.app_id.as_deref().unwrap_or_default(),
            256,
            "attachment app",
        ),
        (
            resource.app_label.as_deref().unwrap_or_default(),
            512,
            "attachment app label",
        ),
        (resource.id.as_str(), 2048, "attachment id"),
        (resource.kind.as_str(), 128, "attachment kind"),
        (resource.label.as_str(), 512, "attachment label"),
    ] {
        bounded(value, maximum, name)?;
        if value.chars().any(char::is_control) {
            return Err("attachment metadata contains control characters".into());
        }
    }
    let app = resource.app_id.as_deref().unwrap_or_default();
    if !app.contains('.')
        || !app
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        || resource.id.starts_with(['/', '\\'])
        || resource
            .id
            .split_once(':')
            .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("file"))
        || resource.agent_request.is_some()
    {
        return Err("manual attachment is not an ordinary application resource".into());
    }
    Ok(())
}

fn manual_command(
    tasks: &mut Vec<WorkspaceTask>,
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    command: &WorkspaceTaskCommand,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let AdmittedTaskActor::Person { id: author, device } = context.actor else {
        return Err("only a person may change manual coordination tasks".into());
    };
    match command {
        WorkspaceTaskCommand::CreateManual { title, resource } => {
            let title = title.trim();
            if title.is_empty()
                || title.chars().count() > MAX_TASK_TITLE_CHARS
                || title.chars().any(char::is_control)
                || tasks.len() >= 1024
            {
                return Err("manual task title or task capacity is invalid".into());
            }
            if let Some(resource) = resource {
                validate_manual_resource(resource)?;
            }
            let task_id = deterministic_task_id(*device, context.invocation);
            if tasks.iter().any(|task| task.id == task_id) || controls.contains_key(&task_id) {
                return Err("deterministic task id is already occupied".into());
            }
            tasks.push(WorkspaceTask {
                id: task_id.clone(),
                title: title.into(),
                created_by: author.clone(),
                assigned_agent: None,
                route: WorkspaceTaskRoute::Manual,
                status: WorkspaceTaskStatus::Queued,
                resource: resource.as_deref().cloned(),
                created_at: context.now,
                updated_at: context.now,
                audit: vec![WorkspaceTaskAuditEvent {
                    at: context.now,
                    actor: author.clone(),
                    summary: format!("Created manual task · {title}"),
                }],
            });
            controls.insert(task_id.clone(), WorkspaceTaskControl::default());
            Ok((
                task_id.clone(),
                WorkspaceTaskMutationResult::Queued { task_id },
            ))
        }
        WorkspaceTaskCommand::AttachResource {
            task_id,
            expected_resource_id,
            resource,
        } => {
            validate_manual_resource(resource)?;
            if let Some(observed) = expected_resource_id {
                bounded(observed, 2048, "observed attachment id")?;
                if observed.chars().any(char::is_control) {
                    return Err("observed attachment id contains control characters".into());
                }
            }
            let task = tasks
                .iter_mut()
                .find(|task| &task.id == task_id)
                .ok_or("manual task is absent")?;
            let control = controls
                .get(task_id)
                .ok_or("manual task control is absent")?;
            if task.route != WorkspaceTaskRoute::Manual
                || task.assigned_agent.is_some()
                || task.status.is_finished()
                || control.active_run.is_some()
                || control.run_epoch != 0
            {
                return Err(
                    "manual attachment cannot change a managed, leased or finished task".into(),
                );
            }
            if task.resource.as_ref().map(|value| value.id.as_str())
                != expected_resource_id.as_deref()
            {
                return Err("task attachment changed since it was observed".into());
            }
            task.resource = Some(resource.as_ref().clone());
            task.updated_at = context.now;
            task.audit.push(WorkspaceTaskAuditEvent {
                at: context.now,
                actor: author.clone(),
                summary: format!("Attached resource · {}", resource.label),
            });
            Ok((
                task_id.clone(),
                WorkspaceTaskMutationResult::ResourceAttached {
                    task_id: task_id.clone(),
                },
            ))
        }
        _ => Err("not a manual coordination command".into()),
    }
}

fn set_execution_budget(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    task_id: &str,
    observed: (
        u64,
        Option<collab_types::task_budget::TaskExecutionBudget>,
        Option<collab_types::task_budget::TaskExecutionBudget>,
    ),
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let AdmittedTaskActor::Person { id: actor, .. } = context.actor else {
        return Err("only the workspace owner may configure task execution budgets".into());
    };
    if let Some(budget) = observed.2 {
        budget.validate()?;
    }
    let task = tasks
        .iter_mut()
        .find(|task| task.id == task_id)
        .ok_or("task is absent")?;
    let control = controls.get_mut(task_id).ok_or("task control is absent")?;
    if task.route == WorkspaceTaskRoute::Manual
        || task.status.is_finished()
        || control.active_run.is_some()
        || control.run_epoch != observed.0
        || control.execution_budget != observed.1
    {
        return Err("task execution budget changed, task is active, manual or finished".into());
    }
    control.execution_budget = observed.2;
    task.updated_at = context.now;
    task.audit.push(WorkspaceTaskAuditEvent {
        at: context.now,
        actor: actor.clone(),
        summary: "Configured bounded task execution policy".into(),
    });
    Ok((
        task_id.into(),
        WorkspaceTaskMutationResult::ExecutionBudgetSet {
            task_id: task_id.into(),
        },
    ))
}

fn queue(
    tasks: &mut Vec<WorkspaceTask>,
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    title: &str,
    instruction: Option<&str>,
    assigned_agent: &str,
    route: WorkspaceTaskRoute,
    resource: Option<WorkspaceTaskResource>,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let AdmittedTaskActor::Person { id: author, device } = context.actor else {
        return Err("only a person may queue a workspace task".into());
    };
    let title = title.trim();
    if title.is_empty() || title.chars().count() > MAX_TASK_TITLE_CHARS {
        return Err("task title is empty or too long".into());
    }
    if assigned_agent.is_empty()
        || assigned_agent.len() > 256
        || route == WorkspaceTaskRoute::Manual
    {
        return Err("queued agent task has no valid agent route".into());
    }
    if let Some(instruction) = instruction {
        bounded(
            instruction,
            MAX_WORKSPACE_TASK_INSTRUCTION_BYTES,
            "task instruction",
        )?;
        if instruction.chars().count() > MAX_TASK_INSTRUCTION_CHARS {
            return Err("task instruction exceeds 8000 characters".into());
        }
    }
    if let Some(request) = resource
        .as_ref()
        .and_then(|value| value.agent_request.as_ref())
    {
        bounded(
            &request.instruction,
            MAX_WORKSPACE_TASK_INSTRUCTION_BYTES,
            "task instruction",
        )?;
        if request.invocation.0.is_nil()
            || request.chat_record.is_empty()
            || request.channel_id.is_empty()
            || request.message_id != format!("m_{}", request.invocation.0)
        {
            return Err("task source lacks its canonical Chat parent".into());
        }
        if let Some(task_id) = existing_chat_task(tasks, author, assigned_agent, route, request)? {
            return Ok((
                task_id.clone(),
                WorkspaceTaskMutationResult::Queued { task_id },
            ));
        }
    }
    let task_id = deterministic_task_id(*device, context.invocation);
    if tasks.iter().any(|task| task.id == task_id) || controls.contains_key(&task_id) {
        return Err("deterministic task id is already occupied".into());
    }
    let queue_ordinal = next_queue_ordinal(controls)?;
    tasks.push(WorkspaceTask {
        id: task_id.clone(),
        title: title.to_owned(),
        created_by: author.clone(),
        assigned_agent: Some(assigned_agent.to_owned()),
        route,
        status: WorkspaceTaskStatus::Queued,
        resource,
        created_at: context.now,
        updated_at: context.now,
        audit: vec![WorkspaceTaskAuditEvent {
            at: context.now,
            actor: author.clone(),
            summary: format!("Created task · {title}"),
        }],
    });
    controls.insert(
        task_id.clone(),
        WorkspaceTaskControl {
            queue_ordinal: Some(queue_ordinal),
            instruction: instruction.map(str::to_owned),
            instruction_required: instruction.is_some(),
            ..WorkspaceTaskControl::default()
        },
    );
    Ok((
        task_id.clone(),
        WorkspaceTaskMutationResult::Queued { task_id },
    ))
}

// The downstream command UUID is deliberately not the parent message UUID.
// A retry may come from another admitted device, so device-scoped invocation
// receipts alone cannot prevent a second executable task for the same message.
// This runs inside the owner's serialized mutation, before creating a task.
fn existing_chat_task(
    tasks: &[WorkspaceTask],
    author: &str,
    assigned_agent: &str,
    route: WorkspaceTaskRoute,
    request: &tilo_desktop_model::WorkspaceAgentRequest,
) -> Result<Option<String>, String> {
    for task in tasks {
        let Some(prior) = task
            .resource
            .as_ref()
            .and_then(|value| value.agent_request.as_ref())
        else {
            continue;
        };
        if prior.chat_record != request.chat_record || prior.message_id != request.message_id {
            continue;
        }
        if prior != request
            || task.created_by != author
            || task.assigned_agent.as_deref() != Some(assigned_agent)
            || task.route != route
        {
            return Err("canonical Chat parent is already bound to another task request".into());
        }
        return Ok(Some(task.id.clone()));
    }
    Ok(None)
}

fn start_run(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    actor: &AdmittedTaskActor,
    task_id: &str,
    agent_id: &str,
    now: i64,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    if !matches!(actor, AdmittedTaskActor::Host { .. }) {
        return Err("only the workspace host may start a task run".into());
    }
    begin_run(tasks, controls, task_id, agent_id, now, None)
}

fn approve_run(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    task_id: &str,
    agent_id: &str,
    frontier: (u64, u64),
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let AdmittedTaskActor::Person { id: person, device } = context.actor else {
        return Err("only an admitted person may approve a task run".into());
    };
    let task = task_mut(tasks, task_id)?;
    let control = controls
        .get(task_id)
        .ok_or("task has no execution control")?;
    if task.status != WorkspaceTaskStatus::Queued
        || task
            .resource
            .as_ref()
            .is_some_and(|resource| resource.agent_request.is_some())
        || control.instruction_required
        || control.instruction.is_some()
        || control.active_run.is_some()
        || control.run_epoch != frontier.0
        || control.directions.last().map_or(0, |value| value.revision) != frontier.1
    {
        return Err("queued task frontier changed; review before approving".into());
    }
    begin_run(
        tasks,
        controls,
        task_id,
        agent_id,
        context.now,
        Some(tilo_desktop_model::WorkspaceTaskApproval {
            person: person.clone(),
            device: *device,
            approved_at: context.now,
            instruction_revision: frontier.1,
        }),
    )
}

fn begin_run(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    task_id: &str,
    agent_id: &str,
    now: i64,
    approval: Option<tilo_desktop_model::WorkspaceTaskApproval>,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let task = task_mut(tasks, task_id)?;
    if task.status.is_finished()
        || task.route == WorkspaceTaskRoute::Manual
        || task.assigned_agent.as_deref() != Some(agent_id)
    {
        return Err("task cannot start for that agent".into());
    }
    let control = controls.entry(task_id.to_owned()).or_default();
    if control.instruction_required && control.instruction.is_none() {
        return Err("task lost its required execution instruction".into());
    }
    if control.active_run.is_some() {
        return Err("task already has an active run".into());
    }
    control.run_epoch = control
        .run_epoch
        .checked_add(1)
        .ok_or("task run epoch exhausted")?;
    let epoch = control.run_epoch;
    control.cancellation = None;
    control.active_run = Some(WorkspaceTaskActiveRun {
        epoch,
        agent_id: agent_id.to_owned(),
        started_at: now,
        applied_direction_revision: 0,
        approval: approval.clone(),
    });
    control.execution = None;
    task.status = WorkspaceTaskStatus::InProgress;
    task.updated_at = now;
    task.audit.push(WorkspaceTaskAuditEvent {
        at: now,
        actor: approval
            .as_ref()
            .map_or_else(|| format!("agent:{agent_id}"), |value| value.person.clone()),
        summary: approval.map_or_else(
            || format!("Started run {epoch}"),
            |_| format!("Approved run {epoch}"),
        ),
    });
    Ok((
        task_id.to_owned(),
        WorkspaceTaskMutationResult::RunStarted {
            task_id: task_id.to_owned(),
            epoch,
        },
    ))
}

fn report_execution(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    task_id: &str,
    fence: (u64, u64),
    phase: WorkspaceTaskExecutionPhase,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let AdmittedTaskActor::Agent { id: agent_id, .. } = context.actor else {
        return Err("only the assigned agent may report execution".into());
    };
    let task = task_mut(tasks, task_id)?;
    let control = controls
        .get_mut(task_id)
        .ok_or("task has no execution control")?;
    let run = control
        .active_run
        .as_ref()
        .ok_or("task has no active run")?;
    if task.status != WorkspaceTaskStatus::InProgress
        || task.assigned_agent.as_deref() != Some(agent_id)
        || run.agent_id != *agent_id
        || run.epoch != fence.0
        || (fence.1 == 0 && phase != WorkspaceTaskExecutionPhase::Accepted)
        || (control.execution.is_none() && phase != WorkspaceTaskExecutionPhase::Accepted)
        || control
            .execution
            .as_ref()
            .is_some_and(|value| value.sequence >= fence.1)
        || (phase == WorkspaceTaskExecutionPhase::Accepted
            && control
                .execution
                .as_ref()
                .is_some_and(|value| value.phase != phase))
    {
        return Err("task execution report is stale or does not own the active run".into());
    }
    control.execution = Some(WorkspaceTaskExecution {
        run_epoch: fence.0,
        sequence: fence.1,
        phase,
        updated_at: context.now,
    });
    task.updated_at = context.now;
    Ok((
        task_id.to_owned(),
        WorkspaceTaskMutationResult::ExecutionReported {
            task_id: task_id.to_owned(),
            sequence: fence.1,
        },
    ))
}

fn direct_for_run(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    task_id: &str,
    text: &str,
    fence: (u64, u64),
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let control = controls
        .get(task_id)
        .ok_or("task has no execution control")?;
    if control
        .active_run
        .as_ref()
        .is_none_or(|run| run.epoch != fence.0)
        || control
            .directions
            .last()
            .map_or(0, |direction| direction.revision)
            != fence.1
    {
        return Err("task run or instruction frontier changed; review before steering".into());
    }
    direct(
        tasks,
        controls,
        context.invocation,
        context.actor,
        task_id,
        text,
        context.now,
    )
}

fn cancel_run(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    actor: &AdmittedTaskActor,
    task_id: &str,
    fence: (u64, u64),
    now: i64,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let AdmittedTaskActor::Person { id, .. } = actor else {
        return Err("only a person may request task cancellation".into());
    };
    let task = task_mut(tasks, task_id)?;
    let control = controls
        .get_mut(task_id)
        .ok_or("active task has no execution control")?;
    let latest = control
        .directions
        .last()
        .map_or(0, |direction| direction.revision);
    if task.status != WorkspaceTaskStatus::InProgress
        || control.active_run.as_ref().map(|run| run.epoch) != Some(fence.0)
        || latest != fence.1
    {
        return Err("task run or instruction frontier changed; review before stopping".into());
    }
    if let Some(existing) = &control.cancellation {
        if existing.run_epoch == fence.0 && existing.requested_by == *id {
            return Ok((
                task_id.into(),
                WorkspaceTaskMutationResult::CancellationRequested {
                    task_id: task_id.into(),
                    epoch: fence.0,
                },
            ));
        }
        return Err("task cancellation was already requested by another person".into());
    }
    control.cancellation = Some(WorkspaceTaskCancellationRequest {
        run_epoch: fence.0,
        requested_by: id.clone(),
        requested_at: now,
    });
    task.updated_at = now;
    task.audit.push(WorkspaceTaskAuditEvent {
        at: now,
        actor: id.clone(),
        summary: format!("Requested cancellation of run {}", fence.0),
    });
    Ok((
        task_id.into(),
        WorkspaceTaskMutationResult::CancellationRequested {
            task_id: task_id.into(),
            epoch: fence.0,
        },
    ))
}

fn direct(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    invocation: uuid::Uuid,
    actor: &AdmittedTaskActor,
    task_id: &str,
    text: &str,
    now: i64,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let AdmittedTaskActor::Person { id: author, .. } = actor else {
        return Err("only a person may direct an active task".into());
    };
    bounded(text, MAX_WORKSPACE_TASK_DIRECTION_BYTES, "task direction")?;
    let task = task_mut(tasks, task_id)?;
    if task.status != WorkspaceTaskStatus::InProgress {
        return Err("only an active task accepts a direction".into());
    }
    let control = controls
        .get_mut(task_id)
        .ok_or("active task has no execution control")?;
    if control.active_run.is_none() {
        return Err("active task has no live run epoch".into());
    }
    let original = control.instruction.as_deref().unwrap_or(&task.title);
    let total_bytes = control
        .directions
        .iter()
        .try_fold(original.len(), |total, direction| {
            total
                .checked_add(direction.text.len())
                .and_then(|value| value.checked_add(3))
        })
        .and_then(|total| total.checked_add(text.len()))
        .and_then(|total| total.checked_add(3))
        .ok_or("task direction byte count overflow")?;
    let total_chars = control
        .directions
        .iter()
        .try_fold(original.chars().count(), |total, direction| {
            total
                .checked_add(direction.text.chars().count())
                .and_then(|value| value.checked_add(3))
        })
        .and_then(|total| total.checked_add(text.chars().count()))
        .and_then(|total| total.checked_add(3))
        .ok_or("task direction character count overflow")?;
    if total_bytes > MAX_WORKSPACE_TASK_INSTRUCTION_BYTES || total_chars > 8_000 {
        return Err("task directions exceed the run instruction bound".into());
    }
    let revision = match control.directions.last() {
        Some(direction) => direction
            .revision
            .checked_add(1)
            .ok_or("task direction revision exhausted")?,
        None => 1,
    };
    control
        .directions
        .push(WorkspaceTaskDirection::owner_admitted(
            revision,
            invocation,
            author.clone(),
            text.to_owned(),
            now,
        ));
    task.updated_at = now;
    task.audit.push(WorkspaceTaskAuditEvent {
        at: now,
        actor: author.clone(),
        summary: format!("Added direction {revision}"),
    });
    Ok((
        task_id.to_owned(),
        WorkspaceTaskMutationResult::DirectionAdded {
            task_id: task_id.to_owned(),
            revision,
        },
    ))
}

fn applied(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    actor: &AdmittedTaskActor,
    task_id: &str,
    run_epoch: u64,
    revision: u64,
    now: i64,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let AdmittedTaskActor::Agent { id: agent_id, .. } = actor else {
        return Err("only the assigned agent may acknowledge a direction".into());
    };
    let task = task_mut(tasks, task_id)?;
    if task.status != WorkspaceTaskStatus::InProgress
        || task.assigned_agent.as_deref() != Some(agent_id)
    {
        return Err("agent does not own this active task".into());
    }
    let control = controls
        .get_mut(task_id)
        .ok_or("active task has no execution control")?;
    let run = control
        .active_run
        .as_mut()
        .ok_or("task has no active run")?;
    let latest = control.directions.last().map_or(0, |value| value.revision);
    if run.epoch != run_epoch
        || run.agent_id != *agent_id
        || revision <= run.applied_direction_revision
        || revision > latest
    {
        return Err("direction acknowledgement is stale or out of range".into());
    }
    for direction in control
        .directions
        .iter_mut()
        .filter(|value| value.revision <= revision && value.applied.is_none())
    {
        direction.applied = Some(WorkspaceTaskDirectionApplied {
            agent_id: agent_id.clone(),
            run_epoch,
            applied_at: now,
        });
    }
    run.applied_direction_revision = revision;
    task.updated_at = now;
    task.audit.push(WorkspaceTaskAuditEvent {
        at: now,
        actor: format!("agent:{agent_id}"),
        summary: format!("Applied direction {revision}"),
    });
    Ok((
        task_id.to_owned(),
        WorkspaceTaskMutationResult::DirectionApplied {
            task_id: task_id.to_owned(),
            revision,
        },
    ))
}

fn finish(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    actor: &AdmittedTaskActor,
    task_id: &str,
    run_epoch: u64,
    outcome: WorkspaceTaskFinish,
    summary: &str,
    now: i64,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let AdmittedTaskActor::Agent { id: agent_id, .. } = actor else {
        return Err("only the assigned agent may finish its run".into());
    };
    bounded(summary, MAX_WORKSPACE_TASK_SUMMARY_BYTES, "task outcome")?;
    let task = task_mut(tasks, task_id)?;
    let control = controls
        .get_mut(task_id)
        .ok_or("active task has no execution control")?;
    let run = control
        .active_run
        .as_ref()
        .ok_or("task has no active run")?;
    let latest = control.directions.last().map_or(0, |value| value.revision);
    let unapplied_direction = run.applied_direction_revision != latest;
    let cancellation_pending = control
        .cancellation
        .as_ref()
        .is_some_and(|request| request.run_epoch == run_epoch);
    if task.status != WorkspaceTaskStatus::InProgress
        || task.assigned_agent.as_deref() != Some(agent_id)
        || run.agent_id != *agent_id
        || run.epoch != run_epoch
        || (cancellation_pending && outcome != WorkspaceTaskFinish::Cancelled)
        || (outcome != WorkspaceTaskFinish::Cancelled && unapplied_direction)
    {
        return Err("task finish is stale or has unapplied directions".into());
    }
    task.status = match outcome {
        WorkspaceTaskFinish::Done => WorkspaceTaskStatus::Done,
        WorkspaceTaskFinish::Waiting => WorkspaceTaskStatus::Waiting,
        WorkspaceTaskFinish::Cancelled => WorkspaceTaskStatus::Cancelled,
    };
    control.active_run = None;
    control.execution = None;
    task.updated_at = now;
    task.audit.push(WorkspaceTaskAuditEvent {
        at: now,
        actor: format!("agent:{agent_id}"),
        summary: summary.to_owned(),
    });
    Ok((
        task_id.to_owned(),
        WorkspaceTaskMutationResult::Finished {
            task_id: task_id.to_owned(),
        },
    ))
}

fn abandon(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    actor: &AdmittedTaskActor,
    task_id: &str,
    run_epoch: u64,
    reason: &str,
    now: i64,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    if !matches!(actor, AdmittedTaskActor::Host { .. }) {
        return Err("only the workspace host may abandon a run".into());
    }
    bounded(reason, MAX_WORKSPACE_TASK_SUMMARY_BYTES, "task abandonment")?;
    let task = task_mut(tasks, task_id)?;
    let control = controls
        .get_mut(task_id)
        .ok_or("active task has no execution control")?;
    if task.status != WorkspaceTaskStatus::InProgress
        || control.active_run.as_ref().map(|run| run.epoch) != Some(run_epoch)
        || control
            .cancellation
            .as_ref()
            .is_some_and(|request| request.run_epoch == run_epoch)
    {
        return Err("task abandonment names a stale run".into());
    }
    control.active_run = None;
    control.execution = None;
    task.status = WorkspaceTaskStatus::Waiting;
    task.updated_at = now;
    task.audit.push(WorkspaceTaskAuditEvent {
        at: now,
        actor: "host".into(),
        summary: reason.to_owned(),
    });
    Ok((
        task_id.to_owned(),
        WorkspaceTaskMutationResult::Abandoned {
            task_id: task_id.to_owned(),
        },
    ))
}

fn retry(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    actor: &AdmittedTaskActor,
    task_id: &str,
    run_epoch: u64,
    now: i64,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let AdmittedTaskActor::Person { id: author, .. } = actor else {
        return Err("only a person may retry a waiting task".into());
    };
    let task = task_mut(tasks, task_id)?;
    if task.status != WorkspaceTaskStatus::Waiting
        || controls
            .get(task_id)
            .is_none_or(|control| control.active_run.is_some() || control.run_epoch != run_epoch)
    {
        return Err("only a waiting task without an active run may be retried".into());
    }
    let queue_ordinal = next_queue_ordinal(controls)?;
    controls
        .get_mut(task_id)
        .ok_or("retried task has no execution control")?
        .queue_ordinal = Some(queue_ordinal);
    task.status = WorkspaceTaskStatus::Queued;
    task.updated_at = now;
    task.audit.push(WorkspaceTaskAuditEvent {
        at: now,
        actor: author.clone(),
        summary: "Queued retry".into(),
    });
    Ok((
        task_id.to_owned(),
        WorkspaceTaskMutationResult::Queued {
            task_id: task_id.to_owned(),
        },
    ))
}

fn next_queue_ordinal(
    controls: &std::collections::BTreeMap<String, WorkspaceTaskControl>,
) -> Result<u64, String> {
    controls
        .values()
        .filter_map(|control| control.queue_ordinal)
        .max()
        .unwrap_or_default()
        .checked_add(1)
        .ok_or_else(|| "task queue admission order exhausted".into())
}

fn cancel_for_run(
    tasks: &mut [WorkspaceTask],
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    task_id: &str,
    fence: (u64, u64),
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let AdmittedTaskActor::Person { id: author, .. } = context.actor else {
        return Err("only a person may cancel a workspace task".into());
    };
    let task = task_mut(tasks, task_id)?;
    let control = controls
        .get_mut(task_id)
        .ok_or("task has no execution control")?;
    if task.status.is_finished()
        || control.run_epoch != fence.0
        || control
            .directions
            .last()
            .map_or(0, |direction| direction.revision)
            != fence.1
    {
        return Err("task run or instruction frontier changed; review before cancelling".into());
    }
    control.active_run = None;
    control.execution = None;
    task.status = WorkspaceTaskStatus::Cancelled;
    task.updated_at = context.now;
    task.audit.push(WorkspaceTaskAuditEvent {
        at: context.now,
        actor: author.clone(),
        summary: "Cancelled task".into(),
    });
    Ok((
        task_id.to_owned(),
        WorkspaceTaskMutationResult::Finished {
            task_id: task_id.to_owned(),
        },
    ))
}

fn task_mut<'a>(tasks: &'a mut [WorkspaceTask], id: &str) -> Result<&'a mut WorkspaceTask, String> {
    tasks
        .iter_mut()
        .find(|task| task.id == id)
        .ok_or_else(|| "workspace task no longer exists".into())
}

fn bounded(text: &str, max: usize, label: &str) -> Result<(), String> {
    if text.trim().is_empty() || text.len() > max {
        return Err(format!("{label} is empty or exceeds {max} bytes"));
    }
    Ok(())
}

fn deterministic_task_id(device: [u8; 32], invocation: uuid::Uuid) -> String {
    let digest = blake3::Hasher::new_derive_key("Tilo workspace task id v1")
        .update(&device)
        .update(invocation.as_bytes())
        .finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    format!("task:{}", uuid::Uuid::from_bytes(bytes))
}

/// Read every typed execution-control sidecar.
///
/// # Errors
/// The root or one entry has the wrong type or invalid encoding.
pub fn controls_of(
    doc: &AutoCommit,
) -> Result<std::collections::BTreeMap<String, WorkspaceTaskControl>, String> {
    let Some((Value::Object(ObjType::Map), map)) = doc
        .get(ROOT, TASK_CONTROL_KEY)
        .map_err(|error| error.to_string())?
    else {
        return Ok(std::collections::BTreeMap::new());
    };
    let mut controls = std::collections::BTreeMap::new();
    let mut queue_ordinals = std::collections::BTreeSet::new();
    for key in doc.keys(&map) {
        let Some((Value::Scalar(value), _)) = doc
            .get(&map, key.as_str())
            .map_err(|error| error.to_string())?
        else {
            return Err("task control entry is not a scalar".into());
        };
        let ScalarValue::Bytes(bytes) = value.as_ref() else {
            return Err("task control entry is not bytes".into());
        };
        let control: WorkspaceTaskControl =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if let Some(ordinal) = control.queue_ordinal
            && (ordinal == 0 || !queue_ordinals.insert(ordinal))
        {
            return Err("task queue admission order is zero or duplicated".into());
        }
        if let Some(budget) = control.execution_budget {
            budget.validate()?;
        }
        if let Some(execution) = &control.execution
            && (control
                .active_run
                .as_ref()
                .is_none_or(|run| run.epoch != execution.run_epoch)
                || (execution.sequence == 0
                    && execution.phase != WorkspaceTaskExecutionPhase::Accepted))
        {
            return Err("task execution phase has no matching active run".into());
        }
        controls.insert(key, control);
    }
    Ok(controls)
}

fn project_controls(
    doc: &mut AutoCommit,
    controls: &std::collections::BTreeMap<String, WorkspaceTaskControl>,
) -> Result<(), String> {
    let map = match doc
        .get(ROOT, TASK_CONTROL_KEY)
        .map_err(|error| error.to_string())?
    {
        Some((Value::Object(ObjType::Map), id)) => id,
        Some(_) => return Err("task control root is not a map".into()),
        None => doc
            .put_object(ROOT, TASK_CONTROL_KEY, ObjType::Map)
            .map_err(|error| error.to_string())?,
    };
    for (task, control) in controls {
        let bytes = serde_json::to_vec(control).map_err(|error| error.to_string())?;
        doc.put(&map, task, ScalarValue::Bytes(bytes))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_resource_binary_decode_preserves_published_bytes_and_rejects_truncation() {
        let resource = WorkspaceTaskResource {
            app_id: Some("a.b".into()),
            app_label: Some("A".into()),
            id: "a:x".into(),
            kind: "x".into(),
            label: "X".into(),
            agent_request: None,
        };
        validate_manual_resource(&resource).unwrap();
        let commands = [
            (
                WorkspaceTaskCommand::AttachResource {
                    task_id: "task:one".into(),
                    expected_resource_id: None,
                    resource: Box::new(resource.clone()),
                },
                vec![
                    11, 8, b't', b'a', b's', b'k', b':', b'o', b'n', b'e', 0, 1, 3, b'a', b'.',
                    b'b', 1, 1, b'A', 3, b'a', b':', b'x', 1, b'x', 1, b'X',
                ],
            ),
            (
                WorkspaceTaskCommand::CreateManual {
                    title: "One".into(),
                    resource: Some(Box::new(resource)),
                },
                vec![
                    10, 3, b'O', b'n', b'e', 1, 1, 3, b'a', b'.', b'b', 1, 1, b'A', 3, b'a', b':',
                    b'x', 1, b'x', 1, b'X',
                ],
            ),
        ];
        for (command, published) in commands {
            assert_eq!(postcard::to_allocvec(&command).unwrap(), published);
            let decoded =
                crate::governed_routes::exact::<WorkspaceTaskCommand>(&published).unwrap();
            assert_eq!(postcard::to_allocvec(&decoded).unwrap(), published);
            for length in 0..published.len() {
                assert!(
                    crate::governed_routes::exact::<WorkspaceTaskCommand>(&published[..length])
                        .is_none()
                );
            }
            let mut trailing = published.clone();
            trailing.push(0);
            assert!(crate::governed_routes::exact::<WorkspaceTaskCommand>(&trailing).is_none());
        }
    }

    #[test]
    fn manual_resource_human_readable_decode_keeps_instruction_evidence_for_admission() {
        let encoded = serde_json::json!({"AttachResource": {
            "task_id": "task:one", "expected_resource_id": null,
            "resource": {"app_id": "a.b", "app_label": "A", "id": "a:x",
                "kind": "x", "label": "X", "agent_request": {
                    "chat_record": "record", "channel_id": "channel", "message_id": "message",
                    "instruction": "Do not omit this evidence",
                    "invocation": uuid::Uuid::from_u128(1)}}}});
        assert!(
            serde_json::from_value::<ManualResourceWire>(
                encoded["AttachResource"]["resource"].clone()
            )
            .is_err(),
            "the five-field map shape must refuse rather than discard instruction evidence"
        );
        let decoded: WorkspaceTaskCommand = serde_json::from_value(encoded).unwrap();
        let WorkspaceTaskCommand::AttachResource { resource, .. } = decoded else {
            panic!("manual attachment");
        };
        assert!(resource.agent_request.is_some());
        assert!(validate_manual_resource(&resource).is_err());
        assert!(matches!(
            serde_json::from_str::<WorkspaceTaskCommand>(r#"{"CreateManual":{"title":"One"}}"#)
                .unwrap(),
            WorkspaceTaskCommand::CreateManual { resource: None, .. }
        ));
    }

    #[test]
    fn native_coordination_wire_positions_remain_before_new_instruction_commands() {
        let attachment = WorkspaceTaskCommand::AttachResource {
            task_id: "task:one".into(),
            expected_resource_id: None,
            resource: Box::new(WorkspaceTaskResource {
                app_id: Some("so.tilo.test".into()),
                app_label: Some("Test".into()),
                id: "test:one".into(),
                kind: "test".into(),
                label: "One".into(),
                agent_request: None,
            }),
        };
        let budget = WorkspaceTaskCommand::SetExecutionBudget {
            task_id: "task:one".into(),
            expected_run_epoch: 0,
            expected_budget: None,
            budget: None,
        };
        let approval = WorkspaceTaskCommand::ApproveRun {
            task_id: "task:one".into(),
            agent_id: "scribe".into(),
            expected_run_epoch: 0,
            after_revision: 0,
        };
        for (command, index) in [(attachment, 11), (budget, 12), (approval, 13)] {
            let encoded = postcard::to_allocvec(&command).unwrap();
            assert_eq!(encoded[0], index);
            let decoded = postcard::from_bytes::<WorkspaceTaskCommand>(&encoded).unwrap();
            assert_eq!(postcard::to_allocvec(&decoded).unwrap(), encoded);
        }
        let frontier = WorkspaceTaskPendingFrontier {
            status: WorkspaceTaskStatus::Queued,
            run_epoch: 0,
            revision: 0,
        };
        for (command, index) in [
            (
                WorkspaceTaskCommand::QueueTask {
                    title: "One".into(),
                    instruction: "Do one thing".into(),
                    assigned_agent: "scribe".into(),
                    route: WorkspaceTaskRoute::Model,
                    resource: None,
                },
                14,
            ),
            (
                WorkspaceTaskCommand::CancelRun {
                    task_id: "task:one".into(),
                    run_epoch: 1,
                    after_revision: 0,
                },
                15,
            ),
            (
                WorkspaceTaskCommand::AmendPending {
                    task_id: "task:one".into(),
                    frontier,
                    title: "One".into(),
                    instruction: "Do one changed thing".into(),
                },
                16,
            ),
            (
                WorkspaceTaskCommand::CancelPending {
                    task_id: "task:one".into(),
                    frontier,
                },
                17,
            ),
        ] {
            let encoded = postcard::to_allocvec(&command).unwrap();
            assert_eq!(encoded[0], index);
            let decoded = postcard::from_bytes::<WorkspaceTaskCommand>(&encoded).unwrap();
            assert_eq!(
                serde_json::to_value(decoded).unwrap(),
                serde_json::to_value(command).unwrap()
            );
        }
    }

    #[test]
    fn native_coordination_wire_positions_remain_before_new_instruction_commands_and_results() {
        for (result, index) in [
            (
                WorkspaceTaskMutationResult::ResourceAttached {
                    task_id: "t".into(),
                },
                7,
            ),
            (
                WorkspaceTaskMutationResult::ExecutionBudgetSet {
                    task_id: "t".into(),
                },
                8,
            ),
            (
                WorkspaceTaskMutationResult::CancellationRequested {
                    task_id: "t".into(),
                    epoch: 1,
                },
                9,
            ),
            (
                WorkspaceTaskMutationResult::PendingAmended {
                    task_id: "t".into(),
                    revision: 1,
                },
                10,
            ),
            (
                WorkspaceTaskMutationResult::PendingCancelled {
                    task_id: "t".into(),
                    revision: 1,
                },
                11,
            ),
        ] {
            let encoded = postcard::to_allocvec(&result).unwrap();
            assert_eq!(encoded[0], index);
            assert_eq!(
                postcard::from_bytes::<WorkspaceTaskMutationResult>(&encoded).unwrap(),
                result
            );
        }
    }

    #[test]
    fn manual_commands_append_encoded_positions_and_refuse_malformed_owner_inputs() {
        assert_eq!(postcard::to_stdvec(&queue_command()).unwrap()[0], 0);
        assert_eq!(
            postcard::to_stdvec(&WorkspaceTaskCommand::Retry {
                task_id: "task:old".into(),
                run_epoch: 1,
            })
            .unwrap()[0],
            7
        );
        let valid = WorkspaceTaskCommand::CreateManual {
            title: "Review".into(),
            resource: None,
        };
        // Main already shipped cancellation at index 9. New manual commands
        // append after it; retain both encoding and decoding of that wire form.
        let cancellation = WorkspaceTaskCommand::CancelForRun {
            task_id: "t".into(),
            run_epoch: 1,
            after_revision: 0,
        };
        let cancellation_bytes = [9, 1, b't', 1, 0];
        assert_eq!(
            postcard::to_stdvec(&cancellation).unwrap(),
            cancellation_bytes
        );
        assert!(matches!(
            postcard::from_bytes::<WorkspaceTaskCommand>(&cancellation_bytes).unwrap(),
            WorkspaceTaskCommand::CancelForRun { task_id, run_epoch: 1, after_revision: 0 }
                if task_id == "t"
        ));
        assert_eq!(postcard::to_stdvec(&valid).unwrap()[0], 10);
        let mut doc = AutoCommit::new();
        for (invocation, title) in [
            (1, " ".to_owned()),
            (2, "x".repeat(241)),
            (3, "Review\ncommand".to_owned()),
        ] {
            assert!(
                run(
                    &mut doc,
                    invocation,
                    1,
                    &person(),
                    &WorkspaceTaskCommand::CreateManual {
                        title,
                        resource: None
                    }
                )
                .is_err()
            );
            assert!(snapshot(&doc).unwrap().tasks.is_empty());
        }
        let actual = run(&mut doc, 4, 1, &person(), &valid).unwrap();
        let before = snapshot(&doc).unwrap();
        assert!(
            run(
                &mut doc,
                4,
                2,
                &person(),
                &WorkspaceTaskCommand::CreateManual {
                    title: "Changed replay payload".into(),
                    resource: None,
                }
            )
            .is_err()
        );
        assert_eq!(snapshot(&doc).unwrap(), before);
        assert_eq!(
            run(&mut doc, 4, 1, &person(), &valid).unwrap().result,
            actual.result
        );
    }

    fn person() -> AdmittedTaskActor {
        AdmittedTaskActor::Person {
            id: "person-1".into(),
            device: [1; 32],
        }
    }

    fn host() -> AdmittedTaskActor {
        AdmittedTaskActor::Host { device: [2; 32] }
    }

    fn agent() -> AdmittedTaskActor {
        AdmittedTaskActor::Agent {
            id: "scribe".into(),
            device: [2; 32],
        }
    }

    fn queue_command() -> WorkspaceTaskCommand {
        WorkspaceTaskCommand::Queue {
            title: "Prepare the release notes".into(),
            assigned_agent: "scribe".into(),
            route: WorkspaceTaskRoute::Model,
            resource: None,
        }
    }

    #[test]
    fn queue_order_survives_equal_timestamps_clock_rollback_reopen_and_replay() {
        let mut invocations = [1, 2, 3];
        invocations
            .sort_by_cached_key(|id| deterministic_task_id([1; 32], uuid::Uuid::from_u128(*id)));
        invocations.reverse();
        let mut doc = AutoCommit::new();
        let mut admitted = Vec::new();
        for (index, invocation) in invocations.iter().enumerate() {
            let result = apply(
                &mut doc,
                uuid::Uuid::from_u128(*invocation),
                [7; 32],
                &person(),
                &queue_command(),
                if index == 2 { -100 } else { 42 },
            )
            .unwrap();
            let WorkspaceTaskMutationResult::Queued { task_id } = result.result else {
                panic!("queue result")
            };
            admitted.push(task_id);
        }
        assert!(
            admitted[0] > admitted[1],
            "opaque identity order must oppose admission order"
        );
        let mut reopened = AutoCommit::load(&doc.save()).unwrap();
        let state = snapshot(&reopened).unwrap();
        let mut ordered = state.tasks.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|task| task.queue_order_key(state.controls.get(&task.id)));
        assert_eq!(
            ordered.iter().map(|task| &task.id).collect::<Vec<_>>(),
            admitted.iter().collect::<Vec<_>>()
        );
        for (index, id) in admitted.iter().enumerate() {
            assert_eq!(
                state.controls[id].queue_ordinal,
                Some(u64::try_from(index).unwrap() + 1)
            );
        }
        let before = reopened.save();
        assert!(
            !apply(
                &mut reopened,
                uuid::Uuid::from_u128(invocations[0]),
                [7; 32],
                &person(),
                &queue_command(),
                999
            )
            .unwrap()
            .changed
        );
        assert_eq!(reopened.save(), before);
    }

    #[test]
    fn explicit_retry_rejoins_the_queue_tail_without_reordering_exact_replays() {
        let (mut doc, task_id) = task_with_applied_direction();
        run(
            &mut doc,
            5,
            5,
            &host(),
            &WorkspaceTaskCommand::Abandon {
                task_id: task_id.clone(),
                run_epoch: 1,
                reason: "Agent session stopped".into(),
            },
        )
        .unwrap();
        let WorkspaceTaskMutationResult::Queued { task_id: later } =
            run(&mut doc, 6, 6, &person(), &queue_command())
                .unwrap()
                .result
        else {
            panic!("queue result")
        };
        let retry = WorkspaceTaskCommand::Retry {
            task_id: task_id.clone(),
            run_epoch: 1,
        };
        run(&mut doc, 7, 7, &person(), &retry).unwrap();
        let mut reopened = AutoCommit::load(&doc.save()).unwrap();
        let state = snapshot(&reopened).unwrap();
        assert_eq!(state.controls[&later].queue_ordinal, Some(2));
        assert_eq!(state.controls[&task_id].queue_ordinal, Some(3));
        let mut ordered = state.tasks.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|task| task.queue_order_key(state.controls.get(&task.id)));
        assert_eq!(ordered[0].id, later);
        assert_eq!(ordered[1].id, task_id);
        let before = reopened.save();
        assert!(!run(&mut reopened, 7, 7, &person(), &retry).unwrap().changed);
        assert_eq!(reopened.save(), before);
    }

    #[test]
    fn invalid_or_exhausted_queue_ordinals_fail_without_partial_admission() {
        let mut doc = AutoCommit::new();
        run(&mut doc, 1, 1, &person(), &queue_command()).unwrap();
        run(&mut doc, 2, 2, &person(), &queue_command()).unwrap();
        let original = snapshot(&doc).unwrap().controls;
        for invalid in [0, 1] {
            let mut controls = original.clone();
            for control in controls.values_mut() {
                control.queue_ordinal = Some(invalid);
            }
            project_controls(&mut doc, &controls).unwrap();
            doc.commit();
            let before = doc.save();
            assert!(snapshot(&doc).is_err());
            assert!(run(&mut doc, 3, 3, &person(), &queue_command()).is_err());
            assert_eq!(doc.save(), before);
        }
        let mut controls = original;
        controls.values_mut().next().unwrap().queue_ordinal = Some(u64::MAX);
        project_controls(&mut doc, &controls).unwrap();
        doc.commit();
        let before = doc.save();
        assert!(run(&mut doc, 3, 3, &person(), &queue_command()).is_err());
        assert_eq!(doc.save(), before);
    }

    fn run(
        doc: &mut AutoCommit,
        invocation: u128,
        digest: u8,
        actor: &AdmittedTaskActor,
        command: &WorkspaceTaskCommand,
    ) -> Result<AppliedTaskCommand, String> {
        apply(
            doc,
            uuid::Uuid::from_u128(invocation),
            [digest; 32],
            actor,
            command,
            i64::try_from(invocation).unwrap(),
        )
    }

    fn pending_task() -> (AutoCommit, String, WorkspaceTaskPendingFrontier) {
        let mut doc = AutoCommit::new();
        let queued = run(&mut doc, 1, 1, &person(), &queue_command()).unwrap();
        let WorkspaceTaskMutationResult::Queued { task_id } = queued.result else {
            panic!("queue result")
        };
        (
            doc,
            task_id,
            WorkspaceTaskPendingFrontier {
                status: WorkspaceTaskStatus::Queued,
                run_epoch: 0,
                revision: 0,
            },
        )
    }

    #[test]
    fn pending_amendment_preserves_queue_order_instruction_and_exact_replay() {
        let (mut doc, task_id, frontier) = pending_task();
        let ordinal = snapshot(&doc).unwrap().controls[&task_id].queue_ordinal;
        let command = WorkspaceTaskCommand::AmendPending {
            task_id: task_id.clone(),
            frontier,
            title: " Updated title ".into(),
            instruction: "Full replacement\n  Preserve indentation and final newline.\n".into(),
        };
        let saved = run(&mut doc, 2, 2, &person(), &command).unwrap();
        assert!(saved.changed);
        assert_eq!(
            saved.result,
            WorkspaceTaskMutationResult::PendingAmended {
                task_id: task_id.clone(),
                revision: 1,
            }
        );
        let state = snapshot(&doc).unwrap();
        assert_eq!(state.tasks[0].title, "Updated title");
        assert_eq!(state.tasks[0].status, WorkspaceTaskStatus::Queued);
        assert_eq!(state.controls[&task_id].queue_ordinal, ordinal);
        assert_eq!(
            state.controls[&task_id].instruction.as_deref(),
            Some("Full replacement\n  Preserve indentation and final newline.\n")
        );
        assert!(state.controls[&task_id].instruction_required);
        run(
            &mut doc,
            3,
            3,
            &host(),
            &WorkspaceTaskCommand::StartRun {
                task_id,
                agent_id: "scribe".into(),
            },
        )
        .unwrap();
        let mut reopened = AutoCommit::load(&doc.save()).unwrap();
        let before = reopened.save();
        let replay = run(&mut reopened, 2, 2, &person(), &command).unwrap();
        assert!(!replay.changed);
        assert_eq!(replay.result, saved.result);
        assert_eq!(
            reopened.save(),
            before,
            "historical Saved must not amend an active run"
        );
        assert!(run(&mut reopened, 2, 9, &person(), &command).is_err());
        assert_eq!(reopened.save(), before);
    }

    #[test]
    fn pending_cancel_is_definitive_and_loses_to_newer_instruction() {
        let (mut doc, task_id, frontier) = pending_task();
        run(
            &mut doc,
            2,
            2,
            &person(),
            &WorkspaceTaskCommand::AmendPending {
                task_id: task_id.clone(),
                frontier,
                title: "Updated".into(),
                instruction: "Do this instead".into(),
            },
        )
        .unwrap();
        let before = doc.save();
        assert!(
            run(
                &mut doc,
                3,
                3,
                &person(),
                &WorkspaceTaskCommand::CancelPending {
                    task_id: task_id.clone(),
                    frontier,
                }
            )
            .is_err()
        );
        assert_eq!(doc.save(), before);
        let cancel = WorkspaceTaskCommand::CancelPending {
            task_id: task_id.clone(),
            frontier: WorkspaceTaskPendingFrontier {
                revision: 1,
                ..frontier
            },
        };
        let saved = run(&mut doc, 4, 4, &person(), &cancel).unwrap();
        assert_eq!(
            saved.result,
            WorkspaceTaskMutationResult::PendingCancelled {
                task_id: task_id.clone(),
                revision: 2,
            }
        );
        let state = snapshot(&doc).unwrap();
        assert_eq!(state.tasks[0].status, WorkspaceTaskStatus::Cancelled);
        assert!(state.controls[&task_id].active_run.is_none());
        assert!(
            state.controls[&task_id].cancellation.is_none(),
            "not an active-run stop request"
        );
        let before = doc.save();
        assert!(
            run(
                &mut doc,
                5,
                5,
                &host(),
                &WorkspaceTaskCommand::StartRun {
                    task_id,
                    agent_id: "scribe".into(),
                }
            )
            .is_err()
        );
        assert_eq!(doc.save(), before);
        let mut reopened = AutoCommit::load(&before).unwrap();
        let replay = run(&mut reopened, 4, 4, &person(), &cancel).unwrap();
        assert!(!replay.changed);
        assert_eq!(replay.result, saved.result);
    }

    #[test]
    fn pending_changes_lose_to_run_admission_waiting_and_retry() {
        let (mut doc, task_id, frontier) = pending_task();
        let amend = |frontier| WorkspaceTaskCommand::AmendPending {
            task_id: task_id.clone(),
            frontier,
            title: "Replacement".into(),
            instruction: "New instruction".into(),
        };
        run(
            &mut doc,
            2,
            2,
            &host(),
            &WorkspaceTaskCommand::StartRun {
                task_id: task_id.clone(),
                agent_id: "scribe".into(),
            },
        )
        .unwrap();
        let before = doc.save();
        for command in [
            amend(frontier),
            WorkspaceTaskCommand::CancelPending {
                task_id: task_id.clone(),
                frontier,
            },
        ] {
            assert!(run(&mut doc, 3, 3, &person(), &command).is_err());
            assert_eq!(doc.save(), before);
        }
        run(
            &mut doc,
            4,
            4,
            &agent(),
            &WorkspaceTaskCommand::Finish {
                task_id: task_id.clone(),
                run_epoch: 1,
                outcome: WorkspaceTaskFinish::Waiting,
                summary: "Needs direction".into(),
            },
        )
        .unwrap();
        let before = doc.save();
        assert!(run(&mut doc, 5, 5, &person(), &amend(frontier)).is_err());
        assert_eq!(doc.save(), before);
        let waiting = WorkspaceTaskPendingFrontier {
            status: WorkspaceTaskStatus::Waiting,
            run_epoch: 1,
            revision: 0,
        };
        run(&mut doc, 6, 6, &person(), &amend(waiting)).unwrap();
        run(
            &mut doc,
            7,
            7,
            &person(),
            &WorkspaceTaskCommand::Retry {
                task_id: task_id.clone(),
                run_epoch: 1,
            },
        )
        .unwrap();
        let before = doc.save();
        assert!(
            run(
                &mut doc,
                8,
                8,
                &person(),
                &WorkspaceTaskCommand::CancelPending {
                    task_id,
                    frontier: WorkspaceTaskPendingFrontier {
                        revision: 1,
                        ..waiting
                    },
                }
            )
            .is_err()
        );
        assert_eq!(
            doc.save(),
            before,
            "retry changes the exact observed pending state"
        );
    }

    #[test]
    fn pending_changes_reject_nonpeople_bounds_and_revision_overflow() {
        let (mut doc, task_id, frontier) = pending_task();
        let cancel = WorkspaceTaskCommand::CancelPending {
            task_id: task_id.clone(),
            frontier,
        };
        let before = doc.save();
        for actor in [host(), agent()] {
            assert!(run(&mut doc, 2, 2, &actor, &cancel).is_err());
            assert_eq!(doc.save(), before);
        }
        for (title, instruction) in [
            (" ".into(), "Instruction".into()),
            ("x".repeat(241), "Instruction".into()),
            ("Title".into(), " ".into()),
            ("Title".into(), "x".repeat(8_001)),
            ("Title".into(), "😀".repeat(8_193)),
        ] {
            assert!(
                run(
                    &mut doc,
                    2,
                    2,
                    &person(),
                    &WorkspaceTaskCommand::AmendPending {
                        task_id: task_id.clone(),
                        frontier,
                        title,
                        instruction,
                    }
                )
                .is_err()
            );
            assert_eq!(doc.save(), before);
        }
        let mut controls = controls_of(&doc).unwrap();
        controls.get_mut(&task_id).unwrap().pending_revision = u64::MAX;
        project_controls(&mut doc, &controls).unwrap();
        doc.commit();
        let before = doc.save();
        assert!(
            run(
                &mut doc,
                2,
                2,
                &person(),
                &WorkspaceTaskCommand::CancelPending {
                    task_id,
                    frontier: WorkspaceTaskPendingFrontier {
                        revision: u64::MAX,
                        ..frontier
                    },
                }
            )
            .is_err()
        );
        assert_eq!(doc.save(), before);
    }

    #[test]
    fn pending_replacement_supersedes_old_guidance_without_rewriting_history() {
        let (mut doc, task_id) = task_with_applied_direction();
        run(
            &mut doc,
            5,
            5,
            &agent(),
            &WorkspaceTaskCommand::Finish {
                task_id: task_id.clone(),
                run_epoch: 1,
                outcome: WorkspaceTaskFinish::Waiting,
                summary: "Needs new instructions".into(),
            },
        )
        .unwrap();
        let history = doc.get_heads();
        let old_control = controls_of(&doc).unwrap()[&task_id].clone();
        assert_eq!(old_control.directions.len(), 1);
        run(
            &mut doc,
            6,
            6,
            &person(),
            &WorkspaceTaskCommand::AmendPending {
                task_id: task_id.clone(),
                frontier: WorkspaceTaskPendingFrontier {
                    status: WorkspaceTaskStatus::Waiting,
                    run_epoch: 1,
                    revision: 0,
                },
                title: "New plan".into(),
                instruction: "Replace the old instruction and guidance".into(),
            },
        )
        .unwrap();
        let current = snapshot(&doc).unwrap();
        assert!(current.controls[&task_id].directions.is_empty());
        assert!(
            current.controls[&task_id]
                .receipts
                .starts_with(&old_control.receipts)
        );
        let old_version = doc.fork_at(&history).unwrap();
        assert_eq!(controls_of(&old_version).unwrap()[&task_id], old_control);
        run(
            &mut doc,
            7,
            7,
            &person(),
            &WorkspaceTaskCommand::Retry {
                task_id: task_id.clone(),
                run_epoch: 1,
            },
        )
        .unwrap();
        run(
            &mut doc,
            8,
            8,
            &host(),
            &WorkspaceTaskCommand::StartRun {
                task_id: task_id.clone(),
                agent_id: "scribe".into(),
            },
        )
        .unwrap();
        let next = snapshot(&doc).unwrap();
        assert_eq!(
            next.controls[&task_id].active_run.as_ref().unwrap().epoch,
            2
        );
        assert!(next.controls[&task_id].directions.is_empty());
    }

    fn task_with_applied_direction() -> (AutoCommit, String) {
        let mut doc = AutoCommit::new();
        let queued = run(&mut doc, 1, 1, &person(), &queue_command()).unwrap();
        let WorkspaceTaskMutationResult::Queued { task_id } = queued.result else {
            panic!("queue result")
        };
        run(
            &mut doc,
            2,
            2,
            &host(),
            &WorkspaceTaskCommand::StartRun {
                task_id: task_id.clone(),
                agent_id: "scribe".into(),
            },
        )
        .unwrap();
        run(
            &mut doc,
            3,
            3,
            &person(),
            &WorkspaceTaskCommand::DirectionForRun {
                task_id: task_id.clone(),
                run_epoch: 1,
                after_revision: 0,
                text: "Preserve this direction across recovery".into(),
            },
        )
        .unwrap();
        run(
            &mut doc,
            4,
            4,
            &agent(),
            &WorkspaceTaskCommand::AppliedAck {
                task_id: task_id.clone(),
                run_epoch: 1,
                revision: 1,
            },
        )
        .unwrap();
        (doc, task_id)
    }

    fn phase_command(
        task: &str,
        epoch: u64,
        sequence: u64,
        phase: WorkspaceTaskExecutionPhase,
    ) -> WorkspaceTaskCommand {
        WorkspaceTaskCommand::ExecutionPhase {
            task_id: task.into(),
            run_epoch: epoch,
            sequence,
            phase,
        }
    }

    #[test]
    fn published_cancel_for_run_wire_index_survives_new_commands() {
        // Bytes emitted by main before QueueTask and CancelRun were added.
        let historical = [9, 1, b't', 1, 2];
        let command = postcard::from_bytes::<WorkspaceTaskCommand>(&historical).unwrap();
        let WorkspaceTaskCommand::CancelForRun {
            task_id,
            run_epoch,
            after_revision,
        } = &command
        else {
            panic!("published cancellation variant changed")
        };
        assert_eq!(task_id, "t");
        assert_eq!((*run_epoch, *after_revision), (1, 2));
        assert_eq!(postcard::to_allocvec(&command).unwrap(), historical);
    }

    #[test]
    fn execution_phase_distinguishes_lease_from_acceptance_and_replays_after_reopen() {
        let (mut doc, task) = task_with_applied_direction();
        assert!(snapshot(&doc).unwrap().controls[&task].execution.is_none());
        let command = phase_command(&task, 1, 0, WorkspaceTaskExecutionPhase::Accepted);
        let encoded = postcard::to_allocvec(&command).unwrap();
        assert_eq!(encoded[0], 8, "the existing phase wire index remains fixed");
        let decoded = postcard::from_bytes::<WorkspaceTaskCommand>(&encoded).unwrap();
        assert_eq!(postcard::to_allocvec(&decoded).unwrap(), encoded);
        let accepted = run(&mut doc, 5, 5, &agent(), &command).unwrap();
        assert!(accepted.changed);
        let mut reopened = AutoCommit::load(&doc.save()).unwrap();
        let replay = run(&mut reopened, 5, 5, &agent(), &command).unwrap();
        assert!(!replay.changed);
        assert_eq!(replay.result, accepted.result);
        assert_eq!(
            snapshot(&reopened).unwrap().controls[&task].execution,
            Some(WorkspaceTaskExecution {
                run_epoch: 1,
                sequence: 0,
                phase: WorkspaceTaskExecutionPhase::Accepted,
                updated_at: 5,
            })
        );
        run(
            &mut reopened,
            6,
            6,
            &agent(),
            &phase_command(&task, 1, 1, WorkspaceTaskExecutionPhase::Planning),
        )
        .unwrap();
        run(
            &mut reopened,
            7,
            7,
            &agent(),
            &phase_command(&task, 1, 2, WorkspaceTaskExecutionPhase::Executing),
        )
        .unwrap();
        let before = reopened.get_heads();
        for (sequence, phase) in [
            (1, WorkspaceTaskExecutionPhase::Planning),
            (3, WorkspaceTaskExecutionPhase::Accepted),
        ] {
            assert!(
                run(
                    &mut reopened,
                    8,
                    8,
                    &agent(),
                    &phase_command(&task, 1, sequence, phase)
                )
                .is_err()
            );
            assert_eq!(reopened.get_heads(), before);
        }
        assert_eq!(
            snapshot(&reopened).unwrap().controls[&task]
                .execution
                .as_ref()
                .unwrap()
                .phase,
            WorkspaceTaskExecutionPhase::Executing
        );
        assert!(
            run(&mut reopened, 5, 9, &agent(), &command).is_err(),
            "a UUID cannot be reused with another admitted digest"
        );
    }

    #[test]
    fn execution_phase_refuses_other_actors_epochs_and_unaccepted_progress() {
        let (mut doc, task) = task_with_applied_direction();
        let before = doc.get_heads();
        let other = AdmittedTaskActor::Agent {
            id: "other".into(),
            device: [2; 32],
        };
        for actor in [person(), host(), other] {
            assert!(
                run(
                    &mut doc,
                    5,
                    5,
                    &actor,
                    &phase_command(&task, 1, 0, WorkspaceTaskExecutionPhase::Accepted)
                )
                .is_err()
            );
            assert_eq!(doc.get_heads(), before);
        }
        for command in [
            phase_command(&task, 2, 0, WorkspaceTaskExecutionPhase::Accepted),
            phase_command(&task, 1, 0, WorkspaceTaskExecutionPhase::Planning),
            phase_command(&task, 1, 1, WorkspaceTaskExecutionPhase::Executing),
        ] {
            assert!(run(&mut doc, 5, 5, &agent(), &command).is_err());
            assert_eq!(doc.get_heads(), before);
        }
    }

    #[test]
    fn execution_phase_is_cleared_on_finish_and_cannot_follow_a_replacement_run() {
        let (mut doc, task) = task_with_applied_direction();
        run(
            &mut doc,
            5,
            5,
            &agent(),
            &phase_command(&task, 1, 0, WorkspaceTaskExecutionPhase::Accepted),
        )
        .unwrap();
        run(
            &mut doc,
            6,
            6,
            &agent(),
            &WorkspaceTaskCommand::Finish {
                task_id: task.clone(),
                run_epoch: 1,
                outcome: WorkspaceTaskFinish::Waiting,
                summary: "Needs a decision".into(),
            },
        )
        .unwrap();
        assert!(snapshot(&doc).unwrap().controls[&task].execution.is_none());
        run(
            &mut doc,
            7,
            7,
            &person(),
            &WorkspaceTaskCommand::Retry {
                task_id: task.clone(),
                run_epoch: 1,
            },
        )
        .unwrap();
        run(
            &mut doc,
            8,
            8,
            &host(),
            &WorkspaceTaskCommand::StartRun {
                task_id: task.clone(),
                agent_id: "scribe".into(),
            },
        )
        .unwrap();
        assert!(snapshot(&doc).unwrap().controls[&task].execution.is_none());
        let before = doc.get_heads();
        assert!(
            run(
                &mut doc,
                9,
                9,
                &agent(),
                &phase_command(&task, 1, 3, WorkspaceTaskExecutionPhase::Executing)
            )
            .is_err()
        );
        assert_eq!(doc.get_heads(), before);
        run(
            &mut doc,
            10,
            10,
            &agent(),
            &phase_command(&task, 2, 0, WorkspaceTaskExecutionPhase::Accepted),
        )
        .unwrap();
        assert_eq!(
            snapshot(&doc).unwrap().controls[&task]
                .execution
                .as_ref()
                .unwrap()
                .run_epoch,
            2
        );
    }

    #[test]
    fn execution_phase_decoder_accepts_legacy_absence_but_rejects_orphaned_state() {
        let legacy: WorkspaceTaskControl =
            serde_json::from_str(r#"{"run_epoch":1,"directions":[],"receipts":[]}"#).unwrap();
        assert!(legacy.execution.is_none());
        let (mut doc, task) = task_with_applied_direction();
        let mut controls = controls_of(&doc).unwrap();
        controls.get_mut(&task).unwrap().execution = Some(WorkspaceTaskExecution {
            run_epoch: 2,
            sequence: 1,
            phase: WorkspaceTaskExecutionPhase::Planning,
            updated_at: 5,
        });
        project_controls(&mut doc, &controls).unwrap();
        assert!(
            snapshot(&doc).is_err(),
            "persisted phase must match the actual active epoch"
        );
    }

    #[test]
    fn canonical_chat_parent_converges_across_devices_and_survives_reopen() {
        let mut doc = AutoCommit::new();
        let parent = uuid::Uuid::from_u128(900);
        let mut command = queue_command();
        if let WorkspaceTaskCommand::Queue { resource, .. } = &mut command {
            *resource = Some(Box::new(WorkspaceTaskResource {
                app_id: Some("so.tilo.chat".into()),
                app_label: Some("Chat".into()),
                id: format!("chat.message:m_{parent}"),
                kind: "chat.message".into(),
                label: "Agent request".into(),
                agent_request: Some(tilo_desktop_model::WorkspaceAgentRequest {
                    chat_record: "chat-record".into(),
                    channel_id: "agent-dm".into(),
                    message_id: format!("m_{parent}"),
                    instruction: "Prepare the complete release notes".into(),
                    invocation: serde_json::from_value(serde_json::json!(parent)).unwrap(),
                }),
            }));
        }
        let first = run(&mut doc, 1, 7, &person(), &command).unwrap();
        let mut reopened = AutoCommit::load(&doc.save()).unwrap();
        let second_device = AdmittedTaskActor::Person {
            id: "person-1".into(),
            device: [3; 32],
        };
        let retry = run(&mut reopened, 2, 8, &second_device, &command).unwrap();
        assert_eq!(retry.result, first.result);
        let state = snapshot(&reopened).unwrap();
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.controls.values().next().unwrap().receipts.len(), 2);
        assert!(
            !run(&mut reopened, 2, 8, &second_device, &command)
                .unwrap()
                .changed
        );
        let before = reopened.save();
        let other_person = AdmittedTaskActor::Person {
            id: "person-2".into(),
            device: [4; 32],
        };
        assert!(run(&mut reopened, 3, 9, &other_person, &command).is_err());
        assert_eq!(reopened.save(), before);
        if let WorkspaceTaskCommand::Queue {
            resource: Some(resource),
            ..
        } = &mut command
        {
            resource
                .agent_request
                .as_mut()
                .unwrap()
                .instruction
                .push_str(" changed");
        }
        assert!(run(&mut reopened, 3, 9, &second_device, &command).is_err());
        assert_eq!(reopened.save(), before);
        assert_eq!(snapshot(&reopened).unwrap().tasks.len(), 1);
    }

    #[test]
    fn exact_queue_replay_is_stable_and_conflicting_reuse_is_refused() {
        let mut doc = AutoCommit::new();
        let command = queue_command();
        let legacy_bytes = postcard::to_allocvec(&command).unwrap();
        assert_eq!(legacy_bytes[0], 0);
        let WorkspaceTaskCommand::Queue { title, .. } =
            postcard::from_bytes::<WorkspaceTaskCommand>(&legacy_bytes).unwrap()
        else {
            panic!("legacy queue variant changed")
        };
        assert_eq!(title, "Prepare the release notes");
        let first = run(&mut doc, 1, 7, &person(), &command).unwrap();
        assert!(first.changed);
        let WorkspaceTaskMutationResult::Queued { task_id } = first.result else {
            panic!("queue result")
        };
        let persisted = doc.save();
        let mut reopened = AutoCommit::load(&persisted).unwrap();
        let replay = run(&mut reopened, 1, 7, &person(), &command).unwrap();
        assert!(!replay.changed);
        assert_eq!(
            replay.result,
            WorkspaceTaskMutationResult::Queued { task_id }
        );
        assert_eq!(snapshot(&reopened).unwrap().tasks.len(), 1);
        assert!(run(&mut reopened, 1, 8, &person(), &command).is_err());
    }

    #[test]
    fn queued_instruction_is_byte_exact_bounded_and_survives_reopen() {
        let instruction = "𐐀".repeat(MAX_TASK_INSTRUCTION_CHARS);
        assert_eq!(instruction.len(), 32_000);
        let command = WorkspaceTaskCommand::QueueTask {
            title: "Review release".into(),
            instruction: instruction.clone(),
            assigned_agent: "scribe".into(),
            route: WorkspaceTaskRoute::Model,
            resource: None,
        };
        // Main's owner-approved native run command occupies index 13. This
        // branch-only instruction command follows it without changing either wire form.
        assert_eq!(postcard::to_allocvec(&command).unwrap()[0], 14);
        let mut doc = AutoCommit::new();
        let queued = run(&mut doc, 21, 21, &person(), &command).unwrap();
        let WorkspaceTaskMutationResult::Queued { task_id } = queued.result else {
            panic!("queue result")
        };
        let mut reopened = AutoCommit::load(&doc.save()).unwrap();
        let state = snapshot(&reopened).unwrap();
        let control = &state.controls[&task_id];
        assert_eq!(control.instruction.as_deref(), Some(instruction.as_str()));
        assert!(control.instruction_required);
        assert!(
            !run(&mut reopened, 21, 21, &person(), &command)
                .unwrap()
                .changed
        );

        let mut too_many_chars = command.clone();
        let WorkspaceTaskCommand::QueueTask { instruction, .. } = &mut too_many_chars else {
            panic!("queue task variant changed")
        };
        instruction.push('x');
        let before = reopened.save();
        assert!(run(&mut reopened, 22, 22, &person(), &too_many_chars).is_err());
        assert_eq!(reopened.save(), before);
    }

    #[test]
    fn required_instruction_cannot_silently_fall_back_to_title() {
        let mut doc = AutoCommit::new();
        let queued = run(
            &mut doc,
            31,
            31,
            &person(),
            &WorkspaceTaskCommand::QueueTask {
                title: "Short display title".into(),
                instruction: "Complete exact private instruction".into(),
                assigned_agent: "scribe".into(),
                route: WorkspaceTaskRoute::Model,
                resource: None,
            },
        )
        .unwrap();
        let WorkspaceTaskMutationResult::Queued { task_id } = queued.result else {
            panic!("queue result")
        };
        let mut controls = controls_of(&doc).unwrap();
        controls.get_mut(&task_id).unwrap().instruction = None;
        project_controls(&mut doc, &controls).unwrap();
        let before = doc.save();
        assert!(
            run(
                &mut doc,
                32,
                32,
                &host(),
                &WorkspaceTaskCommand::StartRun {
                    task_id,
                    agent_id: "scribe".into(),
                },
            )
            .is_err()
        );
        assert_eq!(doc.save(), before);
    }

    #[test]
    fn steering_requires_the_observed_run_and_instruction_frontier() {
        let mut doc = AutoCommit::new();
        let queued = run(&mut doc, 1, 1, &person(), &queue_command()).unwrap();
        let WorkspaceTaskMutationResult::Queued { task_id } = queued.result else {
            panic!("queue result")
        };
        run(
            &mut doc,
            2,
            2,
            &host(),
            &WorkspaceTaskCommand::StartRun {
                task_id: task_id.clone(),
                agent_id: "scribe".into(),
            },
        )
        .unwrap();
        let direction = |epoch, revision| WorkspaceTaskCommand::DirectionForRun {
            task_id: task_id.clone(),
            run_epoch: epoch,
            after_revision: revision,
            text: "Focus on the compatibility changes".into(),
        };
        assert_eq!(postcard::to_allocvec(&direction(1, 0)).unwrap()[0], 6);
        assert_eq!(
            postcard::to_allocvec(&WorkspaceTaskCommand::Direction {
                task_id: task_id.clone(),
                text: "Earlier command".into(),
            })
            .unwrap()[0],
            2
        );
        let before = doc.save();
        assert!(run(&mut doc, 3, 3, &person(), &direction(2, 0)).is_err());
        assert_eq!(doc.save(), before);
        let saved = run(&mut doc, 4, 4, &person(), &direction(1, 0)).unwrap();
        assert_eq!(
            saved.result,
            WorkspaceTaskMutationResult::DirectionAdded {
                task_id: task_id.clone(),
                revision: 1,
            }
        );
        let after = doc.save();
        assert!(run(&mut doc, 5, 5, &person(), &direction(1, 0)).is_err());
        assert_eq!(doc.save(), after);
        let mut reopened = AutoCommit::load(&after).unwrap();
        run(&mut reopened, 6, 6, &person(), &direction(1, 1)).unwrap();
        let state = snapshot(&reopened).unwrap();
        let control = state.controls.get(&task_id).unwrap();
        assert_eq!(control.directions.len(), 2);
        assert_eq!(control.directions[1].revision, 2);
        assert_eq!(
            control
                .active_run
                .as_ref()
                .unwrap()
                .applied_direction_revision,
            0
        );
        assert!(
            !run(&mut reopened, 4, 4, &person(), &direction(1, 0))
                .unwrap()
                .changed
        );
    }

    #[test]
    fn cancellation_is_requested_for_an_exact_run_until_agent_acknowledges() {
        let mut doc = AutoCommit::new();
        let queued = run(&mut doc, 41, 41, &person(), &queue_command()).unwrap();
        let WorkspaceTaskMutationResult::Queued { task_id } = queued.result else {
            panic!("queue result")
        };
        run(
            &mut doc,
            42,
            42,
            &host(),
            &WorkspaceTaskCommand::StartRun {
                task_id: task_id.clone(),
                agent_id: "scribe".into(),
            },
        )
        .unwrap();
        let request = WorkspaceTaskCommand::CancelRun {
            task_id: task_id.clone(),
            run_epoch: 1,
            after_revision: 0,
        };
        assert_eq!(postcard::to_allocvec(&request).unwrap()[0], 15);
        let result = run(&mut doc, 43, 43, &person(), &request).unwrap();
        assert_eq!(
            result.result,
            WorkspaceTaskMutationResult::CancellationRequested {
                task_id: task_id.clone(),
                epoch: 1,
            }
        );
        let state = snapshot(&doc).unwrap();
        assert_eq!(state.tasks[0].status, WorkspaceTaskStatus::InProgress);
        assert_eq!(
            state.controls[&task_id]
                .cancellation
                .as_ref()
                .map(|request| request.run_epoch),
            Some(1)
        );
        assert_cancelled_run_rejects_other_terminal_reports(&mut doc, &task_id);
        let before = doc.save();
        assert!(
            run(
                &mut doc,
                44,
                44,
                &person(),
                &WorkspaceTaskCommand::CancelRun {
                    task_id: task_id.clone(),
                    run_epoch: 2,
                    after_revision: 0,
                },
            )
            .is_err()
        );
        assert_eq!(doc.save(), before);
        run(
            &mut doc,
            45,
            45,
            &agent(),
            &WorkspaceTaskCommand::Finish {
                task_id,
                run_epoch: 1,
                outcome: WorkspaceTaskFinish::Cancelled,
                summary: "Stopped at the person's request".into(),
            },
        )
        .unwrap();
        assert_eq!(
            snapshot(&doc).unwrap().tasks[0].status,
            WorkspaceTaskStatus::Cancelled
        );
    }

    fn assert_cancelled_run_rejects_other_terminal_reports(doc: &mut AutoCommit, task_id: &str) {
        let before_terminal = doc.save();
        assert!(
            run(
                doc,
                44,
                44,
                &agent(),
                &WorkspaceTaskCommand::Finish {
                    task_id: task_id.to_owned(),
                    run_epoch: 1,
                    outcome: WorkspaceTaskFinish::Done,
                    summary: "Finished after the stop request".into(),
                },
            )
            .is_err()
        );
        assert!(
            run(
                doc,
                44,
                44,
                &host(),
                &WorkspaceTaskCommand::Abandon {
                    task_id: task_id.to_owned(),
                    run_epoch: 1,
                    reason: "Failed after the stop request".into(),
                },
            )
            .is_err()
        );
        assert_eq!(doc.save(), before_terminal);
    }

    #[test]
    fn direction_fences_finish_until_the_exact_run_acknowledges_it() {
        let mut doc = AutoCommit::new();
        let queued = run(&mut doc, 1, 1, &person(), &queue_command()).unwrap();
        let WorkspaceTaskMutationResult::Queued { task_id } = queued.result else {
            panic!("queue result")
        };
        let started = run(
            &mut doc,
            2,
            2,
            &host(),
            &WorkspaceTaskCommand::StartRun {
                task_id: task_id.clone(),
                agent_id: "scribe".into(),
            },
        )
        .unwrap();
        assert_eq!(
            started.result,
            WorkspaceTaskMutationResult::RunStarted {
                task_id: task_id.clone(),
                epoch: 1
            }
        );
        run(
            &mut doc,
            3,
            3,
            &person(),
            &WorkspaceTaskCommand::Direction {
                task_id: task_id.clone(),
                text: "Keep the migration warning prominent".into(),
            },
        )
        .unwrap();
        let finish = WorkspaceTaskCommand::Finish {
            task_id: task_id.clone(),
            run_epoch: 1,
            outcome: WorkspaceTaskFinish::Done,
            summary: "Prepared the release notes".into(),
        };
        assert!(run(&mut doc, 4, 4, &agent(), &finish).is_err());
        assert!(
            run(
                &mut doc,
                5,
                5,
                &agent(),
                &WorkspaceTaskCommand::AppliedAck {
                    task_id: task_id.clone(),
                    run_epoch: 2,
                    revision: 1,
                },
            )
            .is_err()
        );
        run(
            &mut doc,
            6,
            6,
            &agent(),
            &WorkspaceTaskCommand::AppliedAck {
                task_id: task_id.clone(),
                run_epoch: 1,
                revision: 1,
            },
        )
        .unwrap();
        run(&mut doc, 7, 7, &agent(), &finish).unwrap();
        let state = snapshot(&doc).unwrap();
        assert_eq!(state.tasks[0].status, WorkspaceTaskStatus::Done);
        assert!(state.controls[&task_id].active_run.is_none());
        assert_eq!(
            state.controls[&task_id].directions[0]
                .applied
                .as_ref()
                .map(|ack| ack.run_epoch),
            Some(1)
        );
        assert!(
            run(
                &mut doc,
                8,
                8,
                &person(),
                &WorkspaceTaskCommand::Direction {
                    task_id,
                    text: "Too late".into(),
                },
            )
            .is_err()
        );
    }

    #[test]
    fn abandoned_run_waits_for_an_explicit_person_retry() {
        let (mut doc, task_id) = task_with_applied_direction();
        run(
            &mut doc,
            5,
            5,
            &host(),
            &WorkspaceTaskCommand::Abandon {
                task_id: task_id.clone(),
                run_epoch: 1,
                reason: "Agent session stopped".into(),
            },
        )
        .unwrap();
        let waiting = snapshot(&doc).unwrap();
        assert_eq!(waiting.tasks[0].status, WorkspaceTaskStatus::Waiting);
        assert!(waiting.controls[&task_id].active_run.is_none());
        assert_eq!(waiting.controls[&task_id].directions.len(), 1);
        assert!(waiting.controls[&task_id].directions[0].applied.is_some());
        assert!(
            run(
                &mut doc,
                6,
                6,
                &person(),
                &WorkspaceTaskCommand::Retry {
                    task_id: task_id.clone(),
                    run_epoch: 2,
                },
            )
            .is_err()
        );
        assert!(
            run(
                &mut doc,
                7,
                7,
                &host(),
                &WorkspaceTaskCommand::Retry {
                    task_id: task_id.clone(),
                    run_epoch: 1,
                },
            )
            .is_err()
        );
        assert_eq!(
            postcard::to_allocvec(&WorkspaceTaskCommand::Retry {
                task_id: task_id.clone(),
                run_epoch: 1,
            })
            .unwrap()[0],
            7
        );
        run(
            &mut doc,
            8,
            8,
            &person(),
            &WorkspaceTaskCommand::Retry {
                task_id: task_id.clone(),
                run_epoch: 1,
            },
        )
        .unwrap();
        let retried = snapshot(&doc).unwrap();
        assert_eq!(retried.tasks[0].status, WorkspaceTaskStatus::Queued);
        assert!(retried.controls[&task_id].active_run.is_none());
        assert_eq!(retried.controls[&task_id].directions.len(), 1);
        run(
            &mut doc,
            9,
            9,
            &host(),
            &WorkspaceTaskCommand::StartRun {
                task_id: task_id.clone(),
                agent_id: "scribe".into(),
            },
        )
        .unwrap();
        assert_eq!(
            snapshot(&doc).unwrap().controls[&task_id]
                .active_run
                .as_ref()
                .map(|run| run.epoch),
            Some(2)
        );
    }
}
