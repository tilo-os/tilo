//! Accepted enrolled leases stay separate from the legacy task registry.
use super::{Context, Run, authority};
use crate::collab_broker_capnp::{
    AdmittedJoinIdentity, InvocationCaller, InvocationInputs, InvocationJob,
};
use launcher::managed::ManagedLaunchScope;
use tilo_desktop_model::WorkspaceTaskStatus;

pub(super) fn observed(handle: &Context, numeric: u64, lease: &str) -> Result<(Run, bool), String> {
    let (scope, connection) = handle.connection()?;
    authority(handle, &connection)?;
    let registry = handle
        .state
        .enrolled_tasks
        .lock()
        .map_err(|_| "enrolled registry unavailable")?;
    let mut matching = registry.runs.values().filter(|run| {
        run.scope == scope
            && run.envelope.task.run_id == numeric
            && run.envelope.task.lease == lease
    });
    let run = matching
        .next()
        .cloned()
        .ok_or("accepted enrolled lease not retained")?;
    if matching.next().is_some() {
        return Err("enrolled lease is ambiguous".into());
    }
    drop(registry);
    let accepted = run
        .acceptance
        .as_ref()
        .ok_or("enrolled lease has not been accepted")?;
    let epoch = run.epoch.ok_or("accepted enrolled epoch absent")?;
    let facts = connection.process.facts()?;
    if run.terminal
        || run.fenced
        || !accepted.matches(&run.envelope)
        || run.start_epoch != Some(epoch)
        || facts.binding.agent != accepted.recipient_agent
        || facts.binding.launch != accepted.launch_id
        || facts.runtime.runtime_epoch != accepted.runtime_epoch
        || facts.channel_id != accepted.channel_lease
        || !matches!(&facts.binding.scope, ManagedLaunchScope::Task { task, run: id }
            if task == &accepted.task_id && id == &accepted.run_id)
    {
        return Err("accepted enrolled custody changed or retired".into());
    }
    let snapshot = handle
        .state
        .services
        .workspace_memberships
        .as_ref()
        .ok_or("native owner absent")?
        .task_snapshot(&connection.route.cluster_id)?;
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == accepted.task_id)
        .ok_or("native task absent")?;
    let control = snapshot
        .controls
        .get(&accepted.task_id)
        .ok_or("native task control absent")?;
    if control.run_epoch != epoch
        || task.assigned_agent.as_deref() != Some(&accepted.recipient_agent)
        || control.execution_budget != run.envelope.task.execution_budget
    {
        return Err("accepted native run changed".into());
    }
    let cancelled = task.status == WorkspaceTaskStatus::Cancelled;
    if !cancelled
        && control.active_run.as_ref().is_none_or(|active| {
            active.epoch != epoch || active.agent_id != accepted.recipient_agent
        })
    {
        return Err("accepted native run is no longer active".into());
    }
    Ok((run, cancelled))
}

pub(in crate::collab_broker_capnp) fn cancelled(
    handle: &Context,
    numeric: u64,
    lease: &str,
) -> Result<bool, String> {
    observed(handle, numeric, lease).map(|(_, cancelled)| cancelled)
}

pub(in crate::collab_broker_capnp) fn start_action(
    handle: &Context,
    admitted: &AdmittedJoinIdentity,
    numeric: u64,
    lease: &str,
    target: runtime_collab::TaskAction<'_>,
) -> Result<InvocationJob, String> {
    let (run, cancelled) = observed(handle, numeric, lease)?;
    if cancelled {
        return Err("the task was cancelled; no further action may start".into());
    }
    let (_, connection) = handle.connection()?;
    let offered = handle
        .state
        .surfaces
        .lock()
        .map_err(|_| "action surfaces unavailable")?
        .get(&target.peer_id)
        .filter(|surface| surface.route.cluster_id == connection.route.cluster_id)
        .and_then(|surface| {
            surface
                .actions
                .iter()
                .find(|action| action.resource == target.resource && action.action == target.action)
        })
        .cloned()
        .ok_or("the task action is no longer published in this workspace")?;
    if offered.contract.confirmation != tilo_actions::ConfirmationPolicy::None {
        return Err(
            "task action requires an interactive confirmation and cannot run unattended".into(),
        );
    }
    let context = serde_json::from_value(run.envelope.task.action_context)
        .map_err(|_| "retained task action context is invalid")?;
    let actor = admitted.broker_actor();
    handle.state.start_invocation_confirmed_for_origin(
        target.peer_id,
        target.resource,
        target.action,
        InvocationInputs {
            requested: None,
            caller: None,
            execution: None,
            argument: target.argument,
            actor: &actor,
            confirmation_token: None,
            context: Some(context),
        },
        Some(InvocationCaller::Local {
            route: connection.route,
            admitted: Box::new(admitted.clone()),
        }),
    )
}
