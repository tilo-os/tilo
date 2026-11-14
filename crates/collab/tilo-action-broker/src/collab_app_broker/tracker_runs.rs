//! Runs a tracker holds for its agents.
//!
//! A run answers a person's message in a conversation, or serves an agent
//! door's connection. The broker delivers the run to its agent as it
//! delivers a workspace task's, under the same native lease and binding, and
//! reads everything else about it from the tracker:
//!
//! - **Delivery**: every going run whose agent is subscribed in the workspace.
//! - **Stopping**: a run the tracker no longer holds as going is cancelled.
//! - **Admission**: the agent acting for the run's starter, on the starter's
//!   live grants and the agent's ([`TrackerRuns::authorize_action`]).
//! - **Asking first**: an action that asks first waits for the person's
//!   answer to the run's question, where the run keeps one. A confirmation
//!   admits exactly the arguments asked about, and only when the draft the
//!   person saw is the one these arguments render: what the agent recorded
//!   is matched against, never trusted.
//!
//! The broker writes nothing into the tracker. The agent reports, asks and
//! settles through the tracker's own actions, as children of its run, so
//! each step is an action the agent's grants decide and the run's record
//! keeps.

use std::sync::Arc;

use super::native_task_confirmation::NativeGate;
use super::{AppBrokerState, PendingTaskRun, PublishedAction, StagedJob, TaskRunEnvelope};
use crate::services::{
    NativeTaskActionRequirement, NativeTaskBinding, TrackerAnswer, TrackerRun, TrackerRuns,
};

#[cfg(test)]
#[path = "tracker_runs_tests.rs"]
mod tests;

/// The run of `binding` among `runs`: the same epoch and agent.
fn run_of<'a>(runs: &'a [TrackerRun], binding: &NativeTaskBinding) -> Option<&'a TrackerRun> {
    runs.iter()
        .find(|run| run.issue == binding.task_id && run.agent == binding.agent_id)
}

/// What `run`'s agent may spend on the day its keeper counts, as the record of
/// the workspace's agents holds it: `None` where this host reads no such
/// record, and then the agent works unmetered.
fn spend_of(
    state: &AppBrokerState,
    workspace: &str,
    run: &TrackerRun,
) -> Option<runtime_collab::TaskRunSpend> {
    state
        .services
        .agents
        .as_deref()?
        .spend(workspace, &run.agent)
}

/// The arguments a person confirmed (with their edits) and the question's id.
type Confirmed = (serde_json::Map<String, serde_json::Value>, String);

/// How the person's answer to `run`'s question lets the child with
/// `arguments` start, `draft` being what those arguments render. `None` when
/// the run asked nothing about exactly these arguments.
pub(super) fn answered(
    run: &TrackerRun,
    (resource, action): (&str, &str),
    arguments: &serde_json::Map<String, serde_json::Value>,
    draft: &tilo_actions::Draft,
) -> Result<Option<Confirmed>, String> {
    let digest = hex::encode(tilo_actions::draft::arguments_digest(arguments));
    let Some(question) = run.question.as_ref().filter(|question| {
        question.resource == resource
            && question.action == action
            && question.arguments_digest == digest
    }) else {
        return Ok(None);
    };
    let shown: tilo_actions::Draft = serde_json::from_value(question.draft.clone())
        .map_err(|_| "the run's question carries no readable draft")?;
    if &shown != draft {
        return Err("the run's question shows another draft than these arguments render".into());
    }
    match &question.answer {
        TrackerAnswer::Waiting => Err(awaiting(draft)),
        TrackerAnswer::Confirmed { edits } => {
            let edits: Vec<tilo_actions::DraftEdit> = if edits.is_null() {
                Vec::new()
            } else {
                serde_json::from_value(edits.clone())
                    .map_err(|_| "the person's edits do not read")?
            };
            Ok(Some((
                draft.apply_edits(arguments, &edits)?,
                question.id.clone(),
            )))
        }
        TrackerAnswer::Declined { reason } => {
            let declined = format!(
                "{}: the person declined {}",
                tilo_actions::draft::DECLINED,
                draft.action_label
            );
            Err(if reason.trim().is_empty() {
                declined
            } else {
                format!("{declined}: {reason}")
            })
        }
    }
}

/// What a child that asks first is answered with until the person answers
/// the run's question: ask it through the tracker, with this draft.
fn awaiting(draft: &tilo_actions::Draft) -> String {
    format!(
        "{}: {} waits for the person to confirm it on the issue",
        tilo_actions::draft::AWAITING_CONFIRMATION,
        draft.action_label
    )
}

impl AppBrokerState {
    pub(super) fn tracker_runs(&self, workspace: &str) -> Option<&Arc<dyn TrackerRuns>> {
        self.services
            .tracker_runs
            .as_ref()
            .filter(|service| service.requires_workspace(workspace))
    }

    /// The tracker's current record of `run`.
    pub(super) fn tracker_run_of(&self, run: &PendingTaskRun) -> Result<TrackerRun, String> {
        let binding = &run.native;
        let runs = self
            .tracker_runs(&run.cluster_id)
            .ok_or("the tracker is unavailable")?
            .runs(&run.cluster_id)?;
        run_of(&runs, binding)
            .cloned()
            .ok_or_else(|| "the run was replaced".into())
    }

    /// Deliver every going run of the subscribed workspaces' trackers to its
    /// agent, and tell its person what it asks.
    pub(super) fn refresh_tracker_runs(
        self: &Arc<Self>,
        workspaces: &std::collections::BTreeSet<String>,
    ) {
        for workspace in workspaces {
            let Some(service) = self.tracker_runs(workspace) else {
                continue;
            };
            let Ok(runs) = service.runs(workspace) else {
                continue;
            };
            // A turn's ask whose turn ended stands on nobody's screen: the
            // sheet that showed it is withdrawn with it.
            self.prune_conversation_asks(&runs);
            for run in runs.iter().filter(|run| run.active) {
                if let Err(error) = self.dispatch_tracker_run(workspace, run) {
                    tracing::warn!(%error, run = %run.issue, "an approved run was not delivered");
                }
            }
        }
    }

    fn dispatch_tracker_run(&self, workspace: &str, run: &TrackerRun) -> Result<u64, String> {
        let subscriber = self
            .task_subscribers
            .lock()
            .map_err(|_| "task subscribers unavailable")?
            .iter()
            .filter(|(_, subscriber)| {
                subscriber.cluster_id == workspace
                    && subscriber.agent_id == run.agent
                    && !subscriber.tx.is_closed()
            })
            .min_by_key(|(peer, _)| **peer)
            .map(|(peer, subscriber)| (*peer, subscriber.tx.clone()))
            .ok_or("assigned agent is offline")?;
        let digest = blake3::hash(
            format!("tracker-run-v1\0{workspace}\0{}\0{}", run.issue, run.epoch).as_bytes(),
        );
        let run_id = u64::from_le_bytes(
            *digest
                .as_bytes()
                .first_chunk::<8>()
                .ok_or("invalid run digest")?,
        ) | (1 << 63);
        let hex = digest.to_hex();
        let lease = format!(
            "native:{}",
            hex.as_str().get(..32).ok_or("invalid run digest")?
        );
        let context = tilo_actions::ActionContext::default()
            .with_purpose(run.title.clone())
            .with_group(
                runtime_collab::task_run_group(&run.issue, run_id),
                run.title.clone(),
                tilo_actions::context::TASK_RUN_GROUP,
            );
        self.offer_native_run(
            workspace,
            NativeTaskBinding {
                task_id: run.issue.clone(),
                agent_id: run.agent.clone(),
                epoch: run.epoch,
                starter: Some(run.starter.clone()),
                turn: run.turn.clone(),
                origin: run.origin.clone(),
            },
            TaskRunEnvelope {
                task_id: run.issue.clone(),
                title: run.title.clone(),
                instruction: Some(run.instruction.clone()),
                subject: run.subject.clone(),
                run_id,
                lease,
                planning: runtime_collab::TaskRunPlanning::Model,
                action_context: context,
                spend: spend_of(self, workspace, run),
            },
            subscriber,
        )
    }

    /// Whether `run` was cancelled: the tracker no longer holds it as going.
    pub(super) fn tracker_status(&self, run: &PendingTaskRun) -> Result<bool, String> {
        let binding = &run.native;
        let runs = self
            .tracker_runs(&run.cluster_id)
            .ok_or("the tracker is unavailable")?
            .runs(&run.cluster_id)?;
        let Some(current) = run_of(&runs, binding).cloned() else {
            // The turn the run answered is over — its reply finished, or
            // its person ended it — so the run is over too: its agent stops
            // and the steps it left running stop with it.
            self.cancel_native_children(run)?;
            return Ok(true);
        };
        if current.epoch != binding.epoch {
            // A later turn of the same conversation replaced this one.
            self.cancel_native_children(run)?;
            return Ok(true);
        }
        self.tracker_runs(&run.cluster_id)
            .ok_or("the tracker is unavailable")?
            .check_agent(&run.cluster_id, &current)?;
        // A run its own agent finished (its `finish-run`, done or failed) is
        // over, not cancelled: the report that ended it is a child of it still
        // being followed to its result, and the agent then reports to the host
        // how it ended. Reading that as a cancellation cancelled the report
        // itself, so a run that answered at once could not say it had.
        if !current.active && current.finished_by_agent {
            return Ok(false);
        }
        if !current.active {
            self.cancel_native_children(run)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// An agent's report on its run of an issue. The tracker is told by the
    /// agent itself, through the tracker's actions; here a run that ends
    /// stops its children and goes to history, as one run to undo whole.
    pub(super) fn report_tracker_run(
        &self,
        run: &PendingTaskRun,
        update: &runtime_collab::TaskRunUpdate,
    ) -> Result<(), String> {
        if let Some(service) = self.tracker_runs(&run.cluster_id) {
            service.reported(&run.cluster_id, &run.task_id, run.native.epoch, update);
        }
        let outcome = match update {
            runtime_collab::TaskRunUpdate::Succeeded { .. } => "completed",
            runtime_collab::TaskRunUpdate::Failed { .. } => "stopped",
            runtime_collab::TaskRunUpdate::Cancelled { .. } => "cancelled",
            runtime_collab::TaskRunUpdate::Progress {
                presentation: Some(presentation),
                ..
            } => {
                // What the turn streams goes to whoever follows the run
                // here, as the agent reported it, bounded.
                if let (Some(service), Ok(presentation)) = (
                    self.tracker_runs(&run.cluster_id),
                    presentation.clone().bounded(),
                ) {
                    service.presented(
                        &run.cluster_id,
                        &run.task_id,
                        run.native.epoch,
                        Some(&presentation),
                    );
                }
                return Ok(());
            }
            _ => return Ok(()),
        };
        if let Some(service) = self.tracker_runs(&run.cluster_id) {
            service.presented(&run.cluster_id, &run.task_id, run.native.epoch, None);
        }
        // A run that ends stops what it left running — except the record
        // that says how it ended, which is a child still being taken in
        // when the refusal or failure is reported.
        if outcome == "cancelled" {
            self.cancel_native_children(run)?;
        }
        let ended = self.tracker_run_of(run).ok();
        let group = run.action_context.group.as_ref();
        self.ended_runs.tasks.push(crate::task_runs::EndedTaskRun {
            workspace: run.cluster_id.clone(),
            group: group.map_or_else(|| run.task_id.clone(), |group| group.id.clone()),
            title: group.map_or_else(|| run.task_id.clone(), |group| group.label.clone()),
            agent: format!("agent:{}", run.agent_id),
            person: run.native.starter.clone().unwrap_or_default(),
            instruction: ended.map(|ended| ended.instruction),
            outcome: outcome.to_owned(),
        });
        Ok(())
    }

    /// Admit `run`'s child under `requirement`, then start it.
    ///
    /// # Errors
    /// As [`TrackerRuns::authorize_action`].
    pub(super) fn admit_tracker_action(
        &self,
        run: &PendingTaskRun,
        requirement: &NativeTaskActionRequirement,
        action: &mut dyn FnMut() -> Result<StagedJob, String>,
    ) -> Result<StagedJob, String> {
        self.authorize_task_action(run, requirement)?;
        action()
    }

    /// The person's answer to a child of `run` that asks first, as
    /// [`native_confirmation_for`](Self::native_confirmation_for) decides it
    /// for a workspace task.
    pub(super) fn tracker_confirmation(
        &self,
        run: &PendingTaskRun,
        (app, offered): (&str, &PublishedAction),
        (resource, action): (&str, &str),
        argument: Option<&str>,
    ) -> Result<NativeGate, String> {
        let binding = run.native.clone();
        let prefixed =
            argument.and_then(|text| text.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX));
        let json = prefixed.or(argument);
        let arguments = match json {
            Some(text) => serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(text)
                .map_err(|_| "task action arguments are not a JSON object")?,
            None => serde_json::Map::new(),
        };
        let descriptor = offered.descriptor(tilo_actions::ExecutionLocus::ClientLocal);
        let draft = tilo_actions::Draft::render(&descriptor, &arguments);
        let current = self.tracker_run_of(run)?;
        let logical = tilo_actions::selection::logical_resource_id(resource);
        if let Some((edited, id)) = answered(&current, (&logical, action), &arguments, &draft)? {
            let edited = serde_json::Value::Object(edited).to_string();
            return Ok(NativeGate::Confirmed {
                argument: if prefixed.is_some() {
                    format!("{}{edited}", tilo_actions::ARGUMENTS_JSON_PREFIX)
                } else {
                    edited
                },
                confirmation: id,
            });
        }
        let contract_argument =
            json.map(|text| format!("{}{text}", tilo_actions::ARGUMENTS_JSON_PREFIX));
        let consequential = descriptor
            .contract
            .asks_for_argument(contract_argument.as_deref());
        let requirement = NativeTaskActionRequirement {
            binding,
            app: app.to_owned(),
            resource_kind: descriptor.resource_kind.clone(),
            resource: resource.into(),
            action: descriptor.address.action.clone(),
            need: descriptor.contract.need(),
            consequential,
            attended: descriptor.contract.is_attended(),
            confirmed: false,
        };
        match self
            .tracker_runs(&run.cluster_id)
            .ok_or("the tracker is unavailable")?
            .authorize_action(&run.cluster_id, &current, &requirement)
        {
            Ok(()) => Ok(NativeGate::Proceed),
            Err(error) if NativeTaskActionRequirement::awaits_confirmation(&error) => {
                Err(awaiting(&draft))
            }
            Err(error) => Err(error),
        }
    }

    /// Authorize `requirement` for `run`'s agent on its tracker.
    pub(super) fn authorize_task_action(
        &self,
        run: &PendingTaskRun,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String> {
        let current = self.tracker_run_of(run)?;
        self.tracker_runs(&run.cluster_id)
            .ok_or("the tracker is unavailable")?
            .authorize_action(&run.cluster_id, &current, requirement)
    }

    /// Admit a host activation for `run`'s child as [`Self::admit_tracker_action`]
    /// admits the child.
    pub(super) fn admit_task_launch(
        &self,
        run: &PendingTaskRun,
        requirement: &NativeTaskActionRequirement,
        launch: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let current = self.tracker_run_of(run)?;
        self.tracker_runs(&run.cluster_id)
            .ok_or("the tracker is unavailable")?
            .authorize_action(&run.cluster_id, &current, requirement)?;
        launch()
    }

    /// Whether the workspace's tracker serves runs here: a task agent may
    /// subscribe to its runs and its retained surfaces are offered.
    pub(super) fn task_admitted_workspace(&self, workspace: &str) -> bool {
        self.tracker_runs(workspace).is_some()
    }

    /// Whether agents in `workspace` act only in a task's run or an agent
    /// session ([`crate::services::TrackerRuns::confines_agents_to_tasks`]).
    pub fn confines_agents_to_tasks(&self, workspace: &str) -> bool {
        self.tracker_runs(workspace)
            .is_some_and(|runs| runs.confines_agents_to_tasks(workspace))
    }
}
