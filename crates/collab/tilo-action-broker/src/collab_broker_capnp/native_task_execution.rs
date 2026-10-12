//! Native runs are recoverable from the canonical approved task, not a queue grant.
use super::{
    AdmissionProvenance, BrokerActor, CapnpBrokerState, DurableContext, InvocationInputs,
    InvocationJob, InvocationKey, InvocationOwner, PendingTaskRun, TaskRunEnvelope,
    WorkspaceTaskStatus, bounded_task_message, durable_local_invocation,
};
use crate::services::NativeTaskBinding;
use std::sync::Arc;
use tilo_peer_state::workspace_task::{WorkspaceTaskCommand, WorkspaceTaskFinish};
use tokio::sync::mpsc;
#[cfg(test)]
#[path = "native_task_tests.rs"]
mod tests;

impl CapnpBrokerState {
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
            return Ok(None);
        };
        let requirement = caller.authorize_retained_task(self, &record)?;
        self.native_tasks(&run.cluster_id)
            .ok_or("native task owner unavailable")?
            .authorize_action(&run.cluster_id, &run.agent_id, &requirement)?;
        let offered = self
            .surfaces
            .lock()
            .map_err(|_| "native task target surface unavailable")?
            .get(&target.peer_id)
            .and_then(|surface| surface.action_projection(target.resource, target.action))
            .ok_or("native task target was removed")?;
        let request = serde_json::json!({
            "task": run.task_id, "epoch": binding.epoch,
            "peer_id": target.peer_id, "app": offered.0.app_id,
            "record": offered.0.route.record_id,
            "resource": target.resource, "action": target.action,
            "argument": target.argument,
        });
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
    ) -> Result<InvocationJob, String> {
        let offered = self
            .surfaces
            .lock()
            .map_err(|_| "task target surface unavailable")?
            .get(&target.peer_id)
            .and_then(|surface| surface.action_projection(target.resource, target.action))
            .ok_or("task target was removed")?;
        if offered.0.route.cluster_id != run.cluster_id {
            return Err("task target changed workspace".into());
        }
        let intent = serde_json::json!({ "task":run.task_id,"epoch":run.native.as_ref().map(|binding| binding.epoch),
            "peer_id":target.peer_id,"app":offered.0.app_id,"record":offered.0.route.record_id,
            "resource":target.resource,"action":target.action,"argument":target.argument });
        let actor = BrokerActor::agent(format!("agent:{}", run.agent_id));
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| "task action admission unavailable")?;
        self.dispatch_invocation_from_offered_owned(
            target.peer_id,
            offered,
            InvocationInputs {
                requested: Some(invocation),
                caller: None,
                execution: None,
                argument: target.argument,
                actor: &actor,
                confirmation_token: None,
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
            },
            TaskRunEnvelope {
                task_id: task_id.into(),
                title: task.title.clone(),
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
                return Ok(());
            }
        }
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
            runtime_collab::TaskRunUpdate::Progress { .. } => {
                WorkspaceTaskCommand::ExecutionPhase {
                    task_id,
                    run_epoch: binding.epoch,
                    sequence: 1,
                    phase: tilo_desktop_model::WorkspaceTaskExecutionPhase::Executing,
                }
            }
            runtime_collab::TaskRunUpdate::Succeeded { message } => WorkspaceTaskCommand::Finish {
                task_id,
                run_epoch: binding.epoch,
                outcome: WorkspaceTaskFinish::Done,
                summary: bounded_task_message(&message)?,
            },
            runtime_collab::TaskRunUpdate::Failed { message }
            | runtime_collab::TaskRunUpdate::Cancelled { message } => {
                WorkspaceTaskCommand::Abandon {
                    task_id,
                    run_epoch: binding.epoch,
                    reason: bounded_task_message(&message)?,
                }
            }
        };
        let receipt = service.report(&run.cluster_id, binding, command).await?;
        if receipt.binding.room != run.cluster_id {
            return Err("native task report receipt room differs".into());
        }
        Ok(())
    }
}
