//! A run paused at an action its person is asked about first.
//!
//! The host records the request ([`request`]) when the agent's action asks
//! first; the run stays in progress. The person answers it ([`answer`]):
//! confirm, with their changes to the draft, or decline, with a reason. The
//! host settles it ([`settle`]) once the answer reached the run: the
//! confirmed action ran, or the agent was told it was declined.

use tilo_desktop_model::{
    WorkspaceTask, WorkspaceTaskAuditEvent, WorkspaceTaskConfirmation,
    WorkspaceTaskConfirmationAnswer, WorkspaceTaskConfirmationState, WorkspaceTaskControl,
    WorkspaceTaskMutationResult, WorkspaceTaskStatus,
};

use super::{AdmittedTaskActor, CommandContext, WorkspaceTaskCommand, task_mut};

/// Largest serialized draft a confirmation keeps.
const MAX_DRAFT_BYTES: usize = 256 * 1024;
/// Longest reason a person gives for declining.
const MAX_REASON_BYTES: usize = 2 * 1024;

type Controls = std::collections::BTreeMap<String, WorkspaceTaskControl>;

fn recorded(task_id: &str) -> (String, WorkspaceTaskMutationResult) {
    (
        task_id.to_owned(),
        WorkspaceTaskMutationResult::ConfirmationRecorded {
            task_id: task_id.to_owned(),
        },
    )
}

/// The active run `run_epoch` of `task_id`, assigned to `agent`, with its
/// task and control.
fn active<'a>(
    tasks: &'a mut [WorkspaceTask],
    controls: &'a mut Controls,
    task_id: &str,
    run_epoch: u64,
) -> Result<(&'a mut WorkspaceTask, &'a mut WorkspaceTaskControl), String> {
    let task = task_mut(tasks, task_id)?;
    let control = controls
        .get_mut(task_id)
        .ok_or("task has no execution control")?;
    let live = control
        .active_run
        .as_ref()
        .is_some_and(|run| run.epoch == run_epoch && control.run_epoch == run_epoch);
    if !live || task.status != WorkspaceTaskStatus::InProgress {
        return Err("task run ended or was replaced".into());
    }
    Ok((task, control))
}

pub(super) fn dispatch(
    (tasks, controls): (&mut [WorkspaceTask], &mut Controls),
    context: &CommandContext<'_>,
    command: &WorkspaceTaskCommand,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    match command {
        WorkspaceTaskCommand::RequestConfirmation {
            task_id,
            run_epoch,
            confirmation,
        } => request(
            (tasks, controls),
            context,
            (task_id, *run_epoch),
            confirmation,
        ),
        WorkspaceTaskCommand::AnswerConfirmation {
            task_id,
            run_epoch,
            confirmation_id,
            answer: given,
        } => answer(
            (tasks, controls),
            context,
            (task_id, *run_epoch, confirmation_id),
            given,
        ),
        WorkspaceTaskCommand::SettleConfirmation {
            task_id,
            run_epoch,
            confirmation_id,
        } => settle(
            (tasks, controls),
            context,
            (task_id, *run_epoch, confirmation_id),
        ),
        _ => Err("not a task confirmation command".into()),
    }
}

fn request(
    (tasks, controls): (&mut [WorkspaceTask], &mut Controls),
    context: &CommandContext<'_>,
    (task_id, run_epoch): (&str, u64),
    confirmation: &WorkspaceTaskConfirmation,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let actor = match context.actor {
        AdmittedTaskActor::Agent { id, .. } if *id == confirmation.agent_id => {
            format!("agent:{id}")
        }
        AdmittedTaskActor::Host { .. } => format!("agent:{}", confirmation.agent_id),
        _ => return Err("only the run's host or agent asks for a confirmation".into()),
    };
    let (task, control) = active(tasks, controls, task_id, run_epoch)?;
    let assigned = control
        .active_run
        .as_ref()
        .is_some_and(|run| run.agent_id == confirmation.agent_id)
        && task.assigned_agent.as_deref() == Some(confirmation.agent_id.as_str());
    if !assigned
        || confirmation.run_epoch != run_epoch
        || !confirmation.is_waiting()
        || confirmation.id
            != WorkspaceTaskConfirmation::id_for(
                &confirmation.resource,
                &confirmation.action,
                &confirmation.arguments_digest,
            )
    {
        return Err("task confirmation does not name this run's request".into());
    }
    let size = serde_json::to_vec(&confirmation.draft)
        .map_err(|error| error.to_string())?
        .len();
    if size > MAX_DRAFT_BYTES {
        return Err(format!(
            "task confirmation draft exceeds {MAX_DRAFT_BYTES} bytes"
        ));
    }
    // Asking again for the same draft keeps the answer it already has.
    if control
        .confirmation
        .as_ref()
        .is_some_and(|held| held.id == confirmation.id)
    {
        return Ok(recorded(task_id));
    }
    control.confirmation = Some(confirmation.clone());
    task.updated_at = context.now;
    task.audit.push(WorkspaceTaskAuditEvent::new(
        context.now,
        actor,
        "ask-confirmation",
        [(
            "action",
            tilo_actions::Scalar::text(&confirmation.draft.action_label),
        )],
    ));
    Ok(recorded(task_id))
}

fn answer(
    (tasks, controls): (&mut [WorkspaceTask], &mut Controls),
    context: &CommandContext<'_>,
    (task_id, run_epoch, confirmation_id): (&str, u64, &str),
    answer: &WorkspaceTaskConfirmationAnswer,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    let AdmittedTaskActor::Person { id: person, .. } = context.actor else {
        return Err("only a person answers a task confirmation".into());
    };
    let (task, control) = active(tasks, controls, task_id, run_epoch)?;
    // The agent acts for the person who approved its run: they answer.
    if control
        .active_run
        .as_ref()
        .and_then(|run| run.approval.as_ref())
        .is_some_and(|approval| approval.person != *person)
    {
        return Err("only the person the run acts for answers its confirmation".into());
    }
    let confirmation = control
        .confirmation
        .as_mut()
        .filter(|held| held.id == confirmation_id && held.run_epoch == run_epoch)
        .ok_or("task confirmation was replaced; review the current draft")?;
    if !confirmation.is_waiting() {
        return Err("task confirmation was already answered".into());
    }
    let (verb, note) = match answer {
        WorkspaceTaskConfirmationAnswer::Confirm { edits } => {
            confirmation.draft.check_edits(edits)?;
            confirmation.state = WorkspaceTaskConfirmationState::Confirmed {
                person: person.clone(),
                at: context.now,
                edits: edits.clone(),
            };
            ("confirm-action", None)
        }
        WorkspaceTaskConfirmationAnswer::Decline { reason } => {
            let reason = reason
                .as_deref()
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
                .map(str::to_owned);
            if reason
                .as_ref()
                .is_some_and(|reason| reason.len() > MAX_REASON_BYTES)
            {
                return Err(format!("decline reason exceeds {MAX_REASON_BYTES} bytes"));
            }
            confirmation.state = WorkspaceTaskConfirmationState::Declined {
                person: person.clone(),
                at: context.now,
                reason: reason.clone(),
            };
            ("decline-action", reason)
        }
    };
    let label = confirmation.draft.action_label.clone();
    task.updated_at = context.now;
    let mut event = WorkspaceTaskAuditEvent::new(
        context.now,
        person.clone(),
        verb,
        [("action", tilo_actions::Scalar::text(&label))],
    );
    event.note = note;
    task.audit.push(event);
    Ok(recorded(task_id))
}

fn settle(
    (tasks, controls): (&mut [WorkspaceTask], &mut Controls),
    context: &CommandContext<'_>,
    (task_id, run_epoch, confirmation_id): (&str, u64, &str),
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    if matches!(context.actor, AdmittedTaskActor::Person { .. }) {
        return Err("only the run's host or agent settles a confirmation".into());
    }
    let (_, control) = active(tasks, controls, task_id, run_epoch)?;
    if control
        .confirmation
        .as_ref()
        .is_some_and(|held| held.id == confirmation_id && !held.is_waiting())
    {
        control.confirmation = None;
    }
    Ok(recorded(task_id))
}

/// Drop every confirmation whose run is no longer the task's active run:
/// a run that ended takes its pause with it.
pub(super) fn lapse(controls: &mut Controls) {
    for control in controls.values_mut() {
        let live = control.active_run.as_ref().map(|run| run.epoch);
        if control
            .confirmation
            .as_ref()
            .is_some_and(|held| Some(held.run_epoch) != live)
        {
            control.confirmation = None;
        }
    }
}

#[cfg(test)]
#[path = "confirmation_tests.rs"]
mod tests;
