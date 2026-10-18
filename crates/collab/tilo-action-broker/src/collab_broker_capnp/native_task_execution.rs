//! Native runs are recoverable from the canonical approved task, not a queue grant.
pub(super) use super::native_task_launch::NativeLaunch;
use super::{
    AdmissionProvenance, CapnpBrokerState, DurableContext, InvocationInputs, InvocationJob,
    InvocationKey, InvocationOwner, PendingTaskRun, TaskRunEnvelope, WorkspaceTaskStatus,
    durable_local_invocation,
};
use crate::services::NativeTaskBinding;
use std::sync::Arc;
use tilo_peer_state::workspace_task::{WorkspaceTaskCommand, WorkspaceTaskFinish};
use tokio::sync::mpsc;
#[cfg(test)]
#[path = "native_task_tests.rs"]
mod tests;

fn bounded_public_task_message(message: &str) -> Result<String, String> {
    if message.trim().is_empty()
        || message.len() > tilo_desktop_model::MAX_WORKSPACE_TASK_SUMMARY_BYTES
    {
        return Err("public task update is empty or exceeds the retained text limit".into());
    }
    Ok(message.to_owned())
}

/// How often a held re-ask samples its run's cancellation. The task owner
/// signals no change for it, so it is read in-process here, at the cadence
/// the agent used to ask across the connection; reading the run cancelled
/// cancels its children, which ends the hold.
const CANCELLATION_SAMPLE: std::time::Duration = std::time::Duration::from_millis(50);

/// What a native task's re-ask of its child asked to wait for: the child
/// moving past `seen` (with no `seen`, settling), until `until`.
pub(super) struct ChildWait {
    seen: Option<tilo_actions::remote::UpdateMark>,
    until: tokio::time::Instant,
}

impl ChildWait {
    /// The wait a re-ask asked for, capped at
    /// [`tilo_actions::remote::MAX_REMOTE_WAIT`]; `None` for an answer now.
    pub(super) fn read(
        request: &super::channel_handle::invoke_task_action_params::Reader<'_>,
    ) -> Result<Option<Self>, capnp::Error> {
        let wait_ms = request.get_wait_ms();
        if wait_ms == 0 {
            return Ok(None);
        }
        let seen = request.get_seen_json()?.to_str()?.trim();
        let seen = if seen.is_empty() {
            None
        } else {
            Some(serde_json::from_str(seen).map_err(|_| {
                capnp::Error::failed("task child wait names an invalid update mark".into())
            })?)
        };
        let wait = std::time::Duration::from_millis(u64::from(wait_ms))
            .min(tilo_actions::remote::MAX_REMOTE_WAIT);
        let now = tokio::time::Instant::now();
        Ok(Some(Self {
            seen,
            until: now.checked_add(wait).unwrap_or(now),
        }))
    }

    /// Whether `update` is news to the caller.
    fn arrived(&self, update: &tilo_actions::InvocationUpdate) -> bool {
        update.is_terminal()
            || self
                .seen
                .is_some_and(|seen| tilo_actions::remote::UpdateMark::of(update) != seen)
    }
}

/// A native task's request about its child: the run, the child's stable
/// id, the action it names and the authenticated caller.
pub(super) struct ChildReask<'a> {
    pub peer_id: u64,
    pub agent_id: &'a str,
    pub run_id: u64,
    pub lease: &'a str,
    pub invocation: tilo_actions::InvocationId,
    pub target: runtime_collab::TaskAction<'a>,
    pub caller: &'a durable_local_invocation::LocalCaller,
}

pub(super) fn child_provenance(
    lease: &str,
    invocation: &str,
    caller: Option<durable_local_invocation::LocalCaller>,
) -> Result<
    Option<(
        durable_local_invocation::LocalCaller,
        tilo_actions::InvocationId,
    )>,
    capnp::Error,
> {
    if runtime_collab::native_task_revision(lease).is_none() {
        return Ok(None);
    }
    let id = uuid::Uuid::parse_str(invocation).map_err(|_| {
        capnp::Error::failed("native task child requires stable invocation UUID".into())
    })?;
    if id.is_nil() {
        return Err(capnp::Error::failed("nil task invocation".into()));
    }
    Ok(Some((
        caller.ok_or_else(|| {
            capnp::Error::failed("native task channel custody unavailable".into())
        })?,
        tilo_actions::InvocationId(id),
    )))
}

/// The step the run of `epoch` is paused at for its person, when a draft of
/// it is still unsettled in the task's `control`: delivered with the run so
/// an agent that restarted resumes at the draft rather than planning the
/// task over, which would ask its person again for what they already saw.
fn paused_step(
    control: &tilo_desktop_model::WorkspaceTaskControl,
    epoch: u64,
) -> Option<runtime_collab::TaskRunPausedStep> {
    let held = control
        .confirmation
        .as_ref()
        .filter(|held| held.run_epoch == epoch)?;
    Some(runtime_collab::TaskRunPausedStep {
        app: held.app.clone(),
        resource: held.resource.clone(),
        action: held.action.clone(),
        draft: held.draft.clone(),
        arguments_digest: held.arguments_digest,
    })
}

fn native_request(
    run: &PendingTaskRun,
    target: runtime_collab::TaskAction<'_>,
    app: &str,
    record: &str,
) -> serde_json::Value {
    let mut request = serde_json::json!({"task":run.task_id,
        "epoch":run.native.as_ref().map(|binding| binding.epoch),
        "peer_id":target.peer_id,"app":app,"record":record,
        "resource":target.resource,"action":target.action,"argument":target.argument});
    if !target.provider.is_empty()
        && let Some(request) = request.as_object_mut()
    {
        request.remove("peer_id");
        request.insert("provider".into(), target.provider.into());
    }
    request
}

impl CapnpBrokerState {
    pub(super) async fn invoke_native_task_child(
        self: &std::sync::Arc<Self>,
        reask: &ChildReask<'_>,
        wait: Option<ChildWait>,
        admission: (&super::CrdtRoute, &super::AdmittedJoinIdentity),
    ) -> Result<(u64, tilo_actions::InvocationUpdate), capnp::Error> {
        if let Some(observed) = self.observe_native_child(reask, wait, admission).await? {
            return Ok(observed);
        }
        if !reask.target.provider.is_empty() {
            return self
                .launch_native_task_action(reask)
                .await
                .map_err(capnp::Error::failed);
        }
        let gate = self
            .native_confirmation_gate(reask)
            .await
            .map_err(capnp::Error::failed)?;
        let (argument, confirmation) = match gate {
            super::native_task_confirmation::NativeGate::Proceed => (None, None),
            super::native_task_confirmation::NativeGate::Confirmed {
                argument,
                confirmation,
            } => (Some(argument), Some(confirmation)),
        };
        let mut target = reask.target;
        if let Some(argument) = argument.as_deref() {
            target.argument = Some(argument);
        }
        let run = self
            .task_run_for(reask.peer_id, reask.agent_id, reask.run_id, reask.lease)
            .map_err(capnp::Error::failed)?;
        let offered = self
            .surfaces
            .lock()
            .map_err(|_| capnp::Error::failed("task surface unavailable".into()))?
            .get(&target.peer_id)
            .and_then(|surface| surface.action_projection(target.resource, target.action))
            .ok_or_else(|| capnp::Error::failed("task action unavailable".into()))?;
        let witness = self
            .native_store_witness(
                (&run, reask.lease, confirmation.is_some()),
                &offered,
                target.argument,
                reask.caller,
                reask.invocation,
            )
            .await
            .map_err(capnp::Error::failed)?;
        reask.caller.current(self).map_err(capnp::Error::failed)?;
        if witness.is_some() {
            self.check_native_store_offer(&offered)
                .map_err(capnp::Error::failed)?;
        }
        let provenance = (reask.caller.clone(), reask.invocation);
        let job = self
            .start_task_action(
                reask.peer_id,
                reask.agent_id,
                reask.run_id,
                reask.lease,
                target,
                (
                    Some(&provenance),
                    (confirmation.is_some(), witness.as_deref()),
                ),
            )
            .map_err(capnp::Error::failed)?;
        if let Some(confirmation) = confirmation {
            let run = self
                .task_run_for(reask.peer_id, reask.agent_id, reask.run_id, reask.lease)
                .map_err(capnp::Error::failed)?;
            // The child started; a failure to clear the answer only leaves
            // the card until the run ends.
            if let Err(error) = self.settle_native_confirmation(&run, &confirmation).await {
                tracing::warn!(%error, "confirmed task action started but its confirmation stayed");
            }
        }
        let update = self.invocation_status(job.id).ok_or_else(|| {
            capnp::Error::failed("accepted task action has no broker state".into())
        })?;
        Ok((job.id, update))
    }

    /// What a native task's request finds of its child: `None` when it has
    /// not started (the request starts it), else the child's state as
    /// [`Self::native_task_observation`] reports it. With `wait`, a running
    /// child is held ([`Self::hold_native_observation`]) and then observed
    /// and authorized afresh, with the connection's admission revalidated
    /// (`admission`); a held re-ask never starts the child anew.
    pub(super) async fn observe_native_child(
        &self,
        reask: &ChildReask<'_>,
        wait: Option<ChildWait>,
        admission: (&super::CrdtRoute, &super::AdmittedJoinIdentity),
    ) -> Result<Option<(u64, tilo_actions::InvocationUpdate)>, capnp::Error> {
        let observe = || {
            let run =
                self.task_run_for(reask.peer_id, reask.agent_id, reask.run_id, reask.lease)?;
            self.native_task_observation(&run, reask.invocation, reask.target, reask.caller)
        };
        let (observed, wait) = match (observe().map_err(capnp::Error::failed)?, wait) {
            (Some(observed), Some(wait)) => (observed, wait),
            (observed, _) => return Ok(observed),
        };
        self.hold_native_observation(reask, observed, wait).await;
        let (route, identity) = admission;
        self.revalidate_admission(route, identity)?;
        observe()
            .map_err(capnp::Error::failed)?
            .ok_or_else(|| capnp::Error::failed("native task child is no longer recorded".into()))
            .map(Some)
    }

    /// Hold a native task's re-ask of its child, observed as `observed`,
    /// until the child moves past what the caller has seen or settles, the
    /// run is read cancelled (or can no longer be read), or the wait ends.
    /// It answers nothing itself: the caller observes and authorizes the
    /// child afresh once it returns. Between wakes it only looks at the
    /// child's status; it never starts, admits or authorizes anything.
    pub(super) async fn hold_native_observation(
        &self,
        reask: &ChildReask<'_>,
        observed: (u64, tilo_actions::InvocationUpdate),
        wait: ChildWait,
    ) {
        let (wire_id, mut update) = observed;
        // Subscribed before looking again, so a change between the
        // observation and this point still wakes the hold.
        let mut changes = self.invocation_changes();
        if let Some(current) = self.invocation_status(wire_id) {
            update = current;
        }
        let mut signalled = true;
        while !wait.arrived(&update) {
            let now = tokio::time::Instant::now();
            if now >= wait.until {
                return;
            }
            let sample = CANCELLATION_SAMPLE.min(wait.until.saturating_duration_since(now));
            tokio::select! {
                signal = changes.changed(), if signalled => {
                    // A broker going away stops signalling: sample only.
                    signalled = signal.is_ok();
                }
                () = tilo_rt::sleep(sample) => {
                    match self.task_run_cancelled(
                        reask.peer_id,
                        reask.agent_id,
                        reask.run_id,
                        reask.lease,
                    ) {
                        Ok(false) => {}
                        Ok(true) | Err(_) => return,
                    }
                }
            }
            match self.invocation_status(wire_id) {
                Some(current) => update = current,
                None => return,
            }
        }
        tracing::trace!(invocation = %reask.invocation.0, "held native re-ask answered");
    }

    pub(super) fn native_task_observation(
        &self,
        run: &PendingTaskRun,
        invocation: tilo_actions::InvocationId,
        target: runtime_collab::TaskAction<'_>,
        caller: &durable_local_invocation::LocalCaller,
    ) -> Result<Option<(u64, tilo_actions::InvocationUpdate)>, String> {
        let binding = run.native.as_ref().ok_or("native task binding absent")?;
        let snapshot = self
            .native_tasks(&run.cluster_id)
            .ok_or("native task owner unavailable")?
            .snapshot(&run.cluster_id)?;
        if !snapshot.tasks.iter().any(|task| {
            task.id == run.task_id && task.assigned_agent.as_deref() == Some(run.agent_id.as_str())
        }) || snapshot
            .controls
            .get(&run.task_id)
            .is_none_or(|control| control.run_epoch != binding.epoch)
        {
            return Err("native task observation source was replaced".into());
        }
        let record = {
            let guard = self
                .journal
                .lock()
                .map_err(|_| "native task journal unavailable")?;
            let journal = guard
                .as_ref()
                .map_err(Clone::clone)?
                .as_ref()
                .ok_or("native task journal unavailable")?;
            let found = journal.lookup(&InvocationKey {
                room: run.cluster_id.clone(),
                host: journal.host(),
                invocation,
                owner: InvocationOwner::NativeTask {
                    agent: run.agent_id.clone(),
                    task: run.task_id.clone(),
                    epoch: binding.epoch,
                },
            })?;
            drop(guard);
            found
        };
        let Some(record) = record else {
            return self.native_host_observation(run, target, caller, invocation);
        };
        let requirement = caller.authorize_retained_task(
            self,
            (
                &record,
                &run.native
                    .as_ref()
                    .ok_or("native binding absent")?
                    .actor()?,
            ),
        )?;
        self.native_tasks(&run.cluster_id)
            .ok_or("native task owner unavailable")?
            .authorize_action(&run.cluster_id, &run.agent_id, &requirement)?;
        let (app, record_id) = if target.provider.is_empty() {
            let offered = self
                .surfaces
                .lock()
                .map_err(|_| "native task target surface unavailable")?
                .get(&target.peer_id)
                .and_then(|surface| surface.action_projection(target.resource, target.action))
                .ok_or("native task target was removed")?;
            (offered.0.app_id, offered.0.route.record_id)
        } else {
            let app = record
                .intent
                .get("app_id")
                .and_then(serde_json::Value::as_str)
                .ok_or("retained task app absent")?
                .to_owned();
            let record_id = record
                .intent
                .get("record_id")
                .and_then(serde_json::Value::as_str)
                .ok_or("retained task record absent")?
                .to_owned();
            (app, record_id)
        };
        let request = native_request(run, target, &app, &record_id);
        if !record.admitted_request_is(&request) {
            return Err("native task child identity was reused with another request".into());
        }
        if record.uncertain {
            return Err(
                "native task action outcome is uncertain; review history before retrying".into(),
            );
        }
        let update = self
            .invocation_status_for_owner(record.wire_id, &record.key.owner)
            .unwrap_or(record.update);
        Ok(Some((record.wire_id, update)))
    }

    fn cancel_native_children(&self, run: &PendingTaskRun) -> Result<(), String> {
        let binding = run.native.as_ref().ok_or("native task binding absent")?;
        let owner = InvocationOwner::NativeTask {
            agent: run.agent_id.clone(),
            task: run.task_id.clone(),
            epoch: binding.epoch,
        };
        let keys = self
            .pending_invocations
            .lock()
            .map_err(|_| "native child registry unavailable")?
            .values()
            .filter(|pending| {
                pending.owner == owner
                    && pending.cluster_id == run.cluster_id
                    && pending.cancellable
            })
            .filter_map(|pending| pending.journal_key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            let record = {
                let guard = self
                    .journal
                    .lock()
                    .map_err(|_| "native child journal unavailable")?;
                guard
                    .as_ref()
                    .map_err(Clone::clone)?
                    .as_ref()
                    .ok_or("native child journal unavailable")?
                    .lookup(&key)?
            };
            if let Some(record) = record {
                self.cancel_durable_record(&record)?;
            }
        }
        Ok(())
    }
    pub(super) fn start_native_task_invocation(
        &self,
        run: &PendingTaskRun,
        target: runtime_collab::TaskAction<'_>,
        caller: &durable_local_invocation::LocalCaller,
        invocation: tilo_actions::InvocationId,
        offered: (super::PublishedSurface, super::PublishedAction),
        (confirmed, witness): (bool, Option<&str>),
    ) -> Result<InvocationJob, String> {
        // Dispatch the exact contract checked by native task admission. A
        // concurrent publication must not replace it with a broader need.
        if offered.0.route.cluster_id != run.cluster_id {
            return Err("task target changed workspace".into());
        }
        let intent = native_request(run, target, &offered.0.app_id, &offered.0.route.record_id);
        let actor = run
            .native
            .as_ref()
            .ok_or("native binding absent")?
            .actor()?;
        // Admission checked the person's answer for this exact invocation;
        // the token carries it to the dispatch, bound to these arguments.
        let token = if super::requires_store_witness(&offered.0.app_id, &offered.1.action) {
            self.confirmation_token(
                target.peer_id,
                &offered.0.app_id,
                &offered.1,
                target.argument,
                &actor,
                witness
                    .map(|token| tilo_actions::remote::Confirmation::Witnessed(token.into()))
                    .as_ref(),
                &self.native_witness_binding(actor.id(), &offered, target.argument, invocation),
            )
            .map_err(|error| error.to_string())?
        } else {
            confirmed
                .then(|| {
                    self.mint_confirmation_grant(
                        target.peer_id,
                        &offered.1,
                        target.argument,
                        &actor,
                    )
                })
                .transpose()?
        };
        let app = offered.0.app_id.clone();
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| "task action admission unavailable")?;
        self.dispatch_invocation_from_offered_owned(
            target.peer_id,
            offered,
            InvocationInputs {
                original_request: None,
                requested: Some(invocation),
                caller: None,
                execution: None,
                argument: target.argument,
                actor: &actor,
                confirmation_token: token.as_deref(),
                context: Some(run.action_context.clone()),
            },
            AdmissionProvenance {
                durable: Some(DurableContext::Task {
                    caller,
                    binding: run.native.as_ref().ok_or("native binding absent")?,
                    invocation,
                    intent: &intent,
                }),
                revision: None,
                origin: None,
                retained_time: None,
            },
        )
        .inspect(|_| {
            self.launchable.note_view(&run.cluster_id, &actor.id, &app);
        })
    }

    pub(super) fn native_tasks(
        &self,
        room: &str,
    ) -> Option<&Arc<dyn crate::services::WorkspaceTaskExecution>> {
        self.services
            .task_execution
            .as_ref()
            .filter(|service| service.requires_room(room))
    }

    pub(super) fn dispatch_native_task(&self, room: &str, task_id: &str) -> Result<u64, String> {
        let service = self
            .native_tasks(room)
            .ok_or("native task execution unavailable")?;
        let snapshot = service.snapshot(room)?;
        let task = snapshot
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .ok_or("native task no longer exists")?;
        let control = snapshot
            .controls
            .get(task_id)
            .ok_or("native task control unavailable")?;
        let run = control
            .active_run
            .as_ref()
            .ok_or("review and approve the queued task first")?;
        if task.status != WorkspaceTaskStatus::InProgress
            || run.approval.is_none()
            || task.assigned_agent.as_deref() != Some(run.agent_id.as_str())
            || task
                .resource
                .as_ref()
                .is_some_and(|resource| resource.agent_request.is_some())
        {
            return Err("task does not carry ordinary native run approval".into());
        }
        service.agent_snapshot(room, &run.agent_id)?;
        let planning = match task.route {
            tilo_desktop_model::WorkspaceTaskRoute::Model => runtime_collab::TaskRunPlanning::Model,
            tilo_desktop_model::WorkspaceTaskRoute::RulesOnly => {
                runtime_collab::TaskRunPlanning::RulesOnly
            }
            tilo_desktop_model::WorkspaceTaskRoute::Manual => {
                return Err("manual tasks cannot execute".into());
            }
        };
        let execution_budget = self.admitted_task_execution_budget(room, task)?;
        let (peer_id, tx) = self
            .task_subscribers
            .lock()
            .map_err(|_| "task subscribers unavailable")?
            .iter()
            .filter(|(_, subscriber)| {
                subscriber.cluster_id == room
                    && subscriber.agent_id == run.agent_id
                    && !subscriber.tx.is_closed()
            })
            .min_by_key(|(peer, _)| **peer)
            .map(|(peer, subscriber)| (*peer, subscriber.tx.clone()))
            .ok_or("assigned agent is offline")?;
        let digest =
            blake3::hash(format!("native-task-v1\0{room}\0{task_id}\0{}", run.epoch).as_bytes());
        let run_id = u64::from_le_bytes(
            *digest
                .as_bytes()
                .first_chunk::<8>()
                .ok_or("invalid run digest")?,
        ) | (1 << 63);
        let hex = digest.to_hex();
        let lease = format!(
            "native:{}:0",
            hex.as_str().get(..32).ok_or("invalid run digest")?
        );
        let context = task.action_context();
        self.offer_native_run(
            room,
            NativeTaskBinding {
                task_id: task_id.into(),
                agent_id: run.agent_id.clone(),
                epoch: run.epoch,
                revision: 0,
                starter: run
                    .approval
                    .as_ref()
                    .map(|approval| approval.person.clone()),
            },
            TaskRunEnvelope {
                paused: paused_step(control, run.epoch),
                task_id: task_id.into(),
                title: task.title.clone(),
                instruction: Some(
                    control
                        .instruction
                        .clone()
                        .unwrap_or_else(|| task.title.clone()),
                ),
                subject: task.subject.clone(),
                run_id,
                lease,
                planning,
                execution_budget,
                action_context: context,
            },
            (peer_id, tx),
        )
    }

    fn offer_native_run(
        &self,
        room: &str,
        binding: NativeTaskBinding,
        envelope: TaskRunEnvelope,
        subscriber: (u64, mpsc::UnboundedSender<TaskRunEnvelope>),
    ) -> Result<u64, String> {
        let (peer_id, tx) = subscriber;
        let run_id = envelope.run_id;
        let mut pending = self
            .pending_task_runs
            .lock()
            .map_err(|_| "task run registry unavailable")?;
        if let Some(prior) = pending.get(&run_id) {
            if prior.cluster_id != room
                || prior.task_id != binding.task_id
                || prior.agent_id != binding.agent_id
            {
                return Err("native task run identity collision".into());
            }
            if prior.peer_id == peer_id {
                return Ok(run_id);
            }
        }
        if pending.len() >= 256 && !pending.contains_key(&run_id) {
            return Err("native task delivery capacity reached; review running tasks".into());
        }
        pending.insert(
            run_id,
            PendingTaskRun {
                peer_id,
                agent_id: binding.agent_id.clone(),
                cluster_id: room.into(),
                task_id: binding.task_id.clone(),
                lease: envelope.lease.clone(),
                action_context: envelope.action_context.clone(),
                terminal: false,
                native: Some(binding),
            },
        );
        drop(pending);
        if tx.send(envelope).is_err() {
            self.pending_task_runs
                .lock()
                .map_err(|_| "task run registry unavailable")?
                .remove(&run_id);
            return Err("agent task stream closed".into());
        }
        Ok(run_id)
    }

    pub(super) fn has_native_task_subscribers(&self) -> bool {
        self.task_subscribers.lock().is_ok_and(|subscribers| {
            subscribers
                .values()
                .any(|subscriber| !subscriber.tx.is_closed())
        })
    }

    pub(super) fn refresh_native_task_delivery(&self) {
        let rooms = match self.task_subscribers.lock() {
            Ok(subscribers) => {
                let rooms = subscribers
                    .values()
                    .map(|subscriber| subscriber.cluster_id.clone())
                    .collect::<std::collections::BTreeSet<_>>();
                drop(subscribers);
                rooms
            }
            Err(_) => return,
        };
        for room in rooms {
            let Some(service) = self.native_tasks(&room) else {
                continue;
            };
            let Ok(snapshot) = service.snapshot(&room) else {
                continue;
            };
            self.prune_native_run_routes(&room, &snapshot);
            for task in snapshot
                .tasks
                .iter()
                .filter(|task| task.status == WorkspaceTaskStatus::InProgress)
            {
                if let Err(error) = self.dispatch_native_task(&room, &task.id) {
                    tracing::debug!(%error, task = %task.id, "native approved task not delivered");
                }
            }
        }
    }

    fn prune_native_run_routes(
        &self,
        room: &str,
        snapshot: &tilo_peer_state::workspace_task::WorkspaceTaskSnapshot,
    ) {
        let Ok(mut runs) = self.pending_task_runs.lock() else {
            return;
        };
        if runs.len() < 256 {
            return;
        }
        // Retire only transport routes for ended/replaced runs. Their owner
        // state, pending decisions and durable effect deduplication remain.
        runs.retain(|_, run| {
            run.native.as_ref().is_none_or(|binding| {
                run.cluster_id != room
                    || snapshot
                        .controls
                        .get(&run.task_id)
                        .and_then(|control| control.active_run.as_ref())
                        .is_some_and(|active| {
                            active.epoch == binding.epoch && active.agent_id == binding.agent_id
                        })
            })
        });
    }

    pub(super) fn native_task_status(
        &self,
        run: &PendingTaskRun,
        lease: &str,
    ) -> Result<runtime_collab::TaskRunStatus, String> {
        let binding = run.native.as_ref().ok_or("not a native task run")?;
        let (_, revision) =
            runtime_collab::native_task_revision(lease).ok_or("invalid native task lease")?;
        let snapshot = self
            .native_tasks(&run.cluster_id)
            .ok_or("native task owner unavailable")?
            .agent_snapshot(&run.cluster_id, &run.agent_id)?;
        let task = snapshot
            .tasks
            .iter()
            .find(|task| task.id == run.task_id)
            .ok_or("task was deleted")?;
        let control = snapshot
            .controls
            .get(&run.task_id)
            .ok_or("task control unavailable")?;
        if task.status == WorkspaceTaskStatus::Cancelled {
            if control.run_epoch != binding.epoch {
                return Err("native cancellation belongs to another run".into());
            }
            self.cancel_native_children(run)?;
            return Ok(runtime_collab::TaskRunStatus {
                cancelled: true,
                control: None,
            });
        }
        let active = control.active_run.as_ref().ok_or("task run ended")?;
        if active.epoch != binding.epoch
            || active.agent_id != run.agent_id
            || active.approval.is_none()
        {
            return Err("native task run was replaced".into());
        }
        let latest = control
            .directions
            .last()
            .map_or(0, |direction| direction.revision);
        if revision > active.applied_direction_revision {
            return Err("task lease claims an unacknowledged direction".into());
        }
        let directions = control
            .directions
            .iter()
            .filter(|direction| direction.revision > revision)
            .take(runtime_collab::TaskRunControl::MAX_DIRECTIONS)
            .map(|direction| runtime_collab::TaskDirection {
                revision: direction.revision,
                author: direction.author.clone(),
                text: direction.text.clone(),
                issued_at: direction.created_at,
            })
            .collect::<Vec<_>>();
        let last = directions
            .last()
            .map_or(revision, |direction| direction.revision);
        let control = runtime_collab::TaskRunControl {
            version: 1,
            task_id: run.task_id.clone(),
            run_epoch: binding.epoch,
            from_revision: revision,
            applied_revision: active.applied_direction_revision,
            latest_revision: latest,
            directions,
            has_more: last < latest,
        };
        control.validate()?;
        Ok(runtime_collab::TaskRunStatus {
            cancelled: false,
            control: Some(control),
        })
    }

    pub(super) async fn report_native_task(
        &self,
        run: PendingTaskRun,
        update: runtime_collab::TaskRunUpdate,
    ) -> Result<(), String> {
        // An agent reports acceptance and its first progress without waiting
        // in between, and a report's command is read from the state the one
        // before committed (progress needs the accepted run). Applied
        // concurrently, progress read the state before acceptance landed and
        // was refused, failing the run at its end.
        let _order = self.native_task_report_order.lock().await;
        let binding = run.native.clone().ok_or("not a native task run")?;
        let service = self
            .native_tasks(&run.cluster_id)
            .ok_or("native task owner unavailable")?;
        if matches!(update, runtime_collab::TaskRunUpdate::Cancelled { .. }) {
            let snapshot = service.snapshot(&run.cluster_id)?;
            if snapshot
                .tasks
                .iter()
                .any(|task| task.id == run.task_id && task.status == WorkspaceTaskStatus::Cancelled)
                && snapshot
                    .controls
                    .get(&run.task_id)
                    .is_some_and(|control| control.run_epoch == binding.epoch)
            {
                self.note_ended_task_run(&run, "cancelled");
                return Ok(());
            }
        }
        let ended = ended_outcome(&update);
        let task_id = run.task_id.clone();
        if matches!(
            update,
            runtime_collab::TaskRunUpdate::Failed { .. }
                | runtime_collab::TaskRunUpdate::Cancelled { .. }
        ) {
            self.cancel_native_children(&run)?;
        }
        let command = match update {
            runtime_collab::TaskRunUpdate::DirectionsApplied { revision } => {
                WorkspaceTaskCommand::AppliedAck {
                    task_id,
                    run_epoch: binding.epoch,
                    revision,
                }
            }
            runtime_collab::TaskRunUpdate::Accepted => WorkspaceTaskCommand::ExecutionPhase {
                task_id,
                run_epoch: binding.epoch,
                sequence: 0,
                phase: tilo_desktop_model::WorkspaceTaskExecutionPhase::Accepted,
            },
            runtime_collab::TaskRunUpdate::Progress {
                message,
                presentation,
            } => {
                let snapshot = service.snapshot(&run.cluster_id)?;
                let execution = snapshot
                    .controls
                    .get(&task_id)
                    .and_then(|control| control.execution.as_ref())
                    .filter(|execution| execution.run_epoch == binding.epoch)
                    .ok_or("public progress requires accepted run")?;
                let sequence = execution
                    .sequence
                    .checked_add(1)
                    .ok_or("task sequence exhausted")?;
                if let Some(presentation) = presentation {
                    WorkspaceTaskCommand::Presentation {
                        task_id,
                        run_epoch: binding.epoch,
                        sequence,
                        presentation: presentation.bounded()?,
                    }
                } else {
                    WorkspaceTaskCommand::PublicProgress {
                        task_id,
                        run_epoch: binding.epoch,
                        sequence,
                        message: bounded_public_task_message(&message)?,
                    }
                }
            }
            runtime_collab::TaskRunUpdate::Succeeded { message } => WorkspaceTaskCommand::Finish {
                task_id,
                run_epoch: binding.epoch,
                outcome: WorkspaceTaskFinish::Done,
                summary: bounded_public_task_message(&message)?,
            },
            runtime_collab::TaskRunUpdate::Failed { message }
            | runtime_collab::TaskRunUpdate::Cancelled { message } => {
                WorkspaceTaskCommand::Abandon {
                    task_id,
                    run_epoch: binding.epoch,
                    reason: bounded_public_task_message(&message)?,
                }
            }
        };
        let receipt = service.report(&run.cluster_id, binding, command).await;
        // What the run changed is in the documents whether or not its
        // report was acknowledged: it is recorded for undo either way.
        if let Some(outcome) = ended {
            self.note_ended_task_run(&run, outcome);
        }
        if receipt?.binding.room != run.cluster_id {
            return Err("native task report receipt room differs".into());
        }
        Ok(())
    }

    /// Hand the run that ended to history, which records what it changed as
    /// one run of the agent's, attributed to it and undoable as a unit.
    fn note_ended_task_run(&self, run: &PendingTaskRun, outcome: &str) {
        let Some(service) = self.native_tasks(&run.cluster_id) else {
            return;
        };
        let Ok(snapshot) = service.snapshot(&run.cluster_id) else {
            return;
        };
        let Some(task) = snapshot.tasks.iter().find(|task| task.id == run.task_id) else {
            return;
        };
        self.ended_runs.tasks.push(crate::task_runs::EndedTaskRun {
            room: run.cluster_id.clone(),
            task_id: task.id.clone(),
            title: task.title.clone(),
            agent: format!("agent:{}", run.agent_id),
            person: task.created_by.clone(),
            instruction: snapshot
                .controls
                .get(&task.id)
                .and_then(|control| control.instruction.clone()),
            outcome: outcome.to_owned(),
        });
    }
}

/// How a run that `update` ends ended, in a word history keeps; `None` for
/// an update that does not end it.
fn ended_outcome(update: &runtime_collab::TaskRunUpdate) -> Option<&'static str> {
    match update {
        runtime_collab::TaskRunUpdate::Succeeded { .. } => Some("completed"),
        runtime_collab::TaskRunUpdate::Failed { .. } => Some("stopped"),
        runtime_collab::TaskRunUpdate::Cancelled { .. } => Some("cancelled"),
        _ => None,
    }
}
