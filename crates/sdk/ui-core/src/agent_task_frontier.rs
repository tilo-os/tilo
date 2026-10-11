//! Authenticated Task Center read/selection/steering for agent controls.
//!
//! The model only derives a steerable task from owner-published `read-task`
//! results. Agent labels, presence text, cursors and elapsed time are never
//! treated as task identity or progress.

use std::collections::BTreeMap;

use serde::Deserialize;
use tilo_actions::remote::{
    CatalogueQuery, InvokeArgument, RemoteActionTarget, workspace_provider,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, InvocationUpdate};

use crate::LocalState;
use crate::remote_actions::{CatalogueCall, InvocationCall, RemoteActions, RemoteCall};
use crate::time::{Duration, Instant};
use crate::widget::Collaborative;

const TASK_RESOURCE_PREFIX: &str = "desktop:task:";
const TASK_COLLECTION: &str = "desktop:tasks";
const READ_CURRENT_TASKS_ACTION: &str = "read-current-tasks";
const READ_TASK_ACTION: &str = "read-task";
const DIRECT_TASK_ACTION: &str = "direct";
const QUEUE_TASK_ACTION: &str = "queue";
const STOP_TASK_ACTION: &str = "stop";
const TASK_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const ASYNC_CONTROL_POLL: Duration = Duration::from_millis(50);
const CONTROL_RESPONSE_WINDOW: Duration = Duration::from_secs(5);

pub use tilo_desktop_model::{WorkspaceTaskExecution, WorkspaceTaskExecutionPhase};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentTaskFrontier {
    pub task_id: String,
    pub title: String,
    pub run_epoch: u64,
    pub after_revision: u64,
    pub applied_revision: u64,
    pub route: String,
    pub cancellation_requested: bool,
    /// Owner-authenticated public milestone for this exact run. Absence is
    /// unknown, never evidence that a created lease is executing.
    pub execution: Option<tilo_desktop_model::WorkspaceTaskExecution>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentPendingTask {
    pub task_id: String,
    pub title: String,
    pub status: String,
    pub route: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AgentTaskChoice {
    #[default]
    None,
    One(AgentTaskFrontier),
    Choose(Vec<AgentTaskFrontier>),
    Selected(AgentTaskFrontier),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AgentDirectionState {
    #[default]
    Draft,
    Submitting,
    Uncertain {
        task_id: String,
        run_epoch: u64,
    },
    Saved {
        task_id: String,
        run_epoch: u64,
        revision: u64,
    },
    Applied {
        task_id: String,
        run_epoch: u64,
        revision: u64,
    },
    NeedsRefresh(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AgentTaskDiscovery {
    #[default]
    Loading,
    Ready,
    Incomplete {
        total: u64,
    },
    Unavailable,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AgentQueueState {
    #[default]
    Ready,
    Submitting,
    Uncertain,
    Queued {
        task_id: String,
    },
    Failed,
}

/// The exact Queue affordance painted for one authenticated reader context.
/// This local gesture witness grants no authority; invocation still goes
/// through the ordinary broker and owner admission checks.
pub struct AgentQueueOffer {
    identity: QueueOfferIdentity,
    expires_at: Instant,
}

#[derive(Clone, PartialEq, Eq)]
struct QueueOfferIdentity {
    reader: InvocationId,
    epoch: u64,
    workspace: String,
    agent: String,
    generation: u64,
    observed: u64,
    revision: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AgentStopState {
    #[default]
    Ready,
    Submitting {
        task_id: String,
        run_epoch: u64,
    },
    Uncertain {
        task_id: String,
        run_epoch: u64,
    },
    Stopping {
        task_id: String,
        run_epoch: u64,
    },
    Failed {
        task_id: String,
        run_epoch: u64,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskRead {
    generation: u64,
    task_id: String,
    title: String,
    assigned_agent: Option<String>,
    route: String,
    status: String,
    run_epoch: Option<u64>,
    active_agent: Option<String>,
    after_revision: u64,
    applied_revision: Option<u64>,
    cancellation_requested: bool,
    #[serde(default)]
    execution: Option<tilo_desktop_model::WorkspaceTaskExecution>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentTasksRead {
    generation: u64,
    complete: bool,
    total: u64,
    tasks: Vec<CurrentTaskRead>,
    pending_complete: bool,
    pending_total: u64,
    pending: Vec<PendingTaskRead>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentTaskRead {
    task_id: String,
    title: String,
    assigned_agent: String,
    status: String,
    run_epoch: u64,
    active_agent: String,
    after_revision: u64,
    applied_revision: u64,
    route: String,
    cancellation_requested: bool,
    #[serde(default)]
    execution: Option<tilo_desktop_model::WorkspaceTaskExecution>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingTaskRead {
    task_id: String,
    title: String,
    assigned_agent: String,
    status: String,
    route: String,
}

struct DirectionCall {
    invocation: InvocationId,
    call: LocalState<InvocationCall>,
    frontier: AgentTaskFrontier,
}

struct QueueCall {
    invocation: InvocationId,
    call: LocalState<InvocationCall>,
}

struct StopCall {
    invocation: InvocationId,
    call: LocalState<InvocationCall>,
    frontier: AgentTaskFrontier,
}

struct SavedDirection {
    task_id: String,
    run_epoch: u64,
    revision: u64,
    read: Option<LocalState<InvocationCall>>,
    read_issued: u64,
}

struct ConfirmedFrontier {
    frontier: AgentTaskFrontier,
    generation: u64,
    observed: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SteerDraftWitness {
    task_id: String,
    run_epoch: u64,
    after_revision: u64,
    route: String,
}

impl From<AgentTaskFrontier> for SteerDraftWitness {
    fn from(frontier: AgentTaskFrontier) -> Self {
        Self {
            task_id: frontier.task_id,
            run_epoch: frontier.run_epoch,
            after_revision: frontier.after_revision,
            route: frontier.route,
        }
    }
}

/// One shared Task Center reader for Chat and marker-adjacent controls.
pub struct AgentTaskFrontiers {
    remote: RemoteActions,
    observation_active: bool,
    workspace: String,
    agent: String,
    current: Option<LocalState<InvocationCall>>,
    /// The current caller's offered actions, not permissions inferred from
    /// task visibility. Refreshed on the same authenticated transport.
    controls: BTreeMap<(String, String), LocalState<CatalogueCall>>,
    control_response_until: Option<Instant>,
    queue_draft_revalidating: bool,
    queue_offer_reader: InvocationId,
    queue_offer_epoch: u64,
    queue_offer_revalidating: Option<QueueOfferIdentity>,
    steer_draft_revalidating: Option<SteerDraftWitness>,
    draft_revalidation_until: Option<Instant>,
    current_consumed: bool,
    current_issued: u64,
    current_accepts_frontier_change: bool,
    next_observation: u64,
    next_refresh: Option<Instant>,
    snapshot_generation: u64,
    /// Local issue order for the last accepted collection snapshot. Owner
    /// generation wins first; this only orders observations within one
    /// generation.
    snapshot_observed: u64,
    snapshot_complete: bool,
    /// Newer exact-task reads replace the immutable collection snapshot.
    confirmed: BTreeMap<String, ConfirmedFrontier>,
    frontiers: Vec<AgentTaskFrontier>,
    pending_tasks: Vec<AgentPendingTask>,
    pending_complete: bool,
    selected: Option<String>,
    /// Last exact owner-backed task shown in the steering context. Retained
    /// across an invalidation so the UI can say which task changed without
    /// continuing to treat it as steerable.
    context: Option<AgentTaskFrontier>,
    direction: Option<DirectionCall>,
    saved_direction: Option<SavedDirection>,
    direction_state: AgentDirectionState,
    queue: Option<QueueCall>,
    queue_state: AgentQueueState,
    stop: Option<StopCall>,
    stop_state: AgentStopState,
    discovery: AgentTaskDiscovery,
    invalidated: bool,
    stale_frontier: bool,
}

impl Default for AgentTaskFrontiers {
    fn default() -> Self {
        Self::with_actions(RemoteActions::current())
    }
}

impl AgentTaskFrontiers {
    /// Build a frontier reader on an app's existing authenticated action
    /// transport. This keeps queries and receipts on the same pumped ledger;
    /// it does not create or infer a caller identity.
    #[must_use]
    pub fn with_actions(remote: RemoteActions) -> Self {
        Self {
            remote,
            observation_active: true,
            workspace: String::new(),
            agent: String::new(),
            current: None,
            controls: BTreeMap::new(),
            control_response_until: None,
            queue_draft_revalidating: false,
            queue_offer_reader: InvocationId::new(),
            queue_offer_epoch: 0,
            queue_offer_revalidating: None,
            steer_draft_revalidating: None,
            draft_revalidation_until: None,
            current_consumed: false,
            current_issued: 0,
            current_accepts_frontier_change: false,
            next_observation: 1,
            next_refresh: None,
            snapshot_generation: 0,
            snapshot_observed: 0,
            snapshot_complete: false,
            confirmed: BTreeMap::new(),
            frontiers: Vec::new(),
            pending_tasks: Vec::new(),
            pending_complete: false,
            selected: None,
            context: None,
            direction: None,
            saved_direction: None,
            direction_state: AgentDirectionState::Draft,
            queue: None,
            queue_state: AgentQueueState::Ready,
            stop: None,
            stop_state: AgentStopState::Ready,
            discovery: AgentTaskDiscovery::Loading,
            invalidated: false,
            stale_frontier: false,
        }
    }

    pub fn observe(&mut self, workspace: &str, agent: &str) {
        if self.workspace == workspace && self.agent == agent {
            self.poll();
            return;
        }
        workspace.clone_into(&mut self.workspace);
        agent.clone_into(&mut self.agent);
        self.queue_offer_epoch = self.queue_offer_epoch.saturating_add(1);
        self.queue_offer_revalidating = None;
        self.current = None;
        self.controls.clear();
        self.control_response_until = None;
        self.queue_draft_revalidating = false;
        self.steer_draft_revalidating = None;
        self.draft_revalidation_until = None;
        self.current_consumed = true;
        self.current_issued = 0;
        self.current_accepts_frontier_change = false;
        self.next_observation = 1;
        self.next_refresh = None;
        self.snapshot_generation = 0;
        self.snapshot_observed = 0;
        self.snapshot_complete = false;
        self.confirmed.clear();
        self.frontiers.clear();
        self.pending_tasks.clear();
        self.pending_complete = false;
        self.selected = None;
        self.context = None;
        self.direction = None;
        self.saved_direction = None;
        self.direction_state = AgentDirectionState::Draft;
        self.queue = None;
        self.queue_state = AgentQueueState::Ready;
        self.stop = None;
        self.stop_state = AgentStopState::Ready;
        self.discovery = AgentTaskDiscovery::Loading;
        self.invalidated = false;
        self.stale_frontier = false;
        self.start_current_refresh(false);
        self.poll();
    }

    /// Suspend collection reads when no presentation needs this context.
    /// Exact submitted mutations and their receipts remain retained and are
    /// settled normally. Resuming requires a fresh owner read, without silently
    /// accepting a changed steering frontier or clearing the caller's draft.
    pub fn set_observation_active(&mut self, active: bool) {
        if self.observation_active == active {
            return;
        }
        self.observation_active = active;
        self.queue_offer_epoch = self.queue_offer_epoch.saturating_add(1);
        self.queue_offer_revalidating = None;
        self.current = None;
        self.current_consumed = true;
        self.controls.clear();
        self.control_response_until = None;
        self.queue_draft_revalidating = false;
        self.steer_draft_revalidating = None;
        self.draft_revalidation_until = None;
        self.next_refresh = None;
        self.snapshot_complete = false;
        if active {
            self.discovery = AgentTaskDiscovery::Loading;
            self.start_current_refresh(false);
        }
    }

    /// Explicitly acknowledge a changed owner frontier and re-read it.
    ///
    /// Private draft text is owned by the caller and is never cleared here.
    /// An automatic refresh never retargets that draft to a replacement run
    /// or revision; this explicit operation permits the next complete owner
    /// snapshot to become its new steering context.
    pub fn refresh(&mut self) {
        if !self.observation_active || self.workspace.is_empty() || self.agent.is_empty() {
            return;
        }
        if matches!(self.direction_state, AgentDirectionState::Saved { .. }) {
            let _ = self.refresh_direction_status();
        }
        if self.current_consumed {
            self.start_current_refresh(true);
        } else {
            self.current_accepts_frontier_change = true;
        }
        self.poll();
    }

    /// Next time the owner collection should be re-read.
    ///
    /// A retained widget forwards this through its normal wake-deadline hook;
    /// no redraw spin or private polling thread is needed.
    #[must_use]
    pub fn next_refresh_deadline(&self) -> Option<Instant> {
        self.next_refresh
    }

    /// Earliest native pump needed for either an in-flight action response or
    /// the next owner refresh. Unlike callback wake handles, this also works
    /// in the production compositor's deadline-driven loop.
    #[must_use]
    pub fn next_wake_deadline(&self) -> Option<Instant> {
        if !self.observation_active {
            return (self.direction.is_some()
                || self.queue.is_some()
                || self.stop.is_some()
                || self
                    .saved_direction
                    .as_ref()
                    .is_some_and(|saved| saved.read.is_some()))
            .then(|| self.remote.next_observation_deadline())
            .flatten();
        }
        let now = crate::background::Background::current().now();
        let control = self
            .controls
            .values()
            .any(|call| call.get().is_pending())
            .then(|| {
                self.control_response_until
                    .map(|until| (now + ASYNC_CONTROL_POLL).min(until))
            })
            .flatten();
        [
            self.next_refresh,
            self.remote.next_observation_deadline(),
            control,
            self.draft_revalidation_until,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Whether the last selected context changed and needs explicit
    /// confirmation before its retained draft can be steered.
    #[must_use]
    pub const fn needs_frontier_confirmation(&self) -> bool {
        self.stale_frontier
    }

    #[must_use]
    pub fn choice(&self) -> AgentTaskChoice {
        if !self.snapshot_complete || self.invalidated || self.stale_frontier {
            return AgentTaskChoice::None;
        }
        match self.frontiers.as_slice() {
            [] => AgentTaskChoice::None,
            [only] => AgentTaskChoice::One(only.clone()),
            many => self
                .selected
                .as_ref()
                .and_then(|task| many.iter().find(|frontier| &frontier.task_id == task))
                .cloned()
                .map_or_else(
                    || AgentTaskChoice::Choose(many.to_vec()),
                    AgentTaskChoice::Selected,
                ),
        }
    }

    /// Every task in the complete authenticated owner snapshot.
    ///
    /// Callers receive no partial list while a snapshot is loading,
    /// incomplete or invalidated; presenting that as the available choices
    /// would make a bounded response look authoritative.
    #[must_use]
    pub fn available_tasks(&self) -> Vec<AgentTaskFrontier> {
        if self.snapshot_complete && !self.invalidated {
            self.frontiers.clone()
        } else {
            Vec::new()
        }
    }

    /// Pending work from a complete owner projection. An incomplete bounded
    /// response is never presented as the whole queue.
    #[must_use]
    pub fn pending_tasks(&self) -> Vec<AgentPendingTask> {
        if self.snapshot_complete && !self.invalidated && self.pending_complete {
            self.pending_tasks.clone()
        } else {
            Vec::new()
        }
    }

    #[must_use]
    pub const fn pending_tasks_complete(&self) -> bool {
        self.snapshot_complete && !self.invalidated && self.pending_complete
    }

    #[must_use]
    pub fn queue_state(&self) -> &AgentQueueState {
        &self.queue_state
    }

    #[must_use]
    pub fn stop_state(&self) -> &AgentStopState {
        &self.stop_state
    }

    #[must_use]
    pub fn direction_state(&self) -> &AgentDirectionState {
        &self.direction_state
    }

    /// The exact task whose context should be presented. A stale retained
    /// value remains descriptive only; [`Self::can_steer`] is the authority
    /// for whether a new direction may be submitted.
    #[must_use]
    pub fn context_task(&self) -> Option<AgentTaskFrontier> {
        match self.choice() {
            AgentTaskChoice::One(task) | AgentTaskChoice::Selected(task) => Some(task),
            AgentTaskChoice::None | AgentTaskChoice::Choose(_) => self.context.clone(),
        }
    }

    /// Whether the current complete owner snapshot names exactly one selected
    /// run, the caller is offered its exact Direct action, and no direction
    /// is already awaiting its receipt. Invocation still rechecks authority.
    #[must_use]
    pub fn can_steer(&self) -> bool {
        matches!(self.actionable_frontier(), Some(frontier)
            if frontier.route == "model" && !frontier.cancellation_requested
                && self.offers(&task_target(&self.workspace, &frontier.task_id, DIRECT_TASK_ACTION)))
            && !matches!(
                self.direction_state,
                AgentDirectionState::Submitting | AgentDirectionState::Uncertain { .. }
            )
    }

    /// An affordance only: the owner rechecks authority when invoked.
    #[must_use]
    pub fn can_queue(&self) -> bool {
        self.snapshot_complete
            && !self.invalidated
            && !self.agent.is_empty()
            && self.queue.is_none()
            && self.offers(&task_collection_target(&self.workspace, QUEUE_TASK_ACTION))
    }

    /// Capture only when painting an enabled Queue control. Semantic action
    /// publication continues to use [`Self::can_queue`].
    #[must_use]
    pub fn queue_offer(&self) -> Option<AgentQueueOffer> {
        Some(AgentQueueOffer {
            identity: self.queue_offer_identity()?,
            expires_at: crate::background::Background::current()
                .now()
                .checked_add(CONTROL_RESPONSE_WINDOW)?,
        })
    }

    fn queue_offer_identity(&self) -> Option<QueueOfferIdentity> {
        if !self.observation_active || !self.can_queue() {
            return None;
        }
        let target = task_collection_target(&self.workspace, QUEUE_TASK_ACTION);
        let RemoteCall::Done(page) = self
            .controls
            .get(&(target.address.resource, target.address.action))?
            .get()
        else {
            return None;
        };
        Some(QueueOfferIdentity {
            reader: self.queue_offer_reader,
            epoch: self.queue_offer_epoch,
            workspace: self.workspace.clone(),
            agent: self.agent.clone(),
            generation: self.snapshot_generation,
            observed: self.snapshot_observed,
            revision: page.revision,
        })
    }

    /// Whether a previously-authorized Queue draft may keep accepting local
    /// editor input while its exact action offer is being revalidated.
    /// Semantic submission continues to require [`Self::can_queue`]; an
    /// already-painted gesture can use its exact [`AgentQueueOffer`].
    #[must_use]
    pub fn can_edit_queue_draft(&self) -> bool {
        self.can_queue() || self.queue_draft_revalidating
    }

    /// Whether a previously-authorized Steer draft may keep accepting local
    /// editor input while its exact action offer is being revalidated.
    /// Submission continues to require [`Self::can_steer`].
    #[must_use]
    pub fn can_edit_steer_draft(&self) -> bool {
        self.can_steer() || self.steer_draft_revalidating.is_some()
    }

    #[must_use]
    pub fn can_stop(&self) -> bool {
        self.stop.is_none()
            && self.actionable_frontier().is_some_and(|frontier| {
                matches!(frontier.route.as_str(), "model" | "rules_only")
                    && !frontier.cancellation_requested
                    && self.offers(&task_target(
                        &self.workspace,
                        &frontier.task_id,
                        STOP_TASK_ACTION,
                    ))
            })
    }

    fn offers(&self, target: &RemoteActionTarget) -> bool {
        let key = (
            target.address.resource.clone(),
            target.address.action.clone(),
        );
        let Some(RemoteCall::Done(page)) = self.controls.get(&key).map(LocalState::get) else {
            return false;
        };
        !page.revision.is_empty()
            && page.exhausted
            && page.next_cursor.is_empty()
            && !page.stale_cursor
            && !page.budget_exhausted
            && page.rows.len() == 1
            && page.rows.iter().any(|row| {
                row.resource.id == target.address.resource && row.target().as_ref() == Some(target)
            })
    }

    #[must_use]
    pub fn discovery(&self) -> &AgentTaskDiscovery {
        &self.discovery
    }

    pub fn select(&mut self, task_id: &str) -> Result<(), String> {
        if self.frontiers.iter().any(|task| task.task_id == task_id) {
            self.selected = Some(task_id.to_owned());
            self.context = self
                .frontiers
                .iter()
                .find(|task| task.task_id == task_id)
                .cloned();
            self.invalidated = false;
            self.stale_frontier = false;
            self.direction_state = AgentDirectionState::Draft;
            self.controls.clear();
            self.control_response_until = None;
            self.queue_draft_revalidating = false;
            self.queue_offer_epoch = self.queue_offer_epoch.saturating_add(1);
            self.queue_offer_revalidating = None;
            self.steer_draft_revalidating = None;
            self.draft_revalidation_until = None;
            self.refresh_control_offers();
            Ok(())
        } else {
            Err("select a current task returned by Task Center".into())
        }
    }

    /// Select only from the authenticated owner snapshot for this exact
    /// conversation. A task id supplied by a View action is descriptive and
    /// cannot retarget a frontier reader to another room or agent.
    pub fn select_for(
        &mut self,
        workspace: &str,
        agent: &str,
        task_id: &str,
    ) -> Result<(), String> {
        self.poll();
        if self.workspace != workspace || self.agent != agent {
            return Err("refresh the current tasks for this agent".into());
        }
        self.select(task_id)
    }

    /// Confirm the exact run/revision the person was offered, never a newer
    /// run occupying the same task id after an asynchronous refresh.
    pub fn select_frontier_for(
        &mut self,
        workspace: &str,
        agent: &str,
        displayed: &AgentTaskFrontier,
    ) -> Result<(), String> {
        self.poll();
        if self.workspace != workspace
            || self.agent != agent
            || !self.available_tasks().iter().any(|current| {
                current.task_id == displayed.task_id
                    && current.run_epoch == displayed.run_epoch
                    && current.after_revision == displayed.after_revision
            })
        {
            return Err("refresh the current task before selecting it".into());
        }
        self.select(&displayed.task_id)
    }

    pub fn steer(&mut self, text: &str) -> Result<InvocationId, String> {
        self.poll();
        if text.trim().is_empty() {
            return Err("direction text is blank".into());
        }
        if self.direction.is_some() {
            return Err("a direction is already being submitted".into());
        }
        let frontier = match self.choice() {
            AgentTaskChoice::One(frontier) | AgentTaskChoice::Selected(frontier) => frontier,
            AgentTaskChoice::Choose(_) => return Err("select which current task to steer".into()),
            AgentTaskChoice::None => return Err("this agent has no current task to steer".into()),
        };
        if frontier.route != "model" {
            return Err("this task route does not accept model directions".into());
        }
        if frontier.cancellation_requested {
            return Err("the task cancellation state is unavailable or stopping".into());
        }
        if !self.can_steer() {
            return Err("the current caller is not offered task steering".into());
        }
        let invocation = InvocationId::new();
        let target = task_target(&self.workspace, &frontier.task_id, DIRECT_TASK_ACTION);
        let argument = InvokeArgument::Json(serde_json::Map::from_iter([
            ("run_epoch".into(), serde_json::json!(frontier.run_epoch)),
            (
                "after_revision".into(),
                serde_json::json!(frontier.after_revision),
            ),
            ("text".into(), serde_json::json!(text)),
        ]));
        let call = self
            .remote
            .invoke_with_id(invocation, target, argument, None);
        self.direction = Some(DirectionCall {
            invocation,
            call,
            frontier,
        });
        self.direction_state = AgentDirectionState::Submitting;
        Ok(invocation)
    }

    /// Queue a new model-assisted task through the canonical Task Center
    /// collection. The full instruction is sent separately from its bounded
    /// display title; neither is written into Chat's CRDT.
    pub fn queue(
        &mut self,
        instruction: &str,
        title: Option<&str>,
    ) -> Result<InvocationId, String> {
        self.queue_with_admission(instruction, title, self.can_queue())
    }

    /// Consume one painted Queue gesture during its exact pending owner
    /// revalidation. A settled but unpolled read is not pending evidence.
    pub fn queue_with_offer(
        &mut self,
        offer: AgentQueueOffer,
        instruction: &str,
        title: Option<&str>,
    ) -> Result<InvocationId, String> {
        let now = crate::background::Background::current().now();
        let current = self.queue_offer_identity().as_ref() == Some(&offer.identity);
        let pending = self.observation_active
            && self.snapshot_complete
            && !self.invalidated
            && self.queue.is_none()
            && !self.current_consumed
            && offer.identity.reader == self.queue_offer_reader
            && offer.identity.epoch == self.queue_offer_epoch
            && offer.identity.workspace == self.workspace
            && offer.identity.agent == self.agent
            && (offer.identity.generation, offer.identity.observed)
                == (self.snapshot_generation, self.snapshot_observed)
            && self.queue_offer_revalidating.as_ref() == Some(&offer.identity)
            && self
                .draft_revalidation_until
                .is_some_and(|until| now < until)
            && self
                .current
                .as_ref()
                .is_some_and(|call| matches!(call.get(), RemoteCall::Pending));
        self.queue_with_admission(
            instruction,
            title,
            current || (now < offer.expires_at && pending),
        )
    }

    fn queue_with_admission(
        &mut self,
        instruction: &str,
        title: Option<&str>,
        offered: bool,
    ) -> Result<InvocationId, String> {
        if instruction.trim().is_empty() {
            return Err("task instruction is blank".into());
        }
        if self.workspace.is_empty() || self.agent.is_empty() {
            return Err("refresh tasks for an authenticated agent first".into());
        }
        if self.queue.is_some() {
            return Err("a task is already being queued".into());
        }
        if !offered {
            return Err("the current caller is not offered task queueing".into());
        }
        // Invalidate every token minted from this offer before dispatch,
        // including when the transport settles the receipt synchronously.
        self.queue_offer_epoch = self.queue_offer_epoch.saturating_add(1);
        self.queue_offer_revalidating = None;
        self.queue_draft_revalidating = false;
        if self.steer_draft_revalidating.is_none() {
            self.draft_revalidation_until = None;
        }
        // The enabled Queue control is the caller's exact, already-observed
        // offer. Starting a periodic refresh here would clear that offer
        // before dispatch; the owner and broker still recheck authority for
        // the invocation below.
        let invocation = InvocationId::new();
        let mut argument = serde_json::Map::from_iter([
            ("instruction".into(), serde_json::json!(instruction)),
            ("agent".into(), serde_json::json!(self.agent)),
            ("route".into(), serde_json::json!("model")),
        ]);
        if let Some(title) = title.filter(|title| !title.trim().is_empty()) {
            argument.insert("title".into(), serde_json::json!(title));
        }
        let call = self.remote.invoke_with_id(
            invocation,
            task_collection_target(&self.workspace, QUEUE_TASK_ACTION),
            InvokeArgument::Json(argument),
            None,
        );
        let settled = !matches!(call.get(), RemoteCall::Pending);
        self.queue = Some(QueueCall { invocation, call });
        self.queue_state = AgentQueueState::Submitting;
        if settled {
            self.poll_queue();
        }
        Ok(invocation)
    }

    /// Request cancellation of the exact owner-published run. Success means
    /// only that cancellation was durably requested; terminal state remains
    /// owner/Agent reported.
    pub fn stop(&mut self) -> Result<InvocationId, String> {
        self.poll();
        if self.stop.is_some() {
            return Err("a stop request is already being submitted".into());
        }
        let frontier = self
            .actionable_frontier()
            .ok_or("select one current task to stop")?;
        if !matches!(frontier.route.as_str(), "model" | "rules_only") {
            return Err("the task route is unavailable or not Agent-owned".into());
        }
        if frontier.cancellation_requested {
            return Err("this task is already stopping or its state is unavailable".into());
        }
        if !self.can_stop() {
            return Err("the current caller is not offered task stopping".into());
        }
        let invocation = InvocationId::new();
        let call = self.remote.invoke_with_id(
            invocation,
            task_target(&self.workspace, &frontier.task_id, STOP_TASK_ACTION),
            InvokeArgument::Json(serde_json::Map::from_iter([
                ("run_epoch".into(), serde_json::json!(frontier.run_epoch)),
                (
                    "after_revision".into(),
                    serde_json::json!(frontier.after_revision),
                ),
            ])),
            None,
        );
        self.stop_state = AgentStopState::Submitting {
            task_id: frontier.task_id.clone(),
            run_epoch: frontier.run_epoch,
        };
        self.stop = Some(StopCall {
            invocation,
            call,
            frontier,
        });
        Ok(invocation)
    }

    fn actionable_frontier(&self) -> Option<AgentTaskFrontier> {
        match self.choice() {
            AgentTaskChoice::One(frontier) | AgentTaskChoice::Selected(frontier) => Some(frontier),
            AgentTaskChoice::None | AgentTaskChoice::Choose(_) => None,
        }
    }

    /// Re-read the exact task after a saved direction so an agent's later,
    /// lease-bound acknowledgement can advance `Saved` to `Applied`.
    pub fn refresh_direction_status(&mut self) -> Result<(), String> {
        let saved = self
            .saved_direction
            .as_mut()
            .ok_or("there is no saved direction to refresh")?;
        if saved.read.is_none() {
            saved.read = Some(self.remote.invoke(
                task_target(&self.workspace, &saved.task_id, READ_TASK_ACTION),
                InvokeArgument::None,
                None,
            ));
            saved.read_issued = self.next_observation;
            self.next_observation = self.next_observation.saturating_add(1);
        }
        Ok(())
    }

    fn poll(&mut self) {
        let now = crate::background::Background::current().now();
        self.poll_at(now);
    }

    fn poll_at(&mut self, now: Instant) {
        if self.observation_active {
            if self.current_consumed && self.next_refresh.is_some_and(|deadline| deadline <= now) {
                self.start_current_refresh(false);
            }
            self.collect_current_tasks(now);
            self.refresh_control_offers();
        }
        let controls_pending = self.controls.values().any(|call| call.get().is_pending());
        let controls_expired = self
            .control_response_until
            .is_some_and(|until| until <= now);
        if !controls_pending || controls_expired {
            self.control_response_until = None;
        }
        if controls_expired
            || self
                .draft_revalidation_until
                .is_some_and(|until| until <= now)
        {
            self.clear_draft_revalidation();
        }
        self.settle_draft_revalidation();
        self.poll_direction();
        self.poll_saved_direction();
        self.poll_queue();
        self.poll_stop();
    }

    fn refresh_control_offers(&mut self) {
        if self.current_consumed && self.snapshot_complete && !self.invalidated {
            let mut targets = vec![task_collection_target(&self.workspace, QUEUE_TASK_ACTION)];
            if let Some(frontier) = self.actionable_frontier() {
                targets.push(task_target(
                    &self.workspace,
                    &frontier.task_id,
                    DIRECT_TASK_ACTION,
                ));
                targets.push(task_target(
                    &self.workspace,
                    &frontier.task_id,
                    STOP_TASK_ACTION,
                ));
            }
            let mut inserted = false;
            for target in targets {
                let key = (
                    target.address.resource.clone(),
                    target.address.action.clone(),
                );
                self.controls.entry(key).or_insert_with(|| {
                    inserted = true;
                    self.remote.catalogue_page(CatalogueQuery {
                        provider: Some(target.provider),
                        target: Some(target.address),
                        max_results: 1,
                        ..CatalogueQuery::default()
                    })
                });
            }
            if inserted && self.control_response_until.is_none() {
                self.control_response_until = crate::background::Background::current()
                    .now()
                    .checked_add(CONTROL_RESPONSE_WINDOW);
            }
        }
    }

    fn start_current_refresh(&mut self, accepts_frontier_change: bool) {
        if !self.observation_active
            || self.workspace.is_empty()
            || self.agent.is_empty()
            || !self.current_consumed
        {
            return;
        }
        self.queue_draft_revalidating = self.can_queue();
        self.queue_offer_revalidating = self.queue_offer_identity();
        self.steer_draft_revalidating = if self.can_steer() {
            self.actionable_frontier().map(SteerDraftWitness::from)
        } else {
            None
        };
        self.draft_revalidation_until =
            if self.queue_draft_revalidating || self.steer_draft_revalidating.is_some() {
                crate::background::Background::current()
                    .now()
                    .checked_add(CONTROL_RESPONSE_WINDOW)
            } else {
                None
            };
        self.current_issued = self.issue_observation();
        // Discard old grants immediately while a fresh observation is pending.
        // Browse only this room's Task Center surface; labels never grant access.
        self.controls.clear();
        self.control_response_until = None;
        self.current_accepts_frontier_change = accepts_frontier_change;
        self.current = Some(self.remote.invoke(
            task_collection_target(&self.workspace, READ_CURRENT_TASKS_ACTION),
            InvokeArgument::Json(serde_json::Map::from_iter([(
                "agent".into(),
                serde_json::json!(self.agent),
            )])),
            None,
        ));
        self.current_consumed = false;
        self.next_refresh = None;
    }

    fn settle_draft_revalidation(&mut self) {
        if !self.current_consumed {
            return;
        }
        if !self.snapshot_complete || self.invalidated {
            self.clear_draft_revalidation();
            return;
        }
        if self.queue_draft_revalidating {
            let target = task_collection_target(&self.workspace, QUEUE_TASK_ACTION);
            let key = (target.address.resource, target.address.action);
            if self
                .controls
                .get(&key)
                .is_some_and(|call| !call.get().is_pending())
            {
                self.queue_draft_revalidating = false;
                self.queue_offer_revalidating = None;
            }
        }
        if let Some(witness) = self.steer_draft_revalidating.clone() {
            let Some(frontier) = self.actionable_frontier() else {
                self.steer_draft_revalidating = None;
                return;
            };
            let current = SteerDraftWitness::from(frontier.clone());
            if witness != current {
                self.steer_draft_revalidating = None;
                return;
            }
            let target = task_target(&self.workspace, &frontier.task_id, DIRECT_TASK_ACTION);
            let key = (target.address.resource, target.address.action);
            if self
                .controls
                .get(&key)
                .is_some_and(|call| !call.get().is_pending())
            {
                self.steer_draft_revalidating = None;
            }
        }
        if !self.queue_draft_revalidating && self.steer_draft_revalidating.is_none() {
            self.draft_revalidation_until = None;
        }
    }

    fn clear_draft_revalidation(&mut self) {
        self.queue_draft_revalidating = false;
        self.queue_offer_revalidating = None;
        self.steer_draft_revalidating = None;
        self.draft_revalidation_until = None;
    }

    fn issue_observation(&mut self) -> u64 {
        let issued = self.next_observation;
        self.next_observation = self.next_observation.saturating_add(1);
        issued
    }

    fn collect_current_tasks(&mut self, now: Instant) {
        if self.current_consumed {
            return;
        }
        let Some(call) = self.current.as_ref().map(LocalState::get) else {
            return;
        };
        let RemoteCall::Done(InvocationUpdate::Succeeded {
            output: ActionOutput {
                value: Some(value), ..
            },
            ..
        }) = call
        else {
            if call.is_settled() {
                self.current_consumed = true;
                self.next_refresh = Some(now + TASK_REFRESH_INTERVAL);
                self.invalidate_snapshot(AgentTaskDiscovery::Unavailable);
            }
            return;
        };
        self.current_consumed = true;
        self.next_refresh = Some(now + TASK_REFRESH_INTERVAL);
        let Ok(read) = serde_json::from_value::<CurrentTasksRead>(value) else {
            self.invalidate_snapshot(AgentTaskDiscovery::Unavailable);
            return;
        };
        let (Ok(total), Ok(pending_total)) = (
            usize::try_from(read.total),
            usize::try_from(read.pending_total),
        ) else {
            self.invalidate_snapshot(AgentTaskDiscovery::Unavailable);
            return;
        };
        if !read.complete {
            self.invalidate_snapshot(AgentTaskDiscovery::Incomplete { total: read.total });
            return;
        }
        if read.generation == 0
            || total != read.tasks.len()
            || (read.pending_complete && pending_total != read.pending.len())
            || (!read.pending_complete && read.pending.len() > pending_total)
        {
            self.invalidate_snapshot(AgentTaskDiscovery::Unavailable);
            return;
        }
        if (read.generation, self.current_issued)
            < (self.snapshot_generation, self.snapshot_observed)
        {
            return;
        }
        let mut pending_tasks = Vec::with_capacity(read.pending.len());
        for task in read.pending {
            if task.task_id.is_empty()
                || task.title.trim().is_empty()
                || task.assigned_agent != self.agent
                || !matches!(task.status.as_str(), "queued" | "waiting")
                || !valid_route(&task.route)
            {
                self.invalidate_snapshot(AgentTaskDiscovery::Unavailable);
                return;
            }
            pending_tasks.push(AgentPendingTask {
                task_id: task.task_id,
                title: task.title,
                status: task.status,
                route: task.route,
            });
        }
        // The owner supplies execution order. Opaque task IDs are identities,
        // not queue positions, so preserve the authoritative projection.
        let mut frontiers = Vec::with_capacity(read.tasks.len());
        for task in read.tasks {
            if task.task_id.is_empty()
                || task.title.trim().is_empty()
                || task.status != "in_progress"
                || task.assigned_agent != self.agent
                || task.active_agent != self.agent
                || task.run_epoch == 0
                || task.applied_revision > task.after_revision
                || !valid_route(&task.route)
                || !valid_execution(task.execution.as_ref(), task.run_epoch)
            {
                self.invalidate_snapshot(AgentTaskDiscovery::Unavailable);
                return;
            }
            frontiers.push(AgentTaskFrontier {
                task_id: task.task_id,
                title: task.title,
                run_epoch: task.run_epoch,
                after_revision: task.after_revision,
                applied_revision: task.applied_revision,
                route: task.route,
                cancellation_requested: task.cancellation_requested,
                execution: task.execution,
            });
        }
        let collection_order = (read.generation, self.current_issued);
        self.confirmed.retain(|_, confirmed| {
            // Owner generation is authoritative. The local observation order
            // only breaks a tie within that exact generation.
            (confirmed.generation, confirmed.observed) > collection_order
        });
        for confirmed in self.confirmed.values() {
            frontiers.retain(|frontier| frontier.task_id != confirmed.frontier.task_id);
            frontiers.push(confirmed.frontier.clone());
        }
        frontiers.sort_by(|left, right| {
            left.title
                .to_lowercase()
                .cmp(&right.title.to_lowercase())
                .then_with(|| left.task_id.cmp(&right.task_id))
        });
        frontiers.dedup_by(|left, right| left.task_id == right.task_id);
        self.confirm_saved_direction_from_collection(&frontiers);
        let prior_context = self.context.clone();
        let current_context = prior_context.as_ref().and_then(|prior| {
            frontiers
                .iter()
                .find(|frontier| frontier.task_id == prior.task_id)
        });
        let frontier_changed = prior_context.as_ref().is_some_and(|prior| {
            current_context.is_none_or(|current| {
                let expected_saved_ack = self.saved_direction.as_ref().is_some_and(|saved| {
                    let same_saved_run = (current.task_id.as_str(), current.run_epoch)
                        == (saved.task_id.as_str(), saved.run_epoch);
                    let same_original_run = (prior.task_id.as_str(), prior.run_epoch)
                        == (saved.task_id.as_str(), saved.run_epoch);
                    let follows_original_frontier =
                        prior.after_revision.checked_add(1) == Some(saved.revision);
                    let exact_saved_frontier = current.after_revision == saved.revision;
                    let saved_revision_applied = current.applied_revision >= saved.revision;
                    same_saved_run
                        && same_original_run
                        && follows_original_frontier
                        && exact_saved_frontier
                        && saved_revision_applied
                });
                (current.run_epoch != prior.run_epoch
                    || current.after_revision != prior.after_revision)
                    && !expected_saved_ack
            })
        });
        self.stale_frontier = frontier_changed && !self.current_accepts_frontier_change;
        self.frontiers = frontiers;
        self.pending_tasks = if read.pending_complete {
            pending_tasks
        } else {
            Vec::new()
        };
        self.pending_complete = read.pending_complete;
        if !self.stale_frontier {
            if self.frontiers.len() == 1 {
                self.context = self.frontiers.first().cloned();
            } else if let Some(selected) = &self.selected {
                self.context = self
                    .frontiers
                    .iter()
                    .find(|task| &task.task_id == selected)
                    .cloned();
            }
            if self.selected.as_ref().is_some_and(|selected| {
                !self.frontiers.iter().any(|task| &task.task_id == selected)
            }) {
                self.selected = None;
            }
        }
        self.current_accepts_frontier_change = false;
        self.snapshot_generation = read.generation;
        self.snapshot_observed = self.current_issued;
        self.snapshot_complete = true;
        self.invalidated = false;
        self.discovery = AgentTaskDiscovery::Ready;
    }

    fn confirm_saved_direction_from_collection(&mut self, frontiers: &[AgentTaskFrontier]) {
        let Some(saved) = &self.saved_direction else {
            return;
        };
        if !matches!(self.direction_state, AgentDirectionState::Saved { .. }) {
            return;
        }
        let applied = frontiers.iter().any(|frontier| {
            let same_saved_run = (frontier.task_id.as_str(), frontier.run_epoch)
                == (saved.task_id.as_str(), saved.run_epoch);
            let includes_saved_revision = frontier.after_revision >= saved.revision;
            let saved_revision_applied = frontier.applied_revision >= saved.revision;
            same_saved_run && includes_saved_revision && saved_revision_applied
        });
        if applied {
            self.direction_state = AgentDirectionState::Applied {
                task_id: saved.task_id.clone(),
                run_epoch: saved.run_epoch,
                revision: saved.revision,
            };
        }
    }

    fn invalidate_snapshot(&mut self, discovery: AgentTaskDiscovery) {
        self.controls.clear();
        self.control_response_until = None;
        self.clear_draft_revalidation();
        self.invalidated = true;
        self.snapshot_complete = false;
        self.frontiers.clear();
        self.pending_tasks.clear();
        self.pending_complete = false;
        self.discovery = discovery;
    }

    fn poll_queue(&mut self) {
        let Some(queue) = &self.queue else {
            return;
        };
        let outcome_uncertain = self.remote.invocation_outcome_uncertain(queue.invocation);
        if outcome_uncertain {
            self.queue_state = AgentQueueState::Uncertain;
        }
        match queue.call.get() {
            RemoteCall::Pending
            | RemoteCall::Done(
                InvocationUpdate::Accepted { .. }
                | InvocationUpdate::Progress { .. }
                | InvocationUpdate::Item { .. },
            ) => {
                if !outcome_uncertain {
                    self.queue_state = AgentQueueState::Submitting;
                }
            }
            RemoteCall::Done(InvocationUpdate::Succeeded { output, .. }) => {
                self.queue_state = queue_task_id(&output)
                    .map_or(AgentQueueState::Failed, |task_id| AgentQueueState::Queued {
                        task_id,
                    });
                self.queue = None;
                self.refresh();
            }
            RemoteCall::Refused(_)
            | RemoteCall::Done(
                InvocationUpdate::Failed { .. } | InvocationUpdate::Cancelled { .. },
            ) => {
                self.queue_state = AgentQueueState::Failed;
                self.queue = None;
            }
        }
    }

    fn poll_stop(&mut self) {
        let Some(stop) = &self.stop else {
            return;
        };
        let outcome_uncertain = self.remote.invocation_outcome_uncertain(stop.invocation);
        if outcome_uncertain {
            self.stop_state = AgentStopState::Uncertain {
                task_id: stop.frontier.task_id.clone(),
                run_epoch: stop.frontier.run_epoch,
            };
        }
        match stop.call.get() {
            RemoteCall::Pending
            | RemoteCall::Done(
                InvocationUpdate::Accepted { .. }
                | InvocationUpdate::Progress { .. }
                | InvocationUpdate::Item { .. },
            ) => {
                if !outcome_uncertain {
                    self.stop_state = AgentStopState::Submitting {
                        task_id: stop.frontier.task_id.clone(),
                        run_epoch: stop.frontier.run_epoch,
                    };
                }
            }
            RemoteCall::Done(InvocationUpdate::Succeeded { output, .. }) => {
                let state = if stop_requested(&output, &stop.frontier) {
                    AgentStopState::Stopping {
                        task_id: stop.frontier.task_id.clone(),
                        run_epoch: stop.frontier.run_epoch,
                    }
                } else {
                    AgentStopState::Failed {
                        task_id: stop.frontier.task_id.clone(),
                        run_epoch: stop.frontier.run_epoch,
                    }
                };
                self.stop_state = state;
                self.stop = None;
                self.refresh();
            }
            RemoteCall::Refused(_)
            | RemoteCall::Done(
                InvocationUpdate::Failed { .. } | InvocationUpdate::Cancelled { .. },
            ) => {
                self.stop_state = AgentStopState::Failed {
                    task_id: stop.frontier.task_id.clone(),
                    run_epoch: stop.frontier.run_epoch,
                };
                self.stop = None;
            }
        }
    }

    fn poll_direction(&mut self) {
        let Some(direction) = &mut self.direction else {
            return;
        };
        let outcome_uncertain = self
            .remote
            .invocation_outcome_uncertain(direction.invocation);
        if outcome_uncertain {
            self.direction_state = AgentDirectionState::Uncertain {
                task_id: direction.frontier.task_id.clone(),
                run_epoch: direction.frontier.run_epoch,
            };
        }
        match direction.call.get() {
            RemoteCall::Pending
            | RemoteCall::Done(
                InvocationUpdate::Accepted { .. }
                | InvocationUpdate::Progress { .. }
                | InvocationUpdate::Item { .. },
            ) => {
                if !outcome_uncertain {
                    self.direction_state = AgentDirectionState::Submitting;
                }
            }
            RemoteCall::Done(InvocationUpdate::Succeeded { output, .. }) => {
                let Some(revision) = direction_revision(&output, &direction.frontier.task_id)
                else {
                    self.invalidate_direction("Task Center returned an unbound direction receipt");
                    return;
                };
                if revision != direction.frontier.after_revision.saturating_add(1) {
                    self.invalidate_direction("the task direction frontier changed");
                    return;
                }
                let call = self.remote.invoke(
                    task_target(
                        &self.workspace,
                        &direction.frontier.task_id,
                        READ_TASK_ACTION,
                    ),
                    InvokeArgument::None,
                    None,
                );
                let read_issued = self.next_observation;
                self.next_observation = self.next_observation.saturating_add(1);
                self.saved_direction = Some(SavedDirection {
                    task_id: direction.frontier.task_id.clone(),
                    run_epoch: direction.frontier.run_epoch,
                    revision,
                    read: Some(call),
                    read_issued,
                });
                self.direction_state = AgentDirectionState::Saved {
                    task_id: direction.frontier.task_id.clone(),
                    run_epoch: direction.frontier.run_epoch,
                    revision,
                };
                self.direction = None;
            }
            RemoteCall::Refused(error) => {
                self.invalidate_direction(&error.to_string());
            }
            RemoteCall::Done(InvocationUpdate::Failed { error, .. }) => {
                self.invalidate_direction(&error.message);
            }
            RemoteCall::Done(InvocationUpdate::Cancelled { message, .. }) => {
                self.invalidate_direction(
                    &message.unwrap_or_else(|| "direction was cancelled".into()),
                );
            }
        }
    }

    fn poll_saved_direction(&mut self) {
        let Some(saved) = &mut self.saved_direction else {
            return;
        };
        let Some(read) = saved.read.as_ref() else {
            return;
        };
        let (generation, frontier) = match task_read(&read.get(), &self.agent, &saved.task_id) {
            Ok(Some(read)) => read,
            Err(TaskReadError::Pending) => return,
            Ok(None) | Err(TaskReadError::Invalid) => {
                saved.read = None;
                self.stale_frontier = true;
                return;
            }
        };
        if (generation, saved.read_issued) < (self.snapshot_generation, self.snapshot_observed) {
            saved.read = None;
            return;
        }
        let same_run = frontier.run_epoch == saved.run_epoch;
        let includes_saved_direction = frontier.after_revision >= saved.revision;
        if !same_run || !includes_saved_direction {
            saved.read = None;
            self.stale_frontier = true;
            return;
        }
        let newer_direction_exists = frontier.after_revision > saved.revision;
        saved.read = None;
        self.direction_state = if frontier.applied_revision >= saved.revision {
            AgentDirectionState::Applied {
                task_id: saved.task_id.clone(),
                run_epoch: saved.run_epoch,
                revision: saved.revision,
            }
        } else {
            AgentDirectionState::Saved {
                task_id: saved.task_id.clone(),
                run_epoch: saved.run_epoch,
                revision: saved.revision,
            }
        };
        self.confirmed.insert(
            frontier.task_id.clone(),
            ConfirmedFrontier {
                frontier,
                generation,
                observed: saved.read_issued,
            },
        );
        if let Some(confirmed) = self.confirmed.get(&saved.task_id) {
            self.frontiers
                .retain(|task| task.task_id != confirmed.frontier.task_id);
            self.frontiers.push(confirmed.frontier.clone());
            if !newer_direction_exists
                && self.context.as_ref().is_some_and(|context| {
                    context.task_id == confirmed.frontier.task_id
                        && context.run_epoch == confirmed.frontier.run_epoch
                })
            {
                self.context = Some(confirmed.frontier.clone());
            }
        }
        self.stale_frontier |= newer_direction_exists;
    }

    fn invalidate_direction(&mut self, message: &str) {
        self.clear_draft_revalidation();
        if let Some(direction) = &self.direction {
            self.context = Some(direction.frontier.clone());
        } else if self.context.is_none() {
            self.context = match self.choice() {
                AgentTaskChoice::One(task) | AgentTaskChoice::Selected(task) => Some(task),
                AgentTaskChoice::None | AgentTaskChoice::Choose(_) => None,
            };
        }
        self.direction_state = AgentDirectionState::NeedsRefresh(message.to_owned());
        self.invalidated = true;
        self.snapshot_complete = false;
        self.frontiers.clear();
        self.selected = None;
        self.direction = None;
        self.saved_direction = None;
        self.discovery = AgentTaskDiscovery::Unavailable;
    }
}

impl Collaborative for AgentTaskFrontiers {
    fn take_collab_requests(&mut self) -> Vec<crate::collaboration::CollabRequest> {
        self.remote.take_collab_requests()
    }

    fn handle_collab_event(&mut self, event: &crate::collaboration::CollabEvent) {
        self.remote.handle_collab_event(event);
        self.poll();
    }
}

fn task_target(workspace: &str, task: &str, action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: workspace_provider(workspace),
        address: ActionAddress::new(format!("{TASK_RESOURCE_PREFIX}{task}"), action),
    }
}

fn task_collection_target(workspace: &str, action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: workspace_provider(workspace),
        address: ActionAddress::new(TASK_COLLECTION, action),
    }
}

fn valid_route(route: &str) -> bool {
    matches!(route, "manual" | "rules_only" | "model")
}

fn valid_execution(
    execution: Option<&tilo_desktop_model::WorkspaceTaskExecution>,
    run_epoch: u64,
) -> bool {
    use tilo_desktop_model::WorkspaceTaskExecutionPhase;
    execution.is_none_or(|execution| {
        execution.run_epoch == run_epoch
            && match execution.phase {
                // Zero is the initial acceptance transaction; later actual
                // reports may confirm Accepted with a higher sequence.
                WorkspaceTaskExecutionPhase::Accepted => true,
                WorkspaceTaskExecutionPhase::Planning | WorkspaceTaskExecutionPhase::Executing => {
                    execution.sequence > 0
                }
            }
    })
}

enum TaskReadError {
    Pending,
    Invalid,
}

fn task_read(
    update: &RemoteCall<InvocationUpdate>,
    agent: &str,
    expected_task: &str,
) -> Result<Option<(u64, AgentTaskFrontier)>, TaskReadError> {
    let RemoteCall::Done(InvocationUpdate::Succeeded {
        output: ActionOutput {
            value: Some(value), ..
        },
        ..
    }) = update
    else {
        return if matches!(
            update,
            RemoteCall::Pending
                | RemoteCall::Done(
                    InvocationUpdate::Accepted { .. }
                        | InvocationUpdate::Progress { .. }
                        | InvocationUpdate::Item { .. }
                )
        ) {
            Err(TaskReadError::Pending)
        } else {
            Err(TaskReadError::Invalid)
        };
    };
    let read =
        serde_json::from_value::<TaskRead>(value.clone()).map_err(|_| TaskReadError::Invalid)?;
    if read.task_id != expected_task {
        return Err(TaskReadError::Invalid);
    }
    let Some(run_epoch) = read.run_epoch else {
        return Ok(None);
    };
    if read.generation == 0
        || read.task_id.is_empty()
        || read.title.trim().is_empty()
        || !valid_route(&read.route)
        || read.status != "in_progress"
        || read.assigned_agent.as_deref() != Some(agent)
        || read.active_agent.as_deref() != Some(agent)
    {
        return Ok(None);
    }
    let Some(applied_revision) = read.applied_revision else {
        return Err(TaskReadError::Invalid);
    };
    if run_epoch == 0 || applied_revision > read.after_revision {
        return Ok(None);
    }
    if !valid_execution(read.execution.as_ref(), run_epoch) {
        return Err(TaskReadError::Invalid);
    }
    Ok(Some((
        read.generation,
        AgentTaskFrontier {
            task_id: read.task_id,
            title: read.title,
            run_epoch,
            after_revision: read.after_revision,
            applied_revision,
            route: read.route,
            cancellation_requested: read.cancellation_requested,
            execution: read.execution,
        },
    )))
}

fn direction_revision(output: &ActionOutput, expected_task: &str) -> Option<u64> {
    let result = output.value.as_ref()?.get("result")?;
    let direction = result.get("direction_added")?;
    (direction.get("task_id")?.as_str()? == expected_task)
        .then(|| direction.get("revision")?.as_u64())?
}

fn queue_task_id(output: &ActionOutput) -> Option<String> {
    output
        .value
        .as_ref()?
        .get("result")?
        .get("queued")?
        .get("task_id")?
        .as_str()
        .filter(|task| !task.is_empty())
        .map(str::to_owned)
}

fn stop_requested(output: &ActionOutput, frontier: &AgentTaskFrontier) -> bool {
    let Some(stop) = output
        .value
        .as_ref()
        .and_then(|value| value.get("result"))
        .and_then(|result| result.get("cancellation_requested"))
    else {
        return false;
    };
    stop.get("task_id").and_then(serde_json::Value::as_str) == Some(frontier.task_id.as_str())
        && stop.get("epoch").and_then(serde_json::Value::as_u64) == Some(frontier.run_epoch)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};

    use tilo_actions::remote::{
        ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
        InvocationState, InvokeRequest, Pending, ReceiptStatus,
    };

    use super::*;

    struct FrontierHost {
        current: Mutex<VecDeque<serde_json::Value>>,
        reads: Mutex<BTreeMap<String, VecDeque<serde_json::Value>>>,
        invocations: Mutex<Vec<InvokeRequest>>,
        control_page: Mutex<Option<CataloguePage>>,
    }

    impl FrontierHost {
        fn new(
            current: Vec<serde_json::Value>,
            reads: impl IntoIterator<Item = (&'static str, Vec<serde_json::Value>)>,
        ) -> Self {
            let reads = reads
                .into_iter()
                .map(|(task, values)| (task.to_owned(), values.into()))
                .collect::<BTreeMap<_, _>>();
            Self {
                current: Mutex::new(VecDeque::from([current_snapshot(1, current)])),
                reads: Mutex::new(reads),
                invocations: Mutex::new(Vec::new()),
                control_page: Mutex::new(None),
            }
        }

        fn with_snapshots(snapshots: Vec<serde_json::Value>) -> Self {
            Self {
                current: Mutex::new(snapshots.into()),
                reads: Mutex::new(BTreeMap::new()),
                invocations: Mutex::new(Vec::new()),
                control_page: Mutex::new(None),
            }
        }

        fn requests(&self) -> Vec<InvokeRequest> {
            self.invocations.lock().unwrap().clone()
        }

        fn incomplete(total: u64) -> Self {
            Self {
                current: Mutex::new(VecDeque::from([serde_json::json!({
                    "generation": 1,
                    "complete": false,
                    "total": total,
                    "tasks": [],
                    "pending_complete": true,
                    "pending_total": 0,
                    "pending": [],
                })])),
                reads: Mutex::new(BTreeMap::new()),
                invocations: Mutex::new(Vec::new()),
                control_page: Mutex::new(None),
            }
        }
    }

    impl ActionHost for FrontierHost {
        fn query(
            &self,
            _caller: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionHostError>> {
            if let Some(mut page) = self.control_page.lock().unwrap().clone() {
                // Keep adversarial resource/provider fields, but answer only
                // the requested operation so each negative exercises its
                // own invariant rather than an unrelated oversized page.
                page.rows.retain(|row| {
                    row.action.as_ref().is_some_and(|action| {
                        query
                            .target
                            .as_ref()
                            .is_some_and(|target| target.action == action.address.action)
                    })
                });
                return Pending::ready(Ok(page));
            }
            let provider = query.provider.as_deref().expect("room-scoped catalogue");
            let mut resources = std::collections::BTreeSet::new();
            for snapshot in self.current.lock().unwrap().iter() {
                for task in snapshot["tasks"].as_array().into_iter().flatten() {
                    if let Some(id) = task["task_id"].as_str() {
                        resources.insert(format!("{TASK_RESOURCE_PREFIX}{id}"));
                    }
                }
            }
            resources.extend(
                self.reads
                    .lock()
                    .unwrap()
                    .keys()
                    .map(|id| format!("{TASK_RESOURCE_PREFIX}{id}")),
            );
            let mut rows = vec![control_row(provider, TASK_COLLECTION, QUEUE_TASK_ACTION)];
            for resource in resources {
                rows.push(control_row(provider, &resource, DIRECT_TASK_ACTION));
                rows.push(control_row(provider, &resource, STOP_TASK_ACTION));
            }
            rows.retain(|row| {
                row.action
                    .as_ref()
                    .is_some_and(|action| query.target.as_ref() == Some(&action.address))
            });
            Pending::ready(Ok(CataloguePage {
                revision: "task-frontier-1".into(),
                rows,
                exhausted: true,
                ..CataloguePage::default()
            }))
        }

        fn invoke(
            &self,
            _caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
            self.invocations.lock().unwrap().push(request.clone());
            let task = request
                .target
                .address
                .resource
                .strip_prefix(TASK_RESOURCE_PREFIX)
                .unwrap_or_default();
            let value = if request.target.address.action == READ_CURRENT_TASKS_ACTION {
                let mut snapshots = self.current.lock().unwrap();
                if snapshots.len() > 1 {
                    snapshots.pop_front()
                } else {
                    snapshots.front().cloned()
                }
            } else if request.target.address.action == READ_TASK_ACTION {
                self.reads
                    .lock()
                    .unwrap()
                    .get_mut(task)
                    .and_then(VecDeque::pop_front)
            } else if request.target.address.action == DIRECT_TASK_ACTION {
                let after = argument_json(&request.argument)
                    .and_then(|value| value.get("after_revision"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default();
                Some(serde_json::json!({
                    "result": {"direction_added": {"task_id": task, "revision": after + 1}}
                }))
            } else if request.target.address.action == QUEUE_TASK_ACTION {
                Some(serde_json::json!({
                    "generation": 2,
                    "result": {"queued": {"task_id": "queued-task"}},
                    "delivery": "queued"
                }))
            } else if request.target.address.action == STOP_TASK_ACTION {
                let epoch = argument_json(&request.argument)
                    .and_then(|value| value.get("run_epoch"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default();
                Some(serde_json::json!({
                    "generation": 2,
                    "result": {"cancellation_requested": {
                        "task_id": task,
                        "epoch": epoch
                    }},
                    "delivery": "saved"
                }))
            } else {
                None
            };
            let Some(value) = value else {
                return Pending::ready(Err(ActionHostError::InvalidRequest {
                    reason: "fixture has no response".into(),
                }));
            };
            Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                },
            }))
        }

        fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
            None
        }

        fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Result<(), ActionHostError> {
            Err(ActionHostError::NotCancellable {
                reason: "fixture invocations settle immediately".into(),
            })
        }
    }

    fn control_row(
        provider: &str,
        resource: &str,
        action: &str,
    ) -> tilo_actions::remote::CatalogueRow {
        tilo_actions::remote::CatalogueRow {
            provider: provider.into(),
            app_id: "task-center".into(),
            parent_resource: String::new(),
            resource: tilo_actions::remote::CatalogueResource {
                id: resource.into(),
                kind: "desktop.task".into(),
                label: "Task".into(),
                presentation: None,
            },
            action: Some(tilo_actions::ActionDescriptor {
                address: ActionAddress::new(resource, action),
                semantic_identity: None,
                resource_kind: "desktop.task".into(),
                resource_label: "Task".into(),
                action_label: action.into(),
                description: None,
                locus: tilo_actions::ExecutionLocus::ClientLocal,
                kind: tilo_actions::OperationKind::Document,
                contract: tilo_actions::ActionContract::default(),
                input: tilo_actions::InputSpec::None,
                params: Vec::new(),
                view: None,
            }),
            has_children: false,
            visibility: None,
            fresh_binding: None,
        }
    }

    fn current_task(
        task: &str,
        title: &str,
        agent: &str,
        after_revision: u64,
        applied_revision: u64,
    ) -> serde_json::Value {
        serde_json::json!({
            "task_id": task,
            "title": title,
            "assigned_agent": agent,
            "status": "in_progress",
            "run_epoch": 7,
            "active_agent": agent,
            "after_revision": after_revision,
            "applied_revision": applied_revision,
            "route": "model",
            "cancellation_requested": false
        })
    }

    fn current_snapshot(generation: u64, tasks: Vec<serde_json::Value>) -> serde_json::Value {
        current_snapshot_with_pending(generation, tasks, true, Vec::new())
    }

    fn current_snapshot_with_pending(
        generation: u64,
        tasks: Vec<serde_json::Value>,
        pending_complete: bool,
        pending: Vec<serde_json::Value>,
    ) -> serde_json::Value {
        serde_json::json!({
            "generation": generation,
            "complete": true,
            "total": tasks.len(),
            "tasks": tasks,
            "pending_complete": pending_complete,
            "pending_total": if pending_complete { pending.len() } else { pending.len() + 1 },
            "pending": pending,
        })
    }

    fn pending_task(task: &str, title: &str, agent: &str, status: &str) -> serde_json::Value {
        serde_json::json!({
            "task_id": task,
            "title": title,
            "assigned_agent": agent,
            "status": status,
            "route": "model",
        })
    }

    fn exact_task(
        task: &str,
        title: &str,
        agent: &str,
        after_revision: u64,
        applied_revision: u64,
    ) -> serde_json::Value {
        exact_task_at(1, task, title, agent, after_revision, applied_revision)
    }

    #[test]
    fn current_tasks_retain_only_exact_authenticated_execution_phases() {
        for (phase, sequence) in [
            (WorkspaceTaskExecutionPhase::Accepted, 0),
            (WorkspaceTaskExecutionPhase::Accepted, 3),
            (WorkspaceTaskExecutionPhase::Planning, 1),
            (WorkspaceTaskExecutionPhase::Executing, 2),
        ] {
            let execution = WorkspaceTaskExecution {
                run_epoch: 7,
                sequence,
                phase,
                updated_at: 12,
            };
            let mut task = current_task("one", "Current task", "agent-a", 4, 4);
            task["execution"] = serde_json::to_value(&execution).unwrap();
            let host = Arc::new(FrontierHost::new(vec![task], []));
            let model = model(host, "room", "agent-a");
            let AgentTaskChoice::One(frontier) = model.choice() else {
                panic!("the owner returned one current task")
            };
            assert_eq!(frontier.execution, Some(execution));
        }
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Current task", "agent-a", 4, 4)],
            [],
        ));
        let model = model(host, "room", "agent-a");
        let AgentTaskChoice::One(frontier) = model.choice() else {
            panic!("legacy owner observation remains available")
        };
        assert_eq!(frontier.execution, None);
    }

    #[test]
    fn foreign_epoch_or_malformed_execution_phase_invalidates_current_observation() {
        for (epoch, sequence, phase) in [
            (6, 0, "accepted"),
            (7, 0, "planning"),
            (7, 0, "executing"),
            (7, 1, "working"),
        ] {
            let mut task = current_task("one", "Current task", "agent-a", 4, 4);
            task["execution"] = serde_json::json!({
                "run_epoch": epoch, "sequence": sequence, "phase": phase, "updated_at": 12,
            });
            let host = Arc::new(FrontierHost::new(vec![task], []));
            let model = model(host, "room", "agent-a");
            assert_eq!(model.discovery(), &AgentTaskDiscovery::Unavailable);
            assert_eq!(model.choice(), AgentTaskChoice::None);
            assert!(!model.can_steer());
            assert!(!model.can_stop());
        }
    }

    #[test]
    fn exact_task_reads_preserve_phases_and_refuse_foreign_or_invalid_execution() {
        for (epoch, sequence, phase, valid) in [
            (7, 0, "accepted", true),
            (7, 3, "accepted", true),
            (7, 1, "planning", true),
            (7, 2, "executing", true),
            (6, 0, "accepted", false),
            (7, 0, "planning", false),
            (7, 0, "executing", false),
            (7, 1, "working", false),
        ] {
            let mut value = exact_task("one", "Current task", "agent-a", 4, 4);
            let execution = serde_json::json!({
                "run_epoch": epoch, "sequence": sequence, "phase": phase, "updated_at": 12,
            });
            value["execution"] = execution.clone();
            let update = RemoteCall::Done(InvocationUpdate::Succeeded {
                invocation: InvocationId::new(),
                output: ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                },
            });
            let result = task_read(&update, "agent-a", "one");
            if valid {
                let Ok(Some((generation, task))) = result else {
                    panic!("an exact authenticated phase must survive the task read")
                };
                assert_eq!(generation, 1);
                assert_eq!(
                    serde_json::to_value(task.execution.unwrap()).unwrap(),
                    execution
                );
            } else {
                assert!(matches!(result, Err(TaskReadError::Invalid)));
            }
        }
    }

    fn exact_task_at(
        generation: u64,
        task: &str,
        title: &str,
        agent: &str,
        after_revision: u64,
        applied_revision: u64,
    ) -> serde_json::Value {
        let mut task = current_task(task, title, agent, after_revision, applied_revision);
        let object = task.as_object_mut().unwrap();
        object.insert("generation".into(), serde_json::json!(generation));
        task
    }

    fn argument_json(
        argument: &InvokeArgument,
    ) -> Option<&serde_json::Map<String, serde_json::Value>> {
        match argument {
            InvokeArgument::Json(value) => Some(value),
            InvokeArgument::None | InvokeArgument::Text(_) => None,
        }
    }

    fn model(host: Arc<FrontierHost>, workspace: &str, agent: &str) -> AgentTaskFrontiers {
        let mut model = AgentTaskFrontiers::with_actions(RemoteActions::with_host(
            host,
            Caller::local_app("so.tilo.chat").in_room(workspace),
        ));
        model.observe(workspace, agent);
        model
    }

    #[test]
    fn reading_tasks_does_not_grant_task_controls() {
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Current task", "agent-a", 4, 4)],
            [],
        ));
        *host.control_page.lock().unwrap() = Some(CataloguePage {
            revision: "read-only".into(),
            exhausted: true,
            ..CataloguePage::default()
        });
        let mut model = model(host.clone(), "room", "agent-a");
        assert!(matches!(model.choice(), AgentTaskChoice::One(_)));
        assert!(!model.can_queue());
        assert!(!model.can_steer());
        assert!(!model.can_stop());
        assert!(model.queue("Keep this draft", None).is_err());
        assert!(model.steer("Keep this direction").is_err());
        assert!(model.stop().is_err());
        assert!(
            host.requests()
                .iter()
                .all(|request| request.target.address.action == READ_CURRENT_TASKS_ACTION)
        );
    }

    #[test]
    fn control_catalogue_requires_complete_fresh_exact_room_and_task() {
        for condition in ["partial", "stale", "budget", "wrong-room", "wrong-task"] {
            let host = Arc::new(FrontierHost::new(
                vec![current_task("one", "Current task", "agent-a", 4, 4)],
                [],
            ));
            let provider = workspace_provider(if condition == "wrong-room" {
                "other"
            } else {
                "room"
            });
            let resource = if condition == "wrong-task" {
                "desktop:task:other"
            } else {
                "desktop:task:one"
            };
            *host.control_page.lock().unwrap() = Some(CataloguePage {
                revision: "controls".into(),
                rows: vec![
                    control_row(&provider, resource, DIRECT_TASK_ACTION),
                    control_row(&provider, resource, STOP_TASK_ACTION),
                ],
                exhausted: condition != "partial",
                stale_cursor: condition == "stale",
                budget_exhausted: condition == "budget",
                ..CataloguePage::default()
            });
            let model = model(host, "room", "agent-a");
            assert!(!model.can_steer(), "{condition}");
            assert!(!model.can_stop(), "{condition}");
        }
    }

    #[test]
    fn each_control_requires_its_own_offered_action() {
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Current task", "agent-a", 4, 4)],
            [],
        ));
        *host.control_page.lock().unwrap() = Some(CataloguePage {
            revision: "queue-only".into(),
            rows: vec![control_row(
                &workspace_provider("room"),
                TASK_COLLECTION,
                QUEUE_TASK_ACTION,
            )],
            exhausted: true,
            ..CataloguePage::default()
        });
        let model = model(host, "room", "agent-a");
        assert!(model.can_queue());
        assert!(!model.can_steer());
        assert!(!model.can_stop());
    }

    #[test]
    fn switching_tasks_discards_previously_offered_controls() {
        let host = Arc::new(FrontierHost::new(
            vec![
                current_task("one", "First", "agent-a", 4, 4),
                current_task("two", "Second", "agent-a", 4, 4),
            ],
            [],
        ));
        let mut model = model(host.clone(), "room", "agent-a");
        assert!(!model.can_steer());
        model.select("one").unwrap();
        assert!(model.can_steer());
        assert!(model.can_stop());
        model.select("two").unwrap();
        assert!(model.can_steer());
        *host.control_page.lock().unwrap() = Some(CataloguePage {
            revision: "viewer".into(),
            exhausted: true,
            ..CataloguePage::default()
        });
        model.select("one").unwrap();
        assert!(!model.can_steer());
        assert!(!model.can_stop());
        assert!(!model.can_queue());
        assert!(model.steer("Do not use the old grant").is_err());
        assert!(
            host.requests()
                .iter()
                .all(|request| request.target.address.action == READ_CURRENT_TASKS_ACTION)
        );
    }

    #[test]
    fn permission_downgrade_revokes_controls_without_losing_task_context() {
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Current task", "agent-a", 4, 4)],
            [],
        ));
        let mut model = model(host.clone(), "room", "agent-a");
        assert!(model.can_queue());
        assert!(model.can_steer());
        assert!(model.can_stop());
        *host.control_page.lock().unwrap() = Some(CataloguePage {
            revision: "viewer".into(),
            exhausted: true,
            ..CataloguePage::default()
        });
        model.refresh();
        assert!(matches!(model.choice(), AgentTaskChoice::One(_)));
        assert!(!model.can_queue());
        assert!(!model.can_steer());
        assert!(!model.can_stop());
    }

    #[test]
    fn private_draft_editability_is_bounded_to_authority_revalidation() {
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Current task", "agent-a", 4, 4)],
            [],
        ));
        let mut model = model(host, "room", "agent-a");
        assert!(model.can_queue());
        assert!(model.can_steer());

        model.remote = RemoteActions::session();
        model.start_current_refresh(false);
        assert!(!model.can_queue());
        assert!(!model.can_steer());
        assert!(model.can_edit_queue_draft());
        assert!(model.can_edit_steer_draft());

        let invocation = InvocationId::new();
        model.current = Some(LocalState::new(RemoteCall::Done(
            InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: Some(serde_json::json!({"not": "an owner task snapshot"})),
                    resources: Vec::new(),
                },
            },
        )));
        model.collect_current_tasks(crate::background::Background::current().now());

        assert!(!model.can_edit_queue_draft());
        assert!(!model.can_edit_steer_draft());
        assert_eq!(model.discovery(), &AgentTaskDiscovery::Unavailable);
    }

    #[test]
    fn private_draft_revalidation_expires_while_the_owner_read_is_pending() {
        let clock = crate::background::Pumped::new();
        let _entered = clock.background().enter();
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Current task", "agent-a", 4, 4)],
            [],
        ));
        let mut model = model(host, "room", "agent-a");
        model.remote = RemoteActions::session();
        model.start_current_refresh(false);
        assert!(model.can_edit_queue_draft());
        assert!(model.can_edit_steer_draft());
        let expiry = model
            .draft_revalidation_until
            .expect("the pending owner read has a bounded editor grace");
        assert!(
            model
                .next_wake_deadline()
                .is_some_and(|wake| wake <= expiry)
        );

        clock.advance(
            expiry
                .saturating_duration_since(clock.background().now())
                .saturating_add(Duration::from_millis(1)),
        );
        model.poll();

        assert!(!model.can_edit_queue_draft());
        assert!(!model.can_edit_steer_draft());
        assert!(
            model
                .current
                .as_ref()
                .is_some_and(|call| call.get().is_pending())
        );
    }

    #[test]
    fn changed_steer_frontier_ends_private_draft_revalidation() {
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Current task", "agent-a", 4, 4)],
            [],
        ));
        let mut model = model(host, "room", "agent-a");
        model.remote = RemoteActions::session();
        model.start_current_refresh(false);
        assert!(model.can_edit_steer_draft());

        let invocation = InvocationId::new();
        model.current = Some(LocalState::new(RemoteCall::Done(
            InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: Some(current_snapshot(
                        9,
                        vec![current_task("one", "Changed frontier", "agent-a", 5, 4)],
                    )),
                    resources: Vec::new(),
                },
            },
        )));
        model.collect_current_tasks(crate::background::Background::current().now());
        model.settle_draft_revalidation();

        assert!(!model.can_edit_steer_draft());
        assert!(!model.can_steer());
    }

    #[test]
    fn multiple_owner_tasks_require_an_explicit_selection() {
        let host = Arc::new(FrontierHost::new(
            vec![
                current_task("one", "First", "agent-a", 2, 2),
                current_task("two", "Second", "agent-a", 4, 4),
            ],
            [],
        ));
        let mut model = model(Arc::clone(&host), "room", "agent-a");
        let AgentTaskChoice::Choose(tasks) = model.choice() else {
            panic!("both authenticated current tasks require a choice")
        };
        assert_eq!(tasks.len(), 2);
        assert_eq!(
            model.steer("Use the second draft"),
            Err("select which current task to steer".into())
        );
        assert!(model.select_for("other-room", "agent-a", "two").is_err());
        assert!(model.select_for("room", "other-agent", "two").is_err());
        model.select_for("room", "agent-a", "two").unwrap();
        model.steer("Use the second draft").unwrap();
        model.observe("room", "agent-a");
        let direct = host
            .requests()
            .into_iter()
            .find(|request| request.target.address.action == DIRECT_TASK_ACTION)
            .expect("the public Task Center direction action");
        assert_eq!(direct.target.address.resource, "desktop:task:two");
        assert_eq!(argument_json(&direct.argument).unwrap()["run_epoch"], 7);
        assert_eq!(
            argument_json(&direct.argument).unwrap()["after_revision"],
            4
        );
    }

    #[test]
    fn saved_and_applied_follow_only_the_exact_direction_revision() {
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Current task", "agent-a", 4, 4)],
            [(
                "one",
                vec![
                    exact_task("one", "Current task", "agent-a", 5, 4),
                    exact_task("one", "Current task", "agent-a", 5, 5),
                ],
            )],
        ));
        let mut model = model(host, "room", "agent-a");
        model.steer("Use the reviewed wording").unwrap();
        model.observe("room", "agent-a");
        assert_eq!(
            model.direction_state(),
            &AgentDirectionState::Saved {
                task_id: "one".into(),
                run_epoch: 7,
                revision: 5
            }
        );
        let AgentTaskChoice::One(frontier) = model.choice() else {
            panic!("the refreshed exact task remains selected")
        };
        assert_eq!(
            frontier.after_revision, 5,
            "the initial read cannot overwrite it"
        );

        model.refresh_direction_status().unwrap();
        model.observe("room", "agent-a");
        assert_eq!(
            model.direction_state(),
            &AgentDirectionState::Applied {
                task_id: "one".into(),
                run_epoch: 7,
                revision: 5
            }
        );
    }

    #[test]
    fn stale_run_retains_descriptive_context_but_cannot_submit_again() {
        let mut changed_run = exact_task("one", "Review launch copy", "agent-a", 5, 4);
        changed_run["run_epoch"] = serde_json::json!(8);
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Review launch copy", "agent-a", 4, 4)],
            [("one", vec![changed_run])],
        ));
        let mut model = model(host, "room", "agent-a");
        assert!(model.can_steer());
        model.steer("Use the reviewed wording").unwrap();
        model.observe("room", "agent-a");

        assert!(matches!(
            model.direction_state(),
            AgentDirectionState::Saved { revision: 5, .. }
        ));
        assert!(model.needs_frontier_confirmation());
        assert!(!model.can_steer());
        assert_eq!(
            model.context_task().map(|task| task.title),
            Some("Review launch copy".into()),
            "stale context names the affected task without authorizing it"
        );
        assert!(model.steer("Do not send this stale direction").is_err());
    }

    #[test]
    fn ended_exact_read_keeps_the_historical_saved_receipt() {
        let mut ended = exact_task_at(3, "one", "Review launch copy", "agent-a", 5, 4);
        ended["status"] = serde_json::json!("finished");
        ended["run_epoch"] = serde_json::Value::Null;
        ended["active_agent"] = serde_json::Value::Null;
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Review launch copy", "agent-a", 4, 4)],
            [(
                "one",
                vec![
                    exact_task_at(2, "one", "Review launch copy", "agent-a", 5, 4),
                    ended,
                ],
            )],
        ));
        let mut model = model(host, "room", "agent-a");
        model.steer("Use the reviewed wording").unwrap();
        model.observe("room", "agent-a");
        assert!(matches!(
            model.direction_state(),
            AgentDirectionState::Saved { revision: 5, .. }
        ));

        model.refresh_direction_status().unwrap();
        model.observe("room", "agent-a");

        assert!(model.needs_frontier_confirmation());
        assert_eq!(model.choice(), AgentTaskChoice::None);
        assert!(matches!(
            model.direction_state(),
            AgentDirectionState::Saved { revision: 5, .. }
        ));
        assert_eq!(model.context_task().unwrap().run_epoch, 7);
    }

    #[test]
    fn delayed_exact_read_does_not_rebind_a_new_draft_to_a_later_direction() {
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Review copy", "agent-a", 4, 4)],
            [(
                "one",
                vec![
                    exact_task_at(2, "one", "Review copy", "agent-a", 5, 4),
                    exact_task_at(3, "one", "Review copy", "agent-a", 6, 5),
                ],
            )],
        ));
        let mut model = model(host, "room", "agent-a");
        model.steer("First direction").unwrap();
        model.observe("room", "agent-a");
        assert_eq!(model.context_task().unwrap().after_revision, 5);

        model.refresh_direction_status().unwrap();
        model.observe("room", "agent-a");

        assert!(model.needs_frontier_confirmation());
        assert_eq!(model.choice(), AgentTaskChoice::None);
        assert_eq!(model.context_task().unwrap().after_revision, 5);
        assert!(matches!(
            model.direction_state(),
            AgentDirectionState::Applied { revision: 5, .. }
        ));
        assert!(model.steer("Unsent later draft").is_err());
    }

    #[test]
    fn a_mismatched_task_read_invalidates_the_whole_choice() {
        let host = Arc::new(FrontierHost::new(
            vec![current_task("different", "Wrong task", "agent-b", 1, 1)],
            [],
        ));
        let mut model = model(host, "room", "agent-a");
        assert_eq!(model.choice(), AgentTaskChoice::None);
        assert!(model.select("one").is_err());
        assert!(model.steer("Do not trust this row").is_err());
    }

    #[test]
    fn malformed_collection_frontiers_are_never_steerable() {
        let mut zero_epoch = current_task("one", "Current task", "agent-a", 4, 4);
        zero_epoch["run_epoch"] = serde_json::json!(0);
        let applied_ahead = current_task("one", "Current task", "agent-a", 4, 5);
        for malformed in [zero_epoch, applied_ahead] {
            let host = Arc::new(FrontierHost::new(vec![malformed], []));
            let model = model(host, "room", "agent-a");
            assert_eq!(model.choice(), AgentTaskChoice::None);
            assert_eq!(model.discovery(), &AgentTaskDiscovery::Unavailable);
            assert!(!model.can_steer());
        }
    }

    #[test]
    fn malformed_exact_frontiers_cannot_claim_a_saved_direction_was_applied() {
        let mut zero_epoch = exact_task_at(2, "one", "Current task", "agent-a", 5, 5);
        zero_epoch["run_epoch"] = serde_json::json!(0);
        let applied_ahead = exact_task_at(2, "one", "Current task", "agent-a", 5, 6);
        let mut unknown_route = exact_task_at(2, "one", "Current task", "agent-a", 5, 5);
        unknown_route["route"] = serde_json::json!("unknown-owner-route");
        for malformed in [zero_epoch, applied_ahead, unknown_route] {
            let host = Arc::new(FrontierHost::new(
                vec![current_task("one", "Current task", "agent-a", 4, 4)],
                [("one", vec![malformed])],
            ));
            let mut model = model(host, "room", "agent-a");
            model.steer("Use the reviewed wording").unwrap();
            model.observe("room", "agent-a");

            assert!(matches!(
                model.direction_state(),
                AgentDirectionState::Saved { revision: 5, .. }
            ));
            assert!(model.needs_frontier_confirmation());
            assert!(model.confirmed.is_empty());
        }
    }

    #[test]
    fn an_incomplete_owner_snapshot_is_not_misrepresented_as_no_task() {
        let host = Arc::new(FrontierHost::incomplete(300));
        let model = model(host, "room", "agent-a");
        assert_eq!(model.choice(), AgentTaskChoice::None);
        assert_eq!(
            model.discovery(),
            &AgentTaskDiscovery::Incomplete { total: 300 }
        );
    }

    #[test]
    fn automatic_refresh_requires_confirmation_before_rebinding_a_draft_frontier() {
        let host = Arc::new(FrontierHost::with_snapshots(vec![
            current_snapshot(
                1,
                vec![current_task("one", "Current task", "agent-a", 4, 4)],
            ),
            current_snapshot(
                2,
                vec![current_task("one", "Current task", "agent-a", 5, 4)],
            ),
            current_snapshot(
                2,
                vec![current_task("one", "Current task", "agent-a", 5, 4)],
            ),
        ]));
        let mut model = model(host, "room", "agent-a");
        let deadline = model.next_refresh_deadline().expect("bounded refresh");

        model.poll_at(deadline);

        assert!(model.needs_frontier_confirmation());
        assert_eq!(model.choice(), AgentTaskChoice::None);
        assert_eq!(model.context_task().unwrap().after_revision, 4);
        assert!(
            model
                .steer("Keep this draft on its reviewed frontier")
                .is_err()
        );

        model.refresh();
        let AgentTaskChoice::One(frontier) = model.choice() else {
            panic!("explicit refresh accepts the current owner frontier")
        };
        assert_eq!(frontier.after_revision, 5);
        assert!(!model.needs_frontier_confirmation());
    }

    #[test]
    fn automatic_refresh_preserves_selection_on_the_same_direction_frontier() {
        let host = Arc::new(FrontierHost::with_snapshots(vec![
            current_snapshot(
                1,
                vec![
                    current_task("one", "First", "agent-a", 2, 2),
                    current_task("two", "Second", "agent-a", 4, 3),
                ],
            ),
            current_snapshot(
                2,
                vec![
                    current_task("one", "First", "agent-a", 2, 2),
                    current_task("two", "Second", "agent-a", 4, 4),
                ],
            ),
        ]));
        let mut model = model(host, "room", "agent-a");
        model.select("two").unwrap();
        let deadline = model.next_refresh_deadline().expect("bounded refresh");

        model.poll_at(deadline);

        let AgentTaskChoice::Selected(frontier) = model.choice() else {
            panic!("same run and direction frontier preserves selection")
        };
        assert_eq!(frontier.task_id, "two");
        assert_eq!(frontier.after_revision, 4);
        assert_eq!(frontier.applied_revision, 4);
        assert!(!model.needs_frontier_confirmation());
    }

    #[test]
    fn ended_task_revokes_steering_but_keeps_its_applied_receipt() {
        let host = Arc::new(FrontierHost {
            current: Mutex::new(VecDeque::from([
                current_snapshot(
                    1,
                    vec![current_task("one", "Current task", "agent-a", 4, 4)],
                ),
                current_snapshot(3, Vec::new()),
            ])),
            reads: Mutex::new(BTreeMap::from([(
                "one".into(),
                VecDeque::from([exact_task_at(2, "one", "Current task", "agent-a", 5, 5)]),
            )])),
            invocations: Mutex::new(Vec::new()),
            control_page: Mutex::new(None),
        });
        let mut model = model(host, "room", "agent-a");
        model.steer("Use the final reviewed wording").unwrap();
        model.observe("room", "agent-a");
        assert!(matches!(
            model.direction_state(),
            AgentDirectionState::Applied { revision: 5, .. }
        ));

        let deadline = model.next_refresh_deadline().expect("bounded refresh");
        model.poll_at(deadline);

        assert_eq!(model.choice(), AgentTaskChoice::None);
        assert!(!model.can_steer());
        assert!(model.needs_frontier_confirmation());
        assert_eq!(model.context_task().unwrap().task_id, "one");
        assert!(matches!(
            model.direction_state(),
            AgentDirectionState::Applied { revision: 5, .. }
        ));
    }

    #[test]
    fn idle_collection_confirms_the_exact_saved_direction_as_applied() {
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Current task", "agent-a", 4, 4)],
            [(
                "one",
                vec![exact_task_at(2, "one", "Current task", "agent-a", 5, 4)],
            )],
        ));
        let mut model = model(host, "room", "agent-a");
        model.steer("Use the final reviewed wording").unwrap();
        model.poll_direction();
        assert!(matches!(
            model.direction_state(),
            AgentDirectionState::Saved { revision: 5, .. }
        ));

        let collection_issued = model.issue_observation();
        let invocation = InvocationId::new();
        model.current = Some(LocalState::new(RemoteCall::Done(
            InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: Some(current_snapshot(
                        3,
                        vec![current_task("one", "Current task", "agent-a", 5, 5)],
                    )),
                    resources: Vec::new(),
                },
            },
        )));
        model.current_consumed = false;
        model.current_issued = collection_issued;
        model.collect_current_tasks(crate::background::Background::current().now());
        model.poll_saved_direction();

        assert!(matches!(
            model.direction_state(),
            AgentDirectionState::Applied { revision: 5, .. }
        ));
        let AgentTaskChoice::One(frontier) = model.choice() else {
            panic!("the unchanged owner run remains steerable")
        };
        assert_eq!(frontier.run_epoch, 7);
        assert_eq!(frontier.after_revision, 5);
    }

    #[test]
    fn cumulative_ack_confirms_history_without_retargeting_to_a_later_direction() {
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Current task", "agent-a", 4, 4)],
            [(
                "one",
                vec![exact_task_at(2, "one", "Current task", "agent-a", 5, 4)],
            )],
        ));
        let mut model = model(host, "room", "agent-a");
        model.steer("Use the final reviewed wording").unwrap();
        model.poll_direction();

        let collection_issued = model.issue_observation();
        let invocation = InvocationId::new();
        model.current = Some(LocalState::new(RemoteCall::Done(
            InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: Some(current_snapshot(
                        3,
                        vec![current_task("one", "Current task", "agent-a", 6, 6)],
                    )),
                    resources: Vec::new(),
                },
            },
        )));
        model.current_consumed = false;
        model.current_issued = collection_issued;
        model.collect_current_tasks(crate::background::Background::current().now());
        model.poll_saved_direction();

        assert!(matches!(
            model.direction_state(),
            AgentDirectionState::Applied { revision: 5, .. }
        ));
        assert_eq!(model.choice(), AgentTaskChoice::None);
        assert!(model.needs_frontier_confirmation());
        assert_eq!(model.context_task().unwrap().after_revision, 4);
        assert!(!model.can_steer());
    }

    #[test]
    fn explicit_refresh_checks_saved_status_and_accepts_a_new_current_run() {
        let mut replacement = current_task("one", "Replacement run", "agent-a", 0, 0);
        replacement["run_epoch"] = serde_json::json!(8);
        let mut replacement_exact = exact_task_at(3, "one", "Replacement run", "agent-a", 0, 0);
        replacement_exact["run_epoch"] = serde_json::json!(8);
        let host = Arc::new(FrontierHost {
            current: Mutex::new(VecDeque::from([
                current_snapshot(
                    1,
                    vec![current_task("one", "Current task", "agent-a", 4, 4)],
                ),
                current_snapshot(3, vec![replacement]),
            ])),
            reads: Mutex::new(BTreeMap::from([(
                "one".into(),
                VecDeque::from([
                    exact_task_at(2, "one", "Current task", "agent-a", 5, 4),
                    replacement_exact,
                ]),
            )])),
            invocations: Mutex::new(Vec::new()),
            control_page: Mutex::new(None),
        });
        let mut model = model(host.clone(), "room", "agent-a");
        model.steer("Use the final reviewed wording").unwrap();
        model.observe("room", "agent-a");
        assert!(matches!(
            model.direction_state(),
            AgentDirectionState::Saved { revision: 5, .. }
        ));

        model.refresh();

        let AgentTaskChoice::One(frontier) = model.choice() else {
            panic!("explicit refresh accepts the replacement owner run")
        };
        assert_eq!(frontier.run_epoch, 8);
        assert_eq!(frontier.after_revision, 0);
        assert!(matches!(
            model.direction_state(),
            AgentDirectionState::Saved { revision: 5, .. }
        ));
        let actions = host
            .requests()
            .into_iter()
            .map(|request| request.target.address.action)
            .collect::<Vec<_>>();
        assert!(actions.ends_with(&[READ_TASK_ACTION.into(), READ_CURRENT_TASKS_ACTION.into()]));
    }

    #[test]
    fn old_saved_ack_never_retargets_an_explicitly_selected_replacement_run() {
        let mut replacement = current_task("one", "Replacement run", "agent-a", 0, 0);
        replacement["run_epoch"] = serde_json::json!(8);
        let mut replacement_exact = exact_task_at(3, "one", "Replacement run", "agent-a", 0, 0);
        replacement_exact["run_epoch"] = serde_json::json!(8);
        let host = Arc::new(FrontierHost {
            current: Mutex::new(VecDeque::from([
                current_snapshot(
                    1,
                    vec![current_task("one", "Original run", "agent-a", 4, 4)],
                ),
                current_snapshot(3, vec![replacement]),
                current_snapshot(
                    4,
                    vec![current_task("one", "Original run", "agent-a", 5, 5)],
                ),
            ])),
            reads: Mutex::new(BTreeMap::from([(
                "one".into(),
                VecDeque::from([
                    exact_task_at(2, "one", "Original run", "agent-a", 5, 4),
                    replacement_exact,
                ]),
            )])),
            invocations: Mutex::new(Vec::new()),
            control_page: Mutex::new(None),
        });
        let mut model = model(host, "room", "agent-a");
        model.steer("Saved for the original run").unwrap();
        model.observe("room", "agent-a");
        model.refresh();
        let AgentTaskChoice::One(selected) = model.choice() else {
            panic!("explicit refresh selects the replacement run")
        };
        assert_eq!(selected.run_epoch, 8);
        let deadline = model.next_refresh_deadline().expect("bounded refresh");
        model.poll_at(deadline);
        assert_eq!(model.choice(), AgentTaskChoice::None);
        assert_eq!(model.context_task().unwrap().run_epoch, 8);
        assert!(model.needs_frontier_confirmation());
        assert!(matches!(
            model.direction_state(),
            AgentDirectionState::Applied {
                run_epoch: 7,
                revision: 5,
                ..
            }
        ));
        assert!(model.steer("Unsent replacement-run draft").is_err());
    }

    #[test]
    fn older_exact_read_cannot_resurrect_a_task_ended_by_a_newer_collection() {
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Current task", "agent-a", 4, 4)],
            [(
                "one",
                vec![exact_task_at(2, "one", "Current task", "agent-a", 5, 5)],
            )],
        ));
        let mut model = model(host, "room", "agent-a");
        model.steer("Use the final reviewed wording").unwrap();
        model.poll_direction();
        assert!(matches!(
            model.direction_state(),
            AgentDirectionState::Saved { revision: 5, .. }
        ));

        let collection_issued = model.issue_observation();
        let invocation = InvocationId::new();
        model.current = Some(LocalState::new(RemoteCall::Done(
            InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: Some(current_snapshot(3, Vec::new())),
                    resources: Vec::new(),
                },
            },
        )));
        model.current_consumed = false;
        model.current_issued = collection_issued;
        model.collect_current_tasks(crate::background::Background::current().now());
        model.poll_saved_direction();

        assert_eq!(model.choice(), AgentTaskChoice::None);
        assert!(!model.can_steer());
        assert!(model.needs_frontier_confirmation());
        assert!(matches!(
            model.direction_state(),
            AgentDirectionState::Saved { revision: 5, .. }
        ));
        assert_eq!(model.context_task().unwrap().task_id, "one");
        assert!(model.confirmed.is_empty());
    }

    #[test]
    fn provider_shape_exposes_only_complete_pending_work_and_exact_stop_state() {
        let mut stopping = current_task("one", "Review release", "agent-a", 4, 4);
        stopping["cancellation_requested"] = serde_json::json!(true);
        let host = Arc::new(FrontierHost::with_snapshots(vec![
            current_snapshot_with_pending(
                7,
                vec![stopping],
                true,
                vec![
                    pending_task("queued", "Prepare notes", "agent-a", "queued"),
                    pending_task("waiting", "Check tests", "agent-a", "waiting"),
                ],
            ),
            current_snapshot_with_pending(
                8,
                vec![current_task("one", "Review release", "agent-a", 4, 4)],
                false,
                vec![pending_task(
                    "bounded",
                    "Only part of the queue",
                    "agent-a",
                    "queued",
                )],
            ),
        ]));
        let mut model = model(host, "room", "agent-a");

        assert_eq!(model.pending_tasks().len(), 2);
        assert!(model.pending_tasks_complete());
        assert!(!model.can_steer());
        assert_eq!(
            model.stop(),
            Err("this task is already stopping or its state is unavailable".into())
        );

        let deadline = model.next_refresh_deadline().expect("bounded refresh");
        model.poll_at(deadline);
        assert!(!model.pending_tasks_complete());
        assert!(model.pending_tasks().is_empty());
        assert_eq!(model.discovery(), &AgentTaskDiscovery::Ready);
    }

    #[test]
    fn pending_tasks_preserve_owner_execution_order_not_opaque_id_order() {
        let host = Arc::new(FrontierHost::with_snapshots(vec![
            current_snapshot_with_pending(
                1,
                Vec::new(),
                true,
                vec![
                    pending_task("z-first", "First admitted", "agent-a", "queued"),
                    pending_task("a-second", "Second admitted", "agent-a", "queued"),
                ],
            ),
        ]));
        let model = model(host, "room", "agent-a");
        assert!(model.pending_tasks_complete());
        assert_eq!(
            model
                .pending_tasks()
                .iter()
                .map(|task| task.task_id.as_str())
                .collect::<Vec<_>>(),
            vec!["z-first", "a-second"]
        );
    }

    #[test]
    fn queue_and_stop_use_exact_public_task_actions_without_claiming_terminal_stop() {
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Review release", "agent-a", 4, 4)],
            [],
        ));
        let mut model = model(Arc::clone(&host), "room", "agent-a");

        model
            .queue("Prepare the release notes in full", Some("Release notes"))
            .unwrap();
        model.observe("room", "agent-a");
        assert_eq!(
            model.queue_state(),
            &AgentQueueState::Queued {
                task_id: "queued-task".into()
            }
        );

        model.stop().unwrap();
        model.observe("room", "agent-a");
        assert_eq!(
            model.stop_state(),
            &AgentStopState::Stopping {
                task_id: "one".into(),
                run_epoch: 7
            }
        );

        let requests = host.requests();
        let queue = requests
            .iter()
            .find(|request| request.target.address.action == QUEUE_TASK_ACTION)
            .expect("canonical queue action");
        assert_eq!(queue.target.address.resource, TASK_COLLECTION);
        assert_eq!(
            argument_json(&queue.argument).unwrap(),
            &serde_json::Map::from_iter([
                ("agent".into(), serde_json::json!("agent-a")),
                (
                    "instruction".into(),
                    serde_json::json!("Prepare the release notes in full"),
                ),
                ("route".into(), serde_json::json!("model")),
                ("title".into(), serde_json::json!("Release notes")),
            ])
        );
        let stop = requests
            .iter()
            .find(|request| request.target.address.action == STOP_TASK_ACTION)
            .expect("canonical stop action");
        assert_eq!(stop.target.address.resource, "desktop:task:one");
        assert_eq!(argument_json(&stop.argument).unwrap()["run_epoch"], 7);
        assert_eq!(argument_json(&stop.argument).unwrap()["after_revision"], 4);
    }

    #[test]
    fn queue_at_due_refresh_uses_the_exact_existing_offer_once() {
        use crate::collaboration::CollabRequest;
        use tilo_actions::remote::RemoteActionMessage;

        let clock = crate::background::Pumped::new();
        let _entered = clock.background().enter();
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Review release", "agent-a", 4, 4)],
            [],
        ));
        let mut model = model(host, "room", "agent-a");
        assert!(model.can_queue());
        let deadline = model
            .next_refresh_deadline()
            .expect("the existing exact offer has a periodic refresh deadline");
        model.remote = RemoteActions::session();
        clock.advance(
            deadline
                .saturating_duration_since(clock.background().now())
                .saturating_add(Duration::from_millis(1)),
        );

        model
            .queue("Prepare the release notes in full", None)
            .expect("the enabled Queue action uses its existing exact offer");

        let mutations = model
            .take_collab_requests()
            .into_iter()
            .filter_map(|request| match request {
                CollabRequest::RemoteAction {
                    message: RemoteActionMessage::Invoke(request),
                } if request.target.address.action == QUEUE_TASK_ACTION => Some(request),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(mutations.len(), 1);
        let request = &mutations[0];
        assert_eq!(
            request.target,
            task_collection_target("room", QUEUE_TASK_ACTION)
        );
        assert_eq!(
            argument_json(&request.argument).unwrap(),
            &serde_json::Map::from_iter([
                ("agent".into(), serde_json::json!("agent-a")),
                (
                    "instruction".into(),
                    serde_json::json!("Prepare the release notes in full"),
                ),
                ("route".into(), serde_json::json!("model")),
            ])
        );
    }

    #[test]
    fn rejected_queue_fails_once_without_resubmitting() {
        use crate::collaboration::{CollabEvent, CollabRequest};
        use tilo_actions::remote::RemoteActionMessage;

        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Review release", "agent-a", 4, 4)],
            [],
        ));
        let mut model = model(host, "room", "agent-a");
        model.remote = RemoteActions::session();
        let invocation = model
            .queue("Prepare the release notes in full", None)
            .expect("the exact visible Queue offer admits one SDK request");
        assert!(model.queue("Do not submit twice", None).is_err());
        let queue_requests = |requests: Vec<CollabRequest>| {
            requests
                .into_iter()
                .filter(|request| {
                    matches!(
                        request,
                        CollabRequest::RemoteAction {
                            message: RemoteActionMessage::Invoke(request),
                        } if request.target.address.action == QUEUE_TASK_ACTION
                    )
                })
                .count()
        };
        assert_eq!(queue_requests(model.take_collab_requests()), 1);

        model.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Rejected {
                    error: ActionHostError::NotPermitted,
                },
            }),
        });

        assert_eq!(model.queue_state(), &AgentQueueState::Failed);
        assert!(model.queue.is_none());
        assert_eq!(queue_requests(model.take_collab_requests()), 0);
        model.invalidated = true;
        assert!(
            model
                .queue("Do not use an invalidated offer", None)
                .is_err()
        );
        assert_eq!(queue_requests(model.take_collab_requests()), 0);
    }

    fn pending_painted_queue_offer() -> (AgentTaskFrontiers, AgentQueueOffer) {
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Review release", "agent-a", 4, 4)],
            [],
        ));
        let mut model = model(host, "room", "agent-a");
        let offer = model.queue_offer().expect("painted enabled Queue");
        model.remote = RemoteActions::session();
        model.start_current_refresh(false);
        assert!(!model.can_queue());
        assert!(model.queue_offer().is_none());
        (model, offer)
    }

    fn take_queue_mutations(model: &mut AgentTaskFrontiers) -> Vec<InvokeRequest> {
        use crate::collaboration::CollabRequest;
        use tilo_actions::remote::RemoteActionMessage;

        model
            .take_collab_requests()
            .into_iter()
            .filter_map(|request| match request {
                CollabRequest::RemoteAction {
                    message: RemoteActionMessage::Invoke(request),
                } if request.target.address.action == QUEUE_TASK_ACTION => Some(request),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn painted_queue_offer_survives_only_its_exact_pending_refresh_once() {
        use crate::collaboration::CollabEvent;
        use tilo_actions::remote::RemoteActionMessage;

        let host = Arc::new(FrontierHost::new(Vec::new(), []));
        let mut model = model(host, "room", "agent-a");
        let offer = model.queue_offer().unwrap();
        // A second painted button from the same offer must not retry even
        // after the first invocation receives an immediate negative receipt.
        let second = model.queue_offer().unwrap();
        model.remote = RemoteActions::session();
        model.start_current_refresh(false);
        let invocation = model
            .queue_with_offer(offer, "Review the full release notes", None)
            .expect("one painted gesture survives the in-flight owner read");
        assert!(!model.can_queue());
        assert!(model.queue_offer_revalidating.is_none());
        let requests = take_queue_mutations(&mut model);
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].target,
            task_collection_target("room", QUEUE_TASK_ACTION)
        );
        assert_eq!(
            argument_json(&requests[0].argument).unwrap()["agent"],
            "agent-a"
        );

        model.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Rejected {
                    error: ActionHostError::NotPermitted,
                },
            }),
        });
        assert_eq!(model.queue_state(), &AgentQueueState::Failed);
        assert!(
            model
                .queue_with_offer(second, "Do not retry", None)
                .is_err()
        );
        assert!(take_queue_mutations(&mut model).is_empty());
    }

    #[test]
    fn painted_queue_offer_is_consumed_before_a_synchronous_receipt() {
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Review release", "agent-a", 4, 4)],
            [],
        ));
        let mut model = model(Arc::clone(&host), "room", "agent-a");
        let first = model.queue_offer().unwrap();
        let second = model.queue_offer().unwrap();
        model
            .queue_with_offer(first, "Review release notes", None)
            .unwrap();
        assert_eq!(
            model.queue_state(),
            &AgentQueueState::Queued {
                task_id: "queued-task".into()
            }
        );
        assert!(
            model.can_queue(),
            "the fresh owner offer remains independently available"
        );
        assert!(
            model
                .queue_with_offer(second, "Do not queue twice", None)
                .is_err()
        );
        assert_eq!(
            host.requests()
                .iter()
                .filter(|request| request.target.address.action == QUEUE_TASK_ACTION)
                .count(),
            1
        );
    }

    #[test]
    fn painted_queue_offer_rejects_a_replacement_snapshot_or_catalogue_revision() {
        for generation in [1, 2] {
            let host = Arc::new(FrontierHost::with_snapshots(vec![
                current_snapshot(1, Vec::new()),
                current_snapshot(generation, Vec::new()),
            ]));
            let mut model = model(Arc::clone(&host), "room", "agent-a");
            let offer = model.queue_offer().unwrap();
            model.refresh();
            assert!(model.can_queue());
            assert!(
                model
                    .queue_with_offer(offer, "Stale painted snapshot", None)
                    .is_err()
            );
            assert!(
                !host
                    .requests()
                    .iter()
                    .any(|request| request.target.address.action == QUEUE_TASK_ACTION)
            );
        }
        let host = Arc::new(FrontierHost::new(Vec::new(), []));
        let mut model = model(host, "room", "agent-a");
        let offer = model.queue_offer().unwrap();
        let key = (TASK_COLLECTION.into(), QUEUE_TASK_ACTION.into());
        let call = model.controls.get(&key).unwrap();
        let RemoteCall::Done(mut page) = call.get() else {
            panic!("offered catalogue page")
        };
        page.revision.push_str("-replacement");
        call.set(RemoteCall::Done(page));
        assert!(model.can_queue());
        assert!(
            model
                .queue_with_offer(offer, "Stale painted catalogue", None)
                .is_err()
        );
        assert!(take_queue_mutations(&mut model).is_empty());
    }

    #[test]
    fn painted_queue_offer_remains_usable_while_the_exact_offer_is_current() {
        let clock = crate::background::Pumped::new();
        let _entered = clock.background().enter();
        let host = Arc::new(FrontierHost::new(Vec::new(), []));
        let mut model = model(Arc::clone(&host), "room", "agent-a");
        let offer = model.queue_offer().unwrap();
        clock.advance(CONTROL_RESPONSE_WINDOW + Duration::from_millis(1));
        assert!(model.can_queue(), "the exact owner offer is still current");

        model
            .queue_with_offer(offer, "Use the still-current painted offer", None)
            .unwrap();

        assert_eq!(
            host.requests()
                .iter()
                .filter(|request| request.target.address.action == QUEUE_TASK_ACTION)
                .count(),
            1
        );
    }

    #[test]
    fn painted_queue_offer_expires_without_polling_the_pending_read() {
        let clock = crate::background::Pumped::new();
        let _entered = clock.background().enter();
        let (mut model, offer) = pending_painted_queue_offer();
        let expiry = model.draft_revalidation_until.unwrap();
        clock.advance(expiry.saturating_duration_since(clock.background().now()));
        assert!(
            model.queue_draft_revalidating,
            "the grace has not been polled away"
        );
        assert!(
            model
                .queue_with_offer(offer, "Expired painted offer", None)
                .is_err()
        );
        assert!(take_queue_mutations(&mut model).is_empty());
    }

    #[test]
    fn painted_queue_offer_rejects_settled_unpolled_or_invalidated_owner_reads() {
        for malformed in [false, true] {
            let (mut model, offer) = pending_painted_queue_offer();
            model.current = Some(LocalState::new(RemoteCall::Done(
                InvocationUpdate::Succeeded {
                    invocation: InvocationId::new(),
                    output: ActionOutput {
                        value: Some(if malformed {
                            serde_json::json!({"not": "an owner snapshot"})
                        } else {
                            current_snapshot(1, Vec::new())
                        }),
                        resources: Vec::new(),
                    },
                },
            )));
            assert!(!model.current_consumed);
            assert!(
                model
                    .queue_with_offer(offer, "Already settled owner read", None)
                    .is_err()
            );
            assert!(take_queue_mutations(&mut model).is_empty());
        }
        let (mut model, offer) = pending_painted_queue_offer();
        model.current = Some(LocalState::new(RemoteCall::Refused(
            ActionHostError::NotPermitted,
        )));
        model.poll();
        assert_eq!(model.discovery(), &AgentTaskDiscovery::Unavailable);
        assert!(
            model
                .queue_with_offer(offer, "Refused owner read", None)
                .is_err()
        );
        assert!(take_queue_mutations(&mut model).is_empty());
    }

    #[test]
    fn painted_queue_offer_rejects_other_readers_context_aba_and_suspension() {
        let (_, offer) = pending_painted_queue_offer();
        let (mut other, _) = pending_painted_queue_offer();
        assert!(
            other
                .queue_with_offer(offer, "Another reader's offer", None)
                .is_err()
        );
        assert!(take_queue_mutations(&mut other).is_empty());

        let host = Arc::new(FrontierHost::new(Vec::new(), []));
        let mut first = model(host, "room", "agent-a");
        let offer = first.queue_offer().unwrap();
        first.observe("other-room", "agent-b");
        first.observe("room", "agent-a");
        assert!(first.can_queue());
        assert!(
            first
                .queue_with_offer(offer, "Returned to the same labels", None)
                .is_err()
        );

        let (mut model, offer) = pending_painted_queue_offer();
        model.set_observation_active(false);
        assert!(
            model
                .queue_with_offer(offer, "Hidden observation", None)
                .is_err()
        );
        assert!(take_queue_mutations(&mut model).is_empty());
    }

    #[test]
    fn undocumented_provider_fields_and_routes_fail_closed() {
        let mut unknown_field = current_snapshot(
            1,
            vec![current_task("one", "Review release", "agent-a", 4, 4)],
        );
        unknown_field["authority"] = serde_json::json!("claim-from-json");
        let mut unknown_route = current_snapshot(
            1,
            vec![current_task("one", "Review release", "agent-a", 4, 4)],
        );
        unknown_route["tasks"][0]["route"] = serde_json::json!("supervisor");

        for snapshot in [unknown_field, unknown_route] {
            let host = Arc::new(FrontierHost::with_snapshots(vec![snapshot]));
            let model = model(host, "room", "agent-a");
            assert_eq!(model.discovery(), &AgentTaskDiscovery::Unavailable);
            assert_eq!(model.choice(), AgentTaskChoice::None);
            assert!(model.pending_tasks().is_empty());
        }
    }

    #[test]
    fn delayed_collection_cannot_replace_a_newer_exact_task_read() {
        let host = Arc::new(FrontierHost::new(
            vec![current_task("one", "Current task", "agent-a", 4, 4)],
            [(
                "one",
                vec![exact_task_at(2, "one", "Current task", "agent-a", 5, 5)],
            )],
        ));
        let mut model = model(host, "room", "agent-a");
        model.steer("Use the final reviewed wording").unwrap();
        model.observe("room", "agent-a");

        let invocation = InvocationId::new();
        model.current = Some(LocalState::new(RemoteCall::Done(
            InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: Some(current_snapshot(
                        1,
                        vec![current_task("one", "Current task", "agent-a", 4, 4)],
                    )),
                    resources: Vec::new(),
                },
            },
        )));
        model.current_consumed = false;
        model.current_issued = 1;
        model.collect_current_tasks(crate::background::Background::current().now());

        let AgentTaskChoice::One(frontier) = model.choice() else {
            panic!("the newer exact read wins")
        };
        assert_eq!(frontier.after_revision, 5);
        assert_eq!(frontier.applied_revision, 5);
    }

    #[test]
    fn suspended_task_context_is_idle_and_resumes_with_a_fresh_frontier() {
        let clock = crate::background::Pumped::new();
        let _entered = clock.background().enter();
        let host = Arc::new(FrontierHost::with_snapshots(vec![
            current_snapshot(1, vec![current_task("one", "Original", "agent-a", 4, 4)]),
            current_snapshot(2, vec![current_task("one", "Original", "agent-a", 5, 4)]),
        ]));
        let mut model = model(host.clone(), "room", "agent-a");
        assert!(matches!(model.choice(), AgentTaskChoice::One(_)));
        let original = model.context_task().expect("original observed context");
        model.set_observation_active(false);
        let requests = host.requests().len();
        assert_eq!(model.next_wake_deadline(), None);
        assert!(!model.can_steer());
        for _ in 0..5 {
            clock.advance(std::time::Duration::from_secs(10));
            model.observe("room", "agent-a");
            model.refresh();
            assert_eq!(host.requests().len(), requests);
            assert_eq!(model.next_wake_deadline(), None);
        }
        assert_eq!(model.context_task(), Some(original));
        model.set_observation_active(true);
        model.observe("room", "agent-a");
        assert_eq!(host.requests().len(), requests + 1);
        assert!(model.needs_frontier_confirmation());
        assert!(
            !model.can_steer(),
            "resuming never silently retargets an old draft"
        );
    }

    #[test]
    fn suspended_task_context_settles_exact_queue_without_collection_polling() {
        use crate::collaboration::CollabEvent;
        use tilo_actions::remote::RemoteActionMessage;

        let remote = RemoteActions::session();
        let mut model = AgentTaskFrontiers::with_actions(remote.clone());
        model.workspace = "room".into();
        model.agent = "agent-a".into();
        let invocation = InvocationId::new();
        model.queue = Some(QueueCall {
            invocation,
            call: remote.invoke_with_id(
                invocation,
                task_collection_target("room", QUEUE_TASK_ACTION),
                InvokeArgument::None,
                None,
            ),
        });
        model.queue_state = AgentQueueState::Submitting;
        let requests = model.take_collab_requests();
        assert_eq!(requests.len(), 1);
        model.set_observation_active(false);
        assert_eq!(model.queue.as_ref().unwrap().invocation, invocation);
        assert!(model.next_wake_deadline().is_some());
        model.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::Update {
                invocation,
                state: None,
            },
        });
        assert_eq!(model.queue_state(), &AgentQueueState::Uncertain);
        assert_eq!(model.queue.as_ref().unwrap().invocation, invocation);
        assert_eq!(model.next_wake_deadline(), None);
        assert!(model.take_collab_requests().is_empty());
        model.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: Some(
                            serde_json::json!({"result": {"queued": {"task_id": "same-original"}}}),
                        ),
                        resources: Vec::new(),
                    },
                },
            }),
        });
        assert_eq!(
            model.queue_state(),
            &AgentQueueState::Queued {
                task_id: "same-original".into()
            }
        );
        assert!(model.queue.is_none());
        assert_eq!(model.next_wake_deadline(), None);
        assert!(
            model.take_collab_requests().is_empty(),
            "settlement must not start hidden collection refresh or another effect"
        );
    }

    #[test]
    fn missing_control_catalogue_response_stops_waking_after_its_bound() {
        let clock = crate::background::Pumped::new();
        let _entered = clock.background().enter();
        let mut model = AgentTaskFrontiers::with_actions(RemoteActions::session());
        model.workspace = "room".into();
        model.agent = "agent-a".into();
        model.current_consumed = true;
        model.snapshot_complete = true;
        model.invalidated = false;

        model.refresh_control_offers();
        let wake = model
            .next_wake_deadline()
            .expect("pending control response schedules a bounded native pump");
        assert!(wake <= clock.background().now() + ASYNC_CONTROL_POLL);
        assert_eq!(model.take_collab_requests().len(), 1);

        clock.advance(CONTROL_RESPONSE_WINDOW);
        model.poll();

        assert_eq!(model.control_response_until, None);
        assert_eq!(model.remote.next_observation_deadline(), None);
        assert_eq!(model.next_refresh, None);
        assert_eq!(model.next_wake_deadline(), None);
        assert!(model.take_collab_requests().is_empty());
        assert!(!model.can_queue());
    }

    #[test]
    fn missing_task_receipts_become_uncertain_without_resubmitting_effects() {
        use crate::collaboration::CollabEvent;
        use tilo_actions::remote::RemoteActionMessage;

        let remote = RemoteActions::session();
        let mut model = AgentTaskFrontiers::with_actions(remote.clone());
        let frontier = AgentTaskFrontier {
            task_id: "original-task".into(),
            title: "Original task".into(),
            run_epoch: 7,
            after_revision: 4,
            applied_revision: 4,
            route: "model".into(),
            cancellation_requested: false,
            execution: None,
        };
        let direction_id = InvocationId(uuid::Uuid::from_bytes([0x71; 16]));
        let queue_id = InvocationId(uuid::Uuid::from_bytes([0x72; 16]));
        let stop_id = InvocationId(uuid::Uuid::from_bytes([0x73; 16]));
        model.direction = Some(DirectionCall {
            invocation: direction_id,
            call: remote.invoke_with_id(
                direction_id,
                task_target("room", "original-task", DIRECT_TASK_ACTION),
                InvokeArgument::None,
                None,
            ),
            frontier: frontier.clone(),
        });
        model.direction_state = AgentDirectionState::Submitting;
        model.queue = Some(QueueCall {
            invocation: queue_id,
            call: remote.invoke_with_id(
                queue_id,
                task_collection_target("room", QUEUE_TASK_ACTION),
                InvokeArgument::None,
                None,
            ),
        });
        model.queue_state = AgentQueueState::Submitting;
        model.stop = Some(StopCall {
            invocation: stop_id,
            call: remote.invoke_with_id(
                stop_id,
                task_target("room", "original-task", STOP_TASK_ACTION),
                InvokeArgument::None,
                None,
            ),
            frontier,
        });
        model.stop_state = AgentStopState::Submitting {
            task_id: "original-task".into(),
            run_epoch: 7,
        };
        assert_eq!(model.take_collab_requests().len(), 3);

        for invocation in [direction_id, queue_id, stop_id] {
            model.handle_collab_event(&CollabEvent::RemoteAction {
                message: RemoteActionMessage::Update {
                    invocation,
                    state: None,
                },
            });
        }

        assert!(matches!(
            model.direction_state(),
            AgentDirectionState::Uncertain {
                task_id,
                run_epoch: 7
            } if task_id == "original-task"
        ));
        assert_eq!(model.queue_state(), &AgentQueueState::Uncertain);
        assert!(matches!(
            model.stop_state(),
            AgentStopState::Uncertain {
                task_id,
                run_epoch: 7
            } if task_id == "original-task"
        ));
        assert!(model.take_collab_requests().is_empty());
        assert!(model.direction.is_some() && model.queue.is_some() && model.stop.is_some());
        assert_eq!(model.next_wake_deadline(), None);

        model.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::Update {
                invocation: queue_id,
                state: Some(InvocationState::new(
                    queue_id,
                    InvocationUpdate::Succeeded {
                        invocation: queue_id,
                        output: ActionOutput {
                            value: Some(serde_json::json!({
                                "result": {"queued": {"task_id": "late-original"}}
                            })),
                            resources: Vec::new(),
                        },
                    },
                )),
            },
        });
        assert_eq!(
            model.queue_state(),
            &AgentQueueState::Queued {
                task_id: "late-original".into()
            }
        );
        assert!(model.queue.is_none());
    }
}
