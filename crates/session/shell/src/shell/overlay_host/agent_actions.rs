//! Nonblocking adapter from private composer intentions to the existing
//! certified semantic action door. It owns no task or Chat document state.
//!
//! A request goes to the agent's own entry point (`respond`), which answers
//! it or names the work; work is queued and started through Task Center,
//! so it runs as an ordinary attributed, undoable task, and this adapter
//! follows that run until it ends. When the agent cannot be asked, the
//! request is queued as it stands and the run decides.

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Map, Value};
use tilo_actions::remote::{
    ActionHostError, CataloguePage, CatalogueQuery, InvocationReceipt, InvokeArgument,
    InvokeRequest, Pending, PendingPoll, ReceiptStatus, workspace_provider,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};

use crate::shell::agent_quick_composer::{
    AnswerUse, ComposerIntent, ComposerOutcome, ComposerOwner, ComposerReceipt, ComposerRequest,
    ComposerRun, ComposerTask, ComposerTurn, RunPhase, SelectionComposer,
};
use crate::shell::certified_action_session::{ActionSessionSource, DesktopActionSession};
use crate::shell::search::invocation_wake::{poll, waker, watch};
use crate::shell::selection_agent::SelectionSeed;

const REFRESH: Duration = Duration::from_secs(1);
const TIMEOUT: Duration = Duration::from_secs(30);
/// The agent's own answer may take its whole model budget, and a little more.
const RESPOND_TIMEOUT: Duration = Duration::from_secs(45);
const TASKS: &str = "desktop:tasks";
const DESKTOP: &str = tilo_desktop_model::commands::APP_ID;
const AGENT: &str = tilo_automation::AGENT_APP_ID;
/// How often a queued run is asked to start before the composer gives up.
const START_ATTEMPTS: u8 = 5;
/// The longest task title, in characters.
const TITLE_CHARS: usize = 120;

#[derive(Clone)]
enum Purpose {
    Current,
    /// Ask the agent to answer the request or name its work.
    Respond(ComposerRequest),
    /// Queue work: a request's, or what the person chose to do with an answer.
    Queue {
        request: Option<ComposerRequest>,
        title: String,
    },
    /// Start the queued run.
    Start,
    /// Read the followed task.
    Follow,
    /// Read what the agent said in the followed run.
    Said,
    /// A direction for, or Stop of, the followed run.
    Submit(ComposerRequest),
    Applied {
        request: ComposerRequest,
        saved: ComposerTask,
    },
}

impl Purpose {
    /// Whether this is something the person asked for, never deduplicated.
    fn submitted(&self) -> bool {
        matches!(
            self,
            Self::Respond(_) | Self::Queue { .. } | Self::Submit(_)
        )
    }
}

#[derive(Clone)]
struct Operation {
    resource: String,
    action: String,
    arguments: Map<String, Value>,
    purpose: Purpose,
    /// The application whose provider publishes the action, when it is not
    /// the room's desktop.
    app: Option<&'static str>,
}

enum Phase {
    Providers(Pending<Result<CataloguePage, ActionHostError>>),
    Resolve {
        provider: String,
        answer: Pending<Result<CataloguePage, ActionHostError>>,
        pages: u8,
        matched: Vec<tilo_actions::remote::CatalogueRow>,
    },
    Invoke {
        invocation: InvocationId,
        answer: Pending<Result<InvocationReceipt, ActionHostError>>,
    },
    Observe {
        invocation: InvocationId,
        changed: Option<Pending<()>>,
    },
}

struct Call {
    operation: Operation,
    phase: Phase,
    deadline: Instant,
}

#[derive(Clone, Deserialize)]
struct Frontier {
    task_id: String,
    title: String,
    assigned_agent: Option<String>,
    active_agent: Option<String>,
    status: String,
    route: String,
    run_epoch: Option<u64>,
    after_revision: Option<u64>,
    applied_revision: Option<u64>,
}

impl Frontier {
    fn choice(&self, owner: &ComposerOwner) -> Option<ComposerTask> {
        let epoch = self.run_epoch?;
        let revision = self.after_revision?;
        (epoch > 0
            && self.assigned_agent.as_deref() == Some(&owner.agent)
            && self.active_agent.as_deref() == Some(&owner.agent)
            && self.status == "in_progress"
            && self.route == "model")
            .then(|| ComposerTask {
                task_id: self.task_id.clone(),
                run_epoch: epoch,
                revision,
                label: self.title.clone(),
            })
    }

    /// The run's phase, when the task has ended.
    fn ended(&self) -> Option<RunPhase> {
        match self.status.as_str() {
            "done" => Some(RunPhase::Finished(None)),
            "waiting" => Some(RunPhase::NeedsAttention(None)),
            "cancelled" => Some(RunPhase::Stopped),
            _ => None,
        }
    }
}

/// The task the composer follows: one it queued, or the one its agent was
/// working on when the person opened it.
struct Followed {
    task_id: String,
    title: String,
    /// Queued here, so started here.
    ours: bool,
    started: bool,
    start_attempts: u8,
    frontier: Option<Frontier>,
    /// How the run ended, with what the agent said once that is read.
    ended: Option<RunPhase>,
    /// Whether what the agent said at the end has been read.
    said: bool,
}

impl Followed {
    fn new(task_id: String, title: String, ours: bool) -> Self {
        Self {
            task_id,
            title,
            ours,
            started: !ours,
            start_attempts: 0,
            frontier: None,
            ended: None,
            said: false,
        }
    }

    /// The run as the composer shows it. An ended run still shows as
    /// working until what the agent said at the end is read, so it is
    /// told once, with those words.
    fn run(&self, owner: &ComposerOwner) -> ComposerRun {
        let current = self
            .frontier
            .as_ref()
            .and_then(|frontier| frontier.choice(owner));
        let phase = match &self.ended {
            Some(phase) if self.said || *phase == RunPhase::Stopped => phase.clone(),
            Some(_) => RunPhase::Working,
            None if current.is_some() => RunPhase::Working,
            None => RunPhase::Starting,
        };
        ComposerRun {
            task: ComposerTask {
                task_id: self.task_id.clone(),
                run_epoch: self.epoch(),
                revision: current.map_or(0, |task| task.revision),
                label: self.title.clone(),
            },
            phase,
        }
    }

    fn epoch(&self) -> u64 {
        self.frontier
            .as_ref()
            .and_then(|frontier| frontier.run_epoch)
            .unwrap_or_default()
    }
}

#[derive(Default)]
pub(super) struct SelectionActions {
    owner: Option<ComposerOwner>,
    source: Option<ActionSessionSource>,
    session: Option<DesktopActionSession>,
    calls: Vec<Call>,
    /// The agent's current runs, for following the one it is working on.
    tasks: Vec<ComposerTask>,
    followed: Option<Followed>,
    /// A run the person stopped or that ended is not followed again.
    retired: Vec<String>,
    refresh_at: Option<Instant>,
    follows: Vec<(ComposerRequest, ComposerTask, Instant, Instant)>,
}

impl SelectionActions {
    pub(super) fn open(&mut self, source: ActionSessionSource, owner: ComposerOwner) -> bool {
        // Opening chrome must not retire a still-observable mutation or an
        // exact-run Saved receipt. It is never a cancellation or a retry.
        if self.owner.as_ref() == Some(&owner) && self.session.is_some() {
            return true;
        }
        if self
            .calls
            .iter()
            .any(|call| call.operation.purpose.submitted())
            || !self.follows.is_empty()
        {
            return false;
        }
        let Some(revision) = source.identity_revision(&owner.room) else {
            return false;
        };
        if revision != owner.identity_revision {
            return false;
        }
        let Ok(session) = source.open(&owner.room, Some(&owner.person)) else {
            return false;
        };
        self.owner = Some(owner);
        self.source = Some(source);
        self.session = Some(session);
        self.calls.clear();
        self.tasks.clear();
        self.follows.clear();
        self.followed = None;
        self.refresh_at = Some(Instant::now());
        true
    }

    fn begin(&mut self, operation: Operation) {
        if self
            .calls
            .iter()
            .any(|call| same_read(&call.operation, &operation))
        {
            return;
        }
        let (Some(session), Some(_)) = (&self.session, &self.owner) else {
            return;
        };
        let deadline = deadline_after(if matches!(operation.purpose, Purpose::Respond(_)) {
            RESPOND_TIMEOUT
        } else {
            TIMEOUT
        });
        // Who publishes the action in this room is asked first: the agent's
        // own provider, or the desktop's, which a native room serves as an
        // application's.
        let phase = Phase::Providers(watch(session.query(CatalogueQuery {
            max_results: 0,
            ..CatalogueQuery::default()
        })));
        self.calls.push(Call {
            operation,
            phase,
            deadline,
        });
    }

    fn refresh(&mut self) {
        let Some(owner) = &self.owner else {
            return;
        };
        let agent = owner.agent.clone();
        self.refresh_at = Some(deadline_after(REFRESH));
        let Some(followed) = &self.followed else {
            self.begin(task_read(
                TASKS,
                "read-current-tasks",
                Map::from_iter([("agent".into(), agent.into())]),
                Purpose::Current,
            ));
            return;
        };
        let task = format!("desktop:{}", followed.task_id);
        if followed.said {
            return;
        }
        if followed.ended.is_some() {
            let epoch = followed.epoch();
            self.begin(task_read(
                &task,
                "read-conversation",
                Map::from_iter([("run_epoch".into(), epoch.into())]),
                Purpose::Said,
            ));
            return;
        }
        let start = followed.ours
            && !followed.started
            && followed
                .frontier
                .as_ref()
                .is_none_or(|frontier| frontier.status == "queued");
        self.begin(task_read(&task, "read-task", Map::new(), Purpose::Follow));
        if start {
            self.begin(Operation {
                resource: task,
                action: "approve-run".into(),
                arguments: Map::from_iter([
                    ("agent_id".into(), agent.into()),
                    ("expected_run_epoch".into(), 0.into()),
                    ("after_revision".into(), 0.into()),
                ]),
                purpose: Purpose::Start,
                app: None,
            });
        }
    }

    /// The run the composer shows: the followed one, or the agent's single
    /// current run, which the composer then follows.
    fn run(&mut self, owner: &ComposerOwner, composer: &SelectionComposer) -> Option<ComposerRun> {
        if self.followed.is_none()
            && let [task] = self.tasks.as_slice()
            && !self.retired.contains(&task.task_id)
            && !self
                .calls
                .iter()
                .any(|call| call.operation.purpose.submitted())
            && composer.followed().is_none()
        {
            self.followed = Some(Followed::new(
                task.task_id.clone(),
                task.label.clone(),
                false,
            ));
            self.refresh_at = Some(Instant::now());
            return Some(ComposerRun {
                task: task.clone(),
                phase: RunPhase::Working,
            });
        }
        self.followed.as_ref().map(|followed| followed.run(owner))
    }

    pub(super) fn pump(&mut self, composer: &mut SelectionComposer) -> bool {
        let Some(owner) = self.owner.clone() else {
            return false;
        };
        let active = composer.owner() == Some(&owner) && composer.is_open();
        if !active {
            self.refresh_at = None;
        }
        if self
            .source
            .as_ref()
            .and_then(|source| source.identity_revision(&owner.room))
            != Some(owner.identity_revision)
        {
            let changed = self.session.is_some();
            for call in std::mem::take(&mut self.calls) {
                self.refuse(&call.operation, composer, true);
            }
            self.tasks.clear();
            self.followed = None;
            self.session = None;
            self.refresh_at = None;
            self.follows.clear();
            composer.project(&owner, None);
            return changed && active;
        }
        if active && self.refresh_at.is_none_or(|due| Instant::now() >= due) {
            self.refresh();
        }
        let pending = std::mem::take(&mut self.calls);
        let mut changed = false;
        for mut call in pending {
            if Instant::now() >= call.deadline {
                self.refuse(&call.operation, composer, true);
                changed = true;
                continue;
            }
            if self.advance(&mut call, composer) {
                self.calls.push(call);
            } else {
                changed = true;
            }
        }
        self.follow_applied();
        if !active {
            return changed;
        }
        let run = self.run(&owner, composer);
        if run.as_ref().is_some_and(|run| !run.phase.running()) {
            // Told now; following ends with it.
            if let Some(followed) = self.followed.take() {
                self.retired.push(followed.task_id);
            }
        }
        if composer.followed() != run.as_ref() {
            changed = true;
        }
        composer.project(&owner, run);
        if let Some(request) = composer.take_request() {
            self.submit(request);
            changed = true;
        }
        if let Some(choice) = composer.take_answer_use() {
            let about = composer.about().cloned();
            self.use_answer(&owner, choice, about.as_ref());
            changed = true;
        }
        changed
    }

    fn submit(&mut self, request: ComposerRequest) {
        if request.stop
            && let Some(task) = &request.task
        {
            self.retired.push(task.task_id.clone());
        }
        if let Some(operation) = submission_operation(request) {
            self.begin(operation);
        }
    }

    /// Hand what the person chose to do with an answer to the agent as work.
    /// An answer put at the selection carries that selection, so the agent
    /// is told the exact place.
    fn use_answer(
        &mut self,
        owner: &ComposerOwner,
        choice: AnswerUse,
        about: Option<&SelectionSeed>,
    ) {
        let (title, instruction, about) = match choice {
            AnswerUse::Insert(text) => (
                "Insert the answer at the selection",
                format!(
                    "Put this text where my selection is, in place of the selected text:\n<<<\n{text}\n>>>"
                ),
                about,
            ),
            AnswerUse::SaveAsNote(text) => (
                "Save the answer as a note",
                format!("Save this text as a new note:\n<<<\n{text}\n>>>"),
                None,
            ),
        };
        self.begin(queue_operation(owner, None, title, &instruction, about));
    }

    fn advance(&mut self, call: &mut Call, composer: &mut SelectionComposer) -> bool {
        let Some(session) = self.session.as_ref() else {
            self.refuse(&call.operation, composer, true);
            return false;
        };
        let step = match &mut call.phase {
            Phase::Providers(answer) => {
                provider_step(session, self.owner.as_ref(), &call.operation, answer)
            }
            Phase::Resolve {
                provider,
                answer,
                pages,
                matched,
            } => resolve_step(session, &call.operation, provider, answer, pages, matched),
            Phase::Invoke { invocation, answer } => invoke_step(session, *invocation, answer),
            Phase::Observe {
                invocation,
                changed,
            } => observe_step(session, *invocation, changed),
        };
        match step {
            Step::Waiting => true,
            Step::Next(phase) => {
                call.phase = phase;
                true
            }
            Step::Complete(value) => {
                self.complete(&call.operation, value, composer);
                false
            }
            Step::Refused(unknown) => {
                self.refuse(&call.operation, composer, unknown);
                false
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one match keeps every answer's handling, and so the whole request flow, in one place"
    )]
    fn complete(
        &mut self,
        operation: &Operation,
        value: Option<Value>,
        composer: &mut SelectionComposer,
    ) {
        match &operation.purpose {
            Purpose::Current => {
                self.tasks = self
                    .owner
                    .as_ref()
                    .map(|owner| current_tasks(owner, value.as_ref()))
                    .unwrap_or_default();
            }
            Purpose::Respond(request) => match respond_outcome(value.as_ref()) {
                Some(Responded::Answer(answer)) => composer.observe_receipt(ComposerReceipt {
                    owner: request.owner.clone(),
                    submission: request.submission,
                    outcome: ComposerOutcome::Answered(answer),
                }),
                Some(Responded::Work(title)) => {
                    self.begin(request_queue(request, Some(title)));
                }
                None => self.begin(request_queue(request, None)),
            },
            Purpose::Queue { request, title } => {
                let queued = self
                    .owner
                    .as_ref()
                    .and_then(|owner| queued_task(owner, value.as_ref()));
                if let Some(task_id) = &queued {
                    self.followed = Some(Followed::new(task_id.clone(), title.clone(), true));
                    self.refresh_at = Some(Instant::now());
                }
                if let Some(request) = request {
                    composer.observe_receipt(ComposerReceipt {
                        owner: request.owner.clone(),
                        submission: request.submission,
                        outcome: queued.map_or(ComposerOutcome::Unknown, |task_id| {
                            ComposerOutcome::Started {
                                task_id,
                                title: title.clone(),
                            }
                        }),
                    });
                }
            }
            Purpose::Start => {
                if let Some(followed) = &mut self.followed
                    && operation.resource == format!("desktop:{}", followed.task_id)
                {
                    followed.started = true;
                    self.refresh_at = Some(Instant::now());
                }
            }
            Purpose::Follow => {
                let Some(frontier) =
                    value.and_then(|value| serde_json::from_value::<Frontier>(value).ok())
                else {
                    return;
                };
                if let Some(followed) = &mut self.followed
                    && followed.task_id == frontier.task_id
                    && operation.resource == format!("desktop:{}", followed.task_id)
                {
                    followed.started |= frontier.status != "queued";
                    followed.ended = frontier.ended();
                    followed.frontier = Some(frontier);
                    if followed.ended.is_some() {
                        self.refresh_at = Some(Instant::now());
                    }
                }
            }
            Purpose::Said => {
                if let Some(followed) = &mut self.followed
                    && operation.resource == format!("desktop:{}", followed.task_id)
                {
                    let said = closing_words(value.as_ref(), followed.epoch());
                    followed.ended = Some(match followed.ended.take() {
                        Some(RunPhase::Finished(_)) => RunPhase::Finished(said),
                        Some(RunPhase::NeedsAttention(_)) => RunPhase::NeedsAttention(said),
                        _ => RunPhase::Stopped,
                    });
                    followed.said = true;
                }
            }
            Purpose::Submit(request) => {
                let outcome = mutation_outcome(request, value.as_ref());
                if let ComposerOutcome::DirectionSaved(saved) = &outcome {
                    self.follows.push((
                        request.clone(),
                        saved.clone(),
                        deadline_after(TIMEOUT),
                        Instant::now(),
                    ));
                }
                if matches!(outcome, ComposerOutcome::Stopped(_)) {
                    self.followed = None;
                }
                composer.observe_receipt(ComposerReceipt {
                    owner: request.owner.clone(),
                    submission: request.submission,
                    outcome,
                });
                self.refresh_at = Some(Instant::now());
            }
            Purpose::Applied { request, saved } => {
                let matched = value
                    .and_then(|value| serde_json::from_value::<Frontier>(value).ok())
                    .is_some_and(|row| {
                        row.task_id == saved.task_id
                            && row.run_epoch == Some(saved.run_epoch)
                            && row.active_agent.as_deref() == Some(&request.owner.agent)
                            && row.assigned_agent.as_deref() == Some(&request.owner.agent)
                            && row.route == "model"
                            && row
                                .applied_revision
                                .is_some_and(|revision| revision >= saved.revision)
                    });
                if matched {
                    composer.observe_receipt(ComposerReceipt {
                        owner: request.owner.clone(),
                        submission: request.submission,
                        outcome: ComposerOutcome::DirectionApplied(saved.clone()),
                    });
                    self.follows
                        .retain(|(pending, _, _, _)| pending.submission != request.submission);
                }
            }
        }
    }

    fn refuse(&mut self, operation: &Operation, composer: &mut SelectionComposer, unknown: bool) {
        match &operation.purpose {
            Purpose::Current => self.tasks.clear(),
            // The agent could not be asked: the run decides instead. Asking
            // has no effect, so this never does anything twice.
            Purpose::Respond(request) => {
                tracing::info!(
                    agent = %request.owner.agent,
                    unknown,
                    "the agent could not be asked; its run decides instead"
                );
                if self.session.is_some() {
                    self.begin(request_queue(request, None));
                } else {
                    composer.observe_receipt(ComposerReceipt {
                        owner: request.owner.clone(),
                        submission: request.submission,
                        outcome: ComposerOutcome::Refused,
                    });
                }
            }
            Purpose::Start => {
                if let Some(followed) = &mut self.followed {
                    followed.start_attempts = followed.start_attempts.saturating_add(1);
                    if followed.start_attempts >= START_ATTEMPTS && !followed.started {
                        followed.ended = Some(RunPhase::NeedsAttention(None));
                        followed.said = true;
                    }
                }
            }
            Purpose::Queue {
                request: Some(request),
                ..
            }
            | Purpose::Submit(request) => {
                composer.observe_receipt(ComposerReceipt {
                    owner: request.owner.clone(),
                    submission: request.submission,
                    outcome: if unknown {
                        ComposerOutcome::Unknown
                    } else {
                        ComposerOutcome::Refused
                    },
                });
            }
            Purpose::Queue { request: None, .. }
            | Purpose::Follow
            | Purpose::Said
            | Purpose::Applied { .. } => {}
        }
    }

    fn follow_applied(&mut self) {
        self.follows
            .retain(|(_, _, deadline, _)| Instant::now() < *deadline);
        let mut reads = Vec::new();
        for (request, saved, _, next) in &mut self.follows {
            if Instant::now() >= *next {
                *next = deadline_after(REFRESH);
                reads.push((request.clone(), saved.clone()));
            }
        }
        for (request, saved) in reads {
            self.begin(task_read(
                &format!("desktop:{}", saved.task_id),
                "read-task",
                Map::new(),
                Purpose::Applied { request, saved },
            ));
        }
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.calls
            .iter()
            .map(|call| call.deadline)
            .chain(self.refresh_at)
            .chain(
                self.follows
                    .iter()
                    .flat_map(|(_, _, until, next)| [*until, *next]),
            )
            .min()
    }
}

fn deadline_after(duration: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(duration).unwrap_or(now)
}

fn task_read(
    resource: &str,
    action: &str,
    arguments: Map<String, Value>,
    purpose: Purpose,
) -> Operation {
    Operation {
        resource: resource.into(),
        action: action.into(),
        arguments,
        purpose,
        app: None,
    }
}

fn current_tasks(owner: &ComposerOwner, value: Option<&Value>) -> Vec<ComposerTask> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Some(rows) = value.get("tasks").and_then(Value::as_array) else {
        return Vec::new();
    };
    if value.get("complete").and_then(Value::as_bool) != Some(true)
        || value.get("total").and_then(Value::as_u64) != u64::try_from(rows.len()).ok()
        || rows.len() > 64
    {
        return Vec::new();
    }
    let frontiers: Option<Vec<Frontier>> = rows
        .iter()
        .map(|row| serde_json::from_value(row.clone()).ok())
        .collect();
    frontiers
        .unwrap_or_default()
        .iter()
        .filter_map(|row| row.choice(owner))
        .collect()
}

/// What the agent said last in run `epoch`: its closing words once it ended.
fn closing_words(value: Option<&Value>, epoch: u64) -> Option<String> {
    value?
        .get("turns")?
        .as_array()?
        .iter()
        .filter(|turn| {
            turn.get("kind").and_then(Value::as_str) == Some("agent-output")
                && turn.get("run_epoch").and_then(Value::as_u64) == Some(epoch)
        })
        .filter_map(|turn| turn.get("text").and_then(Value::as_str))
        .next_back()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

enum Responded {
    Answer(String),
    Work(String),
}

/// The agent's decision about a request, held to the shape it promises.
fn respond_outcome(value: Option<&Value>) -> Option<Responded> {
    let object = value?.as_object()?;
    if object.len() != 1 {
        return None;
    }
    let text = |key| {
        object
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    text("answer")
        .map(Responded::Answer)
        .or_else(|| text("work").map(Responded::Work))
}

fn respond_operation(request: ComposerRequest) -> Operation {
    let mut arguments = Map::from_iter([("request".into(), request.text.clone().into())]);
    // The composer shows the start of a selection; the agent answers from
    // all of it.
    let input = request
        .about
        .as_ref()
        .and_then(|about| about.selection.as_ref()?.anchor.excerpt.clone())
        .unwrap_or_else(|| request.context.clone());
    if !input.trim().is_empty() {
        arguments.insert("input".into(), input.into());
    }
    if !request.conversation.is_empty() {
        arguments.insert(
            "conversation".into(),
            Value::Array(
                request
                    .conversation
                    .iter()
                    .map(|turn| {
                        serde_json::json!({
                            "from": if turn.agent { "agent" } else { "person" },
                            "text": turn.text,
                        })
                    })
                    .collect(),
            ),
        );
    }
    Operation {
        resource: tilo_automation::AGENT_TRANSFORM_RESOURCE.into(),
        action: tilo_automation::AGENT_RESPOND_ACTION.into(),
        arguments,
        purpose: Purpose::Respond(request),
        app: Some(AGENT),
    }
}

/// Queue `request` as work, under the title the agent gave it or its own
/// first line.
fn request_queue(request: &ComposerRequest, title: Option<String>) -> Operation {
    let title = title.unwrap_or_else(|| request.text.lines().next().unwrap_or_default().into());
    // A selection travels as the task's own record of it; the composer's
    // context line only shows it, so it is not repeated as text.
    let instruction = match &request.about {
        Some(about) => instruction(&about.instruction(&request.text), "", &request.conversation),
        None => instruction(&request.text, &request.context, &request.conversation),
    };
    queue_operation(
        &request.owner,
        Some(request.clone()),
        &title,
        &instruction,
        request.about.as_ref(),
    )
}

fn queue_operation(
    owner: &ComposerOwner,
    request: Option<ComposerRequest>,
    title: &str,
    instruction: &str,
    about: Option<&SelectionSeed>,
) -> Operation {
    let title: String = title.trim().chars().take(TITLE_CHARS).collect();
    let instruction: String = instruction
        .chars()
        .scan(0_usize, |bytes, c| {
            *bytes = bytes.saturating_add(c.len_utf8());
            (*bytes <= tilo_desktop_model::MAX_WORKSPACE_TASK_INSTRUCTION_BYTES).then_some(c)
        })
        .collect();
    let mut arguments = Map::from_iter([
        ("title".into(), title.clone().into()),
        ("instruction".into(), instruction.into()),
        ("assigned_agent".into(), owner.agent.clone().into()),
        ("planning".into(), "model".into()),
    ]);
    // The selection rides as the task's subject, so the agent is told the
    // exact place to act at; it attaches nothing, so the run's authority
    // stays a room-wide task's.
    if let Some(subject) = about.and_then(SelectionSeed::subject_json) {
        arguments.insert("subject_json".into(), subject.into());
    }
    if let Some(about) = about {
        // Whether the selection travels as a place the agent can act at, or
        // only as text in the instruction.
        tracing::info!(
            app_id = %about.app_id,
            resource = ?about
                .selection
                .as_ref()
                .and_then(|selection| selection.resource.as_ref())
                .map(|resource| resource.id.as_str()),
            place = about
                .selection
                .as_ref()
                .is_some_and(|selection| selection.anchor.place.is_some()),
            subject = arguments.contains_key("subject_json"),
            "queueing a task about a selection"
        );
    }
    Operation {
        resource: TASKS.into(),
        action: "queue".into(),
        arguments,
        purpose: Purpose::Queue { request, title },
        app: None,
    }
}

/// The run's instruction: the request, what it is about, and the
/// conversation it continues.
fn instruction(text: &str, context: &str, conversation: &[ComposerTurn]) -> String {
    let mut instruction = text.to_owned();
    if !context.trim().is_empty() {
        instruction.push_str(runtime_collab::INSTRUCTION_CONTEXT);
        instruction.push_str("\n<<<\n");
        instruction.push_str(context.trim());
        instruction.push_str("\n>>>");
    }
    if !conversation.is_empty() {
        instruction.push_str(runtime_collab::INSTRUCTION_CONVERSATION);
        for turn in conversation {
            instruction.push_str(if turn.agent { "\nYou: " } else { "\nPerson: " });
            instruction.push_str(&turn.text);
        }
    }
    instruction
}

fn submission_operation(request: ComposerRequest) -> Option<Operation> {
    if request.intent == ComposerIntent::Request && !request.stop {
        return Some(respond_operation(request));
    }
    let task = request.task.as_ref()?;
    let mut arguments = task_arguments(task);
    let action = if request.stop {
        "stop"
    } else {
        arguments.insert("text".into(), request.text.clone().into());
        "steer"
    };
    Some(Operation {
        resource: format!("desktop:{}", task.task_id),
        action: action.into(),
        arguments,
        purpose: Purpose::Submit(request),
        app: None,
    })
}

enum Step {
    Waiting,
    Next(Phase),
    Complete(Option<Value>),
    Refused(bool),
}

fn provider_step(
    session: &DesktopActionSession,
    owner: Option<&ComposerOwner>,
    operation: &Operation,
    answer: &mut Pending<Result<CataloguePage, ActionHostError>>,
) -> Step {
    let page = match poll(answer, &waker()) {
        PendingPoll::Waiting => return Step::Waiting,
        PendingPoll::Ready(Ok(page)) => page,
        PendingPoll::Ready(Err(_)) | PendingPoll::Abandoned => return Step::Refused(false),
    };
    let Some(owner) = owner else {
        return Step::Refused(false);
    };
    if !page.exhausted || page.stale_cursor || page.budget_exhausted {
        return Step::Refused(false);
    }
    let Some(app) = operation.app else {
        let Some(provider) = desktop_provider(&page, &owner.room) else {
            return Step::Refused(false);
        };
        let answer = watch(session.query(CatalogueQuery {
            provider: Some(provider.clone()),
            target: Some(ActionAddress::new(&operation.resource, &operation.action)),
            max_results: 1,
            ..CatalogueQuery::default()
        }));
        return Step::Next(Phase::Resolve {
            provider,
            answer,
            pages: 1,
            matched: Vec::new(),
        });
    };
    let providers: Vec<_> = page
        .providers
        .iter()
        .filter(|provider| provider.app_id == app && provider.room == owner.room)
        .collect();
    let [provider] = providers.as_slice() else {
        return Step::Refused(false);
    };
    let answer = watch(session.query(CatalogueQuery {
        provider: Some(provider.provider.clone()),
        target: Some(ActionAddress::new(&operation.resource, &operation.action)),
        max_results: 1,
        ..CatalogueQuery::default()
    }));
    Step::Next(Phase::Resolve {
        provider: provider.provider.clone(),
        answer,
        pages: 1,
        matched: Vec::new(),
    })
}

/// The provider of the desktop's task actions in `room`: the desktop's own
/// application surface there, which is how a native room publishes them,
/// else the workspace provider of a room whose desktop serves them itself.
/// Two desktop surfaces in one room name no single publisher.
fn desktop_provider(page: &CataloguePage, room: &str) -> Option<String> {
    let desktop: Vec<_> = page
        .providers
        .iter()
        .filter(|provider| provider.app_id == DESKTOP && provider.room == room)
        .collect();
    match desktop.as_slice() {
        [] => Some(workspace_provider(room)),
        [provider] => Some(provider.provider.clone()),
        _ => None,
    }
}

fn resolve_step(
    session: &DesktopActionSession,
    operation: &Operation,
    provider: &str,
    answer: &mut Pending<Result<CataloguePage, ActionHostError>>,
    pages: &mut u8,
    matched: &mut Vec<tilo_actions::remote::CatalogueRow>,
) -> Step {
    let page = match poll(answer, &waker()) {
        PendingPoll::Waiting => return Step::Waiting,
        PendingPoll::Ready(Ok(page)) => page,
        PendingPoll::Ready(Err(_)) | PendingPoll::Abandoned => return Step::Refused(false),
    };
    if page.stale_cursor || page.budget_exhausted {
        return Step::Refused(false);
    }
    matched.extend(
        page.rows
            .iter()
            .filter(|row| row.provider == provider && exact_row(row, operation))
            .cloned(),
    );
    if matched.len() > 1 {
        return Step::Refused(false);
    }
    if !page.exhausted && !page.next_cursor.is_empty() && *pages > 1 {
        *pages = pages.saturating_sub(1);
        *answer = watch(session.query(CatalogueQuery {
            provider: Some(provider.into()),
            needle: operation.action.clone(),
            revision: Some(page.revision),
            cursor: page.next_cursor,
            max_results: 128,
            ..CatalogueQuery::default()
        }));
        return Step::Waiting;
    }
    if !page.exhausted {
        return Step::Refused(false);
    }
    let [row] = matched.as_slice() else {
        return Step::Refused(false);
    };
    let Some(target) = row.target() else {
        return Step::Refused(false);
    };
    let invocation = InvocationId::new();
    let argument = if operation.arguments.is_empty() {
        InvokeArgument::None
    } else {
        InvokeArgument::Json(operation.arguments.clone())
    };
    let answer = watch(session.invoke(InvokeRequest {
        invocation,
        target,
        argument,
        context: None,
        confirmation: None,
        revision: Some(page.revision),
        on_behalf_of: None,
        appearance: None,
        wait_ms: Some(1_000),
    }));
    Step::Next(Phase::Invoke { invocation, answer })
}

fn invoke_step(
    session: &DesktopActionSession,
    invocation: InvocationId,
    answer: &mut Pending<Result<InvocationReceipt, ActionHostError>>,
) -> Step {
    let receipt = match poll(answer, &waker()) {
        PendingPoll::Waiting => return Step::Waiting,
        PendingPoll::Ready(Ok(receipt)) if receipt.invocation == invocation => receipt,
        _ => return Step::Refused(true),
    };
    match receipt.status {
        ReceiptStatus::Completed { output } => Step::Complete(output.value),
        ReceiptStatus::Accepted => Step::Next(Phase::Observe {
            invocation,
            changed: session.next_change(&invocation).map(watch),
        }),
        ReceiptStatus::Rejected { .. } => Step::Refused(false),
        ReceiptStatus::Failed { .. } | ReceiptStatus::Cancelled { .. } => Step::Refused(true),
    }
}

fn observe_step(
    session: &DesktopActionSession,
    invocation: InvocationId,
    changed: &mut Option<Pending<()>>,
) -> Step {
    if let Some(change) = changed
        && matches!(poll(change, &waker()), PendingPoll::Waiting)
    {
        return Step::Waiting;
    }
    let Some(observed) = session.observe(&invocation) else {
        return Step::Refused(true);
    };
    match observed.update {
        InvocationUpdate::Succeeded {
            invocation: returned,
            output,
        } if returned == invocation => Step::Complete(output.value),
        InvocationUpdate::Failed { .. } | InvocationUpdate::Cancelled { .. } => Step::Refused(true),
        _ => {
            *changed = session.next_change(&invocation).map(watch);
            Step::Waiting
        }
    }
}

fn same_read(left: &Operation, right: &Operation) -> bool {
    left.resource == right.resource
        && left.action == right.action
        && !left.purpose.submitted()
        && !right.purpose.submitted()
}

fn task_arguments(task: &ComposerTask) -> Map<String, Value> {
    Map::from_iter([
        ("run_epoch".into(), task.run_epoch.into()),
        ("after_revision".into(), task.revision.into()),
    ])
}

fn exact_row(row: &tilo_actions::remote::CatalogueRow, operation: &Operation) -> bool {
    row.action.as_ref().is_some_and(|action| {
        action.address.action == operation.action
            && action.address.resource == operation.resource
            && row.resource.id == operation.resource
            && operation.app.is_none_or(|app| row.app_id == app)
    })
}

/// The task a queue answer names, held to the run it must be: this agent's,
/// planned by a model.
fn queued_task(owner: &ComposerOwner, value: Option<&Value>) -> Option<String> {
    let value = value?;
    let task_id = value.pointer("/task/id").and_then(Value::as_str)?;
    (!task_id.is_empty()
        && value
            .pointer("/result/queued/task_id")
            .and_then(Value::as_str)
            == Some(task_id)
        && value["resource"].as_str() == Some(format!("desktop:{task_id}").as_str())
        && value["managed_route"] == true
        && value
            .pointer("/task/assigned_agent")
            .and_then(Value::as_str)
            == Some(&owner.agent)
        && value.pointer("/task/route").and_then(Value::as_str) == Some("model"))
    .then(|| task_id.to_owned())
}

fn mutation_outcome(request: &ComposerRequest, value: Option<&Value>) -> ComposerOutcome {
    let Some(value) = value else {
        return ComposerOutcome::Unknown;
    };
    let Some(before) = &request.task else {
        return ComposerOutcome::Unknown;
    };
    if value["resource"].as_str() != Some(&format!("desktop:{}", before.task_id)) {
        return ComposerOutcome::Unknown;
    }
    if request.stop && value["cancelled"] == true {
        return ComposerOutcome::Stopped(before.clone());
    }
    let Some(next_revision) = before.revision.checked_add(1) else {
        return ComposerOutcome::Unknown;
    };
    if !request.stop
        && value["saved"] == true
        && value["run_epoch"].as_u64() == Some(before.run_epoch)
        && value["revision"].as_u64() == Some(next_revision)
        && value
            .pointer("/result/direction_added/task_id")
            .and_then(Value::as_str)
            == Some(before.task_id.as_str())
        && value
            .pointer("/result/direction_added/revision")
            .and_then(Value::as_u64)
            == Some(next_revision)
    {
        return ComposerOutcome::DirectionSaved(ComposerTask {
            revision: next_revision,
            ..before.clone()
        });
    }
    ComposerOutcome::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::agent_quick_composer::AgentQuickComposerOrigin;
    use std::sync::{Arc, Mutex};
    use tilo_actions::remote::{
        ActionHost, Caller, CatalogueProvider, CatalogueResource, CatalogueRow, InvocationState,
    };

    fn owner(room: &str) -> ComposerOwner {
        ComposerOwner {
            room: room.into(),
            agent: "tide".into(),
            person: "person".into(),
            identity_revision: 0,
        }
    }

    /// A room whose desktop publishes its task actions as the desktop
    /// application's surface, as a native room does.
    struct NativeRoom {
        room: String,
        queries: std::sync::Mutex<Vec<CatalogueQuery>>,
    }

    impl tilo_actions::remote::ActionHost for NativeRoom {
        fn query(
            &self,
            _: &tilo_actions::remote::Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionHostError>> {
            self.queries.lock().unwrap().push(query.clone());
            let provider = |provider: &str, app_id: &str, room: &str| {
                tilo_actions::remote::CatalogueProvider {
                    provider: provider.into(),
                    app_id: app_id.into(),
                    room: room.into(),
                    record_id: "membership".into(),
                    browsable: false,
                }
            };
            if query.max_results == 0 {
                return Pending::ready(Ok(CataloguePage {
                    providers: vec![
                        provider("7", DESKTOP, &self.room),
                        provider("9", DESKTOP, "another-room"),
                    ],
                    exhausted: true,
                    ..CataloguePage::default()
                }));
            }
            let rows = match (&query.provider, &query.target) {
                (Some(provider), Some(target)) if provider == "7" => {
                    let descriptor: tilo_actions::ActionDescriptor =
                        serde_json::from_value(serde_json::json!({
                            "address": {"resource": target.resource, "action": target.action},
                            "resource_kind": "desktop.tasks", "resource_label": "Tasks",
                            "action_label": target.action,
                            "locus": tilo_actions::ExecutionLocus::default(),
                            "kind": tilo_actions::OperationKind::Effect,
                            "contract": tilo_actions::ActionContract::view(),
                            "input": tilo_actions::InputSpec::None, "params": []
                        }))
                        .unwrap();
                    vec![tilo_actions::remote::CatalogueRow {
                        provider: "7".into(),
                        app_id: DESKTOP.into(),
                        parent_resource: String::new(),
                        resource: tilo_actions::remote::CatalogueResource {
                            id: target.resource.clone(),
                            kind: "desktop.tasks".into(),
                            label: "Tasks".into(),
                            presentation: None,
                        },
                        action: Some(descriptor),
                        has_children: false,
                        visibility: None,
                    }]
                }
                _ => Vec::new(),
            };
            Pending::ready(Ok(CataloguePage {
                rows,
                revision: "1".into(),
                exhausted: true,
                ..CataloguePage::default()
            }))
        }
        fn invoke(
            &self,
            _: &tilo_actions::remote::Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
            Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                },
            }))
        }
        fn observe(
            &self,
            _: &tilo_actions::remote::Caller,
            _: &InvocationId,
        ) -> Option<tilo_actions::remote::InvocationState> {
            None
        }
        fn cancel(
            &self,
            _: &tilo_actions::remote::Caller,
            _: &InvocationId,
        ) -> Result<(), ActionHostError> {
            Err(ActionHostError::UnknownInvocation)
        }
    }

    #[test]
    fn a_native_rooms_task_actions_are_found_under_the_desktop_application() {
        let host = std::sync::Arc::new(NativeRoom {
            room: uuid::Uuid::new_v4().to_string(),
            queries: std::sync::Mutex::default(),
        });
        let owner = ComposerOwner {
            room: host.room.clone(),
            ..owner("room")
        };
        let mut actions = SelectionActions::default();
        assert!(actions.open(
            ActionSessionSource::with_mock(
                crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(),
                host.clone(),
            ),
            owner.clone(),
        ));
        let mut composer = SelectionComposer::default();
        composer.open(
            owner,
            "Tide",
            crate::shell::agent_quick_composer::AgentQuickComposerOrigin {
                marker: vello::kurbo::Rect::new(0.0, 0.0, 1.0, 1.0),
                protected: vello::kurbo::Rect::new(0.0, 0.0, 1.0, 1.0),
                context: String::new(),
            },
            vello::kurbo::Rect::new(0.0, 0.0, 800.0, 600.0),
        );
        for _ in 0..6 {
            actions.pump(&mut composer);
        }
        drop(actions);
        let queries = host.queries.lock().unwrap().clone();
        assert!(
            queries.iter().any(|query| {
                query.provider.as_deref() == Some("7")
                    && query.target == Some(ActionAddress::new(TASKS, "read-current-tasks"))
            }),
            "the current tasks are read from the desktop's own surface"
        );
        assert!(
            queries
                .iter()
                .all(|query| query.provider.as_deref() != Some("9")),
            "another room's desktop is never asked"
        );
    }

    #[test]
    fn a_room_without_a_desktop_surface_asks_its_workspace_provider() {
        let page = |providers| CataloguePage {
            providers,
            ..CataloguePage::default()
        };
        let desktop = |provider: &str| tilo_actions::remote::CatalogueProvider {
            provider: provider.into(),
            app_id: DESKTOP.into(),
            room: "room".into(),
            record_id: String::new(),
            browsable: false,
        };
        assert_eq!(
            desktop_provider(&page(Vec::new()), "room"),
            Some(workspace_provider("room"))
        );
        assert_eq!(
            desktop_provider(&page(vec![desktop("7")]), "room").as_deref(),
            Some("7")
        );
        assert_eq!(
            desktop_provider(&page(vec![desktop("7"), desktop("8")]), "room"),
            None,
            "two desktops in one room name no single publisher"
        );
    }

    fn request(intent: ComposerIntent) -> ComposerRequest {
        ComposerRequest {
            owner: owner("room"),
            intent,
            submission: 4,
            text: "Fix the link\nand tell me where it points".into(),
            stop: false,
            task: Some(ComposerTask {
                task_id: "task:one".into(),
                run_epoch: 7,
                revision: 2,
                label: "Current task".into(),
            }),
            context: "About your selection in Notes".into(),
            conversation: vec![
                ComposerTurn {
                    agent: false,
                    text: "Where does it point?".into(),
                },
                ComposerTurn {
                    agent: true,
                    text: "Nowhere yet.".into(),
                },
            ],
            about: None,
        }
    }

    #[test]
    fn a_request_asks_the_agent_and_a_running_task_is_steered_or_stopped() {
        let mut asked = request(ComposerIntent::Request);
        asked.task = None;
        let operation = submission_operation(asked.clone()).unwrap();
        assert_eq!(operation.resource, "agent.transform:default");
        assert_eq!(operation.action, "respond");
        assert_eq!(operation.app, Some(AGENT));
        assert_eq!(operation.arguments["request"], asked.text);
        assert_eq!(operation.arguments["input"], asked.context);
        let mut long = asked;
        let excerpt = ["word"; 80].join(" ");
        long.about = Some(SelectionSeed::new(
            "so.tilo.notes",
            "Notes",
            Some(ui::shared_selection::SharedSelection::of_text(&excerpt)),
        ));
        assert_eq!(
            submission_operation(long).unwrap().arguments["input"],
            excerpt.as_str(),
            "the agent answers from the whole selection, not the shown start"
        );
        assert_eq!(
            operation.arguments["conversation"],
            serde_json::json!([
                {"from": "person", "text": "Where does it point?"},
                {"from": "agent", "text": "Nowhere yet."}
            ])
        );

        let steer = request(ComposerIntent::Steer);
        let operation = submission_operation(steer.clone()).unwrap();
        assert_eq!(operation.resource, "desktop:task:one");
        assert_eq!(operation.action, "steer");
        assert_eq!(operation.app, None);
        assert_eq!(operation.arguments["run_epoch"], 7);
        assert_eq!(operation.arguments["after_revision"], 2);
        assert_eq!(operation.arguments["text"], steer.text);

        let mut stop = steer;
        stop.stop = true;
        let operation = submission_operation(stop.clone()).unwrap();
        assert_eq!(operation.action, "stop");
        assert!(operation.arguments.get("text").is_none());
        stop.task = None;
        assert!(submission_operation(stop).is_none(), "nothing to stop");
    }

    #[test]
    fn the_agent_answers_or_names_the_work_and_nothing_else_is_believed() {
        let decided = |value: Value| respond_outcome(Some(&value));
        assert!(matches!(
            decided(serde_json::json!({"answer": " It points nowhere. "})),
            Some(Responded::Answer(answer)) if answer == "It points nowhere."
        ));
        assert!(matches!(
            decided(serde_json::json!({"work": "Fix the link"})),
            Some(Responded::Work(title)) if title == "Fix the link"
        ));
        for wrong in [
            serde_json::json!({"answer": "a", "work": "b"}),
            serde_json::json!({"answer": ""}),
            serde_json::json!("plain"),
            serde_json::json!({}),
        ] {
            assert!(decided(wrong).is_none());
        }
        assert!(respond_outcome(None).is_none());
    }

    #[test]
    fn work_is_queued_for_this_agent_with_its_context_and_conversation() {
        let asked = request(ComposerIntent::Request);
        let operation = request_queue(&asked, Some("x".repeat(300)));
        assert_eq!(operation.resource, TASKS);
        assert_eq!(operation.action, "queue");
        assert_eq!(
            operation.arguments["title"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            TITLE_CHARS
        );
        assert_eq!(operation.arguments["assigned_agent"], "tide");
        assert_eq!(operation.arguments["planning"], "model");
        let instruction = operation.arguments["instruction"].as_str().unwrap();
        assert!(instruction.starts_with("Fix the link\nand tell me where it points"));
        assert!(instruction.contains("About your selection in Notes"));
        assert!(instruction.contains("Person: Where does it point?"));
        assert!(instruction.contains("You: Nowhere yet."));
        let untitled = request_queue(&asked, None);
        assert_eq!(untitled.arguments["title"], "Fix the link");
    }

    #[test]
    fn work_about_a_selection_carries_it_as_the_subject_and_attaches_nothing() {
        use ui::shared_selection::SharedSelection;

        let mut request = request(ComposerIntent::Request);
        request.text = "Tighten this".into();
        request.conversation.clear();
        request.about = Some(SelectionSeed::new(
            "so.tilo.word-processor",
            "Writer",
            Some(
                SharedSelection::of_text("which were late")
                    .at(tilo_actions::SelectionPlace::range("block-4", 12, 27))
                    .in_resource(tilo_actions::ActionResource {
                        id: "doc:proposal".into(),
                        kind: "word-processor.document".into(),
                        label: "Proposal".into(),
                    }),
            ),
        ));
        let operation = request_queue(&request, None);
        assert_eq!(operation.action, "queue");
        assert_eq!(
            operation.arguments["instruction"], "Tighten this",
            "the excerpt travels in the anchor, not the instruction"
        );
        assert!(
            operation.arguments.get("resource_json").is_none(),
            "a selection attaches nothing that would narrow the run"
        );
        let subject: Value =
            serde_json::from_str(operation.arguments["subject_json"].as_str().unwrap()).unwrap();
        assert_eq!(subject["app_id"], "so.tilo.word-processor");
        assert_eq!(subject["resource"]["id"], "doc:proposal");
        assert_eq!(subject["resource"]["kind"], "word-processor.document");
        assert_eq!(
            subject["anchor"]["place"],
            serde_json::json!({"item": "block-4", "start": 12, "end": 27})
        );
        assert_eq!(subject["anchor"]["excerpt"], "which were late");

        // Text with no resource behind it (another app's primary selection)
        // is quoted into the instruction instead.
        request.about = Some(SelectionSeed::new(
            "org.example.term",
            "Terminal",
            Some(SharedSelection::of_text("ls -la /srv")),
        ));
        let operation = request_queue(&request, None);
        assert!(operation.arguments.get("subject_json").is_none());
        let instruction = operation.arguments["instruction"].as_str().unwrap();
        assert!(instruction.starts_with("Tighten this\n\n"));
        assert!(instruction.contains("selected in Terminal"));
        assert!(instruction.contains("<<<\nls -la /srv\n>>>"));
    }

    #[test]
    fn queue_receipt_requires_exact_owner_and_canonical_queued_result() {
        let owner = owner("room");
        let value = serde_json::json!({
            "resource":"desktop:task:new", "result":{"queued":{"task_id":"task:new"}},
            "managed_route":true,
            "task":{"id":"task:new", "assigned_agent":"tide", "route":"model"}
        });
        assert_eq!(queued_task(&owner, Some(&value)), Some("task:new".into()));
        for (pointer, replacement) in [
            ("/task/assigned_agent", Value::from("reed")),
            ("/result/queued/task_id", Value::from("task:other")),
            ("/resource", Value::from("desktop:task:other")),
            ("/managed_route", Value::from(false)),
        ] {
            let mut wrong = value.clone();
            *wrong.pointer_mut(pointer).unwrap() = replacement;
            assert_eq!(queued_task(&owner, Some(&wrong)), None);
        }
        assert_eq!(queued_task(&owner, None), None);
    }

    #[test]
    fn direction_ack_is_saved_not_applied_and_rejects_another_run_or_revision() {
        let request = request(ComposerIntent::Steer);
        let value = serde_json::json!({
            "resource":"desktop:task:one", "saved":true, "applied":true,
            "run_epoch":7, "revision":3,
            "result":{"direction_added":{"task_id":"task:one","revision":3}}
        });
        let mut saved = request.task.clone().unwrap();
        saved.revision = 3;
        assert_eq!(
            mutation_outcome(&request, Some(&value)),
            ComposerOutcome::DirectionSaved(saved)
        );
        for (pointer, replacement) in [
            ("/run_epoch", Value::from(8)),
            ("/revision", Value::from(4)),
            ("/resource", Value::from("desktop:task:other")),
        ] {
            let mut wrong = value.clone();
            *wrong.pointer_mut(pointer).unwrap() = replacement;
            assert_eq!(
                mutation_outcome(&request, Some(&wrong)),
                ComposerOutcome::Unknown
            );
        }
    }

    #[test]
    fn owner_frontier_never_retargets_steer_to_another_agent_or_inactive_run() {
        let owner = owner("room");
        let value = serde_json::json!({
            "task_id":"task:one", "title":"Current task", "assigned_agent":"tide",
            "active_agent":"tide", "status":"in_progress", "route":"model",
            "run_epoch":7, "after_revision":2, "applied_revision":1
        });
        let row: Frontier = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(row.choice(&owner).unwrap().revision, 2);
        for (pointer, replacement) in [
            ("/assigned_agent", Value::from("reed")),
            ("/active_agent", Value::Null),
            ("/run_epoch", Value::from(0)),
            ("/status", Value::from("queued")),
        ] {
            let mut wrong = value.clone();
            *wrong.pointer_mut(pointer).unwrap() = replacement;
            let row: Frontier = serde_json::from_value(wrong).unwrap();
            assert!(row.choice(&owner).is_none());
        }
        let malformed = serde_json::json!({"complete":true,"total":1,"tasks":[{"malformed":true}]});
        assert!(current_tasks(&owner, Some(&malformed)).is_empty());
    }

    #[test]
    fn the_closing_words_are_the_agents_last_output_in_that_run() {
        let value = serde_json::json!({"turns": [
            {"text": "Fix it", "run_epoch": 0},
            {"kind": "agent-output", "run_epoch": 2, "text": "Reviewing the catalogue"},
            {"kind": "agent-output", "run_epoch": 2, "text": "Linked it to tilo.so."},
            {"kind": "agent-output", "run_epoch": 1, "text": "An earlier run"},
        ]});
        assert_eq!(
            closing_words(Some(&value), 2).as_deref(),
            Some("Linked it to tilo.so.")
        );
        assert_eq!(closing_words(Some(&value), 3), None);
    }

    /// A room with an agent that answers `respond` as scripted and a Task
    /// Center whose one task moves as the test says.
    type Invoked = Vec<(String, String, Map<String, Value>)>;

    struct Room {
        id: String,
        respond: Mutex<Option<Value>>,
        task: Mutex<Value>,
        said: Mutex<Vec<Value>>,
        invoked: Mutex<Invoked>,
    }

    impl Room {
        fn new(respond: Option<Value>) -> Arc<Self> {
            Arc::new(Self {
                id: uuid::Uuid::new_v4().to_string(),
                respond: Mutex::new(respond),
                task: Mutex::new(serde_json::json!({
                    "task_id": "task:new", "title": "Fix the link", "assigned_agent": "tide",
                    "active_agent": null, "status": "queued", "route": "model",
                    "run_epoch": 0, "after_revision": 0, "applied_revision": null
                })),
                said: Mutex::new(Vec::new()),
                invoked: Mutex::default(),
            })
        }

        fn invoked(&self) -> Vec<String> {
            self.invoked
                .lock()
                .unwrap()
                .iter()
                .map(|(resource, action, _)| format!("{resource}#{action}"))
                .collect()
        }

        fn arguments(&self, action: &str) -> Map<String, Value> {
            self.invoked
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(_, invoked, _)| invoked == action)
                .map(|(_, _, arguments)| arguments.clone())
                .unwrap()
        }

        fn row(provider: &str, app: &str, address: &ActionAddress) -> CatalogueRow {
            let descriptor: tilo_actions::ActionDescriptor =
                serde_json::from_value(serde_json::json!({
                    "address": {"resource": address.resource, "action": address.action},
                    "resource_kind": "fixture", "resource_label": "Fixture",
                    "action_label": address.action,
                    "locus": tilo_actions::ExecutionLocus::default(),
                    "kind": tilo_actions::OperationKind::Effect,
                    "contract": tilo_actions::ActionContract::view(),
                    "input": tilo_actions::InputSpec::None, "params": []
                }))
                .unwrap();
            CatalogueRow {
                provider: provider.into(),
                app_id: app.into(),
                parent_resource: String::new(),
                resource: CatalogueResource {
                    id: address.resource.clone(),
                    kind: "fixture".into(),
                    label: "Fixture".into(),
                    presentation: None,
                },
                action: Some(descriptor),
                has_children: false,
                visibility: None,
            }
        }
    }

    impl ActionHost for Room {
        fn query(
            &self,
            _: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionHostError>> {
            if query.max_results == 0 {
                return Pending::ready(Ok(CataloguePage {
                    providers: vec![CatalogueProvider {
                        provider: "agent".into(),
                        app_id: AGENT.into(),
                        room: self.id.clone(),
                        record_id: String::new(),
                        browsable: false,
                    }],
                    exhausted: true,
                    ..CataloguePage::default()
                }));
            }
            let rows = match (&query.provider, &query.target) {
                (Some(provider), Some(target)) if *provider == workspace_provider(&self.id) => {
                    vec![Self::row(provider, "desktop", target)]
                }
                (Some(provider), Some(_)) if provider == "agent" => vec![Self::row(
                    provider,
                    AGENT,
                    &ActionAddress::new(
                        tilo_automation::AGENT_TRANSFORM_RESOURCE,
                        tilo_automation::AGENT_RESPOND_ACTION,
                    ),
                )],
                _ => Vec::new(),
            };
            Pending::ready(Ok(CataloguePage {
                rows,
                revision: "1".into(),
                exhausted: true,
                ..CataloguePage::default()
            }))
        }

        fn invoke(
            &self,
            _: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
            let address = request.target.address.clone();
            let arguments = match request.argument {
                InvokeArgument::Json(arguments) => arguments,
                _ => Map::new(),
            };
            self.invoked.lock().unwrap().push((
                address.resource.clone(),
                address.action.clone(),
                arguments.clone(),
            ));
            let task = self.task.lock().unwrap().clone();
            let respond = self.respond.lock().unwrap().clone();
            let value = match address.action.as_str() {
                "respond" => match respond {
                    Some(value) => value,
                    None => {
                        return Pending::ready(Ok(InvocationReceipt {
                            invocation: request.invocation,
                            status: ReceiptStatus::Rejected {
                                error: ActionHostError::NotPermitted,
                            },
                        }));
                    }
                },
                "read-current-tasks" => {
                    serde_json::json!({"complete": true, "total": 0, "tasks": []})
                }
                "queue" => serde_json::json!({
                    "resource": "desktop:task:new", "managed_route": true,
                    "result": {"queued": {"task_id": "task:new"}},
                    "task": {"id": "task:new", "assigned_agent": "tide", "route": "model"}
                }),
                "approve-run" => {
                    let mut started = task;
                    started["status"] = "in_progress".into();
                    started["active_agent"] = "tide".into();
                    started["run_epoch"] = 1.into();
                    *self.task.lock().unwrap() = started;
                    serde_json::json!({"resource": "desktop:task:new"})
                }
                "read-task" => task,
                "read-conversation" => {
                    serde_json::json!({"turns": self.said.lock().unwrap().clone()})
                }
                "steer" => {
                    let revision = arguments["after_revision"]
                        .as_u64()
                        .unwrap()
                        .saturating_add(1);
                    let mut directed = task;
                    directed["after_revision"] = revision.into();
                    *self.task.lock().unwrap() = directed;
                    serde_json::json!({
                        "resource": "desktop:task:new", "saved": true,
                        "run_epoch": arguments["run_epoch"], "revision": revision,
                        "result": {"direction_added": {"task_id": "task:new", "revision": revision}}
                    })
                }
                _ => Value::Null,
            };
            Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                },
            }))
        }

        fn observe(&self, _: &Caller, _: &InvocationId) -> Option<InvocationState> {
            None
        }

        fn cancel(&self, _: &Caller, _: &InvocationId) -> Result<(), ActionHostError> {
            Err(ActionHostError::UnknownInvocation)
        }
    }

    fn composer_in(room: &Arc<Room>) -> (SelectionActions, SelectionComposer, ComposerOwner) {
        let owner = owner(&room.id);
        let mut actions = SelectionActions::default();
        assert!(actions.open(
            ActionSessionSource::with_mock(
                crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(),
                room.clone(),
            ),
            owner.clone(),
        ));
        let mut composer = SelectionComposer::default();
        composer.open(
            owner.clone(),
            "Tide",
            AgentQuickComposerOrigin {
                marker: vello::kurbo::Rect::new(0.0, 0.0, 1.0, 1.0),
                protected: vello::kurbo::Rect::new(0.0, 0.0, 1.0, 1.0),
                context: "About your selection in Notes".into(),
            },
            vello::kurbo::Rect::new(0.0, 0.0, 800.0, 600.0),
        );
        (actions, composer, owner)
    }

    fn perform(composer: &mut SelectionComposer, id: &str, action: &ui::a11y::Action) {
        fn find(
            widget: &mut dyn ui::widget::Widget,
            id: &str,
            action: &ui::a11y::Action,
            doc: &mut automerge::AutoCommit,
        ) -> bool {
            if widget.test_id() == Some(id) {
                return widget.perform_action(
                    action,
                    &[],
                    &mut ui::widget::Edits::lend(doc),
                    &automerge::ROOT,
                );
            }
            widget
                .children_mut()
                .into_iter()
                .any(|child| find(child, id, action, doc))
        }
        let mut doc = automerge::AutoCommit::new();
        ui::widget::Widget::sync(composer, &doc, &automerge::ROOT);
        assert!(find(composer, id, action, &mut doc), "{id} is available");
        ui::widget::Widget::sync(composer, &doc, &automerge::ROOT);
    }

    fn send(composer: &mut SelectionComposer, text: &str) {
        perform(
            composer,
            "agent-quick-editor",
            &ui::a11y::Action::SetValue(text.into()),
        );
        perform(composer, "agent-quick-send", &ui::a11y::Action::Activate);
    }

    fn settle(actions: &mut SelectionActions, composer: &mut SelectionComposer) {
        for _ in 0..32 {
            actions.refresh_at = Some(Instant::now());
            actions.pump(composer);
        }
    }

    #[test]
    fn a_question_is_answered_in_place_and_starts_no_task() {
        let room = Room::new(Some(
            serde_json::json!({"answer": "It points nowhere yet."}),
        ));
        let (mut actions, mut composer, _) = composer_in(&room);
        send(&mut composer, "Where does this link point?");
        settle(&mut actions, &mut composer);
        let invoked = room.invoked();
        assert!(invoked.contains(&"agent.transform:default#respond".to_owned()));
        assert!(
            !invoked.iter().any(|call| call.ends_with("#queue")),
            "{invoked:?}"
        );
        assert_eq!(
            room.arguments("respond")["input"],
            "About your selection in Notes"
        );
        assert!(composer.followed().is_none());
        assert_eq!(
            composer.take_request(),
            None,
            "the answer settled the request"
        );
    }

    #[test]
    fn work_becomes_a_started_task_the_field_steers_until_it_ends_with_its_words() {
        let room = Room::new(Some(serde_json::json!({"work": "Fix the link"})));
        let (mut actions, mut composer, owner) = composer_in(&room);
        send(&mut composer, "Replace the parenthesis with the real link");
        settle(&mut actions, &mut composer);
        assert_eq!(room.arguments("queue")["title"], "Fix the link");
        assert_eq!(room.arguments("approve-run")["agent_id"], "tide");
        let run = composer.followed().cloned().expect("the run is followed");
        assert_eq!(run.phase, RunPhase::Working);
        assert_eq!(run.task.task_id, "task:new");
        assert_eq!(run.task.run_epoch, 1);

        send(&mut composer, "Use the https address");
        settle(&mut actions, &mut composer);
        let direction = room.arguments("steer");
        assert!(room.invoked().iter().all(|call| !call.ends_with("#direct")));
        assert_eq!(direction["text"], "Use the https address");
        assert_eq!(direction["run_epoch"], 1);
        assert!(
            room.invoked()
                .iter()
                .filter(|call| call.ends_with("#respond"))
                .count()
                == 1,
            "a direction is not a new request"
        );

        room.said.lock().unwrap().push(serde_json::json!({
            "kind": "agent-output", "run_epoch": 1, "text": "Linked it to tilo.so."
        }));
        room.task.lock().unwrap()["status"] = "done".into();
        room.task.lock().unwrap()["active_agent"] = Value::Null;
        settle(&mut actions, &mut composer);
        assert!(composer.followed().is_none(), "the run ended");
        assert!(actions.followed.is_none());
        let _ = owner;
        // Asking again is a new request again.
        *room.respond.lock().unwrap() = Some(serde_json::json!({"answer": "You're welcome."}));
        send(&mut composer, "Thanks");
        settle(&mut actions, &mut composer);
        let conversation = room.arguments("respond")["conversation"].clone();
        assert_eq!(
            conversation.as_array().unwrap().last().unwrap()["text"],
            "Linked it to tilo.so.",
            "the run's closing words are part of the conversation: {conversation}"
        );
    }

    #[test]
    fn a_request_the_agent_cannot_take_is_queued_as_it_stands() {
        let room = Room::new(None);
        let (mut actions, mut composer, _) = composer_in(&room);
        send(&mut composer, "Tidy the notes\nall of them");
        settle(&mut actions, &mut composer);
        let queued = room.arguments("queue");
        assert_eq!(queued["title"], "Tidy the notes");
        assert!(
            queued["instruction"]
                .as_str()
                .unwrap()
                .starts_with("Tidy the notes\nall of them")
        );
        assert!(composer.followed().is_some());
    }
}

/// Both entry points retain their outstanding calls when chrome switches.
/// Pumping the hidden composer still observes receipts, never resubmits work.
#[derive(Default)]
pub(super) struct AgentActions {
    marker: marker_actions::AgentActions,
    selection: SelectionActions,
}

impl AgentActions {
    pub(super) fn open(&mut self, source: ActionSessionSource, owner: ComposerOwner) -> bool {
        // Neither open dispatches an action. Refuse a switch while either
        // adapter still owns a mutation whose outcome must be observed.
        self.marker.open(source.clone(), owner.clone()) && self.selection.open(source, owner)
    }

    pub(super) fn pump(
        &mut self,
        composer: &mut crate::shell::agent_quick_composer::AgentQuickComposer,
    ) -> bool {
        self.marker.pump(&mut composer.marker)
            | self.selection.pump(&mut composer.selection_composer)
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.marker
            .deadline()
            .into_iter()
            .chain(self.selection.deadline())
            .min()
    }
}

mod marker_actions {
    //! Nonblocking adapter from private composer intentions to the existing
    //! certified semantic action door. It owns no task or Chat document state.

    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use serde::Deserialize;
    use serde_json::{Map, Value};
    use tilo_actions::remote::{
        ActionHostError, CanonicalActionRoute, CataloguePage, CatalogueQuery, InvocationReceipt,
        InvokeArgument, InvokeRequest, Pending, PendingPoll, ReceiptStatus, RemoteActionTarget,
        workspace_provider,
    };
    use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};

    use crate::shell::agent_quick_composer::marker::{
        AgentQuickComposer, ComposerExpansion, ComposerIntent, ComposerOutcome, ComposerOwner,
        ComposerReceipt, ComposerRequest, ComposerTask,
    };
    use crate::shell::certified_action_session::{ActionSessionSource, DesktopActionSession};
    use crate::shell::search::invocation_wake::{poll, waker, watch};

    const REFRESH: Duration = Duration::from_secs(1);
    const TIMEOUT: Duration = Duration::from_secs(30);
    const TASKS: &str = "desktop:tasks";
    const CHAT: &str = "so.tilo.chat";

    #[derive(Clone)]
    enum Purpose {
        Current,
        Offer,
        Submit(ComposerRequest),
        Applied {
            request: ComposerRequest,
            saved: ComposerTask,
        },
        Expand {
            request: ComposerExpansion,
            token: uuid::Uuid,
        },
    }

    #[derive(Clone)]
    struct Operation {
        resource: String,
        action: String,
        arguments: Map<String, Value>,
        purpose: Purpose,
        chat: bool,
    }

    enum Phase {
        Providers(Pending<Result<CataloguePage, ActionHostError>>),
        Resolve {
            provider: String,
            answer: Pending<Result<CataloguePage, ActionHostError>>,
            pages: u8,
            matched: Vec<tilo_actions::remote::CatalogueRow>,
        },
        Invoke {
            invocation: InvocationId,
            answer: Pending<Result<InvocationReceipt, ActionHostError>>,
        },
        Observe {
            invocation: InvocationId,
            changed: Option<Pending<()>>,
        },
    }

    struct Call {
        operation: Operation,
        phase: Phase,
        deadline: Instant,
    }

    #[derive(Clone)]
    struct Offer {
        target: RemoteActionTarget,
        revision: String,
    }

    #[derive(Deserialize)]
    struct Frontier {
        task_id: String,
        title: String,
        assigned_agent: Option<String>,
        active_agent: Option<String>,
        status: String,
        route: String,
        run_epoch: Option<u64>,
        after_revision: Option<u64>,
        applied_revision: Option<u64>,
    }

    impl Frontier {
        fn choice(&self, owner: &ComposerOwner) -> Option<ComposerTask> {
            let epoch = self.run_epoch?;
            let revision = self.after_revision?;
            (epoch > 0
                && self.assigned_agent.as_deref() == Some(&owner.agent)
                && self.active_agent.as_deref() == Some(&owner.agent)
                && self.status == "in_progress"
                && self.route == "model")
                .then(|| ComposerTask {
                    task_id: self.task_id.clone(),
                    run_epoch: epoch,
                    revision,
                    label: self.title.clone(),
                })
        }
    }

    #[derive(Default)]
    pub(crate) struct AgentActions {
        owner: Option<ComposerOwner>,
        source: Option<ActionSessionSource>,
        session: Option<DesktopActionSession>,
        calls: Vec<Call>,
        offers: HashMap<ActionAddress, Offer>,
        tasks: Vec<ComposerTask>,
        selected: Option<ComposerTask>,
        refresh_at: Option<Instant>,
        follows: Vec<(ComposerRequest, ComposerTask, Instant, Instant)>,
    }

    impl AgentActions {
        pub(crate) fn open(&mut self, source: ActionSessionSource, owner: ComposerOwner) -> bool {
            // Opening chrome must not retire a still-observable mutation or an
            // exact-run Saved receipt. It is never a cancellation or a retry.
            if self.owner.as_ref() == Some(&owner) && self.session.is_some() {
                return true;
            }
            if self
                .calls
                .iter()
                .any(|call| matches!(call.operation.purpose, Purpose::Submit(_)))
                || !self.follows.is_empty()
            {
                return false;
            }
            let Some(revision) = source.identity_revision(&owner.room) else {
                return false;
            };
            if revision != owner.identity_revision {
                return false;
            }
            let Ok(session) = source.open(&owner.room, Some(&owner.person)) else {
                return false;
            };
            self.owner = Some(owner);
            self.source = Some(source);
            self.session = Some(session);
            self.calls.clear();
            self.offers.clear();
            self.tasks.clear();
            self.follows.clear();
            self.selected = None;
            self.refresh_at = Some(Instant::now());
            true
        }

        fn begin(&mut self, operation: Operation) {
            if self
                .calls
                .iter()
                .any(|call| same_read(&call.operation, &operation))
            {
                return;
            }
            let (Some(session), Some(owner)) = (&self.session, &self.owner) else {
                return;
            };
            let phase = if operation.chat {
                Phase::Providers(watch(session.query(CatalogueQuery {
                    canonical_route: Some(CanonicalActionRoute {
                        room: owner.room.clone(),
                        app_id: CHAT.into(),
                        record_id: String::new(),
                    }),
                    max_results: 0,
                    ..CatalogueQuery::default()
                })))
            } else {
                let provider = workspace_provider(&owner.room);
                let answer = watch(session.query(CatalogueQuery {
                    provider: Some(provider.clone()),
                    target: Some(ActionAddress::new(&operation.resource, &operation.action)),
                    max_results: 1,
                    ..CatalogueQuery::default()
                }));
                Phase::Resolve {
                    provider,
                    answer,
                    pages: 1,
                    matched: Vec::new(),
                }
            };
            self.calls.push(Call {
                operation,
                phase,
                deadline: deadline_after(TIMEOUT),
            });
        }

        fn refresh(&mut self) {
            let Some(owner) = &self.owner else {
                return;
            };
            let agent = owner.agent.clone();
            self.offers.clear();
            self.begin(Operation {
                resource: TASKS.into(),
                action: "read-current-tasks".into(),
                arguments: Map::from_iter([("agent".into(), agent.into())]),
                purpose: Purpose::Current,
                chat: false,
            });
            self.begin(Operation {
                resource: TASKS.into(),
                action: "queue".into(),
                arguments: Map::new(),
                purpose: Purpose::Offer,
                chat: false,
            });
            if let Some(task) = self.selected.clone() {
                self.task_offers(&task);
            }
            self.refresh_at = Some(deadline_after(REFRESH));
        }

        pub(crate) fn pump(&mut self, composer: &mut AgentQuickComposer) -> bool {
            let Some(owner) = self.owner.clone() else {
                return false;
            };
            let active = composer.owner() == Some(&owner) && composer.is_open();
            if !active {
                self.refresh_at = None;
            }
            if self
                .source
                .as_ref()
                .and_then(|source| source.identity_revision(&owner.room))
                != Some(owner.identity_revision)
            {
                let changed = self.session.is_some();
                for call in std::mem::take(&mut self.calls) {
                    self.refuse(&call.operation, composer, true);
                }
                self.offers.clear();
                self.tasks.clear();
                self.session = None;
                self.refresh_at = None;
                self.follows.clear();
                composer.project(&owner, [false; 3], Vec::new(), false);
                composer.revoke_conversation();
                return changed && active;
            }
            if active && self.refresh_at.is_none_or(|due| Instant::now() >= due) {
                self.refresh();
            }
            let pending = std::mem::take(&mut self.calls);
            let mut changed = false;
            for mut call in pending {
                if Instant::now() >= call.deadline {
                    self.refuse(&call.operation, composer, true);
                    changed = true;
                    continue;
                }
                if self.advance(&mut call, composer) {
                    self.calls.push(call);
                } else {
                    changed = true;
                }
            }
            self.follow_applied();
            if !active {
                return changed;
            }
            let selected = composer.selected_task();
            if selected != self.selected {
                self.selected.clone_from(&selected);
                if let Some(task) = &selected {
                    self.task_offers(task);
                }
            }
            let queue = self
                .offers
                .contains_key(&ActionAddress::new(TASKS, "queue"));
            let steer = selected.as_ref().is_some_and(|task| {
                self.offers.contains_key(&ActionAddress::new(
                    format!("desktop:{}", task.task_id),
                    "steer",
                ))
            });
            let stop = selected.as_ref().is_some_and(|task| {
                self.offers.contains_key(&ActionAddress::new(
                    format!("desktop:{}", task.task_id),
                    "stop",
                ))
            });
            composer.project(&owner, [steer, queue, steer], self.tasks.clone(), stop);
            if let Some(request) = composer.take_request() {
                self.submit(request);
                changed = true;
            }
            if composer.take_conversation_request() {
                if let (Some(request), Some(session)) =
                    (composer.expansion_request(), self.session.as_ref())
                {
                    let draft = ui::private_draft::Draft {
                        workspace: owner.room.clone(),
                        target: ui::private_draft::Target::Task {
                            agent: owner.agent.clone(),
                            task: request.task.0.clone(),
                            run_epoch: request.task.1,
                        },
                        editor: request.editor.clone(),
                    };
                    let staged = composer
                        .marker_origin()
                        .cloned()
                        .ok_or_else(|| "original task paint source unavailable".to_owned())
                        .and_then(|(app, interaction)| {
                            session.stage_private_task_draft(draft, app, interaction)
                        });
                    match staged {
                        Ok(token) => self.begin(expansion_operation(request, token)),
                        Err(_) => composer.observe_expansion(&request, false),
                    }
                }
                changed = true;
            }
            changed
        }

        fn task_offers(&mut self, task: &ComposerTask) {
            for action in ["steer", "stop"] {
                self.begin(Operation {
                    resource: format!("desktop:{}", task.task_id),
                    action: action.into(),
                    arguments: Map::new(),
                    purpose: Purpose::Offer,
                    chat: false,
                });
            }
        }

        fn submit(&mut self, request: ComposerRequest) {
            if let Some(operation) = submission_operation(request) {
                self.begin(operation);
            }
        }

        fn advance(&mut self, call: &mut Call, composer: &mut AgentQuickComposer) -> bool {
            let Some(session) = self.session.as_ref() else {
                self.refuse(&call.operation, composer, true);
                return false;
            };
            let step = match &mut call.phase {
                Phase::Providers(answer) => {
                    provider_step(session, self.owner.as_ref(), &call.operation, answer)
                }
                Phase::Resolve {
                    provider,
                    answer,
                    pages,
                    matched,
                } => resolve_step(session, &call.operation, provider, answer, pages, matched),
                Phase::Invoke { invocation, answer } => invoke_step(session, *invocation, answer),
                Phase::Observe {
                    invocation,
                    changed,
                } => observe_step(session, *invocation, changed),
            };
            match step {
                Step::Waiting => true,
                Step::Next(phase) => {
                    call.phase = phase;
                    true
                }
                Step::Offer(offer) => {
                    self.offers.insert(
                        ActionAddress::new(&call.operation.resource, &call.operation.action),
                        offer,
                    );
                    false
                }
                Step::Complete(value) => {
                    self.complete(&call.operation, value, composer);
                    false
                }
                Step::Refused(unknown) => {
                    self.refuse(&call.operation, composer, unknown);
                    false
                }
            }
        }

        fn complete(
            &mut self,
            operation: &Operation,
            value: Option<Value>,
            composer: &mut AgentQuickComposer,
        ) {
            match &operation.purpose {
                Purpose::Current => {
                    let (Some(value), Some(owner)) = (value, &self.owner) else {
                        self.tasks.clear();
                        return;
                    };
                    let Some(rows) = value.get("tasks").and_then(Value::as_array) else {
                        self.tasks.clear();
                        return;
                    };
                    if value.get("complete").and_then(Value::as_bool) != Some(true)
                        || value.get("total").and_then(Value::as_u64)
                            != u64::try_from(rows.len()).ok()
                        || rows.len() > 64
                    {
                        self.tasks.clear();
                        return;
                    }
                    let frontiers: Option<Vec<Frontier>> = rows
                        .iter()
                        .map(|row| serde_json::from_value(row.clone()).ok())
                        .collect();
                    let Some(frontiers) = frontiers else {
                        self.tasks.clear();
                        return;
                    };
                    self.tasks = frontiers
                        .iter()
                        .filter_map(|row| row.choice(owner))
                        .collect();
                    if composer.marker_task().is_some_and(|bound| {
                        !self.tasks.iter().any(|task| {
                            task.task_id == bound.task_id && task.run_epoch == bound.run_epoch
                        })
                    }) {
                        composer.observe_ended();
                    }
                }
                Purpose::Submit(request) => {
                    let outcome = mutation_outcome(request, value.as_ref());
                    if let ComposerOutcome::DirectionSaved(saved) = &outcome {
                        self.follows.push((
                            request.clone(),
                            saved.clone(),
                            deadline_after(TIMEOUT),
                            Instant::now(),
                        ));
                    }
                    composer.observe_receipt(ComposerReceipt {
                        owner: request.owner.clone(),
                        submission: request.submission,
                        outcome,
                    });
                    self.refresh_at = Some(Instant::now());
                }
                Purpose::Applied { request, saved } => {
                    let matched = value
                        .and_then(|value| serde_json::from_value::<Frontier>(value).ok())
                        .is_some_and(|row| {
                            row.task_id == saved.task_id
                                && row.run_epoch == Some(saved.run_epoch)
                                && row.active_agent.as_deref() == Some(&request.owner.agent)
                                && row.assigned_agent.as_deref() == Some(&request.owner.agent)
                                && row.route == "model"
                                && row
                                    .applied_revision
                                    .is_some_and(|revision| revision >= saved.revision)
                        });
                    if matched {
                        composer.observe_receipt(ComposerReceipt {
                            owner: request.owner.clone(),
                            submission: request.submission,
                            outcome: ComposerOutcome::DirectionApplied(saved.clone()),
                        });
                        self.follows
                            .retain(|(pending, _, _, _)| pending.submission != request.submission);
                    }
                }
                Purpose::Expand { request, token } => {
                    let accepted = value.as_ref().and_then(|value| value["adopted"].as_str())
                        == Some(token.to_string().as_str());
                    composer.observe_expansion(request, accepted);
                }
                Purpose::Offer => {}
            }
        }

        fn refuse(
            &mut self,
            operation: &Operation,
            composer: &mut AgentQuickComposer,
            unknown: bool,
        ) {
            if matches!(operation.purpose, Purpose::Current) {
                self.tasks.clear();
            }
            if let Purpose::Expand { request, .. } = &operation.purpose {
                composer.observe_expansion(request, false);
            }
            if let Purpose::Submit(request) = &operation.purpose {
                composer.observe_receipt(ComposerReceipt {
                    owner: request.owner.clone(),
                    submission: request.submission,
                    outcome: if unknown {
                        ComposerOutcome::Unknown
                    } else {
                        ComposerOutcome::Refused
                    },
                });
            }
        }

        fn follow_applied(&mut self) {
            self.follows
                .retain(|(_, _, deadline, _)| Instant::now() < *deadline);
            let mut reads = Vec::new();
            for (request, saved, _, next) in &mut self.follows {
                if Instant::now() >= *next {
                    *next = deadline_after(REFRESH);
                    reads.push((request.clone(), saved.clone()));
                }
            }
            for (request, saved) in reads {
                self.begin(Operation {
                    resource: format!("desktop:{}", saved.task_id),
                    action: "read-task".into(),
                    arguments: Map::new(),
                    purpose: Purpose::Applied { request, saved },
                    chat: false,
                });
            }
        }

        pub(crate) fn deadline(&self) -> Option<Instant> {
            self.calls
                .iter()
                .map(|call| call.deadline)
                .chain(self.refresh_at)
                .chain(
                    self.follows
                        .iter()
                        .flat_map(|(_, _, until, next)| [*until, *next]),
                )
                .min()
        }
    }

    fn deadline_after(duration: Duration) -> Instant {
        let now = Instant::now();
        now.checked_add(duration).unwrap_or(now)
    }

    fn submission_operation(request: ComposerRequest) -> Option<Operation> {
        let (resource, action, arguments, chat) = if request.stop {
            let Some(task) = &request.task else {
                return None;
            };
            (
                format!("desktop:{}", task.task_id),
                "stop",
                task_arguments(task),
                false,
            )
        } else {
            match request.intent {
                ComposerIntent::Queue => {
                    let title: String = request
                        .text
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(256)
                        .collect();
                    (
                        TASKS.into(),
                        "queue",
                        Map::from_iter([
                            ("title".into(), title.into()),
                            ("instruction".into(), request.text.clone().into()),
                            ("assigned_agent".into(), request.owner.agent.clone().into()),
                            ("planning".into(), "model".into()),
                        ]),
                        false,
                    )
                }
                ComposerIntent::Message | ComposerIntent::Steer => {
                    let Some(task) = &request.task else {
                        return None;
                    };
                    let mut arguments = task_arguments(task);
                    arguments.insert("text".into(), request.text.clone().into());
                    (
                        format!("desktop:{}", task.task_id),
                        "steer",
                        arguments,
                        false,
                    )
                }
            }
        };
        Some(Operation {
            resource,
            action: action.into(),
            arguments,
            purpose: Purpose::Submit(request),
            chat,
        })
    }

    fn expansion_operation(request: ComposerExpansion, token: uuid::Uuid) -> Operation {
        Operation {
            resource: String::new(),
            action: "adopt-private-draft".into(),
            arguments: Map::from_iter([("token".into(), token.to_string().into())]),
            purpose: Purpose::Expand { request, token },
            chat: true,
        }
    }

    enum Step {
        Waiting,
        Next(Phase),
        Offer(Offer),
        Complete(Option<Value>),
        Refused(bool),
    }

    fn provider_step(
        session: &DesktopActionSession,
        owner: Option<&ComposerOwner>,
        operation: &Operation,
        answer: &mut Pending<Result<CataloguePage, ActionHostError>>,
    ) -> Step {
        let page = match poll(answer, &waker()) {
            PendingPoll::Waiting => return Step::Waiting,
            PendingPoll::Ready(Ok(page)) => page,
            PendingPoll::Ready(Err(_)) | PendingPoll::Abandoned => return Step::Refused(false),
        };
        let Some(owner) = owner else {
            return Step::Refused(false);
        };
        if !page.exhausted || page.stale_cursor || page.budget_exhausted {
            return Step::Refused(false);
        }
        let providers: Vec<_> = page
            .providers
            .iter()
            .filter(|provider| provider.app_id == CHAT && provider.room == owner.room)
            .collect();
        let [provider] = providers.as_slice() else {
            return Step::Refused(false);
        };
        let answer = watch(session.query(CatalogueQuery {
            provider: Some(provider.provider.clone()),
            needle: operation.action.clone(),
            max_results: 128,
            ..CatalogueQuery::default()
        }));
        Step::Next(Phase::Resolve {
            provider: provider.provider.clone(),
            answer,
            pages: 16,
            matched: Vec::new(),
        })
    }

    fn resolve_step(
        session: &DesktopActionSession,
        operation: &Operation,
        provider: &str,
        answer: &mut Pending<Result<CataloguePage, ActionHostError>>,
        pages: &mut u8,
        matched: &mut Vec<tilo_actions::remote::CatalogueRow>,
    ) -> Step {
        let page = match poll(answer, &waker()) {
            PendingPoll::Waiting => return Step::Waiting,
            PendingPoll::Ready(Ok(page)) => page,
            PendingPoll::Ready(Err(_)) | PendingPoll::Abandoned => return Step::Refused(false),
        };
        if page.stale_cursor || page.budget_exhausted {
            return Step::Refused(false);
        }
        matched.extend(
            page.rows
                .iter()
                .filter(|row| row.provider == provider && exact_row(row, operation))
                .cloned(),
        );
        if matched.len() > 1 {
            return Step::Refused(false);
        }
        if !page.exhausted && !page.next_cursor.is_empty() && *pages > 1 {
            *pages = pages.saturating_sub(1);
            *answer = watch(session.query(CatalogueQuery {
                provider: Some(provider.into()),
                needle: operation.action.clone(),
                revision: Some(page.revision),
                cursor: page.next_cursor,
                max_results: 128,
                ..CatalogueQuery::default()
            }));
            return Step::Waiting;
        }
        if !page.exhausted {
            return Step::Refused(false);
        }
        let [row] = matched.as_slice() else {
            return Step::Refused(false);
        };
        let Some(target) = row.target() else {
            return Step::Refused(false);
        };
        let offer = Offer {
            target,
            revision: page.revision,
        };
        if matches!(operation.purpose, Purpose::Offer) {
            return Step::Offer(offer);
        }
        let invocation = InvocationId::new();
        let argument = if operation.arguments.is_empty() {
            InvokeArgument::None
        } else {
            InvokeArgument::Json(operation.arguments.clone())
        };
        let answer = watch(session.invoke(InvokeRequest {
            invocation,
            target: offer.target,
            argument,
            context: None,
            confirmation: None,
            revision: Some(offer.revision),
            on_behalf_of: None,
            appearance: None,
            wait_ms: Some(1_000),
        }));
        Step::Next(Phase::Invoke { invocation, answer })
    }

    fn invoke_step(
        session: &DesktopActionSession,
        invocation: InvocationId,
        answer: &mut Pending<Result<InvocationReceipt, ActionHostError>>,
    ) -> Step {
        let receipt = match poll(answer, &waker()) {
            PendingPoll::Waiting => return Step::Waiting,
            PendingPoll::Ready(Ok(receipt)) if receipt.invocation == invocation => receipt,
            _ => return Step::Refused(true),
        };
        match receipt.status {
            ReceiptStatus::Completed { output } => Step::Complete(output.value),
            ReceiptStatus::Accepted => Step::Next(Phase::Observe {
                invocation,
                changed: session.next_change(&invocation).map(watch),
            }),
            ReceiptStatus::Rejected { .. } => Step::Refused(false),
            ReceiptStatus::Failed { .. } | ReceiptStatus::Cancelled { .. } => Step::Refused(true),
        }
    }

    fn observe_step(
        session: &DesktopActionSession,
        invocation: InvocationId,
        changed: &mut Option<Pending<()>>,
    ) -> Step {
        if let Some(change) = changed
            && matches!(poll(change, &waker()), PendingPoll::Waiting)
        {
            return Step::Waiting;
        }
        let Some(observed) = session.observe(&invocation) else {
            return Step::Refused(true);
        };
        match observed.update {
            InvocationUpdate::Succeeded {
                invocation: returned,
                output,
            } if returned == invocation => Step::Complete(output.value),
            InvocationUpdate::Failed { .. } | InvocationUpdate::Cancelled { .. } => {
                Step::Refused(true)
            }
            _ => {
                *changed = session.next_change(&invocation).map(watch);
                Step::Waiting
            }
        }
    }

    fn same_read(left: &Operation, right: &Operation) -> bool {
        left.resource == right.resource
            && left.action == right.action
            && !matches!(left.purpose, Purpose::Submit(_))
            && !matches!(right.purpose, Purpose::Submit(_))
    }

    fn task_arguments(task: &ComposerTask) -> Map<String, Value> {
        Map::from_iter([
            ("run_epoch".into(), task.run_epoch.into()),
            ("after_revision".into(), task.revision.into()),
        ])
    }

    fn exact_row(row: &tilo_actions::remote::CatalogueRow, operation: &Operation) -> bool {
        row.action.as_ref().is_some_and(|action| {
            action.address.action == operation.action
                && if operation.chat {
                    row.app_id == CHAT
                        && if matches!(operation.purpose, Purpose::Expand { .. }) {
                            action.params.len() == 1
                                && action
                                    .params
                                    .first()
                                    .is_some_and(|param| param.name == "token")
                        } else {
                            action.semantic_identity.as_ref().is_some_and(|identity| {
                                identity.kind == "chat.agent" && identity.key == operation.resource
                            })
                        }
                } else {
                    row.resource.id == operation.resource
                        && action.address.resource == operation.resource
                }
        })
    }

    fn mutation_outcome(request: &ComposerRequest, value: Option<&Value>) -> ComposerOutcome {
        let Some(value) = value else {
            return ComposerOutcome::Unknown;
        };
        if request.intent == ComposerIntent::Queue && !request.stop {
            let Some(task_id) = value.pointer("/task/id").and_then(Value::as_str) else {
                return ComposerOutcome::Unknown;
            };
            if !task_id.is_empty()
                && value
                    .pointer("/result/queued/task_id")
                    .and_then(Value::as_str)
                    == Some(task_id)
                && value["resource"].as_str() == Some(format!("desktop:{task_id}").as_str())
                && value["managed_route"] == true
                && value
                    .pointer("/task/assigned_agent")
                    .and_then(Value::as_str)
                    == Some(&request.owner.agent)
                && value.pointer("/task/route").and_then(Value::as_str) == Some("model")
            {
                return ComposerOutcome::Queued {
                    task_id: task_id.to_owned(),
                };
            }
            return ComposerOutcome::Unknown;
        }
        let Some(before) = &request.task else {
            return ComposerOutcome::Unknown;
        };
        if value["resource"].as_str() != Some(&format!("desktop:{}", before.task_id)) {
            return ComposerOutcome::Unknown;
        }
        if request.stop && value["cancelled"] == true {
            return ComposerOutcome::StopRequested(before.clone());
        }
        let Some(next_revision) = before.revision.checked_add(1) else {
            return ComposerOutcome::Unknown;
        };
        if !request.stop
            && value["saved"] == true
            && value["run_epoch"].as_u64() == Some(before.run_epoch)
            && value["revision"].as_u64() == Some(next_revision)
            && value
                .pointer("/result/direction_added/task_id")
                .and_then(Value::as_str)
                == Some(before.task_id.as_str())
            && value
                .pointer("/result/direction_added/revision")
                .and_then(Value::as_u64)
                == Some(next_revision)
        {
            return ComposerOutcome::DirectionSaved(ComposerTask {
                revision: next_revision,
                ..before.clone()
            });
        }
        ComposerOutcome::Unknown
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn completed_and_queued_tasks_do_not_hide_an_active_composer_target() {
            let owner = request(ComposerIntent::Message).owner;
            let mut actions = AgentActions {
                owner: Some(owner.clone()),
                ..AgentActions::default()
            };
            let operation = Operation {
                resource: TASKS.into(),
                action: "read-current-tasks".into(),
                arguments: Map::new(),
                purpose: Purpose::Current,
                chat: false,
            };
            let mut composer = AgentQuickComposer::default();
            let active = serde_json::json!({
                "task_id":"task:active", "title":"Active", "assigned_agent":owner.agent,
                "active_agent":owner.agent, "status":"in_progress", "route":"model",
                "run_epoch":7, "after_revision":2, "applied_revision":1,
            });
            let mut queued = active.clone();
            queued["task_id"] = "task:queued".into();
            queued["status"] = "todo".into();
            queued["run_epoch"] = 0.into();
            queued["active_agent"] = Value::Null;
            let mut finished = queued.clone();
            finished["task_id"] = "task:finished".into();
            finished["status"] = "done".into();
            actions.complete(
                &operation,
                Some(
                    serde_json::json!({"complete":true,"total":3,"tasks":[active,queued,finished]}),
                ),
                &mut composer,
            );
            assert_eq!(actions.tasks.len(), 1);
            assert_eq!(actions.tasks[0].task_id, "task:active");
            actions.complete(
                &operation,
                Some(serde_json::json!({"complete":true,"total":1,"tasks":[{"malformed":true}]})),
                &mut composer,
            );
            let empty = actions.tasks.is_empty();
            drop(actions);
            assert!(empty, "malformed inventories still fail closed");
        }

        fn request(intent: ComposerIntent) -> ComposerRequest {
            ComposerRequest {
                owner: ComposerOwner {
                    room: "room".into(),
                    agent: "tide".into(),
                    person: "person".into(),
                    identity_revision: 0,
                },
                intent,
                submission: 4,
                text: "private direction".into(),
                stop: false,
                task: Some(ComposerTask {
                    task_id: "task:one".into(),
                    run_epoch: 7,
                    revision: 2,
                    label: "Current task".into(),
                }),
            }
        }

        #[test]
        fn expansion_publishes_only_an_opaque_token_not_the_private_editor() {
            use crate::shell::agent_quick_composer::marker::AgentQuickComposerOrigin;
            use ui::widget::Widget;
            use vello::kurbo::Rect;
            let setup = request(ComposerIntent::Message);
            let mut composer = AgentQuickComposer::default();
            composer.open(
                setup.owner,
                "Agent",
                AgentQuickComposerOrigin {
                    marker: Rect::new(10.0, 10.0, 40.0, 40.0),
                    protected: Rect::new(10.0, 10.0, 40.0, 40.0),
                    context: String::new(),
                },
                Rect::new(0.0, 0.0, 800.0, 600.0),
            );
            composer.bind_marker_task(setup.task.unwrap());
            let mut doc = automerge::AutoCommit::new();
            composer.sync(&doc, &automerge::ROOT);
            fn type_private(widget: &mut dyn Widget, doc: &mut automerge::AutoCommit) -> bool {
                if widget.test_id() == Some("agent-quick-editor") {
                    return widget.perform_action(
                        &ui::a11y::Action::SetValue("unsent secret body".into()),
                        &[],
                        &mut ui::widget::Edits::lend(doc),
                        &automerge::ROOT,
                    );
                }
                widget
                    .children_mut()
                    .into_iter()
                    .any(|child| type_private(child, doc))
            }
            assert!(type_private(&mut composer, &mut doc));
            composer.sync(&doc, &automerge::ROOT);
            let private = composer.expansion_request().unwrap();
            assert_eq!(private.editor.text, "unsent secret body");
            let token = uuid::Uuid::new_v4();
            let operation = expansion_operation(private, token);
            assert_eq!(
                operation.arguments,
                Map::from_iter([("token".into(), token.to_string().into())])
            );
            assert!(
                !serde_json::to_string(&operation.arguments)
                    .unwrap()
                    .contains("unsent secret body")
            );
            assert!(
                doc.get_heads().is_empty(),
                "the real editor never writes room state"
            );
            assert_eq!(operation.action, "adopt-private-draft");
        }

        #[test]
        fn message_dispatches_exact_task_direction_and_requires_durable_receipt() {
            let request = request(ComposerIntent::Message);
            let operation = submission_operation(request.clone()).unwrap();
            assert_eq!(operation.resource, "desktop:task:one");
            assert_eq!(operation.action, "steer");
            assert!(!operation.chat);
            assert_eq!(operation.arguments["run_epoch"], 7);
            assert_eq!(operation.arguments["after_revision"], 2);
            assert_eq!(operation.arguments["text"], request.text);
            let mut steer = request.clone();
            steer.intent = ComposerIntent::Steer;
            let steered = submission_operation(steer).unwrap();
            assert_eq!(steered.action, "steer");
            assert_eq!(steered.arguments, operation.arguments);
            assert_eq!(mutation_outcome(&request, None), ComposerOutcome::Unknown);
            assert_eq!(
                mutation_outcome(&request, Some(&serde_json::json!({}))),
                ComposerOutcome::Unknown
            );
            let value = serde_json::json!({
                "resource":"desktop:task:one", "saved":true, "run_epoch":7, "revision":3,
                "result":{"direction_added":{"task_id":"task:one","revision":3}}
            });
            assert!(matches!(
                mutation_outcome(&request, Some(&value)),
                ComposerOutcome::DirectionSaved(_)
            ));
            let mut wrong = value;
            wrong["result"]["direction_added"]["task_id"] = "task:other".into();
            assert_eq!(
                mutation_outcome(&request, Some(&wrong)),
                ComposerOutcome::Unknown
            );
            let mut missing = request;
            missing.task = None;
            assert!(submission_operation(missing).is_none());
        }

        #[test]
        fn queue_keeps_full_instruction_without_start_or_execution() {
            let mut request = request(ComposerIntent::Queue);
            request.text = format!("{}\nFull second line", "x".repeat(300));
            let operation = submission_operation(request.clone()).unwrap();
            assert_eq!(operation.resource, TASKS);
            assert_eq!(operation.action, "queue");
            assert_eq!(operation.arguments["instruction"], request.text);
            assert_eq!(
                operation.arguments["title"]
                    .as_str()
                    .unwrap()
                    .chars()
                    .count(),
                256
            );
            assert_eq!(operation.arguments["assigned_agent"], "tide");
            assert_eq!(operation.arguments["planning"], "model");
            assert!(!operation.chat);
        }

        #[test]
        fn conversation_opens_exact_queued_task_without_starting_it() {
            use crate::shell::agent_quick_composer::marker::AgentQuickComposerOrigin;
            use vello::kurbo::Rect;
            let owner = request(ComposerIntent::Queue).owner;
            let mut composer = AgentQuickComposer::default();
            let marker = Rect::new(10.0, 10.0, 40.0, 40.0);
            composer.open(
                owner.clone(),
                "Agent",
                AgentQuickComposerOrigin {
                    marker,
                    protected: marker,
                    context: String::new(),
                },
                Rect::new(0.0, 0.0, 800.0, 600.0),
            );
            composer.bind_marker_task(ComposerTask {
                task_id: "task:new".into(),
                run_epoch: 0,
                revision: 0,
                label: "Queued".into(),
            });
            let request = composer.expansion_request().unwrap();
            assert_eq!(request.owner, owner);
            assert_eq!(request.task, ("task:new".into(), 0));
            let token = uuid::Uuid::new_v4();
            let operation = expansion_operation(request, token);
            assert_eq!(operation.action, "adopt-private-draft");
            assert!(operation.chat);
            assert_eq!(
                operation.arguments,
                Map::from_iter([("token".into(), token.to_string().into())])
            );
            let Purpose::Expand { request, .. } = operation.purpose else {
                panic!("private expansion")
            };
            assert_eq!(request.task, ("task:new".into(), 0));
            assert_eq!(request.owner.agent, "tide");
        }

        #[test]
        fn queue_receipt_requires_exact_owner_and_canonical_queued_result() {
            let request = request(ComposerIntent::Queue);
            let value = serde_json::json!({
                "resource":"desktop:task:new", "result":{"queued":{"task_id":"task:new"}},
                "managed_route":true,
                "task":{"id":"task:new", "assigned_agent":"tide", "route":"model"}
            });
            assert_eq!(
                mutation_outcome(&request, Some(&value)),
                ComposerOutcome::Queued {
                    task_id: "task:new".into()
                }
            );
            for (pointer, replacement) in [
                ("/task/assigned_agent", Value::from("reed")),
                ("/result/queued/task_id", Value::from("task:other")),
                ("/resource", Value::from("desktop:task:other")),
                ("/managed_route", Value::from(false)),
            ] {
                let mut wrong = value.clone();
                *wrong.pointer_mut(pointer).unwrap() = replacement;
                assert_eq!(
                    mutation_outcome(&request, Some(&wrong)),
                    ComposerOutcome::Unknown
                );
            }
            assert_eq!(mutation_outcome(&request, None), ComposerOutcome::Unknown);
        }

        #[test]
        fn direction_ack_is_saved_not_applied_and_rejects_another_run_or_revision() {
            let request = request(ComposerIntent::Steer);
            let value = serde_json::json!({
                "resource":"desktop:task:one", "saved":true, "applied":true,
                "run_epoch":7, "revision":3,
                "result":{"direction_added":{"task_id":"task:one","revision":3}}
            });
            let mut saved = request.task.clone().unwrap();
            saved.revision = 3;
            assert_eq!(
                mutation_outcome(&request, Some(&value)),
                ComposerOutcome::DirectionSaved(saved)
            );
            for (pointer, replacement) in [
                ("/run_epoch", Value::from(8)),
                ("/revision", Value::from(4)),
                ("/resource", Value::from("desktop:task:other")),
            ] {
                let mut wrong = value.clone();
                *wrong.pointer_mut(pointer).unwrap() = replacement;
                assert_eq!(
                    mutation_outcome(&request, Some(&wrong)),
                    ComposerOutcome::Unknown
                );
            }
        }

        #[test]
        fn owner_frontier_never_retargets_steer_to_another_agent_or_inactive_run() {
            let owner = request(ComposerIntent::Steer).owner;
            let value = serde_json::json!({
                "task_id":"task:one", "title":"Current task", "assigned_agent":"tide",
                "active_agent":"tide", "status":"in_progress", "route":"model",
                "run_epoch":7, "after_revision":2, "applied_revision":1
            });
            let row: Frontier = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(row.choice(&owner).unwrap().revision, 2);
            for (pointer, replacement) in [
                ("/assigned_agent", Value::from("reed")),
                ("/active_agent", Value::Null),
                ("/run_epoch", Value::from(0)),
                ("/status", Value::from("queued")),
            ] {
                let mut wrong = value.clone();
                *wrong.pointer_mut(pointer).unwrap() = replacement;
                let row: Frontier = serde_json::from_value(wrong).unwrap();
                assert!(row.choice(&owner).is_none());
            }
        }
    }
}
