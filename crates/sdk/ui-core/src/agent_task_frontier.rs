//! Authenticated Task Center read/selection/steering for agent controls.
//!
//! The model only derives a steerable task from owner-published `read-task`
//! results. Agent labels, presence text, cursors and elapsed time are never
//! treated as task identity or progress.

use std::collections::BTreeMap;

use serde::Deserialize;
use tilo_actions::remote::{InvokeArgument, RemoteActionTarget, workspace_provider};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, InvocationUpdate};

use crate::LocalState;
use crate::remote_actions::{InvocationCall, RemoteActions, RemoteCall};
use crate::time::{Duration, Instant};
use crate::widget::Collaborative;

const TASK_RESOURCE_PREFIX: &str = "desktop:task:";
const TASK_COLLECTION: &str = "desktop:tasks";
const READ_CURRENT_TASKS_ACTION: &str = "read-current-tasks";
const READ_TASK_ACTION: &str = "read-task";
const DIRECT_TASK_ACTION: &str = "direct";
const TASK_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentTaskFrontier {
    pub task_id: String,
    pub title: String,
    pub run_epoch: u64,
    pub after_revision: u64,
    pub applied_revision: u64,
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentTasksRead {
    generation: u64,
    complete: bool,
    total: u64,
    tasks: Vec<CurrentTaskRead>,
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
}

struct DirectionCall {
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

/// One shared Task Center reader for Chat and marker-adjacent controls.
pub struct AgentTaskFrontiers {
    remote: RemoteActions,
    workspace: String,
    agent: String,
    current: Option<LocalState<InvocationCall>>,
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
    selected: Option<String>,
    /// Last exact owner-backed task shown in the steering context. Retained
    /// across an invalidation so the UI can say which task changed without
    /// continuing to treat it as steerable.
    context: Option<AgentTaskFrontier>,
    direction: Option<DirectionCall>,
    saved_direction: Option<SavedDirection>,
    direction_state: AgentDirectionState,
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
            workspace: String::new(),
            agent: String::new(),
            current: None,
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
            selected: None,
            context: None,
            direction: None,
            saved_direction: None,
            direction_state: AgentDirectionState::Draft,
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
        self.current = None;
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
        self.selected = None;
        self.context = None;
        self.direction = None;
        self.saved_direction = None;
        self.direction_state = AgentDirectionState::Draft;
        self.discovery = AgentTaskDiscovery::Loading;
        self.invalidated = false;
        self.stale_frontier = false;
        self.start_current_refresh(false);
        self.poll();
    }

    /// Explicitly acknowledge a changed owner frontier and re-read it.
    ///
    /// Private draft text is owned by the caller and is never cleared here.
    /// An automatic refresh never retargets that draft to a replacement run
    /// or revision; this explicit operation permits the next complete owner
    /// snapshot to become its new steering context.
    pub fn refresh(&mut self) {
        if self.workspace.is_empty() || self.agent.is_empty() {
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
    /// run and no direction is already awaiting its receipt.
    #[must_use]
    pub fn can_steer(&self) -> bool {
        matches!(
            self.choice(),
            AgentTaskChoice::One(_) | AgentTaskChoice::Selected(_)
        ) && !matches!(self.direction_state, AgentDirectionState::Submitting)
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
        self.direction = Some(DirectionCall { call, frontier });
        self.direction_state = AgentDirectionState::Submitting;
        Ok(invocation)
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
        if self.current_consumed && self.next_refresh.is_some_and(|deadline| deadline <= now) {
            self.start_current_refresh(false);
        }
        self.collect_current_tasks(now);
        self.poll_direction();
        self.poll_saved_direction();
    }

    fn start_current_refresh(&mut self, accepts_frontier_change: bool) {
        if self.workspace.is_empty() || self.agent.is_empty() || !self.current_consumed {
            return;
        }
        self.current_issued = self.issue_observation();
        self.current_accepts_frontier_change = accepts_frontier_change;
        self.current = Some(self.remote.invoke(
            task_collection_target(&self.workspace),
            InvokeArgument::Json(serde_json::Map::from_iter([(
                "agent".into(),
                serde_json::json!(self.agent),
            )])),
            None,
        ));
        self.current_consumed = false;
        self.next_refresh = None;
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
        let Ok(total) = usize::try_from(read.total) else {
            self.invalidate_snapshot(AgentTaskDiscovery::Unavailable);
            return;
        };
        if !read.complete {
            self.invalidate_snapshot(AgentTaskDiscovery::Incomplete { total: read.total });
            return;
        }
        if read.generation == 0 || total != read.tasks.len() {
            self.invalidate_snapshot(AgentTaskDiscovery::Unavailable);
            return;
        }
        if (read.generation, self.current_issued)
            < (self.snapshot_generation, self.snapshot_observed)
        {
            return;
        }
        let mut frontiers = Vec::with_capacity(read.tasks.len());
        for task in read.tasks {
            if task.task_id.is_empty()
                || task.title.trim().is_empty()
                || task.status != "in_progress"
                || task.assigned_agent != self.agent
                || task.active_agent != self.agent
                || task.run_epoch == 0
                || task.applied_revision > task.after_revision
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
        self.invalidated = true;
        self.snapshot_complete = false;
        self.frontiers.clear();
        self.discovery = discovery;
    }

    fn poll_direction(&mut self) {
        let Some(direction) = &mut self.direction else {
            return;
        };
        match direction.call.get() {
            RemoteCall::Pending
            | RemoteCall::Done(
                InvocationUpdate::Accepted { .. }
                | InvocationUpdate::Progress { .. }
                | InvocationUpdate::Item { .. },
            ) => {}
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

fn task_collection_target(workspace: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: workspace_provider(workspace),
        address: ActionAddress::new(TASK_COLLECTION, READ_CURRENT_TASKS_ACTION),
    }
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
        || read.route.is_empty()
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
    Ok(Some((
        read.generation,
        AgentTaskFrontier {
            task_id: read.task_id,
            title: read.title,
            run_epoch,
            after_revision: read.after_revision,
            applied_revision,
        },
    )))
}

fn direction_revision(output: &ActionOutput, expected_task: &str) -> Option<u64> {
    let result = output.value.as_ref()?.get("result")?;
    let direction = result.get("direction_added")?;
    (direction.get("task_id")?.as_str()? == expected_task)
        .then(|| direction.get("revision")?.as_u64())?
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
            }
        }

        fn with_snapshots(snapshots: Vec<serde_json::Value>) -> Self {
            Self {
                current: Mutex::new(snapshots.into()),
                reads: Mutex::new(BTreeMap::new()),
                invocations: Mutex::new(Vec::new()),
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
                })])),
                reads: Mutex::new(BTreeMap::new()),
                invocations: Mutex::new(Vec::new()),
            }
        }
    }

    impl ActionHost for FrontierHost {
        fn query(
            &self,
            _caller: &Caller,
            _query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionHostError>> {
            Pending::ready(Ok(CataloguePage {
                revision: "task-frontier-1".into(),
                rows: Vec::new(),
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
            "applied_revision": applied_revision
        })
    }

    fn current_snapshot(generation: u64, tasks: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "generation": generation,
            "complete": true,
            "total": tasks.len(),
            "tasks": tasks,
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
        object.insert("route".into(), serde_json::json!("workspace"));
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
        for malformed in [zero_epoch, applied_ahead] {
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
}
