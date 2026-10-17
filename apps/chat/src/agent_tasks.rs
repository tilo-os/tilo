//! Authenticated task/run projection behind Chat's Agent navigation.
//!
//! This is deliberately a thin client of the public action door. Chat owns
//! the private editor and presentation; Task Center owns every task/run fact
//! and revalidates the caller, room, run epoch and revision on invocation.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Map, Value};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{CatalogueQuery, InvokeArgument, RemoteActionTarget};
use ui::LocalState;
use ui::remote_actions::{CatalogueCall, InvocationCall, RemoteActions, RemoteCall};

const TASKS_RESOURCE: &str = "desktop:tasks";
const READ_CURRENT_TASKS: &str = "read-current-tasks";
const QUEUE: &str = "queue";
const DIRECT: &str = "direct";
const STOP: &str = "stop";
const TASKS_PER_PAGE: usize = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AgentComposerIntent {
    #[default]
    Queue,
    Steer,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct AgentTaskFrontier {
    pub(crate) task_id: String,
    pub(crate) title: String,
    pub(crate) assigned_agent: String,
    pub(crate) run_epoch: Option<u64>,
    pub(crate) active_agent: Option<String>,
    pub(crate) after_revision: Option<u64>,
    pub(crate) applied_revision: Option<u64>,
    pub(crate) execution: Option<AgentTaskExecution>,
    #[serde(default)]
    pub(crate) status: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct AgentTaskExecution {
    pub(crate) phase: String,
}

impl AgentTaskFrontier {
    pub(crate) fn execution_phase(&self) -> Option<&str> {
        self.execution
            .as_ref()
            .map(|execution| execution.phase.as_str())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
struct AgentTaskSnapshot {
    #[serde(default)]
    complete: bool,
    #[serde(default)]
    total: usize,
    #[serde(default)]
    tasks: Vec<AgentTaskFrontier>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum AgentTaskReceipt {
    #[default]
    None,
    Saving,
    Saved {
        task_id: String,
        run_epoch: u64,
        revision: u64,
    },
    Applied,
    StopRequested,
    Queued,
    Started,
    Refused,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct AgentTaskView {
    pub(crate) loading: bool,
    pub(crate) complete: bool,
    pub(crate) tasks: Vec<AgentTaskFrontier>,
    pub(crate) receipt: AgentTaskReceipt,
    selected: Option<(String, u64)>,
    page: usize,
    refresh_at: Option<ui::time::Instant>,
    follow_task: Option<String>,
    awaiting_first_run: bool,
    turns: Vec<AgentTaskTurn>,
}

#[derive(Clone, Debug, Deserialize)]
struct AgentTaskTurn {
    task_id: String,
    run_epoch: u64,
    revision: u64,
    text: String,
    #[serde(default)]
    received_at: i64,
    #[serde(default)]
    presentation: Option<TaskTurnPresentation>,
    #[serde(default)]
    author: String,
    #[serde(default)]
    kind: String,
}

/// Wire-compatible with the shared, provider-neutral task snapshot. This is
/// presentation only: neither Chat nor replay can dispatch anything from it.
#[derive(Clone, Debug, Deserialize)]
struct TaskTurnPresentation {
    text: String,
    #[serde(flatten)]
    stream: crate::agent_stream::AgentStream,
}

impl TaskTurnPresentation {
    fn valid(&self) -> bool {
        !self.stream.run.is_empty()
            && self.stream.run.len() <= 192
            && !self.stream.run.chars().any(char::is_control)
            && self.stream.progress.is_empty()
            && self.text.len() <= 32 * 1024
            && self.stream.reasoning.len() <= 16 * 1024
            && self.stream.detail.len() <= 2 * 1024
    }
}

impl AgentTaskView {
    pub(crate) fn conversation_messages(
        &self,
        channel: &str,
        author: &str,
    ) -> Vec<crate::model::Message> {
        let Some(task) = self.current() else {
            return Vec::new();
        };
        let mut turns = self
            .turns
            .iter()
            .filter(|turn| {
                turn.task_id == task.task_id
                    && (turn.run_epoch == 0 || Some(turn.run_epoch) == task.run_epoch)
            })
            .collect::<Vec<_>>();
        // The public read groups directions and outputs separately. The
        // conversation follows when each turn happened, not its storage lane.
        turns.sort_by_key(|turn| turn.received_at);
        turns
            .into_iter()
            .map(|turn| {
                let id = ui::tilo_resource::Id::from_raw(match &turn.presentation {
                    Some(presentation) => format!(
                        "task-reply:{}:{}:{}",
                        turn.task_id, turn.run_epoch, presentation.stream.run
                    ),
                    None => format!(
                        "task-turn:{}:{}:{}:{}",
                        turn.task_id, turn.run_epoch, turn.revision, turn.kind
                    ),
                });
                let message = crate::record::Message {
                    channel: ui::tilo_resource::Id::from_raw(channel),
                    author: ui::tilo_resource::Atom(if turn.author.is_empty() {
                        author.to_owned()
                    } else {
                        turn.author.clone()
                    }),
                    text: if let Some(presentation) = &turn.presentation {
                        presentation.text.clone()
                    } else if turn.kind == "agent-output" {
                        crate::i18n::t_args(
                            "chat-agent-task-output",
                            &[("text", turn.text.clone().into())],
                        )
                    } else {
                        turn.text.clone()
                    },
                    ts: turn.received_at,
                    ..crate::record::Message::default()
                };
                let mut row = crate::projection::message_row(&id, &message);
                if let Some(presentation) = &turn.presentation {
                    use crate::agent_stream::AgentFinish;
                    use crate::model::{AgentWork, AgentWorkState};
                    row.work = Some(AgentWork {
                        stream: Some(presentation.stream.clone()),
                        state: match presentation.stream.finish {
                            None => AgentWorkState::Taken,
                            Some(AgentFinish::Complete) => {
                                AgentWorkState::Done { did: String::new() }
                            }
                            Some(_) => AgentWorkState::Refused,
                        },
                        since: turn.received_at,
                        reasoning: None,
                        trace: Vec::new(),
                    });
                }
                row
            })
            .collect()
    }
    pub(crate) fn draft_route(&self) -> Option<String> {
        let (task, epoch) = self.selected.as_ref()?;
        Some(format!("task:{task}:{epoch}"))
    }

    pub(crate) fn selected_identity(&self) -> Option<(&str, u64)> {
        self.selected
            .as_ref()
            .map(|(task, epoch)| (task.as_str(), *epoch))
    }

    pub(crate) fn current(&self) -> Option<&AgentTaskFrontier> {
        let (task_id, run_epoch) = self.selected.as_ref()?;
        self.tasks
            .iter()
            .find(|task| task.task_id == *task_id && task.run_epoch == Some(*run_epoch))
    }

    pub(crate) fn can_steer(&self) -> bool {
        self.complete
            && !self.loading
            && self.current().is_some_and(|task| {
                task.run_epoch.is_some()
                    && task.after_revision.is_some()
                    && task.active_agent.as_deref() == Some(task.assigned_agent.as_str())
            })
    }

    pub(crate) fn can_start(&self) -> bool {
        self.complete
            && !self.loading
            && self.current().is_some_and(|task| {
                task.status == "queued"
                    && task.active_agent.is_none()
                    && task.run_epoch.is_some()
                    && task.after_revision.is_some()
            })
    }

    fn retain_turn(&mut self, mut turn: AgentTaskTurn) {
        if turn.presentation.as_ref().is_some_and(|p| !p.valid()) {
            return;
        }
        let same = |known: &AgentTaskTurn| {
            known.task_id == turn.task_id
                && known.run_epoch == turn.run_epoch
                && match (&known.presentation, &turn.presentation) {
                    (Some(a), Some(b)) => a.stream.run == b.stream.run,
                    (None, None) => known.revision == turn.revision && known.kind == turn.kind,
                    _ => false,
                }
        };
        let previous = self.turns.iter().find(|known| same(known));
        if let Some(previous) = previous
            && let (Some(a), Some(b)) = (&previous.presentation, &turn.presentation)
            && (a.stream.sequence >= b.stream.sequence || a.stream.finish.is_some())
        {
            return;
        }
        let received_at = previous.map(|p| p.received_at);
        self.turns.retain(|known| !same(known));
        if turn.presentation.is_some()
            && let Some(at) = received_at
        {
            turn.received_at = at;
        }
        self.turns.push(turn);
    }

    pub(crate) fn can_stop(&self) -> bool {
        self.can_steer()
    }

    pub(crate) fn visible_tasks(&self) -> &[AgentTaskFrontier] {
        let start = self
            .page
            .saturating_mul(TASKS_PER_PAGE)
            .min(self.tasks.len());
        let end = start.saturating_add(TASKS_PER_PAGE).min(self.tasks.len());
        self.tasks.get(start..end).unwrap_or_default()
    }

    pub(crate) fn remaining_after_page(&self) -> usize {
        let shown = self.page.saturating_add(1).saturating_mul(TASKS_PER_PAGE);
        self.tasks.len().saturating_sub(shown)
    }

    fn apply_snapshot(&mut self, snapshot: AgentTaskSnapshot) {
        self.loading = false;
        self.complete = snapshot.complete && snapshot.total == snapshot.tasks.len();
        self.tasks = snapshot.tasks;
        self.refresh_at = Some(ui::time::now() + ui::time::Duration::from_secs(2));
        if self.page.saturating_mul(TASKS_PER_PAGE) >= self.tasks.len() {
            self.page = 0;
        }
        if self.awaiting_first_run
            && let Some(task) = self
                .tasks
                .iter()
                .find(|task| Some(task.task_id.as_str()) == self.follow_task.as_deref())
            && let Some(epoch) = task.run_epoch
        {
            self.selected = Some((task.task_id.clone(), epoch));
            if epoch > 0 {
                self.awaiting_first_run = false;
            }
        }
        // Selection names a run, not its changing instruction revision. Never
        // silently retarget an open conversation to a replacement run.
        if self.selected.is_none()
            && self.follow_task.is_none()
            && self.complete
            && self.tasks.len() == 1
            && let Some(task) = self.tasks.first()
            && let Some(epoch) = task.run_epoch
        {
            self.selected = Some((task.task_id.clone(), epoch));
        }
        let applied = matches!(&self.receipt,
            AgentTaskReceipt::Saved {
                task_id,
                run_epoch,
                revision,
            } if self.tasks.iter().any(|task| {
                task.task_id == *task_id
                    && task.run_epoch == Some(*run_epoch)
                    && task
                        .applied_revision
                        .is_some_and(|applied| applied >= *revision)
            })
        );
        if applied {
            self.receipt = AgentTaskReceipt::Applied;
        }
    }
}

#[derive(Clone)]
enum PendingAnswer {
    Catalogue(LocalState<CatalogueCall>),
    Invocation(LocalState<InvocationCall>),
}

#[derive(Clone)]
enum Purpose {
    Refresh {
        agent: String,
    },
    ReadTask {
        agent: String,
    },
    Conversation {
        agent: String,
        task_id: String,
        run_epoch: u64,
    },
    Start {
        agent: String,
        task_id: String,
        expected_run_epoch: u64,
    },
    Direct {
        text: String,
        draft_route: String,
        agent: String,
        task_id: String,
        run_epoch: u64,
        draft_revision: u64,
    },
    Stop {
        agent: String,
        task_id: String,
    },
    Queue {
        instruction: String,
        agent: String,
        draft_revision: u64,
    },
}

struct Pending {
    answer: PendingAnswer,
    purpose: Purpose,
    arguments: Map<String, Value>,
}

pub(crate) struct AgentTaskClient {
    actions: RemoteActions,
    workspace: String,
    views: HashMap<String, AgentTaskView>,
    pending: Vec<Pending>,
    cleared_drafts: Vec<(String, u64)>,
}

impl AgentTaskClient {
    pub(crate) fn new() -> Self {
        Self {
            actions: RemoteActions::current(),
            workspace: String::new(),
            views: HashMap::new(),
            pending: Vec::new(),
            cleared_drafts: Vec::new(),
        }
    }

    /// Capture-only fixture, called by the framework's explicit shot scenario.
    pub(crate) fn stage_capture(&mut self, workspace: &str, agent: &str) -> &mut AgentTaskView {
        workspace.clone_into(&mut self.workspace);
        self.pending.clear();
        let mut view = AgentTaskView::default();
        view.apply_snapshot(AgentTaskSnapshot {
            complete: true,
            total: 1,
            tasks: vec![AgentTaskFrontier {
                task_id: "task:review".into(),
                title: "Example: Review search actions".into(),
                assigned_agent: agent.into(),
                run_epoch: Some(7),
                active_agent: Some(agent.into()),
                after_revision: Some(6),
                applied_revision: Some(1),
                execution: Some(AgentTaskExecution {
                    phase: "executing".into(),
                }),
                status: "in_progress".into(),
            }],
        });
        view.follow_task = Some("task:review".into());
        view.refresh_at = None;
        view.receipt = AgentTaskReceipt::Saved {
            task_id: "task:review".into(),
            run_epoch: 7,
            revision: 6,
        };
        view.turns.push(AgentTaskTurn {
            task_id: "task:review".into(),
            run_epoch: 0,
            revision: 0,
            text: "Review search actions. Keep the existing open action and show me what you find."
                .into(),
            received_at: crate::demo::NOW,
            presentation: None,
            author: String::new(),
            kind: String::new(),
        });
        // These are deterministic public turns, not a model session or hidden
        // reasoning. Their shape is the read-conversation wire contract.
        for (revision, seconds, text) in [
            (
                3,
                30,
                "Example activity: reading the published search actions. No changes requested.",
            ),
            (
                4,
                60,
                "Example action output: read-content returned the search contract. The existing open action accepts a result anchor.",
            ),
            (
                5,
                90,
                "Example resource output: search contract notes. Next: check the compact conversation layout.",
            ),
        ] {
            view.turns.push(AgentTaskTurn {
                task_id: "task:review".into(),
                run_epoch: 7,
                revision,
                text: text.into(),
                received_at: crate::demo::NOW + seconds,
                presentation: None,
                author: format!("agent:{agent}"),
                kind: "agent-output".into(),
            });
        }
        view.turns.push(AgentTaskTurn {
            task_id: "task:review".into(),
            run_epoch: 7,
            revision: 6,
            text: "Please check the narrow layout too, before making changes.".into(),
            received_at: crate::demo::NOW + 120,
            presentation: None,
            author: String::new(),
            kind: String::new(),
        });
        self.views
            .entry(agent.to_owned())
            .insert_entry(view)
            .into_mut()
    }

    /// A saved task awaiting explicit approval; only used by shot scenarios.
    pub(crate) fn stage_queued_capture(&mut self, workspace: &str, agent: &str) {
        let view = self.stage_capture(workspace, agent);
        for task in &mut view.tasks {
            task.run_epoch = Some(0);
            task.active_agent = None;
            task.execution = None;
            task.after_revision = Some(0);
            task.applied_revision = None;
            task.status = "queued".into();
            view.selected = Some((task.task_id.clone(), 0));
        }
        view.receipt = AgentTaskReceipt::Queued;
        view.turns.truncate(1);
        for turn in &mut view.turns {
            turn.run_epoch = 0;
            turn.revision = 0;
        }
    }

    pub(crate) fn actions_mut(&mut self) -> &mut RemoteActions {
        &mut self.actions
    }

    pub(crate) fn observe(&mut self, workspace: &str, agent: &str) {
        if self.workspace != workspace {
            workspace.clone_into(&mut self.workspace);
            self.views.clear();
            self.pending.clear();
            self.cleared_drafts.clear();
        }
        let view = self.views.entry(agent.to_owned()).or_default();
        if view
            .refresh_at
            .is_some_and(|deadline| ui::time::now() < deadline)
        {
            return;
        }
        if view
            .refresh_at
            .is_some_and(|deadline| ui::time::now() >= deadline)
        {
            view.complete = false;
            view.refresh_at = None;
        }
        if !view.loading && !view.complete {
            view.loading = true;
            self.resolve(
                TASKS_RESOURCE,
                READ_CURRENT_TASKS,
                Purpose::Refresh {
                    agent: agent.to_owned(),
                },
                Map::from_iter([("agent".into(), Value::String(agent.to_owned()))]),
            );
        }
    }

    pub(crate) fn view(&self, agent: &str) -> Option<&AgentTaskView> {
        self.views.get(agent)
    }

    pub(crate) fn follow(
        &mut self,
        workspace: &str,
        agent: &str,
        task_id: &str,
        run_epoch: u64,
    ) -> bool {
        if task_resource(task_id).is_none() {
            return false;
        }
        self.observe(workspace, agent);
        let view = self.views.entry(agent.to_owned()).or_default();
        view.selected = Some((task_id.to_owned(), run_epoch));
        view.follow_task = Some(task_id.to_owned());
        view.awaiting_first_run = false;
        view.receipt = AgentTaskReceipt::None;
        view.complete = false;
        view.refresh_at = None;
        // Any pending list read is harmless; the next observation reads the
        // explicitly selected task rather than choosing from its agent.
        view.loading = false;
        self.observe(workspace, agent);
        true
    }

    pub(crate) fn select(&mut self, agent: &str, frontier: &AgentTaskFrontier) -> bool {
        let (Some(run_epoch), Some(after_revision)) = (frontier.run_epoch, frontier.after_revision)
        else {
            return false;
        };
        let Some(view) = self.views.get_mut(agent) else {
            return false;
        };
        let exact = view.tasks.iter().any(|task| {
            task.task_id == frontier.task_id
                && task.run_epoch == Some(run_epoch)
                && task.after_revision == Some(after_revision)
        });
        if exact {
            if view.selected.as_ref() != Some(&(frontier.task_id.clone(), run_epoch)) {
                view.receipt = AgentTaskReceipt::None;
            }
            view.selected = Some((frontier.task_id.clone(), run_epoch));
            view.follow_task = Some(frontier.task_id.clone());
            view.awaiting_first_run = false;
        }
        if exact {
            self.read_selected_conversation(agent);
        }
        exact
    }

    pub(crate) fn next_page(&mut self, agent: &str) -> bool {
        let Some(view) = self.views.get_mut(agent) else {
            return false;
        };
        if view.tasks.len() <= TASKS_PER_PAGE {
            return false;
        }
        let pages = view
            .tasks
            .len()
            .saturating_add(TASKS_PER_PAGE.saturating_sub(1))
            / TASKS_PER_PAGE;
        view.page = view.page.saturating_add(1) % pages;
        true
    }

    pub(crate) fn direct(&mut self, agent: &str, text: String, draft_revision: u64) -> bool {
        if text.trim().is_empty()
            || self.submission_pending(agent)
            || !self.views.get(agent).is_some_and(AgentTaskView::can_steer)
        {
            return false;
        }
        let Some(task) = self.views.get(agent).and_then(AgentTaskView::current) else {
            return false;
        };
        let (Some(run_epoch), Some(after_revision)) = (task.run_epoch, task.after_revision) else {
            return false;
        };
        let task_id = task.task_id.clone();
        let Some(resource) = task_resource(&task_id) else {
            return false;
        };
        let draft_route = format!("task:{task_id}:{run_epoch}");
        let arguments = Map::from_iter([
            ("run_epoch".into(), Value::from(run_epoch)),
            ("after_revision".into(), Value::from(after_revision)),
            ("text".into(), Value::String(text.clone())),
        ]);
        self.resolve(
            &resource,
            DIRECT,
            Purpose::Direct {
                text,
                draft_route,
                agent: agent.to_owned(),
                task_id,
                run_epoch,
                draft_revision,
            },
            arguments,
        );
        if let Some(view) = self.views.get_mut(agent) {
            view.receipt = AgentTaskReceipt::Saving;
        }
        true
    }

    pub(crate) fn start(&mut self, agent: &str) -> bool {
        if self.submission_pending(agent)
            || !self.views.get(agent).is_some_and(AgentTaskView::can_start)
        {
            return false;
        }
        let Some(task) = self.views.get(agent).and_then(AgentTaskView::current) else {
            return false;
        };
        let (Some(epoch), Some(revision)) = (task.run_epoch, task.after_revision) else {
            return false;
        };
        let task_id = task.task_id.clone();
        let Some(resource) = task_resource(&task_id) else {
            return false;
        };
        self.resolve(
            &resource,
            "approve-run",
            Purpose::Start {
                agent: agent.to_owned(),
                task_id,
                expected_run_epoch: epoch,
            },
            Map::from_iter([
                ("agent_id".into(), Value::String(agent.to_owned())),
                ("expected_run_epoch".into(), Value::from(epoch)),
                ("after_revision".into(), Value::from(revision)),
            ]),
        );
        true
    }

    pub(crate) fn stop(&mut self, agent: &str) -> bool {
        if self.submission_pending(agent)
            || !self.views.get(agent).is_some_and(AgentTaskView::can_stop)
        {
            return false;
        }
        let Some(task) = self.views.get(agent).and_then(AgentTaskView::current) else {
            return false;
        };
        let (Some(run_epoch), Some(after_revision)) = (task.run_epoch, task.after_revision) else {
            return false;
        };
        let task_id = task.task_id.clone();
        let Some(resource) = task_resource(&task_id) else {
            return false;
        };
        self.resolve(
            &resource,
            STOP,
            Purpose::Stop {
                agent: agent.to_owned(),
                task_id,
            },
            Map::from_iter([
                ("run_epoch".into(), Value::from(run_epoch)),
                ("after_revision".into(), Value::from(after_revision)),
            ]),
        );
        true
    }

    pub(crate) fn queue(&mut self, agent: &str, instruction: String, draft_revision: u64) -> bool {
        if self.submission_pending(agent) {
            return false;
        }
        let title = instruction
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default()
            .chars()
            .scan(0, |bytes, character| {
                *bytes += character.len_utf8();
                (*bytes <= 120).then_some(character)
            })
            .collect::<String>();
        if title.is_empty() || instruction.trim().is_empty() {
            return false;
        }
        self.resolve(
            TASKS_RESOURCE,
            QUEUE,
            Purpose::Queue {
                instruction: instruction.clone(),
                agent: agent.to_owned(),
                draft_revision,
            },
            Map::from_iter([
                ("title".into(), Value::String(title)),
                ("instruction".into(), Value::String(instruction)),
                ("assigned_agent".into(), Value::String(agent.to_owned())),
                ("planning".into(), Value::String("model".into())),
            ]),
        );
        if let Some(view) = self.views.get_mut(agent) {
            view.receipt = AgentTaskReceipt::Saving;
        }
        true
    }

    pub(crate) fn take_cleared_drafts(&mut self) -> Vec<(String, u64)> {
        std::mem::take(&mut self.cleared_drafts)
    }

    pub(crate) fn next_refresh_deadline(&self, agent: &str) -> Option<ui::time::Instant> {
        self.views.get(agent)?.refresh_at
    }

    fn submission_pending(&self, agent: &str) -> bool {
        self.pending.iter().any(|pending| match &pending.purpose {
            Purpose::Direct {
                agent: pending_agent,
                ..
            }
            | Purpose::Stop {
                agent: pending_agent,
                ..
            }
            | Purpose::Start {
                agent: pending_agent,
                ..
            }
            | Purpose::Queue {
                agent: pending_agent,
                ..
            } => pending_agent == agent,
            Purpose::Refresh { .. } | Purpose::ReadTask { .. } | Purpose::Conversation { .. } => {
                false
            }
        })
    }

    pub(crate) fn pump(&mut self) -> bool {
        let mut changed = false;
        let mut index = 0;
        while let Some(pending) = self.pending.get(index) {
            let settled = match &pending.answer {
                PendingAnswer::Catalogue(answer) => !answer.get().is_pending(),
                PendingAnswer::Invocation(answer) => answer.get().is_settled(),
            };
            if settled {
                changed = true;
                let call = self.pending.remove(index);
                self.settle(call);
            } else {
                index += 1;
            }
        }
        changed
    }

    fn resolve(
        &mut self,
        resource: &str,
        action: &str,
        purpose: Purpose,
        arguments: Map<String, Value>,
    ) {
        let answer = self.actions.catalogue(CatalogueQuery {
            needle: action.to_owned(),
            ..CatalogueQuery::default()
        });
        self.pending.push(Pending {
            answer: PendingAnswer::Catalogue(answer),
            purpose,
            arguments: Map::from_iter([
                ("_resource".into(), Value::String(resource.to_owned())),
                ("_action".into(), Value::String(action.to_owned())),
            ])
            .into_iter()
            .chain(arguments)
            .collect(),
        });
    }

    fn settle(&mut self, mut pending: Pending) {
        match pending.answer {
            PendingAnswer::Catalogue(answer) => {
                let RemoteCall::Done(page) = answer.get() else {
                    self.refuse(&pending.purpose);
                    return;
                };
                let resource = pending
                    .arguments
                    .remove("_resource")
                    .and_then(|value| value.as_str().map(str::to_owned));
                let action = pending
                    .arguments
                    .remove("_action")
                    .and_then(|value| value.as_str().map(str::to_owned));
                let targets = page.rows.into_iter().filter_map(|row| {
                    if Some(row.resource.id.as_str()) == resource.as_deref()
                        && row.action.as_ref().is_some_and(|descriptor| {
                            Some(descriptor.address.action.as_str()) == action.as_deref()
                        })
                    {
                        row.target()
                    } else {
                        None
                    }
                });
                let Some(target) = unique(targets) else {
                    self.refuse(&pending.purpose);
                    return;
                };
                pending.answer = PendingAnswer::Invocation(self.actions.invoke(
                    target,
                    if pending.arguments.is_empty() {
                        InvokeArgument::None
                    } else {
                        InvokeArgument::Json(pending.arguments.clone())
                    },
                    None,
                ));
                self.pending.push(pending);
            }
            PendingAnswer::Invocation(answer) => self.settle_invocation(&pending.purpose, &answer),
        }
    }

    fn settle_invocation(&mut self, purpose: &Purpose, answer: &LocalState<InvocationCall>) {
        let RemoteCall::Done(InvocationUpdate::Succeeded { output, .. }) = answer.get() else {
            self.refuse(purpose);
            return;
        };
        match purpose {
            Purpose::Conversation { .. } => self.settle_conversation(purpose, output),
            Purpose::Start {
                agent,
                task_id,
                expected_run_epoch,
            } => {
                let started = output
                    .value
                    .as_ref()
                    .and_then(|value| value.get("result"))
                    .and_then(|result| result.get("run_started"));
                let epoch = started
                    .filter(|result| {
                        result.get("task_id").and_then(Value::as_str) == Some(task_id.as_str())
                    })
                    .and_then(|result| result.get("epoch"))
                    .and_then(Value::as_u64)
                    .filter(|epoch| *epoch > *expected_run_epoch);
                if let Some(epoch) = epoch {
                    let view = self.views.entry(agent.clone()).or_default();
                    view.selected = Some((task_id.clone(), epoch));
                    view.awaiting_first_run = false;
                    view.receipt = AgentTaskReceipt::Started;
                    view.complete = false;
                    view.refresh_at = None;
                    self.observe(&self.workspace.clone(), agent);
                } else {
                    self.refuse(purpose);
                }
            }
            Purpose::ReadTask { agent } => {
                let task = output
                    .value
                    .and_then(|value| serde_json::from_value::<AgentTaskFrontier>(value).ok());
                if let Some(task) = task.filter(|task| task.assigned_agent == *agent) {
                    let view = self.views.entry(agent.clone()).or_default();
                    let mut tasks = view.tasks.clone();
                    tasks.retain(|known| known.task_id != task.task_id);
                    tasks.push(task);
                    view.apply_snapshot(AgentTaskSnapshot {
                        complete: view.complete,
                        total: tasks.len(),
                        tasks,
                    });
                    self.read_selected_conversation(agent);
                } else {
                    self.refuse(purpose);
                }
            }
            Purpose::Refresh { agent } => {
                let snapshot = output
                    .value
                    .and_then(|value| serde_json::from_value(value).ok());
                if let Some(snapshot) = snapshot {
                    let view = self.views.entry(agent.clone()).or_default();
                    view.apply_snapshot(snapshot);
                    if let Some(resource) = view.follow_task.as_deref().and_then(task_resource) {
                        view.loading = true;
                        self.resolve(
                            &resource,
                            "read-task",
                            Purpose::ReadTask {
                                agent: agent.clone(),
                            },
                            Map::new(),
                        );
                    } else {
                        self.read_selected_conversation(agent);
                    }
                } else {
                    self.refuse(purpose);
                }
            }
            Purpose::Direct { .. } => self.settle_direct(purpose, &output),
            Purpose::Stop { agent, task_id } => {
                let stopped = output
                    .value
                    .as_ref()
                    .and_then(|value| value.get("result"))
                    .and_then(|result| result.get("finished"))
                    .and_then(|finished| finished.get("task_id"))
                    .and_then(Value::as_str)
                    == Some(task_id.as_str());
                if stopped {
                    let view = self.views.entry(agent.clone()).or_default();
                    view.receipt = AgentTaskReceipt::StopRequested;
                    view.complete = false;
                    view.refresh_at = None;
                    self.observe(&self.workspace.clone(), agent);
                } else {
                    self.refuse(purpose);
                }
            }
            Purpose::Queue { .. } => self.settle_queue(purpose, &output),
        }
    }

    fn settle_conversation(&mut self, purpose: &Purpose, output: tilo_actions::ActionOutput) {
        #[derive(Deserialize)]
        struct Conversation {
            task_id: String,
            run_epoch: u64,
            turns: Vec<AgentTaskTurn>,
        }
        let Purpose::Conversation {
            agent,
            task_id,
            run_epoch,
        } = purpose
        else {
            return;
        };
        let conversation = output
            .value
            .and_then(|value| serde_json::from_value::<Conversation>(value).ok());
        let view = self.views.entry(agent.clone()).or_default();
        if view.selected.as_ref() != Some(&(task_id.clone(), *run_epoch)) {
            return;
        }
        if let Some(conversation) = conversation.filter(|read| {
            read.task_id == *task_id
                && read.run_epoch == *run_epoch
                && read.turns.len() <= 1024
                && read.turns.iter().all(|turn| {
                    turn.task_id == *task_id
                        && (turn.run_epoch == 0 || turn.run_epoch == *run_epoch)
                        && turn.presentation.as_ref().is_none_or(|presentation| {
                            presentation.valid()
                                && turn.run_epoch == *run_epoch
                                && *run_epoch > 0
                                && turn.kind == "agent-output"
                                && turn.author == format!("agent:{agent}")
                        })
                })
        }) {
            view.turns.retain(|known| {
                conversation.turns.iter().any(|turn| {
                    known.task_id == turn.task_id
                        && known.run_epoch == turn.run_epoch
                        && match (&known.presentation, &turn.presentation) {
                            (Some(a), Some(b)) => a.stream.run == b.stream.run,
                            (None, None) => {
                                known.revision == turn.revision && known.kind == turn.kind
                            }
                            _ => false,
                        }
                })
            });
            for turn in conversation.turns {
                view.retain_turn(turn);
            }
        } else {
            self.refuse(purpose);
        }
    }

    fn settle_direct(&mut self, purpose: &Purpose, output: &tilo_actions::ActionOutput) {
        let Purpose::Direct {
            text,
            draft_route,
            agent,
            task_id,
            run_epoch,
            draft_revision,
        } = purpose
        else {
            return;
        };
        let receipt = output.value.as_ref().and_then(|value| {
            let result = value.get("result")?.get("direction_added")?;
            Some((
                result.get("task_id")?.as_str()?,
                result.get("revision")?.as_u64()?,
            ))
        });
        let revision = receipt
            .filter(|(receipt_task, _)| *receipt_task == task_id)
            .map(|(_, revision)| revision);
        if let Some(revision) = revision {
            let view = self.views.entry(agent.clone()).or_default();
            view.receipt = AgentTaskReceipt::Saved {
                task_id: task_id.clone(),
                run_epoch: *run_epoch,
                revision,
            };
            view.retain_turn(AgentTaskTurn {
                task_id: task_id.clone(),
                run_epoch: *run_epoch,
                revision,
                text: text.clone(),
                presentation: None,
                received_at: ui::clock::local_now().timestamp(),
                author: String::new(),
                kind: String::new(),
            });
            self.cleared_drafts
                .push((draft_route.clone(), *draft_revision));
            view.complete = false;
            view.refresh_at = None;
            self.observe(&self.workspace.clone(), agent);
        } else {
            self.refuse(purpose);
        }
    }

    fn settle_queue(&mut self, purpose: &Purpose, output: &tilo_actions::ActionOutput) {
        let Purpose::Queue {
            instruction,
            agent,
            draft_revision,
        } = purpose
        else {
            return;
        };
        let task_id = output
            .value
            .as_ref()
            .and_then(|value| value.get("result"))
            .and_then(|result| result.get("queued"))
            .and_then(|queued| queued.get("task_id"))
            .and_then(Value::as_str)
            .filter(|task_id| !task_id.is_empty())
            .map(str::to_owned);
        if let Some(task_id) = task_id {
            let view = self.views.entry(agent.clone()).or_default();
            view.retain_turn(AgentTaskTurn {
                task_id: task_id.clone(),
                run_epoch: 0,
                revision: 0,
                text: instruction.clone(),
                presentation: None,
                received_at: ui::clock::local_now().timestamp(),
                author: String::new(),
                kind: String::new(),
            });
            view.follow_task = Some(task_id);
            view.awaiting_first_run = true;
            view.selected = None;
            view.receipt = AgentTaskReceipt::Queued;
            self.cleared_drafts.push((agent.clone(), *draft_revision));
            view.complete = false;
            view.refresh_at = None;
            self.observe(&self.workspace.clone(), agent);
        } else {
            self.refuse(purpose);
        }
    }

    fn read_selected_conversation(&mut self, agent: &str) {
        let Some((task_id, run_epoch)) =
            self.views.get(agent).and_then(|view| view.selected.clone())
        else {
            return;
        };
        if self.pending.iter().any(|pending| matches!(&pending.purpose, Purpose::Conversation { agent: held, .. } if held == agent)) { return; }
        let Some(resource) = task_resource(&task_id) else {
            return;
        };
        self.resolve(
            &resource,
            "read-conversation",
            Purpose::Conversation {
                agent: agent.to_owned(),
                task_id,
                run_epoch,
            },
            Map::from_iter([("run_epoch".into(), Value::from(run_epoch))]),
        );
    }

    fn refuse(&mut self, purpose: &Purpose) {
        let agent = match purpose {
            Purpose::Refresh { agent }
            | Purpose::ReadTask { agent }
            | Purpose::Conversation { agent, .. }
            | Purpose::Start { agent, .. }
            | Purpose::Direct { agent, .. }
            | Purpose::Stop { agent, .. }
            | Purpose::Queue { agent, .. } => agent,
        };
        let view = self.views.entry(agent.clone()).or_default();
        view.loading = false;
        if matches!(purpose, Purpose::Refresh { .. } | Purpose::ReadTask { .. }) {
            view.complete = false;
            view.refresh_at = Some(ui::time::now() + ui::time::Duration::from_secs(2));
        } else {
            view.receipt = AgentTaskReceipt::Refused;
        }
    }
}

fn unique(mut targets: impl Iterator<Item = RemoteActionTarget>) -> Option<RemoteActionTarget> {
    let target = targets.next()?;
    targets.next().is_none().then_some(target)
}

fn task_resource(task_id: &str) -> Option<String> {
    task_id
        .strip_prefix("task:")
        .is_some_and(|suffix| !suffix.is_empty() && !suffix.chars().any(char::is_control))
        .then(|| format!("desktop:{task_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, applied_revision: Option<u64>) -> AgentTaskFrontier {
        AgentTaskFrontier {
            task_id: id.into(),
            title: format!("Task {id}"),
            assigned_agent: "scribe".into(),
            run_epoch: Some(7),
            active_agent: Some("scribe".into()),
            after_revision: Some(9),
            applied_revision,
            execution: None,
            status: "in_progress".into(),
        }
    }

    #[test]
    fn agent_task_pages_stay_bounded_and_saved_is_not_applied_early() {
        let mut view = AgentTaskView {
            receipt: AgentTaskReceipt::Saved {
                task_id: "a".into(),
                run_epoch: 7,
                revision: 10,
            },
            ..AgentTaskView::default()
        };
        let snapshot = |applied_revision| AgentTaskSnapshot {
            complete: true,
            total: 4,
            tasks: vec![
                task("a", applied_revision),
                task("b", None),
                task("c", None),
                task("d", None),
            ],
        };
        view.apply_snapshot(snapshot(Some(9)));
        assert!(matches!(view.receipt, AgentTaskReceipt::Saved { .. }));
        assert_eq!(view.visible_tasks().len(), 1);
        assert_eq!(view.remaining_after_page(), 3);

        view.page = 1;
        assert_eq!(view.visible_tasks()[0].task_id, "b");
        view.apply_snapshot(snapshot(Some(10)));
        assert_eq!(view.receipt, AgentTaskReceipt::Applied);
    }

    #[test]
    fn selection_follows_revisions_but_never_silently_switches_runs() {
        let mut view = AgentTaskView::default();
        view.apply_snapshot(AgentTaskSnapshot {
            complete: true,
            total: 1,
            tasks: vec![task("task:a", None)],
        });
        assert_eq!(view.current().unwrap().run_epoch, Some(7));
        let mut changed = task("task:a", None);
        changed.after_revision = Some(12);
        view.apply_snapshot(AgentTaskSnapshot {
            complete: true,
            total: 1,
            tasks: vec![changed.clone()],
        });
        assert_eq!(view.current().unwrap().after_revision, Some(12));
        changed.run_epoch = Some(8);
        view.apply_snapshot(AgentTaskSnapshot {
            complete: true,
            total: 1,
            tasks: vec![changed],
        });
        assert!(view.current().is_none());
        assert!(!view.can_steer());
    }

    fn succeeded(value: Value) -> LocalState<InvocationCall> {
        LocalState::new(RemoteCall::Done(InvocationUpdate::Succeeded {
            invocation: tilo_actions::InvocationId::new(),
            output: tilo_actions::ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
        }))
    }

    #[test]
    fn queue_keeps_full_instruction_and_follows_the_receipted_task() {
        let mut client = AgentTaskClient::new();
        client.workspace = "room".into();
        client
            .views
            .insert("scribe".into(), AgentTaskView::default());
        let instruction = format!("{}\nKeep the whole second paragraph.", "x".repeat(180));
        assert!(client.queue("scribe", instruction.clone(), 3));
        assert_eq!(client.pending[0].arguments["instruction"], instruction);
        assert_eq!(
            client.pending[0].arguments["title"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            120
        );
        assert!(client.take_cleared_drafts().is_empty());
        let pending = client.pending.remove(0);
        client.settle_invocation(
            &pending.purpose,
            &succeeded(serde_json::json!({"result":{"queued":{"task_id":"task:new"}}})),
        );
        assert_eq!(
            client.views["scribe"].follow_task.as_deref(),
            Some("task:new")
        );
        assert_eq!(client.take_cleared_drafts(), vec![("scribe".into(), 3)]);
        assert!(matches!(client.pending[0].purpose, Purpose::Refresh { .. }));
        let pending = client.pending.remove(0);
        client.settle_invocation(
            &pending.purpose,
            &succeeded(serde_json::json!({"complete":true,"total":0,"tasks":[]})),
        );
        assert!(matches!(
            client.pending[0].purpose,
            Purpose::ReadTask { .. }
        ));
    }

    #[test]
    fn direction_receipt_retains_conversation_on_exact_task_run_and_draft() {
        let mut client = AgentTaskClient::new();
        client.stage_capture("room", "scribe");
        assert!(client.direct("scribe", "Do not drop search.".into(), 5));
        assert!(client.take_cleared_drafts().is_empty());
        let pending = client.pending.remove(0);
        client.settle_invocation(&pending.purpose, &succeeded(serde_json::json!({"result":{"direction_added":{"task_id":"task:review","revision":7}}})));
        assert!(matches!(
            client.views["scribe"].receipt,
            AgentTaskReceipt::Saved { revision: 7, .. }
        ));
        assert_eq!(
            client.take_cleared_drafts(),
            vec![("task:task:review:7".into(), 5)]
        );
        let messages = client.views["scribe"].conversation_messages("conversation", "me");
        assert_eq!(messages.len(), 6);
        assert_eq!(messages[5].text, "Do not drop search.");
        assert!(messages[5].id.contains("task:review:7:7"));
        let mut other_run = client.views["scribe"].tasks[0].clone();
        other_run.run_epoch = Some(8);
        other_run.applied_revision = Some(99);
        client
            .views
            .get_mut("scribe")
            .unwrap()
            .apply_snapshot(AgentTaskSnapshot {
                complete: true,
                total: 1,
                tasks: vec![other_run],
            });
        assert!(matches!(
            client.views["scribe"].receipt,
            AgentTaskReceipt::Saved { .. }
        ));
        assert!(
            client.views["scribe"]
                .conversation_messages("conversation", "me")
                .is_empty()
        );
    }

    #[test]
    fn reopen_hydrates_durable_turns_and_dedupes_receipted_directions() {
        let mut client = AgentTaskClient::new();
        client.stage_capture("room", "scribe");
        let purpose = Purpose::Conversation {
            agent: "scribe".into(),
            task_id: "task:review".into(),
            run_epoch: 7,
        };
        let read = succeeded(
            serde_json::json!({"task_id":"task:review", "run_epoch":7, "turns":[
                {"task_id":"task:review", "run_epoch":0, "revision":0, "author":"person", "text":"Full original instruction", "received_at":1},
                {"task_id":"task:review", "run_epoch":7, "revision":2, "author":"person", "text":"Durable direction", "received_at":2}
            ]}),
        );
        client.settle_invocation(&purpose, &read);
        client.settle_invocation(&purpose, &read);
        let messages = client.views["scribe"].conversation_messages("conversation", "me");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].text, "Full original instruction");
        assert_eq!(messages[1].text, "Durable direction");
        // A replacement run cannot populate the currently open exact run.
        let stale = Purpose::Conversation {
            agent: "scribe".into(),
            task_id: "task:review".into(),
            run_epoch: 8,
        };
        client.settle_invocation(&stale, &read);
        assert_eq!(client.views["scribe"].turns.len(), 2);
    }

    #[test]
    fn hydration_renders_reported_output_without_direction_id_collisions() {
        let mut client = AgentTaskClient::new();
        client.stage_capture("room", "scribe");
        let purpose = Purpose::Conversation {
            agent: "scribe".into(),
            task_id: "task:review".into(),
            run_epoch: 7,
        };
        let read = succeeded(serde_json::json!({
            "task_id":"task:review", "run_epoch":7, "turns":[
                {"task_id":"task:review","run_epoch":7,"revision":2,"author":"person","text":"Please update the document","received_at":1},
                {"task_id":"task:review","run_epoch":7,"revision":2,"kind":"agent-output","author":"agent:scribe","text":"Appended the requested paragraph.","received_at":2},
                {"task_id":"task:review","run_epoch":7,"revision":3,"kind":"agent-output","author":"agent:scribe","text":"The document is updated.","received_at":3}
            ]
        }));
        client.settle_invocation(&purpose, &read);
        client.settle_invocation(&purpose, &read);
        let messages = client.views["scribe"].conversation_messages("conversation", "me");
        assert_eq!(messages.len(), 3);
        assert_ne!(messages[0].id, messages[1].id);
        assert_eq!(
            messages[1].text,
            "Work update: Appended the requested paragraph."
        );
        assert_eq!(messages[2].text, "Work update: The document is updated.");
    }

    #[test]
    fn typed_task_output_updates_one_reply_and_preserves_terminal_partial_output() {
        let mut client = AgentTaskClient::new();
        client.stage_capture("room", "scribe");
        let purpose = Purpose::Conversation {
            agent: "scribe".into(),
            task_id: "task:review".into(),
            run_epoch: 7,
        };
        let read = |sequence: u64, text: &str, finish: Option<&str>| {
            succeeded(serde_json::json!({
                "task_id":"task:review", "run_epoch":7, "turns":[{
                    "task_id":"task:review", "run_epoch":7, "revision":sequence,
                    "kind":"agent-output", "author":"agent:scribe", "text":"Readable fallback",
                    "received_at":20,
                    "presentation": {"run":"81:0", "sequence":sequence, "text":text,
                        "reasoning":"Provider-exposed summary", "finish":finish,
                        "detail":if finish.is_some() {"Partial output retained"} else {""}}
                }]
            }))
        };
        client.settle_invocation(&purpose, &read(1, "Par", None));
        let first = client.views["scribe"].conversation_messages("conversation", "me");
        let reply_id = first[0].id.clone();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].text, "Par");
        assert!(
            first[0]
                .work
                .as_ref()
                .unwrap()
                .stream
                .as_ref()
                .unwrap()
                .finish
                .is_none()
        );
        for sequence in 2..150 {
            client.settle_invocation(&purpose, &read(sequence, "Partial answer", None));
        }
        client.settle_invocation(&purpose, &read(150, "Partial answer", Some("truncated")));
        // Replayed pages cannot rewind output or resurrect a completed turn.
        client.settle_invocation(&purpose, &read(1, "Par", None));
        client.settle_invocation(&purpose, &read(151, "Replacement", Some("complete")));
        let messages = client.views["scribe"].conversation_messages("conversation", "me");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, reply_id);
        assert_eq!(messages[0].text, "Partial answer");
        assert!(!messages[0].edited);
        let work = messages[0].work.as_ref().unwrap();
        assert!(
            work.trace.is_empty(),
            "presentation cannot claim an executed action"
        );
        let stream = work.stream.as_ref().unwrap();
        assert_eq!(stream.sequence, 150);
        assert_eq!(stream.reasoning, "Provider-exposed summary");
        assert_eq!(
            stream.finish,
            Some(crate::agent_stream::AgentFinish::Truncated)
        );
        assert_eq!(stream.detail, "Partial output retained");
        // An oversized page is refused without replacing the retained reply.
        client.settle_invocation(&purpose, &read(152, &"x".repeat(32 * 1024 + 1), None));
        assert_eq!(
            client.views["scribe"].conversation_messages("conversation", "me")[0].text,
            "Partial answer"
        );
    }

    #[test]
    fn typed_task_output_requires_exact_epoch_and_agent_author() {
        let mut client = AgentTaskClient::new();
        client.stage_capture("room", "scribe");
        let purpose = Purpose::Conversation {
            agent: "scribe".into(),
            task_id: "task:review".into(),
            run_epoch: 7,
        };
        let before = client.views["scribe"].turns.len();
        for (epoch, author, kind) in [
            (0, "agent:scribe", "agent-output"),
            (8, "agent:scribe", "agent-output"),
            (7, "agent:other", "agent-output"),
            (7, "agent:scribe", "direction"),
        ] {
            client.settle_invocation(
                &purpose,
                &succeeded(serde_json::json!({
                    "task_id":"task:review", "run_epoch":7, "turns":[{
                        "task_id":"task:review", "run_epoch":epoch, "revision":0,
                        "author":author, "kind":kind, "text":"not trusted",
                        "presentation":{"run":"7:0", "sequence":1, "text":"not trusted"}
                    }]
                })),
            );
            assert_eq!(client.views["scribe"].turns.len(), before);
            assert_eq!(client.views["scribe"].receipt, AgentTaskReceipt::Refused);
        }
    }

    #[test]
    fn public_read_turns_are_projected_in_time_order_without_rewriting_authority() {
        let mut client = AgentTaskClient::new();
        client.stage_capture("room", "scribe");
        let purpose = Purpose::Conversation {
            agent: "scribe".into(),
            task_id: "task:review".into(),
            run_epoch: 7,
        };
        client.settle_invocation(&purpose, &succeeded(serde_json::json!({
            "task_id":"task:review", "run_epoch":7, "turns":[
                {"task_id":"task:review","run_epoch":7,"revision":2,"author":"person","text":"Keep the original","received_at":30},
                {"task_id":"task:review","run_epoch":7,"revision":2,"kind":"agent-output","author":"agent:scribe","text":"Read the resource","received_at":20}
            ]
        })));
        let messages = client.views["scribe"].conversation_messages("conversation", "me");
        assert_eq!(messages[0].author, "agent:scribe");
        assert_eq!(messages[0].text, "Work update: Read the resource");
        assert_eq!(messages[1].author, "person");
        assert_eq!(messages[1].text, "Keep the original");
        assert!(
            messages.iter().all(|message| message.work.is_none()),
            "a public text update is not an applied-action receipt"
        );
    }

    #[test]
    fn conscious_start_uses_approval_and_does_not_claim_execution() {
        let mut client = AgentTaskClient::new();
        client.workspace = "room".into();
        let mut queued = task("task:new", None);
        queued.run_epoch = Some(0);
        queued.active_agent = None;
        queued.after_revision = Some(0);
        queued.status = "queued".into();
        let mut view = AgentTaskView::default();
        view.apply_snapshot(AgentTaskSnapshot {
            complete: true,
            total: 1,
            tasks: vec![queued],
        });
        client.views.insert("scribe".into(), view);
        assert!(client.pending.is_empty());
        assert!(client.start("scribe"));
        assert!(!client.start("scribe"));
        let pending = client.pending.remove(0);
        assert_eq!(pending.arguments["_action"], "approve-run");
        assert_eq!(pending.arguments["expected_run_epoch"], 0);
        assert_eq!(pending.arguments["after_revision"], 0);
        client.settle_invocation(
            &pending.purpose,
            &succeeded(serde_json::json!({
                "result":{"run_started":{"task_id":"task:new", "epoch":1}},
                "run_approved":true, "execution_started":false
            })),
        );
        assert_eq!(client.views["scribe"].receipt, AgentTaskReceipt::Started);
        assert_eq!(
            client.views["scribe"].selected_identity(),
            Some(("task:new", 1))
        );
        assert!(!client.views["scribe"].can_steer());
    }

    #[test]
    fn canonical_task_id_maps_to_the_exact_task_center_resource() {
        assert_eq!(
            task_resource("task:model").as_deref(),
            Some("desktop:task:model")
        );
        assert_eq!(task_resource("model"), None);
        assert_eq!(task_resource("task:"), None);
    }
}
