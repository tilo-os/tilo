//! Nonblocking adapter from private composer intentions to the certified
//! semantic action door. It owns no task or Chat document state.
//!
//! A request goes to the agent's own entry point (`respond`), which answers
//! it or names the work. Work becomes an issue in Tasks assigned to the
//! agent, whose run the asking starts (`ask-agent`), so it runs as an
//! ordinary attributed, undoable action; this adapter follows that run on
//! the room's tracker until it ends. When the agent cannot be asked, the
//! request is asked as it stands and the run decides.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use tasks::model::RunPhase as IssueRunPhase;
use tilo_actions::remote::{
    ActionHostError, CataloguePage, CatalogueQuery, InvocationReceipt, InvokeArgument,
    InvokeRequest, Pending, PendingPoll, ReceiptStatus,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};

use crate::shell::agent_quick_composer::{
    AgentQuickComposer, AnswerUse, ComposerIntent, ComposerOutcome, ComposerOwner, ComposerReceipt,
    ComposerRequest, ComposerRun, ComposerTask, ComposerTurn, RunPhase,
};
use crate::shell::broker_services::FollowedRun;
use crate::shell::certified_action_session::{ActionSessionSource, DesktopActionSession};
use crate::shell::search::invocation_wake::{poll, waker, watch};
use crate::shell::selection_agent::SelectionSeed;

const TIMEOUT: Duration = Duration::from_secs(30);
/// The agent's own answer may take its whole model budget, and a little more.
const RESPOND_TIMEOUT: Duration = Duration::from_secs(45);
/// The Tasks app: a request the composer starts is an issue there, assigned
/// to the agent and started by the asking.
const TRACKER_APP: &str = tilo_action_broker::services::TRACKER_APP;
const TRACKER: &str = tilo_action_broker::services::TRACKER_RESOURCE;
const AGENT: &str = tilo_automation::AGENT_APP_ID;
/// The longest task title, in characters.
const TITLE_CHARS: usize = 120;

/// Where the composer reads the runs it follows: the rooms' trackers.
pub(crate) trait RunSource {
    /// Every issue's latest run in `room`.
    fn runs(&self, room: &str) -> Vec<FollowedRun>;
}

impl RunSource for crate::shell::broker_services::NativeTrackerRuns {
    fn runs(&self, room: &str) -> Vec<FollowedRun> {
        match self.tracker(room) {
            Ok(tracker) => tracker
                .map(|tracker| tracker.followed.clone())
                .unwrap_or_default(),
            Err(error) => {
                tracing::debug!(%error, "the composer could not read the tracker");
                Vec::new()
            }
        }
    }
}

#[derive(Clone)]
enum Purpose {
    Respond(ComposerRequest),
    /// Ask for work: a request's, or what the person chose to do with an
    /// answer.
    Queue {
        request: Option<ComposerRequest>,
        title: String,
    },
    /// A message to, or Stop of, the followed run.
    Submit(ComposerRequest),
}

#[derive(Clone)]
struct Operation {
    resource: String,
    action: String,
    arguments: Map<String, Value>,
    purpose: Purpose,
    /// The application whose action it is.
    app: &'static str,
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

/// The issue run the composer follows: one it asked for, or the one its
/// agent was working on when the person opened it.
struct Followed {
    /// The issue's address.
    issue: String,
    title: String,
    /// The run as the tracker last held it; none until it is read.
    run: Option<FollowedRun>,
}

impl Followed {
    /// The run as the composer shows it: working until the tracker says it
    /// ended, then how, with what its agent said last.
    fn run(&self) -> ComposerRun {
        let said = || self.run.as_ref().and_then(|run| run.said.clone());
        let phase = match self.run.as_ref().map(|run| run.phase) {
            None
            | Some(
                IssueRunPhase::Approved
                | IssueRunPhase::Accepted
                | IssueRunPhase::Planning
                | IssueRunPhase::Executing,
            ) => RunPhase::Working,
            Some(IssueRunPhase::Done) => RunPhase::Finished(said()),
            // Waiting on the person, who answers on the issue's page.
            Some(IssueRunPhase::Waiting | IssueRunPhase::Failed) => {
                RunPhase::NeedsAttention(said())
            }
            Some(IssueRunPhase::Stopped) => RunPhase::Stopped,
        };
        ComposerRun {
            task: ComposerTask {
                task_id: self.issue.clone(),
                run_epoch: self.run.as_ref().map_or(0, |run| run.epoch),
                revision: self.run.as_ref().map_or(0, |run| run.messages),
                label: self.title.clone(),
            },
            phase,
        }
    }
}

/// Re-reads the tracker when the room's owner publishes, not on a clock.
struct Freshness {
    stale: Arc<AtomicBool>,
    watch: tilo_identity_host::policy_change::Watch,
}

impl Default for Freshness {
    fn default() -> Self {
        let stale = Arc::new(AtomicBool::new(true));
        let woken = stale.clone();
        Self {
            stale,
            watch: tilo_identity_host::policy_change::Watch::new(move || {
                woken.store(true, Ordering::SeqCst);
                crate::shell::wake_compositor();
            }),
        }
    }
}

impl Freshness {
    /// Whether the tracker may have changed since it was last read. Armed
    /// again first, so a change during the read wakes the next one.
    fn take(&self) -> bool {
        if !self.stale.swap(false, Ordering::SeqCst) {
            return false;
        }
        self.watch.arm();
        true
    }

    fn mark(&self) {
        self.stale.store(true, Ordering::SeqCst);
    }
}

#[derive(Default)]
pub(super) struct AgentActions {
    owner: Option<ComposerOwner>,
    source: Option<ActionSessionSource>,
    session: Option<DesktopActionSession>,
    calls: Vec<Call>,
    followed: Option<Followed>,
    /// A run the person stopped or that ended is not followed again.
    retired: Vec<String>,
    /// Messages to the run saved on its issue, until its agent takes them in.
    sent: Vec<(ComposerRequest, ComposerTask)>,
    fresh: Freshness,
}

impl AgentActions {
    pub(super) fn open(&mut self, source: ActionSessionSource, owner: ComposerOwner) -> bool {
        // Opening chrome must not retire a still-observable request or a
        // saved message. It is never a cancellation or a retry.
        if self.owner.as_ref() == Some(&owner) && self.session.is_some() {
            return true;
        }
        if !self.calls.is_empty() || !self.sent.is_empty() {
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
        self.followed = None;
        self.fresh.mark();
        true
    }

    fn begin(&mut self, operation: Operation) {
        let (Some(session), Some(_)) = (&self.session, &self.owner) else {
            return;
        };
        let deadline = deadline_after(if matches!(operation.purpose, Purpose::Respond(_)) {
            RESPOND_TIMEOUT
        } else {
            TIMEOUT
        });
        // Who publishes the action in this room is asked first.
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

    /// Read the room's tracker when it may have changed: the followed run's
    /// phase and messages, or the agent's one going run, which the composer
    /// then follows.
    fn read_runs(
        &mut self,
        owner: &ComposerOwner,
        source: Option<&dyn RunSource>,
        composer: &mut AgentQuickComposer,
    ) {
        let Some(source) = source else {
            return;
        };
        if !self.fresh.take() {
            return;
        }
        let runs = source.runs(&owner.room);
        if self.followed.is_none() && composer.followed().is_none() {
            let agent = format!("agent:{}", owner.agent);
            let going: Vec<&FollowedRun> = runs
                .iter()
                .filter(|run| {
                    run.agent == agent && run.phase.active() && !self.retired.contains(&run.issue)
                })
                .collect();
            if let [run] = going.as_slice() {
                self.followed = Some(Followed {
                    issue: run.issue.clone(),
                    title: run.title.clone(),
                    run: None,
                });
            }
        }
        let Some(followed) = &mut self.followed else {
            return;
        };
        followed.run = runs.into_iter().find(|run| run.issue == followed.issue);
        let Some(run) = &followed.run else {
            return;
        };
        let incorporated = run.incorporated;
        let epoch = run.epoch;
        self.sent.retain(|(request, saved)| {
            if saved.run_epoch != epoch {
                return false;
            }
            if incorporated < saved.revision {
                return true;
            }
            composer.observe_receipt(ComposerReceipt {
                owner: request.owner.clone(),
                submission: request.submission,
                outcome: ComposerOutcome::DirectionApplied(saved.clone()),
            });
            false
        });
    }

    pub(super) fn pump(
        &mut self,
        composer: &mut AgentQuickComposer,
        runs: Option<&dyn RunSource>,
    ) -> bool {
        let Some(owner) = self.owner.clone() else {
            return false;
        };
        let active = composer.owner() == Some(&owner) && composer.is_open();
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
            self.followed = None;
            self.session = None;
            self.sent.clear();
            composer.project(&owner, None);
            return changed && active;
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
        if !active {
            self.fresh.mark();
            return changed;
        }
        self.read_runs(&owner, runs, composer);
        let run = self.followed.as_ref().map(Followed::run);
        if run.as_ref().is_some_and(|run| !run.phase.running()) {
            // Told now; following ends with it.
            if let Some(followed) = self.followed.take() {
                self.retired.push(followed.issue);
            }
            self.sent.clear();
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

    /// Open for `owner` and ask its agent for `title`'s work, described by
    /// `body`: an issue in Tasks whose run the asking starts, as the composer
    /// asks. Whether it was asked.
    pub(super) fn open_and_ask(
        &mut self,
        source: ActionSessionSource,
        owner: &ComposerOwner,
        (title, body): (&str, &str),
    ) -> bool {
        if !self.open(source, owner.clone()) {
            return false;
        }
        self.begin(queue_operation(owner, None, (title, body), None, None));
        true
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
        self.begin(queue_operation(
            owner,
            None,
            (title, &instruction),
            about,
            None,
        ));
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
            Step::Complete(value) => {
                self.complete(&call.operation, value.as_ref(), composer);
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
        value: Option<&Value>,
        composer: &mut AgentQuickComposer,
    ) {
        match &operation.purpose {
            Purpose::Respond(request) => match respond_outcome(value) {
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
                let asked = asked_issue(operation);
                if let Some(issue) = &asked {
                    self.followed = Some(Followed {
                        issue: issue.clone(),
                        title: title.clone(),
                        run: None,
                    });
                    self.fresh.mark();
                }
                if let Some(request) = request {
                    composer.observe_receipt(ComposerReceipt {
                        owner: request.owner.clone(),
                        submission: request.submission,
                        outcome: asked.map_or(ComposerOutcome::Unknown, |task_id| {
                            ComposerOutcome::Started {
                                task_id,
                                title: title.clone(),
                            }
                        }),
                    });
                }
            }
            Purpose::Submit(request) => {
                // The issue took the message or the stop; the tracker shows
                // when the run takes the message in.
                let outcome = match &request.task {
                    Some(task) if request.stop => ComposerOutcome::Stopped(task.clone()),
                    Some(task) => {
                        let saved = ComposerTask {
                            revision: task.revision.saturating_add(1),
                            ..task.clone()
                        };
                        self.sent.push((request.clone(), saved.clone()));
                        ComposerOutcome::DirectionSaved(saved)
                    }
                    None => ComposerOutcome::Unknown,
                };
                if matches!(outcome, ComposerOutcome::Stopped(_)) {
                    self.followed = None;
                }
                composer.observe_receipt(ComposerReceipt {
                    owner: request.owner.clone(),
                    submission: request.submission,
                    outcome,
                });
            }
        }
    }

    fn refuse(&mut self, operation: &Operation, composer: &mut AgentQuickComposer, unknown: bool) {
        match &operation.purpose {
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
            Purpose::Queue { request: None, .. } => {}
        }
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.calls.iter().map(|call| call.deadline).min()
    }
}

fn deadline_after(duration: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(duration).unwrap_or(now)
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
        app: AGENT,
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
        (&title, &instruction),
        request.about.as_ref(),
        request.model.as_ref(),
    )
}

/// The task the composer starts: the one place it does, so where tasks live
/// changes here alone. It is an issue in the Tasks app assigned to `owner`'s
/// agent, whose run the asking starts (the person asking approves it), with
/// `model` in place of the agent's own when one was picked. What it is about
/// rides as its subject and grants nothing.
fn queue_operation(
    owner: &ComposerOwner,
    request: Option<ComposerRequest>,
    (title, instruction): (&str, &str),
    about: Option<&SelectionSeed>,
    model: Option<&runtime_collab::TaskRunModel>,
) -> Operation {
    let title: String = title.trim().chars().take(TITLE_CHARS).collect();
    let mut arguments = Map::from_iter([
        ("id".into(), uuid::Uuid::new_v4().to_string().into()),
        ("title".into(), title.clone().into()),
        ("body".into(), instruction.into()),
        ("agent".into(), format!("agent:{}", owner.agent).into()),
    ]);
    if let Some(model) = model {
        arguments.insert(
            "model".into(),
            serde_json::json!({ "profile": model.profile, "model": model.model }),
        );
    }
    // The selection rides as the issue's subject, so the agent is told the
    // exact place to act at; it attaches nothing, so the run's authority
    // stays the asker's.
    if let Some(subject) = about
        .and_then(SelectionSeed::subject_json)
        .and_then(|subject| serde_json::from_str::<Value>(&subject).ok())
    {
        arguments.insert("subject".into(), subject);
    }
    if let Some(about) = about {
        tracing::info!(
            app_id = %about.app_id,
            place = about
                .selection
                .as_ref()
                .is_some_and(|selection| selection.anchor.place.is_some()),
            subject = arguments.contains_key("subject"),
            "asking an agent about a selection"
        );
    }
    Operation {
        resource: TRACKER.into(),
        action: "ask-agent".into(),
        arguments,
        purpose: Purpose::Queue { request, title },
        app: TRACKER_APP,
    }
}

/// The issue an `ask-agent` operation makes, by its address.
fn asked_issue(operation: &Operation) -> Option<String> {
    let id = operation.arguments.get("id")?.as_str()?;
    Some(format!(
        "{}{id}",
        tilo_action_broker::services::TRACKER_ISSUE_PREFIX
    ))
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
    let issue = request
        .task
        .as_ref()?
        .task_id
        .strip_prefix(tilo_action_broker::services::TRACKER_ISSUE_PREFIX)?
        .to_owned();
    let mut arguments = Map::from_iter([("issue".into(), issue.into())]);
    let action = if request.stop {
        "stop-run"
    } else {
        arguments.insert("text".into(), request.text.clone().into());
        "send-message"
    };
    Some(Operation {
        resource: TRACKER.into(),
        action: action.into(),
        arguments,
        purpose: Purpose::Submit(request),
        app: TRACKER_APP,
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
    let app = operation.app;
    // The tracker is reached whether or not Tasks runs: the host launches it
    // to take the issue.
    if app == TRACKER_APP {
        return invoke_operation(
            session,
            operation,
            tilo_actions::remote::application_action_target(
                &owner.room,
                app,
                operation.resource.clone(),
                operation.action.clone(),
            ),
            None,
        );
    }
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
    invoke_operation(session, operation, target, Some(page.revision))
}

/// Invoke `operation` at `target`, chosen from the catalogue at `revision`.
fn invoke_operation(
    session: &DesktopActionSession,
    operation: &Operation,
    target: tilo_actions::remote::RemoteActionTarget,
    revision: Option<String>,
) -> Step {
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
        revision,
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

fn exact_row(row: &tilo_actions::remote::CatalogueRow, operation: &Operation) -> bool {
    row.action.as_ref().is_some_and(|action| {
        action.address.action == operation.action
            && action.address.resource == operation.resource
            && row.resource.id == operation.resource
            && row.app_id == operation.app
    })
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

    fn request(intent: ComposerIntent) -> ComposerRequest {
        ComposerRequest {
            owner: owner("room"),
            intent,
            submission: 4,
            text: "Fix the link\nand tell me where it points".into(),
            stop: false,
            task: Some(ComposerTask {
                task_id: "tasks.tracker/issues/one".into(),
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
            model: None,
        }
    }

    #[test]
    fn a_request_asks_the_agent_and_a_running_task_is_steered_or_stopped() {
        let mut asked = request(ComposerIntent::Request);
        asked.task = None;
        let operation = submission_operation(asked.clone()).unwrap();
        assert_eq!(operation.resource, "agent.transform:default");
        assert_eq!(operation.action, "respond");
        assert_eq!(operation.app, AGENT);
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
        assert_eq!(
            (operation.resource.as_str(), operation.action.as_str()),
            (TRACKER, "send-message")
        );
        assert_eq!(operation.app, TRACKER_APP);
        assert_eq!(operation.arguments["issue"], "one");
        assert_eq!(operation.arguments["text"], steer.text);

        let mut stop = steer;
        stop.stop = true;
        let operation = submission_operation(stop.clone()).unwrap();
        assert_eq!(operation.action, "stop-run");
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
        assert_eq!(
            (
                operation.resource.as_str(),
                operation.action.as_str(),
                operation.app
            ),
            (TRACKER, "ask-agent", TRACKER_APP)
        );
        assert_eq!(
            operation.arguments["title"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            TITLE_CHARS
        );
        assert_eq!(operation.arguments["agent"], "agent:tide");
        assert!(
            operation.arguments.get("model").is_none(),
            "the agent's own model"
        );
        let instruction = operation.arguments["body"].as_str().unwrap();
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
        assert_eq!(operation.action, "ask-agent");
        assert_eq!(
            operation.arguments["body"], "Tighten this",
            "the excerpt travels in the anchor, not the instruction"
        );
        let subject = operation.arguments["subject"].clone();
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
        assert!(operation.arguments.get("subject").is_none());
        let instruction = operation.arguments["body"].as_str().unwrap();
        assert!(instruction.starts_with("Tighten this\n\n"));
        assert!(instruction.contains("selected in Terminal"));
        assert!(instruction.contains("<<<\nls -la /srv\n>>>"));
    }

    #[test]
    fn a_thing_told_from_a_context_menu_is_a_typed_subject_of_an_ordinary_assigned_task() {
        use ui::shared_selection::SharedSelection;

        let mut request = request(ComposerIntent::Request);
        request.text = "File this under Q3".into();
        request.conversation.clear();
        // What a row's context menu attaches: its item, in its resource.
        request.about = Some(SelectionSeed::new(
            "so.tilo.mail",
            "Mail",
            Some(
                SharedSelection::of_text("Invoice March")
                    .at(tilo_actions::SelectionPlace::item("msg-7"))
                    .in_resource(tilo_actions::ActionResource {
                        id: "mailbox:inbox".into(),
                        kind: "mail.mailbox".into(),
                        label: "Inbox".into(),
                    }),
            ),
        ));
        request.model = Some(runtime_collab::TaskRunModel {
            profile: "work".into(),
            model: "fast".into(),
        });
        let operation = request_queue(&request, None);
        // The one door every task goes through: an issue the asking starts,
        // an action the broker admits like any other.
        assert_eq!(
            (operation.resource.as_str(), operation.action.as_str()),
            (TRACKER, "ask-agent")
        );
        let mut keys: Vec<_> = operation.arguments.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["agent", "body", "id", "model", "subject", "title"],
            "the reference names a place and grants nothing: no scope, resource or grant"
        );
        assert_eq!(
            operation.arguments["agent"],
            format!("agent:{}", request.owner.agent)
        );
        assert_eq!(operation.arguments["model"]["model"], "fast");
        assert!(
            asked_issue(&operation)
                .unwrap()
                .starts_with("tasks.tracker/issues/")
        );
        let subject: tilo_actions::SelectionSubject =
            serde_json::from_value(operation.arguments["subject"].clone()).unwrap();
        subject.validate().unwrap();
        assert_eq!(subject.resource.id, "mailbox:inbox");
        assert_eq!(
            subject.anchor.place,
            Some(tilo_actions::SelectionPlace::item("msg-7"))
        );
        assert_eq!(operation.arguments["body"], "File this under Q3");
    }

    /// A room with an agent that answers `respond` as scripted, and whose
    /// tracker takes what the composer asks.
    type Invoked = Vec<(String, String, Map<String, Value>)>;

    struct Room {
        id: String,
        respond: Mutex<Option<Value>>,
        invoked: Mutex<Invoked>,
    }

    impl Room {
        fn new(respond: Option<Value>) -> Arc<Self> {
            Arc::new(Self {
                id: uuid::Uuid::new_v4().to_string(),
                respond: Mutex::new(respond),
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
                arguments,
            ));
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

        fn cancel(
            &self,
            _: &Caller,
            _: &InvocationId,
        ) -> tilo_actions::remote::Pending<Result<(), ActionHostError>> {
            tilo_actions::remote::Pending::ready(Err(ActionHostError::UnknownInvocation))
        }
    }

    fn composer_in(room: &Arc<Room>) -> (AgentActions, AgentQuickComposer, ComposerOwner) {
        let owner = owner(&room.id);
        let mut actions = AgentActions::default();
        assert!(actions.open(
            ActionSessionSource::with_mock(
                crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(),
                room.clone(),
            ),
            owner.clone(),
        ));
        let mut composer = AgentQuickComposer::default();
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

    fn perform(composer: &mut AgentQuickComposer, id: &str, action: &ui::a11y::Action) {
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

    fn send(composer: &mut AgentQuickComposer, text: &str) {
        perform(
            composer,
            "agent-quick-editor",
            &ui::a11y::Action::SetValue(text.into()),
        );
        perform(composer, "agent-quick-send", &ui::a11y::Action::Activate);
    }

    /// The room's tracker, holding the runs the test says.
    #[derive(Default)]
    struct Tracker(Mutex<Vec<FollowedRun>>);

    impl RunSource for Tracker {
        fn runs(&self, _: &str) -> Vec<FollowedRun> {
            self.0.lock().unwrap().clone()
        }
    }

    impl Tracker {
        /// The issue's run is at `phase` now, and the owner published it.
        fn holds(&self, actions: &AgentActions, run: FollowedRun) {
            *self.0.lock().unwrap() = vec![run];
            actions.fresh.mark();
        }
    }

    fn run_of(issue: &str, phase: IssueRunPhase) -> FollowedRun {
        FollowedRun {
            issue: issue.into(),
            title: "Fix the link".into(),
            agent: "agent:tide".into(),
            starter: "person".into(),
            epoch: 1,
            phase,
            messages: 0,
            incorporated: 0,
            said: None,
        }
    }

    fn settle(actions: &mut AgentActions, composer: &mut AgentQuickComposer, tracker: &Tracker) {
        for _ in 0..32 {
            actions.pump(composer, Some(tracker));
        }
    }

    #[test]
    fn a_question_is_answered_in_place_and_starts_no_task() {
        let room = Room::new(Some(
            serde_json::json!({"answer": "It points nowhere yet."}),
        ));
        let (mut actions, mut composer, _) = composer_in(&room);
        let tracker = Tracker::default();
        send(&mut composer, "Where does this link point?");
        settle(&mut actions, &mut composer, &tracker);
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
    fn work_becomes_an_issue_the_asking_started_which_the_field_steers_and_stops() {
        let room = Room::new(Some(serde_json::json!({"work": "Fix the link"})));
        let (mut actions, mut composer, _) = composer_in(&room);
        let tracker = Tracker::default();
        send(&mut composer, "Replace the parenthesis with the real link");
        settle(&mut actions, &mut composer, &tracker);
        let asked = room.arguments("ask-agent");
        assert_eq!(asked["title"], "Fix the link");
        assert_eq!(asked["agent"], "agent:tide");
        assert!(
            !room
                .invoked()
                .iter()
                .any(|call| call.ends_with("#approve-run")),
            "the asking approved the run"
        );
        let run = composer.followed().cloned().expect("the run is followed");
        assert_eq!(run.phase, RunPhase::Working);
        assert_eq!(
            run.task.task_id,
            format!("tasks.tracker/issues/{}", asked["id"].as_str().unwrap())
        );

        send(&mut composer, "Use the https address");
        settle(&mut actions, &mut composer, &tracker);
        let message = room.arguments("send-message");
        assert_eq!(message["text"], "Use the https address");
        assert_eq!(message["issue"], asked["id"]);
        assert_eq!(
            room.invoked()
                .iter()
                .filter(|call| call.ends_with("#respond"))
                .count(),
            1,
            "a message to the run is not a new request"
        );

        perform(
            &mut composer,
            "agent-quick-stop",
            &ui::a11y::Action::Activate,
        );
        settle(&mut actions, &mut composer, &tracker);
        assert_eq!(room.arguments("stop-run")["issue"], asked["id"]);
        assert!(actions.followed.is_none(), "a stopped run is not followed");
    }

    #[test]
    fn the_composer_follows_the_issues_run_until_the_tracker_says_it_ended() {
        let room = Room::new(Some(serde_json::json!({"work": "Fix the link"})));
        let (mut actions, mut composer, _) = composer_in(&room);
        let tracker = Tracker::default();
        send(&mut composer, "Replace the parenthesis with the real link");
        settle(&mut actions, &mut composer, &tracker);
        let issue = format!(
            "tasks.tracker/issues/{}",
            room.arguments("ask-agent")["id"].as_str().unwrap()
        );
        tracker.holds(&actions, run_of(&issue, IssueRunPhase::Executing));
        settle(&mut actions, &mut composer, &tracker);
        let run = composer.followed().cloned().unwrap();
        assert_eq!((run.phase, run.task.run_epoch), (RunPhase::Working, 1));

        send(&mut composer, "Use the https address");
        settle(&mut actions, &mut composer, &tracker);
        assert_eq!(actions.sent.len(), 1, "saved, not taken in yet");
        tracker.holds(
            &actions,
            FollowedRun {
                messages: 1,
                incorporated: 1,
                ..run_of(&issue, IssueRunPhase::Executing)
            },
        );
        settle(&mut actions, &mut composer, &tracker);
        assert!(actions.sent.is_empty(), "the run took the message in");

        tracker.holds(
            &actions,
            FollowedRun {
                said: Some("Linked it to tilo.so.".into()),
                ..run_of(&issue, IssueRunPhase::Done)
            },
        );
        settle(&mut actions, &mut composer, &tracker);
        assert!(composer.followed().is_none(), "told, and no longer working");
        assert!(actions.followed.is_none());
        assert!(actions.retired.contains(&issue));
    }

    #[test]
    fn an_ended_run_reads_as_how_it_ended_with_its_agents_last_words() {
        let followed = |phase, said: Option<&str>| {
            Followed {
                issue: "tasks.tracker/issues/one".into(),
                title: "Fix the link".into(),
                run: Some(FollowedRun {
                    said: said.map(str::to_owned),
                    ..run_of("tasks.tracker/issues/one", phase)
                }),
            }
            .run()
            .phase
        };
        assert_eq!(
            followed(IssueRunPhase::Done, Some("Linked it.")),
            RunPhase::Finished(Some("Linked it.".into()))
        );
        assert_eq!(
            followed(IssueRunPhase::Failed, Some("No access")),
            RunPhase::NeedsAttention(Some("No access".into()))
        );
        assert_eq!(
            followed(IssueRunPhase::Waiting, None),
            RunPhase::NeedsAttention(None)
        );
        assert_eq!(followed(IssueRunPhase::Stopped, None), RunPhase::Stopped);
        assert_eq!(followed(IssueRunPhase::Planning, None), RunPhase::Working);
    }

    #[test]
    fn the_agents_one_going_run_is_followed_when_the_composer_opens() {
        let room = Room::new(None);
        let (mut actions, mut composer, _) = composer_in(&room);
        let tracker = Tracker::default();
        tracker.holds(
            &actions,
            run_of("tasks.tracker/issues/one", IssueRunPhase::Planning),
        );
        settle(&mut actions, &mut composer, &tracker);
        assert_eq!(
            composer.followed().map(|run| run.task.task_id.as_str()),
            Some("tasks.tracker/issues/one")
        );
    }

    #[test]
    fn a_request_the_agent_cannot_take_is_queued_as_it_stands() {
        let room = Room::new(None);
        let (mut actions, mut composer, _) = composer_in(&room);
        let tracker = Tracker::default();
        send(&mut composer, "Tidy the notes\nall of them");
        settle(&mut actions, &mut composer, &tracker);
        let queued = room.arguments("ask-agent");
        assert_eq!(queued["title"], "Tidy the notes");
        assert!(
            queued["body"]
                .as_str()
                .unwrap()
                .starts_with("Tidy the notes\nall of them")
        );
        assert!(composer.followed().is_some());
    }
}
