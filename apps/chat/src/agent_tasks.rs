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

#[derive(Deserialize)]
struct ComputeSnapshot {
    workers: Vec<crate::task_onboarding::TaskCompute>,
    #[serde(default)]
    status: String,
}

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
    #[serde(default, deserialize_with = "nullable_string")]
    pub(crate) assigned_agent: String,
    pub(crate) run_epoch: Option<u64>,
    pub(crate) active_agent: Option<String>,
    pub(crate) after_revision: Option<u64>,
    pub(crate) applied_revision: Option<u64>,
    pub(crate) execution: Option<AgentTaskExecution>,
    #[serde(default)]
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) parent_task_id: Option<String>,
    #[serde(default)]
    pub(crate) child_order: Option<i64>,
    #[serde(default)]
    pub(crate) labels: Vec<String>,
    #[serde(default)]
    pub(crate) responsible_assignees: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct AgentTaskExecution {
    pub(crate) phase: String,
}

fn nullable_string<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum FirstRun {
    #[default]
    Observed,
    Awaiting,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct AgentTaskView {
    pub(crate) loading: bool,
    pub(crate) complete: bool,
    pub(crate) tasks: Vec<AgentTaskFrontier>,
    pub(crate) receipt: AgentTaskReceipt,
    pub(crate) start_error: Option<String>,
    // The conscious approval's frontier is frozen independently of polling.
    pending_start_revision: Option<u64>,
    stop_pending: bool,
    selected: Option<(String, u64)>,
    selected_snapshot: Option<AgentTaskFrontier>,
    page: usize,
    refresh_at: Option<ui::time::Instant>,
    follow_task: Option<String>,
    first_run: FirstRun,
    new_composer: bool,
    turns: Vec<AgentTaskTurn>,
    // Owner-authorized transcript only; never copied into the room document.
    reports: Vec<tilo_actions::task_messaging::TaskReport>,
    pub(crate) outputs: Vec<tilo_actions::task_messaging::TaskOutputRef>,
    next_message_sequence: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
struct AgentTaskTurn {
    task_id: String,
    run_epoch: u64,
    #[serde(default)]
    revision: u64,
    text: String,
    received_at: i64,
    #[serde(default)]
    author: String,
    #[serde(default)]
    kind: String,
    #[serde(flatten)]
    mail: TaskMailProvenance,
}

#[derive(Deserialize)]
struct AgentTaskConversation {
    task_id: String,
    run_epoch: u64,
    turns: Vec<AgentTaskTurn>,
    #[serde(default)]
    frontier: Option<AgentTaskFrontier>,
    #[serde(default)]
    outputs: Vec<tilo_actions::task_messaging::TaskOutputRef>,
    #[serde(default)]
    reports: Vec<tilo_actions::task_messaging::TaskReport>,
    #[serde(default)]
    messages_has_more: bool,
    #[serde(default)]
    next_message_sequence: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct TaskMailProvenance {
    #[serde(default)]
    author_task_run: Option<tilo_actions::task_messaging::TaskRunAddress>,
    #[serde(default)]
    message_id: String,
    #[serde(default)]
    sequence: u64,
    #[serde(default)]
    status: String,
}

impl TaskMailProvenance {
    fn valid(&self) -> bool {
        self.author_task_run
            .as_ref()
            .is_some_and(|source| source.validate().is_ok())
            && self.sequence > 0
            && matches!(self.status.as_str(), "Accepted" | "DeliveredToModel")
            && uuid::Uuid::parse_str(&self.message_id)
                .is_ok_and(|id| !id.is_nil() && id.to_string() == self.message_id)
    }
}

pub(crate) fn output_label(output: &tilo_actions::task_messaging::TaskOutputRef) -> String {
    use tilo_actions::task_messaging::TaskOutputStatus;
    crate::i18n::t(match output.status {
        TaskOutputStatus::Created => "chat-task-output-created",
        TaskOutputStatus::Updated => "chat-task-output-updated",
        TaskOutputStatus::Viewed => "chat-task-output-viewed",
        TaskOutputStatus::Returned => "chat-task-output-returned",
    })
}

fn report_message(
    report: &tilo_actions::task_messaging::TaskReport,
    channel: &str,
) -> crate::model::Message {
    let id = ui::tilo_resource::Id::from_raw(format!(
        "task-report:{}:{}:{}:{}:{}",
        report.task_run.task_id,
        report.task_run.run_epoch,
        report.task_run.agent_id,
        report.invocation,
        report.sequence
    ));
    let message = crate::record::Message {
        channel: ui::tilo_resource::Id::from_raw(channel),
        author: ui::tilo_resource::Atom(format!("task-report:{}", report.task_run.task_id)),
        text: report.body.clone(),
        ts: (report.accepted_at_unix_ns / 1_000_000_000) as i64,
        ..crate::record::Message::default()
    };
    crate::projection::message_row(&id, &message)
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
                    && turn.kind != "agent-output"
            })
            .collect::<Vec<_>>();
        // The public read groups directions and outputs separately. The
        // conversation follows when each turn happened, not its storage lane.
        turns.sort_by_key(|turn| turn.received_at);
        let mut messages = turns
            .into_iter()
            .map(|turn| {
                let id = ui::tilo_resource::Id::from_raw(format!(
                    "task-turn:{}:{}:{}:{}:{}:{}",
                    turn.task_id,
                    turn.run_epoch,
                    turn.revision,
                    turn.kind,
                    turn.mail.message_id,
                    turn.mail.sequence
                ));
                let message = crate::record::Message {
                    channel: ui::tilo_resource::Id::from_raw(channel),
                    author: ui::tilo_resource::Atom(if turn.kind == "MessageFromTask" {
                        turn.mail.author_task_run.as_ref().map_or_else(
                            || "task-message:".into(),
                            |source| format!("task-message:{}", source.task_id),
                        )
                    } else if turn.author.is_empty() {
                        author.to_owned()
                    } else {
                        turn.author.clone()
                    }),
                    text: if turn.kind == "MessageFromTask" {
                        let source = turn.mail.author_task_run.as_ref();
                        let title = source
                            .and_then(|source| {
                                self.tasks
                                    .iter()
                                    .find(|task| task.task_id == source.task_id)
                            })
                            .map(|task| task.title.clone())
                            .filter(|title| !title.trim().is_empty())
                            .unwrap_or_else(|| crate::i18n::t("chat-task-kind"));
                        let status_key = if turn.mail.status == "DeliveredToModel" {
                            "chat-task-message-delivered"
                        } else {
                            "chat-task-message-accepted"
                        };
                        let status = crate::i18n::t(status_key);
                        crate::i18n::t_args(
                            "chat-task-message-context",
                            &[
                                ("task", title.into()),
                                ("status", status.into()),
                                ("text", turn.text.clone().into()),
                            ],
                        )
                    } else {
                        turn.text.clone()
                    },
                    ts: turn.received_at,
                    ..crate::record::Message::default()
                };
                crate::projection::message_row(&id, &message)
            })
            .collect::<Vec<_>>();
        let mut reports = self
            .reports
            .iter()
            .filter(|report| {
                report.task_run.task_id == task.task_id
                    && Some(report.task_run.run_epoch) == task.run_epoch
            })
            .collect::<Vec<_>>();
        reports.sort_by_key(|report| (report.accepted_at_unix_ns, report.sequence));
        messages.extend(
            reports
                .into_iter()
                .map(|report| report_message(report, channel)),
        );
        messages.sort_by_key(|message| message.ts);
        messages
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
        self.live_selected()
            .filter(|task| task.active_agent.is_some() || task.status == "queued")
            .or_else(|| {
                self.selected_snapshot
                    .as_ref()
                    .filter(|task| task.task_id == *task_id && task.run_epoch == Some(*run_epoch))
            })
            .or_else(|| self.live_selected())
    }

    fn live_selected(&self) -> Option<&AgentTaskFrontier> {
        let (task_id, run_epoch) = self.selected.as_ref()?;
        self.tasks
            .iter()
            .find(|task| task.task_id == *task_id && task.run_epoch == Some(*run_epoch))
    }

    pub(crate) fn can_steer(&self) -> bool {
        self.complete
            && !self.loading
            && self.live_selected().is_some_and(|task| {
                task.run_epoch.is_some()
                    && task.after_revision.is_some()
                    && task.active_agent.as_deref() == Some(task.assigned_agent.as_str())
            })
    }

    pub(crate) fn can_start(&self) -> bool {
        self.pending_start_revision.is_none()
            && !self.stop_pending
            && self.complete
            && !self.loading
            && self.live_selected().is_some_and(|task| {
                task.status == "queued"
                    && task.active_agent.is_none()
                    && task.run_epoch.is_some()
                    && task.after_revision.is_some()
            })
    }

    fn retain_turn(&mut self, turn: AgentTaskTurn) {
        self.turns.retain(|known| {
            !(known.task_id == turn.task_id
                && known.run_epoch == turn.run_epoch
                && known.revision == turn.revision
                && known.kind == turn.kind
                && known.mail.message_id == turn.mail.message_id
                && known.mail.sequence == turn.mail.sequence)
        });
        self.turns.push(turn);
    }

    fn retain_report(&mut self, report: tilo_actions::task_messaging::TaskReport) {
        if !self.reports.iter().any(|known| {
            known.task_run == report.task_run
                && known.invocation == report.invocation
                && known.sequence == report.sequence
        }) {
            self.reports.push(report);
        }
    }

    pub(crate) fn can_stop(&self) -> bool {
        (self.can_steer() || self.pending_start_revision.is_some())
            && !self.stop_pending
            && !matches!(self.receipt, AgentTaskReceipt::StopRequested)
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
        if self.first_run == FirstRun::Awaiting
            && let Some(task) = self
                .tasks
                .iter()
                .find(|task| Some(task.task_id.as_str()) == self.follow_task.as_deref())
            && let Some(epoch) = task.run_epoch
        {
            self.selected = Some((task.task_id.clone(), epoch));
            if epoch > 0 {
                self.first_run = FirstRun::Observed;
            }
        }
        // Selection names a run, not its changing instruction revision. Never
        // silently retarget an open conversation to a replacement run.
        if self.selected.is_none()
            && !self.new_composer
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
    OpenOutput {
        provider: String,
    },
    Compute {
        workspace: String,
    },
    GoalQueue {
        workspace: String,
        parent: Option<String>,
        agent: String,
        revision: u64,
    },
    ListTasks,
    CreateSubtask {
        agent: String,
    },
    Refresh {
        agent: String,
    },
    ReadTask {
        agent: String,
        task_id: String,
        run_epoch: u64,
    },
    Conversation {
        agent: String,
        task_id: String,
        run_epoch: u64,
    },
    BindExecutor {
        agent: String,
        executor: String,
        task_id: String,
        expected_run_epoch: u64,
        after_revision: u64,
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
        run_epoch: u64,
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

/// Identity of one conversation. Worker configuration is deliberately absent:
/// two tasks may use the same compute without sharing a conversation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TaskRunKey {
    pub(crate) task_id: String,
    pub(crate) run_epoch: u64,
}

#[derive(Default)]
struct TaskViews {
    runs: HashMap<TaskRunKey, AgentTaskView>,
    routes: HashMap<String, TaskRunKey>,
}

impl TaskViews {
    fn key(&self, agent: &str) -> TaskRunKey {
        self.routes
            .get(agent)
            .cloned()
            .unwrap_or_else(|| TaskRunKey {
                task_id: format!("new:{agent}"),
                run_epoch: 0,
            })
    }

    fn get(&self, agent: &str) -> Option<&AgentTaskView> {
        self.runs.get(&self.key(agent))
    }

    fn get_mut(&mut self, agent: &str) -> Option<&mut AgentTaskView> {
        self.runs.get_mut(&self.key(agent))
    }

    fn entry(
        &mut self,
        agent: &str,
    ) -> std::collections::hash_map::Entry<'_, TaskRunKey, AgentTaskView> {
        self.runs.entry(self.key(agent))
    }

    #[cfg(test)]
    fn insert(&mut self, agent: String, view: AgentTaskView) {
        let key = self.key(&agent);
        if let Some((task, epoch)) = &view.selected {
            let key = TaskRunKey {
                task_id: task.clone(),
                run_epoch: *epoch,
            };
            self.runs.insert(key.clone(), view);
            self.routes.insert(agent, key);
        } else {
            self.runs.insert(key, view);
        }
    }

    fn clear(&mut self) {
        self.runs.clear();
        self.routes.clear();
    }

    fn activate(&mut self, agent: &str, task_id: &str, run_epoch: u64) {
        let key = TaskRunKey {
            task_id: task_id.to_owned(),
            run_epoch,
        };
        let tasks = self
            .get(agent)
            .map(|view| view.tasks.clone())
            .unwrap_or_default();
        let view = self.runs.entry(key.clone()).or_default();
        view.tasks = tasks;
        view.selected = Some((task_id.to_owned(), run_epoch));
        view.follow_task = Some(task_id.to_owned());
        self.routes.insert(agent.to_owned(), key);
    }
}

#[cfg(test)]
impl std::ops::Index<&str> for TaskViews {
    type Output = AgentTaskView;

    fn index(&self, agent: &str) -> &Self::Output {
        self.get(agent).expect("test conversation must exist")
    }
}

pub(crate) struct AgentTaskClient {
    actions: RemoteActions,
    workspace: String,
    views: TaskViews,
    pending: Vec<Pending>,
    cleared_drafts: Vec<(String, u64)>,
    catalogue_tasks: Vec<AgentTaskFrontier>,
    list_refresh: Option<ui::time::Instant>,
    compute: Vec<crate::task_onboarding::TaskCompute>,
    compute_ready: bool,
    compute_status: String,
    goal_receipt: Option<Result<crate::task_onboarding::SavedGoal, String>>,
}

impl AgentTaskClient {
    pub(crate) fn new() -> Self {
        Self {
            actions: RemoteActions::current(),
            workspace: String::new(),
            views: TaskViews::default(),
            pending: Vec::new(),
            cleared_drafts: Vec::new(),
            catalogue_tasks: Vec::new(),
            list_refresh: None,
            compute: Vec::new(),
            compute_ready: false,
            compute_status: String::new(),
            goal_receipt: None,
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
                ..AgentTaskFrontier::default()
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
            author: String::new(),
            kind: String::new(),
            mail: TaskMailProvenance::default(),
        });
        // Deterministic private report fixtures, never a live model session.
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
            view.reports.push(tilo_actions::task_messaging::TaskReport {
                task_run: tilo_actions::task_messaging::TaskRunAddress {
                    task_id: "task:review".into(),
                    run_epoch: 7,
                    agent_id: agent.into(),
                },
                invocation: uuid::Uuid::from_u128(revision),
                source_device: [1; 32],
                role: tilo_actions::task_messaging::TaskReportRole::Assistant,
                accepted_at_unix_ns: ((crate::demo::NOW + seconds) as u64) * 1_000_000_000,
                sequence: revision as u64,
                actor: format!("agent:{agent}"),
                kind: tilo_actions::task_messaging::TaskReportKind::Progress,
                body: text.into(),
                presentation: None,
            });
        }
        view.turns.push(AgentTaskTurn {
            task_id: "task:review".into(),
            run_epoch: 7,
            revision: 6,
            text: "Please check the narrow layout too, before making changes.".into(),
            received_at: crate::demo::NOW + 120,
            author: String::new(),
            kind: String::new(),
            mail: TaskMailProvenance::default(),
        });
        self.views.activate(agent, "task:review", 7);
        self.views.entry(agent).insert_entry(view).into_mut()
    }

    /// A completed answer in the private transcript; only used by shot scenarios.
    pub(crate) fn stage_answer_capture(&mut self, workspace: &str, agent: &str) {
        let view = self.stage_capture(workspace, agent);
        for task in &mut view.tasks {
            task.status = "done".into();
            task.active_agent = None;
            task.execution = None;
        }
        view.reports.push(tilo_actions::task_messaging::TaskReport {
            task_run: tilo_actions::task_messaging::TaskRunAddress {
                task_id: "task:review".into(),
                run_epoch: 7,
                agent_id: agent.into(),
            },
            invocation: uuid::Uuid::from_u128(8),
            source_device: [1; 32],
            role: tilo_actions::task_messaging::TaskReportRole::Assistant,
            accepted_at_unix_ns: ((crate::demo::NOW + 150) as u64) * 1_000_000_000,
            sequence: 8,
            actor: format!("agent:{agent}"),
            kind: tilo_actions::task_messaging::TaskReportKind::Finish,
            presentation: None,
            body: "Example review complete. No changes made.\n\nThe existing open action accepts a result anchor, so search can open the matching passage directly. Keep that action rather than adding a separate navigation path.\n\nFor the narrow layout, keep the result title and passage preview in one column. Verify that opening a result reaches the same anchor from both the search UI and an action call.".into(),
        });
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
        view.reports.clear();
        for turn in &mut view.turns {
            turn.run_epoch = 0;
            turn.revision = 0;
        }
        let queued = view.clone();
        self.views.activate(agent, "task:review", 0);
        *self.views.entry(agent).or_default() = queued;
    }

    /// Keep other retained runs when the same editor changes hosts. The
    /// receiving surface's exact route wins; unrelated conversations do not.
    pub(crate) fn merge_retained(&mut self, other: &Self) {
        if self.workspace != other.workspace {
            return;
        }
        for (key, view) in &other.views.runs {
            if self.views.routes.values().any(|active| active == key) {
                if let Some(retained) = self.views.runs.get_mut(key) {
                    for report in &view.reports {
                        retained.retain_report(report.clone());
                    }
                }
            } else {
                let mut merged = view.clone();
                if let Some(retained) = self.views.runs.get(key) {
                    for report in &retained.reports {
                        merged.retain_report(report.clone());
                    }
                }
                self.views.runs.insert(key.clone(), merged);
            }
        }
        self.catalogue_tasks.clone_from(&other.catalogue_tasks);
    }

    pub(crate) fn all_tasks(&self) -> Vec<&AgentTaskFrontier> {
        let mut tasks = std::collections::BTreeMap::new();
        for task in &self.catalogue_tasks {
            tasks.insert(task.task_id.clone(), task);
        }
        for view in self.views.runs.values() {
            for task in &view.tasks {
                let replace = tasks
                    .get(&task.task_id)
                    .is_none_or(|known: &&AgentTaskFrontier| {
                        (task.run_epoch, task.after_revision)
                            > (known.run_epoch, known.after_revision)
                    });
                if replace {
                    tasks.insert(task.task_id.clone(), task);
                }
            }
        }
        let mut pending = tasks.into_values().collect::<Vec<_>>();
        pending.sort_by_key(|task| (task.child_order.unwrap_or_default(), task.task_id.as_str()));
        let mut ordered = Vec::new();
        while !pending.is_empty() {
            let index = pending
                .iter()
                .position(|task| {
                    task.parent_task_id.as_ref().is_none_or(|parent| {
                        ordered
                            .iter()
                            .any(|known: &&AgentTaskFrontier| known.task_id == *parent)
                            || !pending.iter().any(|known| known.task_id == *parent)
                    })
                })
                .unwrap_or_default();
            ordered.push(pending.remove(index));
        }
        ordered
    }

    pub(crate) fn observe_tasks(&mut self, workspace: &str) {
        if self.workspace != workspace {
            workspace.clone_into(&mut self.workspace);
            self.compute.clear();
            self.compute_ready = false;
            self.compute_status.clear();
            self.goal_receipt = None;
            self.views.clear();
            self.pending.clear();
            self.catalogue_tasks.clear();
            self.list_refresh = None;
        }
        if self
            .list_refresh
            .is_some_and(|deadline| ui::time::now() < deadline)
            || self
                .pending
                .iter()
                .any(|pending| matches!(pending.purpose, Purpose::ListTasks))
        {
            return;
        }
        self.resolve(TASKS_RESOURCE, "list-tasks", Purpose::ListTasks, Map::new());
    }

    pub(crate) fn outputs(&self) -> Vec<&tilo_actions::task_messaging::TaskOutputRef> {
        self.views
            .runs
            .values()
            .flat_map(|view| view.outputs.iter())
            .collect()
    }

    pub(crate) fn open_output(
        &mut self,
        output: &tilo_actions::task_messaging::TaskOutputRef,
    ) -> bool {
        let retained = self
            .views
            .runs
            .values()
            .any(|view| view.outputs.contains(output));
        if !retained || output.validate().is_err() {
            return false;
        }
        self.resolve(
            &output.resource,
            &output.open_action,
            Purpose::OpenOutput {
                provider: output.provider.clone(),
            },
            Map::new(),
        );
        true
    }

    pub(crate) fn eligible_compute(
        &self,
        workspace: &str,
    ) -> Option<&crate::task_onboarding::TaskCompute> {
        crate::task_onboarding::eligible_compute(&self.compute, workspace)
    }

    pub(crate) fn compute_ready(&self) -> bool {
        self.compute_ready
    }

    pub(crate) fn needs_model_setup(&self) -> bool {
        self.compute_ready && self.compute_status == "needsConfiguration"
    }

    pub(crate) fn needs_worker_setup(&self) -> bool {
        self.compute_ready && self.compute_status == "needsExecutorSetup"
    }

    /// Settings are sufficient for an explicit Start to prepare private compute.
    /// Discovery itself never binds, approves or launches a worker.
    pub(crate) fn can_prepare_model_start(&self) -> bool {
        self.needs_worker_setup() || (self.compute_ready && self.compute_status == "available")
    }

    pub(crate) fn needs_compute_controls(&self) -> bool {
        self.eligible_compute(&self.workspace).is_none() && !self.can_prepare_model_start()
    }

    pub(crate) fn needs_setup(&self) -> bool {
        self.needs_model_setup()
    }

    pub(crate) fn setup_label(&self) -> String {
        crate::i18n::t(if self.needs_model_setup() {
            "chat-task-setup"
        } else {
            "chat-task-service-retry"
        })
    }

    pub(crate) fn refresh_compute(&mut self, workspace: &str) {
        if self.pending.iter().any(|pending| matches!(&pending.purpose, Purpose::Compute { workspace: known } if known == workspace)) { return; }
        self.compute.clear();
        self.compute_status.clear();
        self.compute_ready = false;
        self.resolve(
            TASKS_RESOURCE,
            "read-task-compute",
            Purpose::Compute {
                workspace: workspace.to_owned(),
            },
            Map::new(),
        );
    }

    pub(crate) fn queue_goal(
        &mut self,
        agent: &str,
        instruction: String,
        revision: u64,
        parent: Option<String>,
    ) -> bool {
        if instruction.trim().is_empty()
            || self
                .pending
                .iter()
                .any(|pending| matches!(pending.purpose, Purpose::GoalQueue { .. }))
        {
            return false;
        }
        let resource = match parent.as_deref() {
            Some(parent) => {
                let Some(resource) = task_resource(parent) else {
                    return false;
                };
                resource
            }
            None => TASKS_RESOURCE.to_owned(),
        };
        let title = crate::i18n::t(if parent.is_some() {
            "chat-task-neutral-subtask"
        } else {
            "chat-task-neutral-title"
        });
        let route_key = if parent.is_some() {
            "route"
        } else {
            "planning"
        };
        self.resolve(
            &resource,
            if parent.is_some() {
                "queue-private-subtask"
            } else {
                QUEUE
            },
            Purpose::GoalQueue {
                workspace: self.workspace.clone(),
                parent,
                agent: agent.to_owned(),
                revision,
            },
            Map::from_iter([
                ("title".into(), Value::String(title)),
                ("instruction".into(), Value::String(instruction)),
                ("assigned_agent".into(), Value::String(agent.to_owned())),
                (route_key.into(), Value::String("model".into())),
            ]),
        );
        true
    }

    pub(crate) fn take_goal_receipt(
        &mut self,
    ) -> Option<Result<crate::task_onboarding::SavedGoal, String>> {
        self.goal_receipt.take()
    }

    pub(crate) fn create_subtask(&mut self, agent: &str, title: String) -> bool {
        if title.trim().is_empty() || self.submission_pending(agent) {
            return false;
        }
        let Some(task) = self.view(agent).and_then(AgentTaskView::current) else {
            return false;
        };
        let Some(resource) = task_resource(&task.task_id) else {
            return false;
        };
        self.resolve(
            &resource,
            "create-subtask",
            Purpose::CreateSubtask {
                agent: agent.to_owned(),
            },
            Map::from_iter([
                ("title".into(), Value::String(title)),
                ("assigned_agent".into(), Value::String(agent.to_owned())),
                ("route".into(), Value::String("model".into())),
            ]),
        );
        true
    }

    /// Hierarchy fixture for explicit capture scenarios, never live startup.
    pub(crate) fn stage_subtask_capture(&mut self, workspace: &str, agent: &str) {
        self.stage_capture(workspace, agent);
        let mut child = self
            .views
            .get(agent)
            .and_then(AgentTaskView::current)
            .cloned()
            .unwrap_or_default();
        child.task_id = "task:review-child".into();
        child.title = "Example: Check the narrow layout".into();
        child.parent_task_id = Some("task:review".into());
        child.status = "queued".into();
        child.run_epoch = Some(0);
        child.active_agent = None;
        child.execution = None;
        child.after_revision = Some(0);
        child.labels = vec!["Example".into()];
        if let Some(view) = self.views.get_mut(agent) {
            view.tasks.push(child);
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
            self.compute.clear();
            self.compute_ready = false;
            self.compute_status.clear();
            self.goal_receipt = None;
        }
        let view = self.views.entry(agent).or_default();
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
            if let Some((task_id, run_epoch)) = view.selected.clone()
                && let Some(resource) = task_resource(&task_id)
            {
                // A saved Goal has no executor yet. Read its owner frontier
                // directly; an empty agent is not a registered participant.
                self.resolve(
                    &resource,
                    "read-task",
                    Purpose::ReadTask {
                        agent: agent.to_owned(),
                        task_id,
                        run_epoch,
                    },
                    Map::new(),
                );
            } else if !agent.is_empty() {
                self.resolve(
                    TASKS_RESOURCE,
                    READ_CURRENT_TASKS,
                    Purpose::Refresh {
                        agent: agent.to_owned(),
                    },
                    Map::from_iter([("agent".into(), Value::String(agent.to_owned()))]),
                );
            } else {
                view.loading = false;
            }
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
        if self.workspace != workspace {
            self.observe_tasks(workspace);
        }
        self.views.activate(agent, task_id, run_epoch);
        let view = self.views.entry(agent).or_default();
        view.selected = Some((task_id.to_owned(), run_epoch));
        view.follow_task = Some(task_id.to_owned());
        view.first_run = FirstRun::Observed;
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
        let exact = self.all_tasks().iter().any(|task| {
            task.task_id == frontier.task_id
                && task.assigned_agent == agent
                && task.run_epoch == Some(run_epoch)
                && task.after_revision == Some(after_revision)
        });
        if exact {
            let tasks = self
                .all_tasks()
                .into_iter()
                .filter(|task| task.assigned_agent == agent)
                .cloned()
                .collect();
            self.views.entry(agent).or_default().tasks = tasks;
            self.views.activate(agent, &frontier.task_id, run_epoch);
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
        let unassigned = task.assigned_agent.is_empty();
        let executor = if unassigned {
            if self.can_prepare_model_start() {
                // Even an advertised worker must pass the protected Start gate.
                // Freeze the original frontier before any bind, and let the
                // owner select current compute or consciously relaunch it.
                let mut candidates = self.compute.iter().filter(|worker| {
                    worker.workspace == self.workspace
                        && worker.configured
                        && !worker.agent_id.is_empty()
                        && !worker.profile.trim().is_empty()
                });
                let candidate = candidates.next();
                if candidates.next().is_none() {
                    candidate.map_or_else(String::new, |worker| worker.agent_id.clone())
                } else {
                    String::new()
                }
            } else if let Some(worker) = self
                .eligible_compute(&self.workspace)
                .filter(|_| self.compute_ready())
            {
                worker.agent_id.clone()
            } else {
                if let Some(view) = self.views.get_mut(agent) {
                    view.start_error = Some(crate::i18n::t("chat-task-no-compute"));
                }
                self.refresh_compute(&self.workspace.clone());
                return false;
            }
        } else {
            task.assigned_agent.clone()
        };
        let bind_executor = unassigned && !executor.is_empty() && !self.can_prepare_model_start();
        if let Some(view) = self.views.get_mut(agent) {
            view.start_error = None;
            if !bind_executor {
                view.pending_start_revision = Some(revision);
            }
        }
        let purpose = if bind_executor {
            Purpose::BindExecutor {
                agent: agent.to_owned(),
                executor: executor.clone(),
                task_id,
                expected_run_epoch: epoch,
                after_revision: revision,
            }
        } else {
            Purpose::Start {
                agent: agent.to_owned(),
                task_id,
                expected_run_epoch: epoch,
            }
        };
        self.resolve(
            &resource,
            if bind_executor {
                "bind-executor"
            } else {
                "approve-run"
            },
            purpose,
            Map::from_iter([
                ("agent_id".into(), Value::String(executor)),
                ("expected_run_epoch".into(), Value::from(epoch)),
                ("after_revision".into(), Value::from(revision)),
            ]),
        );
        true
    }

    pub(crate) fn stop(&mut self, agent: &str) -> bool {
        let Some(view) = self.views.get(agent) else {
            return false;
        };
        // Start and Stop have independent invocation handles. Only the exact
        // selected approval may overlap Stop; other writes remain serialized.
        let blocked = self.pending.iter().any(|pending| {
            if let Purpose::Start {
                agent: held,
                task_id,
                expected_run_epoch,
            } = &pending.purpose
            {
                return held == agent
                    && view.selected_identity() != Some((task_id.as_str(), *expected_run_epoch));
            }
            matches!(&pending.purpose,
                Purpose::CreateSubtask { agent: held }
                | Purpose::Direct { agent: held, .. }
                | Purpose::Stop { agent: held, .. }
                | Purpose::BindExecutor { agent: held, .. }
                | Purpose::Queue { agent: held, .. } if held == agent)
        });
        if blocked || !view.can_stop() {
            return false;
        }
        let Some(task) = self.views.get(agent).and_then(AgentTaskView::current) else {
            return false;
        };
        let (Some(run_epoch), Some(after_revision)) = (
            task.run_epoch,
            view.pending_start_revision.or(task.after_revision),
        ) else {
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
                run_epoch,
            },
            Map::from_iter([
                ("run_epoch".into(), Value::from(run_epoch)),
                ("after_revision".into(), Value::from(after_revision)),
            ]),
        );
        if let Some(view) = self.views.get_mut(agent) {
            view.stop_pending = true;
            view.receipt = AgentTaskReceipt::StopRequested;
        }
        true
    }

    pub(crate) fn queue(&mut self, agent: &str, instruction: String, draft_revision: u64) -> bool {
        if self.submission_pending(agent) {
            return false;
        }
        // Instructions are private. A shared label is never an excerpt of
        // the person's goal, even when that would make a convenient title.
        let title = crate::i18n::t("chat-task-neutral-title");
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

    pub(crate) fn task_list_deadline(&self) -> Option<ui::time::Instant> {
        self.list_refresh
    }

    pub(crate) fn next_refresh_deadline(&self, agent: &str) -> Option<ui::time::Instant> {
        self.views.get(agent)?.refresh_at
    }

    fn submission_pending(&self, agent: &str) -> bool {
        self.pending.iter().any(|pending| match &pending.purpose {
            Purpose::CreateSubtask {
                agent: pending_agent,
            }
            | Purpose::Direct {
                agent: pending_agent,
                ..
            }
            | Purpose::Stop {
                agent: pending_agent,
                ..
            }
            | Purpose::BindExecutor {
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
            Purpose::OpenOutput { .. }
            | Purpose::Compute { .. }
            | Purpose::GoalQueue { .. }
            | Purpose::ListTasks
            | Purpose::Refresh { .. }
            | Purpose::ReadTask { .. }
            | Purpose::Conversation { .. } => false,
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
                    if let Purpose::OpenOutput { provider } = &pending.purpose
                        && row
                            .target()
                            .is_none_or(|target| target.provider != *provider)
                    {
                        return None;
                    }
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
            Purpose::OpenOutput { .. } => {}
            Purpose::Compute { .. } => self.settle_compute(purpose, output),
            Purpose::GoalQueue { .. } => self.settle_goal_queue(purpose, &output),
            Purpose::ListTasks => {
                if let Some(snapshot) = output
                    .value
                    .and_then(|value| serde_json::from_value::<AgentTaskSnapshot>(value).ok())
                {
                    self.catalogue_tasks = snapshot.tasks;
                }
                self.list_refresh = Some(ui::time::now() + ui::time::Duration::from_secs(2));
            }
            Purpose::CreateSubtask { agent } => {
                self.list_refresh = None;
                let view = self.views.entry(agent).or_default();
                view.complete = false;
                view.refresh_at = None;
                self.observe(&self.workspace.clone(), agent);
            }
            Purpose::Conversation { .. } => self.settle_conversation(purpose, output),
            Purpose::BindExecutor { .. } => self.settle_bind_executor(purpose, &output),
            Purpose::Start { .. } => self.settle_start(purpose, &output),
            Purpose::ReadTask { .. } => self.settle_read_task(purpose, output),
            Purpose::Refresh { .. } => self.settle_refresh(purpose, output),
            Purpose::Direct { .. } => self.settle_direct(purpose, &output),
            Purpose::Stop { .. } => self.settle_stop(purpose, &output),
            Purpose::Queue { .. } => self.settle_queue(purpose, &output),
        }
    }

    fn settle_compute(&mut self, purpose: &Purpose, output: tilo_actions::ActionOutput) {
        let Purpose::Compute { workspace } = purpose else {
            return;
        };
        self.compute_ready = true;
        self.compute_status.clear();
        self.compute = output
            .value
            .and_then(|value| serde_json::from_value::<ComputeSnapshot>(value).ok())
            .map(|snapshot| {
                self.compute_status = snapshot.status;
                snapshot
                    .workers
                    .into_iter()
                    .filter(|worker| worker.workspace == *workspace)
                    .collect()
            })
            .unwrap_or_default();
    }

    fn settle_goal_queue(&mut self, purpose: &Purpose, output: &tilo_actions::ActionOutput) {
        let Purpose::GoalQueue {
            workspace,
            parent,
            agent,
            revision,
        } = purpose
        else {
            return;
        };
        let task = output
            .value
            .as_ref()
            .and_then(|value| value.get("result"))
            .and_then(|result| result.get("queued"))
            .and_then(|queued| queued.get("task_id"))
            .and_then(Value::as_str)
            .filter(|task| task_resource(task).is_some());
        self.goal_receipt = Some(
            task.map(|task| crate::task_onboarding::SavedGoal {
                workspace: workspace.clone(),
                parent: parent.clone(),
                task: task.to_owned(),
                agent: agent.clone(),
                revision: *revision,
            })
            .ok_or_else(|| crate::i18n::t("chat-task-save-refused")),
        );
        self.list_refresh = None;
    }

    fn settle_bind_executor(&mut self, purpose: &Purpose, output: &tilo_actions::ActionOutput) {
        let Purpose::BindExecutor {
            agent,
            executor,
            task_id,
            expected_run_epoch,
            after_revision,
        } = purpose
        else {
            return;
        };
        let bound = output.value.as_ref().and_then(|value| {
            value
                .pointer("/result/metadata_updated/task_id")
                .and_then(Value::as_str)
        }) == Some(task_id.as_str());
        if !bound {
            self.refuse(purpose);
            return;
        }
        if let Some(view) = self.views.get_mut(agent) {
            for task in &mut view.tasks {
                if task.task_id == *task_id && task.run_epoch == Some(*expected_run_epoch) {
                    task.assigned_agent.clone_from(executor);
                }
            }
        }
        let Some(resource) = task_resource(task_id) else {
            return;
        };
        if let Some(view) = self.views.runs.get_mut(&TaskRunKey {
            task_id: task_id.clone(),
            run_epoch: *expected_run_epoch,
        }) {
            view.pending_start_revision = Some(*after_revision);
        }
        // Binding grants nothing and leaves the frontier unchanged.
        // Approval is still the separate, conscious Start operation.
        self.resolve(
            &resource,
            "approve-run",
            Purpose::Start {
                agent: agent.clone(),
                task_id: task_id.clone(),
                expected_run_epoch: *expected_run_epoch,
            },
            Map::from_iter([
                ("agent_id".into(), Value::String(executor.clone())),
                (
                    "expected_run_epoch".into(),
                    Value::from(*expected_run_epoch),
                ),
                ("after_revision".into(), Value::from(*after_revision)),
            ]),
        );
    }

    fn settle_start(&mut self, purpose: &Purpose, output: &tilo_actions::ActionOutput) {
        let Purpose::Start {
            agent,
            task_id,
            expected_run_epoch,
        } = purpose
        else {
            return;
        };
        if let Some(view) = self.views.runs.get_mut(&TaskRunKey {
            task_id: task_id.clone(),
            run_epoch: *expected_run_epoch,
        }) {
            view.pending_start_revision = None;
        }
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
            let following = self.view(agent).and_then(AgentTaskView::selected_identity)
                == Some((task_id.as_str(), *expected_run_epoch));
            if following {
                self.views.activate(agent, task_id, epoch);
            }
            let key = TaskRunKey {
                task_id: task_id.clone(),
                run_epoch: epoch,
            };
            let view = self.views.runs.entry(key).or_default();
            view.selected = Some((task_id.clone(), epoch));
            view.first_run = FirstRun::Observed;
            view.receipt = AgentTaskReceipt::Started;
            view.complete = false;
            view.refresh_at = None;
            self.observe(&self.workspace.clone(), agent);
        } else {
            self.refuse(purpose);
        }
    }

    fn settle_read_task(&mut self, purpose: &Purpose, output: tilo_actions::ActionOutput) {
        let Purpose::ReadTask {
            agent,
            task_id,
            run_epoch,
        } = purpose
        else {
            return;
        };
        let task = output
            .value
            .and_then(|value| serde_json::from_value::<AgentTaskFrontier>(value).ok());
        if self.view(agent).and_then(AgentTaskView::selected_identity)
            != Some((task_id.as_str(), *run_epoch))
        {
            return;
        }
        if let Some(mut task) = task.filter(|task| task.task_id == *task_id) {
            // Reading survives reassignment; mutating never does.
            if !agent.is_empty() && task.assigned_agent != *agent {
                task.active_agent = None;
                task.status = "ended".into();
            }
            let view = self.views.entry(agent).or_default();
            let mut tasks = view.tasks.clone();
            tasks.retain(|known| known.task_id != task.task_id);
            tasks.push(task);
            view.apply_snapshot(AgentTaskSnapshot {
                complete: true,
                total: tasks.len(),
                tasks,
            });
            self.read_selected_conversation(agent);
        } else {
            self.refuse(purpose);
        }
    }

    fn settle_refresh(&mut self, purpose: &Purpose, output: tilo_actions::ActionOutput) {
        let Purpose::Refresh { agent } = purpose else {
            return;
        };
        let snapshot = output
            .value
            .and_then(|value| serde_json::from_value(value).ok());
        if let Some(snapshot) = snapshot {
            let view = self.views.entry(agent).or_default();
            view.apply_snapshot(snapshot);
            if let Some((task_id, run_epoch)) = view.selected.clone()
                && let Some(resource) = task_resource(&task_id)
            {
                view.loading = true;
                self.resolve(
                    &resource,
                    "read-task",
                    Purpose::ReadTask {
                        agent: agent.clone(),
                        task_id,
                        run_epoch,
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

    fn settle_stop(&mut self, purpose: &Purpose, output: &tilo_actions::ActionOutput) {
        let Purpose::Stop {
            agent,
            task_id,
            run_epoch,
        } = purpose
        else {
            return;
        };
        let stopped = output
            .value
            .as_ref()
            .and_then(|value| value.get("result"))
            .and_then(|result| result.get("finished"))
            .and_then(|finished| finished.get("task_id"))
            .and_then(Value::as_str)
            == Some(task_id.as_str());
        if stopped {
            let view = self
                .views
                .runs
                .entry(TaskRunKey {
                    task_id: task_id.clone(),
                    run_epoch: *run_epoch,
                })
                .or_default();
            view.stop_pending = false;
            // The owner has invalidated this approval frontier. Its separate
            // invocation still settles normally, but cannot be cancelled twice.
            view.pending_start_revision = None;
            view.receipt = AgentTaskReceipt::None;
            view.complete = false;
            view.refresh_at = None;
            self.observe(&self.workspace.clone(), agent);
        } else {
            self.refuse(purpose);
        }
    }

    fn settle_conversation(&mut self, purpose: &Purpose, output: tilo_actions::ActionOutput) {
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
            .and_then(|value| serde_json::from_value::<AgentTaskConversation>(value).ok());
        let view = self.views.entry(agent).or_default();
        if view.selected.as_ref() != Some(&(task_id.clone(), *run_epoch)) {
            return;
        }
        if let Some(conversation) = conversation.filter(|read| {
            read.task_id == *task_id
                && read.run_epoch == *run_epoch
                && read.turns.iter().all(|turn| {
                    turn.task_id == *task_id
                        && (turn.run_epoch == 0 || turn.run_epoch == *run_epoch)
                        && (turn.kind != "MessageFromTask"
                            || (turn.run_epoch == *run_epoch && turn.mail.valid()))
                })
        }) {
            if let Some(frontier) = conversation.frontier {
                if frontier.task_id != *task_id || frontier.run_epoch != Some(*run_epoch) {
                    self.refuse(purpose);
                    return;
                }
                view.selected_snapshot = Some(frontier);
            }
            view.turns.retain(|turn| {
                turn.kind == "MessageFromTask"
                    || turn.task_id != *task_id
                    || (turn.run_epoch != 0 && turn.run_epoch != *run_epoch)
            });
            view.outputs = conversation
                .outputs
                .into_iter()
                .filter(|output| {
                    output.validate().is_ok()
                        && output.task_run.task_id == *task_id
                        && output.task_run.run_epoch == *run_epoch
                })
                .collect();
            // A poll may overlap another poll or contain an older snapshot.
            // Accepted reports are immutable: merge by exact run/invocation/sequence,
            // retaining the first authenticated body rather than replacing it.
            for report in conversation.reports {
                if report.validate().is_ok()
                    && report.task_run.task_id == *task_id
                    && report.task_run.run_epoch == *run_epoch
                {
                    view.retain_report(report);
                }
            }
            let next = conversation.next_message_sequence;
            let advance =
                next.is_some_and(|next| next > view.next_message_sequence.unwrap_or_default());
            if advance {
                view.next_message_sequence = next;
            }
            for turn in conversation.turns {
                view.retain_turn(turn);
            }
            if conversation.messages_has_more && advance {
                self.read_selected_conversation(agent);
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
            let key = TaskRunKey {
                task_id: task_id.clone(),
                run_epoch: *run_epoch,
            };
            let view = self.views.runs.entry(key).or_default();
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
                received_at: ui::clock::local_now().timestamp(),
                author: String::new(),
                kind: String::new(),
                mail: TaskMailProvenance::default(),
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
            self.views.activate(agent, &task_id, 0);
            let view = self.views.entry(agent).or_default();
            view.retain_turn(AgentTaskTurn {
                task_id: task_id.clone(),
                run_epoch: 0,
                revision: 0,
                text: instruction.clone(),
                received_at: ui::clock::local_now().timestamp(),
                author: String::new(),
                kind: String::new(),
                mail: TaskMailProvenance::default(),
            });
            view.follow_task = Some(task_id.clone());
            view.first_run = FirstRun::Observed;
            view.selected = Some((task_id, 0));
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
        let mut arguments = Map::from_iter([("run_epoch".into(), Value::from(run_epoch))]);
        if let Some(sequence) = self
            .views
            .get(agent)
            .and_then(|view| view.next_message_sequence)
        {
            arguments.insert("after_message_sequence".into(), Value::from(sequence));
        }
        self.resolve(
            &resource,
            "read-conversation",
            Purpose::Conversation {
                agent: agent.to_owned(),
                task_id,
                run_epoch,
            },
            arguments,
        );
    }

    fn refuse(&mut self, purpose: &Purpose) {
        // A late approval or cancellation failure belongs to its original run,
        // not whichever conversation now uses the same compute route.
        match purpose {
            Purpose::Start {
                task_id,
                expected_run_epoch,
                ..
            } => {
                let view = self
                    .views
                    .runs
                    .entry(TaskRunKey {
                        task_id: task_id.clone(),
                        run_epoch: *expected_run_epoch,
                    })
                    .or_default();
                view.pending_start_revision = None;
                view.start_error = Some(crate::i18n::t("chat-agent-start-unavailable"));
                view.refresh_at = None;
                return;
            }
            Purpose::Stop {
                task_id, run_epoch, ..
            } => {
                let view = self
                    .views
                    .runs
                    .entry(TaskRunKey {
                        task_id: task_id.clone(),
                        run_epoch: *run_epoch,
                    })
                    .or_default();
                view.stop_pending = false;
                view.receipt = AgentTaskReceipt::Refused;
                view.refresh_at = None;
                return;
            }
            _ => {}
        }
        if matches!(purpose, Purpose::ListTasks) {
            self.list_refresh = Some(ui::time::now() + ui::time::Duration::from_secs(2));
            return;
        }
        let agent = match purpose {
            Purpose::OpenOutput { .. } | Purpose::ListTasks => return,
            Purpose::Compute { .. } => {
                self.compute_ready = true;
                self.compute_status.clear();
                self.compute.clear();
                return;
            }
            Purpose::GoalQueue { .. } => {
                self.goal_receipt = Some(Err(crate::i18n::t("chat-task-save-refused")));
                return;
            }
            Purpose::CreateSubtask { agent }
            | Purpose::Refresh { agent }
            | Purpose::ReadTask { agent, .. }
            | Purpose::Conversation { agent, .. }
            | Purpose::BindExecutor { agent, .. }
            | Purpose::Start { agent, .. }
            | Purpose::Direct { agent, .. }
            | Purpose::Stop { agent, .. }
            | Purpose::Queue { agent, .. } => agent,
        };
        let view = self.views.entry(agent).or_default();
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
            ..AgentTaskFrontier::default()
        }
    }

    #[test]
    fn missing_executor_is_not_missing_model_configuration() {
        let mut client = AgentTaskClient::new();
        let purpose = Purpose::Compute {
            workspace: "room".into(),
        };
        assert!(!client.needs_model_setup());
        for (status, needs_model) in [
            ("needsExecutorSetup", false),
            ("needsConfiguration", true),
            ("available", false),
            ("", false),
        ] {
            client.settle_invocation(
                &purpose,
                &succeeded(serde_json::json!({"workers": [], "status": status})),
            );
            assert!(client.compute_ready());
            assert_eq!(client.needs_model_setup(), needs_model, "{status}");
        }
    }

    #[test]
    fn configured_worker_requires_explicit_workspace_registration_and_eligibility() {
        let mut client = AgentTaskClient::new();
        let purpose = Purpose::Compute {
            workspace: "room".into(),
        };
        let worker = serde_json::json!({"agent_id":"scribe","workspace":"room","eligible":true,"configured":true,"registered":true,"profile":"Local model"});
        client.settle_invocation(
            &purpose,
            &succeeded(serde_json::json!({"workers":[worker]})),
        );
        assert_eq!(client.eligible_compute("room").unwrap().agent_id, "scribe");
        assert!(client.eligible_compute("other").is_none());
        for field in ["eligible", "configured", "registered"] {
            let mut invalid = worker.clone();
            invalid[field] = Value::Bool(false);
            client.settle_invocation(
                &purpose,
                &succeeded(serde_json::json!({"workers":[invalid]})),
            );
            assert!(client.eligible_compute("room").is_none(), "{field}");
        }
        let mut second = worker.clone();
        second["agent_id"] = Value::String("other-worker".into());
        client.settle_invocation(
            &purpose,
            &succeeded(serde_json::json!({"workers":[worker, second]})),
        );
        assert!(
            client.eligible_compute("room").is_none(),
            "ambiguous compute is not an implicit persona choice"
        );
    }

    #[test]
    fn private_child_goal_uses_atomic_registered_parent_action_and_waits_for_saved_receipt() {
        let mut client = AgentTaskClient::new();
        client.workspace = "room".into();
        assert!(client.queue_goal(
            "scribe",
            "Actual private child goal".into(),
            4,
            Some("task:parent".into())
        ));
        let pending = client.pending.remove(0);
        assert_eq!(pending.arguments["_resource"], "desktop:task:parent");
        assert_eq!(pending.arguments["_action"], "queue-private-subtask");
        assert_eq!(
            pending.arguments["instruction"],
            "Actual private child goal"
        );
        assert_eq!(
            pending.arguments["title"],
            crate::i18n::t("chat-task-neutral-subtask")
        );
        assert!(!pending.arguments.contains_key("parent_task_id"));
        assert!(client.all_tasks().is_empty());
        assert!(client.take_goal_receipt().is_none());
        client.settle_invocation(
            &pending.purpose,
            &succeeded(serde_json::json!({"result":{"queued":{"task_id":"task:child"}}})),
        );
        let saved = client.take_goal_receipt().unwrap().unwrap();
        assert_eq!(saved.parent.as_deref(), Some("task:parent"));
        assert_eq!(saved.task, "task:child");
        assert_eq!(saved.revision, 4);
        assert!(
            !client
                .pending
                .iter()
                .any(|pending| matches!(pending.purpose, Purpose::Start { .. }))
        );
    }

    #[test]
    fn private_goal_can_be_saved_without_compute_or_an_executor() {
        for parent in [None, Some("task:parent".to_owned())] {
            let mut client = AgentTaskClient::new();
            client.workspace = "room".into();
            assert!(client.queue_goal("", "Private instructions".into(), 3, parent.clone()));
            let pending = client.pending.remove(0);
            assert_eq!(pending.arguments["assigned_agent"], "");
            assert_eq!(pending.arguments["instruction"], "Private instructions");
            client.settle_invocation(
                &pending.purpose,
                &succeeded(serde_json::json!({"result":{"queued":{"task_id":"task:pending"}}})),
            );
            let saved = client.take_goal_receipt().unwrap().unwrap();
            assert_eq!(saved.task, "task:pending");
            assert_eq!(saved.parent, parent);
            assert!(saved.agent.is_empty());
            assert!(
                client.pending.is_empty(),
                "saving neither binds nor approves"
            );
        }
    }

    fn pending_task_client() -> AgentTaskClient {
        let mut client = AgentTaskClient::new();
        client.workspace = "room".into();
        let mut pending = task("task:pending", None);
        pending.assigned_agent.clear();
        pending.run_epoch = Some(0);
        pending.after_revision = Some(4);
        pending.active_agent = None;
        pending.status = "queued".into();
        let mut view = AgentTaskView::default();
        view.apply_snapshot(AgentTaskSnapshot {
            complete: true,
            total: 1,
            tasks: vec![pending],
        });
        client.views.insert(String::new(), view);
        client
    }

    #[test]
    fn pending_start_requires_setup_instead_of_inventing_an_agent() {
        let mut client = pending_task_client();
        assert!(!client.start(""));
        assert_eq!(
            client.views[""].start_error,
            Some(crate::i18n::t("chat-task-no-compute"))
        );
        assert_eq!(
            client.views[""].selected_identity(),
            Some(("task:pending", 0))
        );
        assert!(
            client
                .pending
                .iter()
                .all(|pending| matches!(pending.purpose, Purpose::Compute { .. }))
        );
    }

    #[test]
    fn configured_settings_start_unassigned_goal_only_after_explicit_approval() {
        for status in ["needsExecutorSetup", "available"] {
            let mut client = pending_task_client();
            let purpose = Purpose::Compute {
                workspace: "room".into(),
            };
            for _ in 0..2 {
                client.settle_invocation(
                    &purpose,
                    &succeeded(serde_json::json!({"workers":[], "status":status})),
                );
                assert!(client.pending.is_empty(), "discovery cannot approve");
                assert_ne!(client.views[""].receipt, AgentTaskReceipt::Started);
            }
            assert!(client.start(""));
            assert!(!client.start(""), "duplicate Start is held while pending");
            assert_eq!(client.pending.len(), 1);
            let approval = client.pending.remove(0);
            assert!(matches!(&approval.purpose, Purpose::Start { .. }));
            assert_eq!(approval.arguments["_action"], "approve-run");
            assert_eq!(approval.arguments["_resource"], "desktop:task:pending");
            assert_eq!(approval.arguments["agent_id"], "");
            assert_eq!(approval.arguments["expected_run_epoch"], 0);
            assert_eq!(approval.arguments["after_revision"], 4);
            assert_eq!(
                client.views[""].selected_identity(),
                Some(("task:pending", 0))
            );
            client.settle_invocation(
                &approval.purpose,
                &succeeded(serde_json::json!({"result":{"run_started":{
                    "task_id":"task:pending", "epoch":1
                }}})),
            );
            assert_eq!(
                client.views[""].selected_identity(),
                Some(("task:pending", 1))
            );
            assert_eq!(client.views[""].receipt, AgentTaskReceipt::Started);
        }
    }

    #[test]
    fn retained_compute_start_uses_protected_approval_without_prebinding() {
        for registered in [false, true] {
            let mut client = pending_task_client();
            client.settle_invocation(
                &Purpose::Compute {
                    workspace: "room".into(),
                },
                &succeeded(serde_json::json!({
                    "status": if registered { "available" } else { "needsExecutorSetup" },
                    "workers": [{"agent_id":"retained", "workspace":"room",
                        "profile":"Configured model", "configured":true,
                        "eligible":registered, "registered":registered}]
                })),
            );
            assert_eq!(client.eligible_compute("room").is_some(), registered);
            assert!(client.pending.is_empty(), "discovery cannot launch");
            assert!(client.start(""));
            let start = client.pending.remove(0);
            assert!(matches!(start.purpose, Purpose::Start { .. }));
            assert_eq!(start.arguments["_action"], "approve-run");
            assert_eq!(start.arguments["agent_id"], "retained");
            assert_eq!(start.arguments["expected_run_epoch"], 0);
            assert_eq!(start.arguments["after_revision"], 4);
            assert_ne!(client.views[""].receipt, AgentTaskReceipt::Started);
            assert_eq!(
                client.views[""].selected_identity(),
                Some(("task:pending", 0))
            );
        }
    }

    #[test]
    fn pending_first_start_can_stop_its_original_frontier_without_consuming_approval() {
        let mut client = pending_task_client();
        client.compute_ready = true;
        client.compute_status = "needsExecutorSetup".into();
        assert!(!client.views[""].can_stop(), "no approval yet");
        assert!(client.start(""));
        assert!(!client.views[""].can_start());
        assert!(client.views[""].can_stop());
        // A poll must not silently change the frontier being cancelled.
        client.views.get_mut("").unwrap().tasks[0].after_revision = Some(9);
        assert!(client.stop(""));
        assert!(!client.stop(""));
        assert_eq!(client.pending.len(), 2);
        let approval = client.pending.remove(0);
        let stop = client.pending.remove(0);
        assert!(matches!(approval.purpose, Purpose::Start { .. }));
        assert!(matches!(stop.purpose, Purpose::Stop { .. }));
        assert_eq!(stop.arguments["_resource"], approval.arguments["_resource"]);
        assert_eq!(
            stop.arguments["run_epoch"],
            approval.arguments["expected_run_epoch"]
        );
        assert_eq!(stop.arguments["after_revision"], 4);
        client.settle_invocation(
            &stop.purpose,
            &succeeded(serde_json::json!({
                "result":{"finished":{"task_id":"task:pending"}}
            })),
        );
        assert_ne!(client.views[""].receipt, AgentTaskReceipt::Started);
        assert_eq!(client.views[""].pending_start_revision, None);
        assert!(!client.views[""].can_stop());
        client.refuse(&approval.purpose);
        assert_eq!(client.views[""].pending_start_revision, None);
        assert_eq!(
            client.views[""].selected_identity(),
            Some(("task:pending", 0))
        );
        assert_eq!(client.views[""].receipt, AgentTaskReceipt::None);
    }

    #[test]
    fn approval_and_stop_receipts_remain_separate_when_start_wins() {
        let mut client = pending_task_client();
        client.compute_ready = true;
        client.compute_status = "available".into();
        assert!(client.start(""));
        assert!(client.stop(""));
        let approval = client.pending.remove(0);
        let stop = client.pending.remove(0);
        client.settle_invocation(
            &approval.purpose,
            &succeeded(serde_json::json!({
                "result":{"run_started":{"task_id":"task:pending","epoch":1}}
            })),
        );
        assert_eq!(client.views[""].receipt, AgentTaskReceipt::Started);
        client.refuse(&stop.purpose);
        assert_eq!(client.views[""].receipt, AgentTaskReceipt::Started);
        let original = &client.views.runs[&TaskRunKey {
            task_id: "task:pending".into(),
            run_epoch: 0,
        }];
        assert_eq!(original.receipt, AgentTaskReceipt::Refused);
        assert!(!original.stop_pending);
        assert_eq!(original.pending_start_revision, None);
    }

    #[test]
    fn late_approval_failure_does_not_mark_another_conversation_refused() {
        let mut client = pending_task_client();
        client.compute_ready = true;
        client.compute_status = "available".into();
        assert!(client.start(""));
        let approval = client.pending.remove(0);
        client.views.activate("", "task:other", 0);
        client.refuse(&approval.purpose);
        assert_eq!(client.views[""].receipt, AgentTaskReceipt::None);
        assert!(client.views[""].start_error.is_none());
        assert!(
            client.views.runs[&TaskRunKey {
                task_id: "task:pending".into(),
                run_epoch: 0,
            }]
                .start_error
                .is_some()
        );
    }

    #[test]
    fn missing_configuration_or_unknown_discovery_cannot_prepare_start() {
        for status in ["needsConfiguration", "", "unknown"] {
            let mut client = pending_task_client();
            client.settle_invocation(
                &Purpose::Compute {
                    workspace: "room".into(),
                },
                &succeeded(serde_json::json!({"workers":[], "status":status})),
            );
            assert!(!client.can_prepare_model_start());
            assert!(!client.start(""));
            assert!(
                client
                    .pending
                    .iter()
                    .all(|pending| matches!(pending.purpose, Purpose::Compute { .. }))
            );
            assert_eq!(
                client.views[""].selected_identity(),
                Some(("task:pending", 0))
            );
        }
    }

    #[test]
    fn pending_start_binds_configured_compute_then_approves_the_exact_frontier() {
        let mut client = pending_task_client();
        client.settle_invocation(
            &Purpose::Compute {
                workspace: "room".into(),
            },
            &succeeded(serde_json::json!({"workers":[{
                "agent_id":"configured-worker", "workspace":"room", "eligible":true,
                "configured":true, "registered":true, "profile":"Local model"
            }]})),
        );
        assert!(client.start(""));
        assert!(!client.start(""));
        let binding = client.pending.remove(0);
        assert_eq!(binding.arguments["_action"], "bind-executor");
        assert_eq!(binding.arguments["agent_id"], "configured-worker");
        assert_eq!(binding.arguments["expected_run_epoch"], 0);
        assert_eq!(binding.arguments["after_revision"], 4);
        client.settle_invocation(
            &binding.purpose,
            &succeeded(
                serde_json::json!({"result":{"metadata_updated":{"task_id":"task:pending"}}}),
            ),
        );
        let approval = client.pending.remove(0);
        assert_eq!(approval.arguments["_action"], "approve-run");
        assert_eq!(
            approval.arguments["_resource"],
            binding.arguments["_resource"]
        );
        for field in ["agent_id", "expected_run_epoch", "after_revision"] {
            assert_eq!(approval.arguments[field], binding.arguments[field]);
        }
        assert_eq!(
            client.views[""].selected_identity(),
            Some(("task:pending", 0))
        );
        assert_ne!(client.views[""].receipt, AgentTaskReceipt::Started);
        client.settle_invocation(
            &binding.purpose,
            &succeeded(serde_json::json!({"result":{"metadata_updated":{"task_id":"task:other"}}})),
        );
        assert!(
            client.pending.is_empty(),
            "a mismatched receipt cannot approve"
        );
        assert_eq!(client.views[""].receipt, AgentTaskReceipt::Refused);
    }

    #[test]
    fn answer_capture_projects_the_exact_private_finish_report() {
        let mut client = AgentTaskClient::new();
        client.stage_answer_capture("room", "scribe");
        let view = client.view("scribe").unwrap();
        let report = view.reports.last().unwrap();
        assert_eq!(
            report.kind,
            tilo_actions::task_messaging::TaskReportKind::Finish
        );
        assert_eq!(report.task_run.task_id, "task:review");
        assert_eq!(report.task_run.run_epoch, 7);
        assert_eq!(report.task_run.agent_id, "scribe");
        assert_eq!(report.actor, "agent:scribe");
        assert_eq!(view.selected_identity(), Some(("task:review", 7)));
        let task = view.current().unwrap();
        assert_eq!(task.status, "done");
        assert!(task.active_agent.is_none());
        assert!(task.execution.is_none());
        assert!(!view.can_steer());
        assert!(!view.can_stop());
        let messages = view.conversation_messages("private", "me");
        let answer = messages.last().unwrap();
        assert_eq!(answer.author, "task-report:task:review");
        assert_eq!(answer.text, report.body);
        assert_eq!(
            answer.text,
            "Example review complete. No changes made.\n\nThe existing open action accepts a result anchor, so search can open the matching passage directly. Keep that action rather than adding a separate navigation path.\n\nFor the narrow layout, keep the result title and passage preview in one column. Verify that opening a result reaches the same anchor from both the search UI and an action call."
        );
    }

    #[test]
    fn historical_run_remains_exact_when_same_agent_moves_to_another_task() {
        let mut client = AgentTaskClient::new();
        client.stage_capture("room", "scribe");
        client.views.get_mut("scribe").unwrap().reports.clear();
        assert!(client.view("scribe").unwrap().can_steer());
        assert!(client.view("scribe").unwrap().can_stop());
        let original = client.views["scribe"].current().unwrap().clone();
        let mut replacement = original.clone();
        replacement.task_id = "task:replacement".into();
        replacement.run_epoch = Some(8);
        client
            .views
            .get_mut("scribe")
            .unwrap()
            .apply_snapshot(AgentTaskSnapshot {
                complete: true,
                total: 1,
                tasks: vec![replacement],
            });
        client.settle_invocation(
            &Purpose::ReadTask {
                agent: "scribe".into(),
                task_id: original.task_id.clone(),
                run_epoch: 7,
            },
            &succeeded(serde_json::json!({
                "task_id":original.task_id, "title":"Original", "assigned_agent":"other",
                "run_epoch":9, "active_agent":"other", "after_revision":3, "status":"in-progress"
            })),
        );
        client.settle_invocation(
            &Purpose::Conversation {
                agent: "scribe".into(),
                task_id: original.task_id.clone(),
                run_epoch: 7,
            },
            &succeeded(serde_json::json!({
                "task_id":original.task_id, "run_epoch":7, "frontier":{
                    "task_id":original.task_id, "title":"Original", "assigned_agent":"scribe",
                    "run_epoch":7, "status":"ended"
                }, "turns":[{"task_id":original.task_id,"run_epoch":7,
                    "text":"Original private conversation", "received_at":1}]
            })),
        );
        let view = client.view("scribe").unwrap();
        assert_eq!(view.current().unwrap().task_id, original.task_id);
        assert_eq!(view.current().unwrap().status, "ended");
        assert_eq!(view.draft_route().as_deref(), Some("task:task:review:7"));
        assert_eq!(view.conversation_messages("private", "me").len(), 1);
        assert!(!view.can_steer());
        assert!(!view.can_stop());
        assert!(!view.can_start());
        assert!(!client.direct("scribe", "Must not reach replacement".into(), 1));
    }

    #[test]
    fn task_mail_keeps_sender_provenance_and_accepted_is_not_read_or_direction() {
        let mut client = AgentTaskClient::new();
        client.stage_capture("room", "scribe");
        client.views.get_mut("scribe").unwrap().reports.clear();
        let purpose = Purpose::Conversation {
            agent: "scribe".into(),
            task_id: "task:review".into(),
            run_epoch: 7,
        };
        let page = |sequence: u64, more: bool| {
            succeeded(serde_json::json!({
                "task_id":"task:review","run_epoch":7,"turns":[{
                    "task_id":"task:review","run_epoch":7,"kind":"MessageFromTask",
                    "message_id":format!("00000000-0000-4000-8000-{sequence:012}"),"sequence":sequence,
                    "author_task_run":{"task_id":"task:sender","run_epoch":2,"agent_id":"other-worker"},
                    "text":"Untrusted private work context","received_at":sequence,"status":"Accepted"
                }],"next_message_sequence":sequence,"messages_has_more":more
            }))
        };
        client.settle_invocation(&purpose, &page(1, true));
        assert_eq!(
            client.pending.last().unwrap().arguments["after_message_sequence"],
            1
        );
        client.settle_invocation(&purpose, &page(2, false));
        let messages = client.views["scribe"].conversation_messages("private", "me");
        assert_eq!(messages.len(), 2);
        assert_ne!(messages[0].id, messages[1].id);
        assert!(
            messages
                .iter()
                .all(|message| message.author == "task-message:task:sender")
        );
        assert!(
            messages
                .iter()
                .all(|message| message.text.contains("Private work context")
                    && message.text.contains("Accepted"))
        );
        assert!(
            messages.iter().all(
                |message| !message.text.contains("Delivered") && !message.text.contains("Read")
            )
        );
    }

    #[test]
    fn output_rows_require_exact_run_receipts_and_open_only_the_published_resource() {
        let mut client = AgentTaskClient::new();
        client.stage_capture("room", "scribe");
        let purpose = Purpose::Conversation {
            agent: "scribe".into(),
            task_id: "task:review".into(),
            run_epoch: 7,
        };
        let receipt = serde_json::json!({
            "version":1,"task_run":{"task_id":"task:review","run_epoch":7,"agent_id":"scribe"},
            "invocation":"00000000-0000-4000-8000-000000000003","provider":"so.tilo.notes",
            "resource":"notes/document/actual","open_action":"open","durable_proof":"a".repeat(64),"status":"Returned"
        });
        let mut wrong_run = receipt.clone();
        wrong_run["task_run"]["run_epoch"] = Value::from(8);
        client.settle_invocation(
            &purpose,
            &succeeded(serde_json::json!({
                "task_id":"task:review","run_epoch":7,"turns":[],"outputs":[receipt,wrong_run]
            })),
        );
        assert_eq!(client.outputs().len(), 1);
        let output = (*client.outputs()[0]).clone();
        assert_eq!(
            output.status,
            tilo_actions::task_messaging::TaskOutputStatus::Returned
        );
        assert!(client.open_output(&output));
        let pending = client.pending.last().unwrap();
        assert!(
            matches!(&pending.purpose, Purpose::OpenOutput { provider } if provider == "so.tilo.notes")
        );
        assert_eq!(pending.arguments["_resource"], "notes/document/actual");
        assert_eq!(pending.arguments["_action"], "open");
        assert_eq!(
            pending.arguments.len(),
            2,
            "no private callback, URL or guessed arguments"
        );
        let mut forged = output;
        forged.resource = "notes/document/guessed".into();
        assert!(!client.open_output(&forged));
        client.settle_invocation(
            &purpose,
            &succeeded(serde_json::json!({
                "task_id":"task:review","run_epoch":7,"turns":[{
                    "task_id":"task:review","run_epoch":7,"revision":1,"received_at":1,
                    "kind":"agent-output","text":"I created a file","author":"agent:scribe"
                }]
            })),
        );
        assert!(
            client.outputs().is_empty(),
            "work narration is never an output receipt"
        );
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

    #[test]
    fn exact_private_answers_survive_duplicate_and_older_polls() {
        let mut client = AgentTaskClient::new();
        client.stage_capture("room", "scribe").reports.clear();
        let purpose = Purpose::Conversation {
            agent: "scribe".into(),
            task_id: "task:review".into(),
            run_epoch: 7,
        };
        let body = format!("  Exact answer\n{}\n最後\n", "long answer ".repeat(60));
        let page = |reports: Vec<Value>| {
            succeeded(serde_json::json!({
                "task_id":"task:review", "run_epoch":7, "turns":[], "reports":reports
            }))
        };
        let read = page(vec![private_report(1, 2, &body)]);
        client.settle_invocation(&purpose, &read);
        client.settle_invocation(&purpose, &read);
        client.settle_invocation(&purpose, &page(vec![]));
        client.settle_invocation(
            &purpose,
            &page(vec![private_report(1, 2, "conflicting body")]),
        );
        let messages = client.views["scribe"].conversation_messages("private", "me");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, body);
        assert_eq!(messages[0].ts, 2);
        assert_eq!(messages[0].author, "task-report:task:review");
        assert!(messages[0].id.contains("task:review:7:scribe:"));
        assert!(messages[0].id.ends_with(":1"));
    }

    #[test]
    fn public_neutral_summary_is_never_a_private_answer() {
        let mut client = AgentTaskClient::new();
        client.stage_capture("room", "scribe").reports.clear();
        let purpose = Purpose::Conversation {
            agent: "scribe".into(),
            task_id: "task:review".into(),
            run_epoch: 7,
        };
        client.settle_invocation(
            &purpose,
            &succeeded(serde_json::json!({
                "task_id":"task:review", "run_epoch":7, "turns":[{
                    "task_id":"task:review", "run_epoch":7, "revision":3,
                    "kind":"agent-output", "author":"agent:scribe",
                    "text":"task-report-finished", "received_at":2
                }]
            })),
        );
        assert!(
            client.views["scribe"]
                .conversation_messages("private", "me")
                .is_empty()
        );
    }

    #[test]
    fn private_report_identity_includes_invocation_not_just_sequence() {
        let mut client = AgentTaskClient::new();
        client.stage_capture("room", "scribe").reports.clear();
        let purpose = Purpose::Conversation {
            agent: "scribe".into(),
            task_id: "task:review".into(),
            run_epoch: 7,
        };
        let mut second = private_report(1, 3, "Second invocation");
        second["invocation"] = private_report(2, 3, "")["invocation"].clone();
        client.settle_invocation(
            &purpose,
            &succeeded(serde_json::json!({
                "task_id":"task:review", "run_epoch":7, "turns":[],
                "reports":[private_report(1, 2, "First invocation"), second]
            })),
        );
        let messages = client.views["scribe"].conversation_messages("private", "me");
        assert_eq!(messages.len(), 2);
        assert_ne!(messages[0].id, messages[1].id);
        assert_eq!(messages[0].text, "First invocation");
        assert_eq!(messages[1].text, "Second invocation");
    }

    #[test]
    fn private_report_scope_and_role_fail_closed() {
        let mut client = AgentTaskClient::new();
        client.stage_capture("room", "scribe").reports.clear();
        let purpose = Purpose::Conversation {
            agent: "scribe".into(),
            task_id: "task:review".into(),
            run_epoch: 7,
        };
        for (field, value) in [
            ("role", Value::from("user")),
            ("actor", Value::from("agent:foreign")),
            ("sequence", Value::from(0)),
        ] {
            let mut report = private_report(1, 2, "Not an authenticated assistant answer");
            report[field] = value;
            client.settle_invocation(
                &purpose,
                &succeeded(serde_json::json!({
                    "task_id":"task:review", "run_epoch":7, "turns":[], "reports":[report]
                })),
            );
            assert!(client.views["scribe"].reports.is_empty());
        }
        let mut report = private_report(1, 2, "Replacement answer");
        report["task_run"]["run_epoch"] = Value::from(8);
        client.settle_invocation(
            &purpose,
            &succeeded(serde_json::json!({
                "task_id":"task:review", "run_epoch":7, "turns":[], "reports":[report]
            })),
        );
        assert!(client.views["scribe"].reports.is_empty());
    }

    #[test]
    fn historical_report_reply_never_replaces_selected_new_run() {
        let mut client = AgentTaskClient::new();
        client.stage_capture("room", "scribe").reports.clear();
        let purpose = Purpose::Conversation {
            agent: "scribe".into(),
            task_id: "task:review".into(),
            run_epoch: 7,
        };
        let read = succeeded(serde_json::json!({
            "task_id":"task:review", "run_epoch":7, "turns":[],
            "reports":[private_report(1, 2, "Historical exact answer")]
        }));
        client.settle_invocation(&purpose, &read);
        client.views.activate("scribe", "task:review", 8);
        client.settle_invocation(&purpose, &read);
        assert!(client.views["scribe"].reports.is_empty());
        client.views.activate("scribe", "task:review", 7);
        assert_eq!(
            client.views["scribe"].reports[0].body,
            "Historical exact answer"
        );
        // A fresh Chat instance hydrates the same stable message identity.
        let original = client.views["scribe"].conversation_messages("private", "me");
        let mut reopened = AgentTaskClient::new();
        reopened.stage_capture("room", "scribe").reports.clear();
        reopened.settle_invocation(&purpose, &read);
        let restored = reopened.views["scribe"].conversation_messages("private", "me");
        assert_eq!(original[0].id, restored[0].id);
        assert_eq!(original[0].text, restored[0].text);
    }

    fn private_report(sequence: u64, seconds: u64, body: &str) -> Value {
        serde_json::json!({
            "invocation": format!("00000000-0000-4000-8000-{sequence:012}"),
            "source_device": ([1; 32].to_vec()), "role":"assistant",
            "accepted_at_unix_ns": seconds * 1_000_000_000, "sequence": sequence,
            "task_run":{"task_id":"task:review","run_epoch":7,"agent_id":"scribe"},
            "actor":"agent:scribe", "kind":"finish", "body":body
        })
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
            client.pending[0].arguments["title"],
            crate::i18n::t("chat-task-neutral-title")
        );
        assert!(!instruction.contains(client.pending[0].arguments["title"].as_str().unwrap()));
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
        assert!(matches!(
            &client.pending[0].purpose,
            Purpose::ReadTask { task_id, run_epoch: 0, .. } if task_id == "task:new"
        ));
        assert_eq!(client.pending[0].arguments["_resource"], "desktop:task:new");
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
        client.views.get_mut("scribe").unwrap().reports.clear();
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
        client.views.get_mut("scribe").unwrap().reports.clear();
        let purpose = Purpose::Conversation {
            agent: "scribe".into(),
            task_id: "task:review".into(),
            run_epoch: 7,
        };
        let read = succeeded(serde_json::json!({
            "task_id":"task:review", "run_epoch":7, "turns":[
                {"task_id":"task:review","run_epoch":7,"revision":2,"author":"person","text":"Please update the document","received_at":1},
                {"task_id":"task:review","run_epoch":7,"revision":2,"kind":"agent-output","author":"agent:scribe","text":"Automatic report accepted","received_at":2}
            ], "reports":[private_report(1, 2, "Appended the requested paragraph."), private_report(2, 3, "The document is updated.")]
        }));
        client.settle_invocation(&purpose, &read);
        client.settle_invocation(&purpose, &read);
        let messages = client.views["scribe"].conversation_messages("conversation", "me");
        assert_eq!(messages.len(), 3);
        assert_ne!(messages[0].id, messages[1].id);
        assert_eq!(messages[1].text, "Appended the requested paragraph.");
        assert_eq!(messages[2].text, "The document is updated.");
        assert_eq!(messages[1].author, "task-report:task:review");
        assert_eq!(messages[2].author, "task-report:task:review");
        assert_ne!(messages[1].id, messages[2].id);
        assert_eq!(messages[1].ts, 2);
        assert_eq!(messages[2].ts, 3);
        let groups = crate::model::group_messages(&messages);
        assert_eq!(
            groups.len(),
            2,
            "consecutive assistant updates share a header"
        );
        assert_eq!(groups[0].message_indices, vec![0]);
        assert_eq!(groups[1].message_indices, vec![1, 2]);
        assert_eq!(
            client.views["scribe"].reports.len(),
            2,
            "grouping must retain every durable report"
        );
    }

    #[test]
    fn private_reports_are_time_ordered_and_public_summaries_are_not_answers() {
        let mut client = AgentTaskClient::new();
        client.stage_capture("room", "scribe");
        client.views.get_mut("scribe").unwrap().reports.clear();
        let purpose = Purpose::Conversation {
            agent: "scribe".into(),
            task_id: "task:review".into(),
            run_epoch: 7,
        };
        client.settle_invocation(&purpose, &succeeded(serde_json::json!({
            "task_id":"task:review", "run_epoch":7, "turns":[
                {"task_id":"task:review","run_epoch":7,"revision":2,"author":"person","text":"Keep the original","received_at":30},
                {"task_id":"task:review","run_epoch":7,"revision":2,"kind":"agent-output","author":"agent:scribe","text":"Automatic report accepted","received_at":20}
            ], "reports":[private_report(1, 20, "Read the resource")]
        })));
        let messages = client.views["scribe"].conversation_messages("conversation", "me");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].author, "task-report:task:review");
        assert_eq!(messages[0].text, "Read the resource");
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
    fn late_start_receipt_never_retargets_another_task_using_the_same_compute() {
        let mut client = AgentTaskClient::new();
        client.stage_queued_capture("room", "scribe");
        let mut other = client.view("scribe").unwrap().tasks[0].clone();
        other.task_id = "task:other".into();
        client
            .views
            .get_mut("scribe")
            .unwrap()
            .tasks
            .push(other.clone());
        assert!(client.start("scribe"));
        let start = client.pending.remove(0);
        assert!(client.select("scribe", &other));
        client.settle_invocation(
            &start.purpose,
            &succeeded(serde_json::json!({
                "result":{"run_started":{"task_id":"task:review", "epoch":1}}
            })),
        );
        assert_eq!(
            client.view("scribe").unwrap().selected_identity(),
            Some(("task:other", 0))
        );
        assert!(client.views.runs.contains_key(&TaskRunKey {
            task_id: "task:review".into(),
            run_epoch: 1
        }));
    }

    #[test]
    fn late_direction_receipt_stays_with_its_exact_task_run() {
        let mut client = AgentTaskClient::new();
        client.stage_subtask_capture("room", "scribe");
        assert!(client.direct("scribe", "Only parent direction".into(), 5));
        let direction = client.pending.remove(0);
        let child = client.view("scribe").unwrap().tasks[1].clone();
        assert!(client.select("scribe", &child));
        client.settle_invocation(
            &direction.purpose,
            &succeeded(serde_json::json!({
                "result":{"direction_added":{"task_id":"task:review", "revision":7}}
            })),
        );
        let view = client.view("scribe").unwrap();
        assert_eq!(view.selected_identity(), Some(("task:review-child", 0)));
        assert!(view.conversation_messages("child", "me").is_empty());
        assert!(!matches!(view.receipt, AgentTaskReceipt::Saved { .. }));
        assert!(matches!(
            client.views.runs[&TaskRunKey {
                task_id: "task:review".into(),
                run_epoch: 7
            }]
                .receipt,
            AgentTaskReceipt::Saved { revision: 7, .. }
        ));
    }

    #[test]
    fn unassigned_saved_goal_reads_its_exact_owner_frontier_without_an_empty_agent_query() {
        let mut client = AgentTaskClient::new();
        client.workspace = "room".into();
        assert!(client.follow("room", "", "task:saved", 0));
        assert_eq!(client.pending.len(), 1);
        let pending = client.pending.remove(0);
        assert!(matches!(
            &pending.purpose,
            Purpose::ReadTask { task_id, run_epoch: 0, agent }
                if task_id == "task:saved" && agent.is_empty()
        ));
        assert_eq!(pending.arguments["_resource"], "desktop:task:saved");
        client.settle_invocation(
            &pending.purpose,
            &succeeded(serde_json::json!({
                "task_id":"task:saved", "title":"Saved task", "assigned_agent":null,
                "run_epoch":0, "after_revision":0, "status":"queued"
            })),
        );
        let view = client.view("").unwrap();
        assert_eq!(view.selected_identity(), Some(("task:saved", 0)));
        assert!(view.can_start());
        assert!(!view.can_steer());
        assert!(!view.can_stop());
        assert!(matches!(&client.pending[0].purpose,
            Purpose::Conversation { task_id, run_epoch: 0, .. } if task_id == "task:saved"));
    }

    #[test]
    fn setup_distinguishes_a_missing_model_from_a_missing_registered_worker() {
        let mut client = AgentTaskClient::new();
        let purpose = Purpose::Compute {
            workspace: "room".into(),
        };
        for (status, model, worker) in [
            ("needsConfiguration", true, false),
            ("needsExecutorSetup", false, true),
            ("available", false, false),
        ] {
            client.settle_invocation(
                &purpose,
                &succeeded(serde_json::json!({
                    "workers":[], "status":status
                })),
            );
            assert!(client.compute_ready());
            assert_eq!(client.needs_model_setup(), model);
            assert_eq!(client.needs_worker_setup(), worker);
            assert_eq!(client.needs_setup(), model);
            assert_eq!(client.can_prepare_model_start(), !model);
            assert_eq!(client.needs_compute_controls(), model);
            assert!(client.eligible_compute("room").is_none());
        }
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
