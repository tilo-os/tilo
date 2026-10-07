//! Metadata-only owner admissions; the broker retains private child provenance.
use super::{
    AdmittedTaskActor, CommandContext, WorkspaceTaskCommand, WorkspaceTaskControl,
    WorkspaceTaskMutationResult, WorkspaceTaskStatus,
};
use std::collections::{BTreeMap, HashSet};
use tilo_desktop_model::{WorkspaceTask, WorkspaceTaskStep, WorkspaceTaskStepState};

const MAX_STEPS_PER_RUN: u32 = 256;
const MAX_RETAINED_STEPS: usize = 1024;

pub(super) fn dispatch(
    tasks: &[WorkspaceTask],
    controls: &mut BTreeMap<String, WorkspaceTaskControl>,
    context: &CommandContext<'_>,
    command: &WorkspaceTaskCommand,
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    if !matches!(context.actor, AdmittedTaskActor::Host { .. }) {
        return Err("only the authenticated host may admit or settle a task step".into());
    }
    let (task_id, run_epoch, index, invocation, intent_digest) = match command {
        WorkspaceTaskCommand::PrepareStep {
            task_id,
            run_epoch,
            index,
            invocation,
            intent_digest,
        }
        | WorkspaceTaskCommand::CommitStep {
            task_id,
            run_epoch,
            index,
            invocation,
            intent_digest,
            ..
        } => (task_id, *run_epoch, *index, *invocation, *intent_digest),
        _ => return Err("not a task step command".into()),
    };
    if run_epoch == 0
        || index >= MAX_STEPS_PER_RUN
        || invocation.0.is_nil()
        || intent_digest == [0; 32]
    {
        return Err("invalid or oversized task step identity".into());
    }
    let task = tasks
        .iter()
        .find(|task| &task.id == task_id)
        .ok_or("task absent")?;
    let control = controls
        .get_mut(task_id)
        .ok_or("task execution control absent")?;
    validate(control)?;
    if control.run_epoch != run_epoch {
        return Err("task step belongs to another owner epoch".into());
    }
    match command {
        WorkspaceTaskCommand::PrepareStep { .. } => {
            prepare(task, control, index, invocation, intent_digest)
        }
        WorkspaceTaskCommand::CommitStep {
            outcome,
            output_digest,
            ..
        } => {
            if *output_digest == [0; 32] {
                return Err("task step output commitment absent".into());
            }
            let step = control
                .steps
                .iter_mut()
                .find(|step| step.run_epoch == run_epoch && step.index == index)
                .ok_or("task step was never admitted")?;
            if step.invocation != invocation || step.intent_digest != intent_digest {
                return Err("task step terminal does not bind the admitted child".into());
            }
            let state = WorkspaceTaskStepState::Committed {
                outcome: *outcome,
                output_digest: *output_digest,
            };
            if step.state != WorkspaceTaskStepState::Prepared && step.state != state {
                return Err("task step terminal commitment cannot be changed".into());
            }
            step.state = state;
            Ok((
                task_id.clone(),
                WorkspaceTaskMutationResult::StepCommitted {
                    task_id: task_id.clone(),
                    step: step.clone(),
                },
            ))
        }
        _ => Err("not a task step command".into()),
    }
}

fn prepare(
    task: &WorkspaceTask,
    control: &mut WorkspaceTaskControl,
    index: u32,
    invocation: tilo_actions::InvocationId,
    intent_digest: [u8; 32],
) -> Result<(String, WorkspaceTaskMutationResult), String> {
    if let Some(step) = control
        .steps
        .iter()
        .find(|step| step.run_epoch == control.run_epoch && step.index == index)
    {
        if step.invocation != invocation || step.intent_digest != intent_digest {
            return Err("task step index reused with changed intent".into());
        }
        return Ok((
            task.id.clone(),
            WorkspaceTaskMutationResult::StepPrepared {
                task_id: task.id.clone(),
                step: step.clone(),
            },
        ));
    }
    let active = control
        .active_run
        .as_ref()
        .ok_or("task step has no active run")?;
    let latest_direction = control
        .directions
        .last()
        .map_or(0, |direction| direction.revision);
    if task.status != WorkspaceTaskStatus::InProgress
        || active.epoch != control.run_epoch
        || task.assigned_agent.as_deref() != Some(&active.agent_id)
        || active.applied_direction_revision != latest_direction
    {
        return Err("task step is cancelled, inactive or has unapplied directions".into());
    }
    let previous = control
        .steps
        .iter()
        .filter(|step| step.run_epoch == control.run_epoch)
        .collect::<Vec<_>>();
    if previous.len() != index as usize
        || previous
            .iter()
            .any(|step| step.state == WorkspaceTaskStepState::Prepared)
        || control.steps.len() >= MAX_RETAINED_STEPS
        || control
            .steps
            .iter()
            .any(|step| step.invocation == invocation)
    {
        return Err(
            "task step is nonsequential, pending, duplicated or exceeds the retained bound".into(),
        );
    }
    let step = WorkspaceTaskStep {
        run_epoch: control.run_epoch,
        index,
        invocation,
        intent_digest,
        state: WorkspaceTaskStepState::Prepared,
    };
    control.steps.push(step.clone());
    Ok((
        task.id.clone(),
        WorkspaceTaskMutationResult::StepPrepared {
            task_id: task.id.clone(),
            step,
        },
    ))
}

pub(super) fn validate(control: &WorkspaceTaskControl) -> Result<(), String> {
    if control.steps.len() > MAX_RETAINED_STEPS {
        return Err("stored task step bound exceeded".into());
    }
    let mut positions = HashSet::new();
    let mut invocations = HashSet::new();
    let mut runs = BTreeMap::<u64, Vec<&WorkspaceTaskStep>>::new();
    for step in &control.steps {
        if step.run_epoch == 0
            || step.run_epoch > control.run_epoch
            || step.index >= MAX_STEPS_PER_RUN
            || step.invocation.0.is_nil()
            || step.intent_digest == [0; 32]
            || !positions.insert((step.run_epoch, step.index))
            || !invocations.insert(step.invocation)
            || matches!(step.state, WorkspaceTaskStepState::Committed { output_digest, .. } if output_digest == [0; 32])
        {
            return Err(
                "stored task step identity or commitment is malformed or duplicated".into(),
            );
        }
        runs.entry(step.run_epoch).or_default().push(step);
    }
    for steps in runs.values_mut() {
        steps.sort_by_key(|step| step.index);
        for (position, step) in steps.iter().enumerate() {
            if step.index as usize != position
                || (step.state == WorkspaceTaskStepState::Prepared
                    && position.checked_add(1) != Some(steps.len()))
            {
                return Err(
                    "stored task steps are nonsequential or contain an unsettled predecessor"
                        .into(),
                );
            }
        }
    }
    Ok(())
}
