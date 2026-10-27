//! Runs of issues in the Tasks app's tracker.
//!
//! An issue assigned to an agent is that agent's task, and a person's
//! `approve-run` on it starts a run (`apps/tasks`). The broker delivers the
//! run to its agent as it delivers a room task's, under the same native lease
//! and binding, and reads everything else about it from the tracker:
//!
//! - **Delivery**: every going run whose agent is subscribed in the room.
//! - **Steering**: the issue's messages to the run, in thread order; the
//!   revision an agent reached is how many of them it took in.
//! - **Stopping**: a run the tracker no longer holds as going is cancelled.
//! - **Admission**: the agent acting for the run's starter, on the starter's
//!   live grants and the agent's ([`TrackerRuns::authorize_action`]), with
//!   no new message unread.
//! - **Asking first**: an action that asks first waits for the person's
//!   answer to the run's question in the tracker. A confirmation admits
//!   exactly the arguments asked about, and only when the draft the person
//!   saw is the one these arguments render: what the agent recorded is
//!   matched against, never trusted.
//!
//! The broker writes nothing into the tracker. The agent reports, asks and
//! settles through the tracker's own actions, as children of its run, so
//! each step is an action the agent's grants decide and the issue's history
//! records.

use std::sync::Arc;

use super::native_task_confirmation::NativeGate;
use super::{CapnpBrokerState, PendingTaskRun, PublishedAction, StagedJob, TaskRunEnvelope};
use crate::services::{
    NativeTaskActionRequirement, NativeTaskBinding, TrackerAnswer, TrackerRun, TrackerRuns,
};

#[cfg(test)]
#[path = "tracker_runs_tests.rs"]
mod tests;

/// The run of `binding` among `runs`: the issue's run of the same epoch and
/// agent.
fn run_of<'a>(runs: &'a [TrackerRun], binding: &NativeTaskBinding) -> Option<&'a TrackerRun> {
    runs.iter()
        .find(|run| run.issue == binding.task_id && run.agent == binding.agent_id)
}

/// The steering page of `run` for an agent whose lease reached `revision`.
///
/// # Errors
/// The lease claims messages the agent never took in.
pub(super) fn control_of(
    run: &TrackerRun,
    revision: u64,
) -> Result<runtime_collab::TaskRunControl, String> {
    let applied = run.applied_revision();
    if revision > applied {
        return Err("task lease claims an unacknowledged direction".into());
    }
    let latest = run.latest_revision();
    let skip = usize::try_from(revision).unwrap_or(usize::MAX);
    let directions = run
        .directions
        .iter()
        .enumerate()
        .skip(skip)
        .take(runtime_collab::TaskRunControl::MAX_DIRECTIONS)
        .map(|(index, direction)| runtime_collab::TaskDirection {
            revision: u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1),
            author: direction.author.clone(),
            text: direction.text.clone(),
            issued_at: direction.at_ms / 1000,
        })
        .collect::<Vec<_>>();
    let last = directions
        .last()
        .map_or(revision, |direction| direction.revision);
    let control = runtime_collab::TaskRunControl {
        version: 1,
        task_id: run.issue.clone(),
        run_epoch: run.epoch,
        from_revision: revision,
        applied_revision: applied,
        latest_revision: latest,
        directions,
        has_more: last < latest,
    };
    control.validate()?;
    Ok(control)
}

/// The step `run` is paused at: its question, while the person has not
/// answered it, so an agent that restarted resumes there rather than asking
/// again.
pub(super) fn paused_step(run: &TrackerRun) -> Option<runtime_collab::TaskRunPausedStep> {
    let question = run
        .question
        .as_ref()
        .filter(|question| question.answer == TrackerAnswer::Waiting)?;
    let digest = hex::decode(&question.arguments_digest).ok()?;
    Some(runtime_collab::TaskRunPausedStep {
        app: question.app.clone(),
        resource: question.resource.clone(),
        action: question.action.clone(),
        draft: serde_json::from_value(question.draft.clone()).ok()?,
        arguments_digest: digest.try_into().ok()?,
    })
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

impl CapnpBrokerState {
    pub(super) fn tracker_runs(&self, room: &str) -> Option<&Arc<dyn TrackerRuns>> {
        self.services
            .tracker_runs
            .as_ref()
            .filter(|service| service.requires_room(room))
    }

    /// The tracker's current record of `run`, a run of one of its issues.
    ///
    /// # Errors
    /// The tracker cannot be read, or holds no such run any more.
    pub(super) fn tracker_run_of(&self, run: &PendingTaskRun) -> Result<TrackerRun, String> {
        let binding = &run.native;
        let runs = self
            .tracker_runs(&run.cluster_id)
            .ok_or("the tracker is unavailable")?
            .runs(&run.cluster_id)?;
        run_of(&runs, binding)
            .cloned()
            .ok_or_else(|| "the issue's run was replaced or the issue deleted".into())
    }

    /// Deliver every going run of the subscribed rooms' trackers to its
    /// agent, and tell its person what it asks.
    pub(super) fn refresh_tracker_runs(
        self: &Arc<Self>,
        rooms: &std::collections::BTreeSet<String>,
    ) {
        for room in rooms {
            let Some(service) = self.tracker_runs(room) else {
                continue;
            };
            let Ok(runs) = service.runs(room) else {
                continue;
            };
            self.notify_run_questions(room, &runs);
            for run in runs.iter().filter(|run| run.active) {
                if let Err(error) = self.dispatch_tracker_run(room, run) {
                    tracing::debug!(%error, issue = %run.issue, "approved issue run not delivered");
                }
            }
        }
    }

    fn dispatch_tracker_run(&self, room: &str, run: &TrackerRun) -> Result<u64, String> {
        let subscriber = self
            .task_subscribers
            .lock()
            .map_err(|_| "task subscribers unavailable")?
            .iter()
            .filter(|(_, subscriber)| {
                subscriber.cluster_id == room
                    && subscriber.agent_id == run.agent
                    && !subscriber.tx.is_closed()
            })
            .min_by_key(|(peer, _)| **peer)
            .map(|(peer, subscriber)| (*peer, subscriber.tx.clone()))
            .ok_or("assigned agent is offline")?;
        let digest = blake3::hash(
            format!("tracker-run-v1\0{room}\0{}\0{}", run.issue, run.epoch).as_bytes(),
        );
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
        let context = tilo_actions::ActionContext::default()
            .with_purpose(run.title.clone())
            .with_group(
                runtime_collab::task_run_group(&run.issue, run_id),
                run.title.clone(),
                "task",
            );
        self.offer_native_run(
            room,
            NativeTaskBinding {
                task_id: run.issue.clone(),
                agent_id: run.agent.clone(),
                epoch: run.epoch,
                revision: 0,
                starter: Some(run.starter.clone()),
            },
            TaskRunEnvelope {
                paused: paused_step(run),
                task_id: run.issue.clone(),
                title: run.title.clone(),
                instruction: Some(run.instruction.clone()),
                subject: run.subject.clone(),
                run_id,
                lease,
                planning: runtime_collab::TaskRunPlanning::Model,
                execution_budget: run.budget,
                action_context: context,
                model: run.model.clone(),
            },
            subscriber,
        )
    }

    /// What an agent polling `run` is told: cancelled once the tracker no
    /// longer holds it as going, else the messages to it it has not taken
    /// in.
    ///
    /// # Errors
    /// The tracker cannot be read, the run was replaced, or the lease claims
    /// messages it never took in.
    pub(super) fn tracker_status(
        &self,
        run: &PendingTaskRun,
        lease: &str,
    ) -> Result<runtime_collab::TaskRunStatus, String> {
        let binding = &run.native;
        let (_, revision) =
            runtime_collab::native_task_revision(lease).ok_or("invalid native task lease")?;
        let current = self.tracker_run_of(run)?;
        if current.epoch != binding.epoch {
            return Err("the issue's run was replaced".into());
        }
        self.tracker_runs(&run.cluster_id)
            .ok_or("the tracker is unavailable")?
            .check_agent(&run.cluster_id, &current.agent)?;
        if !current.active {
            self.cancel_native_children(run)?;
            return Ok(runtime_collab::TaskRunStatus {
                cancelled: true,
                control: None,
            });
        }
        Ok(runtime_collab::TaskRunStatus {
            cancelled: false,
            control: Some(control_of(&current, revision)?),
        })
    }

    /// An agent's report on its run of an issue. The tracker is told by the
    /// agent itself, through the tracker's actions; here a run that ends
    /// stops its children and goes to history, as one run to undo whole.
    ///
    /// # Errors
    /// The run's children cannot be stopped.
    pub(super) fn report_tracker_run(
        &self,
        run: &PendingTaskRun,
        update: &runtime_collab::TaskRunUpdate,
    ) -> Result<(), String> {
        let outcome = match update {
            runtime_collab::TaskRunUpdate::Succeeded { .. } => "completed",
            runtime_collab::TaskRunUpdate::Failed { .. } => "stopped",
            runtime_collab::TaskRunUpdate::Cancelled { .. } => "cancelled",
            _ => return Ok(()),
        };
        if outcome != "completed" {
            self.cancel_native_children(run)?;
        }
        let ended = self.tracker_run_of(run).ok();
        let group = run.action_context.group.as_ref();
        self.ended_runs.tasks.push(crate::task_runs::EndedTaskRun {
            room: run.cluster_id.clone(),
            group: group.map_or_else(|| run.task_id.clone(), |group| group.id.clone()),
            title: group.map_or_else(|| run.task_id.clone(), |group| group.label.clone()),
            agent: format!("agent:{}", run.agent_id),
            person: run.native.starter.clone().unwrap_or_default(),
            instruction: ended.map(|ended| ended.instruction),
            outcome: outcome.to_owned(),
        });
        Ok(())
    }

    /// Admit `run`'s child under `requirement`: the run still going, every
    /// message to it taken in (`binding.revision`), and the agent allowed it
    /// for the run's starter.
    ///
    /// # Errors
    /// As [`TrackerRuns::authorize_action`], or the run has unread messages.
    pub(super) fn admit_tracker_action(
        &self,
        run: &PendingTaskRun,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
        action: &mut dyn FnMut() -> Result<StagedJob, String>,
    ) -> Result<StagedJob, String> {
        let current = self.tracker_run_of(run)?;
        if current.applied_revision() != binding.revision
            || current.latest_revision() != binding.revision
        {
            return Err("read the run's new messages before acting".into());
        }
        self.tracker_runs(&run.cluster_id)
            .ok_or("the tracker is unavailable")?
            .authorize_action(&run.cluster_id, &current, requirement)?;
        action()
    }

    /// The person's answer to a child of `run` that asks first, as
    /// [`native_confirmation_for`](Self::native_confirmation_for) decides it
    /// for a room task.
    ///
    /// # Errors
    /// The child waits for the person (ask on the issue), the person
    /// declined it, or the run or action can no longer be read.
    pub(super) fn tracker_confirmation(
        &self,
        run: &PendingTaskRun,
        lease: &str,
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
        let mut current_binding = binding;
        current_binding.revision = runtime_collab::native_task_revision(lease)
            .ok_or("invalid native lease")?
            .1;
        let requirement = NativeTaskActionRequirement {
            binding: current_binding,
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

    /// Authorize `requirement` for `run`'s agent on its issue's tracker.
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
    ///
    /// # Errors
    /// As the run's admission.
    pub(super) fn admit_task_launch(
        &self,
        run: &PendingTaskRun,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
        launch: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let current = self.tracker_run_of(run)?;
        if current.applied_revision() != binding.revision
            || current.latest_revision() != binding.revision
        {
            return Err("read the run's new messages before acting".into());
        }
        self.tracker_runs(&run.cluster_id)
            .ok_or("the tracker is unavailable")?
            .authorize_action(&run.cluster_id, &current, requirement)?;
        launch()
    }

    /// The revision of steering `run`'s agent has applied.
    ///
    /// # Errors
    /// The run ended or cannot be read.
    pub(super) fn applied_task_revision(&self, run: &PendingTaskRun) -> Result<u64, String> {
        Ok(self.tracker_run_of(run)?.applied_revision())
    }

    /// Whether agents in `room` act only through their runs' admission: a
    /// room whose tracker the host keeps.
    pub(super) fn task_admitted_room(&self, room: &str) -> bool {
        self.tracker_runs(room).is_some()
    }
}
