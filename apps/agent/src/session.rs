//! One agent session: join the room and carry out broker-leased tasks.

use std::path::PathBuf;
use ui::time::{Duration, Instant};

use anyhow::Result;
use tilo_actions::remote::RemoteActionMessage;
use ui::collaboration::{
    CertifiedEndpointIdentity, CollabEvent, CollabRequest, PresencePayload, RouteSpec,
};
use ui::permissions::PermissionContext;
use ui::presence::PresenceKey;
use uuid::Uuid;

use crate::model::Chooser;
use crate::providers::default_selection;
use crate::surface;
use crate::wire::Link;

mod agent_loop;
mod confirmation;
mod native_task;
mod outstanding;
mod selection_prompt;

/// The most characters the Task Center carrier of a non-workspace task
/// takes in one update.
const LEGACY_TASK_MESSAGE_CHARS: usize = 280;

/// How long a task waits for the broker's pushed action catalogue.
const TASK_CATALOGUE_WAIT: Duration = Duration::from_secs(2);
const TASK_ACTION_WAIT: Duration = Duration::from_secs(30);
/// The first pause before observing a running invocation; it doubles up to
/// [`FOLLOW_MOST_WAIT`] while the invocation keeps running.
const FOLLOW_FIRST_WAIT: Duration = Duration::from_millis(5);
const FOLLOW_MOST_WAIT: Duration = Duration::from_millis(50);
#[derive(Clone, Debug, PartialEq, Eq)]
struct TaskAction {
    app: String,
    peer_id: u64,
    provider: String,
    resource: String,
    resource_kind: String,
    resource_label: String,
    action: String,
    arguments: serde_json::Map<String, serde_json::Value>,
    label: String,
    /// The parameter whose content comes from the immediately preceding
    /// action result, never from model-authored JSON.
    carries: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct StepOutcome {
    /// The application-declared result returned by the broker.
    output: tilo_actions::ActionOutput,
}

impl StepOutcome {
    fn content_hash(&self) -> Result<&str, String> {
        match self.output.resources.as_slice() {
            [] => Err("the previous action produced no content resource".into()),
            [resource] => tilo_actions::content_hash(resource).ok_or_else(|| {
                "the previous action produced a resource that is not tilo.content".into()
            }),
            _ => Err("the previous action must produce exactly one content resource".into()),
        }
    }
}

fn arguments_for_step(
    carries: Option<&str>,
    planned: &serde_json::Map<String, serde_json::Value>,
    previous: Option<&StepOutcome>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let Some(param) = carries else {
        return Ok(planned.clone());
    };
    let hash = previous
        .ok_or_else(|| "a content-consuming action has no preceding action result".to_string())?
        .content_hash()?;
    let mut arguments = planned.clone();
    // Replace any model-authored value: the descriptor says the value is
    // carried, so only the preceding application may supply it.
    arguments.insert(
        param.to_owned(),
        serde_json::Value::String(hash.to_string()),
    );
    Ok(arguments)
}

#[derive(Debug)]
enum TaskActionStop {
    Cancelled(Option<String>),
    Failed(String),
}

/// Most actions one task may invoke.
const MAX_TASK_STEPS: usize = 64;

/// A legacy invocation the host accepted, followed to its outcome alone
/// ([`Session::invoke_task_action`]) or beside others started in the same
/// turn. Each keeps its own invocation id, interaction sequence and broker
/// job: following several at once shares only the waiting.
struct Following {
    action: TaskAction,
    /// The arguments it was started with: a native child is re-asked with
    /// the identical request.
    arguments: serde_json::Map<String, serde_json::Value>,
    invocation: tilo_actions::InvocationId,
    /// How a native task's child is observed; `None` for a Task Center
    /// invocation, observed through the action host.
    reask: Option<native_task::Reask>,
    /// The latest state the host reported.
    update: tilo_actions::InvocationUpdate,
    /// The last semantic interaction sequence published for it.
    sequence: u64,
    deadline: Instant,
    /// Its outcome, once settled.
    outcome: Option<Result<StepOutcome, TaskActionStop>>,
}

impl Following {
    fn into_outcome(self) -> Result<StepOutcome, TaskActionStop> {
        self.outcome.unwrap_or_else(|| {
            Err(TaskActionStop::Failed(
                "Task action did not reach a terminal state before its deadline".into(),
            ))
        })
    }
}

fn validate_task_run(run: &runtime_collab::TaskRunEnvelope) -> Result<(), String> {
    if run.version != 1 || run.run_id == 0 {
        return Err("The task envelope has an unsupported version or empty run id".into());
    }
    if run.task_id.trim().is_empty()
        || run.task_id.len() > 256
        || run.title.trim().is_empty()
        || run.title.len() > 4 * 1024
        || run.instruction().trim().is_empty()
        || run.instruction().len() > 32 * 1024
        || run.lease.is_empty()
        || run.lease.len() > 128
    {
        return Err("The task envelope has an invalid id, title, or lease".into());
    }
    if let Some(budget) = run.execution_budget {
        budget.validate().map_err(str::to_owned)?;
    }
    let context = serde_json::from_value::<tilo_actions::ActionContext>(run.action_context.clone())
        .map_err(|_| "The task has an invalid action context".to_string())?;
    let valid_group = context.group.as_ref().is_some_and(|group| {
        group.id == run.task_id && group.label == run.title && group.kind == "task"
    });
    if context.purpose.as_deref() != Some(run.title.as_str()) || !valid_group {
        return Err("The task action context does not bind this task title and id".into());
    }
    Ok(())
}

fn task_action_from_offer(
    offer: &surface::Offer,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<TaskAction, String> {
    let peer_id = match (offer.broker_peer_id, offer.broker_provider.as_ref()) {
        (Some(peer), _) => peer,
        (None, Some(_)) => 0,
        (None, None) => return Err("Tasks cannot invoke a local-only action".into()),
    };
    if !offer.wants_content() {
        tilo_actions::ActionArgs::bind_json(&offer.descriptor.params, &arguments)
            .map_err(|error| format!("The planned action arguments are invalid: {error}"))?;
    }
    Ok(TaskAction {
        app: offer.app.clone(),
        peer_id,
        provider: offer.broker_provider.clone().unwrap_or_default(),
        resource: offer.descriptor.address.resource.clone(),
        resource_kind: offer.descriptor.resource_kind.clone(),
        resource_label: offer.descriptor.resource_label.clone(),
        action: offer.descriptor.address.action.clone(),
        arguments,
        label: offer.descriptor.action_label.clone(),
        carries: offer.content_param(),
    })
}

#[derive(Clone, Debug)]
pub struct Config {
    /// Stable workspace-admitted identity, independent of the display name.
    pub agent_id: String,
    /// Explicit typed Cap'n Proto endpoint for headless diagnostics outside
    /// the launcher. Normal sessions leave this `None` and inherit the fd.
    pub endpoint: Option<PathBuf>,
    /// Certified device credentials for an explicit endpoint. Launcher-bound
    /// sessions leave this `None` and use the inherited identity service.
    pub endpoint_identity: Option<CertifiedEndpointIdentity>,
    /// The app whose document this is — its manifest package id, which is
    /// what a launched copy of the app announces.
    pub app_id: String,
    /// The room (workspace) the document lives in.
    pub cluster: Uuid,
    /// The record the app announced for the document (`file:<path>`), or
    /// `None` for an untitled document.
    pub record: Option<String>,
    /// Display name used by name-addressed instructions. The admitted id
    /// remains the author and structured-mention target.
    pub name: String,
    /// Let a model choose which exported action answers an instruction.
    /// False pins the run to the keyword vocabulary.
    pub model: bool,
    /// How long that call may take before the keyword vocabulary decides
    /// instead. Short on purpose: the work must not wait on the network.
    pub model_timeout: Duration,
    /// What the agent may enumerate and perform. A denied operation is
    /// absent from what it discovers, never merely refused — see
    /// [`crate::surface::offers`].
    pub permissions: PermissionContext,
    /// The agent's content store, created once when it starts: content an
    /// operation produced is read from here by the operation consuming it.
    pub content: tilo_content::ContentStore,
    /// Applications started beside the agent whose actions its first run
    /// waits for ([`Session::await_companions`]).
    pub companions: Vec<String>,
    /// The model the desktop chose for this agent, when it launched it with
    /// one; otherwise the Settings default this process can read, if any.
    pub provider: Option<crate::providers::ProviderSelection>,
}

pub struct Session {
    cfg: Config,
    link: Link,
    /// Decides which exported action answers an instruction.
    chooser: Chooser,
    /// Launched by the launcher with a managed context: limited to its
    /// task-bound broker surface and to native task leases.
    managed: bool,
    /// Monotonic deadline from an explicit broker-minted owner task policy.
    task_deadline: Option<Instant>,
    native_task: Option<native_task::NativeTaskState>,
    /// The broker's pushed, permission-filtered live action directory the
    /// task loop surveys.
    broker_surfaces: Vec<ui::collaboration::BrokerActionSurface>,
    next_broker_request: u64,
    /// The companions not yet seen in the catalogue, until the first run
    /// waited for them; empty afterwards.
    awaiting_companions: Vec<String>,
    /// Bumped whenever the broker republishes its catalogue.
    broker_revision: u64,
    /// The task catalogue survey for [`Self::broker_revision`], shared by
    /// every reader until the broker republishes.
    catalogue_cache: Option<(u64, std::sync::Arc<surface::Catalogue>)>,
    /// The model turn in flight while a task loop runs.
    turn: Option<agent_loop::ActiveTurn>,
    /// What this agent was asked, did and said in earlier tasks.
    memory: agent_loop::Memory,
    /// What the task in progress asked its host without waiting in line.
    outstanding: outstanding::Outstanding,
}

/// Ask the broker to select one current holder of this exact route. Safe to
/// repeat while no holder is available; unlike presence fan-out, one request
/// produces at most one full snapshot.
fn ask_for_document(link: &mut Link) -> Result<()> {
    link.send(&CollabRequest::RequestDocumentSnapshot)
}

impl Session {
    /// Connect, announce the route, and ask the room for the document.
    ///
    /// # Errors
    ///
    /// When the connection to the compositor's collaboration broker cannot
    /// be made, or the initial route join or presence announce fails to
    /// send.
    pub fn connect(cfg: Config) -> Result<Self> {
        let managed = managed_context(&cfg)?;
        let mut link = match &cfg.endpoint {
            Some(path) => Link::connect_endpoint(
                path,
                &cfg.app_id,
                cfg.cluster,
                cfg.record.clone(),
                Some(cfg.agent_id.clone()),
                cfg.endpoint_identity.clone(),
            )?,
            None => Link::connect(
                &cfg.app_id,
                cfg.cluster,
                cfg.record.clone(),
                Some(cfg.agent_id.clone()),
            )?,
        };
        // Discovery is room-wide, but writes never are. This subscription
        // lets the agent learn other applications' exported operations while
        // every mutation below still names its owning app route explicitly.
        if managed.is_none() {
            // A managed agent is limited to its task-bound broker surface: the broker refuses the
            // cluster route, the late-join snapshot and presence on that descriptor, and one
            // refused snapshot RPC shuts down the whole collaboration bridge. Its catalogue
            // arrives through the task-bound ActionCatalogue instead.
            link.join_route(RouteSpec::cluster(cfg.cluster));
            // The broker directs the late-join request to one existing holder.
            ask_for_document(&mut link)?;
            link.send(&CollabRequest::Presence {
                key: PresenceKey::Workspace {
                    id: cfg.cluster.to_string(),
                },
                payload: PresencePayload::Arrived,
            })?;
        }
        tracing::info!(
            transport = if cfg.endpoint.is_some() { "typed-endpoint" } else { "TILO_SVC_COLLAB_FD" },
            app = %cfg.app_id,
            room = %cfg.cluster,
            record = ?cfg.record,
            name = %cfg.name,
            "joined the room; waiting for the document"
        );
        let is_managed = managed.is_some();
        let cfg_companions = cfg.companions.clone();
        let chooser = match managed {
            Some(selection) => {
                let chooser = Chooser::with_provider(true, cfg.model_timeout, selection);
                anyhow::ensure!(chooser.is_live(), "managed model gateway unavailable");
                chooser
            }
            None => cfg.provider.clone().or_else(default_selection).map_or_else(
                || Chooser::new(false, cfg.model_timeout),
                |selection| Chooser::with_provider(cfg.model, cfg.model_timeout, selection),
            ),
        };
        Ok(Self {
            cfg,
            link,
            chooser,
            managed: is_managed,
            task_deadline: None,
            native_task: None,
            broker_surfaces: Vec::new(),
            next_broker_request: 1,
            memory: agent_loop::Memory::default(),
            awaiting_companions: cfg_companions,
            broker_revision: 0,
            catalogue_cache: None,
            turn: None,
            outstanding: outstanding::Outstanding::default(),
        })
    }

    /// Run broker-addressed Task Center leases: an unattended task may invoke
    /// only actions that the authenticated broker currently publishes to this
    /// agent.
    ///
    /// # Errors
    ///
    /// Returns an error when receiving catalogue updates or reporting task
    /// progress through the collaboration link fails.
    pub fn run_task_runs(&mut self) -> Result<()> {
        let mut transforms = crate::transform::TransformService::new(
            std::sync::Arc::new(crate::transform::GatewayModel {
                launched: self.chooser.selection().cloned(),
            }),
            self.cfg.content.clone(),
        );
        // The transforms are published under the agent's own route; a
        // connection announcing another app may not publish for it.
        if self.cfg.app_id == tilo_automation::AGENT_APP_ID {
            self.link.send(&crate::transform::publication())?;
            tracing::info!(
                resource = crate::transform::RESOURCE_ID,
                "published the AI transforms"
            );
        }
        tracing::info!(agent = %self.cfg.name, "waiting for broker-addressed task runs");
        loop {
            match self.link.recv(Duration::from_millis(100))? {
                Some(CollabEvent::ActionCatalogue { surfaces }) => {
                    self.set_broker_surfaces(surfaces);
                }
                Some(CollabEvent::TaskRun(run))
                    if !self.managed
                        || runtime_collab::native_task_revision(&run.lease).is_some() =>
                {
                    if let Err(error) = self.run_task(&run) {
                        if runtime_collab::native_task_revision(&run.lease).is_none() {
                            return Err(error);
                        }
                        tracing::warn!(run_id = run.run_id, %error, "native task stopped before completion");
                        if let Err(report_error) = self.fail_task(&run, &error.to_string()) {
                            tracing::warn!(%report_error, "native task review report was refused");
                        }
                    }
                }
                Some(CollabEvent::Invocation {
                    id,
                    invocation,
                    resource,
                    action,
                    argument,
                    actor,
                    context,
                }) => {
                    if let Some(update) = transforms.start(
                        crate::transform::Directed { id, invocation },
                        &resource,
                        &action,
                        argument.as_deref(),
                        &actor,
                        context.as_deref(),
                    ) {
                        self.link
                            .send(&CollabRequest::UpdateInvocation { id, update })?;
                    }
                }
                Some(CollabEvent::InvocationCancellation { id }) => {
                    if let Some(update) = transforms.cancel(id) {
                        self.link
                            .send(&CollabRequest::UpdateInvocation { id, update })?;
                    }
                }
                Some(_) | None => {}
            }
            for (id, update) in transforms.finished() {
                self.link
                    .send(&CollabRequest::UpdateInvocation { id, update })?;
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one task run keeps planning, per-step reporting and the terminal update in a single auditable sequence"
    )]
    fn run_task(&mut self, run: &runtime_collab::TaskRunEnvelope) -> Result<()> {
        self.task_deadline = run.execution_budget.and_then(|budget| {
            ui::time::now().checked_add(Duration::from_millis(u64::from(budget.total_ms)))
        });
        self.native_task =
            runtime_collab::native_task_revision(&run.lease).map(|(base, revision)| {
                native_task::NativeTaskState {
                    run_id: run.run_id,
                    task_id: run.task_id.clone(),
                    base: base.into(),
                    revision,
                    ..Default::default()
                }
            });
        if let Err(message) = validate_task_run(run) {
            tracing::warn!(run_id = run.run_id, %message, "refused malformed task run");
            self.report_task(run, runtime_collab::TaskRunUpdate::Failed { message })?;
            return Ok(());
        }
        self.begin_task_watch();
        self.report_task(run, runtime_collab::TaskRunUpdate::Accepted)?;
        // None of these three waits for the host: the model call is the long
        // pole, and the planner below starts it at once. The cancellation
        // answer is read before the first step (or the first planned action),
        // which it reaches the host ahead of.
        self.report_task(
            run,
            runtime_collab::TaskRunUpdate::Progress {
                message: "Reviewing the permission-filtered action catalogue".into(),
                presentation: None,
            },
        )?;
        self.poll_cancellation(run)?;
        self.wait_for_task_catalogue()?;
        // A model task is a discover → invoke → observe loop over the live
        // catalogue. A managed agent has no other planner; a legacy one
        // without a model falls back to the rules-only survey below.
        if matches!(run.planning, runtime_collab::TaskRunPlanning::Model)
            && (self.chooser.loop_model().is_ok() || self.managed)
        {
            return match self.run_task_loop(run)? {
                agent_loop::LoopEnd::Finished(finish, completed) => {
                    if self.native_task.is_some() && !finish.done {
                        return self.fail_task(
                            run,
                            finish
                                .reply
                                .as_deref()
                                .unwrap_or("Needs your decision before continuing"),
                        );
                    }
                    // `done: false` is an answer, not a fault: the agent says
                    // in its reply what it could not do, and stays available
                    // for the next message. Reporting it as a failed task
                    // would mark the whole agent failed and end its
                    // conversation. Only the loop's own faults (no model,
                    // a bound reached, a broken turn) fail the task.
                    let finish = if finish.done {
                        finish
                    } else {
                        tracing::info!(completed, "the agent could not complete the request");
                        crate::tools::Finish {
                            reply: Some(finish.reply.unwrap_or_else(|| {
                                "I couldn't complete that with the operations available here."
                                    .into()
                            })),
                            done: true,
                        }
                    };
                    let message = match finish.reply {
                        // The model's own reply is the durable chat answer. A
                        // workspace task keeps the whole of it (an answer to a
                        // question is the task's result); the older Task Center
                        // carrier bounds it to 280 characters.
                        Some(reply) if self.native_task.is_some() => reply,
                        Some(reply) => reply.chars().take(LEGACY_TASK_MESSAGE_CHARS).collect(),
                        None => format!("Completed {completed} broker-approved action(s)"),
                    };
                    self.report_task(run, runtime_collab::TaskRunUpdate::Succeeded { message })
                }
                agent_loop::LoopEnd::Failed(message) => self.fail_task(run, &message),
                agent_loop::LoopEnd::Cancelled(message) => self.cancel_task(run, &message),
            };
        }
        if self
            .native_task
            .as_ref()
            .is_some_and(|state| state.pending.is_some())
        {
            return self.fail_task(
                run,
                "Directions require model planning or person review; none were acknowledged",
            );
        }
        // The rules-only planner has no model call to overlap: settle the
        // poll already out before planning, as before.
        if self.task_cancelled(run)? {
            return self.cancel_task(run, "Cancelled before planning");
        }
        let (actions, reply) = match self.plan_task(run) {
            Ok((actions, reply)) if !actions.is_empty() => (actions, reply),
            Ok(_) => {
                return self.fail_task(run, "No permitted live action can complete this task");
            }
            Err(message) => return self.fail_task(run, &message),
        };
        // What the next content-consuming action is built from.
        let mut previous: Option<StepOutcome> = None;
        for (index, action) in actions.iter().enumerate() {
            if self.task_cancelled(run)? {
                return self.cancel_task(run, "Cancelled before the next action");
            }
            let arguments = match arguments_for_step(
                action.carries.as_deref(),
                &action.arguments,
                previous.as_ref(),
            ) {
                Ok(arguments) => arguments,
                Err(message) => return self.fail_task(run, &message),
            };
            self.report_task(
                run,
                runtime_collab::TaskRunUpdate::Progress {
                    message: format!(
                        "Running {} of {}: {}",
                        index + 1,
                        actions.len(),
                        action.label
                    ),
                    presentation: None,
                },
            )?;
            match self.run_task_step(run, index, action, &arguments)? {
                Ok(outcome) => {
                    self.chooser
                        .mark(tilo_model_gateway::TraceEvent::ActionSettled, index);
                    previous = Some(outcome);
                }
                Err(TaskActionStop::Cancelled(_)) => {
                    return self.cancel_task(run, "Cancelled while an action was running");
                }
                Err(TaskActionStop::Failed(message)) => return self.fail_task(run, &message),
            }
        }
        let message = match reply {
            // The model's own reply is the durable chat answer. The Task
            // Center carrier bounds messages to 280 characters.
            Some(reply) => reply.chars().take(280).collect(),
            None => format!("Completed {} broker-approved action(s)", actions.len()),
        };
        self.report_task(run, runtime_collab::TaskRunUpdate::Succeeded { message })
    }

    /// Invoke one planned action through the broker's invocation path.
    fn run_task_step(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        index: usize,
        action: &TaskAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Result<StepOutcome, TaskActionStop>> {
        // A cancellation poll already out answers ahead of this step in the
        // client's one ordered queue: honouring it costs no round trip.
        if self.cancelled_before_step()? {
            return Ok(Err(TaskActionStop::Cancelled(Some(
                "Cancelled before the next step".into(),
            ))));
        }
        self.chooser
            .mark(tilo_model_gateway::TraceEvent::ActionSubmitted, index);
        let settled = self.invoke_task_action(run, action, arguments)?;
        self.step_settled();
        Ok(settled)
    }

    fn report_task(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        update: runtime_collab::TaskRunUpdate,
    ) -> Result<()> {
        let request_id = self.native_task.as_ref().map(|_| self.next_broker_request);
        if request_id.is_some() {
            self.next_request_id();
        }
        self.link.send(&CollabRequest::ReportTaskRun {
            request_id,
            run_id: run.run_id,
            lease: self.task_lease(run),
            update,
        })?;
        match request_id {
            Some(id) => self.wait_for_native_report(id),
            None => Ok(()),
        }
    }

    fn fail_task(&mut self, run: &runtime_collab::TaskRunEnvelope, message: &str) -> Result<()> {
        self.report_task(
            run,
            runtime_collab::TaskRunUpdate::Failed {
                message: message.to_string(),
            },
        )
    }

    fn cancel_task(&mut self, run: &runtime_collab::TaskRunEnvelope, message: &str) -> Result<()> {
        self.report_task(
            run,
            runtime_collab::TaskRunUpdate::Cancelled {
                message: message.to_string(),
            },
        )
    }

    fn wait_for_task_catalogue(&mut self) -> Result<()> {
        if !self.broker_surfaces.is_empty() {
            return Ok(());
        }
        let deadline = self.task_wait_deadline(TASK_CATALOGUE_WAIT);
        let mut deferred = Vec::new();
        while ui::time::now() < deadline {
            let remaining = deadline.saturating_duration_since(ui::time::now());
            match self.link.recv(remaining.min(Duration::from_millis(100)))? {
                Some(CollabEvent::ActionCatalogue { surfaces }) => {
                    self.set_broker_surfaces(surfaces);
                    self.link.defer(deferred);
                    return Ok(());
                }
                Some(event) => deferred.push(event),
                None => {}
            }
        }
        self.link.defer(deferred);
        Ok(())
    }

    fn task_wait_deadline(&self, maximum: Duration) -> Instant {
        let now = ui::time::now();
        let ordinary = now.checked_add(maximum).unwrap_or(now);
        self.task_deadline
            .map_or(ordinary, |task| ordinary.min(task))
    }

    fn task_budget_expired(&self) -> bool {
        self.task_deadline
            .is_some_and(|deadline| ui::time::now() >= deadline)
    }

    /// The rules-only planner: one surveyed action matched by the words of
    /// the title. Model tasks run [`Self::run_task_loop`] instead.
    fn plan_task(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
    ) -> Result<(Vec<TaskAction>, Option<String>), String> {
        let catalogue = self.task_catalogue();
        let (offer, arguments) = surface::from_verbs(run.instruction(), &catalogue.offers)
            .map_err(|error| format!("Rules-only planner could not choose an action: {error:?}"))?;
        task_action_from_offer(offer, arguments).map(|action| (vec![action], None))
    }

    fn invoke_task_action(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        action: &TaskAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Result<StepOutcome, TaskActionStop>> {
        let following = match self.start_task_invocation(run, action, arguments)? {
            Ok(following) => following,
            Err(stop) => return Ok(Err(stop)),
        };
        let mut followed = [following];
        self.follow_invocations(run, &mut followed)?;
        let [following] = followed;
        Ok(following.into_outcome())
    }

    /// Ask the host to start one legacy invocation: authorized, journaled
    /// and accepted by the broker on its own, exactly as when it is followed
    /// alone. The host answers with the accepted state at once; the action
    /// then runs in its application while this agent starts the next one or
    /// follows it ([`Self::follow_invocations`]).
    fn start_task_invocation(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        action: &TaskAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Result<Following, TaskActionStop>> {
        if self.task_budget_expired() {
            return Ok(Err(TaskActionStop::Failed(
                "Task time budget expired before invoking an action".into(),
            )));
        }
        let request_id = self.next_request_id();
        let invocation = self.task_child_id(run, action, arguments)?;
        self.link.send(&CollabRequest::InvokeTaskAction {
            request_id,
            invocation,
            run_id: run.run_id,
            lease: self.task_lease(run),
            peer_id: action.peer_id,
            provider: action.provider.clone(),
            resource: action.resource.clone(),
            action: action.action.clone(),
            arguments: arguments.clone(),
            wait: None,
        })?;
        let update = match self.wait_for_task_action_result(request_id)? {
            Ok((_, update)) => update,
            Err(error) if confirmation::is_awaiting(&error) => {
                match self.await_confirmation(run, (action, arguments), invocation)? {
                    Ok(update) => update,
                    Err(stop) => return Ok(Err(stop)),
                }
            }
            Err(error) => {
                return Ok(Err(TaskActionStop::Failed(confirmation::step_failure(
                    error,
                ))));
            }
        };
        if self.native_task.is_none() {
            self.publish_task_interaction(run, action, &update, 0);
        }
        Ok(Ok(Following {
            action: action.clone(),
            arguments: arguments.clone(),
            invocation,
            reask: self.native_task.is_some().then(native_task::Reask::default),
            update,
            sequence: 0,
            deadline: self.task_wait_deadline(TASK_ACTION_WAIT),
            outcome: None,
        }))
    }

    /// Follow started legacy invocations, together, until each has its
    /// outcome: one wait for news of any of them, one cancellation poll per
    /// round, and every one still running cancelled when Task Center
    /// cancels the run. One invocation alone is followed exactly as before.
    fn follow_invocations(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        followed: &mut [Following],
    ) -> Result<()> {
        if followed.iter().any(|following| following.reask.is_some()) {
            return self.follow_native(run, followed);
        }
        let mut wait = FOLLOW_FIRST_WAIT;
        loop {
            for following in followed.iter_mut() {
                self.conclude_following(run, following)?;
            }
            let running: Vec<tilo_actions::InvocationId> = followed
                .iter()
                .filter(|following| following.outcome.is_none())
                .map(|following| following.invocation)
                .collect();
            let Some(deadline) = followed
                .iter()
                .filter(|following| following.outcome.is_none())
                .map(|following| following.deadline)
                .min()
            else {
                return Ok(());
            };
            let left = deadline.saturating_duration_since(ui::time::now());
            let pushed = self.await_invocation_news(&running, wait.min(left))?;
            wait = (wait * 2).min(FOLLOW_MOST_WAIT);
            // Asked without waiting: the answer arrives ahead of the
            // observations' in the client's one ordered queue, so it is read
            // below as fresh as the observations themselves.
            self.poll_cancellation(run)?;
            match pushed {
                Some((invocation, update)) => {
                    if let Some(following) = followed
                        .iter_mut()
                        .find(|following| following.invocation == invocation)
                    {
                        self.advance_following(run, following, update);
                    }
                }
                None => {
                    for following in followed
                        .iter_mut()
                        .filter(|following| following.outcome.is_none())
                    {
                        match self.observe(following.invocation)? {
                            Ok(update) => self.advance_following(run, following, update),
                            Err(error) => {
                                following.outcome = Some(Err(TaskActionStop::Failed(error)));
                            }
                        }
                    }
                }
            }
            let still_running = followed
                .iter()
                .any(|following| following.outcome.is_none() && !following.update.is_terminal());
            if still_running && self.cancellation_known()? {
                for following in followed
                    .iter_mut()
                    .filter(|following| following.outcome.is_none())
                {
                    if following.update.is_terminal() {
                        self.conclude_following(run, following)?;
                    } else {
                        self.cancel_following(run, following)?;
                    }
                }
                return Ok(());
            }
        }
    }

    /// Take in a newer state of `following` and show it.
    fn advance_following(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        following: &mut Following,
        update: tilo_actions::InvocationUpdate,
    ) {
        following.update = update;
        following.sequence = following.sequence.saturating_add(1);
        self.publish_task_interaction(
            run,
            &following.action,
            &following.update,
            following.sequence,
        );
    }

    /// Settle `following` once it is terminal or its time is up; a running
    /// one within its time is left as it is.
    fn conclude_following(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        following: &mut Following,
    ) -> Result<()> {
        if following.outcome.is_some() {
            return Ok(());
        }
        let outcome = match &following.update {
            tilo_actions::InvocationUpdate::Succeeded { output, .. } => Ok(StepOutcome {
                output: output.clone(),
            }),
            tilo_actions::InvocationUpdate::Cancelled { message, .. } => {
                Err(TaskActionStop::Cancelled(message.clone()))
            }
            tilo_actions::InvocationUpdate::Failed { error, .. } => {
                Err(TaskActionStop::Failed(error.message.clone()))
            }
            _ if ui::time::now() < following.deadline => return Ok(()),
            _ if self.task_budget_expired() => {
                self.link.send(&CollabRequest::RemoteAction {
                    message: RemoteActionMessage::Cancel {
                        invocation: following.invocation,
                    },
                })?;
                Err(TaskActionStop::Failed(
                    "Task time budget expired; cancellation requested, action outcome remains unknown"
                        .into(),
                ))
            }
            other => {
                let message = "Task action did not reach a terminal state before its deadline";
                let failed = tilo_actions::InvocationUpdate::Failed {
                    invocation: other.invocation(),
                    error: tilo_actions::ActionFailure {
                        code: "timeout".into(),
                        message: message.into(),
                        details: None,
                        retryable: true,
                    },
                };
                following.sequence = following.sequence.saturating_add(1);
                self.publish_task_interaction(run, &following.action, &failed, following.sequence);
                Err(TaskActionStop::Failed(message.into()))
            }
        };
        following.outcome = Some(outcome);
        Ok(())
    }

    /// Cancel a running `following` because Task Center cancelled the run.
    fn cancel_following(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        following: &mut Following,
    ) -> Result<()> {
        let invocation = following.invocation;
        self.link.send(&CollabRequest::RemoteAction {
            message: RemoteActionMessage::Cancel { invocation },
        })?;
        let _ = self.wait_for_remote_answer(|message| {
            matches!(
                message,
                RemoteActionMessage::CancelResponse { invocation: answered, .. }
                    if *answered == invocation
            )
        })?;
        following.sequence = following.sequence.saturating_add(1);
        let cancelled = tilo_actions::InvocationUpdate::Cancelled {
            invocation: following.update.invocation(),
            message: Some("Cancelled from Task Center".into()),
        };
        self.publish_task_interaction(run, &following.action, &cancelled, following.sequence);
        following.outcome = Some(Err(TaskActionStop::Cancelled(Some(
            "Cancelled from Task Center".into(),
        ))));
        Ok(())
    }

    /// Ask the host to stop `following` without waiting for its answer: the
    /// turn that started it ended before it settled. A native child is not
    /// the action host's to stop: the run's terminal report, which every
    /// early end of a native run sends, cancels each child still running.
    fn abandon_following(&mut self, following: &Following) {
        if following.reask.is_some() {
            return;
        }
        if let Err(error) = self.link.send(&CollabRequest::RemoteAction {
            message: RemoteActionMessage::Cancel {
                invocation: following.invocation,
            },
        }) {
            tracing::warn!(%error, "could not cancel an abandoned invocation");
        }
    }

    /// Project a real broker invocation into semantic presence. No pointer,
    /// caret, focus, or viewport mutation is synthesized: peers receive the
    /// resource/action lifecycle and resolve its stable anchor natively.
    fn publish_task_interaction(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        action: &TaskAction,
        update: &tilo_actions::InvocationUpdate,
        sequence: u64,
    ) {
        if self.managed {
            // No managed task diagnostic is constructed as public presence.
            // Terminal detail returns to run_task's single private terminal report.
            return;
        }
        let phase = match update {
            tilo_actions::InvocationUpdate::Accepted { .. } => {
                tilo_actions::InteractionPhase::Applying
            }
            tilo_actions::InvocationUpdate::Progress {
                phase,
                completed,
                total,
                ..
            } => tilo_actions::InteractionPhase::Progress {
                label: phase.clone(),
                completed: *completed,
                total: *total,
            },
            tilo_actions::InvocationUpdate::Item { .. } => tilo_actions::InteractionPhase::Applying,
            tilo_actions::InvocationUpdate::Succeeded { .. } => {
                tilo_actions::InteractionPhase::Committed
            }
            tilo_actions::InvocationUpdate::Failed { error, .. } => {
                tilo_actions::InteractionPhase::Failed {
                    code: Some(error.code.clone()),
                    message: error.message.clone(),
                }
            }
            tilo_actions::InvocationUpdate::Cancelled { message, .. } => {
                tilo_actions::InteractionPhase::Cancelled {
                    message: message.clone(),
                }
            }
        };
        let context =
            serde_json::from_value::<tilo_actions::ActionContext>(run.action_context.clone()).ok();
        let interaction = tilo_actions::SemanticInteraction {
            version: tilo_actions::INTERACTION_VERSION,
            invocation: update.invocation(),
            sequence,
            // The local broker replaces these fields from the authenticated
            // admission before fan-out. They are populated only so the
            // strongly typed value is valid before crossing that boundary.
            actor: tilo_actions::InteractionActor {
                id: self.cfg.agent_id.clone(),
                kind: tilo_actions::ActorKind::Agent,
                // The agent acts for whoever asked.
                for_person: None,
            },
            phase,
            action: tilo_actions::InteractionAction {
                id: action.action.clone(),
                label: action.label.clone(),
                app: Some(tilo_actions::ActionApp {
                    id: action.app.clone(),
                    label: action.app.clone(),
                }),
            },
            resource: tilo_actions::ActionResource {
                id: action.resource.clone(),
                kind: action.resource_kind.clone(),
                label: action.resource_label.clone(),
            },
            anchor: if let Some(id) = action.resource.strip_prefix("ui:") {
                tilo_actions::InteractionAnchor::SemanticControl { id: id.into() }
            } else if action.resource_kind.contains("entry")
                || action.resource_kind.contains("file")
            {
                tilo_actions::InteractionAnchor::ListItems {
                    ids: vec![action.resource.clone()],
                }
            } else {
                tilo_actions::InteractionAnchor::Resource
            },
            group: context.and_then(|context| context.group),
        };
        let key = PresenceKey::resource(&action.app, &action.resource, &action.resource_kind);
        let _ = self.link.send(&CollabRequest::Presence {
            key,
            payload: PresencePayload::Interaction(Box::new(interaction)),
        });
    }

    fn wait_for_task_action_result(
        &mut self,
        request_id: u64,
    ) -> Result<Result<(u64, tilo_actions::InvocationUpdate), String>> {
        let deadline = self.task_wait_deadline(Duration::from_secs(10));
        let mut deferred = Vec::new();
        loop {
            // Keep a streaming model turn moving while the step settles.
            self.pump_turn();
            let now = ui::time::now();
            if now >= deadline {
                self.link.defer(deferred);
                return Ok(Err(
                    "the broker did not answer the task action request".into()
                ));
            }
            match self
                .link
                .recv((deadline - now).min(Duration::from_millis(100)))?
            {
                Some(CollabEvent::TaskActionInvocationResult {
                    request_id: received,
                    result,
                }) if received == request_id => {
                    self.link.defer(deferred);
                    return Ok(result);
                }
                // Only one task action is outstanding at a time; a late
                // reply to an earlier request has no reader left.
                Some(
                    CollabEvent::TaskActionInvocationResult { .. } | CollabEvent::Document { .. },
                )
                | None => {}
                Some(CollabEvent::ActionCatalogue { surfaces }) => {
                    self.set_broker_surfaces(surfaces);
                }
                Some(event) => deferred.push(event),
            }
        }
    }

    /// Wait up to `wait` for news of any of `invocations`: an update the
    /// host pushes (returned, with its invocation), or a document change in
    /// the room (absorbed; the wait ends so the caller asks at once).
    /// Catalogue updates are applied and every other event keeps its place.
    fn await_invocation_news(
        &mut self,
        invocations: &[tilo_actions::InvocationId],
        wait: Duration,
    ) -> Result<Option<(tilo_actions::InvocationId, tilo_actions::InvocationUpdate)>> {
        let deadline = ui::time::now() + wait;
        let mut deferred = Vec::new();
        let mut pushed = None;
        loop {
            self.pump_turn();
            let now = ui::time::now();
            if now >= deadline {
                break;
            }
            match self.link.recv(deadline - now)? {
                Some(CollabEvent::RemoteAction {
                    message:
                        RemoteActionMessage::Update {
                            invocation: answered,
                            state: Some(state),
                        },
                }) if invocations.contains(&answered) => {
                    pushed = Some((answered, state.update));
                    break;
                }
                Some(CollabEvent::ActionCatalogue { surfaces }) => {
                    self.set_broker_surfaces(surfaces);
                }
                Some(CollabEvent::Document { .. }) | None => break,
                Some(event) => deferred.push(event),
            }
        }
        self.link.defer(deferred);
        Ok(pushed)
    }

    /// The latest state of an invocation this agent started.
    fn observe(
        &mut self,
        invocation: tilo_actions::InvocationId,
    ) -> Result<Result<tilo_actions::InvocationUpdate, String>> {
        self.link.send(&CollabRequest::RemoteAction {
            message: RemoteActionMessage::observe(invocation),
        })?;
        Ok(
            match self.wait_for_remote_answer(|message| {
                matches!(
                    message,
                    RemoteActionMessage::Update { invocation: answered, .. }
                        if *answered == invocation
                )
            })? {
                Some(RemoteActionMessage::Update {
                    state: Some(state), ..
                }) => Ok(state.update),
                Some(_) => Err("the action host no longer knows that invocation".into()),
                None => Err("the action host did not answer".into()),
            },
        )
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_broker_request;
        self.next_broker_request = self.next_broker_request.saturating_add(1);
        id
    }

    /// Wait for the answer `wanted` picks out, absorbing document traffic
    /// that arrives meanwhile and keeping every other event for later.
    fn wait_for_remote_answer(
        &mut self,
        wanted: impl Fn(&RemoteActionMessage) -> bool,
    ) -> Result<Option<RemoteActionMessage>> {
        let deadline = self.task_wait_deadline(Duration::from_secs(10));
        let mut deferred = Vec::new();
        loop {
            self.pump_turn();
            let now = ui::time::now();
            if now >= deadline {
                self.link.defer(deferred);
                return Ok(None);
            }
            match self
                .link
                .recv((deadline - now).min(Duration::from_millis(100)))?
            {
                Some(CollabEvent::RemoteAction { message }) if wanted(&message) => {
                    self.link.defer(deferred);
                    return Ok(Some(message));
                }
                Some(CollabEvent::ActionCatalogue { surfaces }) => {
                    self.set_broker_surfaces(surfaces);
                }
                Some(CollabEvent::Document { .. }) | None => {}
                Some(event) => deferred.push(event),
            }
        }
    }
}

/// The model selection of a launcher-managed agent, or `None` when this
/// agent was not launched with a managed context.
fn managed_context(cfg: &Config) -> Result<Option<crate::providers::ProviderSelection>> {
    let Some(raw) = std::env::var_os("TILO_MANAGED_AGENT_CONTEXT") else {
        return Ok(None);
    };
    let raw = raw
        .into_string()
        .map_err(|_| anyhow::anyhow!("invalid managed context encoding"))?;
    anyhow::ensure!(raw.len() <= 4096, "managed context too large");
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("invalid managed context"))?;
    // Explicit field count prevents credentials or ambient endpoints being smuggled in.
    anyhow::ensure!(
        object.len() == 7
            && object.keys().all(|key| matches!(
                key.as_str(),
                "version"
                    | "room"
                    | "agent"
                    | "launch"
                    | "runtime_epoch"
                    | "channel_id"
                    | "provider"
            )),
        "unsupported managed context fields"
    );
    let text = |key: &str| {
        object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing managed context field {key}"))
    };
    anyhow::ensure!(
        value.get("version").and_then(serde_json::Value::as_u64) == Some(1)
            && text("room")? == cfg.cluster.to_string()
            && text("agent")? == cfg.agent_id
            && cfg.app_id == "so.tilo.agent"
            && cfg.model
            && cfg.endpoint.is_none()
            && cfg.record.is_none(),
        "managed context does not bind this invocation"
    );
    let selection = crate::model::managed_provider_selection(
        value
            .get("provider")
            .ok_or_else(|| anyhow::anyhow!("managed selection missing"))?,
    )
    .map_err(anyhow::Error::msg)?;
    // The launch binding fields identify this process to the launcher; the
    // agent itself only requires them to be present.
    for key in ["launch", "runtime_epoch", "channel_id"] {
        text(key)?;
    }
    Ok(Some(selection))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_run() -> runtime_collab::TaskRunEnvelope {
        runtime_collab::TaskRunEnvelope {
            paused: None,
            version: 1,
            task_id: "task:review".into(),
            title: "Review draft".into(),
            instruction: None,
            subject: None,
            run_id: 7,
            lease: "broker-minted-lease".into(),
            planning: runtime_collab::TaskRunPlanning::RulesOnly,
            presentation_version: None,
            execution_budget: None,
            action_context: serde_json::json!({
                "purpose": "Review draft",
                "group": {"id": "task:review", "label": "Review draft", "kind": "task"}
            }),
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario drives every invocation update through the private diagnostic path"
    )]
    fn managed_action_diagnostics_stay_private_through_invocation_updates() {
        use std::{cell::RefCell, collections::VecDeque, rc::Rc};
        struct Peer {
            sent: Rc<RefCell<Vec<CollabRequest>>>,
            events: Vec<CollabEvent>,
            updates: VecDeque<tilo_actions::InvocationUpdate>,
        }
        impl ui::collaboration::PeerTransport for Peer {
            fn is_connected(&self) -> bool {
                true
            }
            fn poll(&mut self) -> Vec<CollabEvent> {
                std::mem::take(&mut self.events)
            }
            fn send(&mut self, request: CollabRequest) -> Result<(), String> {
                let response = match &request {
                    CollabRequest::InvokeTaskAction { request_id, .. } => {
                        Some(CollabEvent::TaskActionInvocationResult {
                            request_id: *request_id,
                            result: Ok((71, self.updates.pop_front().unwrap())),
                        })
                    }
                    CollabRequest::RemoteAction {
                        message: RemoteActionMessage::Observe { invocation, .. },
                    } => Some(CollabEvent::RemoteAction {
                        message: RemoteActionMessage::Update {
                            invocation: *invocation,
                            state: Some(tilo_actions::remote::InvocationState::new(
                                *invocation,
                                self.updates.pop_front().unwrap(),
                            )),
                        },
                    }),
                    CollabRequest::TaskRunCancellation { request_id, .. } => {
                        Some(CollabEvent::TaskRunCancellation {
                            request_id: *request_id,
                            result: Ok(false),
                        })
                    }
                    CollabRequest::ReportTaskRun { .. } | CollabRequest::Presence { .. } => None,
                    _ => panic!("unexpected request in action update path"),
                };
                self.sent.borrow_mut().push(request);
                if let Some(response) = response {
                    self.events.push(response);
                }
                Ok(())
            }
        }
        let invocation = tilo_actions::InvocationId(Uuid::from_u128(71));
        for managed in [true, false] {
            for cancelled in [false, true] {
                let marker = "private-action-argument-marker";
                let terminal = if cancelled {
                    tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some(marker.into()),
                    }
                } else {
                    tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "private.failure".into(),
                            message: marker.into(),
                            details: None,
                            retryable: false,
                        },
                    }
                };
                let sent = Rc::new(RefCell::new(Vec::new()));
                let link = crate::wire::test_link(Peer {
                    sent: sent.clone(),
                    events: Vec::new(),
                    updates: VecDeque::from([
                        tilo_actions::InvocationUpdate::Progress {
                            invocation,
                            phase: marker.into(),
                            message: None,
                            sequence: 1,
                            completed: None,
                            total: None,
                        },
                        terminal,
                    ]),
                });
                let run = task_run();
                let mut session = Session {
                    cfg: Config {
                        agent_id: "agent-1".into(),
                        endpoint: None,
                        endpoint_identity: None,
                        app_id: "so.tilo.agent".into(),
                        cluster: Uuid::from_u128(1),
                        record: None,
                        name: "agent-1".into(),
                        model: false,
                        model_timeout: Duration::ZERO,
                        permissions: ui::permissions::PermissionContext::agent_granted_at(
                            "agent-1",
                            &[],
                            ui::permissions::KindSet::READ_UNDOABLE,
                        ),
                        content: tilo_content::ContentStore::memory(),
                        companions: Vec::new(),
                        provider: None,
                    },
                    link,
                    chooser: Chooser::new(false, Duration::ZERO),
                    managed,
                    task_deadline: None,
                    native_task: None,
                    broker_surfaces: Vec::new(),
                    next_broker_request: 1,
                    memory: agent_loop::Memory::default(),
                    awaiting_companions: Vec::new(),
                    broker_revision: 0,
                    catalogue_cache: None,
                    turn: None,
                    outstanding: outstanding::Outstanding::default(),
                };
                let action = TaskAction {
                    app: "app".into(),
                    peer_id: 2,
                    provider: String::new(),
                    resource: "resource".into(),
                    resource_kind: "item".into(),
                    resource_label: "Resource".into(),
                    action: "act".into(),
                    arguments: serde_json::Map::new(),
                    label: "Action".into(),
                    carries: None,
                };
                let outcome = session
                    .invoke_task_action(&run, &action, &action.arguments)
                    .unwrap();
                match outcome {
                    Err(TaskActionStop::Failed(message)) => {
                        assert_eq!(message, marker);
                        if managed {
                            session.fail_task(&run, &message).unwrap();
                        }
                    }
                    Err(TaskActionStop::Cancelled(Some(message))) => {
                        assert_eq!(message, marker);
                        if managed {
                            session.cancel_task(&run, &message).unwrap();
                        }
                    }
                    other => panic!("action diagnostic lost: {other:?}"),
                }
                let requests = sent.borrow();
                let public: Vec<_> = requests
                    .iter()
                    .filter_map(|request| match request {
                        CollabRequest::Presence {
                            payload: PresencePayload::Interaction(interaction),
                            ..
                        } => Some(serde_json::to_string(interaction).unwrap()),
                        _ => None,
                    })
                    .collect();
                if managed {
                    assert!(
                        public.is_empty(),
                        "managed action updates must not construct or send public diagnostics: {public:?}"
                    );
                    if cancelled {
                        assert!(
                            requests.iter().any(
                                |request| matches!(request, CollabRequest::ReportTaskRun {
                            update: runtime_collab::TaskRunUpdate::Cancelled { message }, ..
                        } if message == marker)
                            ),
                            "private cancellation must retain action detail"
                        );
                    } else {
                        assert!(
                            requests.iter().any(
                                |request| matches!(request, CollabRequest::ReportTaskRun {
                            update: runtime_collab::TaskRunUpdate::Failed { message }, ..
                        } if message == marker)
                            ),
                            "private failure must retain action detail"
                        );
                    }
                } else {
                    assert_eq!(
                        public.len(),
                        2,
                        "legacy progress and terminal interactions remain public"
                    );
                    assert!(public.iter().all(|json| json.contains(marker)));
                }
            }
        }
    }

    #[test]
    fn a_task_run_needs_a_known_version_and_broker_bound_task_context() {
        // Case `task_run_requires_broker_bound_task_context`.
        {
            assert!(validate_task_run(&task_run()).is_ok());
            let mut mismatched = task_run();
            mismatched.action_context = serde_json::json!({
                "purpose": "Review draft",
                "group": {"id": "different", "label": "Review draft", "kind": "task"}
            });
            assert!(validate_task_run(&mismatched).is_err());
        }

        // Case `task_run_rejects_unknown_protocol_version_before_planning`.
        {
            let mut invalid = task_run();
            invalid.version = 2;
            assert!(validate_task_run(&invalid).is_err());
        }
    }

    #[test]
    fn a_steps_arguments_bind_exactly_one_preceding_content_result_and_key_its_dispatch() {
        // Case `a_content_step_uses_only_the_immediately_preceding_application_result`.
        {
            let output = tilo_actions::ActionOutput {
                value: Some(serde_json::json!({"display": "ignored"})),
                resources: vec![
                    tilo_actions::content_resource("abc123")
                        .expect("the producer returned a valid content hash"),
                ],
            };
            let previous = StepOutcome { output };
            let planned = serde_json::Map::from_iter([(
                tilo_actions::PROMPTED_PARAM.to_string(),
                serde_json::Value::String("model-invented".into()),
            )]);

            let bound = arguments_for_step(
                Some(tilo_actions::PROMPTED_PARAM),
                &planned,
                Some(&previous),
            )
            .expect("one content result binds the next content input");
            assert_eq!(
                bound.get(tilo_actions::PROMPTED_PARAM),
                Some(&serde_json::Value::String("abc123".into())),
                "the preceding app result must replace model input"
            );
            assert!(
                arguments_for_step(Some(tilo_actions::PROMPTED_PARAM), &planned, None).is_err(),
                "a content action cannot reuse an older or invented result"
            );
        }

        // Case `a_content_step_refuses_ambiguous_or_wrong_kind_results`.
        {
            let ambiguous = StepOutcome {
                output: tilo_actions::ActionOutput {
                    value: None,
                    resources: vec!["tilo.content:first".into(), "tilo.content:second".into()],
                },
            };
            assert!(
                arguments_for_step(
                    Some(tilo_actions::PROMPTED_PARAM),
                    &serde_json::Map::new(),
                    Some(&ambiguous)
                )
                .is_err(),
                "choosing one of two products would be a model-visible guess"
            );

            let wrong_kind = StepOutcome {
                output: tilo_actions::ActionOutput {
                    value: None,
                    resources: vec!["files.entry:/tmp/picture.png".into()],
                },
            };
            assert!(
                arguments_for_step(
                    Some(tilo_actions::PROMPTED_PARAM),
                    &serde_json::Map::new(),
                    Some(&wrong_kind)
                )
                .is_err(),
                "a generic resource address is not proof that content bytes exist"
            );
        }

        // Case `an_empty_result_feeds_no_content_step`.
        {
            let previous = StepOutcome::default();
            assert!(
                arguments_for_step(
                    Some(tilo_actions::PROMPTED_PARAM),
                    &serde_json::Map::new(),
                    Some(&previous)
                )
                .is_err(),
                "a step that produced nothing cannot feed a content input"
            );
        }
    }
}
