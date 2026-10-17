//! Typed, replay-safe mutations of the room-owned Task Center document.
//!
//! These are owner commands, not a second task store and not caller-authored
//! CRDT bytes. The native owner authenticates the authority before this pure
//! reduction changes `tasks_v1` and its execution-control sidecar together.

use automerge::{
    AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable,
};
use serde::{Deserialize, Serialize};
mod confirmation;

use tilo_desktop_model::{
    MAX_WORKSPACE_TASK_DIRECTION_BYTES, MAX_WORKSPACE_TASK_INSTRUCTION_BYTES,
    MAX_WORKSPACE_TASK_SUMMARY_BYTES, WorkspaceTask, WorkspaceTaskActiveRun,
    WorkspaceTaskAuditEvent, WorkspaceTaskControl, WorkspaceTaskDirection,
    WorkspaceTaskDirectionApplied, WorkspaceTaskExecution, WorkspaceTaskExecutionPhase,
    WorkspaceTaskMutationReceipt, WorkspaceTaskMutationResult, WorkspaceTaskResource,
    WorkspaceTaskRoute, WorkspaceTaskStatus,
};

pub const TASK_CONTROL_KEY: &str = "task_control_v1";
const MAX_TASK_TITLE_CHARS: usize = 240;
const MAX_TASK_RECEIPTS: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceTaskSnapshot {
    pub tasks: Vec<WorkspaceTask>,
    pub controls: std::collections::BTreeMap<String, WorkspaceTaskControl>,
    /// The open tasks' grant sets: grants whose subject is a task.
    pub grants: Vec<tilo_action_policy::Grant>,
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
        grants: tilo_desktop_model::projection::task_grants_of(doc),
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
    /// Appended to retain every existing encoded command variant index.
    CancelForRun {
        task_id: String,
        run_epoch: u64,
        after_revision: u64,
    },
    /// Person-created coordination work; never mints an agent run lease.
    /// Appended to preserve every existing postcard command variant index.
    CreateManual {
        title: String,
        resource: Option<Box<WorkspaceTaskResource>>,
    },
    /// Replace one attachment only against the person's observed attachment.
    /// Manual coordination cannot alter a managed task or active run.
    AttachResource {
        task_id: String,
        expected_resource_id: Option<String>,
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
    /// Appended after the parent coordination commands to preserve postcard indices.
    ApproveRun {
        task_id: String,
        agent_id: String,
        expected_run_epoch: u64,
        after_revision: u64,
    },
    /// Queue a private full instruction while keeping the public task title bounded.
    /// Appended to preserve every existing postcard command variant index.
    QueueInstruction {
        title: String,
        instruction: String,
        assigned_agent: String,
        route: WorkspaceTaskRoute,
    },
    /// Public agent-reported status/action text, never model reasoning.
    /// Appended to preserve existing postcard indices.
    PublicProgress {
        task_id: String,
        run_epoch: u64,
        sequence: u64,
        message: String,
    },
    /// [`Self::QueueInstruction`] about what the person had selected: the
    /// request's subject, never an attachment, so the run's authority is a
    /// room-wide task's. Appended to preserve existing postcard indices.
    QueueInstructionAbout {
        title: String,
        instruction: String,
        assigned_agent: String,
        route: WorkspaceTaskRoute,
        subject: Box<tilo_actions::SelectionSubject>,
    },
    /// The run's host pauses the run at an action its person is asked
    /// about first. Appended to preserve existing postcard indices.
    RequestConfirmation {
        task_id: String,
        run_epoch: u64,
        confirmation: Box<tilo_desktop_model::WorkspaceTaskConfirmation>,
    },
    /// The person the run acts for answers the action it paused at.
    /// Appended to preserve existing postcard indices.
    AnswerConfirmation {
        task_id: String,
        run_epoch: u64,
        confirmation_id: String,
        answer: tilo_desktop_model::WorkspaceTaskConfirmationAnswer,
    },
    /// The run's host took the answer to the run. Appended to preserve
    /// existing postcard indices.
    SettleConfirmation {
        task_id: String,
        run_epoch: u64,
        confirmation_id: String,
    },
}

impl WorkspaceTaskCommand {
    #[must_use]
    pub fn agent(&self) -> Option<&str> {
        match self {
            Self::Queue { assigned_agent, .. }
            | Self::QueueInstruction { assigned_agent, .. }
            | Self::QueueInstructionAbout { assigned_agent, .. }
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
    confirmation::lapse(&mut controls);
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
    // A task that closed takes the grants given for it with it, in the same
    // change, so every reader of the room's grants stops honouring them.
    tilo_desktop_model::projection::lapse_task_grants(doc).map_err(|error| error.to_string())?;
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
        command @ (WorkspaceTaskCommand::Queue { .. }
        | WorkspaceTaskCommand::QueueInstruction { .. }
        | WorkspaceTaskCommand::QueueInstructionAbout { .. }) => {
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
        WorkspaceTaskCommand::AppliedAck { .. }
        | WorkspaceTaskCommand::ExecutionPhase { .. }
        | WorkspaceTaskCommand::PublicProgress { .. } => {
            dispatch_agent_report(tasks, controls, context, command)
        }
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
        command @ (WorkspaceTaskCommand::RequestConfirmation { .. }
        | WorkspaceTaskCommand::AnswerConfirmation { .. }
        | WorkspaceTaskCommand::SettleConfirmation { .. }) => {
            confirmation::dispatch((tasks, controls), context, command)
        }
    }
}

fn dispatch_queue(
    tasks: &mut Vec<WorkspaceTask>,
    controls: &mut std::collections::BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    command: &WorkspaceTaskCommand,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    match command {
        WorkspaceTaskCommand::Queue {
            title,
            assigned_agent,
            route,
            resource,
        } => queue(
            tasks,
            controls,
            context,
            title,
            assigned_agent,
            *route,
            resource.as_deref().cloned(),
            None,
        ),
        WorkspaceTaskCommand::QueueInstruction {
            title,
            instruction,
            assigned_agent,
            route,
        } => queue(
            tasks,
            controls,
            context,
            title,
            assigned_agent,
            *route,
            None,
            Some(instruction.as_str()),
        ),
        WorkspaceTaskCommand::QueueInstructionAbout {
            title,
            instruction,
            assigned_agent,
            route,
            subject,
        } => {
            validate_subject(subject)?;
            let queued = queue(
                tasks,
                controls,
                context,
                title,
                assigned_agent,
                *route,
                None,
                Some(instruction.as_str()),
            )?;
            task_mut(tasks, &queued.0)?.subject = Some(subject.as_ref().clone());
            Ok(queued)
        }
        _ => Err("not a queue command".into()),
    }
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
        WorkspaceTaskCommand::PublicProgress {
            task_id,
            run_epoch,
            sequence,
            message,
        } => {
            bounded(
                message,
                tilo_desktop_model::task::MAX_WORKSPACE_TASK_PROGRESS_BYTES,
                "public task progress",
            )?;
            let task = tasks
                .iter()
                .find(|task| task.id == *task_id)
                .ok_or("task is absent")?;
            if task
                .audit
                .iter()
                .filter(|event| event.action() == "public-progress" && event.for_run(*run_epoch))
                .count()
                >= tilo_desktop_model::task::MAX_WORKSPACE_TASK_PUBLIC_UPDATES
            {
                return Err("public task progress limit reached".into());
            }
            let result = report_execution(
                tasks,
                controls,
                context,
                task_id,
                (*run_epoch, *sequence),
                WorkspaceTaskExecutionPhase::Executing,
            )?;
            let AdmittedTaskActor::Agent { id, .. } = context.actor else {
                return Err("only an agent may report public task progress".into());
            };
            task_mut(tasks, task_id)?.audit.push(
                WorkspaceTaskAuditEvent::new(
                    context.now,
                    format!("agent:{id}"),
                    "public-progress",
                    [
                        (
                            "run_epoch",
                            tilo_actions::Scalar::text(&run_epoch.to_string()),
                        ),
                        (
                            "sequence",
                            tilo_actions::Scalar::text(&sequence.to_string()),
                        ),
                    ],
                )
                .noting(message),
            );
            Ok(result)
        }
        _ => Err("command is not an agent report".into()),
    }
}

fn dispatch_retry(
    tasks: &mut [WorkspaceTask],
    controls: &std::collections::BTreeMap<String, WorkspaceTaskControl>,
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

/// A request's subject names an ordinary application resource, as an
/// attachment must, and a bounded selection in it.
fn validate_subject(subject: &tilo_actions::SelectionSubject) -> Result<(), String> {
    subject.validate()?;
    validate_manual_resource(&WorkspaceTaskResource {
        app_id: Some(subject.app_id.clone()),
        app_label: Some(subject.app_label.clone()),
        id: subject.resource.id.clone(),
        kind: subject.resource.kind.clone(),
        label: subject.resource.label.clone(),
        agent_request: None,
    })
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
                subject: None,
                created_at: context.now,
                updated_at: context.now,
                audit: vec![WorkspaceTaskAuditEvent::new(
                    context.now,
                    author.clone(),
                    "create-task",
                    [
                        ("title", tilo_actions::Scalar::text(title)),
                        ("manual", tilo_actions::Scalar::from(true)),
                    ],
                )],
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
            task.audit.push(WorkspaceTaskAuditEvent::new(
                context.now,
                author.clone(),
                "attach-resource",
                [("resource", tilo_actions::Scalar::text(&resource.label))],
            ));
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
    task.audit.push(WorkspaceTaskAuditEvent::new(
        context.now,
        actor.clone(),
        "set-execution-budget",
        [],
    ));
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
    assigned_agent: &str,
    route: WorkspaceTaskRoute,
    resource: Option<WorkspaceTaskResource>,
    instruction: Option<&str>,
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
        if instruction.trim().is_empty() {
            return Err("task instruction is empty".into());
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
    let instruction = instruction.map(str::to_owned).or_else(|| {
        resource
            .as_ref()
            .and_then(|value| value.agent_request.as_ref())
            .map(|request| request.instruction.clone())
    });
    tasks.push(WorkspaceTask {
        id: task_id.clone(),
        title: title.to_owned(),
        created_by: author.clone(),
        assigned_agent: Some(assigned_agent.to_owned()),
        route,
        status: WorkspaceTaskStatus::Queued,
        resource,
        subject: None,
        created_at: context.now,
        updated_at: context.now,
        audit: vec![WorkspaceTaskAuditEvent::new(
            context.now,
            author.clone(),
            "create-task",
            [("title", tilo_actions::Scalar::text(title))],
        )],
    });
    controls.insert(
        task_id.clone(),
        WorkspaceTaskControl {
            instruction,
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
    if control.active_run.is_some() {
        return Err("task already has an active run".into());
    }
    control.run_epoch = control
        .run_epoch
        .checked_add(1)
        .ok_or("task run epoch exhausted")?;
    let epoch = control.run_epoch;
    let (actor, verb) = approval.as_ref().map_or_else(
        || (format!("agent:{agent_id}"), "start-run"),
        |value| (value.person.clone(), "approve-run"),
    );
    control.active_run = Some(WorkspaceTaskActiveRun {
        epoch,
        agent_id: agent_id.to_owned(),
        started_at: now,
        applied_direction_revision: 0,
        approval,
    });
    control.execution = None;
    task.status = WorkspaceTaskStatus::InProgress;
    task.updated_at = now;
    task.audit.push(WorkspaceTaskAuditEvent::new(
        now,
        actor,
        verb,
        [(
            "run",
            tilo_actions::Scalar::of(&epoch).unwrap_or_else(|| tilo_actions::Scalar::from(0)),
        )],
    ));
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
    task.audit.push(WorkspaceTaskAuditEvent::new(
        now,
        author.clone(),
        "add-direction",
        [(
            "revision",
            tilo_actions::Scalar::of(&revision).unwrap_or_else(|| tilo_actions::Scalar::from(0)),
        )],
    ));
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
    task.audit.push(WorkspaceTaskAuditEvent::new(
        now,
        format!("agent:{agent_id}"),
        "apply-direction",
        [(
            "revision",
            tilo_actions::Scalar::of(&revision).unwrap_or_else(|| tilo_actions::Scalar::from(0)),
        )],
    ));
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
    if task.status != WorkspaceTaskStatus::InProgress
        || task.assigned_agent.as_deref() != Some(agent_id)
        || run.agent_id != *agent_id
        || run.epoch != run_epoch
        || run.applied_direction_revision != latest
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
    task.audit.push(
        WorkspaceTaskAuditEvent::new(
            now,
            format!("agent:{agent_id}"),
            "finish",
            [
                ("outcome", tilo_actions::Scalar::from(task.status.key())),
                (
                    "run_epoch",
                    tilo_actions::Scalar::text(&run_epoch.to_string()),
                ),
            ],
        )
        .noting(summary),
    );
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
    {
        return Err("task abandonment names a stale run".into());
    }
    control.active_run = None;
    control.execution = None;
    task.status = WorkspaceTaskStatus::Waiting;
    task.updated_at = now;
    task.audit.push(
        WorkspaceTaskAuditEvent::new(
            now,
            "host",
            "abandon",
            [(
                "run_epoch",
                tilo_actions::Scalar::text(&run_epoch.to_string()),
            )],
        )
        .noting(reason),
    );
    Ok((
        task_id.to_owned(),
        WorkspaceTaskMutationResult::Abandoned {
            task_id: task_id.to_owned(),
        },
    ))
}

fn retry(
    tasks: &mut [WorkspaceTask],
    controls: &std::collections::BTreeMap<String, WorkspaceTaskControl>,
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
    task.status = WorkspaceTaskStatus::Queued;
    task.updated_at = now;
    task.audit.push(WorkspaceTaskAuditEvent::new(
        now,
        author.clone(),
        "retry",
        [],
    ));
    Ok((
        task_id.to_owned(),
        WorkspaceTaskMutationResult::Queued {
            task_id: task_id.to_owned(),
        },
    ))
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
    task.audit.push(WorkspaceTaskAuditEvent::new(
        context.now,
        author.clone(),
        "cancel",
        [],
    ));
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

pub(crate) fn project_controls(
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
    fn public_progress_is_durable_idempotent_and_stopped_runs_reject_it() {
        let (mut doc, task) = task_with_applied_direction();
        run(
            &mut doc,
            5,
            5,
            &agent(),
            &phase_command(&task, 1, 0, WorkspaceTaskExecutionPhase::Accepted),
        )
        .unwrap();
        let progress = WorkspaceTaskCommand::PublicProgress {
            task_id: task.clone(),
            run_epoch: 1,
            sequence: 1,
            message: "Updated the selected document through its append action.".into(),
        };
        assert!(run(&mut doc, 6, 6, &person(), &progress).is_err());
        let wrong_epoch = WorkspaceTaskCommand::PublicProgress {
            task_id: task.clone(),
            run_epoch: 2,
            sequence: 1,
            message: "Wrong run".into(),
        };
        assert!(run(&mut doc, 6, 6, &agent(), &wrong_epoch).is_err());
        run(&mut doc, 6, 6, &agent(), &progress).unwrap();
        let mut doc = AutoCommit::load(&doc.save()).unwrap();
        assert!(!run(&mut doc, 6, 6, &agent(), &progress).unwrap().changed);
        let state = snapshot(&doc).unwrap();
        let notes = state.tasks[0]
            .audit
            .iter()
            .filter(|event| event.action() == "public-progress")
            .collect::<Vec<_>>();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].for_run(1));
        assert_eq!(
            notes[0].note.as_deref(),
            Some("Updated the selected document through its append action.")
        );
        assert_eq!(
            state.controls[&task].execution.as_ref().unwrap().sequence,
            1
        );
        run(
            &mut doc,
            7,
            7,
            &person(),
            &WorkspaceTaskCommand::CancelForRun {
                task_id: task.clone(),
                run_epoch: 1,
                after_revision: 1,
            },
        )
        .unwrap();
        assert!(
            run(
                &mut doc,
                8,
                8,
                &agent(),
                &WorkspaceTaskCommand::PublicProgress {
                    task_id: task,
                    run_epoch: 1,
                    sequence: 2,
                    message: "Too late".into(),
                }
            )
            .is_err()
        );
    }

    #[test]
    fn execution_phase_distinguishes_lease_from_acceptance_and_replays_after_reopen() {
        let (mut doc, task) = task_with_applied_direction();
        assert!(snapshot(&doc).unwrap().controls[&task].execution.is_none());
        let command = phase_command(&task, 1, 0, WorkspaceTaskExecutionPhase::Accepted);
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
    fn stopping_requires_the_exact_task_run_and_direction_frontier() {
        let (mut doc, task_id) = task_with_applied_direction();
        let stop = |task: &str, epoch, revision| WorkspaceTaskCommand::CancelForRun {
            task_id: task.into(),
            run_epoch: epoch,
            after_revision: revision,
        };
        let before = doc.save();
        for command in [
            stop(&task_id, 2, 1),
            stop(&task_id, 1, 0),
            stop("task:other", 1, 1),
        ] {
            assert!(run(&mut doc, 90, 90, &person(), &command).is_err());
            assert_eq!(doc.save(), before);
        }
        run(&mut doc, 91, 91, &person(), &stop(&task_id, 1, 1)).unwrap();
        let current = snapshot(&doc).unwrap();
        assert_eq!(current.tasks[0].status, WorkspaceTaskStatus::Cancelled);
        assert!(current.controls[&task_id].active_run.is_none());
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

    #[test]
    fn queued_instruction_is_retained_privately_without_truncating_to_the_title() {
        let mut doc = AutoCommit::new();
        let instruction =
            "Inspect every section and preserve the complete private request. ".repeat(12);
        assert!(instruction.chars().count() > MAX_TASK_TITLE_CHARS);
        let applied = run(
            &mut doc,
            71,
            71,
            &person(),
            &WorkspaceTaskCommand::QueueInstruction {
                title: "Inspect the proposal".into(),
                instruction: instruction.clone(),
                assigned_agent: "scribe".into(),
                route: WorkspaceTaskRoute::Model,
            },
        )
        .unwrap();
        assert_eq!(
            postcard::to_allocvec(&WorkspaceTaskCommand::QueueInstruction {
                title: "Inspect the proposal".into(),
                instruction: instruction.clone(),
                assigned_agent: "scribe".into(),
                route: WorkspaceTaskRoute::Model,
            })
            .unwrap()[0],
            14,
            "the new command must remain appended after every established wire variant"
        );
        let WorkspaceTaskMutationResult::Queued { task_id } = applied.result else {
            panic!("queue result")
        };
        let current = snapshot(&doc).unwrap();
        assert_eq!(current.tasks[0].title, "Inspect the proposal");
        assert_eq!(
            current.controls[&task_id].instruction.as_deref(),
            Some(instruction.as_str())
        );

        let oversized = "x".repeat(MAX_WORKSPACE_TASK_INSTRUCTION_BYTES + 1);
        assert!(
            run(
                &mut doc,
                72,
                72,
                &person(),
                &WorkspaceTaskCommand::QueueInstruction {
                    title: "Too large".into(),
                    instruction: oversized,
                    assigned_agent: "scribe".into(),
                    route: WorkspaceTaskRoute::Model,
                },
            )
            .is_err()
        );
    }

    fn subject(anchor: tilo_actions::SelectionAnchor) -> tilo_actions::SelectionSubject {
        tilo_actions::SelectionSubject {
            app_id: "so.tilo.word-processor".into(),
            app_label: "Writer".into(),
            resource: tilo_actions::ActionResource {
                id: "doc:proposal".into(),
                kind: "word-processor.document".into(),
                label: "Proposal".into(),
            },
            anchor,
        }
    }

    fn queue_about(subject: tilo_actions::SelectionSubject) -> WorkspaceTaskCommand {
        WorkspaceTaskCommand::QueueInstructionAbout {
            title: "Tighten this".into(),
            instruction: "Tighten this sentence".into(),
            assigned_agent: "scribe".into(),
            route: WorkspaceTaskRoute::Model,
            subject: Box::new(subject),
        }
    }

    #[test]
    fn an_instruction_about_a_selection_keeps_it_as_the_subject_not_an_attachment() {
        let anchor = tilo_actions::SelectionAnchor::of_text("the opening sentence")
            .at(tilo_actions::SelectionPlace::range("block-2", 0, 20));
        let command = queue_about(subject(anchor.clone()));
        let encoded = postcard::to_allocvec(&command).unwrap();
        assert_eq!(
            encoded[0], 16,
            "the new command stays appended after every established wire variant"
        );
        let WorkspaceTaskCommand::QueueInstructionAbout { subject: read, .. } =
            postcard::from_bytes::<WorkspaceTaskCommand>(&encoded).unwrap()
        else {
            panic!("decoded another command")
        };
        assert_eq!(read.anchor, anchor);

        let mut doc = AutoCommit::new();
        let applied = run(&mut doc, 81, 81, &person(), &command).unwrap();
        let WorkspaceTaskMutationResult::Queued { task_id } = applied.result else {
            panic!("queue result")
        };
        let current = snapshot(&doc).unwrap();
        let task = current
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .unwrap();
        assert_eq!(
            task.subject.as_ref().map(|subject| &subject.anchor),
            Some(&anchor)
        );
        assert!(
            task.resource.is_none(),
            "a selection never becomes the scope-narrowing attachment"
        );
        assert_eq!(
            current.controls[&task_id].instruction.as_deref(),
            Some("Tighten this sentence")
        );
    }

    #[test]
    fn an_instruction_about_a_selection_refuses_carriers_and_oversized_anchors() {
        let oversized = subject(tilo_actions::SelectionAnchor {
            excerpt: Some("x".repeat(tilo_actions::MAX_EXCERPT_BYTES + 1)),
            ..tilo_actions::SelectionAnchor::default()
        });
        let mut doc = AutoCommit::new();
        assert!(run(&mut doc, 82, 82, &person(), &queue_about(oversized)).is_err());
        let mut carrier = subject(tilo_actions::SelectionAnchor::of_text("x"));
        carrier.resource.id = "file:///etc/passwd".into();
        assert!(run(&mut doc, 83, 83, &person(), &queue_about(carrier)).is_err());
        let ordinary = subject(tilo_actions::SelectionAnchor::of_text("x"));
        assert!(run(&mut doc, 84, 84, &agent(), &queue_about(ordinary)).is_err());
    }
}
