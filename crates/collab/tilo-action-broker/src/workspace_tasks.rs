//! Canonical Task Center reads and person-authored mutations through the
//! ordinary durable invocation door. Public arguments select text, routing,
//! and an enrolled Agent; caller identity and owner custody stay out of band.

use super::{
    Arc, CapnpBrokerState, CrdtRoute, InvocationEnvelope, PublishedAction, PublishedInput,
    PublishedSurface,
};
use std::sync::atomic::Ordering;
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand};

const APP: &str = "so.tilo.desktop";
const RECORD: &str = "tasks";
const COLLECTION: &str = "desktop:tasks";
const TASK_PREFIX: &str = "desktop:task:";
const MAX_CURRENT_TASK_FRONTIERS: usize = 256;
const MAX_TASK_ARGUMENT_BYTES: usize = 40 * 1024;
const MAX_ENROLLED_RUNS_PER_PROCESS: usize = 1024;
const MAX_ENROLLED_REPORTS_PER_RUN: usize = 4096;

fn enrolled_label(domain: &[u8], parts: &[&str]) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(domain);
    for part in parts {
        hash.update(&(part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    hash.finalize().to_hex().chars().take(32).collect()
}

fn enrolled_run_number(room: &str, task: &str, epoch: u64) -> u64 {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo/enrolled-task-run/v1");
    hash.update(room.as_bytes());
    hash.update(task.as_bytes());
    hash.update(&epoch.to_le_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&hash.finalize().as_bytes()[..8]);
    u64::from_le_bytes(bytes).max(1)
}

fn is_terminal_update(update: &runtime_collab::TaskRunUpdate) -> bool {
    matches!(
        update,
        runtime_collab::TaskRunUpdate::Succeeded { .. }
            | runtime_collab::TaskRunUpdate::Failed { .. }
            | runtime_collab::TaskRunUpdate::Cancelled { .. }
    )
}

fn reported_execution_phase(
    update: &runtime_collab::TaskRunUpdate,
) -> Option<tilo_desktop_model::WorkspaceTaskExecutionPhase> {
    match update {
        runtime_collab::TaskRunUpdate::Accepted => {
            Some(tilo_desktop_model::WorkspaceTaskExecutionPhase::Accepted)
        }
        runtime_collab::TaskRunUpdate::Planning {} => {
            Some(tilo_desktop_model::WorkspaceTaskExecutionPhase::Planning)
        }
        runtime_collab::TaskRunUpdate::Executing {} => {
            Some(tilo_desktop_model::WorkspaceTaskExecutionPhase::Executing)
        }
        runtime_collab::TaskRunUpdate::Progress { .. }
        | runtime_collab::TaskRunUpdate::Succeeded { .. }
        | runtime_collab::TaskRunUpdate::Failed { .. }
        | runtime_collab::TaskRunUpdate::Cancelled { .. }
        | runtime_collab::TaskRunUpdate::DirectionApplied { .. } => None,
    }
}

impl CapnpBrokerState {
    pub(crate) fn managed_agent_session_is_ready(
        &self,
        room: &str,
        agent: &str,
        launch: &str,
    ) -> bool {
        self.enrolled_task_subscribers
            .lock()
            .is_ok_and(|subscribers| {
                subscribers.values().any(|subscriber| {
                    subscriber.cluster_id == room
                        && subscriber.agent_id == agent
                        && subscriber.facts.binding.launch == launch
                        && !subscriber.tx.is_closed()
                })
            })
    }

    /// Refresh from the admitted owner document, including rooms enrolled
    /// after boot. The published rows contain no task instruction text.
    pub(super) fn refresh_workspace_task_services(self: &Arc<Self>) -> Result<(), String> {
        let Some(service) = self.services.workspace_tasks.clone() else {
            return Ok(());
        };
        let Some(authority) = &self.membership else {
            return Ok(());
        };
        for workspace in authority.snapshot()?.workspaces {
            let room = workspace.room.to_string();
            if !service.requires_room(&room) {
                continue;
            }
            let snapshot = checked_snapshot(service.as_ref(), &room)?;
            let actions = task_actions(&snapshot.tasks, &snapshot.controls);
            let existing = self
                .workspace_task_peers
                .lock()
                .map_err(|_| "task service registry unavailable")?
                .get(&room)
                .copied();
            if let Some(peer) = existing {
                let changed = {
                    let mut surfaces = self
                        .surfaces
                        .lock()
                        .map_err(|_| "task surfaces unavailable")?;
                    surfaces.get_mut(&peer).is_some_and(|surface| {
                        if surface.actions == actions {
                            return false;
                        }
                        surface.actions = actions;
                        true
                    })
                };
                if changed {
                    self.publish_changed_action_catalogue();
                }
                continue;
            }
            self.start_workspace_task_service(room, actions, service.clone())?;
        }
        Ok(())
    }

    fn start_workspace_task_service(
        self: &Arc<Self>,
        room: String,
        actions: Vec<PublishedAction>,
        service: Arc<dyn crate::services::WorkspaceTasks>,
    ) -> Result<(), String> {
        let peer = self.next_service_peer_id();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        self.invocation_subscribers
            .lock()
            .map_err(|_| "invocation subscribers unavailable")?
            .insert(peer, sender);
        self.stable_invocation_subscribers
            .lock()
            .map_err(|_| "stable invocation registry unavailable")?
            .insert(peer);
        self.surfaces
            .lock()
            .map_err(|_| "task surfaces unavailable")?
            .insert(
                peer,
                PublishedSurface {
                    peer_id: peer,
                    app_id: APP.into(),
                    route: CrdtRoute::record(APP, &room, RECORD),
                    live: true,
                    actions,
                },
            );
        self.workspace_task_peers
            .lock()
            .map_err(|_| "task service registry unavailable")?
            .insert(room.clone(), peer);
        self.publish_changed_action_catalogue();
        let state = self.clone();
        tokio::task::spawn_local(async move {
            while let Some(envelope) = receiver.recv().await {
                let result = state
                    .execute_workspace_task_command(peer, &room, &envelope, service.as_ref())
                    .await;
                if result.is_err()
                    && let Err(reason) = service.health().and_then(|()| state.owner_health())
                {
                    if let Ok(mut failure) = state.owner_commit_failure.lock() {
                        failure.get_or_insert(reason);
                    }
                    break;
                }
                let update = match result {
                    Ok(value) => tilo_actions::InvocationUpdate::Succeeded {
                        invocation: envelope.invocation,
                        output: tilo_actions::ActionOutput {
                            value: Some(value),
                            resources: Vec::new(),
                        },
                    },
                    Err(error) => tilo_actions::InvocationUpdate::Failed {
                        invocation: envelope.invocation,
                        error: tilo_actions::ActionFailure {
                            code: "workspace_task_refused".into(),
                            message: error,
                            retryable: false,
                            details: None,
                        },
                    },
                };
                if let Err(error) = state
                    .apply_invocation_update(peer, envelope.id, update, None)
                    .await
                {
                    if let Ok(mut failure) = state.owner_commit_failure.lock() {
                        failure.get_or_insert(error.to_string());
                    }
                    break;
                }
            }
            let removed = state
                .surfaces
                .lock()
                .is_ok_and(|mut surfaces| surfaces.remove(&peer).is_some());
            if removed {
                state.publish_changed_action_catalogue();
            }
            if let Ok(mut peers) = state.workspace_task_peers.lock() {
                peers.remove(&room);
            }
        });
        Ok(())
    }

    async fn execute_workspace_task_command(
        &self,
        peer: u64,
        room: &str,
        envelope: &InvocationEnvelope,
        service: &dyn crate::services::WorkspaceTasks,
    ) -> Result<serde_json::Value, String> {
        if envelope.resource == COLLECTION && envelope.action == "read-current-tasks" {
            return read_current_tasks(room, envelope, service);
        }
        if envelope.action == "read-task" {
            return read_task(room, envelope, service);
        }
        let pending = self
            .pending_invocations
            .lock()
            .map_err(|_| "invocation registry unavailable")?
            .get(&envelope.id)
            .filter(|pending| {
                pending.peer_id == peer && pending.update.invocation() == envelope.invocation
            })
            .cloned()
            .ok_or("task invocation is no longer admitted")?;
        let key = pending
            .journal_key
            .as_ref()
            .ok_or("task invocation has no durable admission")?;
        if key.room != room
            || key.invocation != envelope.invocation
            || key.owner != pending.owner
            || !service.requires_room(room)
        {
            return Err("task invocation binding changed".into());
        }
        let original = pending
            .original
            .as_ref()
            .ok_or("task original custody unavailable")?;
        let caller = original.owner_person_certificate(self, room, envelope)?;
        if envelope.resource == COLLECTION && envelope.action == "start-agent" {
            let agent = startup_agent(envelope)?;
            service.start_agent(room, &agent, &caller)?;
            return Ok(serde_json::json!({"agent": agent, "startup_requested": true}));
        }
        let device = caller.device_public_key;
        let command = person_command(envelope)?;
        let request = OwnerCommitRequest {
            room: room.into(),
            operation: CommitOperation::WorkspaceTaskCommand {
                invocation: envelope.invocation.0,
                authority: WorkspaceTaskAuthority::Person { caller },
                command,
            },
        };
        let receipt = service.execute(request.clone()).await?;
        receipt.validate_for(&request)?;
        let snapshot = checked_snapshot(service, room)?;
        if snapshot.generation < receipt.generation {
            return Err("task owner cannot observe its committed result".into());
        }
        if let Some(surface) = self
            .surfaces
            .lock()
            .map_err(|_| "task surfaces unavailable")?
            .get_mut(&peer)
        {
            surface.actions = task_actions(&snapshot.tasks, &snapshot.controls);
        }
        self.publish_action_catalogue();
        let result = snapshot
            .controls
            .values()
            .flat_map(|control| &control.receipts)
            .find(|saved| {
                saved.device == device
                    && saved.invocation == envelope.invocation.0
                    && saved.request_digest == receipt.binding.request_digest
            })
            .ok_or("task owner lost its exact committed result")?;
        let dispatch = match &result.result {
            tilo_desktop_model::WorkspaceTaskMutationResult::Queued { task_id } => self
                .dispatch_live_workspace_task(room, task_id, service)
                .await
                .map_or_else(
                    |error| {
                        tracing::warn!(%error, %room, %task_id, "queued task delivery deferred");
                        "failed"
                    },
                    |run| if run.is_some() { "started" } else { "queued" },
                ),
            _ => "saved",
        };
        Ok(serde_json::json!({
            "generation": receipt.generation,
            "result": result.result,
            "delivery": dispatch,
        }))
    }

    async fn dispatch_live_workspace_task(
        &self,
        room: &str,
        task_id: &str,
        service: &dyn crate::services::WorkspaceTasks,
    ) -> Result<Option<u64>, String> {
        if let Some((peer, facts)) = self.live_enrolled_subscriber(room, task_id, service)? {
            return self
                .dispatch_enrolled_workspace_task(peer, facts, room, task_id, service)
                .await;
        }
        let _delivery = self.task_delivery_order.lock().await;
        let snapshot = checked_snapshot(service, room)?;
        let task = snapshot
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .ok_or("queued task disappeared before delivery")?;
        if task.status != tilo_desktop_model::WorkspaceTaskStatus::Queued {
            return Ok(None);
        }
        let agent = task
            .assigned_agent
            .as_deref()
            .filter(|_| task.route != tilo_desktop_model::WorkspaceTaskRoute::Manual)
            .ok_or("queued task has no Agent route")?;
        // The Agent executor is serial. Do not label its next instruction as
        // working while another owner-backed run is still active.
        if agent_has_active_run(&snapshot, agent)
            || next_queued_task(&snapshot, agent) != Some(task_id)
        {
            return Ok(None);
        }
        let control = snapshot
            .controls
            .get(task_id)
            .ok_or("queued task has no owner control")?;
        require_retained_task_instruction(control)?;
        let Some((peer_id, tx)) = self.live_task_subscriber(room, agent)? else {
            return Ok(None);
        };
        let epoch = control
            .run_epoch
            .checked_add(1)
            .ok_or("task run epoch exhausted")?;
        let (task, control) = start_owner_task_run(room, task_id, agent, epoch, service).await?;
        let active = control
            .active_run
            .as_ref()
            .filter(|active| active.epoch == epoch && active.agent_id == agent)
            .ok_or("owner did not retain the exact started run")?;
        let run_id = self.task_run_id.fetch_add(1, Ordering::Relaxed);
        let lease = uuid::Uuid::new_v4().to_string();
        let context = task.action_context();
        let envelope = super::TaskRunEnvelope {
            task_id: task.id.clone(),
            title: task.title.clone(),
            instruction: control.instruction.clone(),
            run_id,
            lease: lease.clone(),
            planning: task_run_planning(task.route)?,
            action_context: context.clone(),
        };
        if tx.send(envelope).is_err() {
            self.abandon_undelivered_workspace_task(room, task_id, epoch, service)
                .await?;
            return Err("assigned Agent task stream closed before delivery".into());
        }
        self.pending_task_runs
            .lock()
            .map_err(|_| "task run registry unavailable")?
            .insert(
                run_id,
                super::PendingTaskRun {
                    peer_id,
                    agent_id: agent.into(),
                    cluster_id: room.into(),
                    task_id: task.id.clone(),
                    lease,
                    action_context: context,
                    task_epoch: Some(active.epoch),
                    terminal: false,
                },
            );
        Ok(Some(run_id))
    }

    fn live_enrolled_subscriber(
        &self,
        room: &str,
        task_id: &str,
        service: &dyn crate::services::WorkspaceTasks,
    ) -> Result<Option<(u64, launcher::managed::ChannelProcessFacts)>, String> {
        let snapshot = checked_snapshot(service, room)?;
        let agent = snapshot
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .and_then(|task| task.assigned_agent.as_deref());
        let Some(agent) = agent else {
            return Ok(None);
        };
        let retained = self
            .enrolled_task_subscribers
            .lock()
            .map_err(|_| "enrolled task subscription registry unavailable")?
            .iter()
            .find(|(_, subscriber)| {
                subscriber.cluster_id == room
                    && subscriber.agent_id == agent
                    && !subscriber.tx.is_closed()
                    && !subscriber
                        .runs
                        .values()
                        .any(|run| !run.reports.values().any(is_terminal_update))
            })
            .map(|(peer, subscriber)| (*peer, subscriber.facts.clone()));
        Ok(retained)
    }

    async fn dispatch_enrolled_workspace_task(
        &self,
        peer: u64,
        facts: launcher::managed::ChannelProcessFacts,
        room: &str,
        task_id: &str,
        service: &dyn crate::services::WorkspaceTasks,
    ) -> Result<Option<u64>, String> {
        let _delivery = self.task_delivery_order.lock().await;
        let snapshot = checked_snapshot(service, room)?;
        let task = snapshot
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .ok_or("queued task disappeared before enrolled delivery")?;
        if task.status != tilo_desktop_model::WorkspaceTaskStatus::Queued {
            return Ok(None);
        }
        let agent = task
            .assigned_agent
            .as_deref()
            .filter(|_| task.route != tilo_desktop_model::WorkspaceTaskRoute::Manual)
            .ok_or("queued task has no Agent route")?
            .to_owned();
        if self.managed_agent_facts(peer, room, &agent)? != facts {
            return Err("managed process facts changed before task delivery".into());
        }
        let control = snapshot
            .controls
            .get(task_id)
            .ok_or("queued task has no owner control")?;
        require_retained_task_instruction(control)?;
        let epoch = control
            .run_epoch
            .checked_add(1)
            .ok_or("task run epoch exhausted")?;
        let (task, control) = start_owner_task_run(room, task_id, &agent, epoch, service).await?;
        let active = control
            .active_run
            .as_ref()
            .filter(|active| active.epoch == epoch && active.agent_id == agent)
            .ok_or("owner did not retain the exact enrolled run")?;
        let run_number = enrolled_run_number(room, task_id, epoch);
        let envelope = enrolled_task_envelope(&facts, room, &task, &control, &agent, epoch)?;
        let tx = {
            let mut subscribers = self
                .enrolled_task_subscribers
                .lock()
                .map_err(|_| "enrolled task subscription registry unavailable")?;
            let subscriber = subscribers
                .get_mut(&peer)
                .filter(|subscriber| {
                    subscriber.cluster_id == room
                        && subscriber.agent_id == agent
                        && subscriber.facts == facts
                })
                .ok_or("enrolled task subscriber changed before delivery")?;
            if subscriber.runs.len() >= MAX_ENROLLED_RUNS_PER_PROCESS {
                return Err("enrolled task retention capacity exhausted".into());
            }
            if subscriber.runs.values().any(|run| {
                run.envelope.task_id == task_id && !run.reports.values().any(is_terminal_update)
            }) {
                return Err("enrolled task already has a live run for this process".into());
            }
            subscriber.runs.insert(
                envelope.message_id.clone(),
                super::EnrolledTaskRun {
                    envelope: envelope.clone(),
                    task_epoch: active.epoch,
                    acceptance: None,
                    report_sequence: 0,
                    reports: std::collections::HashMap::new(),
                },
            );
            let tx = subscriber.tx.clone();
            drop(subscribers);
            tx
        };
        if tx.send(envelope).is_err() {
            if let Ok(mut subscribers) = self.enrolled_task_subscribers.lock()
                && let Some(subscriber) = subscribers.get_mut(&peer)
            {
                subscriber
                    .runs
                    .retain(|_, run| run.envelope.task_id != task_id || run.task_epoch != epoch);
            }
            self.abandon_undelivered_workspace_task(room, task_id, epoch, service)
                .await?;
            return Err("enrolled task stream closed before delivery".into());
        }
        Ok(Some(run_number))
    }

    pub(super) async fn dispatch_queued_workspace_task(
        &self,
        room: &str,
        agent: &str,
    ) -> Result<Option<u64>, String> {
        let Some(service) = self.services.workspace_tasks.as_ref() else {
            // Legacy task streams have their own delivery contract.
            return Ok(None);
        };
        let snapshot = checked_snapshot(service.as_ref(), room)?;
        let Some(task_id) = next_queued_task(&snapshot, agent) else {
            return Ok(None);
        };
        self.dispatch_live_workspace_task(room, task_id, service.as_ref())
            .await
    }

    pub(super) async fn install_enrolled_task_subscription(
        self: &Arc<Self>,
        peer: u64,
        agent: String,
        facts: launcher::managed::ChannelProcessFacts,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<runtime_collab::EnrolledTaskEnvelope>, String>
    {
        let room = facts.binding.room.clone();
        self.services
            .workspace_tasks
            .clone()
            .filter(|service| service.requires_room(&room))
            .ok_or("native task owner unavailable")?;
        if self.managed_agent_facts(peer, &room, &agent)? != facts {
            return Err("managed process facts changed before subscription".into());
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.enrolled_task_subscribers
            .lock()
            .map_err(|_| "enrolled task subscription registry unavailable")?
            .insert(
                peer,
                super::EnrolledTaskSubscription {
                    agent_id: agent.clone(),
                    cluster_id: room.clone(),
                    facts,
                    tx,
                    runs: std::collections::HashMap::new(),
                },
            );
        if let Err(error) = self.dispatch_queued_workspace_task(&room, &agent).await {
            if let Ok(mut subscribers) = self.enrolled_task_subscribers.lock() {
                subscribers.remove(&peer);
            }
            return Err(error);
        }
        Ok(rx)
    }

    pub(super) fn revalidate_enrolled_peer(&self, peer: u64) -> Result<(), String> {
        let (room, agent, facts) = self
            .enrolled_task_subscribers
            .lock()
            .map_err(|_| "enrolled task subscription registry unavailable")?
            .get(&peer)
            .map(|subscriber| {
                (
                    subscriber.cluster_id.clone(),
                    subscriber.agent_id.clone(),
                    subscriber.facts.clone(),
                )
            })
            .ok_or("enrolled task subscription closed")?;
        if self.managed_agent_facts(peer, &room, &agent)? != facts {
            return Err("managed process facts changed".into());
        }
        Ok(())
    }

    pub(super) fn close_enrolled_subscription(&self, peer: u64) {
        if let Ok(mut subscribers) = self.enrolled_task_subscribers.lock() {
            subscribers.remove(&peer);
        }
    }

    pub(crate) async fn abandon_managed_agent_tasks(
        self: &std::sync::Arc<Self>,
        room: &str,
        agent: &str,
        launch: &str,
    ) -> Result<(), String> {
        let (peer, tasks) = {
            let subscribers = self
                .enrolled_task_subscribers
                .lock()
                .map_err(|_| "enrolled task subscription registry unavailable")?;
            let Some((peer, subscriber)) = subscribers.iter().find(|(_, subscriber)| {
                subscriber.cluster_id == room
                    && subscriber.agent_id == agent
                    && subscriber.facts.binding.launch == launch
            }) else {
                // No exact subscriber means this launch never acquired an
                // enrolled task lease. In particular, never fall back to a
                // legacy subscriber or another launch for the same Agent.
                return Ok(());
            };
            let tasks = subscriber
                .runs
                .values()
                .filter(|run| !run.reports.values().any(is_terminal_update))
                .map(|run| (run.envelope.task_id.clone(), run.task_epoch))
                .collect::<Vec<_>>();
            let peer = *peer;
            drop(subscribers);
            (peer, tasks)
        };
        let service = self
            .services
            .workspace_tasks
            .as_ref()
            .filter(|service| service.requires_room(room))
            .ok_or("native task owner unavailable")?;
        for (task, epoch) in tasks {
            self.abandon_undelivered_workspace_task(room, &task, epoch, service.as_ref())
                .await?;
        }
        self.close_enrolled_subscription(peer);
        if let Ok(mut pending) = self.pending_task_runs.lock() {
            pending.retain(|_, run| run.peer_id != peer);
        }
        Ok(())
    }

    pub(super) async fn accept_enrolled_task(
        self: &std::sync::Arc<Self>,
        peer: u64,
        acceptance: &runtime_collab::EnrolledTaskAcceptance,
    ) -> Result<(), String> {
        let _turn = self.enrolled_report_order.lock().await;
        self.revalidate_enrolled_peer(peer)?;
        let (run_id, pending) = self.checked_enrolled_pending(peer, acceptance)?;
        {
            let pending_runs = self
                .pending_task_runs
                .lock()
                .map_err(|_| "task run registry unavailable")?;
            if let Some(prior) = pending_runs.get(&run_id)
                && (prior.peer_id != peer
                    || prior.task_id != pending.task_id
                    || prior.task_epoch != pending.task_epoch)
            {
                return Err("enrolled task run identity collided".into());
            }
        }
        self.record_owner_task_acceptance(&pending).await?;
        self.revalidate_enrolled_peer(peer)?;
        self.pending_task_runs
            .lock()
            .map_err(|_| "task run registry unavailable")?
            .entry(run_id)
            .or_insert(pending);
        let mut subscribers = self
            .enrolled_task_subscribers
            .lock()
            .map_err(|_| "enrolled task subscription registry unavailable")?;
        let run = subscribers
            .get_mut(&peer)
            .and_then(|subscriber| subscriber.runs.get_mut(&acceptance.message_id))
            .ok_or("enrolled acceptance lost its retained delivery")?;
        if run.acceptance.as_ref() == Some(acceptance) {
            return Ok(());
        }
        if run.acceptance.is_some() {
            return Err("enrolled acceptance raced another call".into());
        }
        run.acceptance = Some(acceptance.clone());
        drop(subscribers);
        Ok(())
    }

    fn checked_enrolled_pending(
        &self,
        peer: u64,
        acceptance: &runtime_collab::EnrolledTaskAcceptance,
    ) -> Result<(u64, super::PendingTaskRun), String> {
        let (room, agent, envelope, epoch) = {
            let subscribers = self
                .enrolled_task_subscribers
                .lock()
                .map_err(|_| "enrolled task subscription registry unavailable")?;
            let subscriber = subscribers
                .get(&peer)
                .ok_or("enrolled task subscription closed")?;
            let run = subscriber
                .runs
                .get(&acceptance.message_id)
                .ok_or("enrolled acceptance names an undelivered task")?;
            if !acceptance.matches(&run.envelope) {
                return Err("enrolled acceptance changed its delivered binding".into());
            }
            if run
                .acceptance
                .as_ref()
                .is_some_and(|prior| prior != acceptance)
            {
                return Err("enrolled task was already accepted differently".into());
            }
            let retained = (
                subscriber.cluster_id.clone(),
                subscriber.agent_id.clone(),
                run.envelope.clone(),
                run.task_epoch,
            );
            drop(subscribers);
            retained
        };
        let service = self
            .services
            .workspace_tasks
            .as_ref()
            .filter(|service| service.requires_room(&room))
            .ok_or("native task owner unavailable")?;
        let snapshot = checked_snapshot(service.as_ref(), &room)?;
        let task = snapshot
            .tasks
            .iter()
            .find(|task| task.id == envelope.task_id)
            .ok_or("accepted enrolled task no longer exists")?;
        let active = snapshot
            .controls
            .get(&task.id)
            .and_then(|control| control.active_run.as_ref())
            .filter(|active| active.epoch == epoch && active.agent_id == agent)
            .ok_or("accepted enrolled task run is stale")?;
        let pending = super::PendingTaskRun {
            peer_id: peer,
            agent_id: agent,
            cluster_id: room,
            task_id: task.id.clone(),
            lease: envelope.task.lease.clone(),
            action_context: serde_json::from_value(envelope.task.action_context.clone())
                .map_err(|error| error.to_string())?,
            task_epoch: Some(active.epoch),
            terminal: false,
        };
        Ok((envelope.task.run_id, pending))
    }

    async fn record_owner_task_acceptance(
        &self,
        run: &super::PendingTaskRun,
    ) -> Result<(), String> {
        self.record_owner_task_execution_phase(
            run,
            0,
            tilo_desktop_model::WorkspaceTaskExecutionPhase::Accepted,
        )
        .await
    }

    async fn record_owner_task_execution_phase(
        &self,
        run: &super::PendingTaskRun,
        sequence: u64,
        phase: tilo_desktop_model::WorkspaceTaskExecutionPhase,
    ) -> Result<(), String> {
        let epoch = run
            .task_epoch
            .ok_or("canonical task acceptance has no owner epoch")?;
        let service = self
            .services
            .workspace_tasks
            .as_ref()
            .ok_or("canonical task owner unavailable")?;
        let encoded = serde_json::to_vec(&(sequence, &phase)).map_err(|error| error.to_string())?;
        let invocation = update_invocation(&run.cluster_id, &run.task_id, epoch, &encoded);
        let request = OwnerCommitRequest {
            room: run.cluster_id.clone(),
            operation: CommitOperation::WorkspaceTaskCommand {
                invocation,
                authority: WorkspaceTaskAuthority::Agent {
                    attestor: service.host_certificate(&run.cluster_id)?,
                    agent_id: run.agent_id.clone(),
                },
                command: WorkspaceTaskCommand::ExecutionPhase {
                    task_id: run.task_id.clone(),
                    run_epoch: epoch,
                    sequence,
                    phase,
                },
            },
        };
        let receipt = service.execute(request.clone()).await?;
        receipt.validate_for(&request)?;
        let snapshot = checked_snapshot(service.as_ref(), &run.cluster_id)?;
        let execution = snapshot
            .controls
            .get(&run.task_id)
            .and_then(|control| control.execution.as_ref())
            .ok_or("owner did not retain the Agent acceptance phase")?;
        if execution.run_epoch != epoch
            || execution.sequence != sequence
            || execution.phase != phase
        {
            return Err("owner retained a different Agent execution phase".into());
        }
        Ok(())
    }

    pub(super) async fn report_enrolled_task(
        self: &Arc<Self>,
        peer: u64,
        acceptance: runtime_collab::EnrolledTaskAcceptance,
        sequence: u64,
        update: runtime_collab::TaskRunUpdate,
    ) -> Result<(), String> {
        if sequence == 0 {
            return Err("enrolled task report sequence is zero".into());
        }
        // Validation, durable owner mutation and local receipt retention are
        // one serialized turn. Two RPCs must not both pass the same frontier
        // and mutate the owner before either report becomes observable here.
        let _turn = self.enrolled_report_order.lock().await;
        self.revalidate_enrolled_peer(peer)?;
        let (envelope, epoch) = {
            let subscribers = self
                .enrolled_task_subscribers
                .lock()
                .map_err(|_| "enrolled task subscription registry unavailable")?;
            let run = subscribers
                .get(&peer)
                .and_then(|subscriber| subscriber.runs.get(&acceptance.message_id))
                .ok_or("enrolled report names an undelivered task")?;
            if run.acceptance.as_ref() != Some(&acceptance) || !acceptance.matches(&run.envelope) {
                return Err("enrolled report changed its acceptance binding".into());
            }
            if let Some(prior) = run.reports.get(&sequence) {
                return if prior == &update {
                    Ok(())
                } else {
                    Err("enrolled report sequence was reused differently".into())
                };
            }
            if sequence != run.report_sequence.saturating_add(1) {
                return Err("enrolled report sequence is not contiguous".into());
            }
            let terminal = is_terminal_update(&update);
            if run.reports.len()
                >= MAX_ENROLLED_REPORTS_PER_RUN.saturating_sub(usize::from(!terminal))
            {
                return Err("enrolled task report capacity exhausted".into());
            }
            let retained = (run.envelope.clone(), run.task_epoch);
            drop(subscribers);
            retained
        };
        let pending = self
            .pending_task_runs
            .lock()
            .map_err(|_| "task run registry unavailable")?
            .get(&envelope.task.run_id)
            .cloned()
            .filter(|run| run.peer_id == peer && run.task_epoch == Some(epoch))
            .ok_or("accepted enrolled task run disappeared")?;
        if let Some(phase) = reported_execution_phase(&update) {
            self.record_owner_task_execution_phase(&pending, sequence, phase)
                .await?;
        }
        self.record_owner_task_run_update(envelope.task.run_id, &pending, update.clone())
            .await?;
        self.revalidate_enrolled_peer(peer)?;
        let terminal_target = {
            let mut subscribers = self
                .enrolled_task_subscribers
                .lock()
                .map_err(|_| "enrolled task subscription registry unavailable")?;
            let subscriber = subscribers
                .get_mut(&peer)
                .ok_or("enrolled task subscription closed")?;
            let run = subscriber
                .runs
                .get_mut(&acceptance.message_id)
                .ok_or("enrolled report lost its retained run")?;
            run.reports.insert(sequence, update.clone());
            run.report_sequence = sequence;
            let target = is_terminal_update(&update)
                .then(|| (subscriber.cluster_id.clone(), subscriber.agent_id.clone()));
            drop(subscribers);
            target
        };
        if let Some((room, agent)) = terminal_target
            && let Err(error) = self.dispatch_queued_workspace_task(&room, &agent).await
        {
            tracing::warn!(%error, "queued enrolled task delivery deferred");
        }
        Ok(())
    }

    fn live_task_subscriber(
        &self,
        room: &str,
        agent: &str,
    ) -> Result<
        Option<(
            u64,
            tokio::sync::mpsc::UnboundedSender<super::TaskRunEnvelope>,
        )>,
        String,
    > {
        Ok(self
            .task_subscribers
            .lock()
            .map_err(|_| "task subscription registry unavailable")?
            .iter()
            .find(|(_, subscriber)| {
                subscriber.cluster_id == room
                    && subscriber.agent_id == agent
                    && !subscriber.tx.is_closed()
            })
            .map(|(peer, subscriber)| (*peer, subscriber.tx.clone())))
    }

    async fn abandon_undelivered_workspace_task(
        &self,
        room: &str,
        task_id: &str,
        epoch: u64,
        service: &dyn crate::services::WorkspaceTasks,
    ) -> Result<(), String> {
        let invocation = update_invocation(room, task_id, epoch, b"delivery-closed");
        let request = OwnerCommitRequest {
            room: room.into(),
            operation: CommitOperation::WorkspaceTaskCommand {
                invocation,
                authority: WorkspaceTaskAuthority::Host {
                    attestor: service.host_certificate(room)?,
                },
                command: WorkspaceTaskCommand::Abandon {
                    task_id: task_id.into(),
                    run_epoch: epoch,
                    reason: "Agent disconnected before delivery".into(),
                },
            },
        };
        let receipt = service.execute(request.clone()).await?;
        receipt.validate_for(&request)?;
        let snapshot = checked_snapshot(service, room)?;
        let task = snapshot
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .ok_or("undelivered task disappeared")?;
        if task.status != tilo_desktop_model::WorkspaceTaskStatus::Waiting {
            return Err("owner did not retain the undelivered task as waiting".into());
        }
        Ok(())
    }

    pub(super) async fn record_owner_task_run_update(
        &self,
        run_id: u64,
        run: &super::PendingTaskRun,
        update: runtime_collab::TaskRunUpdate,
    ) -> Result<(), String> {
        let Some(epoch) = run.task_epoch else {
            return Err("canonical task run has no owner epoch".into());
        };
        if let runtime_collab::TaskRunUpdate::DirectionApplied { revision } = &update {
            let frontier = self.owner_task_run_control(run)?;
            if frontier.applied_revision == *revision {
                // The Agent may retry the same acknowledgement after losing
                // the transport reply. The owner frontier is the durable
                // receipt; never append a second acknowledgement command.
                return Ok(());
            }
            if frontier
                .directions
                .last()
                .map(|direction| direction.revision)
                != Some(*revision)
            {
                return Err(
                    "direction acknowledgement is not the latest delivered frontier".into(),
                );
            }
        }
        let Some(command) = owner_update_command(&run.task_id, epoch, &update)? else {
            return Ok(());
        };
        let service = self
            .services
            .workspace_tasks
            .as_ref()
            .ok_or("canonical task owner unavailable")?;
        let encoded = serde_json::to_vec(&update).map_err(|error| error.to_string())?;
        let invocation = update_invocation(&run.cluster_id, &run.task_id, epoch, &encoded);
        let request = OwnerCommitRequest {
            room: run.cluster_id.clone(),
            operation: CommitOperation::WorkspaceTaskCommand {
                invocation,
                authority: WorkspaceTaskAuthority::Agent {
                    attestor: service.host_certificate(&run.cluster_id)?,
                    agent_id: run.agent_id.clone(),
                },
                command,
            },
        };
        let receipt = service.execute(request.clone()).await?;
        receipt.validate_for(&request)?;
        let snapshot = checked_snapshot(service.as_ref(), &run.cluster_id)?;
        let task = snapshot
            .tasks
            .iter()
            .find(|task| task.id == run.task_id)
            .ok_or("finished task disappeared from owner projection")?;
        let applied = matches!(
            update,
            runtime_collab::TaskRunUpdate::DirectionApplied { .. }
        );
        if applied && task.status != tilo_desktop_model::WorkspaceTaskStatus::InProgress {
            return Err("owner did not retain the active task direction frontier".into());
        }
        if !applied
            && !task.status.is_finished()
            && task.status != tilo_desktop_model::WorkspaceTaskStatus::Waiting
        {
            return Err("owner did not retain the terminal task state".into());
        }
        if applied {
            return Ok(());
        }
        {
            let mut runs = self
                .pending_task_runs
                .lock()
                .map_err(|_| "task run registry unavailable")?;
            let current = runs
                .get_mut(&run_id)
                .filter(|current| current.lease == run.lease && current.task_epoch == Some(epoch))
                .ok_or("task run changed before terminal owner receipt")?;
            if current.terminal {
                return Err("task run is already terminal".into());
            }
            current.terminal = true;
            drop(runs);
        }
        // The terminal owner receipt, not a UI status or a disconnected stream,
        // releases the next instruction. A failed delivery must not turn this
        // already-durable completion into a failed acknowledgement.
        if let Err(error) = self
            .dispatch_queued_workspace_task(&run.cluster_id, &run.agent_id)
            .await
        {
            tracing::warn!(%error, "queued task delivery deferred after terminal owner receipt");
        }
        Ok(())
    }

    pub(super) fn owner_task_run_control(
        &self,
        run: &super::PendingTaskRun,
    ) -> Result<runtime_collab::TaskRunControl, String> {
        let epoch = run
            .task_epoch
            .ok_or("canonical task run has no owner epoch")?;
        let service = self
            .services
            .workspace_tasks
            .as_ref()
            .ok_or("canonical task owner unavailable")?;
        let snapshot = checked_snapshot(service.as_ref(), &run.cluster_id)?;
        let task = snapshot
            .tasks
            .iter()
            .find(|task| task.id == run.task_id)
            .ok_or("canonical task disappeared")?;
        let control = snapshot
            .controls
            .get(&run.task_id)
            .ok_or("canonical task control disappeared")?;
        let active = control
            .active_run
            .as_ref()
            .filter(|active| active.epoch == epoch && active.agent_id == run.agent_id)
            .ok_or("canonical task run is stale")?;
        let directions = control
            .directions
            .iter()
            .filter(|direction| direction.revision > active.applied_direction_revision)
            .map(|direction| runtime_collab::TaskRunDirection {
                revision: direction.revision,
                text: direction.text.clone(),
            })
            .collect::<Vec<_>>();
        let original = control.instruction.as_deref().unwrap_or(&task.title);
        let total_bytes = direction_extent(original.len(), &directions, str::len)?;
        let total_chars = direction_extent(original.chars().count(), &directions, |text| {
            text.chars().count()
        })?;
        if directions.len() > 64
            || total_bytes > tilo_desktop_model::MAX_WORKSPACE_TASK_INSTRUCTION_BYTES
            || total_chars > 8_000
        {
            return Err("pending task directions exceed the control carrier bound".into());
        }
        Ok(runtime_collab::TaskRunControl {
            version: 1,
            run_epoch: epoch,
            applied_revision: active.applied_direction_revision,
            directions,
            cancel_requested: control
                .cancellation
                .as_ref()
                .is_some_and(|request| request.run_epoch == epoch),
        })
    }

    pub(super) fn validate_owner_task_action_frontier(
        &self,
        run: &super::PendingTaskRun,
    ) -> Result<(), String> {
        let Some(epoch) = run.task_epoch else {
            return Ok(());
        };
        let service = self
            .services
            .workspace_tasks
            .as_ref()
            .ok_or("canonical task owner unavailable")?;
        let snapshot = checked_snapshot(service.as_ref(), &run.cluster_id)?;
        let control = snapshot
            .controls
            .get(&run.task_id)
            .ok_or("canonical task control disappeared")?;
        let active = control
            .active_run
            .as_ref()
            .filter(|active| active.epoch == epoch && active.agent_id == run.agent_id)
            .ok_or("canonical task run is stale")?;
        if control
            .cancellation
            .as_ref()
            .is_some_and(|request| request.run_epoch == epoch)
        {
            return Err("task cancellation was requested before another action".into());
        }
        let latest = control
            .directions
            .last()
            .map_or(0, |direction| direction.revision);
        if active.applied_direction_revision != latest {
            return Err("new task direction must be applied before another action".into());
        }
        Ok(())
    }
}

async fn start_owner_task_run(
    room: &str,
    task_id: &str,
    agent: &str,
    epoch: u64,
    service: &dyn crate::services::WorkspaceTasks,
) -> Result<
    (
        tilo_desktop_model::WorkspaceTask,
        tilo_desktop_model::WorkspaceTaskControl,
    ),
    String,
> {
    let request = OwnerCommitRequest {
        room: room.into(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: run_invocation(room, task_id, epoch),
            authority: WorkspaceTaskAuthority::Host {
                attestor: service.host_certificate(room)?,
            },
            command: WorkspaceTaskCommand::StartRun {
                task_id: task_id.into(),
                agent_id: agent.into(),
            },
        },
    };
    let receipt = service.execute(request.clone()).await?;
    receipt.validate_for(&request)?;
    let snapshot = checked_snapshot(service, room)?;
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .cloned()
        .ok_or("started task disappeared")?;
    let control = snapshot
        .controls
        .get(task_id)
        .cloned()
        .ok_or("started task has no owner control")?;
    Ok((task, control))
}

fn enrolled_task_envelope(
    facts: &launcher::managed::ChannelProcessFacts,
    room: &str,
    task: &tilo_desktop_model::WorkspaceTask,
    control: &tilo_desktop_model::WorkspaceTaskControl,
    agent: &str,
    epoch: u64,
) -> Result<runtime_collab::EnrolledTaskEnvelope, String> {
    let epoch_label = epoch.to_string();
    let envelope = runtime_collab::EnrolledTaskEnvelope {
        version: 1,
        message_id: enrolled_label(
            b"tilo/enrolled-task-message/v1",
            &[room, &task.id, &epoch_label],
        ),
        task_id: task.id.clone(),
        run_id: format!("run-{epoch}"),
        launch_id: facts.binding.launch.clone(),
        runtime_epoch: facts.runtime.runtime_epoch.clone(),
        recipient_agent: agent.into(),
        channel_lease: facts.channel_id.clone(),
        task: runtime_collab::TaskRunEnvelope {
            version: 1,
            task_id: task.id.clone(),
            title: task.title.clone(),
            instruction: control.instruction.clone(),
            run_id: enrolled_run_number(room, &task.id, epoch),
            lease: enrolled_label(
                b"tilo/enrolled-task-lease/v1",
                &[&facts.channel_id, &task.id, &epoch_label],
            ),
            planning: task_run_planning(task.route)?,
            action_context: serde_json::to_value(enrolled_task_action_context(task))
                .map_err(|error| error.to_string())?,
        },
        history: None,
    };
    envelope.validate()?;
    Ok(envelope)
}

/// A managed task already carries its complete private instruction in the
/// owner-admitted envelope. Its action context is only an opaque correlation
/// key: copying the display title into `purpose` or `group.label` both leaks
/// private task text into later action attribution and violates the Agent's
/// enrolled-input contract.
fn enrolled_task_action_context(
    task: &tilo_desktop_model::WorkspaceTask,
) -> tilo_actions::ActionContext {
    tilo_actions::ActionContext::default().with_group(&task.id, &task.id, "task")
}

fn require_retained_task_instruction(
    control: &tilo_desktop_model::WorkspaceTaskControl,
) -> Result<(), String> {
    if control.instruction_required && control.instruction.is_none() {
        return Err("queued task lost its required instruction".into());
    }
    Ok(())
}

fn agent_has_active_run(snapshot: &crate::services::WorkspaceTaskSnapshot, agent: &str) -> bool {
    snapshot.tasks.iter().any(|task| {
        task.status == tilo_desktop_model::WorkspaceTaskStatus::InProgress
            && snapshot.controls.get(&task.id).is_some_and(|control| {
                control
                    .active_run
                    .as_ref()
                    .is_some_and(|run| run.agent_id == agent)
            })
    })
}

fn next_queued_task<'a>(
    snapshot: &'a crate::services::WorkspaceTaskSnapshot,
    agent: &str,
) -> Option<&'a str> {
    snapshot
        .tasks
        .iter()
        .filter(|task| {
            task.status == tilo_desktop_model::WorkspaceTaskStatus::Queued
                && task.assigned_agent.as_deref() == Some(agent)
                && task.route != tilo_desktop_model::WorkspaceTaskRoute::Manual
        })
        .min_by_key(|task| task.queue_order_key(snapshot.controls.get(&task.id)))
        .map(|task| task.id.as_str())
}

fn owner_update_command(
    task_id: &str,
    epoch: u64,
    update: &runtime_collab::TaskRunUpdate,
) -> Result<Option<WorkspaceTaskCommand>, String> {
    let finish = |outcome, message: &str| {
        Ok(Some(WorkspaceTaskCommand::Finish {
            task_id: task_id.into(),
            run_epoch: epoch,
            outcome,
            summary: bounded_summary(message)?,
        }))
    };
    match update {
        runtime_collab::TaskRunUpdate::Accepted
        | runtime_collab::TaskRunUpdate::Planning {}
        | runtime_collab::TaskRunUpdate::Executing {}
        | runtime_collab::TaskRunUpdate::Progress { .. } => Ok(None),
        runtime_collab::TaskRunUpdate::DirectionApplied { revision } => {
            Ok(Some(WorkspaceTaskCommand::AppliedAck {
                task_id: task_id.into(),
                run_epoch: epoch,
                revision: *revision,
            }))
        }
        runtime_collab::TaskRunUpdate::Succeeded { message } => finish(
            tilo_peer_state::workspace_task::WorkspaceTaskFinish::Done,
            message,
        ),
        runtime_collab::TaskRunUpdate::Failed { message } => finish(
            tilo_peer_state::workspace_task::WorkspaceTaskFinish::Waiting,
            message,
        ),
        runtime_collab::TaskRunUpdate::Cancelled { message } => finish(
            tilo_peer_state::workspace_task::WorkspaceTaskFinish::Cancelled,
            message,
        ),
    }
}

fn direction_extent(
    initial: usize,
    directions: &[runtime_collab::TaskRunDirection],
    measure: impl Fn(&str) -> usize,
) -> Result<usize, String> {
    directions.iter().try_fold(initial, |total, direction| {
        total
            .checked_add(measure(&direction.text))
            .and_then(|value| value.checked_add(3))
            .ok_or_else(|| "task direction extent overflow".into())
    })
}

fn run_invocation(room: &str, task: &str, epoch: u64) -> uuid::Uuid {
    let mut hash = blake3::Hasher::new_derive_key("Tilo workspace task run v1");
    hash.update(room.as_bytes());
    hash.update(task.as_bytes());
    hash.update(&epoch.to_le_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.finalize().as_bytes()[..16]);
    uuid::Uuid::from_bytes(bytes)
}

fn task_run_planning(
    route: tilo_desktop_model::WorkspaceTaskRoute,
) -> Result<runtime_collab::TaskRunPlanning, String> {
    match route {
        tilo_desktop_model::WorkspaceTaskRoute::RulesOnly => {
            Ok(runtime_collab::TaskRunPlanning::RulesOnly)
        }
        tilo_desktop_model::WorkspaceTaskRoute::Model => Ok(runtime_collab::TaskRunPlanning::Model),
        tilo_desktop_model::WorkspaceTaskRoute::Manual => {
            Err("a manual task cannot be dispatched to an Agent".into())
        }
    }
}

fn bounded_summary(message: &str) -> Result<String, String> {
    if message.trim().is_empty()
        || message.len() > tilo_desktop_model::MAX_WORKSPACE_TASK_SUMMARY_BYTES
    {
        return Err("task outcome is empty or too long".into());
    }
    Ok(message.to_owned())
}

fn update_invocation(room: &str, task: &str, epoch: u64, update: &[u8]) -> uuid::Uuid {
    let mut hash = blake3::Hasher::new_derive_key("Tilo workspace task update v1");
    hash.update(room.as_bytes());
    hash.update(task.as_bytes());
    hash.update(&epoch.to_le_bytes());
    hash.update(update);
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.finalize().as_bytes()[..16]);
    uuid::Uuid::from_bytes(bytes)
}

fn checked_snapshot(
    service: &dyn crate::services::WorkspaceTasks,
    room: &str,
) -> Result<crate::services::WorkspaceTaskSnapshot, String> {
    let snapshot = service.snapshot(room)?;
    if snapshot.room != room || snapshot.generation == 0 {
        return Err("task owner returned an unbound room projection".into());
    }
    Ok(snapshot)
}

fn person_command(envelope: &InvocationEnvelope) -> Result<WorkspaceTaskCommand, String> {
    let encoded = bounded_argument(envelope)?;
    if envelope.resource == COLLECTION && envelope.action == "queue" {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Queue {
            instruction: String,
            #[serde(default)]
            title: Option<String>,
            agent: String,
            route: tilo_desktop_model::WorkspaceTaskRoute,
        }
        let input: Queue = serde_json::from_str(encoded).map_err(|error| error.to_string())?;
        let title = input
            .title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| instruction_title(&input.instruction));
        return Ok(WorkspaceTaskCommand::QueueTask {
            title,
            instruction: input.instruction,
            assigned_agent: input.agent,
            route: input.route,
            resource: None,
        });
    }
    if let Some(task_id) = valid_task_target(envelope)
        && envelope.action == "direct"
    {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Direction {
            run_epoch: u64,
            after_revision: u64,
            text: String,
        }
        let input: Direction = serde_json::from_str(encoded).map_err(|error| error.to_string())?;
        return Ok(WorkspaceTaskCommand::DirectionForRun {
            task_id: task_id.into(),
            run_epoch: input.run_epoch,
            after_revision: input.after_revision,
            text: input.text,
        });
    }
    if let Some(task_id) = valid_task_target(envelope)
        && envelope.action == "stop"
    {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Stop {
            run_epoch: u64,
            after_revision: u64,
        }
        let input: Stop = serde_json::from_str(encoded).map_err(|error| error.to_string())?;
        return Ok(WorkspaceTaskCommand::CancelRun {
            task_id: task_id.into(),
            run_epoch: input.run_epoch,
            after_revision: input.after_revision,
        });
    }
    Err("unknown person task command".into())
}

fn bounded_argument(envelope: &InvocationEnvelope) -> Result<&str, String> {
    let encoded = envelope
        .argument
        .as_deref()
        .ok_or("task arguments missing")?;
    if encoded.len() > MAX_TASK_ARGUMENT_BYTES {
        return Err("task arguments exceed bounded size".into());
    }
    Ok(encoded
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .unwrap_or(encoded))
}

fn instruction_title(instruction: &str) -> String {
    instruction
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("Task")
        .trim()
        .chars()
        .take(80)
        .collect()
}

fn valid_task_target(envelope: &InvocationEnvelope) -> Option<&str> {
    envelope
        .resource
        .strip_prefix(TASK_PREFIX)
        .filter(|task| !task.is_empty() && task.len() <= 256)
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
    room: &str,
    envelope: &InvocationEnvelope,
    service: &dyn crate::services::WorkspaceTasks,
) -> Result<serde_json::Value, String> {
    let task_id = valid_task_target(envelope).ok_or("task read target is invalid")?;
    if envelope.argument.is_some() {
        return Err("task read accepts no arguments".into());
    }
    let snapshot = checked_snapshot(service, room)?;
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .ok_or("task is absent from the current owner projection")?;
    let control = snapshot.controls.get(task_id);
    let active = control.and_then(|control| control.active_run.as_ref());
    let execution =
        control.and_then(|control| active.and_then(|active| active_execution(control, active)));
    let after_revision = control
        .and_then(|control| control.directions.last())
        .map_or(0, |direction| direction.revision);
    Ok(serde_json::json!({
        "generation": snapshot.generation,
        "task_id": task.id,
        "title": task.title,
        "assigned_agent": task.assigned_agent,
        "route": task.route,
        "status": task.status,
        "run_epoch": control.map(|control| control.run_epoch),
        "active_agent": active.map(|run| &run.agent_id),
        "after_revision": after_revision,
        "applied_revision": active.map(|run| run.applied_direction_revision),
        "execution": execution,
        "cancellation_requested": control
            .and_then(|control| control.cancellation.as_ref())
            .is_some_and(|request| active.is_some_and(|run| run.epoch == request.run_epoch)),
    }))
}

fn read_current_tasks(
    room: &str,
    envelope: &InvocationEnvelope,
    service: &dyn crate::services::WorkspaceTasks,
) -> Result<serde_json::Value, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Query {
        agent: String,
    }
    let encoded = bounded_argument(envelope)?;
    let query: Query = serde_json::from_str(encoded).map_err(|error| error.to_string())?;
    if query.agent.is_empty()
        || query.agent.len() > 128
        || query.agent.chars().any(char::is_control)
    {
        return Err("current task query agent is invalid".into());
    }
    let snapshot = checked_snapshot(service, room)?;
    let mut pending = snapshot
        .tasks
        .iter()
        .filter(|task| {
            task.assigned_agent.as_deref() == Some(query.agent.as_str())
                && matches!(
                    task.status,
                    tilo_desktop_model::WorkspaceTaskStatus::Queued
                        | tilo_desktop_model::WorkspaceTaskStatus::Waiting
                )
        })
        .collect::<Vec<_>>();
    pending.sort_by_key(|task| task.queue_order_key(snapshot.controls.get(&task.id)));
    let mut pending = pending
        .into_iter()
        .map(|task| {
            serde_json::json!({
                "task_id": task.id,
                "title": task.title,
                "assigned_agent": task.assigned_agent,
                "status": task.status,
                "route": task.route,
            })
        })
        .collect::<Vec<_>>();
    let pending_total =
        u64::try_from(pending.len()).map_err(|_| "pending task count exceeds u64")?;
    let pending_complete = pending.len() <= MAX_CURRENT_TASK_FRONTIERS;
    pending.truncate(MAX_CURRENT_TASK_FRONTIERS);
    let mut tasks = snapshot
        .tasks
        .iter()
        .filter_map(|task| {
            let control = snapshot.controls.get(&task.id)?;
            let active = control.active_run.as_ref()?;
            (task.status == tilo_desktop_model::WorkspaceTaskStatus::InProgress
                && task.assigned_agent.as_deref() == Some(query.agent.as_str())
                && active.agent_id == query.agent)
                .then(|| {
                    serde_json::json!({
                        "task_id": task.id,
                        "title": task.title,
                        "assigned_agent": task.assigned_agent,
                        "status": task.status,
                        "route": task.route,
                        "run_epoch": active.epoch,
                        "active_agent": active.agent_id,
                        "after_revision": control.directions.last().map_or(0, |value| value.revision),
                        "applied_revision": active.applied_direction_revision,
                        "execution": active_execution(control, active),
                        "cancellation_requested": control.cancellation.as_ref().is_some_and(|value| value.run_epoch == active.epoch),
                    })
                })
        })
        .collect::<Vec<_>>();
    tasks.sort_by(|left, right| left["task_id"].as_str().cmp(&right["task_id"].as_str()));
    let total = u64::try_from(tasks.len()).map_err(|_| "current task count exceeds u64")?;
    let complete = tasks.len() <= MAX_CURRENT_TASK_FRONTIERS;
    tasks.truncate(MAX_CURRENT_TASK_FRONTIERS);
    Ok(serde_json::json!({
        "generation": snapshot.generation,
        "complete": complete,
        "total": total,
        "tasks": tasks,
        "pending_complete": pending_complete,
        "pending_total": pending_total,
        "pending": pending,
    }))
}

fn task_actions(
    tasks: &[tilo_desktop_model::WorkspaceTask],
    controls: &std::collections::BTreeMap<String, tilo_desktop_model::WorkspaceTaskControl>,
) -> Vec<PublishedAction> {
    let mut actions = vec![current_tasks_action(), queue_action(), start_agent_action()];
    for task in tasks {
        actions.push(task_read_action(task));
        let active = controls
            .get(&task.id)
            .filter(|control| control.active_run.is_some() && control.cancellation.is_none());
        if task.status == tilo_desktop_model::WorkspaceTaskStatus::InProgress && active.is_some() {
            if task.route == tilo_desktop_model::WorkspaceTaskRoute::Model {
                actions.push(effect_action(
                    &format!("{TASK_PREFIX}{}", task.id),
                    "direct",
                    &task.title,
                    "Direct task",
                    vec![
                        tilo_actions::ActionParam::required(
                            "run_epoch",
                            tilo_actions::ParamType::Integer,
                            "Run epoch returned by read-task",
                        ),
                        tilo_actions::ActionParam::required(
                            "after_revision",
                            tilo_actions::ParamType::Integer,
                            "Direction revision returned by read-task",
                        ),
                        tilo_actions::ActionParam::required(
                            "text",
                            tilo_actions::ParamType::Text,
                            "New direction for this exact run",
                        ),
                    ],
                ));
            }
            actions.push(effect_action(
                &format!("{TASK_PREFIX}{}", task.id),
                "stop",
                &task.title,
                "Stop task",
                vec![
                    tilo_actions::ActionParam::required(
                        "run_epoch",
                        tilo_actions::ParamType::Integer,
                        "Run epoch returned by read-task",
                    ),
                    tilo_actions::ActionParam::required(
                        "after_revision",
                        tilo_actions::ParamType::Integer,
                        "Direction revision returned by read-task",
                    ),
                ],
            ));
        }
    }
    actions
}

fn startup_agent(envelope: &InvocationEnvelope) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Arguments {
        agent: String,
    }
    let arguments: Arguments = serde_json::from_str(bounded_argument(envelope)?)
        .map_err(|_| "invalid Agent startup arguments")?;
    tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::RemoveAgent {
        agent: arguments.agent.clone(),
    }
    .validate()?;
    Ok(arguments.agent)
}

fn start_agent_action() -> PublishedAction {
    let mut action = effect_action(
        COLLECTION,
        "start-agent",
        "Tasks",
        "Start agent",
        vec![tilo_actions::ActionParam::required(
            "agent",
            tilo_actions::ParamType::Text,
            "Already admitted workspace Agent",
        )],
    );
    action.contract.permission = tilo_actions::PermissionRequirement::Admin;
    action
}

fn current_tasks_action() -> PublishedAction {
    PublishedAction {
        resource: COLLECTION.into(),
        semantic_identity: None,
        action: "read-current-tasks".into(),
        resource_kind: "task.collection".into(),
        resource_label: "Tasks".into(),
        action_label: "Read current agent tasks".into(),
        description: Some("Read active task run and direction frontiers for one agent".into()),
        operation_kind: tilo_actions::OperationKind::View,
        params: vec![tilo_actions::ActionParam::required(
            "agent",
            tilo_actions::ParamType::Text,
            "Admitted workspace agent",
        )],
        contract: tilo_actions::ActionContract {
            permission: tilo_actions::PermissionRequirement::Read,
            result: tilo_actions::ResultSpec::Json {
                schema: serde_json::json!({"type": "object"}),
            },
            ..tilo_actions::ActionContract::view()
        },
        view: None,
        input: PublishedInput::None,
    }
}

fn queue_action() -> PublishedAction {
    effect_action(
        COLLECTION,
        "queue",
        "Tasks",
        "Queue task",
        vec![
            tilo_actions::ActionParam::required(
                "instruction",
                tilo_actions::ParamType::Text,
                "Complete task instruction (up to 32 KiB and 8000 characters)",
            ),
            tilo_actions::ActionParam::optional(
                "title",
                tilo_actions::ParamType::Text,
                "Short Task Center title; derived from the instruction when omitted",
            ),
            tilo_actions::ActionParam::required(
                "agent",
                tilo_actions::ParamType::Text,
                "Assigned admitted workspace Agent",
            ),
            tilo_actions::ActionParam::required(
                "route",
                tilo_actions::ParamType::Text,
                "model or rules_only",
            ),
        ],
    )
}

fn task_read_action(task: &tilo_desktop_model::WorkspaceTask) -> PublishedAction {
    PublishedAction {
        resource: format!("{TASK_PREFIX}{}", task.id),
        semantic_identity: None,
        action: "read-task".into(),
        resource_kind: "task".into(),
        resource_label: task.title.clone(),
        action_label: "Read current task".into(),
        description: Some("Read the owner-confirmed run and direction frontier".into()),
        operation_kind: tilo_actions::OperationKind::View,
        params: Vec::new(),
        contract: tilo_actions::ActionContract {
            permission: tilo_actions::PermissionRequirement::Read,
            result: tilo_actions::ResultSpec::Json {
                schema: serde_json::json!({"type": "object"}),
            },
            ..tilo_actions::ActionContract::view()
        },
        view: None,
        input: PublishedInput::None,
    }
}

fn effect_action(
    resource: &str,
    action: &str,
    label: &str,
    action_label: &str,
    params: Vec<tilo_actions::ActionParam>,
) -> PublishedAction {
    PublishedAction {
        resource: resource.into(),
        semantic_identity: None,
        action: action.into(),
        resource_kind: "task".into(),
        resource_label: label.into(),
        action_label: action_label.into(),
        description: None,
        operation_kind: tilo_actions::OperationKind::Effect,
        params,
        contract: tilo_actions::ActionContract {
            permission: tilo_actions::PermissionRequirement::Write,
            result: tilo_actions::ResultSpec::Json {
                schema: serde_json::json!({"type": "object"}),
            },
            ..Default::default()
        },
        view: None,
        input: PublishedInput::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Mutex, time::SystemTime};
    use tilo_peer_state::TransportOrigin;

    struct ReadOnlyTasks(crate::services::WorkspaceTaskSnapshot);

    #[derive(Clone)]
    struct NativeTasks {
        owner: tilo_joined_room::NativeWorkspaceOwner,
        room: tilo_desktop_model::RoomId,
        now: SystemTime,
    }

    struct NativeRunFixture {
        _directory: tempfile::TempDir,
        service: Arc<dyn crate::services::WorkspaceTasks>,
        state: Arc<CapnpBrokerState>,
        receiver: tokio::sync::mpsc::UnboundedReceiver<super::super::TaskRunEnvelope>,
        caller: tilo_user_identity::DeviceCert,
    }

    #[test]
    fn enrolled_task_context_is_opaque_and_exactly_correlated() {
        let mut task = task("task:opaque-1", "scribe");
        task.title = "private task title".into();
        let context = enrolled_task_action_context(&task);
        assert!(context.purpose.is_none());
        assert!(context.resource.is_none());
        assert!(context.app.is_none());
        assert_eq!(
            context.group,
            Some(tilo_actions::ActionGroup {
                id: "task:opaque-1".into(),
                label: "task:opaque-1".into(),
                kind: "task".into(),
            })
        );
        assert!(
            !serde_json::to_string(&context)
                .unwrap()
                .contains("private task title")
        );
    }

    impl crate::services::WorkspaceTasks for ReadOnlyTasks {
        fn requires_room(&self, room: &str) -> bool {
            room == self.0.room
        }

        fn snapshot(&self, room: &str) -> Result<crate::services::WorkspaceTaskSnapshot, String> {
            (room == self.0.room)
                .then(|| self.0.clone())
                .ok_or_else(|| "wrong test room".into())
        }

        fn host_certificate(&self, _room: &str) -> Result<tilo_user_identity::DeviceCert, String> {
            Err("read-only fixture has no host authority".into())
        }

        fn execute(
            &self,
            _request: tilo_peer_state::owner_commit::OwnerCommitRequest,
        ) -> tilo_peer_state::owner_commit::CommitFuture {
            Box::pin(async { Err("read-only fixture cannot mutate".into()) })
        }
    }

    impl crate::services::WorkspaceTasks for NativeTasks {
        fn requires_room(&self, room: &str) -> bool {
            room == tilo_joined_room::testing::ROOM
        }

        fn snapshot(&self, room: &str) -> Result<crate::services::WorkspaceTaskSnapshot, String> {
            if !self.requires_room(room) {
                return Err("wrong native task room".into());
            }
            self.owner.with_current(self.room, |view| {
                let state = tilo_peer_state::workspace_task::snapshot(
                    view.peer()
                        .admitted_room()
                        .ok_or("native room has no admitted task document")?,
                )?;
                Ok(crate::services::WorkspaceTaskSnapshot {
                    room: room.into(),
                    generation: view.generation(),
                    tasks: state.tasks,
                    controls: state.controls,
                })
            })
        }

        fn host_certificate(&self, room: &str) -> Result<tilo_user_identity::DeviceCert, String> {
            if !self.requires_room(room) {
                return Err("wrong native task room".into());
            }
            self.owner.with_current(self.room, |view| {
                Ok(view.host().unwrap_or_else(|| view.governor()).clone())
            })
        }

        fn execute(
            &self,
            request: tilo_peer_state::owner_commit::OwnerCommitRequest,
        ) -> tilo_peer_state::owner_commit::CommitFuture {
            let owner = self.owner.clone();
            let now = self.now;
            Box::pin(async move { owner.commit_workspace_task_command(request, now) })
        }
    }

    fn native_tasks() -> (
        tempfile::TempDir,
        NativeTasks,
        tilo_user_identity::DeviceCert,
    ) {
        use collab_types::caps::{CapOverride, Scope, Subject, Verb};
        use ed25519_dalek::SigningKey;

        let directory = tempfile::tempdir().unwrap();
        let owner = tilo_joined_room::open_file(&directory.path().join("native.json")).unwrap();
        let (intent, bootstrap, messages, governor, now) = tilo_joined_room::testing::raw_fixture();
        let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
        let enrollment = tilo_joined_room::testing::finalize(intent, pending, bootstrap).unwrap();
        owner.install(&enrollment, now).unwrap();
        let room = tilo_desktop_model::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
        for message in messages.into_iter().skip(1) {
            owner
                .receive(
                    room,
                    TransportOrigin::RetainedRelayDelta(governor),
                    message,
                    now,
                )
                .unwrap();
        }
        let signed = owner
            .with_current(room, |view| {
                let before = view.peer().clone();
                let mut candidate = before.clone();
                candidate.put_governed_capabilities(
                    &[
                        CapOverride::grant(Subject::agent("scribe"), Scope::Workspace, Verb::Write)
                            .with_id("test-agent-scribe"),
                    ],
                    &SigningKey::from_bytes(&[61; 32]),
                    now,
                )?;
                candidate.durable_mutation_messages(
                    &before,
                    &SigningKey::from_bytes(&[61; 32]),
                    now,
                    view.generation().checked_add(1).unwrap(),
                )
            })
            .unwrap();
        for message in signed {
            owner
                .receive(
                    room,
                    TransportOrigin::RetainedRelayDelta(governor),
                    message,
                    now,
                )
                .unwrap();
        }
        let caller = owner
            .with_current(room, |view| Ok(view.device().clone()))
            .unwrap();
        (directory, NativeTasks { owner, room, now }, caller)
    }

    fn native_run_fixture() -> NativeRunFixture {
        let (directory, service, caller) = native_tasks();
        let service: Arc<dyn crate::services::WorkspaceTasks> = Arc::new(service);
        let fanout = Arc::new(super::super::FanoutState::new());
        let (state, _bridge) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(super::super::BrokerChannels::default())),
            super::super::PeerPublisher::disabled(),
            None,
            None,
            None,
            crate::services::HostServices {
                workspace_tasks: Some(service.clone()),
                ..Default::default()
            },
        );
        let (tx, receiver) = tokio::sync::mpsc::unbounded_channel();
        state.task_subscribers.lock().unwrap().insert(
            71,
            super::super::TaskSubscription {
                agent_id: "scribe".into(),
                cluster_id: tilo_joined_room::testing::ROOM.into(),
                tx,
            },
        );
        NativeRunFixture {
            _directory: directory,
            service,
            state,
            receiver,
            caller,
        }
    }

    fn envelope(
        resource: &str,
        action: &str,
        argument: Option<serde_json::Value>,
    ) -> InvocationEnvelope {
        InvocationEnvelope {
            id: 1,
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(7)),
            resource: resource.into(),
            action: action.into(),
            argument: argument
                .map(|value| format!("{}{}", tilo_actions::ARGUMENTS_JSON_PREFIX, value)),
            actor: "not-authority".into(),
            context: None,
        }
    }

    #[test]
    fn public_start_agent_accepts_only_target_and_declares_administration() {
        let action = start_agent_action();
        assert_eq!(action.resource, COLLECTION);
        assert_eq!(action.action, "start-agent");
        assert_eq!(
            action.contract.permission,
            tilo_actions::PermissionRequirement::Admin
        );
        assert_eq!(action.params.len(), 1);
        assert_eq!(action.params[0].name, "agent");
        assert_eq!(
            startup_agent(&envelope(
                COLLECTION,
                "start-agent",
                Some(serde_json::json!({"agent": "scribe"}))
            ))
            .unwrap(),
            "scribe"
        );
        for extra in [
            "caller",
            "certificate",
            "room",
            "invocation",
            "authority",
            "role",
        ] {
            let mut arguments = serde_json::json!({"agent": "scribe"});
            arguments[extra] = serde_json::json!("must not confer authority");
            assert!(startup_agent(&envelope(COLLECTION, "start-agent", Some(arguments))).is_err());
        }
        for agent in ["", "Scribe", " scribe", "scribe\n"] {
            assert!(
                startup_agent(&envelope(
                    COLLECTION,
                    "start-agent",
                    Some(serde_json::json!({"agent": agent}))
                ))
                .is_err()
            );
        }
        assert!(startup_agent(&envelope(COLLECTION, "start-agent", None)).is_err());
    }

    #[test]
    fn public_queue_keeps_full_instruction_and_rejects_authority_fields() {
        let instruction = "Review line one.\n\nKeep every authored detail.";
        let command = person_command(&envelope(
            COLLECTION,
            "queue",
            Some(serde_json::json!({
                "instruction": instruction,
                "agent": "scribe",
                "route": "model",
            })),
        ))
        .unwrap();
        let WorkspaceTaskCommand::QueueTask {
            instruction: retained,
            title,
            resource,
            ..
        } = command
        else {
            panic!("queue command")
        };
        assert_eq!(retained.as_bytes(), instruction.as_bytes());
        assert_eq!(title, "Review line one.");
        assert!(resource.is_none());

        for field in ["caller", "certificate", "room", "invocation"] {
            let mut arguments = serde_json::json!({
                "instruction": "Do the task",
                "agent": "scribe",
                "route": "rules_only",
            });
            arguments[field] = serde_json::json!("forged");
            assert!(person_command(&envelope(COLLECTION, "queue", Some(arguments))).is_err());
        }
    }

    #[test]
    fn task_surface_never_publishes_instruction_or_fake_stop() {
        let task = task("task:1", "scribe");
        let mut controls = std::collections::BTreeMap::new();
        controls.insert(
            task.id.clone(),
            tilo_desktop_model::WorkspaceTaskControl {
                instruction: Some("private full task instruction".into()),
                instruction_required: true,
                ..Default::default()
            },
        );
        let actions = task_actions(&[task], &controls);
        let encoded = serde_json::to_string(&actions).unwrap();
        assert!(!encoded.contains("private full task instruction"));
        assert!(actions.iter().all(|action| action.action != "stop"));
    }

    #[test]
    fn active_rules_task_exposes_stop_but_not_unsupported_direction() {
        let mut task = task("task:1", "scribe");
        task.status = tilo_desktop_model::WorkspaceTaskStatus::InProgress;
        task.route = tilo_desktop_model::WorkspaceTaskRoute::RulesOnly;
        let controls = std::collections::BTreeMap::from([(
            task.id.clone(),
            control("scribe", 1, 0, "private instruction"),
        )]);
        let actions = task_actions(&[task], &controls);
        assert!(actions.iter().any(|action| action.action == "stop"));
        assert!(actions.iter().all(|action| action.action != "direct"));
    }

    #[test]
    fn owner_reads_publish_only_exact_active_agent_frontiers() {
        let mut scribe = task("task:scribe", "scribe");
        scribe.status = tilo_desktop_model::WorkspaceTaskStatus::InProgress;
        let mut builder = task("task:builder", "builder");
        builder.status = tilo_desktop_model::WorkspaceTaskStatus::InProgress;
        let mut scribe_control = control("scribe", 4, 2, "never publish this instruction");
        scribe_control.execution = Some(tilo_desktop_model::WorkspaceTaskExecution {
            run_epoch: 4,
            sequence: 0,
            phase: tilo_desktop_model::WorkspaceTaskExecutionPhase::Accepted,
            updated_at: 7,
        });
        let controls = std::collections::BTreeMap::from([
            (scribe.id.clone(), scribe_control),
            (
                builder.id.clone(),
                control("builder", 1, 0, "other agent private instruction"),
            ),
        ]);
        let queued = task("task:queued", "scribe");
        let service = ReadOnlyTasks(crate::services::WorkspaceTaskSnapshot {
            room: "room-a".into(),
            generation: 7,
            tasks: vec![scribe, builder, queued],
            controls,
        });
        let result = read_current_tasks(
            "room-a",
            &envelope(
                COLLECTION,
                "read-current-tasks",
                Some(serde_json::json!({"agent": "scribe"})),
            ),
            &service,
        )
        .unwrap();
        assert_eq!(result["generation"], 7);
        assert_eq!(result["complete"], true);
        assert_eq!(result["total"], 1);
        assert_eq!(result["tasks"][0]["task_id"], "task:scribe");
        assert_eq!(result["tasks"][0]["run_epoch"], 4);
        assert_eq!(result["tasks"][0]["applied_revision"], 2);
        assert_eq!(result["tasks"][0]["execution"]["run_epoch"], 4);
        assert_eq!(result["tasks"][0]["execution"]["sequence"], 0);
        assert_eq!(result["tasks"][0]["execution"]["phase"], "accepted");
        assert_eq!(result["pending_complete"], true);
        assert_eq!(result["pending_total"], 1);
        assert_eq!(result["pending"][0]["task_id"], "task:queued");
        assert_eq!(result["pending"][0]["status"], "queued");
        let encoded = result.to_string();
        assert!(!encoded.contains("never publish"));
        assert!(!encoded.contains("other agent"));
    }

    #[test]
    fn owner_pending_projection_preserves_execution_fifo_and_waiting_state() {
        let mut later_lexically_first = task("task:a-later", "scribe");
        later_lexically_first.created_at = 30;
        let mut waiting = task("task:m-waiting", "scribe");
        waiting.created_at = 20;
        waiting.status = tilo_desktop_model::WorkspaceTaskStatus::Waiting;
        let mut first_lexically_last = task("task:z-first", "scribe");
        first_lexically_last.created_at = 10;
        let service = ReadOnlyTasks(crate::services::WorkspaceTaskSnapshot {
            room: "room-a".into(),
            generation: 8,
            tasks: vec![later_lexically_first, waiting, first_lexically_last],
            controls: std::collections::BTreeMap::new(),
        });

        let result = read_current_tasks(
            "room-a",
            &envelope(
                COLLECTION,
                "read-current-tasks",
                Some(serde_json::json!({"agent": "scribe"})),
            ),
            &service,
        )
        .unwrap();

        let pending = result["pending"].as_array().unwrap();
        assert_eq!(pending[0]["task_id"], "task:z-first");
        assert_eq!(pending[0]["status"], "queued");
        assert_eq!(pending[1]["task_id"], "task:m-waiting");
        assert_eq!(pending[1]["status"], "waiting");
        assert_eq!(pending[2]["task_id"], "task:a-later");
        assert_eq!(next_queued_task(&service.0, "scribe"), Some("task:z-first"));
    }

    #[test]
    fn runnable_head_and_pending_projection_use_owner_ordinals_not_time_or_identity() {
        let mut first = task("task:z-first", "scribe");
        let mut second = task("task:a-second", "scribe");
        first.created_at = 42;
        second.created_at = 42;
        let mut waiting = task("task:m-waiting", "scribe");
        waiting.created_at = -100;
        waiting.status = tilo_desktop_model::WorkspaceTaskStatus::Waiting;
        let legacy = task("task:legacy", "scribe");
        let mut service = ReadOnlyTasks(crate::services::WorkspaceTaskSnapshot {
            room: "room-a".into(),
            generation: 8,
            tasks: vec![second, waiting, first, legacy],
            controls: [
                ("task:z-first", 1),
                ("task:a-second", 2),
                ("task:m-waiting", 3),
            ]
            .into_iter()
            .map(|(id, ordinal)| {
                (
                    id.into(),
                    tilo_desktop_model::WorkspaceTaskControl {
                        queue_ordinal: Some(ordinal),
                        ..Default::default()
                    },
                )
            })
            .collect(),
        });
        assert_eq!(next_queued_task(&service.0, "scribe"), Some("task:legacy"));
        service.0.tasks.retain(|task| task.id != "task:legacy");
        assert_eq!(next_queued_task(&service.0, "scribe"), Some("task:z-first"));
        let result = read_current_tasks(
            "room-a",
            &envelope(
                COLLECTION,
                "read-current-tasks",
                Some(serde_json::json!({"agent": "scribe"})),
            ),
            &service,
        )
        .unwrap();
        let pending = result["pending"].as_array().unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|row| row["task_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["task:z-first", "task:a-second", "task:m-waiting"]
        );
        assert_eq!(pending[2]["status"], "waiting");
        service
            .0
            .tasks
            .iter_mut()
            .find(|task| task.id == "task:z-first")
            .unwrap()
            .status = tilo_desktop_model::WorkspaceTaskStatus::Done;
        assert_eq!(
            next_queued_task(&service.0, "scribe"),
            Some("task:a-second")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_offline_queue_drains_one_run_after_each_owner_receipt() {
        let NativeRunFixture {
            _directory,
            service,
            state,
            receiver: _offline_receiver,
            caller,
        } = native_run_fixture();
        let room = tilo_joined_room::testing::ROOM;
        state.task_subscribers.lock().unwrap().clear();
        let first = queue_native_task(service.as_ref(), room, caller.clone()).await;
        assert!(
            state
                .dispatch_queued_workspace_task(room, "scribe")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            service.snapshot(room).unwrap().tasks[0].status,
            tilo_desktop_model::WorkspaceTaskStatus::Queued
        );
        assert!(state.pending_task_runs.lock().unwrap().is_empty());

        let mut receiver = register_scribe_subscriber(&state, room, 72);
        let (left, right) = tokio::join!(
            state.dispatch_queued_workspace_task(room, "scribe"),
            state.dispatch_queued_workspace_task(room, "scribe"),
        );
        let starts = [left.unwrap(), right.unwrap()];
        assert_eq!(starts.iter().filter(|run| run.is_some()).count(), 1);
        let run_id = starts.into_iter().flatten().next().unwrap();
        let delivered = receiver.try_recv().unwrap();
        assert_eq!(delivered.task_id, first);
        assert_eq!(
            delivered.instruction.as_deref(),
            Some("Line one.\n\nKeep all details.")
        );
        assert_eq!(delivered.run_id, run_id);

        let second = queue_second_native_task(service.as_ref(), room, caller, &first).await;
        let (left, right) = tokio::join!(
            state.dispatch_queued_workspace_task(room, "scribe"),
            state.dispatch_queued_workspace_task(room, "scribe"),
        );
        assert!(left.unwrap().is_none());
        assert!(right.unwrap().is_none());
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            service
                .snapshot(room)
                .unwrap()
                .tasks
                .iter()
                .find(|task| task.id == second)
                .unwrap()
                .status,
            tilo_desktop_model::WorkspaceTaskStatus::Queued
        );

        let run = state.pending_task_runs.lock().unwrap()[&run_id].clone();
        state
            .record_owner_task_run_update(
                run_id,
                &run,
                runtime_collab::TaskRunUpdate::Succeeded {
                    message: "First task finished".into(),
                },
            )
            .await
            .unwrap();
        let next = receiver.try_recv().unwrap();
        assert_eq!(next.task_id, second);
        assert_eq!(
            next.instruction.as_deref(),
            Some("Do the second task only after the first.")
        );
        assert_ne!(next.run_id, run_id);
        assert_ne!(next.lease, delivered.lease);
        assert!(receiver.try_recv().is_err());
        let snapshot = service.snapshot(room).unwrap();
        assert_eq!(
            snapshot
                .tasks
                .iter()
                .find(|task| task.id == first)
                .unwrap()
                .status,
            tilo_desktop_model::WorkspaceTaskStatus::Done
        );
        assert_eq!(
            snapshot
                .tasks
                .iter()
                .find(|task| task.id == second)
                .unwrap()
                .status,
            tilo_desktop_model::WorkspaceTaskStatus::InProgress
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retirement_abandons_a_pre_readiness_lease_for_only_the_exact_launch() {
        let NativeRunFixture {
            _directory,
            service,
            state,
            caller,
            ..
        } = native_run_fixture();
        let room = tilo_joined_room::testing::ROOM;
        let task_id = queue_native_task(service.as_ref(), room, caller).await;
        let epoch = 1;
        let (task, control) =
            start_owner_task_run(room, &task_id, "scribe", epoch, service.as_ref())
                .await
                .unwrap();
        let facts = test_enrolled_facts(room, "launch-exact");
        let envelope =
            enrolled_task_envelope(&facts, room, &task, &control, "scribe", epoch).unwrap();
        let (tx, _receiver) = tokio::sync::mpsc::unbounded_channel();
        state.enrolled_task_subscribers.lock().unwrap().insert(
            91,
            super::super::EnrolledTaskSubscription {
                agent_id: "scribe".into(),
                cluster_id: room.into(),
                facts,
                tx,
                runs: std::collections::HashMap::from([(
                    envelope.message_id.clone(),
                    super::super::EnrolledTaskRun {
                        envelope,
                        task_epoch: epoch,
                        acceptance: None,
                        report_sequence: 0,
                        reports: std::collections::HashMap::new(),
                    },
                )]),
            },
        );

        state
            .abandon_managed_agent_tasks(room, "scribe", "another-launch")
            .await
            .unwrap();
        assert_eq!(
            service.snapshot(room).unwrap().tasks[0].status,
            tilo_desktop_model::WorkspaceTaskStatus::InProgress,
            "another launch and every legacy lane are ineligible to retire this lease"
        );
        assert!(
            state
                .enrolled_task_subscribers
                .lock()
                .unwrap()
                .contains_key(&91)
        );

        state
            .abandon_managed_agent_tasks(room, "scribe", "launch-exact")
            .await
            .unwrap();
        assert_eq!(
            service.snapshot(room).unwrap().tasks[0].status,
            tilo_desktop_model::WorkspaceTaskStatus::Waiting,
            "retirement must abandon a lease even before the shell observes readiness"
        );
        assert!(
            !state
                .enrolled_task_subscribers
                .lock()
                .unwrap()
                .contains_key(&91)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_current_read_retains_stopping_until_owner_terminal() {
        let NativeRunFixture {
            _directory,
            service,
            state,
            mut receiver,
            caller,
        } = native_run_fixture();
        let room = tilo_joined_room::testing::ROOM;
        let task_id = queue_native_task(service.as_ref(), room, caller.clone()).await;
        let run_id = state
            .dispatch_queued_workspace_task(room, "scribe")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receiver.try_recv().unwrap().run_id, run_id);
        let query = envelope(
            COLLECTION,
            "read-current",
            Some(serde_json::json!({"agent":"scribe"})),
        );
        let current = read_current_tasks(room, &query, service.as_ref()).unwrap();
        assert_eq!(current["tasks"][0]["route"], "model");
        assert_eq!(current["tasks"][0]["cancellation_requested"], false);
        commit_person_command(
            service.as_ref(),
            room,
            WorkspaceTaskCommand::CancelRun {
                task_id: task_id.clone(),
                run_epoch: 1,
                after_revision: 0,
            },
            303,
            caller,
        )
        .await;
        let stopping = read_current_tasks(room, &query, service.as_ref()).unwrap();
        assert_eq!(stopping["tasks"][0]["status"], "in_progress");
        assert_eq!(stopping["tasks"][0]["cancellation_requested"], true);
        let run = state.pending_task_runs.lock().unwrap()[&run_id].clone();
        state
            .record_owner_task_run_update(
                run_id,
                &run,
                runtime_collab::TaskRunUpdate::Cancelled {
                    message: "Cancellation acknowledged".into(),
                },
            )
            .await
            .unwrap();
        let terminal = read_current_tasks(room, &query, service.as_ref()).unwrap();
        assert_eq!(terminal["total"], 0);
        assert_eq!(
            service.snapshot(room).unwrap().tasks[0].status,
            tilo_desktop_model::WorkspaceTaskStatus::Cancelled
        );
        assert!(
            state
                .dispatch_queued_workspace_task(room, "scribe")
                .await
                .unwrap()
                .is_none()
        );
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_owner_run_delivers_direction_ack_and_exact_stop() {
        let NativeRunFixture {
            _directory,
            service,
            state,
            mut receiver,
            caller,
        } = native_run_fixture();
        let room = tilo_joined_room::testing::ROOM;
        let task_id = queue_native_task(service.as_ref(), room, caller.clone()).await;
        let run_id = state
            .dispatch_live_workspace_task(room, &task_id, service.as_ref())
            .await
            .unwrap()
            .unwrap();
        let envelope = receiver.recv().await.unwrap();
        assert_eq!(envelope.run_id, run_id);
        assert_eq!(
            envelope.instruction.as_deref(),
            Some("Line one.\n\nKeep all details.")
        );
        let run = state.pending_task_runs.lock().unwrap()[&run_id].clone();

        commit_person_command(
            service.as_ref(),
            room,
            WorkspaceTaskCommand::DirectionForRun {
                task_id: task_id.clone(),
                run_epoch: 1,
                after_revision: 0,
                text: "Use the revised date.".into(),
            },
            102,
            caller.clone(),
        )
        .await;
        let control = state.owner_task_run_control(&run).unwrap();
        assert_eq!(control.directions[0].text, "Use the revised date.");
        state
            .record_owner_task_run_update(
                run_id,
                &run,
                runtime_collab::TaskRunUpdate::DirectionApplied { revision: 1 },
            )
            .await
            .unwrap();
        // A lost transport reply retries against the same durable frontier.
        state
            .record_owner_task_run_update(
                run_id,
                &run,
                runtime_collab::TaskRunUpdate::DirectionApplied { revision: 1 },
            )
            .await
            .unwrap();
        assert!(
            state
                .owner_task_run_control(&run)
                .unwrap()
                .directions
                .is_empty()
        );

        commit_person_command(
            service.as_ref(),
            room,
            WorkspaceTaskCommand::CancelRun {
                task_id: task_id.clone(),
                run_epoch: 1,
                after_revision: 1,
            },
            103,
            caller,
        )
        .await;
        assert!(state.owner_task_run_control(&run).unwrap().cancel_requested);
        assert!(state.validate_owner_task_action_frontier(&run).is_err());
        state
            .record_owner_task_run_update(
                run_id,
                &run,
                runtime_collab::TaskRunUpdate::Cancelled {
                    message: "Stopped at the person's request".into(),
                },
            )
            .await
            .unwrap();
        let snapshot = service.snapshot(room).unwrap();
        assert_eq!(
            snapshot
                .tasks
                .iter()
                .find(|task| task.id == task_id)
                .unwrap()
                .status,
            tilo_desktop_model::WorkspaceTaskStatus::Cancelled
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_agent_acceptance_publishes_phase_zero_before_reports() {
        let NativeRunFixture {
            _directory,
            service,
            state,
            mut receiver,
            caller,
        } = native_run_fixture();
        let room = tilo_joined_room::testing::ROOM;
        let task_id = queue_native_task(service.as_ref(), room, caller).await;
        let run_id = state
            .dispatch_live_workspace_task(room, &task_id, service.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receiver.recv().await.unwrap().run_id, run_id);
        let run = state.pending_task_runs.lock().unwrap()[&run_id].clone();
        assert!(
            service.snapshot(room).unwrap().controls[&task_id]
                .execution
                .is_none()
        );

        state.record_owner_task_acceptance(&run).await.unwrap();
        // A lost RPC acknowledgement retries the same owner invocation.
        state.record_owner_task_acceptance(&run).await.unwrap();
        let accepted = service.snapshot(room).unwrap().controls[&task_id]
            .execution
            .clone()
            .unwrap();
        assert_eq!(accepted.run_epoch, 1);
        assert_eq!(accepted.sequence, 0);
        assert_eq!(
            accepted.phase,
            tilo_desktop_model::WorkspaceTaskExecutionPhase::Accepted
        );
        state
            .record_owner_task_execution_phase(
                &run,
                1,
                tilo_desktop_model::WorkspaceTaskExecutionPhase::Accepted,
            )
            .await
            .unwrap();
        let reported = service.snapshot(room).unwrap().controls[&task_id]
            .execution
            .clone()
            .unwrap();
        assert_eq!(reported.sequence, 1);
        assert_eq!(
            reported.phase,
            tilo_desktop_model::WorkspaceTaskExecutionPhase::Accepted
        );
        state
            .record_owner_task_execution_phase(
                &run,
                2,
                reported_execution_phase(&runtime_collab::TaskRunUpdate::Planning {}).unwrap(),
            )
            .await
            .unwrap();
        state
            .record_owner_task_execution_phase(
                &run,
                3,
                reported_execution_phase(&runtime_collab::TaskRunUpdate::Executing {}).unwrap(),
            )
            .await
            .unwrap();
        let executing = service.snapshot(room).unwrap().controls[&task_id]
            .execution
            .clone()
            .unwrap();
        assert_eq!(executing.sequence, 3);
        assert_eq!(
            executing.phase,
            tilo_desktop_model::WorkspaceTaskExecutionPhase::Executing
        );
        assert!(
            reported_execution_phase(&runtime_collab::TaskRunUpdate::Progress {
                message: "untyped progress".into()
            })
            .is_none()
        );
    }

    #[test]
    fn task_read_refuses_arguments_and_returns_owner_frontier() {
        let mut task = task("task:1", "scribe");
        task.status = tilo_desktop_model::WorkspaceTaskStatus::InProgress;
        let mut owner_control = control("scribe", 3, 1, "private instruction");
        owner_control.execution = Some(tilo_desktop_model::WorkspaceTaskExecution {
            run_epoch: 3,
            sequence: 2,
            phase: tilo_desktop_model::WorkspaceTaskExecutionPhase::Planning,
            updated_at: 4,
        });
        let service = ReadOnlyTasks(crate::services::WorkspaceTaskSnapshot {
            room: "room-a".into(),
            generation: 9,
            controls: std::collections::BTreeMap::from([(task.id.clone(), owner_control)]),
            tasks: vec![task],
        });
        let mut request = envelope("desktop:task:task:1", "read-task", None);
        let result = read_task("room-a", &request, &service).unwrap();
        assert_eq!(result["run_epoch"], 3);
        assert_eq!(result["active_agent"], "scribe");
        assert_eq!(result["applied_revision"], 1);
        assert_eq!(result["execution"]["run_epoch"], 3);
        assert_eq!(result["execution"]["sequence"], 2);
        assert_eq!(result["execution"]["phase"], "planning");
        assert!(result.get("instruction").is_none());
        request.argument = Some("{}".into());
        assert!(read_task("room-a", &request, &service).is_err());
    }

    fn register_scribe_subscriber(
        state: &CapnpBrokerState,
        room: &str,
        peer_id: u64,
    ) -> tokio::sync::mpsc::UnboundedReceiver<super::super::TaskRunEnvelope> {
        let (tx, receiver) = tokio::sync::mpsc::unbounded_channel();
        state.task_subscribers.lock().unwrap().insert(
            peer_id,
            super::super::TaskSubscription {
                agent_id: "scribe".into(),
                cluster_id: room.into(),
                tx,
            },
        );
        receiver
    }

    fn test_enrolled_facts(room: &str, launch: &str) -> launcher::managed::ChannelProcessFacts {
        launcher::managed::ChannelProcessFacts {
            binding: launcher::managed::ManagedBinding {
                room: room.into(),
                app: launcher::managed::WORKSPACE_AGENT_PACKAGE_ID.into(),
                agent: "scribe".into(),
                scope: launcher::managed::ManagedLaunchScope::AgentSession,
                launch: launch.into(),
                generation: 1,
            },
            runtime: launcher::managed::ManagedRuntimeFacts {
                runtime_epoch: "runtime-epoch".into(),
                package_id: launcher::managed::WORKSPACE_AGENT_PACKAGE_ID.into(),
                package_hash: "package-hash".into(),
                executable_hash: "executable-hash".into(),
                runtime_hash: "runtime-hash".into(),
            },
            process: launcher::managed::ProcessIdentity { pid: 91, start: 1 },
            supervisor: launcher::managed::ProcessIdentity { pid: 90, start: 1 },
            channel_id: "channel-exact".into(),
        }
    }

    async fn queue_second_native_task(
        service: &dyn crate::services::WorkspaceTasks,
        room: &str,
        caller: tilo_user_identity::DeviceCert,
        first: &str,
    ) -> String {
        commit_person_command(
            service,
            room,
            WorkspaceTaskCommand::QueueTask {
                title: "Next instruction".into(),
                instruction: "Do the second task only after the first.".into(),
                assigned_agent: "scribe".into(),
                route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                resource: None,
            },
            202,
            caller,
        )
        .await;
        service
            .snapshot(room)
            .unwrap()
            .tasks
            .into_iter()
            .find(|task| task.id != first)
            .unwrap()
            .id
    }

    async fn queue_native_task(
        service: &dyn crate::services::WorkspaceTasks,
        room: &str,
        caller: tilo_user_identity::DeviceCert,
    ) -> String {
        let request = tilo_peer_state::owner_commit::OwnerCommitRequest {
            room: room.into(),
            operation: CommitOperation::WorkspaceTaskCommand {
                invocation: uuid::Uuid::from_u128(101),
                authority: WorkspaceTaskAuthority::Person { caller },
                command: WorkspaceTaskCommand::QueueTask {
                    title: "Line one".into(),
                    instruction: "Line one.\n\nKeep all details.".into(),
                    assigned_agent: "scribe".into(),
                    route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                    resource: None,
                },
            },
        };
        service
            .execute(request.clone())
            .await
            .unwrap()
            .validate_for(&request)
            .unwrap();
        service
            .snapshot(room)
            .unwrap()
            .tasks
            .into_iter()
            .next()
            .map(|task| task.id)
            .unwrap()
    }

    async fn commit_person_command(
        service: &dyn crate::services::WorkspaceTasks,
        room: &str,
        command: WorkspaceTaskCommand,
        invocation: u128,
        caller: tilo_user_identity::DeviceCert,
    ) {
        let request = tilo_peer_state::owner_commit::OwnerCommitRequest {
            room: room.into(),
            operation: CommitOperation::WorkspaceTaskCommand {
                invocation: uuid::Uuid::from_u128(invocation),
                authority: WorkspaceTaskAuthority::Person { caller },
                command,
            },
        };
        service
            .execute(request.clone())
            .await
            .unwrap()
            .validate_for(&request)
            .unwrap();
    }

    fn task(id: &str, agent: &str) -> tilo_desktop_model::WorkspaceTask {
        tilo_desktop_model::WorkspaceTask {
            id: id.into(),
            title: "Review".into(),
            created_by: "person".into(),
            assigned_agent: Some(agent.into()),
            route: tilo_desktop_model::WorkspaceTaskRoute::Model,
            status: tilo_desktop_model::WorkspaceTaskStatus::Queued,
            resource: None,
            created_at: 1,
            updated_at: 1,
            audit: Vec::new(),
        }
    }

    fn control(
        agent: &str,
        epoch: u64,
        applied: u64,
        instruction: &str,
    ) -> tilo_desktop_model::WorkspaceTaskControl {
        tilo_desktop_model::WorkspaceTaskControl {
            run_epoch: epoch,
            instruction: Some(instruction.into()),
            instruction_required: true,
            active_run: Some(tilo_desktop_model::WorkspaceTaskActiveRun {
                epoch,
                agent_id: agent.into(),
                started_at: 1,
                applied_direction_revision: applied,
            }),
            ..Default::default()
        }
    }
}
