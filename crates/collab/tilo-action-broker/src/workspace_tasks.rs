//! Canonical Task Center reads and person-authored mutations through the
//! ordinary durable invocation door. Public arguments select text, routing,
//! and an enrolled Agent; caller identity and owner custody stay out of band.

use super::{
    Arc, CapnpBrokerState, CrdtRoute, InvocationEnvelope, PublishedAction, PublishedInput,
    PublishedSurface,
};
use std::sync::atomic::Ordering;
use tilo_desktop_model::{
    WorkspaceTask, WorkspaceTaskMutationResult, WorkspaceTaskResource, WorkspaceTaskRoute,
};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand};
use tilo_peer_state::workspace_task::{WorkspaceTaskSnapshot, validate_manual_resource};

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
        | runtime_collab::TaskRunUpdate::DirectionsApplied { .. }
        | runtime_collab::TaskRunUpdate::DirectionApplied { .. } => None,
    }
}

impl CapnpBrokerState {
    pub(super) fn task_availability_changed(&self) {
        self.task_availability_generation
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    pub(crate) fn task_availability_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.task_availability_generation.subscribe()
    }

    /// Observe an exact Agent's current task-consumer availability.
    /// Registry and authority observation failures remain distinct from a
    /// healthy absence.
    pub(crate) fn agent_task_availability(&self, room: &str, agent: &str) -> Result<bool, String> {
        let mut observation_error = None;
        let legacy = if let Ok(subscribers) = self.task_subscribers.lock() {
            subscribers
                .iter()
                .filter(|(_, subscriber)| {
                    subscriber.cluster_id == room
                        && subscriber.agent_id == agent
                        && !subscriber.tx.is_closed()
                })
                .map(|(peer, subscriber)| (*peer, subscriber.clone()))
                .collect::<Vec<_>>()
        } else {
            observation_error = Some("task subscription registry unavailable".to_string());
            Vec::new()
        };
        let enrolled = if let Ok(subscribers) = self.enrolled_task_subscribers.lock() {
            subscribers
                .iter()
                .filter(|(_, subscriber)| {
                    subscriber.cluster_id == room
                        && subscriber.agent_id == agent
                        && !subscriber.tx.is_closed()
                })
                .map(|(peer, subscriber)| (*peer, subscriber.clone()))
                .collect::<Vec<_>>()
        } else {
            observation_error
                .get_or_insert_with(|| "enrolled task subscription registry unavailable".into());
            Vec::new()
        };

        if legacy.is_empty() && enrolled.is_empty() {
            return observation_error.map_or(Ok(false), Err);
        }
        if !self.agent_is_active_member(room, agent)? {
            return Ok(false);
        }

        for (peer, subscriber) in enrolled {
            match self.enrolled_candidate_is_available(peer, room, agent, &subscriber) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(error) => {
                    observation_error.get_or_insert(error);
                }
            }
        }
        for (peer, subscriber) in legacy {
            match self.legacy_candidate_is_available(peer, room, agent, &subscriber) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(error) => {
                    observation_error.get_or_insert(error);
                }
            }
        }
        observation_error.map_or(Ok(false), Err)
    }

    fn enrolled_candidate_is_available(
        &self,
        peer: u64,
        room: &str,
        agent: &str,
        candidate: &super::EnrolledTaskSubscription,
    ) -> Result<bool, String> {
        if self.observed_managed_agent_facts(peer, room, agent)? != Some(candidate.facts.clone()) {
            return Ok(false);
        }
        Ok(self
            .enrolled_task_subscribers
            .lock()
            .map_err(|_| "enrolled task subscription registry unavailable")?
            .get(&peer)
            .is_some_and(|current| {
                current.subscription == candidate.subscription
                    && current.facts == candidate.facts
                    && !current.tx.is_closed()
            }))
    }

    fn legacy_candidate_is_available(
        &self,
        peer: u64,
        room: &str,
        agent: &str,
        candidate: &super::TaskSubscription,
    ) -> Result<bool, String> {
        let (route, identity) = match &candidate.custody {
            super::TaskSubscriptionCustody::Admitted { route, identity } => (route, identity),
            #[cfg(test)]
            super::TaskSubscriptionCustody::Fixture => return Ok(false),
        };
        if route.cluster_id != room || identity.agent_id.as_deref() != Some(agent) {
            return Ok(false);
        }
        let current = self
            .revalidate_admission(route, identity)
            .map_err(|error| error.to_string())?;
        if current.agent_id.as_deref() != Some(agent) {
            return Ok(false);
        }
        Ok(self
            .task_subscribers
            .lock()
            .map_err(|_| "task subscription registry unavailable")?
            .get(&peer)
            .is_some_and(|retained| {
                retained.subscription == candidate.subscription && !retained.tx.is_closed()
            }))
    }

    pub(super) fn observed_managed_agent_facts(
        &self,
        peer: u64,
        room: &str,
        agent: &str,
    ) -> Result<Option<launcher::managed::ChannelProcessFacts>, String> {
        let process = self
            .managed_workers
            .lock()
            .map_err(|_| "managed worker registry unavailable")?
            .get(&peer)
            .cloned();
        let Some(process) = process else {
            return Ok(None);
        };
        let facts = process.facts()?;
        if facts.binding.room != room
            || facts.binding.agent != agent
            || facts.binding.app != launcher::managed::WORKSPACE_AGENT_PACKAGE_ID
            || !self.agent_is_active_member(room, agent)?
        {
            return Ok(None);
        }
        Ok(Some(facts))
    }

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
        if envelope.action == "read-pending-task" {
            return read_pending_task(room, envelope, service);
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
        let offered = checked_snapshot(service, room)?;
        let Some(task) = offered.tasks.iter().find(|task| task.id == task_id) else {
            return Err("queued task disappeared before delivery".into());
        };
        if task.status != tilo_desktop_model::WorkspaceTaskStatus::Queued {
            return Ok(None);
        }
        if !offered
            .controls
            .get(task_id)
            .is_some_and(retained_managed_task)
        {
            return Ok(None);
        }
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
            execution_budget: control.execution_budget,
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
                    native: None,
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
        let task = snapshot.tasks.iter().find(|task| task.id == task_id);
        let Some(task) = task else {
            return Ok(None);
        };
        let Some(control) = snapshot.controls.get(task_id) else {
            return Ok(None);
        };
        if !retained_managed_task(control) {
            return Ok(None);
        }
        let Some(agent) = task.assigned_agent.as_deref() else {
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
    ) -> Result<
        (
            tokio::sync::mpsc::UnboundedReceiver<runtime_collab::EnrolledTaskEnvelope>,
            uuid::Uuid,
        ),
        String,
    > {
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
        let subscription = uuid::Uuid::new_v4();
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
                    subscription,
                    runs: std::collections::HashMap::new(),
                },
            );
        self.task_availability_changed();
        if let Err(error) = self.dispatch_queued_workspace_task(&room, &agent).await {
            self.close_enrolled_subscription(peer, subscription);
            return Err(error);
        }
        Ok((rx, subscription))
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

    pub(super) fn close_enrolled_subscription(&self, peer: u64, subscription: uuid::Uuid) {
        let removed = self
            .enrolled_task_subscribers
            .lock()
            .is_ok_and(|mut subscribers| {
                if subscribers
                    .get(&peer)
                    .is_some_and(|current| current.subscription == subscription)
                {
                    subscribers.remove(&peer);
                    true
                } else {
                    false
                }
            });
        if removed {
            self.task_availability_changed();
        }
    }

    pub(super) fn close_legacy_task_subscription(&self, peer: u64, subscription: uuid::Uuid) {
        let removed = self.task_subscribers.lock().is_ok_and(|mut subscribers| {
            if subscribers
                .get(&peer)
                .is_some_and(|current| current.subscription == subscription)
            {
                subscribers.remove(&peer);
                true
            } else {
                false
            }
        });
        if removed {
            self.task_availability_changed();
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
        let subscription = self
            .enrolled_task_subscribers
            .lock()
            .ok()
            .and_then(|subscribers| subscribers.get(&peer).map(|value| value.subscription));
        if let Some(subscription) = subscription {
            self.close_enrolled_subscription(peer, subscription);
        }
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
            native: None,
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
    ) -> Result<runtime_collab::EnrolledTaskRunControl, String> {
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
            .map(|direction| runtime_collab::EnrolledTaskRunDirection {
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
        Ok(runtime_collab::EnrolledTaskRunControl {
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
            execution_budget: control.execution_budget,
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
    if let Some(budget) = control.execution_budget {
        budget.validate()?;
    }
    if !retained_managed_task(control) {
        return Err("queued task requires an exact retained managed instruction".into());
    }
    Ok(())
}

fn retained_managed_task(control: &tilo_desktop_model::WorkspaceTaskControl) -> bool {
    control.instruction_required
        && control
            .instruction
            .as_deref()
            .is_some_and(|instruction| !instruction.is_empty())
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
                && snapshot
                    .controls
                    .get(&task.id)
                    .is_some_and(retained_managed_task)
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
        runtime_collab::TaskRunUpdate::DirectionsApplied { .. } => {
            Err("native direction acknowledgement is invalid for an enrolled task".into())
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
    directions: &[runtime_collab::EnrolledTaskRunDirection],
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
    if let Some(task_id) = valid_task_target(envelope)
        && matches!(envelope.action.as_str(), "amend-pending" | "cancel-pending")
    {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct PendingChange {
            status: tilo_desktop_model::WorkspaceTaskStatus,
            run_epoch: u64,
            pending_revision: u64,
            #[serde(default)]
            title: Option<String>,
            #[serde(default)]
            instruction: Option<String>,
        }
        let input: PendingChange =
            serde_json::from_str(encoded).map_err(|_| "invalid pending task change arguments")?;
        let frontier = tilo_desktop_model::WorkspaceTaskPendingFrontier {
            status: input.status,
            run_epoch: input.run_epoch,
            revision: input.pending_revision,
        };
        return if envelope.action == "amend-pending" {
            Ok(WorkspaceTaskCommand::AmendPending {
                task_id: task_id.into(),
                frontier,
                title: input.title.ok_or("pending amendment title missing")?,
                instruction: input
                    .instruction
                    .ok_or("pending amendment instruction missing")?,
            })
        } else if input.title.is_some() || input.instruction.is_some() {
            Err("pending cancellation accepts no replacement content".into())
        } else {
            Ok(WorkspaceTaskCommand::CancelPending {
                task_id: task_id.into(),
                frontier,
            })
        };
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

fn read_pending_task(
    room: &str,
    envelope: &InvocationEnvelope,
    service: &dyn crate::services::WorkspaceTasks,
) -> Result<serde_json::Value, String> {
    let task_id = valid_task_target(envelope).ok_or("pending task read target is invalid")?;
    if envelope.argument.is_some() {
        return Err("pending task read accepts no arguments".into());
    }
    let snapshot = checked_snapshot(service, room)?;
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .ok_or("task is absent from the current owner projection")?;
    let control = snapshot
        .controls
        .get(task_id)
        .ok_or("pending task has no owner control")?;
    if !matches!(
        task.status,
        tilo_desktop_model::WorkspaceTaskStatus::Queued
            | tilo_desktop_model::WorkspaceTaskStatus::Waiting
    ) || control.active_run.is_some()
    {
        return Err("task is no longer pending".into());
    }
    if control.instruction_required && control.instruction.is_none() {
        return Err("pending task instruction is unavailable".into());
    }
    let instruction = control
        .instruction
        .as_deref()
        .or_else(|| {
            task.resource
                .as_ref()
                .and_then(|resource| resource.agent_request.as_ref())
                .map(|request| request.instruction.as_str())
        })
        .unwrap_or(&task.title);
    if instruction.trim().is_empty()
        || instruction.len() > tilo_desktop_model::MAX_WORKSPACE_TASK_INSTRUCTION_BYTES
        || instruction.chars().count() > 8_000
    {
        return Err("pending task instruction is invalid or exceeds its bound".into());
    }
    Ok(serde_json::json!({
        "generation": snapshot.generation,
        "task_id": task.id,
        "title": task.title,
        "instruction": instruction,
        "frontier": tilo_desktop_model::WorkspaceTaskPendingFrontier {
            status: task.status, run_epoch: control.run_epoch, revision: control.pending_revision,
        },
        "queue_ordinal": control.queue_ordinal,
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
        if !manual(task)
            && matches!(
                task.status,
                tilo_desktop_model::WorkspaceTaskStatus::Queued
                    | tilo_desktop_model::WorkspaceTaskStatus::Waiting
            )
            && controls
                .get(&task.id)
                .is_some_and(|control| control.active_run.is_none())
        {
            let mut read = task_read_action(task);
            read.action = "read-pending-task".into();
            read.action_label = "Read pending task".into();
            read.description =
                Some("Read the exact pending instruction and edit/cancel frontier".into());
            actions.push(read);
            actions.extend(pending_change_actions(task));
        }
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

fn pending_change_actions(task: &tilo_desktop_model::WorkspaceTask) -> [PublishedAction; 2] {
    let frontier = vec![
        tilo_actions::ActionParam::required(
            "status",
            tilo_actions::ParamType::Text,
            "Exact queued or waiting status returned by read-pending-task",
        ),
        tilo_actions::ActionParam::required(
            "run_epoch",
            tilo_actions::ParamType::Integer,
            "Run epoch returned by read-pending-task",
        ),
        tilo_actions::ActionParam::required(
            "pending_revision",
            tilo_actions::ParamType::Integer,
            "Pending instruction revision returned by read-pending-task",
        ),
    ];
    let mut amendment = frontier.clone();
    amendment.extend([
        tilo_actions::ActionParam::required(
            "title",
            tilo_actions::ParamType::Text,
            "Replacement short task title",
        ),
        tilo_actions::ActionParam::required(
            "instruction",
            tilo_actions::ParamType::Text,
            "Complete replacement pending instruction (up to 32 KiB and 8000 characters)",
        ),
    ]);
    let resource = format!("{TASK_PREFIX}{}", task.id);
    [
        effect_action(
            &resource,
            "amend-pending",
            &task.title,
            "Edit pending task",
            amendment,
        ),
        effect_action(
            &resource,
            "cancel-pending",
            &task.title,
            "Cancel pending task",
            frontier,
        ),
    ]
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
    fn enrolled_task_delivery_retains_the_frozen_owner_budget() {
        let task = task("task:budget", "scribe");
        let mut control = control("scribe", 3, 0, "Complete private instruction");
        let budget = collab_types::task_budget::TaskExecutionBudget {
            turn_ms: 60_000,
            total_ms: 600_000,
        };
        control.execution_budget = Some(budget);
        let envelope = enrolled_task_envelope(
            &test_enrolled_facts("room-a", "launch-exact"),
            "room-a",
            &task,
            &control,
            "scribe",
            3,
        )
        .unwrap();
        assert_eq!(envelope.task.execution_budget, Some(budget));
        control.execution_budget = Some(collab_types::task_budget::TaskExecutionBudget {
            turn_ms: 900_001,
            total_ms: 7_200_000,
        });
        assert!(require_retained_task_instruction(&control).is_err());
        assert!(
            enrolled_task_envelope(
                &test_enrolled_facts("room-a", "launch-exact"),
                "room-a",
                &task,
                &control,
                "scribe",
                3,
            )
            .is_err()
        );
    }

    #[test]
    fn coordination_bounds_do_not_truncate_complete_task_instructions() {
        // Multi-byte text remains within the managed instruction's character
        // limit while exceeding the independent manual coordination byte cap.
        let instruction = "界".repeat(4_000);
        let managed = envelope(
            COLLECTION,
            "queue",
            Some(serde_json::json!({
                "instruction": instruction,
                "agent": "scribe",
                "route": "model",
            })),
        );
        assert!(matches!(person_command(&managed).unwrap(),
            WorkspaceTaskCommand::QueueTask { instruction: retained, .. }
                if retained == instruction));
        let manual = envelope(
            WORKSPACE,
            "create-task",
            Some(serde_json::json!({
                "title": instruction,
            })),
        );
        assert!(command(&manual).is_err());
    }

    #[test]
    fn manual_coordination_does_not_publish_managed_pending_edits() {
        let mut task = task("task:manual", "scribe");
        task.route = WorkspaceTaskRoute::Manual;
        task.assigned_agent = None;
        let controls = std::collections::BTreeMap::from([(
            task.id.clone(),
            tilo_desktop_model::WorkspaceTaskControl::default(),
        )]);
        let published = task_actions(&[task], &controls);
        assert!(published.iter().any(|action| action.action == "read-task"));
        assert!(!published.iter().any(|action| matches!(
            action.action.as_str(),
            "read-pending-task" | "amend-pending" | "cancel-pending"
        )));
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
                subscription: uuid::Uuid::new_v4(),
                custody: super::super::TaskSubscriptionCustody::Fixture,
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

    #[test]
    fn task_availability_watch_changes_only_for_the_exact_retained_subscription() {
        let fixture = native_run_fixture();
        let retained = fixture.state.task_subscribers.lock().unwrap()[&71].subscription;
        let mut changes = fixture.state.task_availability_changes();
        assert!(!changes.has_changed().unwrap());

        fixture
            .state
            .close_legacy_task_subscription(71, uuid::Uuid::new_v4());
        assert!(
            !changes.has_changed().unwrap(),
            "an old stream cannot retire its replacement"
        );

        fixture.state.close_legacy_task_subscription(71, retained);
        assert!(changes.has_changed().unwrap());
        assert_eq!(
            fixture
                .state
                .agent_task_availability(tilo_joined_room::testing::ROOM, "scribe"),
            Ok(false),
            "a healthy registry with no exact subscriber is unavailable, not unknown"
        );
        let _generation = *changes.borrow_and_update();
        fixture.state.close_legacy_task_subscription(71, retained);
        assert!(
            !changes.has_changed().unwrap(),
            "a no-op cleanup does not create an idle repaint edge"
        );
        drop(fixture);
    }

    #[test]
    fn task_availability_facade_closes_and_wakes_on_broker_stop() {
        let fixture = native_run_fixture();
        let broker =
            crate::collab_broker::AppActionBroker::from_state_for_test(Arc::clone(&fixture.state));
        let changes = broker.agent_task_availability_changes().unwrap();

        fixture.state.stop();

        assert!(changes.has_changed().unwrap());
        assert!(
            broker
                .agent_task_availability(tilo_joined_room::testing::ROOM, "scribe")
                .is_err(),
            "stopped observation is unknown, not a healthy unavailable Agent"
        );
        drop(broker);
        drop(fixture);
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
    fn pending_commands_require_exact_frontier_and_reject_authority_or_extra_content() {
        let resource = format!("{TASK_PREFIX}task:pending");
        let arguments = serde_json::json!({
            "status": "queued", "run_epoch": 0, "pending_revision": 3,
            "title": "New title", "instruction": "Complete\n  replacement\n",
        });
        let command = person_command(&envelope(
            &resource,
            "amend-pending",
            Some(arguments.clone()),
        ))
        .unwrap();
        let WorkspaceTaskCommand::AmendPending {
            task_id,
            frontier,
            instruction,
            ..
        } = command
        else {
            panic!("pending amendment")
        };
        assert_eq!(task_id, "task:pending");
        assert_eq!(frontier.revision, 3);
        assert_eq!(frontier.run_epoch, 0);
        assert_eq!(
            frontier.status,
            tilo_desktop_model::WorkspaceTaskStatus::Queued
        );
        assert_eq!(instruction, "Complete\n  replacement\n");
        for field in [
            "status",
            "run_epoch",
            "pending_revision",
            "title",
            "instruction",
        ] {
            let mut incomplete = arguments.clone();
            incomplete.as_object_mut().unwrap().remove(field);
            assert!(
                person_command(&envelope(&resource, "amend-pending", Some(incomplete))).is_err()
            );
        }
        for field in [
            "caller",
            "authority",
            "certificate",
            "room",
            "invocation",
            "assigned_agent",
        ] {
            let mut extra = arguments.clone();
            extra[field] = serde_json::json!("must not confer authority");
            assert!(person_command(&envelope(&resource, "amend-pending", Some(extra))).is_err());
        }
        assert!(person_command(&envelope(&resource, "cancel-pending", Some(arguments))).is_err());
        assert!(matches!(
            person_command(&envelope(
                &resource,
                "cancel-pending",
                Some(serde_json::json!({
                    "status": "waiting", "run_epoch": 7, "pending_revision": 3,
                }))
            ))
            .unwrap(),
            WorkspaceTaskCommand::CancelPending { .. }
        ));
    }

    #[test]
    fn pending_reader_fails_closed_for_missing_or_unbounded_instructions() {
        let room = "room-pending";
        let task = task("task:pending", "scribe");
        let read = envelope(
            &format!("{TASK_PREFIX}{}", task.id),
            "read-pending-task",
            None,
        );
        for instruction in [
            None,
            Some(" ".into()),
            Some("x".repeat(8_001)),
            Some("x".repeat(tilo_desktop_model::MAX_WORKSPACE_TASK_INSTRUCTION_BYTES + 1)),
        ] {
            let service = ReadOnlyTasks(crate::services::WorkspaceTaskSnapshot {
                room: room.into(),
                generation: 1,
                tasks: vec![task.clone()],
                controls: std::collections::BTreeMap::from([(
                    task.id.clone(),
                    tilo_desktop_model::WorkspaceTaskControl {
                        instruction_required: true,
                        instruction,
                        ..Default::default()
                    },
                )]),
            });
            assert!(read_pending_task(room, &read, &service).is_err());
        }
        let service = ReadOnlyTasks(crate::services::WorkspaceTaskSnapshot {
            room: room.into(),
            generation: 1,
            tasks: vec![task.clone()],
            controls: std::collections::BTreeMap::from([(
                task.id.clone(),
                tilo_desktop_model::WorkspaceTaskControl::default(),
            )]),
        });
        assert_eq!(
            read_pending_task(room, &read, &service).unwrap()["instruction"],
            task.title,
            "only explicitly legacy title-only work may use its title as instruction"
        );
    }

    #[test]
    fn pending_reader_and_catalogue_follow_current_owner_frontier() {
        use tilo_desktop_model::WorkspaceTaskStatus;

        let room = "room-pending";
        let mut task = task("task:pending", "scribe");
        let resource = format!("{TASK_PREFIX}{}", task.id);
        let read = envelope(&resource, "read-pending-task", None);
        let instruction = "Complete replacement\n  preserve indentation\n";
        let mut owner_control = control("scribe", 7, 0, instruction);
        owner_control.active_run = None;
        owner_control.pending_revision = 3;
        owner_control.queue_ordinal = Some(12);

        for status in [
            WorkspaceTaskStatus::Queued,
            WorkspaceTaskStatus::Waiting,
            WorkspaceTaskStatus::InProgress,
            WorkspaceTaskStatus::Done,
            WorkspaceTaskStatus::Cancelled,
        ] {
            task.status = status;
            for active in [false, true] {
                let mut current = owner_control.clone();
                if active {
                    current.active_run = control("scribe", 7, 0, instruction).active_run;
                }
                let service = ReadOnlyTasks(crate::services::WorkspaceTaskSnapshot {
                    room: room.into(),
                    generation: 9,
                    tasks: vec![task.clone()],
                    controls: std::collections::BTreeMap::from([(task.id.clone(), current)]),
                });
                let pending = matches!(
                    status,
                    WorkspaceTaskStatus::Queued | WorkspaceTaskStatus::Waiting
                ) && !active;
                let result = read_pending_task(room, &read, &service);
                assert_eq!(
                    result.is_ok(),
                    pending,
                    "status {status:?}, active {active}"
                );
                let actions = task_actions(&service.0.tasks, &service.0.controls);
                for verb in ["read-pending-task", "amend-pending", "cancel-pending"] {
                    assert_eq!(
                        actions
                            .iter()
                            .any(|action| action.resource == resource && action.action == verb),
                        pending,
                        "catalogue {verb}, status {status:?}, active {active}"
                    );
                }
                if let Ok(value) = result {
                    assert_eq!(value["generation"], 9);
                    assert_eq!(value["instruction"], instruction);
                    assert_eq!(value["queue_ordinal"], 12);
                    assert_eq!(
                        value["frontier"],
                        serde_json::json!({
                            "status": status, "run_epoch": 7, "revision": 3,
                        })
                    );
                    for arguments in [serde_json::json!({}), serde_json::json!({"room": room})] {
                        assert!(
                            read_pending_task(
                                room,
                                &envelope(&resource, "read-pending-task", Some(arguments)),
                                &service
                            )
                            .is_err()
                        );
                    }
                    assert!(read_pending_task("other-room", &read, &service).is_err());
                    let mut unavailable = service.0.clone();
                    unavailable.generation = 0;
                    assert!(read_pending_task(room, &read, &ReadOnlyTasks(unavailable)).is_err());
                    let mut unavailable = service.0.clone();
                    unavailable.controls.clear();
                    assert!(read_pending_task(room, &read, &ReadOnlyTasks(unavailable)).is_err());
                }
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_pending_amendment_delivers_replacement_and_loses_to_admission() {
        let NativeRunFixture {
            _directory,
            service,
            state,
            mut receiver,
            caller,
        } = native_run_fixture();
        let room = tilo_joined_room::testing::ROOM;
        let task_id = queue_native_task(service.as_ref(), room, caller.clone()).await;
        let resource = format!("{TASK_PREFIX}{task_id}");
        let read = read_pending_task(
            room,
            &envelope(&resource, "read-pending-task", None),
            service.as_ref(),
        )
        .unwrap();
        assert_eq!(read["instruction"], "Line one.\n\nKeep all details.");
        assert_eq!(read["frontier"]["revision"], 0);
        let command = person_command(&envelope(
            &resource,
            "amend-pending",
            Some(serde_json::json!({
                "status": read["frontier"]["status"], "run_epoch": read["frontier"]["run_epoch"],
                "pending_revision": read["frontier"]["revision"],
                "title": "Replacement", "instruction": "Exact replacement\n  with every detail.\n",
            })),
        ))
        .unwrap();
        commit_person_command(service.as_ref(), room, command, 401, caller.clone()).await;
        let amended = read_pending_task(
            room,
            &envelope(&resource, "read-pending-task", None),
            service.as_ref(),
        )
        .unwrap();
        assert_eq!(amended["frontier"]["revision"], 1);
        assert_eq!(amended["queue_ordinal"], read["queue_ordinal"]);
        assert_eq!(amended["title"], "Replacement");
        let snapshot = service.snapshot(room).unwrap();
        assert!(
            task_actions(&snapshot.tasks, &snapshot.controls)
                .iter()
                .any(|action| action.resource == resource && action.action == "amend-pending")
        );
        state
            .dispatch_queued_workspace_task(room, "scribe")
            .await
            .unwrap()
            .unwrap();
        let delivered = receiver.try_recv().unwrap();
        assert_eq!(
            delivered.instruction.as_deref(),
            Some("Exact replacement\n  with every detail.\n")
        );
        let cancel = person_command(&envelope(
            &resource,
            "cancel-pending",
            Some(serde_json::json!({
                "status": "queued", "run_epoch": 0, "pending_revision": 1,
            })),
        ))
        .unwrap();
        let request = OwnerCommitRequest {
            room: room.into(),
            operation: CommitOperation::WorkspaceTaskCommand {
                invocation: uuid::Uuid::from_u128(402),
                authority: WorkspaceTaskAuthority::Person { caller },
                command: cancel,
            },
        };
        assert!(service.execute(request).await.is_err());
        let current = service.snapshot(room).unwrap();
        assert_eq!(
            current.tasks[0].status,
            tilo_desktop_model::WorkspaceTaskStatus::InProgress
        );
        assert!(!task_actions(&current.tasks, &current.controls).iter().any(
            |action| action.resource == resource
                && matches!(
                    action.action.as_str(),
                    "amend-pending" | "cancel-pending" | "read-pending-task"
                )
        ));
        assert!(
            read_pending_task(
                room,
                &envelope(&resource, "read-pending-task", None),
                service.as_ref()
            )
            .is_err()
        );
        assert!(
            receiver.try_recv().is_err(),
            "refused cancellation cannot dispatch another run"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_pending_cancellation_prevents_dispatch_and_replays_exact_receipt() {
        let NativeRunFixture {
            _directory,
            service,
            state,
            mut receiver,
            caller,
        } = native_run_fixture();
        let room = tilo_joined_room::testing::ROOM;
        let task_id = queue_native_task(service.as_ref(), room, caller.clone()).await;
        let resource = format!("{TASK_PREFIX}{task_id}");
        let command = person_command(&envelope(
            &resource,
            "cancel-pending",
            Some(serde_json::json!({
                "status": "queued", "run_epoch": 0, "pending_revision": 0,
            })),
        ))
        .unwrap();
        let request = OwnerCommitRequest {
            room: room.into(),
            operation: CommitOperation::WorkspaceTaskCommand {
                invocation: uuid::Uuid::from_u128(501),
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
        service
            .execute(request.clone())
            .await
            .unwrap()
            .validate_for(&request)
            .unwrap();
        let snapshot = service.snapshot(room).unwrap();
        assert_eq!(
            snapshot.tasks[0].status,
            tilo_desktop_model::WorkspaceTaskStatus::Cancelled
        );
        let control = &snapshot.controls[&task_id];
        assert!(control.cancellation.is_none());
        assert_eq!(control.pending_revision, 1);
        assert_eq!(
            control
                .receipts
                .iter()
                .filter(|receipt| receipt.invocation == uuid::Uuid::from_u128(501))
                .count(),
            1
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
            controls: ["task:z-first", "task:a-later"]
                .into_iter()
                .map(|id| {
                    (
                        id.into(),
                        tilo_desktop_model::WorkspaceTaskControl {
                            instruction: Some(format!("Execute {id}")),
                            instruction_required: true,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
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
                        instruction: Some(format!("Execute {id}")),
                        instruction_required: true,
                        ..Default::default()
                    },
                )
            })
            .collect(),
        });
        assert_eq!(next_queued_task(&service.0, "scribe"), Some("task:z-first"));
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
    async fn approval_required_task_does_not_block_managed_subscriber_delivery() {
        let NativeRunFixture {
            _directory,
            service,
            state,
            mut receiver,
            caller,
        } = native_run_fixture();
        let room = tilo_joined_room::testing::ROOM;
        commit_person_command(
            service.as_ref(),
            room,
            WorkspaceTaskCommand::Queue {
                title: "Review the approved native task".into(),
                assigned_agent: "scribe".into(),
                route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                resource: None,
            },
            401,
            caller.clone(),
        )
        .await;
        commit_person_command(
            service.as_ref(),
            room,
            WorkspaceTaskCommand::QueueTask {
                title: "Managed follow-up".into(),
                instruction: "Execute only this retained managed instruction".into(),
                assigned_agent: "scribe".into(),
                route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                resource: None,
            },
            402,
            caller,
        )
        .await;
        let before = service.snapshot(room).unwrap();
        let ordinary = before
            .tasks
            .iter()
            .find(|task| {
                !before.controls[&task.id].instruction_required
                    && before.controls[&task.id].instruction.is_none()
            })
            .unwrap()
            .id
            .clone();
        let managed = before
            .tasks
            .iter()
            .find(|task| before.controls[&task.id].instruction_required)
            .unwrap()
            .id
            .clone();

        assert!(
            state
                .dispatch_queued_workspace_task(room, "scribe")
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(receiver.recv().await.unwrap().task_id, managed);
        let after = service.snapshot(room).unwrap();
        assert_eq!(
            after
                .tasks
                .iter()
                .find(|task| task.id == ordinary)
                .unwrap()
                .status,
            tilo_desktop_model::WorkspaceTaskStatus::Queued
        );
        assert_eq!(
            after
                .tasks
                .iter()
                .find(|task| task.id == managed)
                .unwrap()
                .status,
            tilo_desktop_model::WorkspaceTaskStatus::InProgress
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
                subscription: uuid::Uuid::new_v4(),
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
                subscription: uuid::Uuid::new_v4(),
                custody: super::super::TaskSubscriptionCustody::Fixture,
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
                approval: None,
            }),
            ..Default::default()
        }
    }
}

// Manual coordination and next-run owner policy use the existing membership service.
const WORKSPACE: &str = "desktop:workspace";
const MAX_ARGUMENT_BYTES: usize = 8192;

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

fn argument(envelope: &InvocationEnvelope) -> Result<&str, String> {
    let encoded = envelope
        .argument
        .as_deref()
        .ok_or("task arguments missing")?;
    if encoded.len() > MAX_ARGUMENT_BYTES {
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

pub(super) fn command(
    envelope: &InvocationEnvelope,
) -> Result<Option<WorkspaceTaskCommand>, String> {
    if let Some(id) = envelope.resource.strip_prefix(TASK_PREFIX)
        && envelope.action == "approve-run"
    {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            agent_id: String,
            expected_run_epoch: u64,
            after_revision: u64,
        }
        let input: Input =
            serde_json::from_str(argument(envelope)?).map_err(|error| error.to_string())?;
        return Ok(Some(WorkspaceTaskCommand::ApproveRun {
            task_id: format!("task:{id}"),
            agent_id: input.agent_id,
            expected_run_epoch: input.expected_run_epoch,
            after_revision: input.after_revision,
        }));
    }
    if let Some(id) = envelope.resource.strip_prefix(TASK_PREFIX)
        && envelope.action == "set-execution-budget"
    {
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
        return Ok(Some(WorkspaceTaskCommand::SetExecutionBudget {
            task_id: format!("task:{id}"),
            expected_run_epoch: input.expected_run_epoch,
            expected_budget: input.expected_budget,
            budget: input.budget,
        }));
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

fn manual(task: &WorkspaceTask) -> bool {
    task.route == WorkspaceTaskRoute::Manual && task.assigned_agent.is_none()
}

fn coordination_queue_action(workspace_label: &str) -> PublishedAction {
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
) -> Vec<PublishedAction> {
    use tilo_actions::{ActionParam, ParamType};
    let mut result = vec![action(
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
    )];
    result.push(coordination_queue_action(workspace_label));
    for task in &snapshot.tasks {
        let target = format!("desktop:{}", task.id);
        result.push(action(
            &target,
            "inspect",
            &task.title,
            "Inspect task execution policy",
            Vec::new(),
            true,
        ));
        if task.status == tilo_desktop_model::WorkspaceTaskStatus::Queued
            && !manual(task)
            && task.assigned_agent.is_some()
            && task
                .resource
                .as_ref()
                .is_none_or(|resource| resource.agent_request.is_none())
            && snapshot.controls.get(&task.id).is_some_and(|value| {
                value.active_run.is_none()
                    && !value.instruction_required
                    && value.instruction.is_none()
            })
        {
            result.push(approve_action(&target, &task.title));
        }
        if !manual(task)
            && !task.status.is_finished()
            && snapshot
                .controls
                .get(&task.id)
                .is_some_and(|control| control.active_run.is_none())
        {
            let mut operation = action(
                &target,
                "set-execution-budget",
                &task.title,
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
                    )
                    .nullable(),
                    ActionParam::optional(
                        "budget",
                        ParamType::Object,
                        "Owner policy: turn_ms up to 900000, total_ms up to7200000, turn <= total; null restores defaults",
                    )
                    .nullable(),
                ],
                false,
            );
            operation.description = Some("Only the current workspace owner can configure the next task run. This grants no action, model, check or resource authority.".into());
            result.push(operation);
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

fn approve_action(target: &str, title: &str) -> PublishedAction {
    use tilo_actions::{ActionParam, ParamType};
    action(
        target,
        "approve-run",
        title,
        "Approve this task run",
        vec![
            ActionParam::required("agent_id", ParamType::Text, "Exact assigned agent"),
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
        description: Some(if inspect { "Inspect current task coordination and execution policy; no execution result is asserted." } else if operation == "approve-run" { "Approve the observed queued task for its assigned agent. Existing workspace and action permissions still apply; approval does not assert execution or completion." } else { "Manual coordination only. Attachments are shared resource references; no agent run, execution, integration or verification is asserted." }.into()),
        operation_kind: if inspect { tilo_actions::OperationKind::View } else { tilo_actions::OperationKind::Document }, params,
        contract: tilo_actions::ActionContract {
            permission: if inspect { tilo_actions::PermissionRequirement::Read } else { tilo_actions::PermissionRequirement::Write },
            result: tilo_actions::ResultSpec::Json { schema: serde_json::json!({"type":"object"}) },
            ..Default::default()
        }, view: None, input: PublishedInput::None,
    }
}

pub(super) fn is_inspect(envelope: &InvocationEnvelope) -> bool {
    envelope.resource.starts_with(TASK_PREFIX) && envelope.action == "inspect"
}

pub(super) fn inspect(
    snapshot: &WorkspaceTaskSnapshot,
    envelope: &InvocationEnvelope,
) -> Result<serde_json::Value, String> {
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
    let mut output = serde_json::json!({"task":task,"coordination_only":manual(task),
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
        | WorkspaceTaskMutationResult::RunStarted { task_id, .. } => (task_id, false),
        WorkspaceTaskMutationResult::ExecutionBudgetSet { task_id } => (task_id, true),
        _ => return Err("task command returned an unexpected managed execution result".into()),
    };
    let task = snapshot
        .tasks
        .iter()
        .find(|task| &task.id == task_id)
        .ok_or("owner task receipt has no canonical task")?;
    let queued_agent = envelope.resource == WORKSPACE && envelope.action == "queue-agent-task";
    let approved = envelope.action == "approve-run";
    if (!approved && !budget_change && !queued_agent && !manual(task))
        || (queued_agent
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
mod coordination_tests {
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
        request: &tilo_peer_state::owner_commit::OwnerCommitRequest,
        certificate: &tilo_user_identity::DeviceCert,
        snapshot: &WorkspaceTaskSnapshot,
        context: (tilo_desktop_model::RoomId, std::time::SystemTime),
    ) {
        use tilo_peer_state::owner_commit::CommitOperation;
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
        use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
        use tilo_peer_state::workspace_membership::{
            AgentMembershipPreset, WorkspaceMembershipCommand,
        };
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
            preset: AgentMembershipPreset::Contributor,
        });
        owner
            .commit_workspace_membership_command(enrolled.clone(), &certificate, &key, now)
            .unwrap()
            .validate_for(&enrolled)
            .unwrap();
        let before = owner.workspace_task_snapshot(room).unwrap();
        let published = actions(&before, "Workspace")
            .into_iter()
            .find(|row| row.action == "queue-agent-task")
            .unwrap();
        assert_eq!(
            published.contract.permission,
            tilo_actions::PermissionRequirement::Write
        );
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
    fn published_budget_contract_accepts_explicit_absence_but_not_non_objects() {
        let task = WorkspaceTask {
            id: "task:one".into(),
            title: "Review RTL".into(),
            created_by: "owner".into(),
            assigned_agent: Some("scribe".into()),
            route: WorkspaceTaskRoute::Model,
            status: tilo_desktop_model::WorkspaceTaskStatus::Queued,
            resource: None,
            created_at: 1,
            updated_at: 1,
            audit: Vec::new(),
        };
        let snapshot = WorkspaceTaskSnapshot {
            tasks: vec![task],
            controls: [(
                "task:one".into(),
                tilo_desktop_model::WorkspaceTaskControl::default(),
            )]
            .into_iter()
            .collect(),
        };
        let published = actions(&snapshot, "Workspace")
            .into_iter()
            .find(|action| {
                action.resource == "desktop:task:one" && action.action == "set-execution-budget"
            })
            .expect("eligible task publishes its budget action");
        let explicit_absence = serde_json::json!({
            "expected_run_epoch": 0,
            "expected_budget": null,
            "budget": null,
        });
        let supplied = explicit_absence
            .as_object()
            .expect("test arguments are an object");
        let bound = tilo_actions::ActionArgs::bind_json(&published.params, supplied)
            .expect("the published typed contract accepts its documented null values");
        assert_eq!(serde_json::Value::Object(bound.to_json()), explicit_absence);

        let mut request = envelope(&explicit_absence);
        request.resource = "desktop:task:one".into();
        request.action = "set-execution-budget".into();
        assert!(matches!(
            command(&request).unwrap(),
            Some(WorkspaceTaskCommand::SetExecutionBudget {
                expected_run_epoch: 0,
                expected_budget: None,
                budget: None,
                ..
            })
        ));

        for malformed in [
            serde_json::json!({"expected_run_epoch": 0, "expected_budget": "none"}),
            serde_json::json!({"expected_run_epoch": 0, "budget": []}),
        ] {
            assert!(
                tilo_actions::ActionArgs::bind_json(
                    &published.params,
                    malformed.as_object().expect("test arguments are an object"),
                )
                .is_err()
            );
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

    #[test]
    fn task_catalog_exposes_managed_policy_without_execution_authority() {
        let mut document = automerge::AutoCommit::new();
        // Project the real typed production document shape, then read through
        // the same canonical snapshot function used by the installed service.
        let mut task = WorkspaceTask {
            id: "task:00000000-0000-4000-8000-000000000001".into(),
            title: "Review report".into(),
            created_by: "person".into(),
            assigned_agent: None,
            route: WorkspaceTaskRoute::Manual,
            status: tilo_desktop_model::WorkspaceTaskStatus::Queued,
            resource: None,
            created_at: 1,
            updated_at: 1,
            audit: Vec::new(),
        };
        tilo_desktop_model::projection::project_workspace_tasks(&mut document, &[task.clone()])
            .unwrap();
        let mut snapshot = tilo_peer_state::workspace_task::snapshot(&document).unwrap();
        snapshot.controls.insert(
            task.id.clone(),
            tilo_desktop_model::WorkspaceTaskControl::default(),
        );
        let catalogue = actions(&snapshot, "Workspace");
        assert_eq!(catalogue.len(), 4);
        assert_eq!(catalogue[0].resource_label, "Workspace");
        assert_eq!(catalogue[0].resource_kind, "workspace-membership");
        let inspect_action = catalogue
            .iter()
            .find(|row| row.action == "inspect")
            .unwrap();
        assert_eq!(
            inspect_action.contract.permission,
            tilo_actions::PermissionRequirement::Read
        );
        let mut request = envelope(&serde_json::json!({}));
        request.resource = format!("desktop:{}", task.id);
        request.action = "inspect".into();
        request.argument = None;
        assert_eq!(inspect(&snapshot, &request).unwrap()["task"]["id"], task.id);
        task.assigned_agent = Some("scribe".into());
        task.route = WorkspaceTaskRoute::Model;
        snapshot.tasks = vec![task];
        let managed = actions(&snapshot, "Workspace");
        assert_eq!(managed.len(), 5);
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
        let control = snapshot.controls.get_mut(&snapshot.tasks[0].id).unwrap();
        control.instruction = Some("Complete the governed managed task".into());
        control.instruction_required = true;
        let enrolled = actions(&snapshot, "Workspace");
        assert_eq!(enrolled.len(), 4);
        assert!(!enrolled.iter().any(|row| row.action == "approve-run"));
    }
}
