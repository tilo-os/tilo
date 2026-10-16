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

use crate::shell::agent_quick_composer::{
    AgentQuickComposer, ComposerIntent, ComposerOutcome, ComposerOwner, ComposerReceipt,
    ComposerRequest, ComposerTask,
};
use crate::shell::certified_action_session::{ActionSessionSource, DesktopActionSession};
use crate::shell::search::invocation_wake::{poll, waker, watch};

const REFRESH: Duration = Duration::from_secs(1);
const TIMEOUT: Duration = Duration::from_secs(30);
const TASKS: &str = "desktop:tasks";
const CHAT: &str = "so.tilo.chat";
const DESKTOP: &str = tilo_desktop_model::commands::APP_ID;

#[derive(Clone)]
enum Purpose {
    Current,
    Offer,
    Submit(ComposerRequest),
    Applied {
        request: ComposerRequest,
        saved: ComposerTask,
    },
    Conversation,
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
pub(super) struct AgentActions {
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
    pub(super) fn open(&mut self, source: ActionSessionSource, owner: ComposerOwner) -> bool {
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
            // Who publishes the desktop's task actions in this room: asked
            // first, since a native room serves them as an application's.
            Phase::Providers(watch(session.query(CatalogueQuery {
                max_results: 0,
                ..CatalogueQuery::default()
            })))
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

    pub(super) fn pump(&mut self, composer: &mut AgentQuickComposer) -> bool {
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
        let direct = selected.as_ref().is_some_and(|task| {
            self.offers.contains_key(&ActionAddress::new(
                format!("desktop:{}", task.task_id),
                "direct",
            ))
        });
        let stop = selected.as_ref().is_some_and(|task| {
            self.offers.contains_key(&ActionAddress::new(
                format!("desktop:{}", task.task_id),
                "stop",
            ))
        });
        composer.project(&owner, [direct, queue, direct], self.tasks.clone(), stop);
        if let Some(request) = composer.take_request() {
            self.submit(request);
            changed = true;
        }
        if composer.take_conversation_request() {
            let task = composer.conversation_task();
            self.begin(conversation_operation(&owner, task.as_ref()));
            changed = true;
        }
        changed
    }

    fn task_offers(&mut self, task: &ComposerTask) {
        for action in ["direct", "stop"] {
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
                    || value.get("total").and_then(Value::as_u64) != u64::try_from(rows.len()).ok()
                    || rows.len() > 64
                {
                    self.tasks.clear();
                    return;
                }
                let frontiers: Option<Vec<Frontier>> = rows
                    .iter()
                    .map(|row| serde_json::from_value(row.clone()).ok())
                    .collect();
                self.tasks = frontiers
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|row| row.choice(owner))
                    .collect();
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
            Purpose::Offer | Purpose::Conversation => {}
        }
    }

    fn refuse(&mut self, operation: &Operation, composer: &mut AgentQuickComposer, unknown: bool) {
        if matches!(operation.purpose, Purpose::Current) {
            self.tasks.clear();
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
                    "direct",
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

fn conversation_operation(owner: &ComposerOwner, task: Option<&(String, u64)>) -> Operation {
    Operation {
        resource: format!("chat.agent:{}", owner.agent),
        action: if task.is_some() {
            "open-task-conversation"
        } else {
            "open-agent-conversation"
        }
        .into(),
        // Chat binds workspace and authenticated agent on the Agent row.
        // Task identity is explicit even before Chat has projected this task.
        arguments: task.map_or_else(Map::new, |(id, epoch)| {
            Map::from_iter([
                ("task".into(), id.clone().into()),
                ("run_epoch".into(), (*epoch).into()),
            ])
        }),
        purpose: Purpose::Conversation,
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
    if !operation.chat {
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
                    && action.semantic_identity.as_ref().is_some_and(|identity| {
                        identity.kind == "chat.agent" && identity.key == operation.resource
                    })
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
            Some(serde_json::json!({"complete":true,"total":3,"tasks":[active,queued,finished]})),
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
            ..request(ComposerIntent::Queue).owner
        };
        let mut actions = AgentActions::default();
        assert!(actions.open(
            ActionSessionSource::with_mock(
                crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(),
                host.clone(),
            ),
            owner.clone(),
        ));
        let mut composer = AgentQuickComposer::default();
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
        let offered = actions
            .offers
            .contains_key(&ActionAddress::new(TASKS, "queue"));
        drop(actions);
        assert!(
            offered,
            "queueing is offered once the desktop's own surface answers"
        );
        let queries = host.queries.lock().unwrap().clone();
        assert!(queries.iter().any(|query| {
            query.provider.as_deref() == Some("7")
                && query.target == Some(ActionAddress::new(TASKS, "queue"))
        }));
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
    fn message_dispatches_exact_task_direction_and_requires_durable_receipt() {
        let request = request(ComposerIntent::Message);
        let operation = submission_operation(request.clone()).unwrap();
        assert_eq!(operation.resource, "desktop:task:one");
        assert_eq!(operation.action, "direct");
        assert!(!operation.chat);
        assert_eq!(operation.arguments["run_epoch"], 7);
        assert_eq!(operation.arguments["after_revision"], 2);
        assert_eq!(operation.arguments["text"], request.text);
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
        let owner = request(ComposerIntent::Queue).owner;
        let operation = conversation_operation(&owner, Some(&("task:new".into(), 0)));
        assert_eq!(operation.resource, "chat.agent:tide");
        assert_eq!(operation.action, "open-task-conversation");
        assert!(operation.chat);
        assert_eq!(operation.arguments["task"], "task:new");
        assert_eq!(operation.arguments["run_epoch"], 0);
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
